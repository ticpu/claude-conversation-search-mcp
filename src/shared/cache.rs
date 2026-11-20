use super::indexer::SearchIndexer;
use super::lock::clear_index_dir;
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
    pub(super) cache_dir: PathBuf,
    metadata_file: PathBuf,
    pub(super) metadata: CacheMetadata,
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

/// Outcome of parsing one file. A file gone by the time it is read is purged
/// like one found missing at triage.
enum ParseOutcome {
    Parsed(ParsedFile),
    Vanished(PathBuf),
    Failed,
}

/// Files parsed, indexed and committed per chunk by `update_with_progress`.
pub const INDEX_CHUNK_FILES: usize = 200;

/// Where an update stands after a committed chunk.
#[derive(Debug, Clone, Copy)]
pub struct IndexProgress {
    pub files_done: usize,
    pub files_total: usize,
}

/// What one update did.
#[derive(Debug, Default)]
pub struct UpdateSummary {
    pub files_indexed: usize,
    pub files_done: usize,
    pub entries: usize,
    pub purged: usize,
    pub commits: usize,
}

impl std::fmt::Display for UpdateSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} of {} files indexed ({} entries), {} purged, {} commits",
            self.files_indexed, self.files_done, self.entries, self.purged, self.commits
        )
    }
}

/// What triage decided: files to (re)parse and indexed files now gone.
struct Triage {
    to_parse: Vec<PathBuf>,
    vanished: Vec<PathBuf>,
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        })
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

    /// Full pass: `discovered` plus every file the metadata knows, so a
    /// transcript deleted since the last pass is found missing and purged.
    pub fn update_full(
        &mut self,
        indexer: &mut SearchIndexer,
        discovered: Vec<PathBuf>,
        on_chunk: &mut dyn FnMut(IndexProgress) -> Result<()>,
    ) -> Result<UpdateSummary> {
        let mut files: HashSet<PathBuf> = discovered
            .into_iter()
            .collect();
        files.extend(
            self.metadata
                .indexed_files
                .keys()
                .cloned(),
        );
        self.update_with_progress(
            indexer,
            files
                .into_iter()
                .collect(),
            on_chunk,
        )
    }

    /// Bring the index in line with `files` as they are on disk: changed files
    /// are reindexed, missing ones have their documents and metadata purged.
    #[cfg(any(test, target_os = "linux"))]
    pub fn update_incremental(
        &mut self,
        indexer: &mut SearchIndexer,
        files: Vec<PathBuf>,
    ) -> Result<UpdateSummary> {
        self.update_with_progress(indexer, files, &mut |_| Ok(()))
    }

    /// `update_incremental` reporting after each chunk. Each chunk of
    /// `INDEX_CHUNK_FILES` is committed and its metadata saved before the next
    /// is parsed, which bounds memory and limits a kill to one chunk's work.
    /// An error from `on_chunk` stops the run after the chunk it reports.
    pub fn update_with_progress(
        &mut self,
        indexer: &mut SearchIndexer,
        files: Vec<PathBuf>,
        on_chunk: &mut dyn FnMut(IndexProgress) -> Result<()>,
    ) -> Result<UpdateSummary> {
        let Triage {
            to_parse,
            mut vanished,
        } = self.triage_files(files)?;
        let mut summary = UpdateSummary::default();
        if to_parse.is_empty() && vanished.is_empty() {
            debug!("No files needed indexing");
            return Ok(summary);
        }

        let files_total = to_parse.len();
        let mut chunks: Vec<&[PathBuf]> = to_parse
            .chunks(INDEX_CHUNK_FILES)
            .collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        for chunk in chunks {
            let mut parsed = Vec::new();
            for outcome in Self::parse_files_parallel(chunk.to_vec()) {
                match outcome {
                    ParseOutcome::Parsed(file) => parsed.push(file),
                    ParseOutcome::Vanished(path) => vanished.push(path),
                    ParseOutcome::Failed => {}
                }
            }

            summary.purged += self.purge_files(indexer, std::mem::take(&mut vanished));
            let (files_processed, entries) = self.index_parsed_files(indexer, parsed)?;
            indexer.commit()?;
            self.refresh_derived_metadata();
            self.metadata
                .last_full_scan = Some(Utc::now());
            self.save_metadata()?;

            summary.files_indexed += files_processed;
            summary.entries += entries;
            summary.commits += 1;
            summary.files_done += chunk.len();
            let progress = IndexProgress {
                files_done: summary.files_done,
                files_total,
            };
            debug!(
                "Committed {}/{} files",
                progress.files_done, progress.files_total
            );
            on_chunk(progress)?;
        }

        info!("Indexing complete: {summary}");
        Ok(summary)
    }

    /// Phase 1 (serial): sort `files` into those needing a parse and indexed
    /// ones that no longer exist. Unchanged and unknown-missing files drop out.
    fn triage_files(&self, files: Vec<PathBuf>) -> Result<Triage> {
        let mut triage = Triage {
            to_parse: Vec::new(),
            vanished: Vec::new(),
        };
        for file_path in files {
            let known = self
                .metadata
                .indexed_files
                .get(&file_path);
            let current = fs::metadata(&file_path)
                .with_context(|| format!("reading metadata of {}", file_path.display()))
                .and_then(|m| Ok((m.len(), file_mtime(&file_path)?)));
            match (current, known) {
                (Err(e), Some(_)) if is_not_found(&e) => triage
                    .vanished
                    .push(file_path),
                (Err(e), None) if is_not_found(&e) => {}
                (Err(e), _) => return Err(e),
                (Ok((size, mtime)), Some(cached))
                    if cached.size == size && cached.modified == mtime =>
                {
                    debug!("Skipping unchanged file: {}", file_path.display());
                }
                (Ok(_), _) => triage
                    .to_parse
                    .push(file_path),
            }
        }
        Ok(triage)
    }

    /// Delete the documents and metadata of files that no longer exist.
    fn purge_files(&mut self, indexer: &mut SearchIndexer, vanished: Vec<PathBuf>) -> usize {
        let count = vanished.len();
        for path in vanished {
            indexer.delete_source_file(&path.to_string_lossy());
            self.metadata
                .indexed_files
                .remove(&path);
            info!("Purged deleted transcript: {}", path.display());
        }
        count
    }

    /// Phase 2 (parallel): parse all files concurrently.
    fn parse_files_parallel(to_parse: Vec<PathBuf>) -> Vec<ParseOutcome> {
        let parser = JsonlParser::default();
        to_parse
            .into_par_iter()
            .map(|file_path| {
                info!("Processing: {}", file_path.display());
                let source_kind = SourceKind::classify(&file_path);
                let stat = fs::metadata(&file_path)
                    .with_context(|| format!("reading metadata of {}", file_path.display()))
                    .and_then(|m| Ok((m.len(), file_mtime(&file_path)?)));
                let (file_size, file_modified) = match stat {
                    Ok(stat) => stat,
                    Err(e) if is_not_found(&e) => return ParseOutcome::Vanished(file_path),
                    Err(e) => {
                        warn!("Skipping {}: {:#}", file_path.display(), e);
                        return ParseOutcome::Failed;
                    }
                };
                match parser.parse_file(&file_path) {
                    Ok(entries) => ParseOutcome::Parsed(ParsedFile {
                        path: file_path,
                        source_kind,
                        file_size,
                        file_modified,
                        entries,
                    }),
                    Err(e) if is_not_found(&e) => ParseOutcome::Vanished(file_path),
                    Err(e) => {
                        warn!("Failed to parse {}: {:#}", file_path.display(), e);
                        ParseOutcome::Failed
                    }
                }
            })
            .collect()
    }

    /// Phase 3 (serial): replace each parsed file's documents and record its
    /// metadata. A file parsing to no entries keeps none. Returns
    /// (files_processed, total_entries); the caller commits.
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
            let path_str = parsed_file
                .path
                .to_string_lossy()
                .into_owned();
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
            debug!("  Indexed {} entries", entry_count);

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
            clear_index_dir(&self.cache_dir)?;
        } else {
            fs::create_dir_all(&self.cache_dir).with_context(|| {
                format!(
                    "creating {}",
                    self.cache_dir
                        .display()
                )
            })?;
        }

        self.metadata = CacheMetadata::default();
        self.save_metadata()?;

        info!("Cache cleared successfully");
        Ok(())
    }

    /// Indexed files at or below `prefix`, as the metadata knows them.
    #[cfg(target_os = "linux")]
    pub fn indexed_paths_under(&self, prefix: &Path) -> Vec<PathBuf> {
        self.metadata
            .indexed_files
            .keys()
            .filter(|path| path.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Entries indexed from `path`, if the metadata knows it.
    #[cfg(test)]
    pub fn indexed_entries(&self, path: &Path) -> Option<usize> {
        self.metadata
            .indexed_files
            .get(path)
            .map(|file| file.entry_count)
    }

    /// Get cached session interaction counts
    pub fn get_session_counts(&self) -> &HashMap<String, usize> {
        &self
            .metadata
            .session_counts
    }

    /// Replace the metadata file by rename, so a reader or a kill mid-write never
    /// sees a truncated file. The fixed temp name is safe: only the lock holder saves.
    fn save_metadata(&self) -> Result<()> {
        fs::create_dir_all(&self.cache_dir).with_context(|| {
            format!(
                "creating {}",
                self.cache_dir
                    .display()
            )
        })?;
        let content = serde_json::to_vec_pretty(&self.metadata)?;
        let tmp = self
            .metadata_file
            .with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            let mut file = fs::File::create(&tmp)?;
            std::io::Write::write_all(&mut file, &content)?;
            file.sync_all()
        };
        write().with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &self.metadata_file).with_context(|| {
            format!(
                "renaming {} to {}",
                tmp.display(),
                self.metadata_file
                    .display()
            )
        })?;
        Ok(())
    }
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

    fn session_doc_count(index_dir: &Path, session: &str) -> usize {
        SearchEngine::new(index_dir, HashMap::new())
            .unwrap()
            .get_session_messages(session)
            .unwrap()
            .len()
    }

    #[test]
    fn deleted_transcript_is_purged_from_index_and_metadata() {
        let temp = TempDir::new().unwrap();
        let index_dir = temp
            .path()
            .join("idx");
        let kept = temp
            .path()
            .join("kept.jsonl");
        let gone = temp
            .path()
            .join("gone.jsonl");
        write_transcript(&kept, "sess-kept", 2);
        write_transcript(&gone, "sess-gone", 3);

        let mut indexer = SearchIndexer::new(&index_dir).unwrap();
        let mut cache = CacheManager::new(&index_dir).unwrap();
        cache
            .update_full(&mut indexer, vec![kept.clone(), gone.clone()], &mut |_| {
                Ok(())
            })
            .unwrap();
        assert_eq!(session_doc_count(&index_dir, "sess-gone"), 3);

        fs::remove_file(&gone).unwrap();
        // Discovery no longer lists it; the full pass must still find it gone.
        cache
            .update_full(&mut indexer, vec![kept.clone()], &mut |_| Ok(()))
            .unwrap();
        drop(indexer);

        assert_eq!(session_doc_count(&index_dir, "sess-gone"), 0);
        assert_eq!(session_doc_count(&index_dir, "sess-kept"), 2);
        let reloaded = CacheManager::new(&index_dir).unwrap();
        assert!(
            !reloaded
                .metadata
                .indexed_files
                .contains_key(&gone)
        );
        assert_eq!(
            reloaded
                .get_session_counts()
                .get("sess-gone"),
            None
        );
    }

    #[test]
    fn transcript_emptied_to_zero_entries_loses_its_docs() {
        let temp = TempDir::new().unwrap();
        let index_dir = temp
            .path()
            .join("idx");
        let transcript = temp
            .path()
            .join("t.jsonl");
        write_transcript(&transcript, "sess-a", 3);

        let mut indexer = SearchIndexer::new(&index_dir).unwrap();
        let mut cache = CacheManager::new(&index_dir).unwrap();
        cache
            .update_incremental(&mut indexer, vec![transcript.clone()])
            .unwrap();
        assert_eq!(session_doc_count(&index_dir, "sess-a"), 3);

        fs::write(&transcript, "").unwrap();
        cache
            .update_incremental(&mut indexer, vec![transcript.clone()])
            .unwrap();
        drop(indexer);

        assert_eq!(session_doc_count(&index_dir, "sess-a"), 0);
        assert_eq!(
            cache
                .metadata
                .total_entries,
            0
        );
    }

    fn write_many(dir: &Path, count: usize) -> Vec<PathBuf> {
        (0..count)
            .map(|i| {
                let path = dir.join(format!("f{i:04}.jsonl"));
                write_transcript(&path, &format!("sess-{i:04}"), 1);
                path
            })
            .collect()
    }

    #[test]
    fn indexing_commits_once_per_chunk() {
        let temp = TempDir::new().unwrap();
        let index_dir = temp
            .path()
            .join("idx");
        let files = write_many(temp.path(), 2 * INDEX_CHUNK_FILES + 50);

        let mut indexer = SearchIndexer::new(&index_dir).unwrap();
        let mut cache = CacheManager::new(&index_dir).unwrap();
        let mut reports = Vec::new();
        let summary = cache
            .update_with_progress(&mut indexer, files, &mut |p| {
                reports.push((p.files_done, p.files_total));
                Ok(())
            })
            .unwrap();
        assert_eq!(summary.commits, 3);
        assert_eq!(summary.files_indexed, 450);
        assert_eq!(reports, [(200, 450), (400, 450), (450, 450)]);
    }

    #[test]
    fn failure_after_the_first_chunk_leaves_it_searchable() {
        let temp = TempDir::new().unwrap();
        let index_dir = temp
            .path()
            .join("idx");
        let files = write_many(temp.path(), 2 * INDEX_CHUNK_FILES + 50);

        let mut indexer = SearchIndexer::new(&index_dir).unwrap();
        let mut cache = CacheManager::new(&index_dir).unwrap();
        let err = cache
            .update_with_progress(&mut indexer, files, &mut |_| {
                anyhow::bail!("killed before chunk 2")
            })
            .unwrap_err();
        assert_eq!(err.to_string(), "killed before chunk 2");
        drop(indexer);

        let reloaded = CacheManager::new(&index_dir).unwrap();
        let indexed: Vec<_> = reloaded
            .metadata
            .indexed_files
            .keys()
            .cloned()
            .collect();
        assert_eq!(indexed.len(), INDEX_CHUNK_FILES);
        for path in &indexed {
            let session = format!(
                "sess-{}",
                &path
                    .file_stem()
                    .unwrap()
                    .to_str()
                    .unwrap()[1..]
            );
            assert_eq!(session_doc_count(&index_dir, &session), 1, "{session}");
        }
    }

    #[test]
    fn file_vanishing_after_discovery_does_not_abort_the_pass() {
        let temp = TempDir::new().unwrap();
        let index_dir = temp
            .path()
            .join("idx");
        let present = temp
            .path()
            .join("present.jsonl");
        write_transcript(&present, "sess-p", 1);
        let never_seen = temp
            .path()
            .join("vanished.jsonl");

        let mut indexer = SearchIndexer::new(&index_dir).unwrap();
        let mut cache = CacheManager::new(&index_dir).unwrap();
        cache
            .update_incremental(&mut indexer, vec![never_seen, present])
            .unwrap();
        drop(indexer);
        assert_eq!(session_doc_count(&index_dir, "sess-p"), 1);
    }

    #[test]
    fn metadata_readers_never_see_a_partial_save() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let temp = TempDir::new().unwrap();
        let cache_dir = temp
            .path()
            .to_path_buf();
        let mut cache = CacheManager::new(&cache_dir).unwrap();
        for i in 0..20_000 {
            cache
                .metadata
                .indexed_files
                .insert(
                    PathBuf::from(format!("/projects/p/{i:08}.jsonl")),
                    FileMetadata {
                        size: i,
                        modified: Utc::now(),
                        indexed_at: Utc::now(),
                        entry_count: 1,
                        conversation_counts: HashMap::new(),
                        counts_backfilled: true,
                    },
                );
        }
        cache
            .save_metadata()
            .unwrap();

        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let done = done.clone();
            std::thread::spawn(move || {
                for _ in 0..30 {
                    cache
                        .save_metadata()
                        .unwrap();
                }
                done.store(true, Ordering::SeqCst);
            })
        };
        let mut reads = 0;
        while !done.load(Ordering::SeqCst) {
            if let Err(e) = CacheManager::new(&cache_dir) {
                panic!("read {reads} saw a partial metadata file: {e:#}");
            }
            reads += 1;
        }
        writer
            .join()
            .unwrap();
        assert!(reads > 0);
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
