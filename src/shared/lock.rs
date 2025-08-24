use anyhow::{Result, anyhow};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use tracing::{debug, info};

use super::config::get_config;

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

        if !config.locking.enabled {
            // Create a dummy lock file for consistency
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
            LockType::Shared => file.try_lock_shared().map_err(std::io::Error::from),
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
            Err(e) => Err(anyhow!(
                "Could not acquire {:?} lock on index: {}. Another instance may be running.",
                lock_type,
                e
            )),
        }
    }

    /// Check if we can acquire a lock without actually acquiring it
    pub fn can_lock(lock_type: LockType) -> bool {
        Self::try_lock(lock_type).is_ok()
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        if get_config().locking.enabled {
            let _ = self._file.unlock();
            debug!("Released {:?} lock on index", self.lock_type);
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
