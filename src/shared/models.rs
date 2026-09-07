use anyhow::anyhow;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Raw JSONL message structure for parsing Claude Code logs
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawJsonlMessage {
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    pub session_id: Option<String>,
    #[serde(rename = "type")]
    pub message_type: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub message: Option<RawMessage>,
    pub is_sidechain: Option<bool>,
    pub agent_id: Option<String>,
    // Summary type fields
    pub summary: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RawMessage {
    pub content: Option<serde_json::Value>,
    pub model: Option<String>,
}

/// Content block types in assistant messages
#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text(String),
    Thinking(String),
    ToolUse {
        name: String,
        input_preview: String,
    },
    ToolResult {
        content_preview: String,
        is_error: bool,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConversationEntry {
    pub uuid: String,
    pub parent_uuid: Option<String>,
    pub session_id: String,
    pub project_path: String,
    pub timestamp: DateTime<Utc>,
    pub message_type: MessageType,
    pub content: String,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub sequence_num: usize,
    pub is_sidechain: bool,
    pub agent_id: Option<String>,

    // Enhanced metadata for better search and categorization
    pub technologies: Vec<String>,
    pub has_code: bool,
    pub code_languages: Vec<String>,
    pub has_error: bool,
    pub tools_mentioned: Vec<String>,
}

impl ConversationEntry {
    pub fn is_displayable(&self) -> bool {
        displayable(&self.message_type, &self.content)
    }
}

#[cfg(test)]
pub struct EntryBuilder(ConversationEntry);

#[cfg(test)]
impl EntryBuilder {
    pub fn new(uuid: &str, session_id: &str) -> Self {
        Self(ConversationEntry {
            uuid: uuid.to_string(),
            parent_uuid: None,
            session_id: session_id.to_string(),
            project_path: "/test/project".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::User,
            content: String::new(),
            model: None,
            cwd: None,
            sequence_num: 0,
            is_sidechain: false,
            agent_id: None,
            technologies: vec![],
            has_code: false,
            code_languages: vec![],
            has_error: false,
            tools_mentioned: vec![],
        })
    }

    pub fn message_type(mut self, message_type: MessageType) -> Self {
        self.0
            .message_type = message_type;
        self
    }

    pub fn content(mut self, content: &str) -> Self {
        self.0
            .content = content.to_string();
        self
    }

    pub fn sequence_num(mut self, sequence_num: usize) -> Self {
        self.0
            .sequence_num = sequence_num;
        self
    }

    pub fn timestamp(mut self, timestamp: DateTime<Utc>) -> Self {
        self.0
            .timestamp = timestamp;
        self
    }

    pub fn project(mut self, project_path: &str, cwd: &str) -> Self {
        self.0
            .project_path = project_path.to_string();
        self.0
            .cwd = Some(cwd.to_string());
        self
    }

    pub fn build(self) -> ConversationEntry {
        self.0
    }
}

/// Filters noise: non-conversational message types and internal warmup messages.
pub fn displayable(kind: &MessageType, content: &str) -> bool {
    matches!(
        kind,
        MessageType::User | MessageType::Assistant | MessageType::Summary
    ) && content.trim() != "Warmup"
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum MessageType {
    User,
    Assistant,
    Summary,
    System,
}

impl MessageType {
    /// Short display name for output (User, AI, Sum, Sys)
    pub fn short_name(&self) -> &'static str {
        match self {
            MessageType::User => "User",
            MessageType::Assistant => "AI",
            MessageType::Summary => "Sum",
            MessageType::System => "Sys",
        }
    }

    fn as_index_str(&self) -> &'static str {
        match self {
            MessageType::User => "User",
            MessageType::Assistant => "Assistant",
            MessageType::Summary => "Summary",
            MessageType::System => "System",
        }
    }
}

/// Exact inverse of `FromStr`: this is what the indexer stores in `message_type`.
impl fmt::Display for MessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_index_str())
    }
}

impl FromStr for MessageType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "User" => Ok(MessageType::User),
            "Assistant" => Ok(MessageType::Assistant),
            "Summary" => Ok(MessageType::Summary),
            "System" => Ok(MessageType::System),
            other => Err(anyhow!("unknown message type {other:?}")),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum SortOrder {
    #[default]
    Relevance,
    DateDesc,
    DateAsc,
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub text: String,
    pub project_filter: Option<String>,
    pub session_filter: Option<String>,
    pub limit: usize,
    pub sort_by: SortOrder,
    pub after: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub uuid: String,
    pub content: String,
    pub project: String,
    pub project_path: String,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub technologies: Vec<String>,
    pub code_languages: Vec<String>,
    pub tools_mentioned: Vec<String>,
    pub has_code: bool,
    pub has_error: bool,
    pub interaction_count: usize,
    pub sequence_num: usize,
    pub message_type: MessageType,
}

impl SearchResult {
    pub fn is_displayable(&self) -> bool {
        displayable(&self.message_type, &self.content)
    }
}
