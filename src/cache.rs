use crate::indexer::SearchIndexer;
use crate::parser::JsonlParser;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CacheMetadata {
    pub indexed_files: HashMap<PathBuf, FileMetadata>,
    pub last_full_scan: Option<DateTime<Utc>>,
    pub index_version: u32,
    pub total_entries: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileMetadata {
    pub hash: String,
    pub size: u64,
    pub modified: DateTime<Utc>,
    pub indexed_at: DateTime<Utc>,
    pub entry_count: usize,
}

pub struct CacheManager {
    cache_dir: PathBuf,
    metadata_file: PathBuf,
    metadata: CacheMetadata,
}

impl CacheManager {
    pub fn new(cache_dir: &Path) -> Result<Self> {
        let metadata_file = cache_dir.join("cache-metadata.json");

        let metadata = if metadata_file.exists() {
            let content = fs::read_to_string(&metadata_file)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            CacheMetadata::default()
        };

        Ok(Self {
            cache_dir: cache_dir.to_path_buf(),
            metadata_file,
            metadata,
        })
    }

    pub fn needs_indexing(&self, file_path: &Path) -> Result<bool> {
        let file_metadata = fs::metadata(file_path)?;
        let file_size = file_metadata.len();
        let file_modified = DateTime::from_timestamp(
            file_metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64,
            0,
        )
        .unwrap_or_else(Utc::now);

        match self.metadata.indexed_files.get(file_path) {
            Some(cached) => {
                // Check if file has changed using mtime and size
                Ok(cached.size != file_size || cached.modified != file_modified)
            }
            None => Ok(true), // File not indexed yet
        }
    }

    pub fn update_incremental(
        &mut self,
        indexer: &mut SearchIndexer,
        files: Vec<PathBuf>,
    ) -> Result<()> {
        let parser = JsonlParser::new();
        let mut files_processed = 0;
        let mut total_entries = 0;

        for file_path in files {
            if !file_path.exists() {
                // Remove from cache if file was deleted
                if self.metadata.indexed_files.remove(&file_path).is_some() {
                    debug!("Removed deleted file from cache: {}", file_path.display());
                }
                continue;
            }

            if !self.needs_indexing(&file_path)? {
                debug!("Skipping unchanged file: {}", file_path.display());
                continue;
            }

            info!("Processing: {}", file_path.display());

            // Parse and index the file
            match parser.parse_file(&file_path) {
                Ok(entries) => {
                    let entry_count = entries.len();
                    total_entries += entry_count;

                    if entry_count > 0 {
                        indexer.index_conversations(entries)?;
                        info!("  Indexed {} entries", entry_count);
                    }

                    // Update cache metadata
                    let file_metadata = fs::metadata(&file_path)?;
                    let file_size = file_metadata.len();
                    let file_modified = DateTime::from_timestamp(
                        file_metadata
                            .modified()?
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_secs() as i64,
                        0,
                    )
                    .unwrap_or_else(Utc::now);

                    let cached_metadata = FileMetadata {
                        hash: format!("{file_size:x}"), // Use simple hash for tracking
                        size: file_size,
                        modified: file_modified,
                        indexed_at: Utc::now(),
                        entry_count,
                    };

                    self.metadata
                        .indexed_files
                        .insert(file_path.clone(), cached_metadata);
                    files_processed += 1;
                }
                Err(e) => {
                    warn!("Failed to parse {}: {}", file_path.display(), e);
                }
            }
        }

        self.metadata.total_entries += total_entries as u64;
        self.metadata.last_full_scan = Some(Utc::now());
        self.save_metadata()?;

        if files_processed > 0 {
            info!(
                "Incremental indexing complete: {} files processed, {} entries added",
                files_processed, total_entries
            );
        } else {
            info!("No files needed indexing");
        }

        Ok(())
    }

    #[cfg(feature = "cli")]
    pub fn clear_cache(&mut self) -> Result<()> {
        if self.cache_dir.exists() {
            fs::remove_dir_all(&self.cache_dir)?;
        }
        fs::create_dir_all(&self.cache_dir)?;

        self.metadata = CacheMetadata::default();
        self.save_metadata()?;

        info!("Cache cleared successfully");
        Ok(())
    }

    pub fn get_stats(&self) -> CacheStats {
        CacheStats {
            total_files: self.metadata.indexed_files.len(),
            total_entries: self.metadata.total_entries,
            last_updated: self.metadata.last_full_scan,
            cache_size_mb: self.calculate_cache_size_mb(),
            projects: self.get_project_stats(),
        }
    }

    fn save_metadata(&self) -> Result<()> {
        fs::create_dir_all(&self.cache_dir)?;
        let content = serde_json::to_string_pretty(&self.metadata)?;
        fs::write(&self.metadata_file, content)?;
        Ok(())
    }

    fn calculate_cache_size_mb(&self) -> f64 {
        if let Ok(entries) = fs::read_dir(&self.cache_dir) {
            let total_bytes: u64 = entries
                .filter_map(|entry| entry.ok())
                .filter_map(|entry| fs::metadata(entry.path()).ok())
                .map(|metadata| metadata.len())
                .sum();
            total_bytes as f64 / (1024.0 * 1024.0)
        } else {
            0.0
        }
    }

    fn get_project_stats(&self) -> Vec<ProjectStats> {
        let mut projects: HashMap<String, ProjectStats> = HashMap::new();

        for (file_path, file_meta) in &self.metadata.indexed_files {
            if let Some(parent) = file_path.parent() {
                if let Some(project_name) = parent.file_name().and_then(|n| n.to_str()) {
                    let stats =
                        projects
                            .entry(project_name.to_string())
                            .or_insert_with(|| ProjectStats {
                                name: project_name.to_string(),
                                files: 0,
                                entries: 0,
                                last_updated: file_meta.indexed_at,
                            });

                    stats.files += 1;
                    stats.entries += file_meta.entry_count as u64;
                    if file_meta.indexed_at > stats.last_updated {
                        stats.last_updated = file_meta.indexed_at;
                    }
                }
            }
        }

        let mut project_list: Vec<ProjectStats> = projects.into_values().collect();
        project_list.sort_by(|a, b| b.last_updated.cmp(&a.last_updated));
        project_list
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_files: usize,
    pub total_entries: u64,
    pub last_updated: Option<DateTime<Utc>>,
    pub cache_size_mb: f64,
    pub projects: Vec<ProjectStats>,
}

#[derive(Debug, Clone)]
pub struct ProjectStats {
    pub name: String,
    pub files: usize,
    pub entries: u64,
    pub last_updated: DateTime<Utc>,
}
