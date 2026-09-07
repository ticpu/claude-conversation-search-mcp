use crate::shared::session_view;
use anyhow::{Context, Result};
use std::path::Path;

pub(crate) fn summarize_session(index_path: &Path, session_id: String) -> Result<()> {
    let (entries, _) = session_view::load_session(index_path, &session_id)?;
    if entries.is_empty() {
        anyhow::bail!("no messages found for session {session_id}");
    }

    let mut conversation = String::new();
    for entry in session_view::displayable_entries(&entries) {
        let content: String = entry
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        conversation.push_str(&format!(
            "{}: {}\n",
            entry
                .message_type
                .short_name(),
            content
        ));
    }

    let prompt = format!(
        "Summarize this conversation concisely. Include: topic, key decisions, outcome.\n\n{}",
        conversation
    );

    run_summary_subprocess(&prompt)
}

/// Run `claude --print` in a jailed, empty directory with no tools, feeding
/// `prompt` on stdin and using haiku for cost.
fn run_summary_subprocess(prompt: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[cfg(unix)]
    let temp_dir = std::env::var("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    #[cfg(windows)]
    let temp_dir = std::env::temp_dir();
    let jail = tempfile::Builder::new()
        .prefix("claude-summary-jail-")
        .tempdir_in(&temp_dir)
        .with_context(|| format!("creating a jail directory in {}", temp_dir.display()))?;
    let jail_dir = jail.path();

    let mut child = Command::new("claude")
        .args([
            "--print",
            "--tools",
            "",
            "--no-session-persistence",
            "--model",
            "haiku",
        ])
        .current_dir(jail_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    if let Some(mut stdin) = child
        .stdin
        .take()
    {
        stdin.write_all(prompt.as_bytes())?;
    }

    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("claude exited with status: {}", status);
    }

    Ok(())
}
