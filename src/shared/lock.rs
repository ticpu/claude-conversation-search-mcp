//! Inter-process locks on the search index.
//!
//! Two lock files live in the index directory, their paths derived from the
//! index path so that a test or a non-default index never touches another
//! index's locks (`locking.lock_file` in the config still overrides
//! index.lock's path):
//!
//! - index.lock, the writer lock. Whoever writes the index holds it
//!   exclusively for the duration of the write: an inline auto-index, a
//!   rebuild, `cache clear`, or `index watch` for its whole life. Readers
//!   take no lock; tantivy's meta.json is replaced atomically, so a reader
//!   always opens a committed state.
//! - indexer.lock, the service marker. Only `index watch` holds it, for its
//!   whole life. Others probe it with a shared try-lock to learn that the
//!   service owns indexing, and turn into pure readers.
//!
//! `locking.enabled: false` only turns index.lock into a no-op. indexer.lock
//! always takes a real flock, since a probe that cannot fail would report the
//! service as always absent; `index watch` refuses to start with locking
//! disabled because nothing would then keep a second writer out.
//!
//! Every wipe of the index directory goes through `clear_index_dir`, which
//! keeps both lock files: deleting a held lock file lets the next process
//! create a fresh inode and lock it while the holder still writes.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use tracing::{debug, error, info};

use super::config::get_config;

/// Lock held by another process. Callers downcast to it to tell contention
/// apart from a lock file they could not open at all.
#[derive(Debug)]
pub struct IndexBusy {
    pub lock_type: LockType,
}

impl fmt::Display for IndexBusy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "another process holds the {:?} index lock",
            self.lock_type
        )
    }
}

impl std::error::Error for IndexBusy {}

/// True when the failure is contention rather than an unusable lock file.
pub fn is_index_busy(err: &anyhow::Error) -> bool {
    err.downcast_ref::<IndexBusy>()
        .is_some()
}

#[derive(Debug, Clone, Copy)]
pub enum LockType {
    Shared,
    Exclusive,
}

/// The writer lock of the index at `index_path`, unless the config overrides it.
pub fn index_lock_path(index_path: &Path) -> PathBuf {
    match &get_config()
        .locking
        .lock_file
    {
        Some(path) => path.clone(),
        None => index_path.join("index.lock"),
    }
}

fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(path)
        .with_context(|| format!("opening lock file {}", path.display()))
}

/// Non-blocking flock of `file`; contention comes back as `IndexBusy`.
fn try_flock(file: &File, lock_type: LockType, path: &Path) -> Result<()> {
    let result = match lock_type {
        LockType::Shared => file
            .try_lock_shared()
            .map_err(std::io::Error::from),
        LockType::Exclusive => file.try_lock_exclusive(),
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(IndexBusy { lock_type })
            .with_context(|| format!("acquiring {:?} lock on {}", lock_type, path.display())),
        Err(e) => {
            Err(e).with_context(|| format!("acquiring {:?} lock on {}", lock_type, path.display()))
        }
    }
}

/// A held index.lock. With `locking.enabled: false` the file is opened but never
/// flocked, and `flocked` stays false so Drop does not unlock what it never locked.
pub struct IndexLock {
    file: File,
    lock_type: LockType,
    flocked: bool,
}

impl IndexLock {
    fn try_lock(index_path: &Path, lock_type: LockType) -> Result<Self> {
        let lock_path = index_lock_path(index_path);
        let file = open_lock_file(&lock_path)?;
        let enabled = get_config()
            .locking
            .enabled;
        if enabled {
            debug!(
                "Attempting to acquire {:?} lock on {}",
                lock_type,
                lock_path.display()
            );
            try_flock(&file, lock_type, &lock_path)?;
            info!("Acquired {:?} lock on {}", lock_type, lock_path.display());
        }
        Ok(Self {
            file,
            lock_type,
            flocked: enabled,
        })
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        if !self.flocked {
            return;
        }
        match self
            .file
            .unlock()
        {
            Ok(()) => debug!("Released {:?} lock on index", self.lock_type),
            Err(e) => error!(
                "Failed to release {:?} lock on index: {}",
                self.lock_type, e
            ),
        }
    }
}

/// Exclusive hold on index.lock: the right to write the index.
pub struct ExclusiveIndexAccess {
    _lock: IndexLock,
}

impl ExclusiveIndexAccess {
    /// Contention while the index service runs names the command that stops it.
    pub fn acquire(index_path: &Path) -> Result<Self> {
        match IndexLock::try_lock(index_path, LockType::Exclusive) {
            Ok(lock) => Ok(Self { _lock: lock }),
            Err(e) if is_index_busy(&e) && service_running(index_path) => Err(e.context(format!(
                "the index service owns the index; stop it first with \
                 `systemctl --user stop {SERVICE_UNIT}`"
            ))),
            Err(e) => Err(e),
        }
    }
}

/// The systemd user unit running `index watch`.
pub const SERVICE_UNIT: &str = "claude-conversation-search-index";

/// The service marker of the index at `index_path`. Never overridden by config.
pub fn indexer_lock_path(index_path: &Path) -> PathBuf {
    index_path.join("indexer.lock")
}

/// The held service marker. Always a real flock, whatever `locking.enabled`
/// says; released when the process exits.
#[cfg(target_os = "linux")]
pub struct IndexerLock {
    _file: File,
}

#[cfg(target_os = "linux")]
impl IndexerLock {
    /// Contention comes back as `IndexBusy`.
    pub fn try_acquire(index_path: &Path) -> Result<Self> {
        let path = indexer_lock_path(index_path);
        let file = open_lock_file(&path)?;
        try_flock(&file, LockType::Exclusive, &path)?;
        Ok(Self { _file: file })
    }
}

/// Whether another process holds `path`, probed with a shared try-lock released
/// at once. A missing file is not held and is not created.
fn probe_held(path: &Path) -> Result<bool> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("opening lock file {}", path.display())),
    };
    match try_flock(&file, LockType::Shared, path) {
        Ok(()) => Ok(false),
        Err(e) if is_index_busy(&e) => Ok(true),
        Err(e) => Err(e),
    }
}

/// Whether `index watch` holds the index at `index_path`.
pub fn indexer_lock_held(index_path: &Path) -> Result<bool> {
    probe_held(&indexer_lock_path(index_path))
}

/// Whether a writer holds index.lock right now. Always false with locking disabled.
pub fn index_lock_held(index_path: &Path) -> Result<bool> {
    probe_held(&index_lock_path(index_path))
}

/// `indexer_lock_held` for callers that only branch on it: a failing probe is
/// logged with its path and reads as "not running".
pub fn service_running(index_path: &Path) -> bool {
    indexer_lock_held(index_path).unwrap_or_else(|e| {
        error!("Treating the index service as not running: {e:#}");
        false
    })
}

/// Empty the index directory, keeping both lock files: a holder keeps its lock
/// only while the file it locked is the one others open.
pub fn clear_index_dir(index_path: &Path) -> Result<()> {
    let keep = [index_lock_path(index_path), indexer_lock_path(index_path)];
    let entries = std::fs::read_dir(index_path)
        .with_context(|| format!("reading index directory {}", index_path.display()))?;
    for entry in entries {
        let path = entry
            .with_context(|| format!("listing index directory {}", index_path.display()))?
            .path();
        if keep.contains(&path) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn lock_lives_in_the_index_dir_and_excludes_a_second_writer() {
        let temp = TempDir::new().unwrap();
        let index = temp
            .path()
            .join("idx");
        let held = ExclusiveIndexAccess::acquire(&index).unwrap();
        assert!(
            index
                .join("index.lock")
                .exists()
        );
        let second = ExclusiveIndexAccess::acquire(&index);
        assert!(is_index_busy(
            &second
                .err()
                .unwrap()
        ));
        assert!(index_lock_held(&index).unwrap());

        let other = temp
            .path()
            .join("other");
        assert!(
            ExclusiveIndexAccess::acquire(&other).is_ok(),
            "another index's lock is independent"
        );
        drop(held);
        assert!(!index_lock_held(&index).unwrap());
    }

    #[test]
    fn wipe_keeps_lock_files_and_the_held_lock_still_excludes() {
        let temp = TempDir::new().unwrap();
        let index = temp
            .path()
            .join("idx");
        let held = ExclusiveIndexAccess::acquire(&index).unwrap();
        let marker = File::create(indexer_lock_path(&index)).unwrap();
        marker
            .try_lock_exclusive()
            .unwrap();
        std::fs::write(index.join("meta.json"), "{}").unwrap();
        std::fs::create_dir(index.join("segdir")).unwrap();

        clear_index_dir(&index).unwrap();

        let mut left: Vec<_> = std::fs::read_dir(&index)
            .unwrap()
            .map(|e| {
                e.unwrap()
                    .file_name()
            })
            .collect();
        left.sort();
        assert_eq!(left, ["index.lock", "indexer.lock"]);
        let second = ExclusiveIndexAccess::acquire(&index)
            .err()
            .unwrap();
        assert!(is_index_busy(&second));
        assert!(
            format!("{second:#}").contains("systemctl --user stop"),
            "{second:#}"
        );
        assert!(indexer_lock_held(&index).unwrap());
        drop(held);
    }
}
