use super::config::get_config;
use anyhow::{Result, bail};
use glob::glob;
use std::collections::HashSet;
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

/// Find a session's JSONL file by session ID (prefix match).
/// Searches all project directories for a file named `<session_id>.jsonl`.
pub fn find_session_jsonl(session_id: &str) -> Result<Option<PathBuf>> {
    let root = projects_dir()?;
    let pattern = root.join("**/*.jsonl");
    let mut tally = SkipTally::new(&root);
    let mut prefix_matches: Vec<PathBuf> = Vec::new();
    for path in glob(&pattern.to_string_lossy())?.filter_map(|e| tally.keep_readable(e)) {
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
        else {
            continue;
        };
        if stem == session_id {
            return Ok(Some(path));
        }
        if stem.starts_with(session_id) {
            prefix_matches.push(path);
        }
    }

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
    let root = projects_dir()?;
    let pattern = root.join("**/*.jsonl");
    let mut tally = SkipTally::new(&root);
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

/// Find the most-recently-modified JSONL across all projects.
/// Used by long-lived processes (e.g. MCP server) whose cwd does not reflect
/// the currently active Claude session.
pub fn globally_active_session_jsonl() -> Option<PathBuf> {
    let root = projects_dir().ok()?;
    let pattern = root.join("**/*.jsonl");
    let mut tally = SkipTally::new(&root);
    glob(&pattern.to_string_lossy())
        .ok()?
        .filter_map(|e| tally.keep_readable(e))
        .max_by_key(|p| {
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
}
