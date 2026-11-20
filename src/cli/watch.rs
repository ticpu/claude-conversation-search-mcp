//! `index watch`: the long-lived sole writer of the search index.
//!
//! Tantivy only lands a merge while the IndexWriter that scheduled it is
//! alive; dropping the writer kills its segment updater. A writer opened per
//! command therefore never merges, and segments pile up until every search
//! pays for them. This loop keeps one writer open for the life of the process
//! so merges complete in the background.
//!
//! It holds index.lock (exclusive) and indexer.lock for its whole life. Other
//! processes see indexer.lock held and act as readers: the CLI skips its
//! inline auto-index, the MCP server hides its reindex tool. Both locks are
//! retried with backoff for a few seconds at startup, so a restart racing a
//! short-lived inline writer does not fail. It refuses to run with
//! `locking.enabled: false`, since nothing would then keep a second writer out.
//!
//! Batching: the active session's transcript is appended on every turn, so
//! flushing per event would commit several times a minute. Changed paths
//! collect in a pending set, flushed 2 s after the last event or 30 s after
//! the oldest pending one, whichever comes first.
//!
//! A new directory reports no events for files already inside it, so a
//! directory creation or rename queues a scan of its `**/*.jsonl`. A watcher
//! rescan request triggers a full pass, as does an hourly timer, as a safety
//! net for events the kernel dropped. A full pass covers the discovered files
//! plus every file the metadata knows, so deleted transcripts are purged.
//!
//! There is no shutdown handling: SIGTERM kills the process mid-anything.
//! That is safe because tantivy writes meta.json by temp file, fsync and
//! rename, and cache-metadata.json is written the same way; a kill loses at
//! most the uncommitted chunk, which the next startup pass redoes.
//!
//! Every failure ends the process with a non-zero status so systemd's
//! Restart=on-failure starts it clean: a failed flush (the in-memory metadata
//! may already claim the files that failed), a watcher error such as an
//! exhausted inotify watch limit, or the binary at /proc/self/exe being
//! replaced or deleted by an upgrade. The loop's return type is Infallible on
//! success: on-failure ignores exit 0, so a loop that could return Ok would
//! leave the index unwatched without a restart.
