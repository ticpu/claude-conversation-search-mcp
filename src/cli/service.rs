//! `install` and `uninstall`: the MCP registration plus the index service.
//!
//! The unit source is packaging/claude-conversation-search-index.service,
//! compiled in. Two layouts:
//!
//! - Packaged: the running binary is /usr/bin/claude-conversation-search and
//!   the package shipped the unit in /usr/lib/systemd/user/. Install only
//!   enables it, after removing a unit an earlier self-managed install wrote
//!   under the user's config dir, which would otherwise override the
//!   packaged one.
//! - Self-managed: any other binary path. Install renders the unit with that
//!   path into $XDG_CONFIG_HOME/systemd/user/.
//!
//! A rendered unit starts with a marker comment. Uninstall deletes a user
//! unit only when it carries the marker, so a unit the user wrote by hand is
//! never removed.
//!
//! The unit keeps ConditionFileIsExecutable on its binary: once the binary is
//! removed, systemd skips the unit and leaves it inactive instead of looping
//! it through restarts into a failed state.
//!
//! When `systemctl --user` cannot reach a user manager, install warns and
//! finishes as standalone: the MCP registration already succeeded and is
//! useful alone. Any systemctl failure after that point is an error carrying
//! systemctl's stderr.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;
use tracing::debug;

const MCP_NAME: &str = "claude-conversation-search";

/// Register this binary with Claude Code, then set up the index service unless
/// `standalone` (always standalone off Linux).
pub fn install(project_scope: bool, standalone: bool) -> Result<()> {
    let exe = std::env::current_exe().context("getting the current executable path")?;
    let exe_path = exe
        .to_str()
        .with_context(|| format!("executable path {} is not UTF-8", exe.display()))?;
    let scope = scope_name(project_scope);

    // Nothing registered yet is the common case, so a failure here is not an error.
    match Command::new("claude")
        .args(["mcp", "remove", "-s", scope, MCP_NAME])
        .output()
    {
        Ok(out)
            if out
                .status
                .success() => {}
        Ok(out) => debug!("claude mcp remove exited with {}", out.status),
        Err(e) => return Err(e).context("running claude mcp remove"),
    }
    let status = Command::new("claude")
        .args(["mcp", "add", "-s", scope, MCP_NAME, exe_path])
        .status()
        .context("running claude mcp add")?;
    if !status.success() {
        bail!("claude mcp add failed with {status}");
    }
    println!("registered {exe_path} with Claude Code ({scope} scope)");

    if standalone {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    systemd::install(&exe)?;
    Ok(())
}

/// Undo `install`: the MCP registration, then the index service. The index
/// itself stays.
pub fn uninstall(project_scope: bool, index_path: &Path) -> Result<()> {
    let scope = scope_name(project_scope);
    let out = Command::new("claude")
        .args(["mcp", "remove", "-s", scope, MCP_NAME])
        .output()
        .context("running claude mcp remove")?;
    let mcp_result = if out
        .status
        .success()
    {
        println!("removed the Claude Code registration ({scope} scope)");
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "claude mcp remove failed with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    };

    #[cfg(target_os = "linux")]
    systemd::uninstall()?;

    println!(
        "the index stays at {}; `claude-conversation-search cache clear` removes it",
        index_path.display()
    );
    mcp_result
}

fn scope_name(project_scope: bool) -> &'static str {
    if project_scope { "project" } else { "user" }
}

#[cfg(target_os = "linux")]
mod systemd {
    use anyhow::{Context, Result, bail};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use crate::shared::SERVICE_UNIT;

    const UNIT_SOURCE: &str =
        include_str!("../../packaging/claude-conversation-search-index.service");
    const PACKAGED_BIN: &str = "/usr/bin/claude-conversation-search";
    const PACKAGED_UNIT_DIR: &str = "/usr/lib/systemd/user";
    pub(super) const MARKER: &str = "# Written by `claude-conversation-search install`; \
         `claude-conversation-search uninstall` removes it.";

    fn unit_file_name() -> String {
        format!("{SERVICE_UNIT}.service")
    }

    fn user_unit_path() -> Result<PathBuf> {
        let config = dirs::config_dir().context("locating the user config directory")?;
        Ok(config
            .join("systemd/user")
            .join(unit_file_name()))
    }

    fn packaged_unit_path() -> PathBuf {
        Path::new(PACKAGED_UNIT_DIR).join(unit_file_name())
    }

    /// The unit for a binary at `exe`, starting with the marker.
    pub(super) fn render_unit(exe: &Path) -> Result<String> {
        let path = exe
            .to_str()
            .with_context(|| format!("{} is not UTF-8", exe.display()))?;
        if !exe.is_absolute()
            || path.contains(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '\\'))
        {
            bail!(
                "{path} cannot be written into a unit file unquoted; install the binary under a plain absolute path"
            );
        }
        // systemd expands % specifiers in ExecStart and conditions.
        let escaped = path.replace('%', "%%");
        Ok(format!(
            "{MARKER}\n{}",
            UNIT_SOURCE.replace(PACKAGED_BIN, &escaped)
        ))
    }

    /// Whether the file at `path` is a unit `install` wrote. Missing reads as no.
    pub(super) fn written_by_install(path: &Path) -> Result<bool> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(text
                .lines()
                .next()
                == Some(MARKER)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn systemctl(args: &[&str]) -> Result<()> {
        let out = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
            .with_context(|| format!("running systemctl --user {}", args.join(" ")))?;
        if !out
            .status
            .success()
        {
            bail!(
                "systemctl --user {} failed with {}: {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Why the user manager cannot be used, if it cannot.
    fn user_manager_unavailable() -> Option<String> {
        systemctl(&["show-environment"])
            .err()
            .map(|e| format!("{e:#}"))
    }

    pub(super) fn install(exe: &Path) -> Result<()> {
        if let Some(reason) = user_manager_unavailable() {
            eprintln!(
                "warning: no systemd user manager ({reason}); installed without the index service"
            );
            return Ok(());
        }
        let user_unit = user_unit_path()?;
        let packaged = exe == Path::new(PACKAGED_BIN) && packaged_unit_path().exists();
        if packaged {
            if written_by_install(&user_unit)? {
                std::fs::remove_file(&user_unit)
                    .with_context(|| format!("removing {}", user_unit.display()))?;
                println!(
                    "removed {}, which would override the packaged unit",
                    user_unit.display()
                );
            }
        } else {
            if user_unit.exists() && !written_by_install(&user_unit)? {
                bail!(
                    "{} exists and was not written by install; remove it or install with --standalone",
                    user_unit.display()
                );
            }
            let dir = user_unit
                .parent()
                .context("unit path has no parent")?;
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            std::fs::write(&user_unit, render_unit(exe)?)
                .with_context(|| format!("writing {}", user_unit.display()))?;
            println!("wrote {}", user_unit.display());
        }
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", SERVICE_UNIT])?;
        systemctl(&["restart", SERVICE_UNIT])?;
        println!(
            "enabled and started {SERVICE_UNIT}; `claude-conversation-search uninstall` undoes this"
        );
        Ok(())
    }

    pub(super) fn uninstall() -> Result<()> {
        let user_unit = user_unit_path()?;
        let ours = written_by_install(&user_unit)?;
        let known = ours || user_unit.exists() || packaged_unit_path().exists();
        if let Some(reason) = user_manager_unavailable() {
            if ours {
                std::fs::remove_file(&user_unit)
                    .with_context(|| format!("removing {}", user_unit.display()))?;
                println!("removed {}", user_unit.display());
            }
            eprintln!("warning: no systemd user manager ({reason}); the service was not stopped");
            return Ok(());
        }
        if known {
            systemctl(&["disable", "--now", SERVICE_UNIT])?;
            println!("stopped and disabled {SERVICE_UNIT}");
        }
        if ours {
            std::fs::remove_file(&user_unit)
                .with_context(|| format!("removing {}", user_unit.display()))?;
            println!("removed {}", user_unit.display());
            systemctl(&["daemon-reload"])?;
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::systemd::{MARKER, render_unit, written_by_install};
    use std::path::Path;

    const SHIPPED: &str = include_str!("../../packaging/claude-conversation-search-index.service");

    #[test]
    fn rendered_unit_names_the_binary_and_carries_the_marker() {
        let unit = render_unit(Path::new("/home/u/.local/bin/ccs-50%")).unwrap();
        assert_eq!(
            unit.lines()
                .next(),
            Some(MARKER)
        );
        assert!(unit.contains("\nExecStart=/home/u/.local/bin/ccs-50%% index watch\n"));
        assert!(unit.contains("\nConditionFileIsExecutable=/home/u/.local/bin/ccs-50%%\n"));
        assert!(!unit.contains("/usr/bin/"), "{unit}");
    }

    #[test]
    fn shipped_unit_is_the_packaged_one_unmarked() {
        assert!(!SHIPPED.contains(MARKER));
        assert!(SHIPPED.contains("\nExecStart=/usr/bin/claude-conversation-search index watch\n"));
        assert!(
            SHIPPED.contains("\nConditionFileIsExecutable=/usr/bin/claude-conversation-search\n")
        );
    }

    #[test]
    fn a_path_systemd_would_split_is_refused() {
        assert!(render_unit(Path::new("/home/u/my bin/ccs")).is_err());
        assert!(render_unit(Path::new("relative/ccs")).is_err());
    }

    #[test]
    fn only_a_marked_unit_counts_as_written_by_install() {
        let temp = tempfile::TempDir::new().unwrap();
        let marked = temp
            .path()
            .join("marked.service");
        std::fs::write(&marked, render_unit(Path::new("/opt/ccs")).unwrap()).unwrap();
        let hand = temp
            .path()
            .join("hand.service");
        std::fs::write(&hand, SHIPPED).unwrap();
        assert!(written_by_install(&marked).unwrap());
        assert!(!written_by_install(&hand).unwrap());
        assert!(
            !written_by_install(
                &temp
                    .path()
                    .join("missing")
            )
            .unwrap()
        );
    }
}
