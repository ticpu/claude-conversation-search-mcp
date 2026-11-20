use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tracing::{debug, error};

use super::server::McpServer;
use crate::shared::explain_index_error;

/// Parse date string: YYYY-MM-DD (as start of day UTC) or full ISO 8601
// MCP Protocol Structures
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

/// The message carries the whole cause chain: the client sees nothing else.
fn internal_error(e: &anyhow::Error) -> JsonRpcError {
    JsonRpcError {
        code: -32603,
        message: format!("{e:#}"),
        data: None,
    }
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
pub(crate) struct Tool {
    pub(crate) name: String,
    pub(crate) description: String,
    #[serde(rename = "inputSchema")]
    pub(crate) input_schema: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CallToolRequest {
    pub(crate) name: String,
    pub(crate) arguments: Option<Value>,
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

/// Build a successful tool response (`isError` omitted).
pub(crate) fn tool_ok(text: impl Into<String>) -> Result<Value> {
    Ok(serde_json::to_value(CallToolResponse {
        content: vec![ToolResult {
            result_type: "text".to_string(),
            text: text.into(),
        }],
        is_error: None,
    })?)
}

/// Build a tool response flagged `isError: true`.
pub(crate) fn tool_err(text: impl Into<String>) -> Result<Value> {
    Ok(serde_json::to_value(CallToolResponse {
        content: vec![ToolResult {
            result_type: "text".to_string(),
            text: text.into(),
        }],
        is_error: Some(true),
    })?)
}

impl McpServer {
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
                name: "claude-conversation-search".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        Ok(serde_json::to_value(response)?)
    }

    /// The reindex tool is listed only while no index service owns indexing.
    async fn handle_list_tools(&mut self) -> Result<Value> {
        debug!("Handling list_tools request");

        let service_running = self.advertise_tools();
        let tools = super::tools::schema::tool_list()
            .into_iter()
            .filter(|tool| !(service_running && tool.name == "reindex"))
            .collect();
        Ok(serde_json::to_value(ListToolsResponse { tools })?)
    }

    /// Returns `None` for JSON-RPC notifications (no `id`), which must not be
    /// answered. The method is still dispatched so side effects are not lost.
    pub(crate) async fn handle_request(
        &mut self,
        request: JsonRpcRequest,
    ) -> Option<JsonRpcResponse> {
        let is_notification = request
            .id
            .is_none();

        let result = match request
            .method
            .as_str()
        {
            "initialize" => {
                self.handle_initialize(request.params)
                    .await
            }
            "ping" => Ok(serde_json::json!({})),
            "tools/list" => {
                self.handle_list_tools()
                    .await
            }
            "tools/call" => {
                self.handle_call_tool(
                    request
                        .params
                        .unwrap_or_default(),
                )
                .await
            }
            // Spec-defined notification namespaces: dispatching them here keeps
            // routine traffic out of the unknown-method error path.
            m if m.starts_with("notifications/") || m.starts_with("$/") => {
                debug!("Received notification: {}", m);
                Ok(serde_json::Value::Null)
            }
            _ => Err(anyhow::anyhow!("Unknown method: {}", request.method)),
        };

        if is_notification {
            if let Err(e) = result {
                error!("Notification {} failed: {:#}", request.method, e);
            }
            return None;
        }

        Some(match result {
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
                error: Some(internal_error(&explain_index_error(e, &self.cache_dir))),
            },
        })
    }
}

const TOOLS_LIST_CHANGED: &str = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;

impl McpServer {
    /// The lines one input line produces on stdout: a tools/list_changed
    /// notification first when the tool set changed, then the response if any.
    pub(crate) async fn process_line(&mut self, line: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if self.take_tools_list_change() {
            out.push(TOOLS_LIST_CHANGED.to_string());
        }
        let response = match serde_json::from_str::<JsonRpcRequest>(line) {
            Ok(request) => {
                self.handle_request(request)
                    .await
            }
            Err(e) => {
                error!("Failed to parse JSON-RPC request: {}", e);
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {e}"),
                        data: None,
                    }),
                })
            }
        };
        if let Some(response) = response {
            out.push(serde_json::to_string(&response)?);
        }
        Ok(out)
    }
}

pub async fn run_mcp_server() -> Result<()> {
    // Initialize logging to stderr so it doesn't interfere with JSON-RPC
    // Only show CRITICAL/ERROR level logs to avoid JSON parsing issues
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter("error")
        .init();

    let mut server = McpServer::new()?;
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = AsyncBufReader::new(stdin).lines();

    while let Some(line) = reader
        .next_line()
        .await?
    {
        if line
            .trim()
            .is_empty()
        {
            continue;
        }
        debug!("Received line: {}", line);

        for out in server
            .process_line(&line)
            .await?
        {
            debug!("Sending: {}", out);
            stdout
                .write_all(out.as_bytes())
                .await?;
            stdout
                .write_all(b"\n")
                .await?;
        }
        stdout
            .flush()
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_notification(line: &str) -> bool {
        serde_json::from_str::<JsonRpcRequest>(line)
            .unwrap()
            .id
            .is_none()
    }

    #[test]
    fn requests_without_id_are_notifications() {
        assert!(is_notification(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        ));
        assert!(is_notification(
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#
        ));
    }

    #[test]
    fn internal_error_keeps_the_cause_chain() {
        let e = anyhow::anyhow!("permission denied for schema freeswitch").context("reading index");
        let message = internal_error(&e).message;
        assert_eq!(
            message,
            "reading index: permission denied for schema freeswitch"
        );
    }

    #[test]
    fn requests_with_id_expect_a_response() {
        assert!(!is_notification(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#
        ));
        // Zero is a valid id, not an absent one.
        assert!(!is_notification(
            r#"{"jsonrpc":"2.0","id":0,"method":"tools/list"}"#
        ));
    }

    fn write_session(dir: &std::path::Path, session: &str) -> std::path::PathBuf {
        let path = dir.join(format!("{session}.jsonl"));
        std::fs::write(
            &path,
            format!(
                r#"{{"uuid":"{session}-u","sessionId":"{session}","type":"user","timestamp":"2025-12-28T10:00:00Z","message":{{"role":"user","content":"hello {session}"}}}}"#
            ) + "\n",
        )
        .unwrap();
        path
    }

    /// An index under `dir` holding the given transcripts, its writer closed.
    fn index_with(dir: &std::path::Path, files: Vec<std::path::PathBuf>) -> std::path::PathBuf {
        let index = dir.join("idx");
        let (mut indexer, mut cache) = crate::shared::open_or_create_indexer(&index).unwrap();
        cache
            .update_incremental(&mut indexer, files)
            .unwrap();
        index
    }

    fn tool_names(response: &str) -> Vec<String> {
        let value: Value = serde_json::from_str(response).unwrap();
        value["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                t["name"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn reindex_follows_the_index_service() {
        let temp = tempfile::TempDir::new().unwrap();
        let index = index_with(temp.path(), Vec::new());
        let mut server = McpServer::open(index.clone()).unwrap();
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

        let out = server
            .process_line(list)
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(tool_names(&out[0]).contains(&"reindex".to_string()));

        let service = crate::shared::IndexerLock::try_acquire(&index).unwrap();
        let out = server
            .process_line(list)
            .await
            .unwrap();
        assert_eq!(out[0], TOOLS_LIST_CHANGED);
        assert!(!tool_names(&out[1]).contains(&"reindex".to_string()));

        let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"reindex"}}"#;
        let out = server
            .process_line(call)
            .await
            .unwrap();
        assert_eq!(out.len(), 1, "no change since the last list: {out:?}");
        assert!(out[0].contains("index service owns indexing"), "{}", out[0]);

        drop(service);
        let out = server
            .process_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await
            .unwrap();
        assert_eq!(out, [TOOLS_LIST_CHANGED], "a notification gets no response");
    }

    #[tokio::test]
    async fn session_counts_follow_commits_by_another_writer() {
        let temp = tempfile::TempDir::new().unwrap();
        let first = write_session(temp.path(), "sess-a");
        let index = index_with(temp.path(), vec![first]);
        let mut server = McpServer::open(index.clone()).unwrap();

        // Another process indexes a new session; mtime granularity needs a tick.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = write_session(temp.path(), "sess-b");
        index_with(temp.path(), vec![second]);

        let call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_messages","arguments":{"ids":["x"]}}}"#;
        server
            .process_line(call)
            .await
            .unwrap();
        let hits = server
            .search_engine
            .search(crate::shared::SearchQuery {
                text: "hello".to_string(),
                project_filter: None,
                session_filter: Some("sess-b".to_string()),
                limit: 10,
                sort_by: crate::shared::SortOrder::Relevance,
                after: None,
                before: None,
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].interaction_count, 1,
            "counts from the newer metadata"
        );
    }

    #[tokio::test]
    async fn ping_answers_an_empty_result() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::open(index_with(temp.path(), Vec::new())).unwrap();
        let out = server
            .process_line(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#)
            .await
            .unwrap();
        assert_eq!(out, [r#"{"jsonrpc":"2.0","id":7,"result":{}}"#]);
    }
}
