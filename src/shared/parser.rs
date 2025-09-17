use super::metadata::MetadataExtractor;
use super::models::{ConversationEntry, MessageType};
use super::utils::extract_content_from_json;
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::Path;

pub struct JsonlParser;

impl Default for JsonlParser {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonlParser {
    pub fn new() -> Self {
        Self
    }

    pub fn parse_file(&self, path: &Path) -> Result<Vec<ConversationEntry>> {
        let content = std::fs::read_to_string(path)?;
        let mut entries = Vec::new();

        let project_name = self.extract_project_name(path);

        let mut sequence_counter = 0;
        for (line_num, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<Value>(line) {
                Ok(json) => {
                    if let Ok(entry) = self.parse_entry(json, &project_name, sequence_counter) {
                        entries.push(entry);
                        sequence_counter += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!("Invalid JSON at {}:{}: {}", path.display(), line_num + 1, e);
                }
            }
        }

        Ok(entries)
    }

    fn parse_entry(
        &self,
        json: Value,
        fallback_project_name: &str,
        sequence_num: usize,
    ) -> Result<ConversationEntry> {
        let session_id = json
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing sessionId"))?
            .to_string();

        let message_uuid = json
            .get("uuid")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let timestamp_str = json
            .get("timestamp")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing timestamp"))?;
        let timestamp: DateTime<Utc> = timestamp_str.parse()?;

        let message_type = json
            .get("type")
            .and_then(|v| v.as_str())
            .map(|s| match s {
                "user" => MessageType::User,
                "assistant" => MessageType::Assistant,
                "tool_use" => MessageType::ToolUse,
                "tool_result" => MessageType::ToolResult,
                _ => MessageType::System,
            })
            .unwrap_or(MessageType::System);

        let content = extract_content_from_json(&json);

        let model = json
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let cwd = json
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Use cwd for project path if available, otherwise fallback to directory name
        let project_path = if let Some(ref cwd_path) = cwd {
            self.extract_project_name_from_path(cwd_path)
        } else {
            fallback_project_name.to_string()
        };

        // Extract metadata from content
        let (technologies, tools_mentioned, code_languages, has_code, has_error) =
            MetadataExtractor::extract_all_metadata(&content);

        Ok(ConversationEntry {
            session_id,
            message_uuid,
            project_path,
            timestamp,
            message_type,
            content,
            model,
            cwd,
            sequence_num,
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
        // Extract a nice project name from the cwd path
        let path = Path::new(cwd_path);

        // Try to find a meaningful project name by looking for common patterns
        let components: Vec<&str> = path
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();

        // Look for patterns like /home/user/project or /mnt/drive/path/to/project
        // Take the last meaningful directory name
        for i in (0..components.len()).rev() {
            let component = components[i];

            // Skip common non-project directories
            if matches!(
                component,
                "src" | "lib" | "bin" | "target" | "node_modules" | ".git"
            ) {
                continue;
            }

            // If we find a component that looks like a project name, use it
            if !component.starts_with('.') && component.len() > 1 {
                return component.to_string();
            }
        }

        // Fallback to the last component
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    }
}
