use super::SearchEngine;
use crate::shared::models::{MessageType, SearchResult};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use std::str::FromStr;
use tantivy::TantivyDocument;
use tantivy::schema::{Field, Value};

fn doc_str_opt(doc: &TantivyDocument, field: Field) -> Option<&str> {
    doc.get_first(field)
        .and_then(|v| v.as_str())
}

fn doc_str(doc: &TantivyDocument, field: Field) -> String {
    doc_str_opt(doc, field)
        .unwrap_or("")
        .to_string()
}

fn doc_bool(doc: &TantivyDocument, field: Field) -> bool {
    doc.get_first(field)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Space-separated list field, as stored by the indexer.
fn doc_str_list(doc: &TantivyDocument, field: Field) -> Vec<String> {
    doc_str_opt(doc, field)
        .map(|s| {
            s.split_whitespace()
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

impl SearchEngine {
    pub(super) fn doc_to_result(&self, doc: &TantivyDocument) -> Result<SearchResult> {
        let uuid = doc_str(
            doc,
            self.fields
                .uuid_field,
        );
        let content = doc_str(
            doc,
            self.fields
                .content_field,
        );
        let project = doc_str(
            doc,
            self.fields
                .project_field,
        );
        let project_path = doc_str_opt(
            doc,
            self.fields
                .cwd_field,
        )
        .unwrap_or(&project)
        .to_string();
        let session_id = doc_str(
            doc,
            self.fields
                .session_field,
        );

        let timestamp = doc
            .get_first(
                self.fields
                    .timestamp_field,
            )
            .and_then(|v| v.as_datetime())
            .map(|dt| {
                DateTime::from_timestamp_millis(dt.into_timestamp_millis()).unwrap_or_else(Utc::now)
            })
            .unwrap_or_else(Utc::now);

        let stored_type = doc_str_opt(
            doc,
            self.fields
                .message_type_field,
        )
        .ok_or_else(|| anyhow!("document {uuid} has no message_type field"))?;
        let message_type = MessageType::from_str(stored_type)
            .with_context(|| format!("document {uuid} carries an unindexable message type"))?;

        let technologies = doc_str_list(
            doc,
            self.fields
                .technologies_field,
        );
        let code_languages = doc_str_list(
            doc,
            self.fields
                .code_languages_field,
        );
        let tools_mentioned = doc_str_list(
            doc,
            self.fields
                .tools_mentioned_field,
        );
        let has_code = doc_bool(
            doc,
            self.fields
                .has_code_field,
        );
        let has_error = doc_bool(
            doc,
            self.fields
                .has_error_field,
        );

        let sequence_num = doc
            .get_first(
                self.fields
                    .sequence_num_field,
            )
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

    pub(super) fn get_interaction_count(&self, session_id: &str) -> usize {
        self.interaction_counts
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }
}
