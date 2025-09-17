use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConversationEntry {
    pub session_id: String,
    pub message_uuid: String,
    pub project_path: String,
    pub timestamp: DateTime<Utc>,
    pub message_type: MessageType,
    pub content: String,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub sequence_num: usize,

    // Enhanced metadata for better search and categorization
    pub technologies: Vec<String>,
    pub has_code: bool,
    pub code_languages: Vec<String>,
    pub has_error: bool,
    pub tools_mentioned: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum MessageType {
    User,
    Assistant,
    ToolUse,
    ToolResult,
    System,
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: String,
    pub project_filter: Option<String>,
    pub session_filter: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub content: String,
    pub project: String,
    pub project_path: String,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub score: f32,
    pub snippet: String,
    pub technologies: Vec<String>,
    pub code_languages: Vec<String>,
    pub tools_mentioned: Vec<String>,
    pub has_code: bool,
    pub has_error: bool,
    pub interaction_count: usize,
    pub sequence_num: usize,
}
