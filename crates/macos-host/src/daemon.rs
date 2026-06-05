//! Pure control plane for the `rustscreen` daemon: runtime paths, PID-file
//! lifecycle, stale-PID decisions, subcommand parsing, and user-facing messages.
//! Cross-platform and feature-free so it is fully unit-testable without hardware.

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
}
