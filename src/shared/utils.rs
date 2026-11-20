use super::cache::{CacheManager, IndexProgress};
use super::config::get_config;
use super::indexer::SearchIndexer;
use super::lock::{ExclusiveIndexAccess, clear_index_dir, is_index_busy};
use super::path_utils::discover_jsonl_files;
use anyhow::Result;
use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone, Utc};
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

/// Parse a date as full ISO 8601 or YYYY-MM-DD. A date-only value means local
/// midnight (TZ honoured); `--before` stays exclusive of that day.
pub fn parse_date(s: &str) -> Result<DateTime<Utc>> {
    parse_date_in(s, &Local)
}

/// `parse_date` with the zone a date-only value is read in.
fn parse_date_in<Tz: TimeZone>(s: &str, tz: &Tz) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let midnight = tz
            .from_local_datetime(&date.and_time(NaiveTime::MIN))
            .earliest()
            .ok_or_else(|| {
                anyhow::anyhow!("midnight of {s} does not exist in the local timezone")
            })?;
        return Ok(midnight.with_timezone(&Utc));
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

/// Whether an inline indexer waits for the merges its commits scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merges {
    /// Wait, so they land: the writer is about to be dropped, which kills them.
    Wait,
    /// Drop the writer at once, abandoning them, to answer sooner.
    Abandon,
}

/// Print the merge notice to stderr, then close the writer per `merges`.
pub fn finish_writer(indexer: SearchIndexer, merges: Merges) -> Result<()> {
    if merges == Merges::Abandon {
        return Ok(());
    }
    if let Some(notice) = indexer.merge_notice()? {
        eprintln!("{notice}");
    }
    indexer.wait_merging()
}

/// Open the index at `index_path` for writing, creating it when absent and
/// recreating it when its schema is outdated or unreadable. The caller must
/// hold index.lock. The metadata is loaded after any wipe, which deleted it,
/// so a recreated index is fully reindexed instead of skipped as unchanged.
pub fn open_or_create_indexer(index_path: &Path) -> Result<(SearchIndexer, CacheManager)> {
    let indexer = if index_path
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
    let cache = CacheManager::new(index_path)?;
    Ok((indexer, cache))
}

pub fn auto_index(
    index_path: &Path,
    merges: Merges,
    on_progress: &mut dyn FnMut(IndexProgress) -> Result<()>,
) -> Result<()> {
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

    let (mut indexer, mut cache_manager) = open_or_create_indexer(index_path)?;
    let all_files = discover_jsonl_files()?;
    cache_manager.update_full(&mut indexer, all_files, on_progress)?;
    finish_writer(indexer, merges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::Index;
    use tantivy::indexer::NoMergePolicy;
    use tempfile::TempDir;

    fn write_one(dir: &Path, i: usize) -> PathBuf {
        let path = dir.join(format!("s{i}.jsonl"));
        std::fs::write(
            &path,
            format!(
                r#"{{"uuid":"u{i}","sessionId":"s{i}","type":"user","timestamp":"2025-12-28T10:00:00Z","message":{{"role":"user","content":"m {i}"}}}}"#
            ) + "\n",
        )
        .unwrap();
        path
    }

    /// A backlog of twenty one-file segments, as left by writers dropped before
    /// their merges landed, then one inline update by a writer with the given
    /// policy, closed per `merges`. Returns the segments a fresh reader sees.
    fn segments_after_inline_update(no_merge: bool, merges: Merges) -> usize {
        let temp = TempDir::new().unwrap();
        let index_dir = temp
            .path()
            .join("idx");
        let mut cache = CacheManager::new(&index_dir).unwrap();
        let mut backlog = SearchIndexer::new(&index_dir).unwrap();
        backlog.set_merge_policy(Box::new(NoMergePolicy));
        for i in 0..20 {
            cache
                .update_incremental(&mut backlog, vec![write_one(temp.path(), i)])
                .unwrap();
        }
        drop(backlog);

        let mut indexer = SearchIndexer::open(&index_dir).unwrap();
        if no_merge {
            indexer.set_merge_policy(Box::new(NoMergePolicy));
        }
        cache
            .update_incremental(&mut indexer, vec![write_one(temp.path(), 20)])
            .unwrap();
        finish_writer(indexer, merges).unwrap();
        Index::open_in_dir(&index_dir)
            .unwrap()
            .searchable_segment_ids()
            .unwrap()
            .len()
    }

    #[test]
    fn waiting_lands_the_merges_of_an_inline_update() {
        let merged = segments_after_inline_update(false, Merges::Wait);
        assert!(merged <= 8, "{merged} segments after waiting on merges");
        let control = segments_after_inline_update(true, Merges::Wait);
        assert!(control > 8, "control without merges kept {control}");
    }
}

#[cfg(test)]
mod date_tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn date_only_bounds_mean_local_midnight() {
        let montreal_winter = FixedOffset::west_opt(5 * 3600).unwrap();
        assert_eq!(
            parse_date_in("2026-03-01", &montreal_winter)
                .unwrap()
                .to_rfc3339(),
            "2026-03-01T05:00:00+00:00"
        );
        assert_eq!(
            parse_date_in("2026-03-01T10:00:00-02:00", &montreal_winter)
                .unwrap()
                .to_rfc3339(),
            "2026-03-01T12:00:00+00:00",
            "an explicit offset is kept"
        );
        assert!(parse_date_in("March 1st", &montreal_winter).is_err());
    }
}
