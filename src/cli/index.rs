use crate::shared::{
    CacheManager, ExclusiveIndexAccess, SearchIndexer, SharedIndexAccess, discover_jsonl_files,
};
use anyhow::Result;
use std::path::Path;
use tracing::info;

pub fn show_status(index_path: &Path) -> Result<()> {
    println!("Index Status");
    println!("============");

    if ExclusiveIndexAccess::is_available() {
        println!("Lock Status: Available");
    } else if SharedIndexAccess::is_available() {
        println!("Lock Status: Read-only access available");
    } else {
        println!("Lock Status: Locked by another process");
    }

    if !index_path.exists() {
        println!("Index: Not found (will be created on next search)");
        return Ok(());
    }

    let _lock = match SharedIndexAccess::acquire() {
        Ok(lock) => lock,
        Err(e) => {
            println!("Index: Unable to read ({})", e);
            return Ok(());
        }
    };

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

    let _lock = ExclusiveIndexAccess::acquire()?;

    let mut cache_manager = CacheManager::new(index_path)?;
    cache_manager.clear_cache()?;

    let mut indexer = SearchIndexer::new(index_path)?;
    let all_files = discover_jsonl_files()?;

    info!("Found {} files to process", all_files.len());
    cache_manager.update_incremental(&mut indexer, all_files)?;

    println!("Index rebuild completed successfully.");
    Ok(())
}

pub fn vacuum(index_path: &Path) -> Result<()> {
    info!("Starting index vacuum operation...");

    let _lock = ExclusiveIndexAccess::acquire()?;

    if !index_path.exists() {
        println!("No index found to vacuum.");
        return Ok(());
    }

    println!("Vacuuming index by rebuilding...");
    rebuild(index_path)?;

    println!("Index vacuum completed.");
    Ok(())
}
