use super::cache::CacheManager;
use super::config::get_config;
use super::indexer::SearchIndexer;
use super::lock::{ExclusiveIndexAccess, clear_index_dir, is_index_busy};
use super::path_utils::discover_jsonl_files;
use anyhow::Result;
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use std::fs::{self};
use std::path::{Path, PathBuf};
use tantivy::directory::error::OpenReadError;
use tracing::{error, info, warn};

pub fn get_cache_dir() -> Result<PathBuf> {
    get_config().get_cache_dir()
}

/// Tops tantivy's IncompatibleIndex with the user's remedy; tantivy's own text tells a
/// developer to change the dependency. Other errors pass through untouched.
pub fn explain_index_error(err: anyhow::Error, index_path: &Path) -> anyhow::Error {
    let incompatible = err
        .chain()
        .any(|cause| {
            matches!(
                cause.downcast_ref::<OpenReadError>(),
                Some(OpenReadError::IncompatibleIndex(_))
            )
        });
    if !incompatible {
        return err;
    }
    err.context(format!(
        "the search index at {} is in a format claude-conversation-search {} cannot read; \
         `claude-conversation-search cache clear` rebuilds it from the conversation files",
        index_path.display(),
        env!("CARGO_PKG_VERSION")
    ))
}

/// Parse a date as full ISO 8601 or YYYY-MM-DD
pub fn parse_date(s: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Utc.from_utc_datetime(&date.and_time(NaiveTime::MIN)));
    }
    anyhow::bail!("Invalid date '{}': use YYYY-MM-DD or ISO 8601", s)
}

/// Parse an optional date string, passing `None` through unchanged.
pub fn parse_date_opt(s: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    s.map(parse_date)
        .transpose()
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

    let _lock = match ExclusiveIndexAccess::acquire(index_path) {
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
                clear_index_dir(index_path)?;
                SearchIndexer::new(index_path)?
            }
            Err(e) => {
                warn!("Failed to validate index: {e:#}. Rebuilding...");
                clear_index_dir(index_path)?;
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
