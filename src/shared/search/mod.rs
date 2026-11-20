//! Query construction and retrieval over the tantivy index.
//!
//! Query text. User text is code more often than prose, so `word:` is a field
//! clause only when `word` is a schema field; every other colon is escaped
//! before parsing (`std::fs::read`, `error: foo`, `http://x`). Text that still
//! fails strict parsing goes through `parse_query_lenient`, and its parse
//! errors travel with the results so the caller can say the query was
//! reinterpreted. A query is never refused for its syntax.
//!
//! Filters and the fetch. The project, session, exclude, active-session,
//! hidden-content and one-per-session filters run after tantivy's top-K, so
//! the fetch starts at three times the limit and doubles until `limit + 1`
//! hits survive or the matches run out. The extra survivor is what `more`
//! reports. Date bounds stay inside the tantivy query, where BM25 only sees
//! the window. A limit of 0 is an error, never handed to TopDocs, which
//! asserts on it.
//!
//! Project filter. A hit matches when its cwd contains the filter's path
//! components contiguously, and an absolute filter must be a prefix of it, so
//! a subdirectory or `worktrees/<x>` of the project matches. The tantivy
//! clause on the cwd field is a superset prefilter; `project_matches` decides.
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

use super::format::{SearchResultWithContext, self_context_result};
use super::models::{SearchQuery, SearchResult, SortOrder};
use super::parsers::SourceKind;
use super::path_utils::main_transcript_of;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use tantivy::collector::{Count, DocSetCollector, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, RangeQuery, TermQuery};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::{Index, IndexReader, Order, ReloadPolicy, Searcher, Term};

/// Extract project name from a path and split into TEXT-tokenizer segments.
/// Tantivy's default TEXT tokenizer splits on non-alphanumeric characters,
/// so "/path/to/my-project_name" → ["my", "project", "name"].
fn project_filter_segments(filter: &str) -> Vec<&str> {
    let path = Path::new(filter);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(filter);
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect()
}

fn build_project_query(project_field: Field, filter: &str) -> Box<dyn tantivy::query::Query> {
    let segments = project_filter_segments(filter);
    let segment_queries: Vec<_> = segments
        .iter()
        .map(|seg| {
            let term = Term::from_field_text(project_field, &seg.to_lowercase());
            (
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                    as Box<dyn tantivy::query::Query>,
            )
        })
        .collect();
    Box::new(BooleanQuery::new(segment_queries))
}

/// Build a Boolean AND of TermQuery per hyphen segment of `value`.
/// The TEXT field tokenizes at hyphens, so a UUID or session id must be
/// matched segment by segment rather than as a single term.
fn hyphenated_term_query(field: Field, value: &str) -> BooleanQuery {
    let segment_queries: Vec<_> = value
        .split('-')
        .map(|seg| {
            let term = Term::from_field_text(field, seg);
            (
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                    as Box<dyn tantivy::query::Query>,
            )
        })
        .collect();
    BooleanQuery::new(segment_queries)
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

fn project_matches(project_path: &str, filter: &str) -> bool {
    let filter_name = Path::new(filter)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(filter);
    let result_name = Path::new(project_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(project_path);
    result_name == filter_name
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

    pub fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        let searcher = self
            .reader
            .searcher();

        let query_parser = QueryParser::for_index(
            &self.index,
            vec![
                self.fields
                    .content_field,
                self.fields
                    .session_field,
                self.fields
                    .project_field,
            ],
        );
        let text_query = query_parser.parse_query(&query.text)?;

        let mut final_query_parts = vec![(
            Occur::Must,
            Box::new(text_query) as Box<dyn tantivy::query::Query>,
        )];

        if let Some(ref project_filter) = query.project_filter {
            let project_query = build_project_query(
                self.fields
                    .project_field,
                project_filter,
            );
            final_query_parts.push((Occur::Must, project_query));
        }

        if let Some(ref session_filter) = query.session_filter {
            let session_query = hyphenated_term_query(
                self.fields
                    .session_field,
                session_filter,
            );
            final_query_parts.push((Occur::Must, Box::new(session_query)));
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
                .search(
                    &final_query,
                    &TopDocs::with_limit(query.limit).order_by_score(),
                )?
                .into_iter()
                .map(|(_, addr)| addr)
                .collect(),
            SortOrder::DateDesc => searcher
                .search(
                    &final_query,
                    &TopDocs::with_limit(query.limit)
                        .order_by_fast_field::<tantivy::DateTime>("timestamp", Order::Desc),
                )?
                .into_iter()
                .map(|(_, addr)| addr)
                .collect(),
            SortOrder::DateAsc => searcher
                .search(
                    &final_query,
                    &TopDocs::with_limit(query.limit)
                        .order_by_fast_field::<tantivy::DateTime>("timestamp", Order::Asc),
                )?
                .into_iter()
                .map(|(_, addr)| addr)
                .collect(),
        };

        let mut results = Vec::new();
        for doc_address in doc_addresses {
            let result = self.doc_to_result(&searcher.doc(doc_address)?)?;
            if !self.passes_post_filters(&result, &query) {
                continue;
            }
            results.push(result);
        }

        Ok(results)
    }

    /// Session and project post-filters: precision checks that Tantivy's segment-based
    /// queries cannot express (prefix matching, full name equality).
    fn passes_post_filters(&self, result: &SearchResult, query: &SearchQuery) -> bool {
        if let Some(ref session_filter) = query.session_filter
            && !result
                .session_id
                .starts_with(session_filter.as_str())
        {
            return false;
        }
        if let Some(ref project_filter) = query.project_filter
            && !project_matches(&result.project_path, project_filter)
        {
            return false;
        }
        true
    }

    /// Search with context - returns matches with surrounding messages (grep -C style)
    pub fn search_with_context(
        &self,
        query: SearchQuery,
        context_before: usize,
        context_after: usize,
    ) -> Result<Vec<SearchResultWithContext>> {
        // Save sort order before consuming query
        let sort_by = query
            .sort_by
            .clone();

        // First, get the matching messages
        let matches = self.search(query)?;

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

        // Apply sorting based on sort_by
        match sort_by {
            SortOrder::DateDesc => {
                results_with_context.sort_by(|a, b| {
                    b.matched_message
                        .timestamp
                        .cmp(
                            &a.matched_message
                                .timestamp,
                        )
                });
            }
            SortOrder::DateAsc => {
                results_with_context.sort_by(|a, b| {
                    a.matched_message
                        .timestamp
                        .cmp(
                            &b.matched_message
                                .timestamp,
                        )
                });
            }
            SortOrder::Relevance => {
                // Already sorted by BM25 score from Tantivy
            }
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
        results.retain(|r| {
            r.session_id
                .starts_with(session_id)
        });
        results.sort_by(|a, b| transcript_order(a).cmp(&transcript_order(b)));
        Ok(results)
    }

    /// Get specific messages by their UUIDs
    pub fn get_messages_by_uuid(&self, uuids: &[String]) -> Result<Vec<SearchResult>> {
        let searcher = self
            .reader
            .searcher();
        let mut results = Vec::new();

        for uuid in uuids {
            let query = hyphenated_term_query(
                self.fields
                    .uuid_field,
                uuid,
            );

            let top_docs = searcher.search(&query, &TopDocs::with_limit(10).order_by_score())?;

            for (_, doc_address) in top_docs {
                let result = self.doc_to_result(&searcher.doc(doc_address)?)?;
                // Exact match or prefix match
                if result.uuid == *uuid
                    || result
                        .uuid
                        .starts_with(uuid)
                {
                    results.push(result);
                    break;
                }
            }
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

/// Compile exclusion patterns, refusing an invalid one rather than searching
/// unfiltered.
pub fn compile_exclude_patterns(patterns: &[String]) -> Result<Vec<regex::Regex>> {
    patterns
        .iter()
        .map(|p| regex::Regex::new(p).with_context(|| format!("invalid exclude pattern '{p}'")))
        .collect()
}

/// Post-search filtering: excluded projects, excluded path/project regexes,
/// an optional session to drop (the one currently being written), and the
/// result cap applied after dedup.
#[derive(Debug, Default)]
pub struct ResultFilter {
    pub exclude_projects: Vec<String>,
    pub exclude_regexes: Vec<regex::Regex>,
    pub active_session: Option<String>,
    pub limit: usize,
}

/// Drop excluded projects/patterns and the active session (if set), then
/// deduplicate by session id and cap at `filter.limit`.
pub fn apply_search_filters(
    results: Vec<SearchResultWithContext>,
    filter: &ResultFilter,
) -> Vec<SearchResultWithContext> {
    let mut session_seen = std::collections::HashSet::new();
    results
        .into_iter()
        .filter(|r| {
            let proj = &r
                .matched_message
                .project;
            let path = &r
                .matched_message
                .project_path;
            let session = &r
                .matched_message
                .session_id;

            if let Some(ref active) = filter.active_session
                && session == active
            {
                return false;
            }

            if filter
                .exclude_projects
                .contains(proj)
            {
                return false;
            }
            for regex in &filter.exclude_regexes {
                if regex.is_match(proj) || regex.is_match(path) {
                    return false;
                }
            }
            session_seen.insert(session.clone())
        })
        .take(filter.limit)
        .collect()
}

#[cfg(test)]
mod tests;
