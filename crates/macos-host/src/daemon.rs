//! Pure control plane for the `rustscreen` daemon: runtime paths, PID-file
//! lifecycle, stale-PID decisions, subcommand parsing, and user-facing messages.
//! Cross-platform and feature-free so it is fully unit-testable without hardware.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The runtime directory holding the PID and log files (`~/.rustscreen`).
pub fn runtime_dir(home: &str) -> PathBuf {
    Path::new(home).join(".rustscreen")
}

/// Path to the PID file (`~/.rustscreen/rustscreen.pid`).
pub fn pid_path(home: &str) -> PathBuf {
    runtime_dir(home).join("rustscreen.pid")
}

/// Path to the daemon log file (`~/.rustscreen/rustscreen.log`).
pub fn log_path(home: &str) -> PathBuf {
    runtime_dir(home).join("rustscreen.log")
}

/// Parse PID-file contents. Returns `None` for empty/malformed/non-positive input.
pub fn parse_pid(contents: &str) -> Option<i32> {
    contents.trim().parse::<i32>().ok().filter(|&p| p > 0)
}

/// Write `pid` to `path`, creating the parent directory if needed.
pub fn write_pid(path: &Path, pid: i32) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{pid}\n"))
}

/// Read a PID from `path`. `None` if the file is missing, empty, or malformed.
pub fn read_pid(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok().and_then(|s| parse_pid(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_live_under_dot_rustscreen_in_home() {
        assert_eq!(runtime_dir("/Users/me"), PathBuf::from("/Users/me/.rustscreen"));
        assert_eq!(
            pid_path("/Users/me"),
            PathBuf::from("/Users/me/.rustscreen/rustscreen.pid")
        );
        assert_eq!(
            log_path("/Users/me"),
            PathBuf::from("/Users/me/.rustscreen/rustscreen.log")
        );
    }

    #[test]
    fn parse_pid_accepts_clean_positive_and_rejects_junk() {
        assert_eq!(parse_pid("12345\n"), Some(12345));
        assert_eq!(parse_pid("  678  "), Some(678));
        assert_eq!(parse_pid(""), None);
        assert_eq!(parse_pid("not-a-pid"), None);
        assert_eq!(parse_pid("0"), None);
        assert_eq!(parse_pid("-4"), None);
    }

    #[test]
    fn write_then_read_pid_round_trips() {
        let dir = std::env::temp_dir().join(format!("rs-test-{}", std::process::id()));
        let path = dir.join("rustscreen.pid");
        write_pid(&path, 4242).unwrap();
        assert_eq!(read_pid(&path), Some(4242));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_pid_missing_file_is_none() {
        let path = std::env::temp_dir().join("definitely-absent-rs.pid");
        let _ = fs::remove_file(&path);
        assert_eq!(read_pid(&path), None);
    }
}
