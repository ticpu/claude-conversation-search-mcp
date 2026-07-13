use super::indexer::SearchIndexer;
use super::models::MessageType;
use super::parsers::{JsonlParser, SourceKind};
use super::utils::file_mtime;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
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
    /// Cached message counts per session (user + assistant messages only)
    #[serde(default)]
    pub session_counts: HashMap<String, usize>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileMetadata {
    #[serde(alias = "hash")]
    pub size_hex: String,
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
        let file_size = fs::metadata(file_path)?.len();
        let file_modified = file_mtime(file_path)?;

        match self
            .metadata
            .indexed_files
            .get(file_path)
        {
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
        let parser = JsonlParser::default();

        // Phase 1 (serial): remove deleted files and collect files that need parsing.
        let mut to_parse: Vec<PathBuf> = Vec::new();
        for file_path in files {
            if !file_path.exists() {
                if self
                    .metadata
                    .indexed_files
                    .remove(&file_path)
                    .is_some()
                {
                    debug!("Removed deleted file from cache: {}", file_path.display());
                }
                continue;
            }
            if !self.needs_indexing(&file_path)? {
                debug!("Skipping unchanged file: {}", file_path.display());
                continue;
            }
            to_parse.push(file_path);
        }

        if to_parse.is_empty() {
            info!("No files needed indexing");
            return Ok(());
        }

        // Phase 2 (parallel): parse all files concurrently.
        struct ParsedFile {
            path: PathBuf,
            source_kind: SourceKind,
            file_size: u64,
            file_modified: DateTime<Utc>,
            entries: Vec<super::models::ConversationEntry>,
        }

        let parsed: Vec<_> = to_parse
            .into_par_iter()
            .filter_map(|file_path| {
                info!("Processing: {}", file_path.display());
                let source_kind = SourceKind::classify(&file_path);
                let file_size = fs::metadata(&file_path)
                    .ok()?
                    .len();
                let file_modified = file_mtime(&file_path).ok()?;
                match parser.parse_file(&file_path) {
                    Ok(entries) => Some(ParsedFile {
                        path: file_path,
                        source_kind,
                        file_size,
                        file_modified,
                        entries,
                    }),
                    Err(e) => {
                        warn!("Failed to parse {}: {}", file_path.display(), e);
                        None
                    }
                }
            })
            .collect();

        // Phase 3 (serial): feed into IndexWriter and update cache metadata.
        let mut files_processed = 0;
        let mut total_entries = 0;

        for parsed_file in parsed {
            let entry_count = parsed_file
                .entries
                .len();
            total_entries += entry_count;

            if entry_count > 0 {
                let path_str = parsed_file
                    .path
                    .to_string_lossy();
                indexer.delete_source_file(&path_str)?;

                if matches!(parsed_file.source_kind, SourceKind::MainSession) {
                    if let Some(first) = parsed_file
                        .entries
                        .first()
                    {
                        self.metadata
                            .session_counts
                            .remove(&first.session_id);
                    }
                    for entry in &parsed_file.entries {
                        if matches!(
                            entry.message_type,
                            MessageType::User | MessageType::Assistant
                        ) {
                            *self
                                .metadata
                                .session_counts
                                .entry(
                                    entry
                                        .session_id
                                        .clone(),
                                )
                                .or_insert(0) += 1;
                        }
                    }
                }

                indexer.index_conversations(parsed_file.entries, &path_str)?;
                info!("  Indexed {} entries", entry_count);
            }

            self.metadata
                .indexed_files
                .insert(
                    parsed_file.path,
                    FileMetadata {
                        size_hex: format!("{:x}", parsed_file.file_size),
                        size: parsed_file.file_size,
                        modified: parsed_file.file_modified,
                        indexed_at: Utc::now(),
                        entry_count,
                    },
                );
            files_processed += 1;
        }

        if files_processed > 0 {
            indexer.commit()?;
        }

        self.metadata
            .total_entries += total_entries as u64;
        self.metadata
            .last_full_scan = Some(Utc::now());
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

    pub fn clear_cache(&mut self) -> Result<()> {
        if self
            .cache_dir
            .exists()
        {
            fs::remove_dir_all(&self.cache_dir)?;
        }
        fs::create_dir_all(&self.cache_dir)?;

        self.metadata = CacheMetadata::default();
        self.save_metadata()?;

        info!("Cache cleared successfully");
        Ok(())
    }

    pub fn get_basic_stats(&self) -> (usize, u64, Option<DateTime<Utc>>) {
        (
            self.metadata
                .indexed_files
                .len(),
            self.metadata
                .total_entries,
            self.metadata
                .last_full_scan,
        )
    }

    /// Get cached session interaction counts
    pub fn get_session_counts(&self) -> &HashMap<String, usize> {
        &self
            .metadata
            .session_counts
    }

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

/// Result of checking index health
#[derive(Debug, Clone)]
pub struct IndexHealth {
    pub total_indexed_files: usize,
    pub total_entries: u64,
    pub last_indexed: Option<DateTime<Utc>>,
    pub stale_files: Vec<PathBuf>,
    pub missing_files: Vec<PathBuf>,
    pub new_files: Vec<PathBuf>,
    pub status: IndexHealthStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IndexHealthStatus {
    Healthy,
    NeedsUpdate,
    NeedsRebuild,
}

impl CacheManager {
    /// Quick health check - just counts stale/new files without full scan
    /// Returns (stale_count, new_count) for passive reporting
    pub fn quick_health_check(&self, all_jsonl_files: &[PathBuf]) -> (usize, usize) {
        let mut stale = 0;
        let mut new_files = 0;
        for (path, meta) in &self
            .metadata
            .indexed_files
        {
            if let Ok(current_mtime) = file_mtime(path) {
                let current_size = fs::metadata(path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                if current_size != meta.size || current_mtime != meta.modified {
                    stale += 1;
                }
            }
        }
        for path in all_jsonl_files {
            if !self
                .metadata
                .indexed_files
                .contains_key(path)
            {
                new_files += 1;
            }
        }
        (stale, new_files)
    }

    /// Check index health by comparing cached metadata with actual files
    pub fn check_index_health(&self, all_jsonl_files: &[PathBuf]) -> Result<IndexHealth> {
        let mut stale_files = Vec::new();
        let mut missing_files = Vec::new();
        let mut new_files = Vec::new();

        // Check for stale and missing files
        for (cached_path, cached_meta) in &self
            .metadata
            .indexed_files
        {
            if !cached_path.exists() {
                missing_files.push(cached_path.clone());
            } else if let Ok(current_mtime) = file_mtime(cached_path) {
                let current_size = fs::metadata(cached_path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                if current_size != cached_meta.size || current_mtime != cached_meta.modified {
                    stale_files.push(cached_path.clone());
                }
            }
        }

        // Check for new files not in cache
        for file_path in all_jsonl_files {
            if !self
                .metadata
                .indexed_files
                .contains_key(file_path)
            {
                new_files.push(file_path.clone());
            }
        }

        // Determine overall status
        let status = if missing_files.len()
            > self
                .metadata
                .indexed_files
                .len()
                / 2
        {
            IndexHealthStatus::NeedsRebuild
        } else if !stale_files.is_empty() || !new_files.is_empty() || !missing_files.is_empty() {
            IndexHealthStatus::NeedsUpdate
        } else {
            IndexHealthStatus::Healthy
        };

        Ok(IndexHealth {
            total_indexed_files: self
                .metadata
                .indexed_files
                .len(),
            total_entries: self
                .metadata
                .total_entries,
            last_indexed: self
                .metadata
                .last_full_scan,
            stale_files,
            missing_files,
            new_files,
            status,
        })
    }
}

impl std::fmt::Display for IndexHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Index Health Report")?;
        writeln!(f, "===================")?;
        writeln!(
            f,
            "Total indexed: {} files, {} entries",
            self.total_indexed_files, self.total_entries
        )?;
        if let Some(last) = self.last_indexed {
            writeln!(f, "Last indexed: {}", last.format("%Y-%m-%d %H:%M:%S UTC"))?;
        }
        writeln!(
            f,
            "Stale files: {} (modified since indexed)",
            self.stale_files
                .len()
        )?;
        writeln!(
            f,
            "Missing files: {} (deleted from disk)",
            self.missing_files
                .len()
        )?;
        writeln!(
            f,
            "New files: {} (not yet indexed)",
            self.new_files
                .len()
        )?;
        writeln!(f, "Status: {:?}", self.status)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::indexer::SearchIndexer;
    use crate::shared::search::SearchEngine;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn write_jsonl(path: &std::path::Path, lines: &[&str]) {
        let content = lines.join("\n") + "\n";
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_file_granularity_delete_preserves_main_session() {
        let tmp = TempDir::new().unwrap();
        let index_dir = tmp
            .path()
            .join("idx");

        let session_id = "aabbccdd-1234-5678-abcd-ef0123456789";

        // Create fake project layout: main JSONL + subagents/agent-x.jsonl
        let project_dir = tmp
            .path()
            .join("project");
        fs::create_dir_all(project_dir.join("subagents")).unwrap();

        let main_path = project_dir.join(format!("{}.jsonl", session_id));
        let agent_path = project_dir
            .join("subagents")
            .join("agent-tst001.jsonl");

        let main_line = format!(
            r#"{{"uuid":"main-uuid-001","sessionId":"{}","type":"user","timestamp":"2025-12-28T10:00:00Z","message":{{"role":"user","content":"Main session message"}}}}"#,
            session_id
        );
        let agent_line_v1 = format!(
            r#"{{"uuid":"agent-uuid-001","sessionId":"{}","type":"user","timestamp":"2025-12-28T10:01:00Z","message":{{"role":"user","content":"Agent message v1"}}}}"#,
            session_id
        );

        write_jsonl(&main_path, &[&main_line]);
        write_jsonl(&agent_path, &[&agent_line_v1]);

        // First update_incremental: index both files
        let mut indexer = SearchIndexer::new(&index_dir).unwrap();
        let mut cache = CacheManager::new(&index_dir).unwrap();
        cache
            .update_incremental(&mut indexer, vec![main_path.clone(), agent_path.clone()])
            .unwrap();
        drop(indexer);

        // Verify both are indexed
        let engine = SearchEngine::new(&index_dir, HashMap::new()).unwrap();
        let messages = engine
            .get_session_messages(session_id)
            .unwrap();
        assert_eq!(
            messages.len(),
            2,
            "Both main and agent entries should be indexed"
        );
        drop(engine);

        // Verify session_counts counts only the main file's messages
        let counts = cache.get_session_counts();
        assert_eq!(
            *counts
                .get(session_id)
                .unwrap_or(&0),
            1,
            "session_counts should only count main file's user/assistant messages"
        );

        // Modify ONLY the agent file (different content → different size → stale)
        let agent_line_v2 = format!(
            r#"{{"uuid":"agent-uuid-002","sessionId":"{}","type":"user","timestamp":"2025-12-28T10:02:00Z","message":{{"role":"user","content":"Agent message v2 updated content here"}}}}"#,
            session_id
        );
        write_jsonl(&agent_path, &[&agent_line_v2]);

        // Second update_incremental: only agent file is stale
        let mut indexer = SearchIndexer::open(&index_dir).unwrap();
        cache
            .update_incremental(&mut indexer, vec![main_path.clone(), agent_path.clone()])
            .unwrap();
        drop(indexer);

        // Main session docs must still be present in the index
        let engine = SearchEngine::new(&index_dir, HashMap::new()).unwrap();
        let messages = engine
            .get_session_messages(session_id)
            .unwrap();

        let main_present = messages
            .iter()
            .any(|m| m.content == "Main session message");
        assert!(
            main_present,
            "Main session docs must survive agent file re-indexing; got: {:?}",
            messages
                .iter()
                .map(|m| &m.content)
                .collect::<Vec<_>>()
        );

        // session_counts must still only reflect the main file
        let counts = cache.get_session_counts();
        assert_eq!(
            *counts
                .get(session_id)
                .unwrap_or(&0),
            1,
            "session_counts must not be affected by agent file re-indexing"
        );
    }
}
