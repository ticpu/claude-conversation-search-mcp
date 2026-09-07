use super::models::{MessageType, SearchQuery, SearchResult, SortOrder};
use super::path_utils::{home_to_tilde, session_jsonl_path, short_uuid};
use super::terminal::file_hyperlink;
use super::utils::truncate_content;
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::ops::Bound;
use std::path::Path;
use std::str::FromStr;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, QueryParser, RangeQuery, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Value};
use tantivy::{Index, IndexReader, Order, ReloadPolicy, TantivyDocument, Term};

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
    uuid_field: Field,
    content_field: Field,
    project_field: Field,
    session_field: Field,
    timestamp_field: Field,
    message_type_field: Field,
    technologies_field: Field,
    code_languages_field: Field,
    tools_mentioned_field: Field,
    has_code_field: Field,
    has_error_field: Field,
    cwd_field: Field,
    sequence_num_field: Field,
    interaction_counts: HashMap<String, usize>,
}

impl SearchEngine {
    pub fn new(index_path: &Path, session_counts: HashMap<String, usize>) -> Result<Self> {
        let index = Index::open_in_dir(index_path)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        let schema = index.schema();
        let uuid_field = schema.get_field("uuid")?;
        let content_field = schema.get_field("content")?;
        let project_field = schema.get_field("project")?;
        let session_field = schema.get_field("session_id")?;
        let timestamp_field = schema.get_field("timestamp")?;
        let message_type_field = schema.get_field("message_type")?;
        let technologies_field = schema.get_field("technologies")?;
        let code_languages_field = schema.get_field("code_languages")?;
        let tools_mentioned_field = schema.get_field("tools_mentioned")?;
        let has_code_field = schema.get_field("has_code")?;
        let has_error_field = schema.get_field("has_error")?;
        let cwd_field = schema.get_field("cwd")?;
        let sequence_num_field = schema.get_field("sequence_num")?;

        Ok(Self {
            index,
            reader,
            uuid_field,
            content_field,
            project_field,
            session_field,
            timestamp_field,
            message_type_field,
            technologies_field,
            code_languages_field,
            tools_mentioned_field,
            has_code_field,
            has_error_field,
            cwd_field,
            sequence_num_field,
            interaction_counts: session_counts,
        })
    }

    pub fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        let searcher = self
            .reader
            .searcher();

        let query_parser = QueryParser::for_index(
            &self.index,
            vec![self.content_field, self.session_field, self.project_field],
        );
        let text_query = query_parser.parse_query(&query.text)?;

        let mut final_query_parts = vec![(
            Occur::Must,
            Box::new(text_query) as Box<dyn tantivy::query::Query>,
        )];

        if let Some(ref project_filter) = query.project_filter {
            let project_query = build_project_query(self.project_field, project_filter);
            final_query_parts.push((Occur::Must, project_query));
        }

        if let Some(ref session_filter) = query.session_filter {
            let session_query = hyphenated_term_query(self.session_field, session_filter);
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

        let mut results = Vec::new();

        match &query.sort_by {
            SortOrder::Relevance => {
                let top_docs = searcher.search(&*final_query, &TopDocs::with_limit(query.limit))?;
                for (_, doc_address) in top_docs {
                    let result = self.doc_to_result(&searcher.doc(doc_address)?)?;
                    if !self.passes_post_filters(&result, &query) {
                        continue;
                    }
                    results.push(result);
                }
            }
            SortOrder::DateDesc => {
                let collector = TopDocs::with_limit(query.limit)
                    .order_by_fast_field::<tantivy::DateTime>("timestamp", Order::Desc);
                let top_docs = searcher.search(&*final_query, &collector)?;
                for (_, doc_address) in top_docs {
                    let result = self.doc_to_result(&searcher.doc(doc_address)?)?;
                    if !self.passes_post_filters(&result, &query) {
                        continue;
                    }
                    results.push(result);
                }
            }
            SortOrder::DateAsc => {
                let collector = TopDocs::with_limit(query.limit)
                    .order_by_fast_field::<tantivy::DateTime>("timestamp", Order::Asc);
                let top_docs = searcher.search(&*final_query, &collector)?;
                for (_, doc_address) in top_docs {
                    let result = self.doc_to_result(&searcher.doc(doc_address)?)?;
                    if !self.passes_post_filters(&result, &query) {
                        continue;
                    }
                    results.push(result);
                }
            }
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
                results_with_context.push(SearchResultWithContext {
                    matched_message: match_result.clone(),
                    context_messages: vec![match_result],
                    match_index: 0,
                    total_session_messages: 1,
                });
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
                    context_messages.push(match_result.clone());
                    new_match_idx = 0;
                }

                results_with_context.push(SearchResultWithContext {
                    matched_message: match_result,
                    context_messages,
                    match_index: new_match_idx,
                    total_session_messages,
                });
            } else {
                // UUID/sequence not found in session, return match with itself as context
                results_with_context.push(SearchResultWithContext {
                    matched_message: match_result.clone(),
                    context_messages: vec![match_result],
                    match_index: 0,
                    total_session_messages,
                });
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

        let query = hyphenated_term_query(self.session_field, session_id);

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
            let query = hyphenated_term_query(self.uuid_field, uuid);

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

    fn doc_to_result(&self, doc: &TantivyDocument) -> Result<SearchResult> {
        let uuid = doc
            .get_first(self.uuid_field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let content = doc
            .get_first(self.content_field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let project = doc
            .get_first(self.project_field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let project_path = doc
            .get_first(self.cwd_field)
            .and_then(|v| v.as_str())
            .unwrap_or(&project)
            .to_string();

        let session_id = doc
            .get_first(self.session_field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let timestamp = doc
            .get_first(self.timestamp_field)
            .and_then(|v| v.as_datetime())
            .map(|dt| {
                DateTime::from_timestamp_millis(dt.into_timestamp_millis()).unwrap_or_else(Utc::now)
            })
            .unwrap_or_else(Utc::now);

        let stored_type = doc
            .get_first(self.message_type_field)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("document {uuid} has no message_type field"))?;
        let message_type = MessageType::from_str(stored_type)
            .with_context(|| format!("document {uuid} carries an unindexable message type"))?;

        let technologies = doc
            .get_first(self.technologies_field)
            .and_then(|v| v.as_str())
            .map(|s| {
                s.split_whitespace()
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let code_languages = doc
            .get_first(self.code_languages_field)
            .and_then(|v| v.as_str())
            .map(|s| {
                s.split_whitespace()
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let tools_mentioned = doc
            .get_first(self.tools_mentioned_field)
            .and_then(|v| v.as_str())
            .map(|s| {
                s.split_whitespace()
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let has_code = doc
            .get_first(self.has_code_field)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let has_error = doc
            .get_first(self.has_error_field)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let sequence_num = doc
            .get_first(self.sequence_num_field)
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let interaction_count = self.get_interaction_count(&session_id);

        Ok(SearchResult {
            uuid,
            content,
            project,
            project_path,
            session_id,
            timestamp,
            technologies,
            code_languages,
            tools_mentioned,
            has_code,
            has_error,
            interaction_count,
            sequence_num,
            message_type,
        })
    }

    fn get_interaction_count(&self, session_id: &str) -> usize {
        self.interaction_counts
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }
}

/// Search result with surrounding context messages
#[derive(Debug, Clone)]
pub struct SearchResultWithContext {
    pub matched_message: SearchResult,
    pub context_messages: Vec<SearchResult>,
    pub match_index: usize,
    pub total_session_messages: usize,
}

/// Options for what to include in search result display
#[derive(Debug, Clone)]
pub struct DisplayOptions {
    pub include_thinking: bool,
    pub include_tools: bool,
    /// Characters shown per message around match (0 = full content)
    pub truncate_length: usize,
}

impl Default for DisplayOptions {
    fn default() -> Self {
        Self {
            include_thinking: false,
            include_tools: false,
            truncate_length: 300,
        }
    }
}

/// Filter content based on display options
pub(crate) fn filter_content(s: &str, opts: &DisplayOptions) -> Option<String> {
    // Check if content should be hidden
    if !opts.include_thinking && s.starts_with("[thinking]") {
        return None;
    }
    if !opts.include_tools
        && (s.starts_with('[') && s.contains(']') && !s.starts_with("[result]"))
        && !s.starts_with("[thinking]")
    {
        // Looks like a tool call [ToolName] {...}
        if let Some(bracket_end) = s.find(']') {
            let prefix = &s[1..bracket_end];
            // Tool names are typically CamelCase or contain underscores/colons
            if prefix
                .chars()
                .any(|c| c.is_uppercase() || c == '_' || c == ':')
                && !prefix.contains(' ')
            {
                return None;
            }
        }
    }
    Some(s.to_string())
}

impl SearchResultWithContext {
    /// Format with display options
    pub fn format_compact_with_options(&self, index: usize, opts: &DisplayOptions) -> String {
        let mut output = String::new();

        let project_path_full = &self
            .matched_message
            .project_path;
        let project_path_display = home_to_tilde(project_path_full);
        let session_id = &self
            .matched_message
            .session_id;

        let jsonl_path = session_jsonl_path(project_path_full, session_id).unwrap_or_default();
        let jsonl_path_str = jsonl_path.to_string_lossy();

        let short_session = short_uuid(session_id);
        let short_msg = short_uuid(
            &self
                .matched_message
                .uuid,
        );

        let path_link = file_hyperlink(project_path_full, &project_path_display);
        let session_link = file_hyperlink(&jsonl_path_str, short_session);

        output.push_str(&format!(
            "{}. 📁 {} 🗒️ {} ({} msgs) 💬 {} 📅 {}\n",
            index + 1,
            path_link,
            session_link,
            self.total_session_messages,
            short_msg,
            self.matched_message
                .timestamp
                .format("%Y-%m-%d %H:%M"),
        ));

        let mut tags = Vec::new();
        tags.extend(
            self.matched_message
                .technologies
                .iter()
                .take(3)
                .cloned(),
        );
        tags.extend(
            self.matched_message
                .code_languages
                .iter()
                .take(2)
                .cloned(),
        );
        if self
            .matched_message
            .has_error
        {
            tags.push("error".to_string());
        }
        if !tags.is_empty() {
            output.push_str(&format!("🎟️{}\n", tags.join(",")));
        }

        self.format_context_messages(&mut output, opts);
        output
    }

    fn format_context_messages(&self, output: &mut String, opts: &DisplayOptions) {
        for (i, msg) in self
            .context_messages
            .iter()
            .enumerate()
        {
            // Filter content based on options
            if filter_content(&msg.content, opts).is_none() {
                continue;
            }

            let prefix = if i == self.match_index { "»  " } else { "   " };
            let content = if opts.truncate_length == 0 {
                msg.content
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                truncate_content(&msg.content, opts.truncate_length, true)
            };

            output.push_str(&format!(
                "{}{}: {}\n",
                prefix,
                msg.message_type
                    .short_name(),
                content
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::indexer::SearchIndexer;
    use crate::shared::models::{EntryBuilder, MessageType};
    use chrono::Utc;
    use tempfile::TempDir;

    #[test]
    fn test_get_session_messages_returns_all_indexed() {
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path();

        // Create 100 messages for a session
        let session_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let entries: Vec<_> = (0..100)
            .map(|i| {
                let msg_type = if i % 2 == 0 {
                    MessageType::User
                } else {
                    MessageType::Assistant
                };
                EntryBuilder::new(&format!("uuid-{:04}", i), session_id)
                    .message_type(msg_type)
                    .content(&format!("Message {}", i))
                    .sequence_num(i)
                    .build()
            })
            .collect();

        // Index them
        let mut indexer = SearchIndexer::new(index_path).unwrap();
        indexer
            .index_conversations(entries, "test-source.jsonl")
            .unwrap();
        indexer
            .commit()
            .unwrap();
        drop(indexer);

        // Retrieve with SearchEngine
        let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();
        let messages = engine
            .get_session_messages(session_id)
            .unwrap();

        assert_eq!(
            messages.len(),
            100,
            "Should retrieve all 100 indexed messages"
        );
    }

    #[test]
    fn test_get_session_messages_with_short_id() {
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path();

        let session_id = "12345678-abcd-efgh-ijkl-mnopqrstuvwx";
        let entries = vec![
            EntryBuilder::new("uuid-1", session_id)
                .content("Hello")
                .build(),
            EntryBuilder::new("uuid-2", session_id)
                .message_type(MessageType::Assistant)
                .content("Hi there")
                .sequence_num(1)
                .build(),
        ];

        let mut indexer = SearchIndexer::new(index_path).unwrap();
        indexer
            .index_conversations(entries, "test-source.jsonl")
            .unwrap();
        indexer
            .commit()
            .unwrap();
        drop(indexer);

        let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

        // Test with short ID (first 8 chars)
        let messages = engine
            .get_session_messages("12345678")
            .unwrap();
        assert_eq!(
            messages.len(),
            2,
            "Should find messages with short session ID"
        );
    }

    #[test]
    fn test_project_filter_with_full_path() {
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path();

        let session_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let entries = vec![
            EntryBuilder::new("uuid-1", session_id)
                .content("hello world")
                .project("my-cool-project", "/home/user/GIT/my-cool-project")
                .build(),
            EntryBuilder::new("uuid-2", session_id)
                .message_type(MessageType::Assistant)
                .content("hi there")
                .sequence_num(1)
                .project("my-cool-project", "/home/user/GIT/my-cool-project")
                .build(),
            EntryBuilder::new("uuid-3", session_id)
                .content("other stuff")
                .sequence_num(2)
                .project("other-project", "/home/user/GIT/other-project")
                .build(),
        ];

        let mut indexer = SearchIndexer::new(index_path).unwrap();
        indexer
            .index_conversations(entries, "test-source.jsonl")
            .unwrap();
        indexer
            .commit()
            .unwrap();
        drop(indexer);

        let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

        // Filter by full path (how users pass --project)
        let results = engine
            .search(SearchQuery {
                text: "hello".to_string(),
                limit: 10,
                project_filter: Some("/home/user/GIT/my-cool-project".to_string()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(
            results.len(),
            1,
            "Should find 1 result with full path project filter"
        );
        assert_eq!(results[0].uuid, "uuid-1");

        // Filter by short project name
        let results = engine
            .search(SearchQuery {
                text: "hello".to_string(),
                limit: 10,
                project_filter: Some("my-cool-project".to_string()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(
            results.len(),
            1,
            "Should find 1 result with short project name filter"
        );

        // Filter should exclude non-matching projects
        let results = engine
            .search(SearchQuery {
                text: "hello".to_string(),
                limit: 10,
                project_filter: Some("other-project".to_string()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(results.len(), 0, "Should find 0 results for wrong project");
    }

    #[test]
    fn test_session_filter_with_full_uuid() {
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path();

        let session_a = "aaaaaaaa-1111-2222-3333-444444444444";
        let session_b = "bbbbbbbb-5555-6666-7777-888888888888";
        let entries = vec![
            EntryBuilder::new("uuid-1", session_a)
                .content("hello world")
                .build(),
            EntryBuilder::new("uuid-2", session_b)
                .content("hello world")
                .build(),
        ];

        let mut indexer = SearchIndexer::new(index_path).unwrap();
        indexer
            .index_conversations(entries, "test-source.jsonl")
            .unwrap();
        indexer
            .commit()
            .unwrap();
        drop(indexer);

        let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

        // Full session ID
        let results = engine
            .search(SearchQuery {
                text: "hello".to_string(),
                limit: 10,
                session_filter: Some(session_a.to_string()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(results.len(), 1, "Should find 1 result for session A");
        assert_eq!(results[0].uuid, "uuid-1");

        // Short prefix
        let results = engine
            .search(SearchQuery {
                text: "hello".to_string(),
                limit: 10,
                session_filter: Some("aaaaaaaa".to_string()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(
            results.len(),
            1,
            "Should find 1 result with short session prefix"
        );
        assert_eq!(results[0].uuid, "uuid-1");
    }

    #[test]
    fn test_get_session_messages_by_prefix() {
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path();

        let session_id = "aabbccdd-1122-3344-5566-778899001122";
        let entries = vec![
            EntryBuilder::new("uuid-1", session_id)
                .content("first")
                .build(),
            EntryBuilder::new("uuid-2", session_id)
                .message_type(MessageType::Assistant)
                .content("second")
                .sequence_num(1)
                .build(),
            EntryBuilder::new("uuid-3", session_id)
                .content("third")
                .sequence_num(2)
                .build(),
        ];

        let mut indexer = SearchIndexer::new(index_path).unwrap();
        indexer
            .index_conversations(entries, "test-source.jsonl")
            .unwrap();
        indexer
            .commit()
            .unwrap();
        drop(indexer);

        let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

        // Full ID
        let messages = engine
            .get_session_messages(session_id)
            .unwrap();
        assert_eq!(messages.len(), 3);

        // Short prefix
        let messages = engine
            .get_session_messages("aabbccdd")
            .unwrap();
        assert_eq!(
            messages.len(),
            3,
            "Should find all messages with short prefix"
        );

        // Non-matching prefix
        let messages = engine
            .get_session_messages("xxxxxxxx")
            .unwrap();
        assert_eq!(
            messages.len(),
            0,
            "Should find no messages for wrong prefix"
        );
    }

    #[test]
    fn test_displayable_count_matches_retrieval() {
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path();

        let session_id = "testtest-1234-5678-abcd-ef0123456789";
        let entries = vec![
            EntryBuilder::new("uuid-1", session_id)
                .content("User message")
                .build(),
            EntryBuilder::new("uuid-2", session_id)
                .message_type(MessageType::Assistant)
                .content("Assistant message")
                .sequence_num(1)
                .build(),
            EntryBuilder::new("uuid-3", session_id)
                .message_type(MessageType::System)
                .content("System message")
                .sequence_num(2)
                .build(),
            EntryBuilder::new("uuid-4", session_id)
                .message_type(MessageType::Summary)
                .content("Summary")
                .sequence_num(3)
                .build(),
            EntryBuilder::new("uuid-5", session_id)
                .content("Warmup")
                .sequence_num(4)
                .build(),
        ];

        let mut indexer = SearchIndexer::new(index_path).unwrap();
        indexer
            .index_conversations(entries, "test-source.jsonl")
            .unwrap();
        indexer
            .commit()
            .unwrap();
        drop(indexer);

        let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();
        let messages = engine
            .get_session_messages(session_id)
            .unwrap();

        // Count displayable
        let displayable_count = messages
            .iter()
            .filter(|m| m.is_displayable())
            .count();
        // User, Assistant, Summary are displayable; System is not; "Warmup" content filtered
        assert_eq!(
            displayable_count, 3,
            "Should have 3 displayable messages (User, Assistant, Summary)"
        );
    }

    #[test]
    fn test_date_filter_pushed_into_query() {
        // Old doc has high term frequency for "common"; new doc has one occurrence.
        // Without the date filter in the query, top-1 by BM25 fetches only the old doc
        // and the post-filter drops it, returning nothing.
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path();

        let session_id = "dddddddd-1111-2222-3333-444444444444";
        let old_ts = "2025-01-01T00:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        let new_ts = "2026-07-01T12:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();

        let old_content = "common ".repeat(20);
        let entries = vec![
            EntryBuilder::new("old-uuid", session_id)
                .content(old_content.trim())
                .timestamp(old_ts)
                .build(),
            EntryBuilder::new("new-uuid", session_id)
                .content("common recent doc")
                .sequence_num(1)
                .timestamp(new_ts)
                .build(),
        ];

        let mut indexer = SearchIndexer::new(index_path).unwrap();
        indexer
            .index_conversations(entries, "test-source.jsonl")
            .unwrap();
        indexer
            .commit()
            .unwrap();
        drop(indexer);

        let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

        // After filter set to 2026-06-01 — old doc must be excluded from the corpus.
        let after = "2026-06-01T00:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        let results = engine
            .search(SearchQuery {
                text: "common".to_string(),
                limit: 1,
                after: Some(after),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(
            results.len(),
            1,
            "Date filter in query must surface the recent doc even with limit 1; got: {:?}",
            results
                .iter()
                .map(|r| (&r.uuid, r.timestamp))
                .collect::<Vec<_>>()
        );
        assert_eq!(results[0].uuid, "new-uuid");
    }

    #[test]
    fn test_date_sort_in_collector() {
        // Without collector-level sort, DateDesc limit=1 could return the high-BM25 old doc.
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path();

        let session_id = "eeeeeeee-5555-6666-7777-888888888888";
        let old_ts = "2025-03-01T00:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        let new_ts = "2026-07-10T08:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();

        let old_content = "sort ".repeat(15);
        let entries = vec![
            EntryBuilder::new("old-sort", session_id)
                .content(old_content.trim())
                .timestamp(old_ts)
                .build(),
            EntryBuilder::new("new-sort", session_id)
                .content("sort newer")
                .sequence_num(1)
                .timestamp(new_ts)
                .build(),
        ];

        let mut indexer = SearchIndexer::new(index_path).unwrap();
        indexer
            .index_conversations(entries, "test-source.jsonl")
            .unwrap();
        indexer
            .commit()
            .unwrap();
        drop(indexer);

        let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

        let results = engine
            .search(SearchQuery {
                text: "sort".to_string(),
                limit: 1,
                sort_by: SortOrder::DateDesc,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(results.len(), 1, "Should return exactly 1 result");
        assert_eq!(
            results[0].uuid, "new-sort",
            "DateDesc limit=1 must return the newest doc, not the highest-BM25 one"
        );
    }
}
