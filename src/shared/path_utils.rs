//! The `.claude/projects/` layout: where transcripts live and which session
//! a transcript belongs to.
//!
//! A main transcript is `<project-dir>/<session-id>.jsonl`. A subagent
//! transcript is `<project-dir>/<session-id>/subagents/**/agent-<id>.jsonl`
//! and belongs to the session named by the directory above `subagents`; its
//! own stem is never a session id. Session prefixes resolve against main
//! transcript stems only, case-insensitively.
//!
//! Active session. The MCP server leaves the session calling it out of its
//! results. Resolution order: CLAUDE_CODE_SESSION_ID; else the newest main
//! transcript in the project dir of CLAUDE_PROJECT_DIR; else the newest main
//! transcript anywhere. Claude Code sets both variables when it spawns the
//! server, so they reflect the session the server was spawned for: if that
//! process changes session without respawning the server, the exclusion
//! keeps the old id. The source that decided travels with the id so the
//! debug line can name it.

use super::config::get_config;
use super::models::SessionFilter;
use super::parsers::SourceKind;
use anyhow::{Result, bail};
use glob::glob;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tracing::error;

/// Extract first 8 characters of a UUID for display
pub fn short_uuid(uuid: &str) -> &str {
    &uuid[..8.min(uuid.len())]
}

/// Replace home directory with ~ for display
pub fn home_to_tilde(path: &str) -> String {
    if path.is_empty() || path == "unknown" {
        return path.to_string();
    }
    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };
    let home_str = home.to_string_lossy();
    if home_str.is_empty() {
        path.to_string()
    } else {
        path.replace(home_str.as_ref(), "~")
    }
}

/// Convert an absolute path to Claude's project directory name format.
/// Claude replaces `/`, `\`, and `.` with `-`.
pub fn project_dir_name(path: &str) -> String {
    path.replace(['/', '\\', '.'], "-")
}

/// Path to the `.claude/projects/` directory.
pub fn projects_dir() -> Result<PathBuf> {
    Ok(get_config()
        .get_claude_dir()?
        .join("projects"))
}

/// Construct path to a session's JSONL file.
pub fn session_jsonl_path(project_path: &str, session_id: &str) -> Option<PathBuf> {
    let dir_name = project_dir_name(project_path);
    Some(
        get_config()
            .get_claude_dir()
            .ok()?
            .join("projects")
            .join(dir_name)
            .join(format!("{}.jsonl", session_id)),
    )
}

/// The `<session-id>` directory holding a subagent transcript, or None for a
/// main transcript.
fn subagent_session_dir(transcript: &Path) -> Option<&Path> {
    if !matches!(SourceKind::classify(transcript), SourceKind::Agent { .. }) {
        return None;
    }
    transcript
        .ancestors()
        .find(|dir| dir.file_name() == Some(OsStr::new("subagents")))?
        .parent()
}

/// The main transcript a JSONL file belongs to: itself, or the
/// `<session-id>.jsonl` beside the directory holding a subagent transcript.
pub fn main_transcript_of(transcript: &Path) -> PathBuf {
    let Some(dir) = subagent_session_dir(transcript) else {
        return transcript.to_path_buf();
    };
    let mut name = dir
        .file_name()
        .unwrap_or_default()
        .to_os_string();
    name.push(".jsonl");
    dir.with_file_name(name)
}

/// Counts the entries a walk could not read and reports them as one line when
/// the walk ends: one unreadable directory among thousands must neither fail
/// the walk nor bury its result under a line per path.
struct SkipTally {
    root: PathBuf,
    skipped: usize,
    first: Option<String>,
}

impl SkipTally {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            skipped: 0,
            first: None,
        }
    }

    fn keep_readable(&mut self, entry: glob::GlobResult) -> Option<PathBuf> {
        match entry {
            Ok(path) => Some(path),
            Err(e) => {
                if first_report_of(e.path()) {
                    self.skipped += 1;
                    self.first
                        .get_or_insert_with(|| {
                            format!(
                                "{}: {}",
                                e.path()
                                    .display(),
                                e.error()
                            )
                        });
                }
                None
            }
        }
    }
}

impl Drop for SkipTally {
    fn drop(&mut self) {
        let Some(first) = self
            .first
            .as_deref()
        else {
            return;
        };
        if self.skipped == 1 {
            error!(
                "Skipped one unreadable path under {}: {}",
                self.root
                    .display(),
                first
            );
        } else {
            error!(
                "Skipped {} unreadable paths under {} (first: {})",
                self.skipped,
                self.root
                    .display(),
                first
            );
        }
    }
}

/// True the first time this process meets `path`. Several walks cross the same
/// unreadable directory, and repeating it once per walk tells nobody anything.
fn first_report_of(path: &Path) -> bool {
    static REPORTED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    REPORTED
        .get_or_init(Default::default)
        .lock()
        .map(|mut reported| reported.insert(path.to_path_buf()))
        .unwrap_or(true)
}

/// Main transcripts under `projects` whose session id starts with `prefix`,
/// ignoring case; only the exact match when there is one.
fn main_transcripts_matching(projects: &Path, prefix: &str) -> Result<Vec<PathBuf>> {
    let prefix = prefix.to_lowercase();
    let pattern = projects.join("*/*.jsonl");
    let mut tally = SkipTally::new(projects);
    let mut prefix_matches: Vec<PathBuf> = Vec::new();
    for path in glob(&pattern.to_string_lossy())?.filter_map(|e| tally.keep_readable(e)) {
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_lowercase)
        else {
            continue;
        };
        if stem == prefix {
            return Ok(vec![path]);
        }
        if stem.starts_with(&prefix) {
            prefix_matches.push(path);
        }
    }
    Ok(prefix_matches)
}

/// Full session ids whose transcript name starts with `prefix`, ignoring
/// case; only the exact one when it exists.
pub fn session_ids_with_prefix_in(projects: &Path, prefix: &str) -> Result<Vec<String>> {
    let mut ids: Vec<String> = main_transcripts_matching(projects, prefix)?
        .iter()
        .filter_map(|p| session_of_transcript(p))
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Resolve a user-supplied session prefix for filtering search results.
pub fn resolve_session_filter(prefix: &str) -> Result<SessionFilter> {
    let ids = session_ids_with_prefix_in(&projects_dir()?, prefix)?;
    Ok(if ids.is_empty() {
        SessionFilter::Prefix(prefix.to_string())
    } else {
        SessionFilter::Ids(ids)
    })
}

/// Find a session's main transcript by session id prefix, ignoring case.
pub fn find_session_jsonl(session_id: &str) -> Result<Option<PathBuf>> {
    let mut prefix_matches = main_transcripts_matching(&projects_dir()?, session_id)?;
    match prefix_matches.len() {
        0 => Ok(None),
        1 => Ok(prefix_matches.pop()),
        _ => {
            let candidates: Vec<String> = prefix_matches
                .iter()
                .map(|p| {
                    p.display()
                        .to_string()
                })
                .collect();
            bail!(
                "session id '{}' matches {} files: {}",
                session_id,
                candidates.len(),
                candidates.join(", ")
            )
        }
    }
}

/// Discover all JSONL session files under `.claude/projects/`.
pub fn discover_jsonl_files() -> Result<Vec<PathBuf>> {
    discover_jsonl_files_under(&projects_dir()?)
}

/// Every JSONL file below `root`, at any depth.
pub fn discover_jsonl_files_under(root: &Path) -> Result<Vec<PathBuf>> {
    let pattern = root.join("**/*.jsonl");
    let mut tally = SkipTally::new(root);
    let files: Vec<PathBuf> = glob(&pattern.to_string_lossy())?
        .filter_map(|e| tally.keep_readable(e))
        .collect();
    Ok(files)
}

/// Walk up from `cwd` through `projects`, returning the most-recently-modified
/// JSONL for the first matching project directory found.
#[cfg(test)]
fn find_session_in_projects(cwd: &Path, projects: &Path) -> Option<PathBuf> {
    let mut current = cwd;
    loop {
        let dir_name = project_dir_name(&current.to_string_lossy());
        let project_dir = projects.join(&dir_name);
        if project_dir.exists() {
            let mut tally = SkipTally::new(&project_dir);
            let best = glob(
                &project_dir
                    .join("*.jsonl")
                    .to_string_lossy(),
            )
            .ok()?
            .filter_map(|e| tally.keep_readable(e))
            .max_by_key(|p| {
                p.metadata()
                    .and_then(|m| m.modified())
                    .ok()
            });
            if best.is_some() {
                return best;
            }
        }
        match current.parent() {
            Some(p) if p != current => current = p,
            _ => return None,
        }
    }
}

/// The session a transcript belongs to: a main transcript's stem, or the
/// directory above `subagents` for a subagent transcript.
pub fn session_of_transcript(transcript: &Path) -> Option<String> {
    let named = subagent_session_dir(transcript)
        .and_then(|dir| dir.file_name())
        .or_else(|| transcript.file_stem())?;
    named
        .to_str()
        .map(str::to_string)
}

/// What decided the active session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ActiveSessionSource {
    SessionIdEnv,
    ProjectDirNewest,
    GlobalNewest,
}

impl std::fmt::Display for ActiveSessionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SessionIdEnv => "CLAUDE_CODE_SESSION_ID",
            Self::ProjectDirNewest => "newest transcript of CLAUDE_PROJECT_DIR",
            Self::GlobalNewest => "newest transcript",
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveSession {
    pub session_id: String,
    pub source: ActiveSessionSource,
}

/// The session the MCP server was spawned for, from its environment.
pub fn active_session(projects: &Path, transcripts: &[PathBuf]) -> Option<ActiveSession> {
    let session_id = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    let project_dir = std::env::var("CLAUDE_PROJECT_DIR").ok();
    resolve_active_session(
        projects,
        transcripts,
        session_id.as_deref(),
        project_dir.as_deref(),
    )
}

/// `active_session` with its environment passed in. An empty variable
/// counts as unset.
fn resolve_active_session(
    projects: &Path,
    transcripts: &[PathBuf],
    session_id: Option<&str>,
    project_dir: Option<&str>,
) -> Option<ActiveSession> {
    if let Some(id) = session_id.filter(|id| !id.is_empty()) {
        return Some(ActiveSession {
            session_id: id.to_string(),
            source: ActiveSessionSource::SessionIdEnv,
        });
    }
    let mains = || {
        transcripts
            .iter()
            .filter(|p| SourceKind::classify(p) == SourceKind::MainSession)
    };
    let in_project = project_dir
        .filter(|dir| !dir.is_empty())
        .and_then(|dir| {
            let project = projects.join(project_dir_name(dir));
            newest(mains().filter(|p| p.parent() == Some(project.as_path())))
        });
    let (newest_main, source) = match in_project {
        Some(path) => (path, ActiveSessionSource::ProjectDirNewest),
        None => (newest(mains())?, ActiveSessionSource::GlobalNewest),
    };
    Some(ActiveSession {
        session_id: session_of_transcript(newest_main)?,
        source,
    })
}

fn newest<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Option<&'a PathBuf> {
    paths.max_by_key(|p| {
        p.metadata()
            .and_then(|m| m.modified())
            .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_short_uuid() {
        assert_eq!(short_uuid("12345678-abcd-efgh"), "12345678");
        assert_eq!(short_uuid("abc"), "abc");
        assert_eq!(short_uuid(""), "");
    }

    #[test]
    fn test_project_dir_name() {
        assert_eq!(
            project_dir_name("/home/user/my.project"),
            "-home-user-my-project"
        );
        assert_eq!(project_dir_name("/home/user/foo"), "-home-user-foo");
        assert_eq!(project_dir_name("C:\\Users\\foo"), "C:-Users-foo");
    }

    #[test]
    fn test_find_session_in_projects_finds_session() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Simulate: cwd = root/my/project, claude projects dir at root/claude/projects
        let cwd = root
            .join("my")
            .join("project");
        fs::create_dir_all(&cwd).unwrap();

        let dir_name = project_dir_name(&cwd.to_string_lossy());
        let project_dir = root
            .join("claude")
            .join("projects")
            .join(&dir_name);
        fs::create_dir_all(&project_dir).unwrap();

        let session_file = project_dir.join("abc123.jsonl");
        fs::write(&session_file, b"{}").unwrap();

        let found = find_session_in_projects(
            &cwd,
            &root
                .join("claude")
                .join("projects"),
        );
        assert_eq!(found, Some(session_file));
    }

    #[test]
    fn test_find_session_in_projects_walks_up() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // cwd is a subdirectory; session registered at parent level
        let parent = root
            .join("my")
            .join("project");
        let cwd = parent
            .join("src")
            .join("lib");
        fs::create_dir_all(&cwd).unwrap();

        let dir_name = project_dir_name(&parent.to_string_lossy());
        let project_dir = root
            .join("claude")
            .join("projects")
            .join(&dir_name);
        fs::create_dir_all(&project_dir).unwrap();
        let session_file = project_dir.join("sess.jsonl");
        fs::write(&session_file, b"{}").unwrap();

        let found = find_session_in_projects(
            &cwd,
            &root
                .join("claude")
                .join("projects"),
        );
        assert_eq!(found, Some(session_file));
    }

    #[test]
    fn test_find_session_in_projects_not_found() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp
            .path()
            .join("some")
            .join("random")
            .join("path");
        fs::create_dir_all(&cwd).unwrap();
        let projects = tmp
            .path()
            .join("claude")
            .join("projects");
        fs::create_dir_all(&projects).unwrap();

        let found = find_session_in_projects(&cwd, &projects);
        assert!(found.is_none());
    }

    /// Two projects, each with a main transcript and a subagent transcript.
    /// Modification order, oldest first: `here` main, `other` main, `other`
    /// subagent, `here` subagent; the newest file belongs to `here`, the
    /// newest main transcript to `other`.
    struct Layout {
        _tmp: TempDir,
        projects: PathBuf,
        files: Vec<PathBuf>,
    }

    const HERE: &str = "11111111-aaaa-bbbb-cccc-000000000001";
    const OTHER: &str = "22222222-aaaa-bbbb-cccc-000000000002";

    fn layout() -> Layout {
        let tmp = TempDir::new().unwrap();
        let projects = tmp
            .path()
            .join("projects");
        let here = projects.join(project_dir_name("/home/u/GIT/here"));
        let other = projects.join(project_dir_name("/home/u/GIT/other"));
        let files = vec![
            here.join(format!("{HERE}.jsonl")),
            other.join(format!("{OTHER}.jsonl")),
            other
                .join(OTHER)
                .join("subagents")
                .join("agent-b2.jsonl"),
            here.join(HERE)
                .join("subagents")
                .join("agent-a1.jsonl"),
        ];
        let base = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for (i, f) in files
            .iter()
            .enumerate()
        {
            fs::create_dir_all(
                f.parent()
                    .unwrap(),
            )
            .unwrap();
            fs::File::create(f)
                .unwrap()
                .set_modified(base + std::time::Duration::from_secs(60 * i as u64))
                .unwrap();
        }
        Layout {
            _tmp: tmp,
            projects,
            files,
        }
    }

    #[test]
    fn session_id_env_decides_the_active_session() {
        let l = layout();
        let active = resolve_active_session(
            &l.projects,
            &l.files,
            Some("env-id"),
            Some("/home/u/GIT/here"),
        );
        assert_eq!(
            active,
            Some(ActiveSession {
                session_id: "env-id".to_string(),
                source: ActiveSessionSource::SessionIdEnv,
            })
        );
    }

    #[test]
    fn project_dir_env_picks_its_newest_main_transcript() {
        let l = layout();
        let active =
            resolve_active_session(&l.projects, &l.files, Some(""), Some("/home/u/GIT/here"));
        assert_eq!(
            active,
            Some(ActiveSession {
                session_id: HERE.to_string(),
                source: ActiveSessionSource::ProjectDirNewest,
            })
        );
    }

    #[test]
    fn without_env_the_newest_main_transcript_wins_over_subagents() {
        let l = layout();
        let active = resolve_active_session(&l.projects, &l.files, None, None);
        assert_eq!(
            active,
            Some(ActiveSession {
                session_id: OTHER.to_string(),
                source: ActiveSessionSource::GlobalNewest,
            })
        );
    }

    #[test]
    fn session_prefix_resolves_against_main_transcripts_ignoring_case() {
        let l = layout();
        let longer = format!("{HERE}-extra");
        fs::write(l.files[0].with_file_name(format!("{longer}.jsonl")), b"").unwrap();
        let ids = |prefix: &str| session_ids_with_prefix_in(&l.projects, prefix).unwrap();

        assert_eq!(ids("1111"), [HERE.to_string(), longer.clone()]);
        assert_eq!(ids("11111111-AAAA-b"), [HERE.to_string(), longer]);
        assert_eq!(ids(&HERE.to_uppercase()), [HERE], "the exact id wins");
        assert!(ids("agent").is_empty(), "subagent stems are not sessions");
    }

    #[test]
    fn subagent_transcript_maps_to_its_parent_session() {
        let l = layout();
        assert_eq!(session_of_transcript(&l.files[3]).as_deref(), Some(HERE));
        assert_eq!(session_of_transcript(&l.files[0]).as_deref(), Some(HERE));
        assert_eq!(
            main_transcript_of(&l.files[2]),
            l.files[1],
            "a subagent's main transcript is its session's file"
        );
    }
}
