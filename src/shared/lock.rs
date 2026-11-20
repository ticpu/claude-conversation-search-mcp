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

/// Index lock for coordinating access between multiple processes
pub struct IndexLock {
    _file: File,
    lock_type: LockType,
}

#[derive(Debug, Clone, Copy)]
pub enum LockType {
    Shared,    // Multiple readers allowed
    Exclusive, // Single writer only
}

impl IndexLock {
    /// Acquire a shared (read) lock on the index
    pub fn try_shared() -> Result<Self> {
        Self::try_lock(LockType::Shared)
    }

    /// Acquire an exclusive (write) lock on the index
    pub fn try_exclusive() -> Result<Self> {
        Self::try_lock(LockType::Exclusive)
    }

    fn try_lock(lock_type: LockType) -> Result<Self> {
        let config = get_config();

        if !config
            .locking
            .enabled
        {
            let lock_path = config.get_lock_file_path()?;
            if let Some(parent) = lock_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .read(true)
                .open(&lock_path)?;

            return Ok(Self {
                _file: file,
                lock_type,
            });
        }

        let lock_path = config.get_lock_file_path()?;

        // Ensure parent directory exists
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        debug!(
            "Attempting to acquire {:?} lock on {}",
            lock_type,
            lock_path.display()
        );

        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&lock_path)?;

        let result = match lock_type {
            LockType::Shared => file
                .try_lock_shared()
                .map_err(std::io::Error::from),
            LockType::Exclusive => file.try_lock_exclusive(),
        };

        match result {
            Ok(()) => {
                info!("Acquired {:?} lock on index", lock_type);
                Ok(Self {
                    _file: file,
                    lock_type,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(IndexBusy { lock_type })
                .with_context(|| {
                    format!("acquiring {:?} lock on {}", lock_type, lock_path.display())
                }),
            Err(e) => Err(e).with_context(|| {
                format!("acquiring {:?} lock on {}", lock_type, lock_path.display())
            }),
        }
    }

    /// Check if we can acquire a lock without actually acquiring it
    pub fn can_lock(lock_type: LockType) -> bool {
        Self::try_lock(lock_type).is_ok()
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        if get_config()
            .locking
            .enabled
        {
            match self
                ._file
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
}

/// RAII wrapper that ensures exclusive access during index operations
pub struct ExclusiveIndexAccess {
    _lock: IndexLock,
}

impl ExclusiveIndexAccess {
    pub fn acquire() -> Result<Self> {
        let lock = IndexLock::try_exclusive()?;
        Ok(Self { _lock: lock })
    }

    /// Check if we can get exclusive access without acquiring it
    pub fn is_available() -> bool {
        IndexLock::can_lock(LockType::Exclusive)
    }
}

/// RAII wrapper that ensures shared access during read operations
pub struct SharedIndexAccess {
    _lock: IndexLock,
}

impl SharedIndexAccess {
    pub fn acquire() -> Result<Self> {
        let lock = IndexLock::try_shared()?;
        Ok(Self { _lock: lock })
    }

    /// Check if we can get shared access without acquiring it
    pub fn is_available() -> bool {
        IndexLock::can_lock(LockType::Shared)
    }
}
