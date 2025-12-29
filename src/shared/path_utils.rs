use std::path::PathBuf;

use super::config::get_config;

/// Extract first 8 characters of a UUID for display
pub fn short_uuid(uuid: &str) -> &str {
    &uuid[..8.min(uuid.len())]
}

/// Replace home directory with ~ for display
pub fn home_to_tilde(path: &str) -> String {
    if path.is_empty() || path == "unknown" {
        return path.to_string();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        path.to_string()
    } else {
        path.replace(&home, "~")
    }
}

/// Convert path to Claude's project directory name format (slashes and dots become dashes)
pub fn project_dir_name(path: &str) -> String {
    path.replace(['/', '.'], "-")
}

/// Construct path to a session's JSONL file
pub fn session_jsonl_path(project_path: &str, session_id: &str) -> Option<PathBuf> {
    let claude_dir = get_config().get_claude_dir().ok()?;
    let dir_name = project_dir_name(project_path);
    Some(
        claude_dir
            .join("projects")
            .join(dir_name)
            .join(format!("{}.jsonl", session_id)),
    )
}
