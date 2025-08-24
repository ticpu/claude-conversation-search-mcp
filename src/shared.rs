use crate::cache::CacheManager;
use crate::indexer::SearchIndexer;
use anyhow::Result;
use dirs::home_dir;
use glob::glob;
use std::path::{Path, PathBuf};
use tracing::info;

pub fn get_claude_dir() -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

    let claude_dir = home.join(".claude");
    if claude_dir.exists() {
        return Ok(claude_dir);
    }

    let config_claude_dir = home.join(".config").join("claude");
    if config_claude_dir.exists() {
        return Ok(config_claude_dir);
    }

    Ok(claude_dir)
}

pub fn get_cache_dir() -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let cache_dir = home.join(".cache").join("claude-search");
    Ok(cache_dir)
}

pub async fn auto_index(index_path: &Path) -> Result<()> {
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