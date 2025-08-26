use super::config::get_config;
use super::models::ConversationEntry;
use anyhow::Result;
use std::path::Path;
use tantivy::schema::{FAST, INDEXED, STORED, SchemaBuilder, TEXT};
use tantivy::{Index, IndexWriter, doc};

pub struct SearchIndexer {
    writer: IndexWriter,
    content_field: tantivy::schema::Field,
    project_field: tantivy::schema::Field,
    session_field: tantivy::schema::Field,
    timestamp_field: tantivy::schema::Field,
    message_type_field: tantivy::schema::Field,
    model_field: tantivy::schema::Field,
    technologies_field: tantivy::schema::Field,
    code_languages_field: tantivy::schema::Field,
    tools_mentioned_field: tantivy::schema::Field,
    has_code_field: tantivy::schema::Field,
    has_error_field: tantivy::schema::Field,
    cwd_field: tantivy::schema::Field,
}

impl SearchIndexer {
    pub fn new(index_path: &Path) -> Result<Self> {
        let mut schema_builder = SchemaBuilder::default();

        let content_field = schema_builder.add_text_field("content", TEXT | STORED);
        let project_field = schema_builder.add_text_field("project", TEXT | STORED | FAST);
        let session_field = schema_builder.add_text_field("session_id", TEXT | STORED | FAST);
        let timestamp_field = schema_builder.add_date_field("timestamp", INDEXED | STORED | FAST);
        let message_type_field =
            schema_builder.add_text_field("message_type", TEXT | STORED | FAST);
        let model_field = schema_builder.add_text_field("model", TEXT | STORED | FAST);

        // Add new metadata fields
        let technologies_field =
            schema_builder.add_text_field("technologies", TEXT | STORED | FAST);
        let code_languages_field =
            schema_builder.add_text_field("code_languages", TEXT | STORED | FAST);
        let tools_mentioned_field =
            schema_builder.add_text_field("tools_mentioned", TEXT | STORED | FAST);
        let has_code_field = schema_builder.add_bool_field("has_code", INDEXED | STORED | FAST);
        let has_error_field = schema_builder.add_bool_field("has_error", INDEXED | STORED | FAST);
        let cwd_field = schema_builder.add_text_field("cwd", TEXT | STORED | FAST);

        let schema = schema_builder.build();

        std::fs::create_dir_all(index_path)?;
        let index = Index::create_in_dir(index_path, schema.clone())?;
        let config = get_config();
        let writer = index.writer(config.get_writer_heap_size())?;

        Ok(Self {
            writer,
            content_field,
            project_field,
            session_field,
            timestamp_field,
            message_type_field,
            model_field,
            technologies_field,
            code_languages_field,
            tools_mentioned_field,
            has_code_field,
            has_error_field,
            cwd_field,
        })
    }

    pub fn open(index_path: &Path) -> Result<Self> {
        let index = Index::open_in_dir(index_path)?;
        let schema = index.schema();

        let content_field = schema.get_field("content")?;
        let project_field = schema.get_field("project")?;
        let session_field = schema.get_field("session_id")?;
        let timestamp_field = schema.get_field("timestamp")?;
        let message_type_field = schema.get_field("message_type")?;
        let model_field = schema.get_field("model")?;

        // Get new metadata fields, with fallbacks for older indexes
        let technologies_field = schema.get_field("technologies").unwrap_or(content_field);
        let code_languages_field = schema.get_field("code_languages").unwrap_or(content_field);
        let tools_mentioned_field = schema.get_field("tools_mentioned").unwrap_or(content_field);
        let has_code_field = schema.get_field("has_code").unwrap_or(content_field);
        let has_error_field = schema.get_field("has_error").unwrap_or(content_field);
        let cwd_field = schema.get_field("cwd").unwrap_or(content_field);

        let config = get_config();
        let writer = index.writer(config.get_writer_heap_size())?;

        Ok(Self {
            writer,
            content_field,
            project_field,
            session_field,
            timestamp_field,
            message_type_field,
            model_field,
            technologies_field,
            code_languages_field,
            tools_mentioned_field,
            has_code_field,
            has_error_field,
            cwd_field,
        })
    }

    pub fn index_conversations(&mut self, entries: Vec<ConversationEntry>) -> Result<()> {
        for entry in entries {
            let doc = doc!(
                self.content_field => entry.content,
                self.project_field => entry.project_path,
                self.session_field => entry.session_id,
                self.timestamp_field => tantivy::DateTime::from_timestamp_secs(entry.timestamp.timestamp()),
                self.message_type_field => format!("{:?}", entry.message_type),
                self.model_field => entry.model.unwrap_or_else(|| "unknown".to_string()),
                self.technologies_field => entry.technologies.join(" "),
                self.code_languages_field => entry.code_languages.join(" "),
                self.tools_mentioned_field => entry.tools_mentioned.join(" "),
                self.has_code_field => entry.has_code,
                self.has_error_field => entry.has_error,
                self.cwd_field => entry.cwd.unwrap_or_else(|| "unknown".to_string()),
            );

            self.writer.add_document(doc)?;
        }

        self.writer.commit()?;
        Ok(())
    }
}
