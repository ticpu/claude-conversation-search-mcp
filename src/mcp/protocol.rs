use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tracing::{debug, error};

use super::server::McpServer;

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

    async fn handle_list_tools(&self) -> Result<Value> {
        debug!("Handling list_tools request");

        let response = ListToolsResponse {
            tools: super::tools::schema::tool_list(),
        };
        Ok(serde_json::to_value(response)?)
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
                error!("Notification {} failed: {}", request.method, e);
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
                error: Some(JsonRpcError {
                    code: -32603,
                    message: e.to_string(),
                    data: None,
                }),
            },
        })
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

        match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => {
                if let Some(response) = server
                    .handle_request(request)
                    .await
                {
                    let response_json = serde_json::to_string(&response)?;
                    debug!("Sending response: {}", response_json);

                    stdout
                        .write_all(response_json.as_bytes())
                        .await?;
                    stdout
                        .write_all(b"\n")
                        .await?;
                    stdout
                        .flush()
                        .await?;
                }
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
                stdout
                    .write_all(response_json.as_bytes())
                    .await?;
                stdout
                    .write_all(b"\n")
                    .await?;
                stdout
                    .flush()
                    .await?;
            }
        }
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
    fn requests_with_id_expect_a_response() {
        assert!(!is_notification(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#
        ));
        // Zero is a valid id, not an absent one.
        assert!(!is_notification(
            r#"{"jsonrpc":"2.0","id":0,"method":"tools/list"}"#
        ));
    }
}
