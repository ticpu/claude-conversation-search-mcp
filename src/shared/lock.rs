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
    pub fn acquire(index_path: &Path) -> Result<Self> {
        let lock = IndexLock::try_lock(index_path, LockType::Exclusive)?;
        Ok(Self { _lock: lock })
    }

    /// Probe: takes and immediately releases the lock.
    pub fn is_available(index_path: &Path) -> bool {
        IndexLock::try_lock(index_path, LockType::Exclusive).is_ok()
    }
}

/// Shared hold on index.lock.
pub struct SharedIndexAccess {
    _lock: IndexLock,
}

impl SharedIndexAccess {
    pub fn acquire(index_path: &Path) -> Result<Self> {
        let lock = IndexLock::try_lock(index_path, LockType::Shared)?;
        Ok(Self { _lock: lock })
    }

    /// Probe: takes and immediately releases the lock.
    pub fn is_available(index_path: &Path) -> bool {
        IndexLock::try_lock(index_path, LockType::Shared).is_ok()
    }
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
        assert!(!SharedIndexAccess::is_available(&index));

        let other = temp
            .path()
            .join("other");
        assert!(
            ExclusiveIndexAccess::is_available(&other),
            "another index's lock is independent"
        );
        drop(held);
        assert!(ExclusiveIndexAccess::is_available(&index));
    }
}
