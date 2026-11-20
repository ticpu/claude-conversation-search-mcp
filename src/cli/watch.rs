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
//! may already claim the files that failed) or a watcher error such as an
//! exhausted inotify watch limit. The loop's return type is Infallible on
//! success: on-failure ignores exit 0, so a loop that could return Ok would
//! leave the index unwatched without a restart.
//!
//! The binary at /proc/self/exe being replaced or deleted, as an upgrade
//! does, is not a failure: it exits with `EXIT_BINARY_REPLACED`, which the
//! unit lists in both SuccessExitStatus and RestartForceExitStatus, so
//! systemd restarts it on the new binary without recording a failure.

use anyhow::{Context, Result, bail};
use notify::event::{EventKind, ModifyKind};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::convert::Infallible;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};
use tantivy::merge_policy::LogMergePolicy;
use tracing::debug;

use crate::shared::{
    CacheManager, ExclusiveIndexAccess, IndexerLock, SearchIndexer, WriterThreads,
    discover_jsonl_files_under, get_config, is_index_busy, open_or_create_indexer,
};

/// Deleted documents a segment may carry before a merge rewrites it. Every
/// full reindex of a transcript deletes all of its earlier documents.
const DEL_DOCS_RATIO_BEFORE_MERGE: f32 = 0.3;

/// Indexing threads of the service's writer. A flush carries a few changed
/// files, and each thread holding documents at commit writes its own segment.
const INDEXING_THREADS: usize = 2;

/// Backoff between attempts to take the locks at startup, about 5 s in total.
const LOCK_RETRY_DELAYS_MS: [u64; 6] = [100, 200, 400, 800, 1600, 2000];

/// The batching and polling periods.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Flush once no event arrived for this long.
    pub quiet: Duration,
    /// Flush once the oldest pending change is this old, even amid events.
    pub max_delay: Duration,
    /// Period of the safety-net full pass.
    pub full_pass_every: Duration,
    /// Longest wait for an event before the timers and the binary are checked.
    pub tick: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            quiet: Duration::from_secs(2),
            max_delay: Duration::from_secs(30),
            full_pass_every: Duration::from_secs(3600),
            tick: Duration::from_secs(1),
        }
    }
}

/// Exit status after `BinaryReplaced` (EX_TEMPFAIL), named in the unit file.
pub const EXIT_BINARY_REPLACED: i32 = 75;

/// Run the service on `projects` and the index at `index_path` until a
/// failure, ending the process with `EXIT_BINARY_REPLACED` on `BinaryReplaced`.
pub fn run(projects: &Path, index_path: &Path) -> Result<Infallible> {
    let mut watch = IndexWatch::start(projects, index_path, Timing::default())?;
    loop {
        if let Err(err) = watch.step() {
            if err
                .downcast_ref::<BinaryReplaced>()
                .is_some()
            {
                eprintln!("{err:#}");
                std::process::exit(EXIT_BINARY_REPLACED);
            }
            return Err(err);
        }
    }
}

/// The running binary, identified by device and inode at startup.
struct RunningExe {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl RunningExe {
    fn current() -> Result<Self> {
        let path = std::fs::read_link("/proc/self/exe").context("reading /proc/self/exe")?;
        let meta = std::fs::metadata("/proc/self/exe").context("reading /proc/self/exe")?;
        Ok(Self {
            path,
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    /// Fails once the path no longer names the binary this process runs.
    fn check(&self) -> Result<(), BinaryReplaced> {
        let gone = match std::fs::metadata(&self.path) {
            Ok(meta) if meta.dev() == self.dev && meta.ino() == self.ino => return Ok(()),
            Ok(_) => None,
            Err(e) => Some(e),
        };
        Err(BinaryReplaced {
            path: self
                .path
                .clone(),
            gone,
        })
    }
}

/// The binary this process runs was replaced or deleted, typically by an upgrade.
#[derive(Debug)]
pub struct BinaryReplaced {
    path: PathBuf,
    /// Why the path could not be read, when it is gone rather than replaced.
    gone: Option<std::io::Error>,
}

impl std::fmt::Display for BinaryReplaced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = self
            .path
            .display();
        match self.gone {
            None => write!(
                f,
                "{path} was replaced; exiting so the service restarts on the new binary"
            ),
            Some(_) => write!(
                f,
                "{path} is gone; exiting so the service restarts on its replacement"
            ),
        }
    }
}

impl std::error::Error for BinaryReplaced {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.gone
            .as_ref()
            .map(|e| e as _)
    }
}

pub struct IndexWatch {
    projects: PathBuf,
    indexer: SearchIndexer,
    cache: CacheManager,
    events: Receiver<notify::Result<Event>>,
    timing: Timing,
    exe: RunningExe,
    pending: HashSet<PathBuf>,
    oldest_pending: Option<Instant>,
    last_event: Instant,
    full_pass_requested: bool,
    last_full_pass: Instant,
    // Dropped last: the locks outlive everything that writes under them.
    _watcher: RecommendedWatcher,
    _indexer_lock: IndexerLock,
    _index_lock: ExclusiveIndexAccess,
}

/// The batching policy: flush after `quiet` without an event, or once the
/// oldest pending change waited `max_delay`.
fn flush_due(
    timing: &Timing,
    last_event: Instant,
    oldest_pending: Option<Instant>,
    now: Instant,
) -> bool {
    oldest_pending.is_some_and(|oldest| {
        now.duration_since(last_event) >= timing.quiet
            || now.duration_since(oldest) >= timing.max_delay
    })
}

/// Take a lock, retrying on contention only, with `LOCK_RETRY_DELAYS_MS` backoff.
fn acquire_with_retry<T>(mut acquire: impl FnMut() -> Result<T>) -> Result<T> {
    for delay in LOCK_RETRY_DELAYS_MS {
        match acquire() {
            Err(e) if is_index_busy(&e) => std::thread::sleep(Duration::from_millis(delay)),
            other => return other,
        }
    }
    acquire()
}

impl IndexWatch {
    /// Take the locks, open the index, start watching, signal readiness, then
    /// bring the index up to date with a full pass.
    pub fn start(projects: &Path, index_path: &Path, timing: Timing) -> Result<Self> {
        if !get_config()
            .locking
            .enabled
        {
            bail!(
                "index watch refuses to run with locking.enabled: false, nothing would keep a second writer out"
            );
        }
        let index_lock = acquire_with_retry(|| ExclusiveIndexAccess::acquire(index_path))?;
        let indexer_lock = acquire_with_retry(|| IndexerLock::try_acquire(index_path))?;
        let exe = RunningExe::current()?;

        let (indexer, cache) =
            open_or_create_indexer(index_path, WriterThreads::Fixed(INDEXING_THREADS))?;
        let mut policy = LogMergePolicy::default();
        policy.set_del_docs_ratio_before_merge(DEL_DOCS_RATIO_BEFORE_MERGE);
        indexer.set_merge_policy(Box::new(policy));

        std::fs::create_dir_all(projects)
            .with_context(|| format!("creating {}", projects.display()))?;
        let (tx, events) = channel();
        let mut watcher = notify::recommended_watcher(tx).context("creating the file watcher")?;
        watcher
            .watch(projects, RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", projects.display()))?;
        sd_notify::notify(&[sd_notify::NotifyState::Ready]).context("notifying systemd")?;
        eprintln!(
            "watching {} for the index at {}",
            projects.display(),
            index_path.display()
        );

        let now = Instant::now();
        let mut watch = Self {
            projects: projects.to_path_buf(),
            indexer,
            cache,
            events,
            timing,
            exe,
            pending: HashSet::new(),
            oldest_pending: None,
            last_event: now,
            full_pass_requested: false,
            last_full_pass: now,
            _watcher: watcher,
            _indexer_lock: indexer_lock,
            _index_lock: index_lock,
        };
        watch.full_pass()?;
        // Merges are only considered on commit, and a pass with nothing to do
        // does not commit: this one lets an old backlog start merging.
        watch
            .indexer
            .commit()?;
        if let Some(notice) = watch
            .indexer
            .merge_notice()?
        {
            eprintln!("{notice}");
        }
        Ok(watch)
    }

    /// Wait up to one tick for events, then flush or run a full pass if due.
    pub fn step(&mut self) -> Result<()> {
        self.exe
            .check()?;
        match self
            .events
            .recv_timeout(
                self.timing
                    .tick,
            ) {
            Ok(event) => self.handle_event(event)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("the file watcher stopped"),
        }
        while let Ok(event) = self
            .events
            .try_recv()
        {
            self.handle_event(event)?;
        }

        let now = Instant::now();
        if self.full_pass_requested
            || now.duration_since(self.last_full_pass)
                >= self
                    .timing
                    .full_pass_every
        {
            return self.full_pass();
        }
        if flush_due(&self.timing, self.last_event, self.oldest_pending, now) {
            self.flush()?;
        }
        Ok(())
    }

    fn handle_event(&mut self, event: notify::Result<Event>) -> Result<()> {
        let event = match event {
            Ok(event) => event,
            Err(e) if matches!(e.kind, notify::ErrorKind::MaxFilesWatch) => {
                return Err(e).context(
                    "the inotify watch limit is exhausted (fs.inotify.max_user_watches); \
                     changes are no longer seen",
                );
            }
            Err(e) => return Err(e).context("the file watcher failed"),
        };
        if event.need_rescan() {
            debug!("watcher asked for a rescan");
            self.full_pass_requested = true;
            return Ok(());
        }
        if matches!(event.kind, EventKind::Access(_)) {
            return Ok(());
        }
        let structural = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        );
        for path in event.paths {
            if path.extension() == Some("jsonl".as_ref()) {
                self.queue([path]);
            } else if structural && path.is_dir() {
                // Files already inside a directory moved or created here fire nothing.
                let found = discover_jsonl_files_under(&path)?;
                self.queue(found);
            } else if structural {
                let known = self
                    .cache
                    .indexed_paths_under(&path);
                self.queue(known);
            }
        }
        Ok(())
    }

    /// Add `paths` to the pending set; any path, even one already pending,
    /// counts as an event for the quiet period.
    fn queue(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut queued = false;
        for path in paths {
            self.pending
                .insert(path);
            queued = true;
        }
        if queued {
            let now = Instant::now();
            self.last_event = now;
            self.oldest_pending
                .get_or_insert(now);
        }
    }

    fn flush(&mut self) -> Result<()> {
        let files: Vec<PathBuf> = self
            .pending
            .drain()
            .collect();
        self.oldest_pending = None;
        debug!("flushing {} changed files", files.len());
        self.cache
            .update_incremental(&mut self.indexer, files)
            .context("indexing changed transcripts")?;
        Ok(())
    }

    fn full_pass(&mut self) -> Result<()> {
        self.pending
            .clear();
        self.oldest_pending = None;
        self.full_pass_requested = false;
        self.last_full_pass = Instant::now();
        let discovered = discover_jsonl_files_under(&self.projects)?;
        let summary = self
            .cache
            .update_full(&mut self.indexer, discovered, &mut |_| Ok(()))
            .context("full pass over the transcripts")?;
        if summary.commits > 0 {
            eprintln!("full pass: {summary}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::Flag;
    use std::io::Write;
    use tempfile::TempDir;

    const FAST: Timing = Timing {
        quiet: Duration::from_millis(100),
        max_delay: Duration::from_secs(1),
        full_pass_every: Duration::from_secs(3600),
        tick: Duration::from_millis(50),
    };

    struct Env {
        temp: TempDir,
        projects: PathBuf,
        watch: IndexWatch,
    }

    fn env() -> Env {
        let temp = TempDir::new().unwrap();
        let projects = temp
            .path()
            .join("projects");
        let watch = IndexWatch::start(
            &projects,
            &temp
                .path()
                .join("idx"),
            FAST,
        )
        .unwrap();
        Env {
            temp,
            projects,
            watch,
        }
    }

    fn append_line(path: &Path, i: usize) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(
            file,
            r#"{{"uuid":"u-{i}","sessionId":"sess","type":"user","timestamp":"2025-12-28T10:00:00Z","message":{{"role":"user","content":"line {i}"}}}}"#
        )
        .unwrap();
    }

    /// Step until `path` has `expected` entries indexed, for at most 10 s.
    fn step_until(watch: &mut IndexWatch, path: &Path, expected: Option<usize>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while watch
            .cache
            .indexed_entries(path)
            != expected
        {
            assert!(
                Instant::now() < deadline,
                "{} still at {:?}, expected {:?}",
                path.display(),
                watch
                    .cache
                    .indexed_entries(path),
                expected
            );
            watch
                .step()
                .unwrap();
        }
    }

    #[test]
    fn follows_create_append_and_delete() {
        let mut env = env();
        let project = env
            .projects
            .join("p");
        std::fs::create_dir(&project).unwrap();
        let transcript = project.join("s.jsonl");

        append_line(&transcript, 0);
        step_until(&mut env.watch, &transcript, Some(1));
        append_line(&transcript, 1);
        step_until(&mut env.watch, &transcript, Some(2));
        std::fs::remove_file(&transcript).unwrap();
        step_until(&mut env.watch, &transcript, None);
    }

    #[test]
    fn one_flush_writes_at_most_one_segment_per_indexing_thread() {
        let mut env = env();
        env.watch
            .indexer
            .set_merge_policy(Box::new(tantivy::indexer::NoMergePolicy));
        let before = env
            .watch
            .indexer
            .searchable_segments()
            .unwrap();
        let transcript = env
            .projects
            .join("s.jsonl");
        let lines: String = (0..2000)
            .map(|i| {
                format!(
                    r#"{{"uuid":"u-{i}","sessionId":"sess","type":"user","timestamp":"2025-12-28T10:00:00Z","message":{{"role":"user","content":"line {i}"}}}}"#
                ) + "\n"
            })
            .collect();
        std::fs::write(&transcript, lines).unwrap();
        step_until(&mut env.watch, &transcript, Some(2000));

        let written = env
            .watch
            .indexer
            .searchable_segments()
            .unwrap()
            - before;
        assert!(written <= INDEXING_THREADS, "{written} segments");
    }

    #[test]
    fn finds_files_inside_a_directory_moved_in() {
        let mut env = env();
        let outside = env
            .temp
            .path()
            .join("outside");
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        append_line(&outside.join("sub/s.jsonl"), 0);

        let moved = env
            .projects
            .join("moved");
        std::fs::rename(&outside, &moved).unwrap();
        step_until(&mut env.watch, &moved.join("sub/s.jsonl"), Some(1));

        let gone = env
            .temp
            .path()
            .join("gone");
        std::fs::rename(&moved, &gone).unwrap();
        step_until(&mut env.watch, &moved.join("sub/s.jsonl"), None);
    }

    #[test]
    fn rescan_recovers_changes_whose_events_were_lost() {
        let mut env = env();
        let transcript = env
            .projects
            .join("s.jsonl");
        append_line(&transcript, 0);
        std::thread::sleep(Duration::from_millis(300));
        while env
            .watch
            .events
            .try_recv()
            .is_ok()
        {}
        for _ in 0..5 {
            env.watch
                .step()
                .unwrap();
        }
        assert_eq!(
            env.watch
                .cache
                .indexed_entries(&transcript),
            None,
            "control: with its events dropped the file stays unseen"
        );

        env.watch
            .handle_event(Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)))
            .unwrap();
        env.watch
            .step()
            .unwrap();
        assert_eq!(
            env.watch
                .cache
                .indexed_entries(&transcript),
            Some(1)
        );
    }

    #[test]
    fn flush_waits_for_quiet_or_max_delay() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        assert!(!flush_due(&FAST, t0, None, at(5000)), "nothing pending");
        assert!(!flush_due(&FAST, at(0), Some(at(0)), at(50)), "still busy");
        assert!(
            flush_due(&FAST, at(0), Some(at(0)), at(100)),
            "quiet period over"
        );
        assert!(
            flush_due(&FAST, at(950), Some(at(0)), at(1000)),
            "events keep coming, the oldest waited max_delay"
        );
    }

    #[test]
    fn lock_retry_rides_out_a_short_contention() {
        let mut attempts = 0;
        let held = acquire_with_retry(|| {
            attempts += 1;
            if attempts < 3 {
                Err(anyhow::Error::new(crate::shared::IndexBusy {
                    lock_type: crate::shared::LockType::Exclusive,
                }))
            } else {
                Ok(attempts)
            }
        })
        .unwrap();
        assert_eq!(held, 3);

        let mut attempts = 0;
        let err = acquire_with_retry(|| -> Result<()> {
            attempts += 1;
            bail!("unusable lock file")
        })
        .unwrap_err();
        assert_eq!(attempts, 1, "only contention is retried: {err}");
    }

    #[test]
    fn a_second_service_cannot_start_on_the_same_index() {
        let env = env();
        let err = IndexWatch::start(
            &env.projects,
            &env.temp
                .path()
                .join("idx"),
            FAST,
        )
        .err()
        .unwrap();
        assert!(is_index_busy(&err), "{err:#}");
    }
}
