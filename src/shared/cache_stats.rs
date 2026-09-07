use super::cache::CacheManager;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fs;

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

impl CacheManager {
    pub fn get_stats(&self) -> CacheStats {
        CacheStats {
            total_files: self
                .metadata
                .indexed_files
                .len(),
            total_entries: self
                .metadata
                .total_entries,
            last_updated: self
                .metadata
                .last_full_scan,
            cache_size_mb: self.calculate_cache_size_mb(),
            projects: self.get_project_stats(),
        }
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

        for (file_path, file_meta) in &self
            .metadata
            .indexed_files
        {
            if let Some(parent) = file_path.parent()
                && let Some(project_name) = parent
                    .file_name()
                    .and_then(|n| n.to_str())
            {
                let stats = projects
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

        let mut project_list: Vec<ProjectStats> = projects
            .into_values()
            .collect();
        project_list.sort_by(|a, b| {
            b.last_updated
                .cmp(&a.last_updated)
        });
        project_list
    }
}
