use crate::shared::{
    CacheManager, ExclusiveIndexAccess, SearchIndexer, discover_jsonl_files, index_lock_held,
    indexer_lock_held,
};
use anyhow::Result;
use std::path::Path;
use tracing::info;

/// Reads without taking any lock: the metadata and meta.json are both replaced
/// by rename, so a concurrent writer never shows a torn state.
pub fn show_status(index_path: &Path) -> Result<()> {
    println!("Index Status");
    println!("============");

    let service = match indexer_lock_held(index_path) {
        Ok(true) => "running".to_string(),
        Ok(false) => "not running".to_string(),
        Err(e) => format!("unknown ({e:#})"),
    };
    println!("Index Service: {service}");
    let writer = match index_lock_held(index_path) {
        Ok(true) => "held by another process".to_string(),
        Ok(false) => "free".to_string(),
        Err(e) => format!("unknown ({e:#})"),
    };
    println!("Writer Lock: {writer}");

    if !index_path.exists() {
        println!("Index: Not found (will be created on next search)");
        return Ok(());
    }

    let cache_manager = CacheManager::new(index_path)?;
    let stats = cache_manager.get_stats();

    println!("Index Path: {}", index_path.display());
    println!("Total Files: {}", stats.total_files);
    println!("Total Entries: {}", stats.total_entries);

    if let Some(last_updated) = stats.last_updated {
        println!(
            "Last Updated: {}",
            last_updated.format("%Y-%m-%d %H:%M:%S UTC")
        );
    } else {
        println!("Last Updated: Never");
    }

    println!("Index Size: {:.2} MB", stats.cache_size_mb);

    Ok(())
}

pub fn rebuild(index_path: &Path) -> Result<()> {
    info!("Starting index rebuild...");

    let _lock = ExclusiveIndexAccess::acquire(index_path)?;

    let mut cache_manager = CacheManager::new(index_path)?;
    cache_manager.clear_cache()?;

    let mut indexer = SearchIndexer::new(index_path)?;
    let all_files = discover_jsonl_files()?;

    info!("Found {} files to process", all_files.len());
    cache_manager.update_full(&mut indexer, all_files, &mut |_| Ok(()))?;

    println!("Index rebuild completed successfully.");
    Ok(())
}

pub fn vacuum(index_path: &Path) -> Result<()> {
    info!("Starting index vacuum operation...");

    // rebuild() takes the exclusive lock, which flock refuses to a second descriptor.
    if !index_path.exists() {
        println!("No index found to vacuum.");
        return Ok(());
    }

    println!("Vacuuming index by rebuilding...");
    rebuild(index_path)?;

    println!("Index vacuum completed.");
    Ok(())
}
