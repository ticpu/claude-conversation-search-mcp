use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tracing::{debug, error};

use super::conversation_aggregator;
use crate::shared::{
    CacheManager, SearchEngine, SearchQuery, SearchResult, auto_index, get_cache_dir, get_config,
};

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
struct InitializeRequest {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    client_info: ClientInfo,
}

#[derive(Debug, Serialize, Deserialize)]
struct ClientCapabilities {
    #[serde(default)]
    experimental: HashMap<String, Value>,
    #[serde(default)]
    sampling: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ClientInfo {
    name: String,
    version: String,
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
                description: "Search through Claude Code conversation history with optional project filtering".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query text. Can use field syntax like 'session_id:abc' or 'project:name'"
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project name to filter results",
                            "optional": true
                        },
                        "exclude_projects": {
                            "type": "array",
                            "items": {
                                "type": "string"
                            },
                            "description": "Optional exact project names to exclude from results",
                            "optional": true
                        },
                        "exclude_patterns": {
                            "type": "array",
                            "items": {
                                "type": "string"
                            },
                            "description": "Optional regex patterns to exclude projects by path/name. Examples: [\".*test.*\"] excludes anything with 'test', [\"^backup-.*\"] excludes projects starting with 'backup-', [\".*temp.*\", \".*old.*\"] excludes multiple patterns. Patterns match against both project name and full path.",
                            "optional": true
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results to return (default: 10)",
                            "optional": true,
                            "default": 10
                        },
                        "debug": {
                            "type": "string",
                            "description": "Set to 'true' to enable debug output",
                            "optional": true
                        }
                    },
                    "required": ["query"]
                }),
            },
            Tool {
                name: "get_conversation_context".to_string(),
                description: "Get detailed context for a specific session including all messages and metadata".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "Session ID to retrieve full context for"
                        },
                        "include_content": {
                            "type": "boolean",
                            "description": "Whether to include full message content (default: false - returns snippets)",
                            "optional": true,
                            "default": false
                        }
                    },
                    "required": ["session_id"]
                }),
            },
            Tool {
                name: "analyze_conversation_topics".to_string(),
                description: "Analyze technology topics and patterns from search results. Use query to focus analysis.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query to find conversations for analysis (e.g. 'rust error', 'database api'). Defaults to broad search.",
                            "optional": true
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project name to filter analysis",
                            "optional": true
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of projects to show (default: 20)",
                            "optional": true,
                            "default": 20
                        },
                        "search_limit": {
                            "type": "integer",
                            "description": "Number of search results to analyze (default: 500)",
                            "optional": true,
                            "default": 500
                        }
                    }
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
                name: "analyze_conversation_content".to_string(),
                description: "AI-powered analysis of selected conversations using WebFetch".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_ids": {
                            "type": "array",
                            "items": {
                                "type": "string"
                            },
                            "description": "Array of conversation session IDs to analyze"
                        },
                    },
                    "required": ["session_ids"]
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
            "get_conversation_context" => {
                super::context_viewer::handle_get_conversation_context(
                    self.search_engine.as_ref(),
                    request.arguments,
                )
                .await?
            }
            "analyze_conversation_topics" => {
                super::topic_analyzer::handle_analyze_topics(
                    self.search_engine.as_ref(),
                    request.arguments,
                )
                .await?
            }
            "respawn_server" => self.tool_respawn().await?,
            "analyze_conversation_content" => {
                self.tool_analyze_conversation_content(request.arguments)
                    .await?
            }
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

        // Check if debug mode is enabled
        let debug_mode = args
            .get("debug")
            .and_then(|v| v.as_str())
            .map(|s| s == "true")
            .unwrap_or(false);

        let project_filter = args
            .get("project")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
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
                // Handle both array and string representations
                if let Some(arr) = v.as_array() {
                    // Direct array
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.to_string())
                        .collect()
                } else if let Some(s) = v.as_str() {
                    // JSON string representation - parse it
                    match serde_json::from_str::<Vec<String>>(s) {
                        Ok(patterns) => patterns,
                        Err(_) => Vec::new(),
                    }
                } else {
                    Vec::new()
                }
            })
            .unwrap_or_default();
        
        // Merge runtime exclusion patterns with configured ones
        let config = get_config();
        let mut all_exclude_patterns = config.search.exclude_patterns.clone();
        all_exclude_patterns.extend(exclude_patterns.clone());
        
        // Compile regex patterns for exclusion
        let exclude_regexes: Vec<Regex> = all_exclude_patterns
            .iter()
            .filter_map(|pattern| {
                match Regex::new(pattern) {
                    Ok(regex) => Some(regex),
                    Err(e) => {
                        tracing::warn!("Invalid regex pattern '{}': {}", pattern, e);
                        None
                    }
                }
            })
            .collect();
        
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        let query = SearchQuery {
            text: query_text,
            project_filter,
            session_filter: None,
            limit: limit * 3, // Get more results to allow for deduplication
        };

        let search_engine = self.search_engine.as_ref().unwrap();
        let all_results = search_engine.search(query)?;

        // Filter out excluded projects (both exact names and regex patterns)
        let filtered_results: Vec<_> = all_results
            .iter()
            .filter(|result| {
                // Check exact project name exclusions
                if exclude_projects.contains(&result.project) {
                    return false;
                }
                
                // Check regex pattern exclusions (against both project name and full path)
                for regex in &exclude_regexes {
                    if regex.is_match(&result.project) || regex.is_match(&result.project_path) {
                        return false;
                    }
                }
                
                true
            })
            .collect();

        // Deduplicate by session_id, keeping highest scoring result per session
        let mut session_best: std::collections::HashMap<String, &SearchResult> =
            std::collections::HashMap::new();
        for result in &filtered_results {
            match session_best.get(&result.session_id) {
                Some(existing) if existing.score >= result.score => {}
                _ => {
                    session_best.insert(result.session_id.clone(), result);
                }
            }
        }

        let mut results: Vec<_> = session_best.values().cloned().collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);

        let mut output = String::new();
        
        // Show debug information if requested
        if debug_mode {
            output.push_str("=== DEBUG MODE ===\n");
            output.push_str(&format!("Raw arguments: {:?}\n", args));
            output.push_str(&format!("Parsed exclude_projects: {:?}\n", exclude_projects));
            output.push_str(&format!("Parsed exclude_patterns: {:?}\n", exclude_patterns));
            output.push_str(&format!("Config exclude_patterns: {:?}\n", config.search.exclude_patterns));
            output.push_str(&format!("All exclude_patterns: {:?}\n", all_exclude_patterns));
            output.push_str(&format!("Compiled {} regex patterns\n", exclude_regexes.len()));
            output.push_str(&format!("Results: {} -> {} -> {}\n", all_results.len(), filtered_results.len(), results.len()));
            output.push_str("==================\n\n");
        }
        
        // Show active exclusion filters (only if filters are active and not in debug mode)
        if !debug_mode && (!exclude_projects.is_empty() || !all_exclude_patterns.is_empty()) {
            let mut filter_info = Vec::new();
            if !exclude_projects.is_empty() {
                filter_info.push(format!("{} projects", exclude_projects.len()));
            }
            if !all_exclude_patterns.is_empty() {
                filter_info.push(format!("{} patterns", all_exclude_patterns.len()));
            }
            output.push_str(&format!("Excluding: {}\n\n", filter_info.join(", ")));
        }
        
        if results.is_empty() {
            output.push_str("No results found.\n");
        } else {
            output.push_str(&format!("Found {} results:\n\n", results.len()));

            for (i, result) in results.iter().enumerate() {
                output.push_str(&format!(
                    "{}. [{}] {} (score: {:.2})\n",
                    i + 1,
                    result.project,
                    result.timestamp.format("%Y-%m-%d %H:%M"),
                    result.score
                ));

                // Show full project path if different from project name
                if result.project_path != result.project && result.project_path != "unknown" {
                    output.push_str(&format!("   Path: {}\n", result.project_path));
                }

                // Add metadata tags (machine-readable, no emojis)
                let mut tags = Vec::new();
                if !result.technologies.is_empty() {
                    tags.push(format!("tech: {}", result.technologies.join(", ")));
                }
                if !result.code_languages.is_empty() {
                    tags.push(format!("lang: {}", result.code_languages.join(", ")));
                }
                if result.has_code {
                    tags.push("has_code".to_string());
                }
                if result.has_error {
                    tags.push("has_error".to_string());
                }
                if !result.tools_mentioned.is_empty() && result.tools_mentioned.len() <= 3 {
                    tags.push(format!("tools: {}", result.tools_mentioned.join(", ")));
                }
                tags.push(format!("words: {}", result.word_count));

                if !tags.is_empty() {
                    output.push_str(&format!("   {}\n", tags.join(" • ")));
                }

                output.push_str(&format!("   Session: {}\n", result.session_id));
                output.push_str(&format!("   {}\n\n", result.snippet));
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

    async fn tool_analyze_conversation_content(&self, args: Option<Value>) -> Result<Value> {
        conversation_aggregator::handle_analyze_conversation_content(
            self.search_engine.as_ref(),
            args,
        )
        .await
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
