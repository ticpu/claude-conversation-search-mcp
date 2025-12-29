use super::config::get_config;
use super::models::{SearchQuery, SearchResult};
use super::terminal::file_hyperlink;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, QueryParser, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Value};
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument, Term};

/// Maximum messages to retrieve per session.
/// Claude Code sessions rarely exceed 1000 messages; this limit prevents
/// runaway queries while covering all realistic session sizes.
const MAX_SESSION_MESSAGES: usize = 5000;

pub struct SearchEngine {
    index: Index,
    reader: IndexReader,
    uuid_field: Field,
    parent_uuid_field: Field,
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
    is_sidechain_field: Field,
    agent_id_field: Field,
    interaction_counts: HashMap<String, usize>,
}

impl SearchEngine {
    pub fn new(index_path: &Path) -> Result<Self> {
        let index = Index::open_in_dir(index_path)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        let schema = index.schema();
        let uuid_field = schema.get_field("uuid")?;
        let parent_uuid_field = schema.get_field("parent_uuid")?;
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
        let is_sidechain_field = schema.get_field("is_sidechain")?;
        let agent_id_field = schema.get_field("agent_id")?;

        let mut search_engine = Self {
            index,
            reader,
            uuid_field,
            parent_uuid_field,
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
            is_sidechain_field,
            agent_id_field,
            interaction_counts: HashMap::new(),
        };

        search_engine.populate_interaction_counts()?;
        Ok(search_engine)
    }

    pub fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        let searcher = self.reader.searcher();

        let query_parser = QueryParser::for_index(
            &self.index,
            vec![self.content_field, self.session_field, self.project_field],
        );
        let text_query = query_parser.parse_query(&query.text)?;

        let mut final_query_parts = vec![(
            Occur::Must,
            Box::new(text_query) as Box<dyn tantivy::query::Query>,
        )];

        if let Some(project_filter) = query.project_filter {
            let project_term = Term::from_field_text(self.project_field, &project_filter);
            let project_query =
                TermQuery::new(project_term, tantivy::schema::IndexRecordOption::Basic);
            final_query_parts.push((Occur::Must, Box::new(project_query)));
        }

        if let Some(session_filter) = query.session_filter {
            let session_term = Term::from_field_text(self.session_field, &session_filter);
            let session_query =
                TermQuery::new(session_term, tantivy::schema::IndexRecordOption::Basic);
            final_query_parts.push((Occur::Must, Box::new(session_query)));
        }

        let final_query = if final_query_parts.len() > 1 {
            Box::new(BooleanQuery::new(final_query_parts)) as Box<dyn tantivy::query::Query>
        } else {
            final_query_parts.into_iter().next().unwrap().1
        };

        let top_docs = searcher.search(&*final_query, &TopDocs::with_limit(query.limit))?;

        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let result = self.doc_to_result(&searcher.doc(doc_address)?, score, &query.text)?;
            results.push(result);
        }

        Ok(results)
    }

    /// Search with context - returns matches with surrounding messages (grep -C style)
    pub fn search_with_context(
        &self,
        query: SearchQuery,
        context_before: usize,
        context_after: usize,
    ) -> Result<Vec<SearchResultWithContext>> {
        // First, get the matching messages
        let matches = self.search(query)?;

        let mut results_with_context = Vec::new();

        for match_result in matches {
            let session_messages = self.get_session_messages(&match_result.session_id)?;
            let total_session_messages = session_messages.len();

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
                for (i, msg) in session_messages[start..end].iter().enumerate() {
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

        Ok(results_with_context)
    }

    /// Get all messages for a session
    pub fn get_session_messages(&self, session_id: &str) -> Result<Vec<SearchResult>> {
        let searcher = self.reader.searcher();

        // Use TermQuery on each UUID segment for exact matching
        // Session IDs are UUIDs like "9e1e6a58-cd5a-4651-a9fd-c24c04cb8809"
        // TEXT field tokenizes at hyphens, so we match all segments with AND
        let segments: Vec<_> = session_id.split('-').collect();
        let segment_queries: Vec<_> = segments
            .iter()
            .map(|seg| {
                let term = Term::from_field_text(self.session_field, seg);
                (
                    Occur::Must,
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                        as Box<dyn tantivy::query::Query>,
                )
            })
            .collect();
        let query = BooleanQuery::new(segment_queries);

        let top_docs = searcher.search(&query, &TopDocs::with_limit(MAX_SESSION_MESSAGES))?;

        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let result = self.doc_to_result(&searcher.doc(doc_address)?, score, "")?;
            // Filter to session_id match - support prefix matching for short IDs
            if result.session_id == session_id || result.session_id.starts_with(session_id) {
                results.push(result);
            }
        }

        // Sort by sequence number
        results.sort_by_key(|r| r.sequence_num);

        Ok(results)
    }

    fn doc_to_result(
        &self,
        doc: &TantivyDocument,
        score: f32,
        query_text: &str,
    ) -> Result<SearchResult> {
        let uuid = doc
            .get_first(self.uuid_field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let parent_uuid = doc
            .get_first(self.parent_uuid_field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

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

        let message_type = doc
            .get_first(self.message_type_field)
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();

        let snippet = if query_text.is_empty() {
            self.truncate_content(&content, 150)
        } else {
            self.generate_snippet(&content, query_text)
        };

        let technologies = doc
            .get_first(self.technologies_field)
            .and_then(|v| v.as_str())
            .map(|s| s.split_whitespace().map(|s| s.to_string()).collect())
            .unwrap_or_default();

        let code_languages = doc
            .get_first(self.code_languages_field)
            .and_then(|v| v.as_str())
            .map(|s| s.split_whitespace().map(|s| s.to_string()).collect())
            .unwrap_or_default();

        let tools_mentioned = doc
            .get_first(self.tools_mentioned_field)
            .and_then(|v| v.as_str())
            .map(|s| s.split_whitespace().map(|s| s.to_string()).collect())
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

        let is_sidechain = doc
            .get_first(self.is_sidechain_field)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let agent_id = doc
            .get_first(self.agent_id_field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let interaction_count = self.get_interaction_count(&session_id);

        Ok(SearchResult {
            uuid,
            parent_uuid,
            content,
            project,
            project_path,
            session_id,
            timestamp,
            score,
            snippet,
            technologies,
            code_languages,
            tools_mentioned,
            has_code,
            has_error,
            interaction_count,
            sequence_num,
            is_sidechain,
            agent_id,
            message_type,
        })
    }

    fn truncate_content(&self, content: &str, max_chars: usize) -> String {
        if content.chars().count() <= max_chars {
            content.to_string()
        } else {
            let truncated: String = content.chars().take(max_chars - 1).collect();
            format!("{}…", truncated)
        }
    }

    fn generate_snippet(&self, content: &str, query: &str) -> String {
        let words: Vec<&str> = content.split_whitespace().collect();
        let query_words: Vec<&str> = query.split_whitespace().collect();

        if words.len() <= 30 {
            return content.to_string();
        }

        let mut best_start = 0;
        let mut best_score = 0;

        for (i, window) in words.windows(30).enumerate() {
            let window_text = window.join(" ");
            let mut score = 0;

            for query_word in &query_words {
                if window_text
                    .to_lowercase()
                    .contains(&query_word.to_lowercase())
                {
                    score += 1;
                }
            }

            if score > best_score {
                best_score = score;
                best_start = i;
            }
        }

        let snippet_words = &words[best_start..std::cmp::min(best_start + 30, words.len())];
        let mut snippet = snippet_words.join(" ");

        if best_start > 0 {
            snippet = format!("...{snippet}");
        }
        if best_start + 30 < words.len() {
            snippet = format!("{snippet}...");
        }

        snippet
    }

    fn populate_interaction_counts(&mut self) -> Result<()> {
        let config = get_config();
        let claude_dir = config.get_claude_dir()?;
        let pattern = claude_dir.join("projects/**/*.jsonl");
        let pattern_str = pattern.to_string_lossy();

        for entry in glob::glob(&pattern_str)? {
            let file_path = entry?;
            if let Ok(content) = std::fs::read_to_string(&file_path) {
                let mut session_counts: HashMap<String, usize> = HashMap::new();

                for line in content.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }

                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(line)
                        && let Some(session_id) = json.get("sessionId").and_then(|v| v.as_str())
                    {
                        let msg_type = json.get("type").and_then(|v| v.as_str());
                        // Only count user/assistant messages
                        if matches!(msg_type, Some("user") | Some("assistant")) {
                            *session_counts.entry(session_id.to_string()).or_insert(0) += 1;
                        }
                    }
                }

                for (session_id, count) in session_counts {
                    *self.interaction_counts.entry(session_id).or_insert(0) += count;
                }
            }
        }

        Ok(())
    }

    fn get_interaction_count(&self, session_id: &str) -> usize {
        self.interaction_counts
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }

    pub fn get_all_documents(
        &self,
        project_filter: Option<String>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let searcher = self.reader.searcher();

        let query: Box<dyn tantivy::query::Query> = if let Some(project_filter) = project_filter {
            let project_term = Term::from_field_text(self.project_field, &project_filter);
            Box::new(TermQuery::new(
                project_term,
                tantivy::schema::IndexRecordOption::Basic,
            ))
        } else {
            Box::new(tantivy::query::AllQuery)
        };

        let top_docs = searcher.search(&*query, &TopDocs::with_limit(limit))?;

        let mut results = Vec::new();
        for (_score, doc_address) in top_docs {
            let result = self.doc_to_result(&searcher.doc(doc_address)?, 1.0, "")?;
            results.push(result);
        }

        Ok(results)
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

/// Safely truncate string at UTF-8 character boundary
fn truncate_content(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        // Collapse whitespace for dense output
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        let truncated: String = s.chars().take(max_chars - 1).collect();
        let collapsed = truncated.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("{}…", collapsed)
    }
}

impl SearchResultWithContext {
    /// Format as grep -C style output - compact and dense
    /// Format: N. 📁 ~/path 🗒️ session (M msgs) 💬 msg_uuid
    ///            User: content preview...
    ///         »  AI: matched content...
    ///            User: content...
    pub fn format_compact(&self, index: usize) -> String {
        let mut output = String::new();
        let config = get_config();
        let claude_dir = config.get_claude_dir().unwrap_or_default();

        // Get full project path, replace $HOME with ~
        let home = std::env::var("HOME").unwrap_or_default();
        let project_path_full = if !self.matched_message.project_path.is_empty()
            && self.matched_message.project_path != "unknown"
        {
            self.matched_message.project_path.clone()
        } else {
            format!("{}/{}", home, self.matched_message.project)
        };
        let project_path_display = project_path_full.replace(&home, "~");

        // Build JSONL file path for session hyperlink
        // Claude uses format: -home-user-path-to-project (slashes and dots become dashes)
        let session_id = &self.matched_message.session_id;
        let project_dir_name = project_path_full.replace(['/', '.'], "-");
        let jsonl_path = claude_dir
            .join("projects")
            .join(&project_dir_name)
            .join(format!("{}.jsonl", session_id));
        let jsonl_path_str = jsonl_path.to_string_lossy();

        let short_session = &session_id[..8.min(session_id.len())];
        let short_msg = &self.matched_message.uuid[..8.min(self.matched_message.uuid.len())];

        // Create hyperlinks
        let path_link = file_hyperlink(&project_path_full, &project_path_display);
        let session_link = file_hyperlink(&jsonl_path_str, short_session);

        // Header: N. 📁 path 🗒️ session (M msgs) 💬 msg_uuid
        output.push_str(&format!(
            "{}. 📁 {} 🗒️ {} ({} msgs) 💬 {}\n",
            index + 1,
            path_link,
            session_link,
            self.total_session_messages,
            short_msg,
        ));

        // Tags line if any metadata present
        let mut tags = Vec::new();
        tags.extend(self.matched_message.technologies.iter().take(3).cloned());
        tags.extend(self.matched_message.code_languages.iter().take(2).cloned());
        if self.matched_message.has_error {
            tags.push("error".to_string());
        }
        if !tags.is_empty() {
            output.push_str(&format!("🎟️{}\n", tags.join(",")));
        }

        // Context messages with » marker for match
        for (i, msg) in self.context_messages.iter().enumerate() {
            let role = match msg.message_type.as_str() {
                "User" => "User",
                "Assistant" => "AI",
                "Summary" => "Sum",
                _ => "?",
            };

            let prefix = if i == self.match_index { "»  " } else { "   " };
            let content = truncate_content(&msg.content, 300);

            output.push_str(&format!("{}{}: {}\n", prefix, role, content));
        }

        output
    }

    /// Format with more detail for verbose output
    pub fn format_verbose(&self, index: usize) -> String {
        let mut output = String::new();

        output.push_str(&format!(
            "{}. [{}] {} | {} | score: {:.2}\n",
            index + 1,
            self.matched_message.project,
            self.matched_message.timestamp.format("%Y-%m-%d %H:%M"),
            &self.matched_message.session_id[..12.min(self.matched_message.session_id.len())],
            self.matched_message.score,
        ));
        output.push_str(&format!(
            "   {} msgs in session | uuid: {}\n",
            self.total_session_messages,
            &self.matched_message.uuid[..12.min(self.matched_message.uuid.len())],
        ));

        // Metadata tags on one line
        let mut tags = Vec::new();
        if !self.matched_message.technologies.is_empty() {
            tags.push(self.matched_message.technologies.join(","));
        }
        if !self.matched_message.code_languages.is_empty() {
            tags.push(self.matched_message.code_languages.join(","));
        }
        if self.matched_message.has_code {
            tags.push("code".to_string());
        }
        if self.matched_message.has_error {
            tags.push("error".to_string());
        }
        if !tags.is_empty() {
            output.push_str(&format!("   tags: {}\n", tags.join(" ")));
        }

        // Context messages
        for (i, msg) in self.context_messages.iter().enumerate() {
            let role = match msg.message_type.as_str() {
                "User" => "User",
                "Assistant" => "AI",
                "Summary" => "Sum",
                _ => "?",
            };

            let prefix = if i == self.match_index { ">> " } else { "   " };
            let content = truncate_content(&msg.content, 500);

            output.push_str(&format!("{}{}: {}\n", prefix, role, content));
        }

        output
    }
}
