use super::config::get_config;
use super::models::ConversationEntry;
use anyhow::Result;
use std::path::Path;
use tantivy::schema::{FAST, Field, INDEXED, STORED, Schema, SchemaBuilder, TEXT};
use tantivy::{Index, IndexWriter, doc};

pub struct IndexFields {
    pub content_field: Field,
    pub project_field: Field,
    pub session_field: Field,
    pub timestamp_field: Field,
    pub message_type_field: Field,
    pub model_field: Field,
    pub technologies_field: Field,
    pub code_languages_field: Field,
    pub tools_mentioned_field: Field,
    pub has_code_field: Field,
    pub has_error_field: Field,
    pub cwd_field: Field,
    pub sequence_num_field: Field,
}

pub struct SearchIndexer {
    writer: IndexWriter,
    fields: IndexFields,
}

impl SearchIndexer {
    /// Create the canonical schema - single source of truth
    pub fn build_schema() -> (Schema, IndexFields) {
        let mut schema_builder = SchemaBuilder::default();

        let content_field = schema_builder.add_text_field("content", TEXT | STORED);
        let project_field = schema_builder.add_text_field("project", TEXT | STORED | FAST);
        let session_field = schema_builder.add_text_field("session_id", TEXT | STORED | FAST);
        let timestamp_field = schema_builder.add_date_field("timestamp", INDEXED | STORED | FAST);
        let message_type_field =
            schema_builder.add_text_field("message_type", TEXT | STORED | FAST);
        let model_field = schema_builder.add_text_field("model", TEXT | STORED | FAST);
        let technologies_field =
            schema_builder.add_text_field("technologies", TEXT | STORED | FAST);
        let code_languages_field =
            schema_builder.add_text_field("code_languages", TEXT | STORED | FAST);
        let tools_mentioned_field =
            schema_builder.add_text_field("tools_mentioned", TEXT | STORED | FAST);
        let has_code_field = schema_builder.add_bool_field("has_code", INDEXED | STORED | FAST);
        let has_error_field = schema_builder.add_bool_field("has_error", INDEXED | STORED | FAST);
        let cwd_field = schema_builder.add_text_field("cwd", TEXT | STORED | FAST);
        let sequence_num_field =
            schema_builder.add_u64_field("sequence_num", INDEXED | STORED | FAST);

        let schema = schema_builder.build();
        let fields = IndexFields {
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
            sequence_num_field,
        };

        (schema, fields)
    }

    /// Validate that an existing index matches our expected schema
    pub fn validate_schema(index_path: &Path) -> Result<bool> {
        let index = Index::open_in_dir(index_path)?;
        let actual_schema = index.schema();
        let (_, expected_fields) = Self::build_schema();

        // Check required fields exist and have correct types
        let required_checks = [
            ("content", &expected_fields.content_field),
            ("project", &expected_fields.project_field),
            ("session_id", &expected_fields.session_field),
            ("timestamp", &expected_fields.timestamp_field),
            ("message_type", &expected_fields.message_type_field),
            ("model", &expected_fields.model_field),
        ];

        for (field_name, _) in required_checks {
            if actual_schema.get_field(field_name).is_err() {
                return Ok(false);
            }
        }

        // Simple validation: if we can get the required fields, schema is compatible
        Ok(true)
    }

    pub fn new(index_path: &Path) -> Result<Self> {
        let (schema, fields) = Self::build_schema();

        std::fs::create_dir_all(index_path)?;
        let index = Index::create_in_dir(index_path, schema)?;
        let config = get_config();
        let writer = index.writer(config.get_writer_heap_size())?;

        Ok(Self { writer, fields })
    }

    pub fn open(index_path: &Path) -> Result<Self> {
        let index = Index::open_in_dir(index_path)?;
        let schema = index.schema();

        // Get fields from the existing schema
        let fields = IndexFields {
            content_field: schema.get_field("content")?,
            project_field: schema.get_field("project")?,
            session_field: schema.get_field("session_id")?,
            timestamp_field: schema.get_field("timestamp")?,
            message_type_field: schema.get_field("message_type")?,
            model_field: schema.get_field("model")?,
            // Handle optional fields for backward compatibility
            technologies_field: schema
                .get_field("technologies")
                .unwrap_or_else(|_| schema.get_field("content").unwrap()),
            code_languages_field: schema
                .get_field("code_languages")
                .unwrap_or_else(|_| schema.get_field("content").unwrap()),
            tools_mentioned_field: schema
                .get_field("tools_mentioned")
                .unwrap_or_else(|_| schema.get_field("content").unwrap()),
            has_code_field: schema
                .get_field("has_code")
                .unwrap_or_else(|_| schema.get_field("content").unwrap()),
            has_error_field: schema
                .get_field("has_error")
                .unwrap_or_else(|_| schema.get_field("content").unwrap()),
            cwd_field: schema
                .get_field("cwd")
                .unwrap_or_else(|_| schema.get_field("content").unwrap()),
            sequence_num_field: schema
                .get_field("sequence_num")
                .unwrap_or_else(|_| schema.get_field("timestamp").unwrap()),
        };

        let config = get_config();
        let writer = index.writer(config.get_writer_heap_size())?;

        Ok(Self { writer, fields })
    }

    pub fn index_conversations(&mut self, entries: Vec<ConversationEntry>) -> Result<()> {
        for entry in entries {
            let doc = doc!(
                self.fields.content_field => entry.content,
                self.fields.project_field => entry.project_path,
                self.fields.session_field => entry.session_id,
                self.fields.timestamp_field => tantivy::DateTime::from_timestamp_millis(entry.timestamp.timestamp_millis()),
                self.fields.message_type_field => format!("{:?}", entry.message_type),
                self.fields.model_field => entry.model.unwrap_or_else(|| "unknown".to_string()),
                self.fields.technologies_field => entry.technologies.join(" "),
                self.fields.code_languages_field => entry.code_languages.join(" "),
                self.fields.tools_mentioned_field => entry.tools_mentioned.join(" "),
                self.fields.has_code_field => entry.has_code,
                self.fields.has_error_field => entry.has_error,
                self.fields.cwd_field => entry.cwd.unwrap_or_else(|| "unknown".to_string()),
                self.fields.sequence_num_field => entry.sequence_num as u64,
            );

            self.writer.add_document(doc)?;
        }

        self.writer.commit()?;
        Ok(())
    }
}
