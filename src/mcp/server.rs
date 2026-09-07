use anyhow::{Context, Result};
use serde_json::Value;
use tracing::debug;

use super::protocol::{CallToolRequest, tool_err};
use crate::shared::{self, SearchEngine, auto_index, get_cache_dir, get_config};

/// Empty the index directory for a full rebuild, keeping the lock file: removing
/// it would drop the exclusive access this rebuild is running under.
pub(crate) fn clear_index_dir(dir: &std::path::Path) -> Result<()> {
    let lock_file = get_config().get_lock_file_path()?;
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("reading index directory {}", dir.display()))?;
    for entry in entries {
        let path = entry
            .with_context(|| format!("listing index directory {}", dir.display()))?
            .path();
        if path == lock_file {
            continue;
        }
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

pub struct McpServer {
    pub(crate) search_engine: SearchEngine,
    pub(crate) cache_dir: std::path::PathBuf,
}

impl McpServer {
    pub fn new() -> Result<Self> {
        let cache_dir = get_cache_dir()?;

        // Auto-index if needed
        auto_index(&cache_dir)?;

        let (_cache, search_engine) = shared::open_search_engine(&cache_dir)?;

        Ok(Self {
            search_engine,
            cache_dir,
        })
    }

    pub(crate) async fn handle_call_tool(&mut self, params: Value) -> Result<Value> {
        let request: CallToolRequest = serde_json::from_value(params)?;
        debug!("Handling tool call: {}", request.name);

        let result = match request
            .name
            .as_str()
        {
            "search_conversations" => {
                self.tool_search_conversations(request.arguments)
                    .await?
            }
            "respawn_server" => {
                self.tool_respawn()
                    .await?
            }
            "reindex" => {
                self.tool_reindex(request.arguments)
                    .await?
            }
            "get_session_messages" => {
                self.tool_get_session_messages(request.arguments)
                    .await?
            }
            "summarize_session" => {
                self.tool_summarize_session(request.arguments)
                    .await?
            }
            "get_messages" => {
                self.tool_get_messages(request.arguments)
                    .await?
            }
            _ => {
                return tool_err(format!("Unknown tool: {}", request.name));
            }
        };

        Ok(result)
    }
}
