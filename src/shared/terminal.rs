use std::io::IsTerminal;
use std::sync::OnceLock;

/// Check if terminal supports OSC 8 hyperlinks by querying it
pub fn supports_hyperlinks() -> bool {
    static SUPPORTS: OnceLock<bool> = OnceLock::new();
    *SUPPORTS.get_or_init(|| {
        // Explicit override via env var
        if let Ok(val) = std::env::var("HYPERLINKS") {
            return val != "0" && val.to_lowercase() != "false";
        }

        // Must be a TTY
        if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
            return false;
        }

        // Query terminal with DA1 (Primary Device Attributes)
        query_terminal_da1()
    })
}

/// Query terminal for capabilities using DA1 (Unix only)
#[cfg(unix)]
fn query_terminal_da1() -> bool {
    use std::io::{Read, Write};

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    // Save terminal settings and set raw mode
    let orig_termios = match nix::sys::termios::tcgetattr(&stdin) {
        Ok(t) => t,
        Err(_) => return false,
    };

    let mut raw = orig_termios.clone();
    raw.local_flags
        .remove(nix::sys::termios::LocalFlags::ICANON);
    raw.local_flags.remove(nix::sys::termios::LocalFlags::ECHO);
    raw.control_chars[nix::sys::termios::SpecialCharacterIndices::VMIN as usize] = 0;
    raw.control_chars[nix::sys::termios::SpecialCharacterIndices::VTIME as usize] = 1; // 100ms timeout

    if nix::sys::termios::tcsetattr(&stdin, nix::sys::termios::SetArg::TCSANOW, &raw).is_err() {
        return false;
    }

    // Send DA1 query: ESC [ c
    let result = (|| {
        let mut stdout = stdout.lock();
        stdout.write_all(b"\x1b[c").ok()?;
        stdout.flush().ok()?;

        // Read response with timeout
        let mut buf = [0u8; 64];
        let mut stdin_lock = stdin.lock();
        let n = stdin_lock.read(&mut buf).ok()?;

        // Any response starting with ESC [ means modern terminal
        // Format: ESC [ ? ... c
        if n >= 3 && buf[0] == 0x1b && buf[1] == b'[' {
            Some(true)
        } else {
            Some(false)
        }
    })();

    // Restore terminal settings
    let _ = nix::sys::termios::tcsetattr(&stdin, nix::sys::termios::SetArg::TCSANOW, &orig_termios);

    result.unwrap_or(false)
}

/// Windows: terminal hyperlink detection not supported
#[cfg(windows)]
fn query_terminal_da1() -> bool {
    false
}

/// Create OSC 8 hyperlink if terminal supports it, otherwise plain text
pub fn hyperlink(url: &str, text: &str) -> String {
    if supports_hyperlinks() {
        format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
    } else {
        text.to_string()
    }
}

/// Create file:// hyperlink
pub fn file_hyperlink(path: &str, text: &str) -> String {
    hyperlink(&format!("file://{}", path), text)
}
