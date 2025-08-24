use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tracing::{debug, error, info, warn};

mod cache;
mod indexer;
mod metadata;
mod models;
mod parser;
mod search;
mod shared;

use cache::CacheManager;
use models::SearchQuery;
use search::SearchEngine;

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
struct CallToolResponse {
    content: Vec<ToolResult>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolResult {
    #[serde(rename = "type")]
    result_type: String,
    text: String,
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
            let cache_dir = shared::get_cache_dir()?;

            // Auto-index if needed
            shared::auto_index(&cache_dir).await?;

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
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results to return (default: 10)",
                            "optional": true,
                            "default": 10
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
                description: "Analyze technology topics and patterns across conversations".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional project name to filter analysis",
                            "optional": true
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of topics to return per category (default: 20)",
                            "optional": true,
                            "default": 20
                        }
                    }
                }),
            },
            Tool {
                name: "get_conversation_stats".to_string(),
                description: "Get detailed statistics about conversation data and cache".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional project name to filter stats",
                            "optional": true
                        }
                    }
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
                self.tool_get_conversation_context(request.arguments)
                    .await?
            }
            "analyze_conversation_topics" => self.tool_analyze_topics(request.arguments).await?,
            "get_conversation_stats" => self.tool_get_stats(request.arguments).await?,
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

        let project_filter = args
            .get("project")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        let query = SearchQuery {
            text: query_text,
            project_filter,
            session_filter: None,
            limit,
        };

        let search_engine = self.search_engine.as_ref().unwrap();
        let results = search_engine.search(query)?;

        let mut output = String::new();
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

                // Add metadata tags
                let mut tags = Vec::new();
                if !result.technologies.is_empty() {
                    tags.push(format!("🔧 {}", result.technologies.join(", ")));
                }
                if !result.code_languages.is_empty() {
                    tags.push(format!("💻 {}", result.code_languages.join(", ")));
                }
                if result.has_code {
                    tags.push("📝 code".to_string());
                }
                if result.has_error {
                    tags.push("🚨 error".to_string());
                }
                if !result.tools_mentioned.is_empty() && result.tools_mentioned.len() <= 3 {
                    tags.push(format!("🔨 {}", result.tools_mentioned.join(", ")));
                }
                tags.push(format!("📊 {} words", result.word_count));

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

    async fn tool_get_conversation_context(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?
            .to_string();

        let include_content = args
            .get("include_content")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let query = SearchQuery {
            text: format!("session_id:{session_id}"),
            project_filter: None,
            session_filter: None,
            limit: 100,
        };

        let search_engine = self.search_engine.as_ref().unwrap();
        let results = search_engine.search(query)?;

        if results.is_empty() {
            return Ok(serde_json::to_value(CallToolResponse {
                content: vec![ToolResult {
                    result_type: "text".to_string(),
                    text: format!("No conversations found for session: {session_id}"),
                }],
                is_error: Some(true),
            })?);
        }

        // Sort by timestamp
        let mut sorted_results = results;
        sorted_results.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        let mut output = String::new();
        output.push_str(&format!("📋 Session: {session_id}\n"));
        output.push_str(&format!("📂 Project: {}\n", sorted_results[0].project));
        output.push_str(&format!(
            "🕒 Time range: {} to {}\n",
            sorted_results[0].timestamp.format("%Y-%m-%d %H:%M"),
            sorted_results
                .last()
                .unwrap()
                .timestamp
                .format("%Y-%m-%d %H:%M")
        ));
        output.push_str(&format!("💬 Total messages: {}\n\n", sorted_results.len()));

        // Session-level metadata
        let mut session_techs = std::collections::HashSet::new();
        let mut session_langs = std::collections::HashSet::new();
        let mut has_code = false;
        let mut has_errors = false;

        for result in &sorted_results {
            session_techs.extend(result.technologies.iter().cloned());
            session_langs.extend(result.code_languages.iter().cloned());
            if result.has_code {
                has_code = true;
            }
            if result.has_error {
                has_errors = true;
            }
        }

        let mut session_tags = Vec::new();
        if !session_techs.is_empty() {
            let mut techs: Vec<_> = session_techs.into_iter().collect();
            techs.sort();
            session_tags.push(format!("🔧 {}", techs.join(", ")));
        }
        if !session_langs.is_empty() {
            let mut langs: Vec<_> = session_langs.into_iter().collect();
            langs.sort();
            session_tags.push(format!("💻 {}", langs.join(", ")));
        }
        if has_code {
            session_tags.push("📝 code".to_string());
        }
        if has_errors {
            session_tags.push("🚨 errors".to_string());
        }

        if !session_tags.is_empty() {
            output.push_str(&format!("Session topics: {}\n\n", session_tags.join(" • ")));
        }

        output.push_str("Messages:\n");
        output.push_str(&format!("{}\n", "─".repeat(80)));

        for (i, result) in sorted_results.iter().enumerate() {
            output.push_str(&format!(
                "{}. {} | Score: {:.2}\n",
                i + 1,
                result.timestamp.format("%H:%M:%S"),
                result.score
            ));

            if include_content {
                output.push_str(&format!("{}\n", result.content));
            } else {
                output.push_str(&format!("{}\n", result.snippet));
            }

            if i < sorted_results.len() - 1 {
                output.push_str(&format!("{}\n", "─".repeat(40)));
            }
        }

        if !include_content && sorted_results.len() > 3 {
            output.push_str("\nTip: Use include_content: true to see full message content\n");
        }

        Ok(serde_json::to_value(CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: output,
            }],
            is_error: None,
        })?)
    }

    async fn tool_analyze_topics(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let project_filter = args
            .get("project")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        let query = SearchQuery {
            text: "*".to_string(),
            project_filter: project_filter.clone(),
            session_filter: None,
            limit: 1000,
        };

        let search_engine = self.search_engine.as_ref().unwrap();
        let results = search_engine.search(query)?;

        let mut tech_counts = HashMap::new();
        let mut lang_counts = HashMap::new();
        let mut tool_counts = HashMap::new();
        let mut project_counts = HashMap::new();

        for result in &results {
            project_counts
                .entry(result.project.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);

            for tech in &result.technologies {
                tech_counts
                    .entry(tech.clone())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
            }

            for lang in &result.code_languages {
                lang_counts
                    .entry(lang.clone())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
            }

            for tool in &result.tools_mentioned {
                tool_counts
                    .entry(tool.clone())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
            }
        }

        let mut output = String::new();
        output.push_str(&format!(
            "Topic Analysis - {} conversations analyzed\n\n",
            results.len()
        ));

        if let Some(ref project) = project_filter {
            output.push_str(&format!("Filtered by project: {project}\n\n"));
        }

        // Top technologies
        if !tech_counts.is_empty() {
            output.push_str("🔧 Top Technologies:\n");
            let mut sorted_tech: Vec<_> = tech_counts.iter().collect();
            sorted_tech.sort_by(|a, b| b.1.cmp(a.1));

            for (tech, count) in sorted_tech.iter().take(limit) {
                output.push_str(&format!("   {tech} ({count})\n"));
            }
            output.push('\n');
        }

        // Top programming languages
        if !lang_counts.is_empty() {
            output.push_str("💻 Top Programming Languages:\n");
            let mut sorted_lang: Vec<_> = lang_counts.iter().collect();
            sorted_lang.sort_by(|a, b| b.1.cmp(a.1));

            for (lang, count) in sorted_lang.iter().take(limit) {
                output.push_str(&format!("   {lang} ({count})\n"));
            }
            output.push('\n');
        }

        // Top tools mentioned
        if !tool_counts.is_empty() {
            output.push_str("🔨 Top Tools Mentioned:\n");
            let mut sorted_tools: Vec<_> = tool_counts.iter().collect();
            sorted_tools.sort_by(|a, b| b.1.cmp(a.1));

            for (tool, count) in sorted_tools.iter().take(limit) {
                output.push_str(&format!("   {tool} ({count})\n"));
            }
            output.push('\n');
        }

        // Project breakdown (if not filtering by project)
        if project_filter.is_none() && !project_counts.is_empty() {
            output.push_str("📂 Project Activity:\n");
            let mut sorted_projects: Vec<_> = project_counts.iter().collect();
            sorted_projects.sort_by(|a, b| b.1.cmp(a.1));

            for (project, count) in sorted_projects.iter().take(limit) {
                output.push_str(&format!("   {project} ({count} conversations)\n"));
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

    async fn tool_get_stats(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let project_filter = args
            .get("project")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let cache_manager = self.cache_manager.as_ref().unwrap();
        let (total_files, total_entries, last_updated) = cache_manager.get_basic_stats();

        let query = SearchQuery {
            text: "*".to_string(),
            project_filter: project_filter.clone(),
            session_filter: None,
            limit: 2000,
        };

        let search_engine = self.search_engine.as_ref().unwrap();
        let results = search_engine.search(query)?;

        let mut code_conversations = 0;
        let mut error_conversations = 0;
        let mut total_words = 0;
        let mut session_counts = HashMap::new();

        for result in &results {
            if result.has_code {
                code_conversations += 1;
            }
            if result.has_error {
                error_conversations += 1;
            }
            total_words += result.word_count;

            session_counts
                .entry(result.session_id.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
        }

        let mut output = String::new();
        if let Some(ref project) = project_filter {
            output.push_str(&format!("📊 Statistics for project: {project}\n\n"));
        } else {
            output.push_str("📊 Overall Statistics\n\n");
        }

        output.push_str("Cache Information:\n");
        output.push_str(&format!(
            "  📁 Total files indexed: {}\n",
            total_files
        ));
        output.push_str(&format!(
            "  💾 Total entries: {}\n",
            total_entries
        ));

        if let Some(last_updated_time) = last_updated {
            output.push_str(&format!(
                "  🕒 Last updated: {}\n",
                last_updated_time.format("%Y-%m-%d %H:%M UTC")
            ));
        }

        output.push('\n');

        output.push_str("Conversation Analysis:\n");
        output.push_str(&format!("  💬 Total conversations: {}\n", results.len()));
        output.push_str(&format!("  🏗️ Unique sessions: {}\n", session_counts.len()));
        output.push_str(&format!(
            "  📝 Conversations with code: {} ({:.1}%)\n",
            code_conversations,
            (code_conversations as f64 / results.len() as f64) * 100.0
        ));
        output.push_str(&format!(
            "  🚨 Conversations with errors: {} ({:.1}%)\n",
            error_conversations,
            (error_conversations as f64 / results.len() as f64) * 100.0
        ));
        output.push_str(&format!(
            "  📊 Total words: {} (avg: {} per conversation)\n",
            total_words,
            if !results.is_empty() {
                total_words / results.len()
            } else {
                0
            }
        ));

        // Show most active sessions
        if !session_counts.is_empty() {
            output.push('\n');
            output.push_str("Most Active Sessions:\n");
            let mut sorted_sessions: Vec<_> = session_counts.iter().collect();
            sorted_sessions.sort_by(|a, b| b.1.cmp(a.1));

            for (session_id, count) in sorted_sessions.iter().take(5) {
                let short_id = if session_id.len() > 12 {
                    format!("{}...", &session_id[..12])
                } else {
                    session_id.to_string()
                };
                output.push_str(&format!("  {short_id} ({count} messages)\n"));
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

    fn create_error_response(
        &self,
        id: Option<Value>,
        code: i32,
        message: &str,
    ) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
        }
    }

    async fn handle_request(&mut self, request: JsonRpcRequest) -> JsonRpcResponse {
        debug!("Handling request: {}", request.method);

        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params).await,
            "tools/list" => self.handle_list_tools().await,
            "tools/call" => {
                self.handle_call_tool(request.params.unwrap_or_default())
                    .await
            }
            "initialized" => {
                // Just acknowledge the initialized notification
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(serde_json::json!({})),
                    error: None,
                };
            }
            _ => {
                warn!("Unknown method: {}", request.method);
                return self.create_error_response(
                    request.id,
                    -32601,
                    &format!("Method not found: {}", request.method),
                );
            }
        };

        match result {
            Ok(result) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: Some(result),
                error: None,
            },
            Err(e) => {
                error!("Error handling request: {}", e);
                self.create_error_response(request.id, -32603, &e.to_string())
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging to stderr so it doesn't interfere with JSON-RPC
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter("claude_search=info")
        .init();

    info!("Claude Search MCP Server starting...");

    let mut server = McpServer::new();
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = AsyncBufReader::new(stdin).lines();

    info!("MCP Server ready, listening on stdin/stdout");

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

    info!("MCP Server shutting down");
    Ok(())
}
