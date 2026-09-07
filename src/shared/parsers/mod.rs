mod content;
pub mod source;

pub use source::SourceKind;

use super::config::get_config;
use super::metadata;
use super::models::{ConversationEntry, MessageType, RawJsonlMessage};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::io::BufReader;
use std::path::Path;
use strip_ansi_escapes::strip_str;
use tracing::warn;

/// Read text file, skipping UTF-8 BOM if present
fn read_text_file(path: &Path) -> Result<String> {
    use std::fs::File;
    use std::io::Read;

    let mut file = BufReader::new(File::open(path)?);
    let mut first3 = [0u8; 3];

    // Check for UTF-8 BOM (EF BB BF)
    match file.read_exact(&mut first3) {
        Ok(()) if first3 == [0xEF, 0xBB, 0xBF] => {
            // BOM found, skip it
        }
        Ok(()) => {
            // No BOM, include these bytes
            let mut content = String::from_utf8(first3.to_vec())?;
            file.read_to_string(&mut content)?;
            return Ok(content);
        }
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(String::from_utf8(first3[..].to_vec())?);
        }
        Err(e) => return Err(e.into()),
    }

    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

#[derive(Default)]
pub struct JsonlParser {
    full_content: bool,
}

impl JsonlParser {
    pub fn with_full_content() -> Self {
        Self { full_content: true }
    }

    pub fn parse_file(&self, path: &Path) -> Result<Vec<ConversationEntry>> {
        let content = read_text_file(path)?;
        let mut entries = Vec::new();
        let project_name = self.extract_project_name(path);

        let file_agent_id = match SourceKind::classify(path) {
            SourceKind::Agent { agent_id } => Some(agent_id),
            SourceKind::MainSession => None,
        };

        let mut sequence_counter = 0;
        for (line_num, line) in content
            .lines()
            .enumerate()
        {
            if line
                .trim()
                .is_empty()
            {
                continue;
            }

            match serde_json::from_str::<RawJsonlMessage>(line) {
                Ok(raw) => {
                    if let Some(entry) =
                        self.parse_raw_message(raw, &project_name, sequence_counter, &file_agent_id)
                    {
                        entries.push(entry);
                        sequence_counter += 1;
                    }
                }
                Err(e) => {
                    warn!("Invalid JSON at {}:{}: {}", path.display(), line_num + 1, e);
                }
            }
        }

        Ok(entries)
    }

    pub fn parse_raw_message(
        &self,
        raw: RawJsonlMessage,
        fallback_project: &str,
        sequence_num: usize,
        file_agent_id: &Option<String>,
    ) -> Option<ConversationEntry> {
        let msg_type = raw
            .message_type
            .as_deref()?;

        match msg_type {
            "file-history-snapshot" | "queue-operation" => return None,
            "user" | "assistant" | "summary" => {}
            _ => return None,
        }

        let uuid = raw
            .uuid
            .clone()?;
        let session_id = raw
            .session_id
            .clone()?;
        let timestamp_str = raw
            .timestamp
            .as_deref()?;
        let timestamp: DateTime<Utc> = timestamp_str
            .parse()
            .ok()?;

        let message_type = match msg_type {
            "user" => MessageType::User,
            "assistant" => MessageType::Assistant,
            "summary" => MessageType::Summary,
            _ => MessageType::System,
        };

        let extracted = if msg_type == "summary" {
            content::SearchableContent {
                content: raw
                    .summary
                    .unwrap_or_default(),
                has_error: false,
                tools_used: Vec::new(),
            }
        } else {
            content::extract_searchable_content(&raw, self.full_content)
        };
        let mut has_error = extracted.has_error;
        let tools_used = extracted.tools_used;
        let content = strip_str(&extracted.content);

        if content
            .trim()
            .is_empty()
        {
            return None;
        }

        let project_path = raw
            .cwd
            .as_ref()
            .map(|cwd| self.extract_project_name_from_path(cwd))
            .unwrap_or_else(|| fallback_project.to_string());

        let model = raw
            .message
            .as_ref()
            .and_then(|m| {
                m.model
                    .clone()
            });

        let agent_id = raw
            .agent_id
            .or_else(|| file_agent_id.clone());

        let (technologies, code_languages, has_code, tools_mentioned) = if get_config()
            .index
            .enable_tagging
        {
            let meta = metadata::extract_all_metadata(&content);
            has_error |= meta.has_error;
            let mut all_tools = meta.tools_mentioned;
            for tool in tools_used {
                if !all_tools.contains(&tool) {
                    all_tools.push(tool);
                }
            }
            (
                meta.technologies,
                meta.code_languages,
                meta.has_code,
                all_tools,
            )
        } else {
            (vec![], vec![], false, tools_used)
        };

        Some(ConversationEntry {
            uuid,
            parent_uuid: raw.parent_uuid,
            session_id,
            project_path,
            timestamp,
            message_type,
            content,
            model,
            cwd: raw.cwd,
            sequence_num,
            is_sidechain: raw
                .is_sidechain
                .unwrap_or(false),
            agent_id,
            technologies,
            has_code,
            code_languages,
            has_error,
            tools_mentioned,
        })
    }

    fn extract_project_name(&self, path: &Path) -> String {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    }

    fn extract_project_name_from_path(&self, cwd_path: &str) -> String {
        let path = Path::new(cwd_path);
        let components: Vec<&str> = path
            .components()
            .filter_map(|c| {
                c.as_os_str()
                    .to_str()
            })
            .collect();

        for i in (0..components.len()).rev() {
            let component = components[i];
            if matches!(
                component,
                "src" | "lib" | "bin" | "target" | "node_modules" | ".git"
            ) {
                continue;
            }
            if !component.starts_with('.') && component.len() > 1 {
                return component.to_string();
            }
        }

        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_user_message() {
        let json = r#"{"uuid":"abc123","sessionId":"sess1","type":"user","timestamp":"2025-12-28T10:00:00Z","message":{"role":"user","content":"Hello world"}}"#;
        let raw: RawJsonlMessage = serde_json::from_str(json).unwrap();
        let parser = JsonlParser::default();
        let entry = parser
            .parse_raw_message(raw, "test", 0, &None)
            .unwrap();

        assert_eq!(entry.uuid, "abc123");
        assert_eq!(entry.content, "Hello world");
        assert_eq!(entry.message_type, MessageType::User);
    }

    #[test]
    fn test_skip_file_history_snapshot() {
        let json = r#"{"type":"file-history-snapshot","messageId":"xyz"}"#;
        let raw: RawJsonlMessage = serde_json::from_str(json).unwrap();
        let parser = JsonlParser::default();
        let entry = parser.parse_raw_message(raw, "test", 0, &None);

        assert!(entry.is_none());
    }

    #[test]
    fn test_parse_assistant_with_text_block() {
        let json = r#"{"uuid":"abc123","sessionId":"sess1","type":"assistant","timestamp":"2025-12-28T10:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Here is my response"}]}}"#;
        let raw: RawJsonlMessage = serde_json::from_str(json).unwrap();
        let parser = JsonlParser::default();
        let entry = parser
            .parse_raw_message(raw, "test", 0, &None)
            .unwrap();

        assert_eq!(entry.content, "Here is my response");
        assert_eq!(entry.message_type, MessageType::Assistant);
    }

    #[test]
    fn test_parse_thinking_block() {
        let json = r#"{"uuid":"abc123","sessionId":"sess1","type":"assistant","timestamp":"2025-12-28T10:00:00Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Let me think about this..."}]}}"#;
        let raw: RawJsonlMessage = serde_json::from_str(json).unwrap();
        let parser = JsonlParser::default();
        let entry = parser
            .parse_raw_message(raw, "test", 0, &None)
            .unwrap();

        assert!(
            entry
                .content
                .contains("[thinking]")
        );
        assert!(
            entry
                .content
                .contains("Let me think about this")
        );
    }

    #[test]
    fn test_tool_result_truncation() {
        let long_content = "x".repeat(5000);
        let json = format!(
            r#"{{"uuid":"abc123","sessionId":"sess1","type":"assistant","timestamp":"2025-12-28T10:00:00Z","message":{{"role":"assistant","content":[{{"type":"tool_result","content":"{}"}}]}}}}"#,
            long_content
        );
        let raw: RawJsonlMessage = serde_json::from_str(&json).unwrap();
        let parser = JsonlParser::default();
        let entry = parser
            .parse_raw_message(raw, "test", 0, &None)
            .unwrap();

        assert!(
            entry
                .content
                .len()
                < get_config()
                    .limits
                    .tool_result_max_chars
                    + 100
        );
        assert!(
            entry
                .content
                .ends_with('…')
        );
    }
}
