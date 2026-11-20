//! `index watch` as a process: killed by SIGTERM at any time without losing
//! committed work, and exiting with the status the unit restarts cleanly on
//! when its binary is replaced.
//!
//! Each test runs the binary against its own config (XDG_CONFIG_HOME) naming
//! an index and a Claude dir inside a sandbox under the crate's scratch/, so
//! nothing touches the real index or transcripts.
#![cfg(target_os = "linux")]

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_claude-conversation-search");

struct Sandbox {
    dir: TempDir,
    projects: PathBuf,
    index: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let dir = tempfile::Builder::new()
            .prefix(&format!("{name}-"))
            .tempdir_in(&scratch)
            .unwrap();
        let claude = dir
            .path()
            .join("claude");
        let index = dir
            .path()
            .join("index");
        let config_dir = dir
            .path()
            .join("config/claude-conversation-search-mcp");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.yaml"),
            format!(
                "index:\n  cache_dir: {}\n  claude_dir: {}\n",
                index.display(),
                claude.display()
            ),
        )
        .unwrap();
        let projects = claude.join("projects/p");
        std::fs::create_dir_all(&projects).unwrap();
        Self {
            dir,
            projects,
            index,
        }
    }

    fn command(&self, bin: &Path) -> Command {
        let mut cmd = Command::new(bin);
        cmd.env(
            "XDG_CONFIG_HOME",
            self.dir
                .path()
                .join("config"),
        )
        .env_remove("NOTIFY_SOCKET")
        .stdin(Stdio::null());
        cmd
    }

    fn spawn_watch(&self, bin: &Path, log: &str) -> Child {
        let log = std::fs::File::create(
            self.dir
                .path()
                .join(log),
        )
        .unwrap();
        self.command(bin)
            .args(["index", "watch"])
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap()
    }

    fn write_transcript(&self, name: &str, word: &str) -> PathBuf {
        let path = self
            .projects
            .join(format!("{name}.jsonl"));
        std::fs::write(
            &path,
            format!(
                r#"{{"uuid":"{name}-u","sessionId":"{name}","type":"user","timestamp":"2025-12-28T10:00:00Z","message":{{"role":"user","content":"{word}"}}}}"#
            ) + "\n",
        )
        .unwrap();
        path
    }

    /// Wait until the saved metadata lists `path`.
    fn wait_indexed(&self, path: &Path, child: &mut Child) {
        let deadline = Instant::now() + Duration::from_secs(15);
        let needle = path
            .to_str()
            .unwrap();
        loop {
            let metadata = std::fs::read_to_string(
                self.index
                    .join("cache-metadata.json"),
            )
            .unwrap_or_default();
            if metadata.contains(needle) {
                return;
            }
            if let Some(status) = child
                .try_wait()
                .unwrap()
            {
                panic!("index watch exited early with {status}");
            }
            assert!(Instant::now() < deadline, "{needle} never indexed");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn wait_exit(child: &mut Child, within: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(status) = child
            .try_wait()
            .unwrap()
        {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// The exit status the shipped unit restarts on regardless of Restart=.
fn unit_restart_force_status() -> i32 {
    include_str!("../packaging/claude-conversation-search-index.service")
        .lines()
        .find_map(|line| line.strip_prefix("RestartForceExitStatus="))
        .expect("the unit sets RestartForceExitStatus")
        .parse()
        .unwrap()
}

fn sigterm(child: &Child) {
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
}

#[test]
fn sigterm_kills_at_once_and_a_restart_keeps_committed_work() {
    let sandbox = Sandbox::new("watch-sigterm");
    let bin = Path::new(BIN);
    let first = sandbox.write_transcript("first", "zebracorn");
    let mut watch = sandbox.spawn_watch(bin, "watch1.log");
    sandbox.wait_indexed(&first, &mut watch);
    let second = sandbox.write_transcript("second", "quokkafish");
    sandbox.wait_indexed(&second, &mut watch);

    sigterm(&watch);
    let status =
        wait_exit(&mut watch, Duration::from_secs(1)).expect("still running 1 s after SIGTERM");
    assert_eq!(status.signal(), Some(Signal::SIGTERM as i32));

    let mut restarted = sandbox.spawn_watch(bin, "watch2.log");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        restarted
            .try_wait()
            .unwrap()
            .is_none(),
        "the restart must reopen the index the kill left behind"
    );
    for word in ["zebracorn", "quokkafish"] {
        let out = sandbox
            .command(bin)
            .args(["search", word])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status
                .success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains(word),
            "{word} not found after restart: {stdout}"
        );
    }
    sigterm(&restarted);
    wait_exit(&mut restarted, Duration::from_secs(1)).expect("restarted watch ignored SIGTERM");
}

#[test]
fn replacing_the_binary_exits_with_the_status_the_unit_restarts_on() {
    let sandbox = Sandbox::new("watch-replace");
    let bin_dir = sandbox
        .dir
        .path()
        .join("bin");
    std::fs::create_dir(&bin_dir).unwrap();
    let bin = bin_dir.join("claude-conversation-search");
    std::fs::copy(BIN, &bin).unwrap();
    let first = sandbox.write_transcript("first", "zebracorn");
    let mut watch = sandbox.spawn_watch(&bin, "watch.log");
    sandbox.wait_indexed(&first, &mut watch);
    assert!(
        wait_exit(&mut watch, Duration::from_millis(1500)).is_none(),
        "control: an untouched binary keeps running"
    );

    let staged = bin_dir.join("claude-conversation-search.new");
    std::fs::copy(BIN, &staged).unwrap();
    std::fs::rename(&staged, &bin).unwrap();

    let status =
        wait_exit(&mut watch, Duration::from_secs(5)).expect("still running on a replaced binary");
    assert_eq!(status.code(), Some(unit_restart_force_status()), "{status}");
    let log = std::fs::read_to_string(
        sandbox
            .dir
            .path()
            .join("watch.log"),
    )
    .unwrap();
    assert!(log.contains("was replaced"), "{log}");
}
