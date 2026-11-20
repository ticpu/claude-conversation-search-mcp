//! Query construction and retrieval over the tantivy index. Query text
//! parsing lives in `query`.
//!
//! Filters and the fetch. The project, session, exclude, active-session,
//! hidden-content and one-per-session filters run after tantivy's top-K, so
//! the fetch starts at three times the limit and doubles until `limit + 1`
//! hits survive or the matches run out. The extra survivor is what `more`
//! reports. Date bounds stay inside the tantivy query, where BM25 only sees
//! the window. A limit of 0 is an error, never handed to TopDocs, which
//! asserts on it. The filters themselves live in `filter`.
//!
//! Context window. sequence_num restarts at 0 in every JSONL file, and
//! subagent and resumed transcripts carry their parent's sessionId, so a
//! session id does not name one ordered transcript. A hit's context is the
//! docs of its own source_file with sequence_num in [seq - B, seq + A], and
//! its "(N msgs)" is the doc count of its main transcript: the file itself,
//! or the parent `<uuid>.jsonl` of a subagent file. Both are bounded queries;
//! nothing fetches a whole session to build a window.
//!
//! Session and uuid lookups. A session filter arrives resolved: full ids
//! from the transcript file names, or the raw prefix when no file matches (a
//! deleted transcript). A full id matches every hyphen segment and must equal
//! the stored value. A prefix matches the stored value case-insensitively,
//! prefiltered by a regex on the term of its last segment. uuid lookups work
//! the same way. `get_session_messages` takes a full id and returns every doc
//! of the session, main transcripts first, each file in sequence order, with
//! no cap.

mod doc;
mod filter;
mod query;

pub use filter::{ResultFilter, compile_exclude_patterns};

use super::format::{DisplayOptions, SearchResultWithContext, self_context_result};
use super::models::{SearchQuery, SearchResult, SessionFilter, SortOrder};
use super::parsers::SourceKind;
use super::path_utils::main_transcript_of;
use anyhow::{Context, Result, bail};
use filter::{project_matches, project_query};
use query::{TextQuery, parse_text_query};
use std::collections::{HashMap, HashSet};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use tantivy::collector::{Count, DocSetCollector, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, RangeQuery, RegexQuery, TermQuery};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::{Index, IndexReader, Order, ReloadPolicy, Searcher, Term};

/// Build a Boolean AND of TermQuery per hyphen segment of `value`.
/// The TEXT field tokenizes at hyphens, so a UUID or session id must be
/// matched segment by segment rather than as a single term.
fn hyphenated_term_query(field: Field, value: &str) -> BooleanQuery {
    let segment_queries: Vec<_> = value
        .split('-')
        .map(|seg| {
            let term = Term::from_field_text(field, &seg.to_lowercase());
            (
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                    as Box<dyn tantivy::query::Query>,
            )
        })
        .collect();
    BooleanQuery::new(segment_queries)
}

/// Superset prefilter for a case-insensitive prefix of a hyphenated id: every
/// complete token must match and the last, possibly partial, one is a term
/// prefix. The caller compares the stored value with `starts_with_ignore_case`.
fn prefix_query(field: Field, prefix: &str) -> Result<BooleanQuery> {
    let lower = prefix.to_lowercase();
    let mut tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .collect();
    let last = tokens
        .pop()
        .unwrap_or_default();
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = tokens
        .into_iter()
        .filter(|t| !t.is_empty())
        .map(|t| {
            let term = Term::from_field_text(field, t);
            (
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
            )
        })
        .collect();
    let pattern = format!("{last}.*");
    let last_query = RegexQuery::from_pattern(&pattern, field)
        .with_context(|| format!("building the prefix query for {prefix:?}"))?;
    clauses.push((Occur::Must, Box::new(last_query)));
    Ok(BooleanQuery::new(clauses))
}

fn starts_with_ignore_case(value: &str, prefix: &str) -> bool {
    value
        .to_lowercase()
        .starts_with(&prefix.to_lowercase())
}

/// Sort key putting main transcripts before subagent ones, then each file's
/// messages in sequence order.
fn transcript_order(r: &SearchResult) -> (bool, &str, usize) {
    let is_subagent = matches!(
        SourceKind::classify(Path::new(&r.source_file)),
        SourceKind::Agent { .. }
    );
    (
        is_subagent,
        r.source_file
            .as_str(),
        r.sequence_num,
    )
}

/// Search hits that survived every filter, at most the requested limit.
pub struct FilteredResults {
    pub results: Vec<SearchResultWithContext>,
    /// More hits survive the filters than the limit let through.
    pub more: bool,
    /// Why strict parsing of the query text failed, when it was parsed
    /// leniently instead.
    pub lenient: Option<String>,
}

/// The line telling the user their query text was reinterpreted.
pub fn lenient_note(reason: &str) -> String {
    format!("query parsed leniently, some operators may be ignored ({reason})")
}

pub struct SearchEngine {
    index: Index,
    reader: IndexReader,
    fields: super::indexer::IndexFields,
    interaction_counts: HashMap<String, usize>,
}

impl SearchEngine {
    pub fn new(index_path: &Path, session_counts: HashMap<String, usize>) -> Result<Self> {
        let index = Index::open_in_dir(index_path)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        let fields = super::indexer::IndexFields::from_schema(&index.schema())?;

        Ok(Self {
            index,
            reader,
            fields,
            interaction_counts: session_counts,
        })
    }

    /// Build the engine seeded with `cache`'s session counts.
    pub fn from_cache(index_path: &Path, cache: &super::cache::CacheManager) -> Result<Self> {
        Self::new(
            index_path,
            cache
                .get_session_counts()
                .clone(),
        )
    }

    /// The top `query.limit` hits that pass the project and session filters.
    /// Query text that only parses leniently is an error here.
    pub fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        let text = self.parse_text(&query.text);
        if let Some(reason) = text.lenient {
            bail!("query {:?} does not parse: {reason}", query.text);
        }
        let mut hits = self.fetch_hits(
            &query,
            text.query
                .as_ref(),
            query.limit,
        )?;
        hits.retain(|hit| self.passes_post_filters(hit, &query));
        Ok(hits)
    }

    /// Up to `limit` hits that pass every filter, each with its context. The
    /// fetch grows until `limit + 1` hits survive or the matches run out.
    pub fn search_filtered(
        &self,
        query: &SearchQuery,
        filter: &ResultFilter,
        display: &DisplayOptions,
        context_before: usize,
        context_after: usize,
    ) -> Result<FilteredResults> {
        let limit = query.limit;
        let text = self.parse_text(&query.text);
        let mut fetch = limit.saturating_mul(3);
        let survivors = loop {
            let hits = self.fetch_hits(
                query,
                text.query
                    .as_ref(),
                fetch,
            )?;
            let exhausted = hits.len() < fetch;
            let mut sessions_seen = HashSet::new();
            let survivors: Vec<SearchResult> = hits
                .into_iter()
                .filter(|hit| self.passes_post_filters(hit, query) && filter.keeps(hit, display))
                .filter(|hit| {
                    sessions_seen.insert(
                        hit.session_id
                            .clone(),
                    )
                })
                .take(limit.saturating_add(1))
                .collect();
            if exhausted || survivors.len() > limit {
                break survivors;
            }
            fetch = fetch.saturating_mul(2);
        };
        let more = survivors.len() > limit;
        let results = self.attach_context(
            survivors
                .into_iter()
                .take(limit)
                .collect(),
            context_before,
            context_after,
        )?;
        Ok(FilteredResults {
            results,
            more,
            lenient: text.lenient,
        })
    }

    fn parse_text(&self, text: &str) -> TextQuery {
        parse_text_query(
            &self.index,
            vec![
                self.fields
                    .content_field,
                self.fields
                    .session_field,
                self.fields
                    .project_field,
            ],
            text,
        )
    }

    /// The top `fetch` hits of the tantivy query, before any post-filter.
    fn fetch_hits(
        &self,
        query: &SearchQuery,
        text_query: &dyn Query,
        fetch: usize,
    ) -> Result<Vec<SearchResult>> {
        let searcher = self
            .reader
            .searcher();

        let mut final_query_parts = vec![(Occur::Must, text_query.box_clone())];

        if let Some(ref project_filter) = query.project_filter {
            let project_query = project_query(
                self.fields
                    .cwd_field,
                project_filter,
            );
            final_query_parts.push((Occur::Must, Box::new(project_query)));
        }

        if let Some(ref session_filter) = query.session_filter {
            let field = self
                .fields
                .session_field;
            let session_query: Box<dyn Query> = match session_filter {
                SessionFilter::Ids(ids) => Box::new(BooleanQuery::new(
                    ids.iter()
                        .map(|id| {
                            (
                                Occur::Should,
                                Box::new(hyphenated_term_query(field, id)) as Box<dyn Query>,
                            )
                        })
                        .collect(),
                )),
                SessionFilter::Prefix(prefix) => Box::new(prefix_query(field, prefix)?),
            };
            final_query_parts.push((Occur::Must, session_query));
        }

        // Push date bounds into the query so BM25 scoring operates only over the
        // matching time window. Post-hoc filtering on a top-K relevance fetch silently
        // drops recent docs when high-frequency terms rank old docs above the limit.
        let date_term = |dt: &chrono::DateTime<chrono::Utc>| {
            Term::from_field_date(
                self.fields
                    .timestamp_field,
                tantivy::DateTime::from_timestamp_millis(dt.timestamp_millis()),
            )
        };
        let lower = query
            .after
            .as_ref()
            .map(date_term);
        let upper = query
            .before
            .as_ref()
            .map(date_term);
        if lower.is_some() || upper.is_some() {
            let date_query = RangeQuery::new(
                lower.map_or(Bound::Unbounded, Bound::Included),
                upper.map_or(Bound::Unbounded, Bound::Included),
            );
            final_query_parts.push((
                Occur::Must,
                Box::new(date_query) as Box<dyn tantivy::query::Query>,
            ));
        }

        // BooleanQuery specializes a single-clause vector to the clause's own
        // scorer, so this scores identically to using the clause directly.
        let final_query = BooleanQuery::new(final_query_parts);

        // The three sort orders use different tantivy collectors (with different
        // Fruit types), so each match arm runs its own search and discards the
        // sort key, leaving a uniform doc address list for the one loop below.
        let doc_addresses: Vec<_> = match &query.sort_by {
            SortOrder::Relevance => searcher
                .search(&final_query, &TopDocs::with_limit(fetch).order_by_score())?
                .into_iter()
                .map(|(_, addr)| addr)
                .collect(),
            SortOrder::DateDesc => searcher
                .search(
                    &final_query,
                    &TopDocs::with_limit(fetch)
                        .order_by_fast_field::<tantivy::DateTime>("timestamp", Order::Desc),
                )?
                .into_iter()
                .map(|(_, addr)| addr)
                .collect(),
            SortOrder::DateAsc => searcher
                .search(
                    &final_query,
                    &TopDocs::with_limit(fetch)
                        .order_by_fast_field::<tantivy::DateTime>("timestamp", Order::Asc),
                )?
                .into_iter()
                .map(|(_, addr)| addr)
                .collect(),
        };

        doc_addresses
            .into_iter()
            .map(|addr| self.doc_to_result(&searcher.doc(addr)?))
            .collect()
    }

    /// Session and project post-filters: precision checks that Tantivy's segment-based
    /// queries cannot express (prefix matching, full name equality).
    fn passes_post_filters(&self, result: &SearchResult, query: &SearchQuery) -> bool {
        let session_kept = match &query.session_filter {
            None => true,
            Some(SessionFilter::Ids(ids)) => ids.contains(&result.session_id),
            Some(SessionFilter::Prefix(prefix)) => {
                starts_with_ignore_case(&result.session_id, prefix)
            }
        };
        if !session_kept {
            return false;
        }
        if let Some(ref project_filter) = query.project_filter
            && !project_matches(&result.project_path, project_filter)
        {
            return false;
        }
        true
    }

    /// Each hit with the context window of its own transcript, in hit order.
    fn attach_context(
        &self,
        matches: Vec<SearchResult>,
        context_before: usize,
        context_after: usize,
    ) -> Result<Vec<SearchResultWithContext>> {
        let searcher = self
            .reader
            .searcher();
        let mut transcript_lengths: HashMap<PathBuf, usize> = HashMap::new();
        let mut results_with_context = Vec::new();

        for match_result in matches {
            let main_transcript = main_transcript_of(Path::new(&match_result.source_file));
            let total_session_messages = match transcript_lengths.get(&main_transcript) {
                Some(&n) => n,
                None => {
                    let n = self.transcript_len(&searcher, &main_transcript)?;
                    transcript_lengths.insert(main_transcript, n);
                    n
                }
            };

            let window =
                self.context_window(&searcher, &match_result, context_before, context_after)?;
            let context_messages: Vec<SearchResult> = window
                .into_iter()
                .filter(|m| m.is_displayable())
                .collect();
            let match_index = context_messages
                .iter()
                .position(|m| m.sequence_num == match_result.sequence_num);

            results_with_context.push(match match_index {
                Some(match_index) => SearchResultWithContext {
                    matched_message: match_result,
                    context_messages,
                    match_index,
                    total_session_messages,
                },
                None => self_context_result(match_result, total_session_messages),
            });
        }

        Ok(results_with_context)
    }

    /// The docs of `hit`'s own transcript with sequence_num within `before`
    /// and `after` of it, in sequence order.
    fn context_window(
        &self,
        searcher: &Searcher,
        hit: &SearchResult,
        before: usize,
        after: usize,
    ) -> Result<Vec<SearchResult>> {
        let seq_term = |seq: usize| {
            Term::from_field_u64(
                self.fields
                    .sequence_num_field,
                seq as u64,
            )
        };
        let first = hit
            .sequence_num
            .saturating_sub(before);
        let last = hit
            .sequence_num
            .saturating_add(after);
        let query = BooleanQuery::new(vec![
            (
                Occur::Must,
                Box::new(self.source_file_query(&hit.source_file)) as Box<dyn Query>,
            ),
            (
                Occur::Must,
                Box::new(RangeQuery::new(
                    Bound::Included(seq_term(first)),
                    Bound::Included(seq_term(last)),
                )),
            ),
        ]);
        let mut window = self.collect_docs(searcher, &query)?;
        window.sort_by_key(|m| m.sequence_num);
        Ok(window)
    }

    /// Number of docs indexed from one transcript file.
    fn transcript_len(&self, searcher: &Searcher, transcript: &Path) -> Result<usize> {
        let query = self.source_file_query(&transcript.to_string_lossy());
        searcher
            .search(&query, &Count)
            .with_context(|| format!("counting the messages of {}", transcript.display()))
    }

    fn source_file_query(&self, source_file: &str) -> TermQuery {
        TermQuery::new(
            Term::from_field_text(
                self.fields
                    .source_file_field,
                source_file,
            ),
            IndexRecordOption::Basic,
        )
    }

    /// Every doc matching `query`, unordered and uncapped.
    fn collect_docs(&self, searcher: &Searcher, query: &dyn Query) -> Result<Vec<SearchResult>> {
        searcher
            .search(query, &DocSetCollector)?
            .into_iter()
            .map(|addr| self.doc_to_result(&searcher.doc(addr)?))
            .collect()
    }

    /// Every message of one session, main transcripts first, then subagent
    /// transcripts, each file in sequence order.
    pub fn get_session_messages(&self, session_id: &str) -> Result<Vec<SearchResult>> {
        let searcher = self
            .reader
            .searcher();
        let query = hyphenated_term_query(
            self.fields
                .session_field,
            session_id,
        );
        let mut results = self.collect_docs(&searcher, &query)?;
        results.retain(|r| r.session_id == session_id);
        results.sort_by(|a, b| transcript_order(a).cmp(&transcript_order(b)));
        Ok(results)
    }

    /// Distinct indexed session ids starting with `prefix`, ignoring case,
    /// for sessions whose transcript is gone.
    pub fn session_ids_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let searcher = self
            .reader
            .searcher();
        let query = prefix_query(
            self.fields
                .session_field,
            prefix,
        )?;
        let mut ids: Vec<String> = self
            .collect_docs(&searcher, &query)?
            .into_iter()
            .map(|r| r.session_id)
            .filter(|id| starts_with_ignore_case(id, prefix))
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// One message per uuid prefix, ignoring case; the exact uuid wins over
    /// a longer one sharing the prefix.
    pub fn get_messages_by_uuid(&self, uuids: &[String]) -> Result<Vec<SearchResult>> {
        let searcher = self
            .reader
            .searcher();
        let mut results = Vec::new();

        for uuid in uuids {
            let query = prefix_query(
                self.fields
                    .uuid_field,
                uuid,
            )?;
            let mut matches: Vec<SearchResult> = searcher
                .search(&query, &TopDocs::with_limit(10).order_by_score())?
                .into_iter()
                .map(|(_, addr)| self.doc_to_result(&searcher.doc(addr)?))
                .collect::<Result<_>>()?;
            matches.retain(|m| starts_with_ignore_case(&m.uuid, uuid));
            matches.sort_by_key(|m| {
                !m.uuid
                    .eq_ignore_ascii_case(uuid)
            });
            results.extend(
                matches
                    .into_iter()
                    .next(),
            );
        }

        Ok(results)
    }
}

/// Open the cache and the search engine seeded with its session counts.
pub fn open_search_engine(index_path: &Path) -> Result<(super::cache::CacheManager, SearchEngine)> {
    let cache = super::cache::CacheManager::new(index_path)?;
    let engine = SearchEngine::from_cache(index_path, &cache)?;
    Ok((cache, engine))
}

#[cfg(test)]
mod tests;
