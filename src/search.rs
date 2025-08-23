use crate::models::{SearchQuery, SearchResult};
use anyhow::Result;
use chrono::{DateTime, Utc};
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

        Ok(Self {
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
        })
    }

    pub fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        let searcher = self.reader.searcher();

        let query_parser = QueryParser::for_index(&self.index, vec![self.content_field]);
        let text_query = query_parser.parse_query(&query.text)?;

        let final_query = if let Some(project_filter) = query.project_filter {
            let project_term = Term::from_field_text(self.project_field, &project_filter);
            let project_query =
                TermQuery::new(project_term, tantivy::schema::IndexRecordOption::Basic);

            let mut subqueries = vec![(
                Occur::Must,
                Box::new(text_query) as Box<dyn tantivy::query::Query>,
            )];
            subqueries.push((Occur::Must, Box::new(project_query)));

            Box::new(BooleanQuery::new(subqueries)) as Box<dyn tantivy::query::Query>
        } else {
            text_query
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

            let session_id = retrieved_doc
                .get_first(self.session_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let timestamp = retrieved_doc
                .get_first(self.timestamp_field)
                .and_then(|v| v.as_datetime())
                .map(|dt| {
                    DateTime::<Utc>::from_timestamp(dt.into_timestamp_secs(), 0)
                        .unwrap_or_else(Utc::now)
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

            let word_count = content.split_whitespace().count();

            results.push(SearchResult {
                content,
                project,
                session_id,
                timestamp,
                score,
                snippet,
                technologies,
                code_languages,
                tools_mentioned,
                has_code,
                has_error,
                word_count,
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
}
