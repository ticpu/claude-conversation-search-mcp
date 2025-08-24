use super::cache::CacheManager;
use super::config::get_config;
use super::indexer::SearchIndexer;
use super::lock::ExclusiveIndexAccess;
use anyhow::Result;
use glob::glob;
use std::path::{Path, PathBuf};
use tracing::info;

pub fn get_claude_dir() -> Result<PathBuf> {
    get_config().get_claude_dir()
}

pub fn get_cache_dir() -> Result<PathBuf> {
    get_config().get_cache_dir()
}

pub async fn auto_index(index_path: &Path) -> Result<()> {
    let config = get_config();

    // Skip auto-indexing if disabled in config
    if !config.index.auto_index_on_startup {
        return Ok(());
    }

    // Try to acquire exclusive lock for indexing
    let _lock = match ExclusiveIndexAccess::acquire() {
        Ok(lock) => lock,
        Err(_) => {
            // Another process is already indexing, skip
            info!("Skipping auto-index: another process is currently indexing");
            return Ok(());
        }
    };

    let mut cache_manager = CacheManager::new(index_path)?;

    let mut indexer = if index_path.join("meta.json").exists() {
        SearchIndexer::open(index_path)?
    } else {
        info!("No index found, creating new one...");
        SearchIndexer::new(index_path)?
    };

    let claude_dir = get_claude_dir()?;
    let pattern = claude_dir.join("projects/**/*.jsonl");
    let pattern_str = pattern.to_string_lossy();

    let mut all_files = Vec::new();
    // Silently skip errors during auto-indexing
    for path in glob(&pattern_str)?.flatten() {
        all_files.push(path);
    }

    cache_manager.update_incremental(&mut indexer, all_files)?;
    Ok(())
}
