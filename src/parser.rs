use crate::models::{ConversationEntry, MessageType};
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::Path;

pub struct JsonlParser;

impl JsonlParser {
    pub fn new() -> Self {
        Self
    }

    pub fn parse_file(&self, path: &Path) -> Result<Vec<ConversationEntry>> {
        let content = std::fs::read_to_string(path)?;
        let mut entries = Vec::new();

        let project_name = self.extract_project_name(path);

        for (line_num, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<Value>(line) {
                Ok(json) => {
                    if let Ok(entry) = self.parse_entry(json, &project_name) {
                        entries.push(entry);
                    }
                }
                Err(e) => {
                    tracing::warn!("Invalid JSON at {}:{}: {}", path.display(), line_num + 1, e);
                }
            }
        }

        Ok(entries)
    }

    fn parse_entry(&self, json: Value, project_name: &str) -> Result<ConversationEntry> {
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

        let content = self.extract_content(&json)?;

        let model = json
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let cwd = json
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(ConversationEntry {
            session_id,
            message_uuid,
            project_path: project_name.to_string(),
            timestamp,
            message_type,
            content,
            model,
            cwd,
        })
    }

    fn extract_content(&self, json: &Value) -> Result<String> {
        if let Some(message) = json.get("message") {
            if let Some(content) = message.get("content") {
                if let Some(text) = content.as_str() {
                    return Ok(text.to_string());
                }
                if content.is_array() {
                    let mut text_parts = Vec::new();
                    for part in content.as_array().unwrap() {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            text_parts.push(text);
                        }
                    }
                    return Ok(text_parts.join(" "));
                }
            }
        }

        if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
            return Ok(content.to_string());
        }

        Ok(String::new())
    }

    fn extract_project_name(&self, path: &Path) -> String {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    }
}
