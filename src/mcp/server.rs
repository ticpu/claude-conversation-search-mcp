//! MCP server state and tool dispatch.
//!
//! While the index service holds indexer.lock this server is a pure reader:
//! the reindex tool is left out of tools/list and refused if called, and the
//! staleness note is not shown, since the service indexes changes within
//! seconds. Whether the service runs is probed on every request and compared
//! with what tools/list last advertised; a change sends
//! notifications/tools/list_changed before the response, so the client
//! re-lists.
//!
//! The engine's per-session message counts are a snapshot of
//! cache-metadata.json; it is reopened when that file's mtime changes, which
//! is every commit by whichever process writes the index.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracing::debug;

use super::protocol::{CallToolRequest, tool_err};
use crate::shared::{self, SearchEngine, auto_index, get_cache_dir, service_running};

pub struct McpServer {
    pub(crate) search_engine: SearchEngine,
    pub(crate) cache_dir: PathBuf,
    /// cache-metadata.json's mtime when the engine was opened.
    engine_metadata_mtime: Option<SystemTime>,
    /// Whether the service ran when tools/list last answered; None before any.
    advertised_service: Option<bool>,
}

fn metadata_mtime(cache_dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(cache_dir.join("cache-metadata.json"))
        .and_then(|m| m.modified())
        .ok()
}

impl McpServer {
    pub fn new() -> Result<Self> {
        let cache_dir = get_cache_dir()?;
        auto_index(&cache_dir, shared::Merges::Abandon)?;
        Self::open(cache_dir)
    }

    /// Serve the index at `cache_dir` as it stands, without indexing first.
    pub(crate) fn open(cache_dir: PathBuf) -> Result<Self> {
        let engine_metadata_mtime = metadata_mtime(&cache_dir);
        let (_cache, search_engine) = shared::open_search_engine(&cache_dir)?;
        Ok(Self {
            search_engine,
            cache_dir,
            engine_metadata_mtime,
            advertised_service: None,
        })
    }

    pub(crate) fn service_running(&self) -> bool {
        service_running(&self.cache_dir)
    }

    /// Record what tools/list is about to advertise.
    pub(crate) fn advertise_tools(&mut self) -> bool {
        let running = self.service_running();
        self.advertised_service = Some(running);
        running
    }

    /// True once per change of the service state since tools/list last answered.
    pub(crate) fn take_tools_list_change(&mut self) -> bool {
        let Some(advertised) = self.advertised_service else {
            return false;
        };
        let running = self.service_running();
        self.advertised_service = Some(running);
        running != advertised
    }

    /// Reopen the engine when another commit changed the metadata it snapshotted.
    fn refresh_engine(&mut self) -> Result<()> {
        let mtime = metadata_mtime(&self.cache_dir);
        if mtime == self.engine_metadata_mtime {
            return Ok(());
        }
        let (_cache, engine) = shared::open_search_engine(&self.cache_dir)
            .context("reopening the index after a commit")?;
        self.search_engine = engine;
        self.engine_metadata_mtime = mtime;
        Ok(())
    }

    pub(crate) async fn handle_call_tool(&mut self, params: Value) -> Result<Value> {
        let request: CallToolRequest = serde_json::from_value(params)?;
        debug!("Handling tool call: {}", request.name);
        self.refresh_engine()?;

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
