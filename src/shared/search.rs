use super::config::get_config;
use super::models::{SearchQuery, SearchResult};
use super::utils::extract_content_from_json;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, QueryParser, TermQuery};
use tantivy::schema::{Field, Value};
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument, Term};

pub struct SearchEngine {
    index: Index,
    reader: IndexReader,
    content_field: Field,
    project_field: Field,
    session_field: Field,
    timestamp_field: Field,
    technologies_field: Option<Field>,
    code_languages_field: Option<Field>,
    tools_mentioned_field: Option<Field>,
    has_code_field: Option<Field>,
    has_error_field: Option<Field>,
    cwd_field: Option<Field>,
    sequence_num_field: Option<Field>,
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
        let content_field = schema.get_field("content")?;
        let project_field = schema.get_field("project")?;
        let session_field = schema.get_field("session_id")?;
        let timestamp_field = schema.get_field("timestamp")?;

        // Try to get new metadata fields (may not exist in older indexes)
        let technologies_field = schema.get_field("technologies").ok();
        let code_languages_field = schema.get_field("code_languages").ok();
        let tools_mentioned_field = schema.get_field("tools_mentioned").ok();
        let has_code_field = schema.get_field("has_code").ok();
        let has_error_field = schema.get_field("has_error").ok();
        let cwd_field = schema.get_field("cwd").ok();
        let sequence_num_field = schema.get_field("sequence_num").ok();

        let mut search_engine = Self {
            index,
            reader,
            content_field,
            project_field,
            session_field,
            timestamp_field,
            technologies_field,
            code_languages_field,
            tools_mentioned_field,
            has_code_field,
            has_error_field,
            cwd_field,
            sequence_num_field,
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

        // Add project filter if specified
        if let Some(project_filter) = query.project_filter {
            let project_term = Term::from_field_text(self.project_field, &project_filter);
            let project_query =
                TermQuery::new(project_term, tantivy::schema::IndexRecordOption::Basic);
            final_query_parts.push((Occur::Must, Box::new(project_query)));
        }

        // Add session filter if specified
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
            let retrieved_doc: TantivyDocument = searcher.doc(doc_address)?;

            let content = retrieved_doc
                .get_first(self.content_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let project = retrieved_doc
                .get_first(self.project_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let project_path = if let Some(cwd_field) = self.cwd_field {
                retrieved_doc
                    .get_first(cwd_field)
                    .and_then(|v| v.as_str())
                    .unwrap_or(&project)
                    .to_string()
            } else {
                project.clone()
            };

            let session_id = retrieved_doc
                .get_first(self.session_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let timestamp = retrieved_doc
                .get_first(self.timestamp_field)
                .and_then(|v| v.as_datetime())
                .map(|dt| {
                    // Convert from tantivy::DateTime to chrono::DateTime<Utc>
                    let timestamp_millis = dt.into_timestamp_millis();
                    DateTime::from_timestamp_millis(timestamp_millis).unwrap_or_else(Utc::now)
                })
                .unwrap_or_else(Utc::now);

            let snippet = self.generate_snippet(&content, &query.text);

            // Extract metadata fields (with fallbacks for older indexes)
            let technologies = self
                .technologies_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_str())
                .map(|s| s.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();

            let code_languages = self
                .code_languages_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_str())
                .map(|s| s.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();

            let tools_mentioned = self
                .tools_mentioned_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_str())
                .map(|s| s.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();

            let has_code = self
                .has_code_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let has_error = self
                .has_error_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let interaction_count = self.get_interaction_count(&session_id);

            let sequence_num = self
                .sequence_num_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;

            results.push(SearchResult {
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
            });
        }

        Ok(results)
    }

    pub fn get_all_documents(
        &self,
        project_filter: Option<String>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let searcher = self.reader.searcher();

        let query: Box<dyn tantivy::query::Query> = if let Some(project_filter) = project_filter {
            // Filter by project
            let project_term = Term::from_field_text(self.project_field, &project_filter);
            Box::new(TermQuery::new(
                project_term,
                tantivy::schema::IndexRecordOption::Basic,
            ))
        } else {
            // Match all documents
            Box::new(tantivy::query::AllQuery)
        };

        let top_docs = searcher.search(&*query, &TopDocs::with_limit(limit))?;

        let mut results = Vec::new();
        for (_score, doc_address) in top_docs {
            let retrieved_doc: TantivyDocument = searcher.doc(doc_address)?;

            let content = retrieved_doc
                .get_first(self.content_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let project = retrieved_doc
                .get_first(self.project_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let project_path = if let Some(cwd_field) = self.cwd_field {
                retrieved_doc
                    .get_first(cwd_field)
                    .and_then(|v| v.as_str())
                    .unwrap_or(&project)
                    .to_string()
            } else {
                project.clone()
            };

            let session_id = retrieved_doc
                .get_first(self.session_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let timestamp = retrieved_doc
                .get_first(self.timestamp_field)
                .and_then(|v| v.as_datetime())
                .map(|dt| {
                    // Convert from tantivy::DateTime to chrono::DateTime<Utc>
                    let timestamp_millis = dt.into_timestamp_millis();
                    DateTime::from_timestamp_millis(timestamp_millis).unwrap_or_else(Utc::now)
                })
                .unwrap_or_else(Utc::now);

            let snippet = if content.len() > 100 {
                format!("{}...", &content[..97])
            } else {
                content.clone()
            };

            // Extract metadata fields (with fallbacks for older indexes)
            let technologies = self
                .technologies_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_str())
                .map(|s| s.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();

            let code_languages = self
                .code_languages_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_str())
                .map(|s| s.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();

            let tools_mentioned = self
                .tools_mentioned_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_str())
                .map(|s| s.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();

            let has_code = self
                .has_code_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let has_error = self
                .has_error_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let interaction_count = self.get_interaction_count(&session_id);

            let sequence_num = self
                .sequence_num_field
                .and_then(|field| retrieved_doc.get_first(field))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;

            results.push(SearchResult {
                content,
                project,
                project_path,
                session_id,
                timestamp,
                score: 1.0, // No relevance score for get_all
                snippet,
                technologies,
                code_languages,
                tools_mentioned,
                has_code,
                has_error,
                interaction_count,
                sequence_num,
            });
        }

        Ok(results)
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
                        // Only count messages that would appear in exports (non-empty content)
                        let content = extract_content_from_json(&json);
                        if !content.trim().is_empty() {
                            *session_counts.entry(session_id.to_string()).or_insert(0) += 1;
                        }
                    }
                }

                // Merge session counts from this file into the main map
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
}
