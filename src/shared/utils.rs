use super::cache::CacheManager;
use super::config::get_config;
use super::indexer::SearchIndexer;
use super::lock::ExclusiveIndexAccess;
use anyhow::Result;
use chrono::{DateTime, Utc};
use glob::glob;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

pub fn get_claude_dir() -> Result<PathBuf> {
    get_config().get_claude_dir()
}

pub fn get_cache_dir() -> Result<PathBuf> {
    get_config().get_cache_dir()
}

/// Discover all JSONL files in Claude projects directory
pub fn discover_jsonl_files() -> Result<Vec<PathBuf>> {
    let claude_dir = get_claude_dir()?;
    let pattern = claude_dir.join("projects/**/*.jsonl");
    let files: Vec<PathBuf> = glob(&pattern.to_string_lossy())?.flatten().collect();
    Ok(files)
}

/// Get file modification time as DateTime<Utc>
pub fn file_mtime(path: &Path) -> Result<DateTime<Utc>> {
    let metadata = fs::metadata(path)?;
    let mtime = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    Ok(DateTime::from_timestamp(mtime, 0).unwrap_or_else(Utc::now))
}

/// Truncate string at UTF-8 character boundary, optionally collapsing whitespace
pub fn truncate_content(s: &str, max_chars: usize, collapse_whitespace: bool) -> String {
    let processed = if collapse_whitespace {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        s.to_string()
    };

    if processed.chars().count() <= max_chars {
        processed
    } else {
        let truncated: String = processed.chars().take(max_chars - 1).collect();
        format!("{}…", truncated)
    }
}

/// Count lines in a JSONL file (returns None if file doesn't exist or can't be read)
pub fn count_jsonl_lines(path: &Path) -> Option<usize> {
    let file = File::open(path).ok()?;
    Some(BufReader::new(file).lines().count())
}

pub fn auto_index(index_path: &Path) -> Result<()> {
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
        // Check if existing index has correct schema
        match SearchIndexer::validate_schema(index_path) {
            Ok(true) => {
                // Schema is valid, open existing index
                SearchIndexer::open(index_path)?
            }
            Ok(false) => {
                // Schema mismatch, rebuild
                info!("Index schema mismatch detected. Rebuilding index...");

                // Remove the old index
                if let Err(rm_err) = std::fs::remove_dir_all(index_path) {
                    warn!("Failed to remove old index: {}", rm_err);
                }

                // Create new index
                SearchIndexer::new(index_path)?
            }
            Err(e) => {
                // Failed to validate (corrupted index), rebuild
                warn!("Failed to validate index: {}. Rebuilding...", e);

                // Remove the corrupted index
                if let Err(rm_err) = std::fs::remove_dir_all(index_path) {
                    warn!("Failed to remove corrupted index: {}", rm_err);
                }

                // Create new index
                SearchIndexer::new(index_path)?
            }
        }
    } else {
        info!("No index found, creating new one...");
        SearchIndexer::new(index_path)?
    };

    let all_files = discover_jsonl_files()?;
    cache_manager.update_incremental(&mut indexer, all_files)?;
    Ok(())
}
