use anyhow::Result;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tracing::{debug, error};

use crate::shared::{
    CacheManager, DisplayOptions, SearchEngine, SearchQuery, SortOrder, auto_index, get_cache_dir,
    get_config,
};

const HAIKU_CONTEXT_WINDOW: usize = 200_000;
const CONTEXT_SAFETY_MARGIN: f64 = 0.75;

/// Parse date string: YYYY-MM-DD (as start of day UTC) or full ISO 8601
fn parse_date(s: &str) -> Result<DateTime<Utc>, String> {
    // Try full ISO 8601 first
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    // Try YYYY-MM-DD
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap()));
    }
    Err(format!("Invalid date '{}': use YYYY-MM-DD or ISO 8601", s))
}

// MCP Protocol Structures
#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct InitializeResponse {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    server_info: ServerInfo,
}

#[derive(Debug, Serialize, Deserialize)]
struct ServerCapabilities {
    #[serde(default)]
    experimental: HashMap<String, Value>,
    #[serde(default)]
    logging: HashMap<String, Value>,
    #[serde(default)]
    prompts: HashMap<String, Value>,
    #[serde(default)]
    resources: HashMap<String, Value>,
    tools: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ServerInfo {
    name: String,
    version: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ListToolsResponse {
    tools: Vec<Tool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Tool {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct CallToolRequest {
    name: String,
    arguments: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CallToolResponse {
    pub content: Vec<ToolResult>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolResult {
    #[serde(rename = "type")]
    pub result_type: String,
    pub text: String,
}

pub struct McpServer {
    search_engine: Option<SearchEngine>,
    cache_manager: Option<CacheManager>,
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

impl McpServer {
    pub fn new() -> Self {
        Self {
            search_engine: None,
            cache_manager: None,
        }
    }

    async fn ensure_initialized(&mut self) -> Result<()> {
        if self.search_engine.is_none() || self.cache_manager.is_none() {
            let cache_dir = get_cache_dir()?;

            // Auto-index if needed
            auto_index(&cache_dir).await?;

            self.search_engine = Some(SearchEngine::new(&cache_dir)?);
            self.cache_manager = Some(CacheManager::new(&cache_dir)?);
        }

        Ok(())
    }

    async fn handle_initialize(&self, params: Option<Value>) -> Result<Value> {
        debug!("Handling initialize request: {:?}", params);

        let response = InitializeResponse {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ServerCapabilities {
                experimental: HashMap::new(),
                logging: HashMap::new(),
                prompts: HashMap::new(),
                resources: HashMap::new(),
                tools: {
                    let mut tools = HashMap::new();
                    tools.insert("listChanged".to_string(), Value::Bool(true));
                    tools
                },
            },
            server_info: ServerInfo {
                name: "claude-search-mcp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn handle_list_tools(&self) -> Result<Value> {
        debug!("Handling list_tools request");

        let tools = vec![
            Tool {
                name: "search_conversations".to_string(),
                description: "Search conversation history (Tantivy/BM25). Exact terms for functions (`_fix_ssh_agent`), natural language for concepts. Workflow: search → get_messages(ids)/truncate_length:0 for full text → summarize_session for AI summary.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query. Field syntax: 'session_id:abc', 'project:name'"
                        },
                        "project": {
                            "type": "string",
                            "description": "Filter by project name",
                            "optional": true
                        },
                        "context": {
                            "type": "integer",
                            "description": "Messages before/after match (grep -C style)",
                            "optional": true,
                            "default": 2
                        },
                        "exclude_projects": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Project names to exclude",
                            "optional": true
                        },
                        "exclude_patterns": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Regex patterns to exclude",
                            "optional": true
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Max results",
                            "optional": true,
                            "default": 10
                        },
                        "sort_by": {
                            "type": "string",
                            "enum": ["relevance", "date_desc", "date_asc"],
                            "optional": true,
                            "default": "relevance"
                        },
                        "after": {
                            "type": "string",
                            "description": "Results after date (YYYY-MM-DD or ISO 8601)",
                            "optional": true
                        },
                        "before": {
                            "type": "string",
                            "description": "Results before date (YYYY-MM-DD or ISO 8601)",
                            "optional": true
                        },
                        "include": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["thinking", "tools", "current_session"] },
                            "description": "Include: thinking, tools, current_session",
                            "optional": true
                        },
                        "truncate_length": {
                            "type": "integer",
                            "description": "Chars shown per message around match. 0 = full content",
                            "optional": true,
                            "default": 300
                        },
                        "debug": {
                            "type": "boolean",
                            "optional": true
                        }
                    },
                    "required": ["query"]
                }),
            },
            Tool {
                name: "respawn_server".to_string(),
                description: "Respawn the MCP server to reload with latest changes".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            Tool {
                name: "reindex".to_string(),
                description: "Update index for stale/new files. Use when search results seem incomplete or index warning shown.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "full": { "type": "boolean", "description": "Force full rebuild (default: incremental)", "optional": true }
                    }
                }),
            },
            Tool {
                name: "get_session_messages".to_string(),
                description: "Paginate session messages. For full summary, start by using summarize_session to setup a task.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "Session ID to retrieve messages for"
                        },
                        "offset": {
                            "type": "integer",
                            "description": "Starting message index (default: 0)",
                            "optional": true,
                            "default": 0
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Messages per page (default: 50)",
                            "optional": true,
                            "default": 50
                        }
                    },
                    "required": ["session_id"]
                }),
            },
            Tool {
                name: "summarize_session".to_string(),
                description: "Get instructions for summarizing a session with AI.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "Session ID to summarize"
                        }
                    },
                    "required": ["session_id"]
                }),
            },
            Tool {
                name: "get_messages".to_string(),
                description: "Get full content of specific messages by UUID. Use after search to read complete message text.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Message UUIDs (from 💬 in search results)"
                        }
                    },
                    "required": ["ids"]
                }),
            },
        ];

        let response = ListToolsResponse { tools };
        Ok(serde_json::to_value(response)?)
    }

    async fn handle_call_tool(&mut self, params: Value) -> Result<Value> {
        let request: CallToolRequest = serde_json::from_value(params)?;
        debug!("Handling tool call: {}", request.name);

        self.ensure_initialized().await?;

        let result = match request.name.as_str() {
            "search_conversations" => self.tool_search_conversations(request.arguments).await?,
            "respawn_server" => self.tool_respawn().await?,
            "reindex" => self.tool_reindex(request.arguments).await?,
            "get_session_messages" => self.tool_get_session_messages(request.arguments).await?,
            "summarize_session" => self.tool_summarize_session(request.arguments).await?,
            "get_messages" => self.tool_get_messages(request.arguments).await?,
            _ => {
                return Ok(serde_json::to_value(CallToolResponse {
                    content: vec![ToolResult {
                        result_type: "text".to_string(),
                        text: format!("Unknown tool: {}", request.name),
                    }],
                    is_error: Some(true),
                })?);
            }
        };

        Ok(result)
    }

    async fn tool_search_conversations(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let query_text = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?
            .to_string();

        let debug_mode = args
            .get("debug")
            .and_then(|v| v.as_str())
            .map(|s| s == "true")
            .unwrap_or(false);

        let project_filter = args
            .get("project")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let context_size = args.get("context").and_then(|v| v.as_u64()).unwrap_or(2) as usize;

        let exclude_projects: Vec<String> = args
            .get("exclude_projects")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let exclude_patterns: Vec<String> = args
            .get("exclude_patterns")
            .map(|v| {
                if let Some(arr) = v.as_array() {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.to_string())
                        .collect()
                } else if let Some(s) = v.as_str() {
                    serde_json::from_str::<Vec<String>>(s).unwrap_or_default()
                } else {
                    Vec::new()
                }
            })
            .unwrap_or_default();

        let config = get_config();
        let claude_dir = config.get_claude_dir()?;
        let pattern = claude_dir.join("projects/**/*.jsonl");
        let all_files: Vec<_> = glob::glob(&pattern.to_string_lossy())?.flatten().collect();

        // Detect current session early to exclude from stale check
        let current_session_file: Option<std::path::PathBuf> =
            std::env::current_dir().ok().and_then(|cwd| {
                let cwd_str = cwd.to_string_lossy().replace(['/', '.'], "-");
                let sess_pattern = claude_dir.join("projects").join(&cwd_str).join("*.jsonl");
                glob::glob(&sess_pattern.to_string_lossy())
                    .ok()?
                    .flatten()
                    .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
            });

        // Exclude current session from stale check (it's always being written to)
        let current_session_name = current_session_file
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());

        let files_for_stale_check: Vec<_> = all_files
            .iter()
            .filter(|f| {
                let name = f.file_name().and_then(|n| n.to_str());
                name != current_session_name
            })
            .cloned()
            .collect();

        let cache = CacheManager::new(&config.get_cache_dir()?)?;
        let (stale_count, new_count) = cache.quick_health_check(&files_for_stale_check);

        let mut all_exclude_patterns = config.search.exclude_patterns.clone();
        all_exclude_patterns.extend(exclude_patterns.clone());

        let exclude_regexes: Vec<Regex> = all_exclude_patterns
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();

        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        let sort_by = match args
            .get("sort_by")
            .and_then(|v| v.as_str())
            .unwrap_or("relevance")
        {
            "date_desc" => SortOrder::DateDesc,
            "date_asc" => SortOrder::DateAsc,
            _ => SortOrder::Relevance,
        };

        let after = if let Some(s) = args.get("after").and_then(|v| v.as_str()) {
            match parse_date(s) {
                Ok(dt) => Some(dt),
                Err(e) => {
                    return Ok(serde_json::to_value(CallToolResponse {
                        content: vec![ToolResult {
                            result_type: "text".to_string(),
                            text: e,
                        }],
                        is_error: Some(true),
                    })?);
                }
            }
        } else {
            None
        };

        let before = if let Some(s) = args.get("before").and_then(|v| v.as_str()) {
            match parse_date(s) {
                Ok(dt) => Some(dt),
                Err(e) => {
                    return Ok(serde_json::to_value(CallToolResponse {
                        content: vec![ToolResult {
                            result_type: "text".to_string(),
                            text: e,
                        }],
                        is_error: Some(true),
                    })?);
                }
            }
        } else {
            None
        };

        // Parse include parameter
        let include: Vec<String> = args
            .get("include")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let truncate_length = args
            .get("truncate_length")
            .and_then(|v| v.as_u64())
            .unwrap_or(300) as usize;

        let display_opts = DisplayOptions {
            include_thinking: include.contains(&"thinking".to_string()),
            include_tools: include.contains(&"tools".to_string()),
            truncate_length,
        };

        let include_current_session = include.contains(&"current_session".to_string());

        // Get current session ID from file detected earlier
        let current_session_id: Option<String> = if !include_current_session {
            current_session_file.as_ref().and_then(|p| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
        } else {
            None
        };

        let query = SearchQuery {
            text: query_text,
            project_filter,
            session_filter: None,
            limit: limit * 3,
            sort_by,
            after,
            before,
        };

        let search_engine = self.search_engine.as_ref().unwrap();
        let results_with_context =
            search_engine.search_with_context(query, context_size, context_size)?;

        // Filter and deduplicate
        let mut session_seen = std::collections::HashSet::new();
        let filtered: Vec<_> = results_with_context
            .into_iter()
            .filter(|r| {
                let proj = &r.matched_message.project;
                let path = &r.matched_message.project_path;
                let session = &r.matched_message.session_id;

                // Exclude current session unless explicitly included
                if let Some(ref current) = current_session_id
                    && session == current
                {
                    return false;
                }

                if exclude_projects.contains(proj) {
                    return false;
                }
                for regex in &exclude_regexes {
                    if regex.is_match(proj) || regex.is_match(path) {
                        return false;
                    }
                }
                // Deduplicate by session
                session_seen.insert(session.clone())
            })
            .take(limit)
            .collect();

        let mut output = String::new();

        if debug_mode {
            output.push_str(&format!(
                "DEBUG: query={:?}, context={}, limit={}, exclude_projects={:?}, patterns={:?}\n\n",
                args.get("query"),
                context_size,
                limit,
                exclude_projects,
                all_exclude_patterns
            ));
        }

        if !exclude_projects.is_empty() || !all_exclude_patterns.is_empty() {
            output.push_str(&format!(
                "Excluding: {} projects, {} patterns\n",
                exclude_projects.len(),
                all_exclude_patterns.len()
            ));
        }

        if filtered.is_empty() {
            if stale_count > 0 || new_count > 0 {
                // No results but index is stale - return error prompting reindex
                return Ok(serde_json::to_value(CallToolResponse {
                    content: vec![ToolResult {
                        result_type: "text".to_string(),
                        text: format!(
                            "No results found. Index is stale ({} modified, {} new files). Call reindex tool and retry search.",
                            stale_count, new_count
                        ),
                    }],
                    is_error: Some(true),
                })?);
            }
            output.push_str("No results found.\n");
        } else {
            for (i, result) in filtered.iter().enumerate() {
                output.push_str(&result.format_compact_with_options(i, &display_opts));
                if i < filtered.len() - 1 {
                    output.push('\n');
                }
            }
            if filtered.len() == limit {
                output.push_str(&format!("\n+more: limit={}\n", limit));
            }
        }

        Ok(serde_json::to_value(CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: output,
            }],
            is_error: None,
        })?)
    }

    async fn tool_get_session_messages(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?;

        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

        let search_engine = self.search_engine.as_ref().unwrap();
        let mut messages = search_engine.get_session_messages(session_id)?;

        if messages.is_empty() {
            return Ok(serde_json::to_value(CallToolResponse {
                content: vec![ToolResult {
                    result_type: "text".to_string(),
                    text: format!("No messages found for session {}", session_id),
                }],
                is_error: Some(true),
            })?);
        }

        // Sort by sequence number and filter displayable messages
        messages.sort_by_key(|m| m.sequence_num);
        let messages: Vec<_> = messages
            .into_iter()
            .filter(|m| m.is_displayable())
            .collect();

        let total = messages.len();
        let project = messages
            .first()
            .map(|m| m.project_path.clone())
            .unwrap_or_default();
        let short_session = &session_id[..8.min(session_id.len())];

        // Apply pagination
        let end = (offset + limit).min(total);
        let page_messages = &messages[offset..end];
        let has_more = end < total;

        // Format header
        let mut output = format!(
            "📁 {} 🗒️ {} ({} msgs) [{}-{}/{}]\n\n",
            project,
            short_session,
            total,
            offset,
            end.saturating_sub(1),
            total
        );

        // Format messages - full content, collapse redundant whitespace
        for (i, msg) in page_messages.iter().enumerate() {
            let idx = offset + i;
            let time = msg.timestamp.format("%H:%M");
            let msg_type = match msg.message_type.as_str() {
                "assistant" => "AI",
                "user" => "User",
                "summary" => "Sum",
                other => other,
            };
            // Collapse whitespace but keep full content
            let content: String = msg.content.split_whitespace().collect::<Vec<_>>().join(" ");
            output.push_str(&format!("[{}] {} {}: {}\n", idx, time, msg_type, content));
        }

        if has_more {
            output.push_str(&format!("\n+more: offset={}\n", end));
        }

        Ok(serde_json::to_value(CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: output,
            }],
            is_error: None,
        })?)
    }

    async fn tool_summarize_session(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?;

        // Get session stats for size estimation
        let search_engine = self.search_engine.as_ref().unwrap();
        let messages = search_engine.get_session_messages(session_id)?;
        let msg_count = messages.len();
        let total_chars: usize = messages.iter().map(|m| m.content.len()).sum();
        let approx_tokens = total_chars / 4; // rough estimate: ~4 chars per token

        let safe_limit = (HAIKU_CONTEXT_WINDOW as f64 * CONTEXT_SAFETY_MARGIN) as usize;
        let size_note = if approx_tokens > safe_limit {
            " (large - may need multiple agents)"
        } else {
            ""
        };

        let output = format!(
            r#"Session {session_id}: {msg_count} messages, ~{approx_tokens} tokens{size_note}

Task(
  subagent_type: "general-purpose",
  model: "haiku",
  prompt: "Summarize session {session_id}:
1. Call get_session_messages(session_id=\"{session_id}\")
2. If output ends with '+more: offset=N', call again with that offset
3. Repeat until no '+more' appears
4. Return a concise summary: topic, key decisions, outcome"
)"#
        );

        Ok(serde_json::to_value(CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: output,
            }],
            is_error: None,
        })?)
    }

    async fn tool_get_messages(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let ids: Vec<String> = args
            .get("ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        if ids.is_empty() {
            return Ok(serde_json::to_value(CallToolResponse {
                content: vec![ToolResult {
                    result_type: "text".to_string(),
                    text: "No message IDs provided".to_string(),
                }],
                is_error: Some(true),
            })?);
        }

        let search_engine = self.search_engine.as_ref().unwrap();
        let messages = search_engine.get_messages_by_uuid(&ids)?;

        if messages.is_empty() {
            return Ok(serde_json::to_value(CallToolResponse {
                content: vec![ToolResult {
                    result_type: "text".to_string(),
                    text: "No messages found for provided IDs".to_string(),
                }],
                is_error: None,
            })?);
        }

        let mut output = String::new();
        for msg in &messages {
            output.push_str(&format!(
                "💬 {} 📅 {} [{}]\n{}\n\n",
                &msg.uuid[..8.min(msg.uuid.len())],
                msg.timestamp.format("%Y-%m-%d %H:%M"),
                msg.message_type,
                msg.content
            ));
        }

        Ok(serde_json::to_value(CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: output,
            }],
            is_error: None,
        })?)
    }

    async fn tool_respawn(&self) -> Result<Value> {
        // Try to find the release binary first, fallback to current_exe
        let current_dir = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("Failed to get current directory: {}", e))?;

        let release_path = current_dir.join("target/release/claude-conversation-search");
        let exe_path = if release_path.exists() {
            release_path
        } else {
            std::env::current_exe()
                .map_err(|e| anyhow::anyhow!("Failed to get current executable path: {}", e))?
        };

        // Prepare response
        let response = CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: "Respawning MCP server...".to_string(),
            }],
            is_error: None,
        };

        // Schedule respawn after a short delay to allow response to be sent
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Replace current process with new instance using exec
            let args: Vec<String> = std::env::args().collect();
            let err = exec::execvp(&exe_path, &args);
            eprintln!("Failed to exec with {}: {}", exe_path.display(), err);
        });

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_reindex(&mut self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let full_rebuild = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
        let config = crate::shared::get_config();
        let index_path = config.get_cache_dir()?;
        let claude_dir = config.get_claude_dir()?;
        let pattern = claude_dir.join("projects/**/*.jsonl");
        let all_files: Vec<_> = glob::glob(&pattern.to_string_lossy())?.flatten().collect();

        let result = if full_rebuild {
            // Full rebuild - clear and recreate
            if index_path.exists() {
                std::fs::remove_dir_all(&index_path)?;
            }
            let mut indexer = crate::shared::SearchIndexer::new(&index_path)?;
            let mut cache = crate::shared::CacheManager::new(&index_path)?;
            cache.update_incremental(&mut indexer, all_files)?;
            self.search_engine = Some(crate::shared::SearchEngine::new(&index_path)?);
            "Full rebuild complete".to_string()
        } else {
            // Incremental update
            let mut indexer = crate::shared::SearchIndexer::open(&index_path)?;
            let mut cache = crate::shared::CacheManager::new(&index_path)?;
            let (stale, new) = cache.quick_health_check(&all_files);
            cache.update_incremental(&mut indexer, all_files)?;
            self.search_engine = Some(crate::shared::SearchEngine::new(&index_path)?);
            format!(
                "Incremental update: {} stale + {} new files reindexed",
                stale, new
            )
        };
        Ok(serde_json::to_value(CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: result,
            }],
            is_error: None,
        })?)
    }

    async fn handle_request(&mut self, request: JsonRpcRequest) -> JsonRpcResponse {
        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params).await,
            "tools/list" => self.handle_list_tools().await,
            "tools/call" => {
                self.handle_call_tool(request.params.unwrap_or_default())
                    .await
            }
            _ => Err(anyhow::anyhow!("Unknown method: {}", request.method)),
        };

        match result {
            Ok(result) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: Some(result),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32603,
                    message: e.to_string(),
                    data: None,
                }),
            },
        }
    }
}

pub async fn run_mcp_server() -> Result<()> {
    // Initialize logging to stderr so it doesn't interfere with JSON-RPC
    // Only show CRITICAL/ERROR level logs to avoid JSON parsing issues
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter("error")
        .init();

    let mut server = McpServer::new();
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = AsyncBufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        debug!("Received line: {}", line);

        match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => {
                let response = server.handle_request(request).await;
                let response_json = serde_json::to_string(&response)?;
                debug!("Sending response: {}", response_json);

                stdout.write_all(response_json.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            Err(e) => {
                error!("Failed to parse JSON-RPC request: {}", e);
                let error_response = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {e}"),
                        data: None,
                    }),
                };
                let response_json = serde_json::to_string(&error_response)?;
                stdout.write_all(response_json.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }
    }

    Ok(())
}
