use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tokio::fs as tokio_fs;
use tracing::{debug, warn};
use uuid::Uuid;

use super::server::{CallToolResponse, ToolResult};
use crate::shared::{SearchEngine, SearchQuery};

#[derive(Debug, Serialize, Deserialize)]
struct WebServerConfig {
    path: String,
    url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct LimitsConfig {
    per_file_chars: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            per_file_chars: 150_000, // 150k chars per file - split into multiple files when exceeded
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct AnalysisConfig {
    web_server: WebServerConfig,
    #[serde(default)]
    limits: LimitsConfig,
}

impl AnalysisConfig {
    fn load() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow!("Could not determine config directory"))?
            .join("claude-conversation-search-mcp");

        let config_path = config_dir.join("config.yaml");

        if !config_path.exists() {
            return Err(anyhow!(
                "Analysis config not found at {}. Please create this file with web_server configuration.",
                config_path.display()
            ));
        }

        let config_content = fs::read_to_string(&config_path)?;
        let config: AnalysisConfig = serde_yaml::from_str(&config_content)?;

        Ok(config)
    }
}

fn truncate_conversation(content: &str, limit: usize) -> String {
    if content.len() <= limit {
        return content.to_string();
    }

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    if total_lines <= 10 {
        // Very short conversation, just truncate
        let mut result = content.chars().take(limit - 100).collect::<String>();
        result.push_str("\n\n*[Truncated: conversation too long]*\n");
        return result;
    }

    // Smart truncation: keep first 20%, last 20%, and middle 60% of most important content
    let keep_start = (total_lines as f32 * 0.2) as usize;
    let keep_end = (total_lines as f32 * 0.2) as usize;

    let mut result = String::new();

    // Add first 20%
    for line in lines.iter().take(keep_start) {
        result.push_str(line);
        result.push('\n');
    }

    result.push_str("\n*[Truncated: middle content removed to fit size limits]*\n\n");

    // Add last 20%
    for line in lines.iter().skip(total_lines - keep_end) {
        result.push_str(line);
        result.push('\n');
    }

    // If still too long, hard truncate
    if result.len() > limit {
        result.truncate(limit - 100);
        result.push_str("\n\n*[Further truncated due to size limits]*");
    }

    result
}

pub async fn handle_analyze_conversation_content(
    search_engine: Option<&SearchEngine>,
    args: Option<Value>,
) -> Result<Value> {
    let args = args.unwrap_or_default();

    let session_ids: Vec<String> = args
        .get("session_ids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("session_ids parameter is required and must be an array"))?
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();

    if session_ids.is_empty() {
        return Ok(serde_json::to_value(CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: "No session IDs provided for analysis".to_string(),
            }],
            is_error: Some(true),
        })?);
    }

    // Load configuration
    let config = match AnalysisConfig::load() {
        Ok(config) => config,
        Err(e) => {
            return Ok(serde_json::to_value(CallToolResponse {
                content: vec![ToolResult {
                    result_type: "text".to_string(),
                    text: format!("Configuration error: {}", e),
                }],
                is_error: Some(true),
            })?);
        }
    };

    // Get conversation content
    let search_engine = search_engine.ok_or_else(|| anyhow!("Search engine not initialized"))?;

    // Collect all conversations first
    let mut conversations = Vec::new();

    for (i, session_id) in session_ids.iter().enumerate() {
        let query = SearchQuery {
            text: format!("session_id:{}", session_id),
            project_filter: None,
            session_filter: None,
            limit: 100,
        };

        match search_engine.search(query) {
            Ok(mut results) => {
                if results.is_empty() {
                    warn!("No messages found for session: {}", session_id);
                    continue;
                }

                // Sort by timestamp to get chronological order
                results.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

                let mut conversation_content = String::new();
                conversation_content.push_str(&format!(
                    "## Conversation {} - Session ID: {}\n\n",
                    i + 1,
                    session_id
                ));
                conversation_content.push_str(&format!("**Project**: {}\n", results[0].project));
                conversation_content.push_str(&format!(
                    "**Time range**: {} to {}\n",
                    results[0].timestamp.format("%Y-%m-%d %H:%M"),
                    results.last().unwrap().timestamp.format("%Y-%m-%d %H:%M")
                ));
                conversation_content.push_str(&format!("**Messages**: {}\n\n", results.len()));

                // Add all message content, skipping empty messages entirely
                let mut section_counter = 1;

                for result in results.iter() {
                    if result.content.trim().is_empty() {
                        // Just skip empty messages completely - they're tool-only interactions
                        continue;
                    }

                    conversation_content.push_str(&format!(
                        "§{} {}\n",
                        section_counter,
                        result.timestamp.format("%H:%M:%S")
                    ));
                    conversation_content.push_str(&result.content);
                    conversation_content.push('\n');
                    section_counter += 1;
                }

                // Truncate if conversation is too large
                if conversation_content.len() > config.limits.per_file_chars {
                    conversation_content =
                        truncate_conversation(&conversation_content, config.limits.per_file_chars);
                }

                conversations.push(conversation_content);
            }
            Err(e) => {
                warn!("Failed to get context for session {}: {}", session_id, e);
                conversations.push(format!("## Conversation {} - Session ID: {}\n\n*Error retrieving conversation: {}*\n\n", i + 1, session_id, e));
            }
        }
    }

    if conversations.is_empty() {
        return Ok(serde_json::to_value(CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: "No valid conversations found for the provided session IDs".to_string(),
            }],
            is_error: Some(true),
        })?);
    }

    // Split into multiple files if needed
    let mut files = Vec::new();
    let mut current_file_content = String::new();
    let mut current_conversations = Vec::new();

    // Add header to first file
    current_file_content.push_str("# Conversation Content for Analysis\n\n");
    current_file_content.push_str(&format!(
        "**Total Conversations**: {}\n\n",
        conversations.len()
    ));

    for (idx, conversation) in conversations.iter().enumerate() {
        let test_length = current_file_content.len() + conversation.len() + 100; // 100 chars buffer

        if test_length > config.limits.per_file_chars && !current_conversations.is_empty() {
            // Current file would be too large, start new file
            files.push((current_file_content.clone(), current_conversations.clone()));

            // Start new file
            current_file_content.clear();
            current_file_content.push_str(&format!(
                "# Conversation Content for Analysis (Part {})\n\n",
                files.len() + 1
            ));
            current_file_content.push_str(&format!(
                "**Total Conversations**: {} (showing conversations {}-...)\n\n",
                conversations.len(),
                idx + 1
            ));
            current_conversations.clear();
        }

        current_file_content.push_str(conversation);
        current_file_content.push_str("---\n\n");
        current_conversations.push(idx + 1);
    }

    // Add final file
    if !current_conversations.is_empty() {
        files.push((current_file_content, current_conversations));
    }

    // Write all files and collect URLs
    let base_uuid = Uuid::new_v4();
    let mut urls_and_ranges = Vec::new();

    for (file_idx, (content, conv_numbers)) in files.iter().enumerate() {
        let filename = if files.len() == 1 {
            format!("claude-analysis-{}.txt", base_uuid)
        } else {
            format!("claude-analysis-{}-part{}.txt", base_uuid, file_idx + 1)
        };

        let file_path = PathBuf::from(&config.web_server.path).join(&filename);
        let web_url = format!(
            "{}/{}",
            config.web_server.url.trim_end_matches('/'),
            filename
        );

        // Write content to file
        if let Err(e) = tokio_fs::write(&file_path, content).await {
            return Ok(serde_json::to_value(CallToolResponse {
                content: vec![ToolResult {
                    result_type: "text".to_string(),
                    text: format!("Failed to write analysis file {}: {}", filename, e),
                }],
                is_error: Some(true),
            })?);
        }

        debug!("Created analysis file at: {}", file_path.display());

        let range_desc = if conv_numbers.len() == 1 {
            format!("conversation {}", conv_numbers[0])
        } else {
            format!(
                "conversations {}-{}",
                conv_numbers[0],
                conv_numbers.last().unwrap()
            )
        };

        urls_and_ranges.push((web_url, range_desc));
    }

    // Generate instructions
    let instructions = if files.len() == 1 {
        format!(
            "I've prepared the {} selected conversation(s) for analysis. Please use WebFetch to analyze the content:\n\n\
                 URL: {}\n\n\
                 The file contains all conversation content formatted for analysis.",
            session_ids.len(),
            urls_and_ranges[0].0
        )
    } else {
        let mut inst = format!(
            "The {} conversations are split across {} files due to size limits. Please use WebFetch to analyze each:\n\n",
            session_ids.len(),
            files.len()
        );

        for (idx, (url, range)) in urls_and_ranges.iter().enumerate() {
            inst.push_str(&format!("{}. {} - {}\n", idx + 1, range, url));
        }

        inst.push_str("\nEach file contains the analysis prompt at the top. After analyzing all parts, provide a combined summary.");
        inst
    };

    Ok(serde_json::to_value(CallToolResponse {
        content: vec![ToolResult {
            result_type: "text".to_string(),
            text: instructions,
        }],
        is_error: None,
    })?)
}
