use super::cache::CacheManager;
use super::config::get_config;
use super::indexer::SearchIndexer;
use super::lock::{ExclusiveIndexAccess, is_index_busy};
use super::path_utils::discover_jsonl_files;
use anyhow::Result;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use std::fs::{self};
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

pub fn get_cache_dir() -> Result<PathBuf> {
    get_config().get_cache_dir()
}

/// Parse a date as full ISO 8601 or YYYY-MM-DD
pub fn parse_date(s: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Utc.from_utc_datetime(
            &date
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        ));
    }
    anyhow::bail!("Invalid date '{}': use YYYY-MM-DD or ISO 8601", s)
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
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        s.to_string()
    };

    if processed
        .chars()
        .count()
        <= max_chars
    {
        processed
    } else {
        let truncated: String = processed
            .chars()
            .take(max_chars - 1)
            .collect();
        format!("{}…", truncated)
    }
}

pub fn auto_index(index_path: &Path) -> Result<()> {
    let config = get_config();

    if !config
        .index
        .auto_index_on_startup
    {
        return Ok(());
    }

    let _lock = match ExclusiveIndexAccess::acquire() {
        Ok(lock) => lock,
        Err(e) if is_index_busy(&e) => {
            info!("Skipping auto-index: another process is currently indexing");
            return Ok(());
        }
        Err(e) => {
            error!("Skipping auto-index, the index lock is unusable: {e:#}");
            return Ok(());
        }
    };

    // Build/open the indexer first — a schema-mismatch rebuild wipes index_path,
    // which also deletes cache-metadata.json (it lives in the same dir). Creating
    // CacheManager after guarantees a fresh empty metadata when a rebuild occurs,
    // so the subsequent update_incremental re-indexes everything instead of skipping.
    let mut indexer = if index_path
        .join("meta.json")
        .exists()
    {
        match SearchIndexer::validate_schema(index_path) {
            Ok(true) => SearchIndexer::open(index_path)?,
            Ok(false) => {
                info!("Index schema mismatch detected. Rebuilding index...");
                if let Err(rm_err) = std::fs::remove_dir_all(index_path) {
                    warn!("Failed to remove old index: {}", rm_err);
                }
                SearchIndexer::new(index_path)?
            }
            Err(e) => {
                warn!("Failed to validate index: {}. Rebuilding...", e);
                if let Err(rm_err) = std::fs::remove_dir_all(index_path) {
                    warn!("Failed to remove corrupted index: {}", rm_err);
                }
                SearchIndexer::new(index_path)?
            }
        }
    } else {
        info!("No index found, creating new one...");
        SearchIndexer::new(index_path)?
    };

    let mut cache_manager = CacheManager::new(index_path)?;
    let all_files = discover_jsonl_files()?;
    cache_manager.update_incremental(&mut indexer, all_files)?;
    Ok(())
}
