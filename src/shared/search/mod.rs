mod doc;

use super::format::{SearchResultWithContext, self_context_result};
use super::models::{SearchQuery, SearchResult, SortOrder};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::ops::Bound;
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, QueryParser, RangeQuery, TermQuery};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::{Index, IndexReader, Order, ReloadPolicy, Term};

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

/// Maximum messages to retrieve per session.
const MAX_SESSION_MESSAGES: usize = 5000;

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
        let lower = query
            .after
            .map(|dt| tantivy::DateTime::from_timestamp_millis(dt.timestamp_millis()));
        let upper = query
            .before
            .map(|dt| tantivy::DateTime::from_timestamp_millis(dt.timestamp_millis()));
        if lower.is_some() || upper.is_some() {
            let date_query = RangeQuery::new_date_bounds(
                "timestamp".to_string(),
                lower.map_or(Bound::Unbounded, Bound::Included),
                upper.map_or(Bound::Unbounded, Bound::Included),
            );
            final_query_parts.push((
                Occur::Must,
                Box::new(date_query) as Box<dyn tantivy::query::Query>,
            ));
        }

        let final_query = if final_query_parts.len() > 1 {
            Box::new(BooleanQuery::new(final_query_parts)) as Box<dyn tantivy::query::Query>
        } else {
            final_query_parts
                .into_iter()
                .next()
                .unwrap()
                .1
        };

        // The three sort orders use different tantivy collectors (with different
        // Fruit types), so each match arm runs its own search and discards the
        // sort key, leaving a uniform doc address list for the one loop below.
        let doc_addresses: Vec<_> = match &query.sort_by {
            SortOrder::Relevance => searcher
                .search(&*final_query, &TopDocs::with_limit(query.limit))?
                .into_iter()
                .map(|(_, addr)| addr)
                .collect(),
            SortOrder::DateDesc => searcher
                .search(
                    &*final_query,
                    &TopDocs::with_limit(query.limit)
                        .order_by_fast_field::<tantivy::DateTime>("timestamp", Order::Desc),
                )?
                .into_iter()
                .map(|(_, addr)| addr)
                .collect(),
            SortOrder::DateAsc => searcher
                .search(
                    &*final_query,
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

        let mut results_with_context = Vec::new();

        for match_result in matches {
            let session_messages = self.get_session_messages(&match_result.session_id)?;

            // If we can't get session messages, still return the match with just itself as context
            if session_messages.is_empty() {
                results_with_context.push(self_context_result(match_result, 1));
                continue;
            }

            // Sort by sequence number
            let mut session_messages = session_messages;
            session_messages.sort_by_key(|m| m.sequence_num);

            // Count only displayable messages (consistent with get_session_messages)
            let total_session_messages = session_messages
                .iter()
                .filter(|m| m.is_displayable())
                .count();

            // Find the matching message index by UUID or by content/timestamp as fallback
            let match_idx = session_messages
                .iter()
                .position(|m| m.uuid == match_result.uuid)
                .or_else(|| {
                    // Fallback: find by sequence number
                    session_messages
                        .iter()
                        .position(|m| m.sequence_num == match_result.sequence_num)
                });

            if let Some(idx) = match_idx {
                // Get context window around the match
                let start = idx.saturating_sub(context_before);
                let end = (idx + context_after + 1).min(session_messages.len());

                // Filter to displayable messages only, track new match index
                let mut context_messages = Vec::new();
                let mut new_match_idx = 0;
                for (i, msg) in session_messages[start..end]
                    .iter()
                    .enumerate()
                {
                    if msg.is_displayable() {
                        if start + i == idx {
                            new_match_idx = context_messages.len();
                        }
                        context_messages.push(msg.clone());
                    }
                }

                // If no context found (e.g., all filtered out), use match as its own context
                if context_messages.is_empty() {
                    results_with_context
                        .push(self_context_result(match_result, total_session_messages));
                } else {
                    results_with_context.push(SearchResultWithContext {
                        matched_message: match_result,
                        context_messages,
                        match_index: new_match_idx,
                        total_session_messages,
                    });
                }
            } else {
                // UUID/sequence not found in session, return match with itself as context
                results_with_context
                    .push(self_context_result(match_result, total_session_messages));
            }
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

    /// Get all messages for a session
    pub fn get_session_messages(&self, session_id: &str) -> Result<Vec<SearchResult>> {
        let searcher = self
            .reader
            .searcher();

        let query = hyphenated_term_query(
            self.fields
                .session_field,
            session_id,
        );

        let top_docs = searcher.search(&query, &TopDocs::with_limit(MAX_SESSION_MESSAGES))?;

        let mut results = Vec::new();
        for (_, doc_address) in top_docs {
            let result = self.doc_to_result(&searcher.doc(doc_address)?)?;
            // Filter to session_id match - support prefix matching for short IDs
            if result.session_id == session_id
                || result
                    .session_id
                    .starts_with(session_id)
            {
                results.push(result);
            }
        }

        // Sort by sequence number
        results.sort_by_key(|r| r.sequence_num);

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

            let top_docs = searcher.search(&query, &TopDocs::with_limit(10))?;

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
