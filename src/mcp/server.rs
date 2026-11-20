//! MCP server state and tool dispatch.
//!
//! The server answers from the first request: indexing never runs before the
//! stdin loop. A background task brings the index up to date while an
//! existing index is searched as it stands; only with no index yet do the
//! search tools answer with a retry message carrying the progress. A failure
//! of the background task is kept for the life of the process, so it is seen
//! rather than leaving searches silently stale: while an index is open, every
//! later search, session, messages and summarize answer ends with a line
//! naming it, and only when no index could be opened is it the tool error by
//! itself. The engine lives
//! in an arc-swap snapshot shared with that task, which swaps in a fresh one
//! when it finishes.
//!
//! While the index service holds indexer.lock this server is a pure reader:
//! the background pass finds the writer lock busy and stops at once, the
//! reindex tool is left out of tools/list and refused if called, and the
//! staleness note is not shown, since the service indexes changes within
//! seconds. Whether the service runs is probed on every request and compared
//! with what tools/list last advertised; a change sends
//! notifications/tools/list_changed before the response, so the client
//! re-lists.
//!
//! The engine's per-session message counts are a snapshot of
//! cache-metadata.json; it is reopened when that file's mtime changes, which
//! is every commit by whichever process writes the index.

use anyhow::Result;
use arc_swap::ArcSwapOption;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::SystemTime;
use tracing::{debug, error};

use super::protocol::{CallToolRequest, tool_err};
use crate::shared::{
    self, IndexProgress, SearchEngine, auto_index, get_cache_dir, service_running,
};

/// An open engine and the metadata mtime its session counts were read at.
pub(crate) struct LoadedEngine {
    pub(crate) engine: SearchEngine,
    metadata_mtime: Option<SystemTime>,
}

impl LoadedEngine {
    fn open(cache_dir: &Path) -> Result<Self> {
        let metadata_mtime = metadata_mtime(cache_dir);
        let (_cache, engine) = shared::open_search_engine(cache_dir)?;
        Ok(Self {
            engine,
            metadata_mtime,
        })
    }
}

/// The background indexing task as tool calls see it.
#[derive(Default)]
pub(crate) struct Indexing {
    running: AtomicBool,
    files_done: AtomicUsize,
    files_total: AtomicUsize,
    failure: ArcSwapOption<anyhow::Error>,
}

impl Indexing {
    pub(crate) fn report(&self, progress: IndexProgress) {
        self.files_total
            .store(progress.files_total, Ordering::Relaxed);
        self.files_done
            .store(progress.files_done, Ordering::Relaxed);
    }
}

pub struct McpServer {
    pub(crate) cache_dir: PathBuf,
    engine: Arc<ArcSwapOption<LoadedEngine>>,
    indexing: Arc<Indexing>,
    /// Whether the service ran when tools/list last answered; None before any.
    advertised_service: Option<bool>,
}

fn metadata_mtime(cache_dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(cache_dir.join("cache-metadata.json"))
        .and_then(|m| m.modified())
        .ok()
}

fn index_exists(cache_dir: &Path) -> bool {
    cache_dir
        .join("meta.json")
        .exists()
}

impl McpServer {
    /// Serve the configured index, updating it in the background.
    pub fn new() -> Result<Self> {
        let cache_dir = get_cache_dir()?;
        let job_dir = cache_dir.clone();
        Ok(Self::start(cache_dir, move |indexing| {
            auto_index(&job_dir, shared::Merges::Abandon, &mut |progress| {
                indexing.report(progress);
                Ok(())
            })
        }))
    }

    /// Serve the index at `cache_dir`, opening it now if it exists, while `job`
    /// runs on a blocking thread. Must be called within a tokio runtime.
    pub(crate) fn start(
        cache_dir: PathBuf,
        job: impl FnOnce(&Indexing) -> Result<()> + Send + 'static,
    ) -> Self {
        let engine = Arc::new(ArcSwapOption::empty());
        if index_exists(&cache_dir) {
            match LoadedEngine::open(&cache_dir) {
                Ok(loaded) => engine.store(Some(Arc::new(loaded))),
                Err(e) => error!(
                    "Opening {} at startup, waiting for the background pass: {e:#}",
                    cache_dir.display()
                ),
            }
        }
        let indexing = Arc::new(Indexing::default());
        indexing
            .running
            .store(true, Ordering::SeqCst);

        let (task_engine, task_indexing, task_dir) =
            (engine.clone(), indexing.clone(), cache_dir.clone());
        tokio::task::spawn_blocking(move || {
            let result = job(&task_indexing).and_then(|()| {
                if index_exists(&task_dir) {
                    task_engine.store(Some(Arc::new(LoadedEngine::open(&task_dir)?)));
                }
                Ok(())
            });
            if let Err(e) = result {
                error!("Background indexing failed: {e:#}");
                task_indexing
                    .failure
                    .store(Some(Arc::new(e)));
            }
            task_indexing
                .running
                .store(false, Ordering::SeqCst);
        });

        Self {
            cache_dir,
            engine,
            indexing,
            advertised_service: None,
        }
    }

    /// The engine to answer a tool call with, or the text of the tool error to
    /// answer instead: the background failure when no index could be opened,
    /// or that the index is not built yet.
    pub(crate) fn ready_engine(&self) -> Result<Arc<LoadedEngine>, String> {
        let failure = self
            .indexing
            .failure
            .load_full();
        let running = self
            .indexing
            .running
            .load(Ordering::SeqCst);
        let current = self
            .engine
            .load_full();
        let stale = match &current {
            Some(loaded) => loaded.metadata_mtime != metadata_mtime(&self.cache_dir),
            None => !running && index_exists(&self.cache_dir),
        };
        if stale {
            let loaded = Arc::new(LoadedEngine::open(&self.cache_dir).map_err(
                |e| match &failure {
                    Some(failure) => format!(
                        "indexing the conversations failed: {failure:#}; opening the index: {e:#}"
                    ),
                    None => format!("opening the index: {e:#}"),
                },
            )?);
            self.engine
                .store(Some(loaded.clone()));
            return Ok(loaded);
        }
        match (current, failure) {
            (Some(loaded), _) => Ok(loaded),
            (None, Some(failure)) => Err(format!("indexing the conversations failed: {failure:#}")),
            (None, None) => Err(format!(
                "the index is being built ({}/{} files), retry shortly",
                self.indexing
                    .files_done
                    .load(Ordering::Relaxed),
                self.indexing
                    .files_total
                    .load(Ordering::Relaxed)
            )),
        }
    }

    /// `text` followed by the background failure line, when the pass failed.
    pub(crate) fn with_failure_note(&self, text: impl Into<String>) -> String {
        let mut text = text.into();
        if let Some(failure) = self
            .indexing
            .failure
            .load_full()
        {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&format!(
                "background indexing failed: {failure:#}; results may be stale"
            ));
        }
        text
    }

    /// Swap in an engine opened after this process committed.
    pub(crate) fn reload_engine(&self) -> Result<()> {
        self.engine
            .store(Some(Arc::new(LoadedEngine::open(&self.cache_dir)?)));
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn write_session(dir: &Path, session: &str) -> PathBuf {
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

    fn build_index(index: &Path, files: Vec<PathBuf>) -> Result<()> {
        let (mut indexer, mut cache) =
            shared::open_or_create_indexer(index, shared::WriterThreads::PerCore)?;
        cache.update_incremental(&mut indexer, files)?;
        Ok(())
    }

    /// The text and isError flag of a get_messages call for `id`.
    async fn get_message(server: &mut McpServer, id: &str) -> (String, bool) {
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"get_messages","arguments":{{"ids":["{id}"]}}}}}}"#
        );
        let out = server
            .process_line(&line)
            .await
            .unwrap();
        let value: Value = serde_json::from_str(
            out.last()
                .unwrap(),
        )
        .unwrap();
        let result = &value["result"];
        (
            result["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("no tool result in {value}"))
                .to_string(),
            result["isError"] == Value::Bool(true),
        )
    }

    async fn wait_until(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn answers_while_the_first_index_is_built() {
        let temp = tempfile::TempDir::new().unwrap();
        let index = temp
            .path()
            .join("idx");
        let transcript = write_session(temp.path(), "sess-a");
        let (release, gate) = mpsc::channel::<()>();
        let job_index = index.clone();
        let mut server = McpServer::start(index.clone(), move |indexing| {
            indexing.report(IndexProgress {
                files_done: 3,
                files_total: 10,
            });
            gate.recv()?;
            build_index(&job_index, vec![transcript])
        });

        let out = server
            .process_line(r#"{"jsonrpc":"2.0","id":0,"method":"initialize"}"#)
            .await
            .unwrap();
        assert!(out[0].contains("serverInfo"), "{out:?}");

        let indexing = server
            .indexing
            .clone();
        wait_until(|| {
            indexing
                .files_total
                .load(Ordering::Relaxed)
                == 10
        })
        .await;
        let (text, is_error) = get_message(&mut server, "sess-a-u").await;
        assert!(is_error);
        assert_eq!(text, "the index is being built (3/10 files), retry shortly");

        release
            .send(())
            .unwrap();
        wait_until(|| {
            !indexing
                .running
                .load(Ordering::SeqCst)
        })
        .await;
        let (text, is_error) = get_message(&mut server, "sess-a-u").await;
        assert!(!is_error, "{text}");
        assert!(text.contains("hello sess-a"), "{text}");
    }

    #[tokio::test]
    async fn an_existing_index_is_searched_while_the_pass_runs() {
        let temp = tempfile::TempDir::new().unwrap();
        let index = temp
            .path()
            .join("idx");
        build_index(&index, vec![write_session(temp.path(), "sess-a")]).unwrap();
        let (release, gate) = mpsc::channel::<()>();
        let mut server = McpServer::start(index, move |_| Ok(gate.recv()?));

        let (text, is_error) = get_message(&mut server, "sess-a-u").await;
        assert!(!is_error, "{text}");
        assert!(text.contains("hello sess-a"), "{text}");
        release
            .send(())
            .unwrap();
    }

    /// A server whose background pass on `index` failed with a two-level chain.
    async fn server_after_failed_pass(index: PathBuf) -> McpServer {
        let server = McpServer::start(index, |_| {
            Err(anyhow::anyhow!("No space left on device").context("committing chunk"))
        });
        let indexing = server
            .indexing
            .clone();
        wait_until(|| {
            !indexing
                .running
                .load(Ordering::SeqCst)
        })
        .await;
        server
    }

    #[tokio::test]
    async fn a_background_failure_without_an_index_is_the_tool_error() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut server = server_after_failed_pass(
            temp.path()
                .join("idx"),
        )
        .await;

        for _ in 0..2 {
            let (text, is_error) = get_message(&mut server, "sess-a-u").await;
            assert!(is_error);
            assert_eq!(
                text,
                "indexing the conversations failed: committing chunk: No space left on device"
            );
        }
    }

    #[tokio::test]
    async fn a_background_failure_is_noted_on_answers_from_the_open_index() {
        let temp = tempfile::TempDir::new().unwrap();
        let index = temp
            .path()
            .join("idx");
        build_index(&index, vec![write_session(temp.path(), "sess-a")]).unwrap();
        let mut server = server_after_failed_pass(index).await;

        for _ in 0..2 {
            let (text, is_error) = get_message(&mut server, "sess-a-u").await;
            assert!(!is_error, "{text}");
            assert!(text.contains("hello sess-a"), "{text}");
            assert!(
                text.ends_with(
                    "\nbackground indexing failed: committing chunk: No space left on device; results may be stale"
                ),
                "{text}"
            );
        }
    }
}
