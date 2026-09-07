use super::indexer::SearchIndexer;
use super::models::MessageType;
use super::parsers::{JsonlParser, SourceKind};
use super::utils::file_mtime;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CacheMetadata {
    pub indexed_files: HashMap<PathBuf, FileMetadata>,
    pub last_full_scan: Option<DateTime<Utc>>,
    pub total_entries: u64,
    /// Cached message counts per session (user + assistant messages only)
    #[serde(default)]
    pub session_counts: HashMap<String, usize>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileMetadata {
    pub size: u64,
    pub modified: DateTime<Utc>,
    pub indexed_at: DateTime<Utc>,
    pub entry_count: usize,
    /// Per-session message counts contributed by this file. Global totals are
    /// folded from these so they cannot drift as files are reindexed.
    #[serde(default)]
    pub conversation_counts: HashMap<String, usize>,
    /// False for entries deserialized from a cache written before
    /// `conversation_counts` existed. Distinguishes "not yet reindexed" from
    /// "reindexed and contributes no counts" (subagent transcripts).
    #[serde(default)]
    pub counts_backfilled: bool,
}

pub struct CacheManager {
    cache_dir: PathBuf,
    metadata_file: PathBuf,
    metadata: CacheMetadata,
}

/// A file parsed by the parallel phase of `update_incremental`, ready to be
/// fed into the IndexWriter by the serial phase that follows it.
struct ParsedFile {
    path: PathBuf,
    source_kind: SourceKind,
    file_size: u64,
    file_modified: DateTime<Utc>,
    entries: Vec<super::models::ConversationEntry>,
}

impl CacheManager {
    pub fn new(cache_dir: &Path) -> Result<Self> {
        let metadata_file = cache_dir.join("cache-metadata.json");

        let metadata = if metadata_file.exists() {
            let content = fs::read_to_string(&metadata_file)
                .with_context(|| format!("reading {}", metadata_file.display()))?;
            serde_json::from_str(&content)
                .with_context(|| format!("parsing {}", metadata_file.display()))?
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
        let to_parse = self.triage_files(files)?;

        if to_parse.is_empty() {
            info!("No files needed indexing");
            return Ok(());
        }

        let parsed = Self::parse_files_parallel(to_parse);
        let (files_processed, total_entries) = self.index_parsed_files(indexer, parsed)?;

        self.refresh_derived_metadata();
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

    /// Phase 1 (serial): remove deleted files from the cache and collect
    /// the files that need (re)parsing.
    fn triage_files(&mut self, files: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
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
        Ok(to_parse)
    }

    /// Phase 2 (parallel): parse all files concurrently.
    fn parse_files_parallel(to_parse: Vec<PathBuf>) -> Vec<ParsedFile> {
        let parser = JsonlParser::default();
        to_parse
            .into_par_iter()
            .filter_map(|file_path| {
                info!("Processing: {}", file_path.display());
                let source_kind = SourceKind::classify(&file_path);
                let file_size = match fs::metadata(&file_path) {
                    Ok(m) => m.len(),
                    Err(e) => {
                        warn!("Skipping {}: {}", file_path.display(), e);
                        return None;
                    }
                };
                let file_modified = match file_mtime(&file_path) {
                    Ok(t) => t,
                    Err(e) => {
                        warn!("Skipping {}: {:#}", file_path.display(), e);
                        return None;
                    }
                };
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
            .collect()
    }

    /// Phase 3 (serial): feed parsed files into the IndexWriter, update cache
    /// metadata, and commit. Returns (files_processed, total_entries).
    fn index_parsed_files(
        &mut self,
        indexer: &mut SearchIndexer,
        parsed: Vec<ParsedFile>,
    ) -> Result<(usize, usize)> {
        let mut files_processed = 0;
        let mut total_entries = 0;

        for parsed_file in parsed {
            let entry_count = parsed_file
                .entries
                .len();
            total_entries += entry_count;

            let mut conversation_counts: HashMap<String, usize> = HashMap::new();

            if entry_count > 0 {
                let path_str = parsed_file
                    .path
                    .to_string_lossy();
                indexer.delete_source_file(&path_str);

                // Subagent transcripts share their parent's sessionId; counting
                // them would double-count the session against its main file.
                if matches!(parsed_file.source_kind, SourceKind::MainSession) {
                    for entry in &parsed_file.entries {
                        if matches!(
                            entry.message_type,
                            MessageType::User | MessageType::Assistant
                        ) {
                            *conversation_counts
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
                        size: parsed_file.file_size,
                        modified: parsed_file.file_modified,
                        indexed_at: Utc::now(),
                        entry_count,
                        conversation_counts,
                        counts_backfilled: true,
                    },
                );
            files_processed += 1;
        }

        if files_processed > 0 {
            indexer.commit()?;
        }

        Ok((files_processed, total_entries))
    }

    /// Recompute global totals as a fold over per-file metadata. Deriving them
    /// rather than adjusting them in place keeps reindexing from drifting the
    /// counters, and repairs a cache that already drifted.
    fn refresh_derived_metadata(&mut self) {
        self.metadata
            .total_entries = self
            .metadata
            .indexed_files
            .values()
            .map(|file| file.entry_count as u64)
            .sum();

        // Caches written before conversation_counts existed deserialize it as
        // empty. Folding those in would zero out interaction counts for every
        // file that has not happened to change since the upgrade, so keep the
        // stored counts until each file has been reindexed at least once.
        let backfilled = self
            .metadata
            .indexed_files
            .values()
            .all(|file| file.counts_backfilled);
        if !backfilled {
            return;
        }

        self.metadata
            .session_counts
            .clear();
        for file in self
            .metadata
            .indexed_files
            .values()
        {
            for (session_id, count) in &file.conversation_counts {
                *self
                    .metadata
                    .session_counts
                    .entry(session_id.clone())
                    .or_insert(0) += count;
            }
        }
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

impl CacheManager {
    /// Quick health check - just counts stale/new files without full scan
    /// Returns (stale_count, new_count) for passive reporting
    pub fn quick_health_check(&self, all_jsonl_files: &[PathBuf]) -> FileHealthCounts {
        let mut stale = 0;
        let mut new_files = 0;
        // Only files the caller passed count as stale; excluding a path (such as
        // the always-being-written active session) must suppress its warning.
        let considered: HashSet<&Path> = all_jsonl_files
            .iter()
            .map(|p| p.as_path())
            .collect();
        for (path, meta) in &self
            .metadata
            .indexed_files
        {
            if !considered.contains(path.as_path()) {
                continue;
            }
            let current = fs::metadata(path)
                .with_context(|| format!("reading metadata of {}", path.display()))
                .and_then(|m| Ok((m.len(), file_mtime(path)?)));
            match current {
                Ok((size, mtime)) => {
                    if size != meta.size || mtime != meta.modified {
                        stale += 1;
                    }
                }
                Err(e) => {
                    error!("Counting {} as stale: {:#}", path.display(), e);
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
        FileHealthCounts { stale, new_files }
    }
}

/// Counts from `quick_health_check`: named, since two same-typed counts in
/// positional order are swap-prone.
pub struct FileHealthCounts {
    pub stale: usize,
    pub new_files: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::search::SearchEngine;
    use tempfile::TempDir;

    fn write_jsonl(path: &Path, lines: &[&str]) {
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

    fn transcript_lines(session: &str, count: usize) -> Vec<String> {
        (0..count)
            .map(|i| {
                format!(
                    r#"{{"uuid":"{session}-uuid-{i}","sessionId":"{session}","type":"user","timestamp":"2025-12-28T10:00:00Z","message":{{"role":"user","content":"message {i}"}}}}"#
                )
            })
            .collect()
    }

    fn write_transcript(path: &Path, session: &str, count: usize) {
        fs::write(path, transcript_lines(session, count).join("\n") + "\n").unwrap();
    }

    fn sum_of_file_counts(cache: &CacheManager) -> usize {
        cache
            .metadata
            .indexed_files
            .values()
            .map(|m| m.entry_count)
            .sum()
    }

    #[test]
    fn reindexing_changed_file_does_not_inflate_total() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp
            .path()
            .join("cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let transcript = temp
            .path()
            .join("session.jsonl");

        let mut cache = CacheManager::new(&cache_dir).unwrap();
        let mut indexer = SearchIndexer::new(
            &temp
                .path()
                .join("index"),
        )
        .unwrap();

        write_transcript(&transcript, "sess-a", 6);
        cache
            .update_incremental(&mut indexer, vec![transcript.clone()])
            .unwrap();
        assert_eq!(
            cache
                .metadata
                .total_entries,
            6
        );

        // Replace the 6-entry session with a 7-entry version.
        write_transcript(&transcript, "sess-a", 7);
        cache
            .update_incremental(&mut indexer, vec![transcript.clone()])
            .unwrap();

        assert_eq!(
            cache
                .metadata
                .total_entries as usize,
            sum_of_file_counts(&cache),
            "global total must track the sum of per-file counts"
        );
        assert_eq!(
            cache
                .metadata
                .total_entries,
            7
        );
    }

    #[test]
    fn session_counts_do_not_leak_when_a_session_leaves_a_file() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp
            .path()
            .join("cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let transcript = temp
            .path()
            .join("mixed.jsonl");

        let mut cache = CacheManager::new(&cache_dir).unwrap();
        let mut indexer = SearchIndexer::new(
            &temp
                .path()
                .join("index"),
        )
        .unwrap();

        // One file carrying two sessions; session_id is per-line, not per-file.
        let mut lines = transcript_lines("sess-a", 2);
        lines.extend(transcript_lines("sess-b", 3));
        fs::write(&transcript, lines.join("\n") + "\n").unwrap();
        cache
            .update_incremental(&mut indexer, vec![transcript.clone()])
            .unwrap();
        assert_eq!(
            cache
                .metadata
                .session_counts
                .get("sess-b"),
            Some(&3)
        );

        // Rewrite the file with sess-b gone entirely.
        fs::write(&transcript, transcript_lines("sess-a", 4).join("\n") + "\n").unwrap();
        cache
            .update_incremental(&mut indexer, vec![transcript.clone()])
            .unwrap();

        assert_eq!(
            cache
                .metadata
                .session_counts
                .get("sess-b"),
            None,
            "a session no longer present in the file must not linger"
        );
        assert_eq!(
            cache
                .metadata
                .session_counts
                .get("sess-a"),
            Some(&4)
        );
    }

    #[test]
    fn pre_upgrade_cache_keeps_session_counts_until_reindexed() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp
            .path()
            .join("cache");
        fs::create_dir_all(&cache_dir).unwrap();

        let mut cache = CacheManager::new(&cache_dir).unwrap();
        // Simulate metadata written before conversation_counts existed.
        cache
            .metadata
            .indexed_files
            .insert(
                PathBuf::from("/old/session.jsonl"),
                FileMetadata {
                    size: 0,
                    modified: Utc::now(),
                    indexed_at: Utc::now(),
                    entry_count: 5,
                    conversation_counts: HashMap::new(),
                    counts_backfilled: false,
                },
            );
        cache
            .metadata
            .session_counts
            .insert("sess-old".to_string(), 5);

        cache.refresh_derived_metadata();

        assert_eq!(
            cache
                .metadata
                .session_counts
                .get("sess-old"),
            Some(&5),
            "counts from a pre-upgrade cache must survive until reindex"
        );
        assert_eq!(
            cache
                .metadata
                .total_entries,
            5
        );
    }

    #[test]
    fn subagent_file_does_not_suppress_the_session_counts_fold() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp
            .path()
            .join("cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let project = temp
            .path()
            .join("project");
        fs::create_dir_all(project.join("subagents")).unwrap();

        let main_path = project.join("sess-a.jsonl");
        let agent_path = project
            .join("subagents")
            .join("agent-tst001.jsonl");
        write_transcript(&main_path, "sess-a", 4);
        write_transcript(&agent_path, "sess-a", 3);

        let mut cache = CacheManager::new(&cache_dir).unwrap();
        let mut indexer = SearchIndexer::new(
            &temp
                .path()
                .join("index"),
        )
        .unwrap();
        cache
            .update_incremental(&mut indexer, vec![main_path, agent_path])
            .unwrap();

        assert_eq!(
            cache
                .metadata
                .session_counts
                .get("sess-a"),
            Some(&4),
            "a subagent file contributes no counts yet must not block the fold"
        );
    }

    #[test]
    fn quick_health_check_ignores_files_not_passed_by_caller() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp
            .path()
            .join("cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let transcript = temp
            .path()
            .join("active.jsonl");

        let mut cache = CacheManager::new(&cache_dir).unwrap();
        let mut indexer = SearchIndexer::new(
            &temp
                .path()
                .join("index"),
        )
        .unwrap();

        write_transcript(&transcript, "sess-a", 3);
        cache
            .update_incremental(&mut indexer, vec![transcript.clone()])
            .unwrap();

        // Modify it so it is genuinely stale against the cached size/mtime.
        write_transcript(&transcript, "sess-a", 5);

        let health = cache.quick_health_check(std::slice::from_ref(&transcript));
        assert_eq!(
            (health.stale, health.new_files),
            (1, 0),
            "included file counts as stale"
        );

        let health = cache.quick_health_check(&[]);
        assert_eq!(
            (health.stale, health.new_files),
            (0, 0),
            "excluded file must not count as stale"
        );
    }
}
