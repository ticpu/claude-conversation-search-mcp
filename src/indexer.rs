use crate::models::ConversationEntry;
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

        let schema = schema_builder.build();

        std::fs::create_dir_all(index_path)?;
        let index = Index::create_in_dir(index_path, schema.clone())?;
        let writer = index.writer(50_000_000)?; // 50MB heap

        Ok(Self {
            writer,
            content_field,
            project_field,
            session_field,
            timestamp_field,
            message_type_field,
            model_field,
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

        let writer = index.writer(50_000_000)?;

        Ok(Self {
            writer,
            content_field,
            project_field,
            session_field,
            timestamp_field,
            message_type_field,
            model_field,
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
            );

            self.writer.add_document(doc)?;
        }

        self.writer.commit()?;
        Ok(())
    }
}
