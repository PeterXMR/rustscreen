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

/// What `start` should do given a recorded PID (if any) and a liveness check.
#[derive(Debug, PartialEq, Eq)]
pub enum StartState {
    /// No PID file, or its process is gone — safe to spawn.
    NotRunning,
    /// A live daemon already owns this PID — refuse to double-start.
    Running(i32),
    /// PID file exists but its process is dead — overwrite and proceed.
    Stale(i32),
}

/// Decide `start` behavior. `alive(pid)` is the liveness seam (real impl: `kill(pid,0)`).
pub fn start_state(recorded: Option<i32>, alive: impl Fn(i32) -> bool) -> StartState {
    match recorded {
        None => StartState::NotRunning,
        Some(pid) if alive(pid) => StartState::Running(pid),
        Some(pid) => StartState::Stale(pid),
    }
}

/// A parsed CLI subcommand. `Serve` is the hidden worker entry point.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
    Status,
    /// Hidden: the detached worker process (`rustscreen __serve`).
    Serve,
    /// No subcommand or `help`/`-h`/`--help` → print usage.
    Help,
    /// Anything else → print usage to stderr, exit non-zero.
    Unknown(String),
}

/// Parse the first CLI argument into a [`Command`].
pub fn parse_command(arg: Option<&str>) -> Command {
    match arg {
        None | Some("help") | Some("-h") | Some("--help") => Command::Help,
        Some("start") => Command::Start,
        Some("stop") => Command::Stop,
        Some("status") => Command::Status,
        Some("__serve") => Command::Serve,
        Some(other) => Command::Unknown(other.to_string()),
    }
}

/// Actionable message shown when the Screen & System Audio Recording grant is missing (U3).
pub fn permission_help() -> String {
    "\
rustscreen needs the macOS Screen & System Audio Recording permission to \
capture your display.\n\
Grant it in: System Settings \u{25b8} Privacy & Security \u{25b8} Screen & System Audio Recording\n\
Enable the terminal app you launched rustscreen from, then run `rustscreen start` again.\n\
A permission prompt has been requested now; if you don't see it, add the app manually."
        .to_string()
}

/// CLI usage text.
pub fn usage() -> String {
    "\
rustscreen \u{2014} turn your phone into a USB-C second monitor for your Mac\n\n\
USAGE:\n\
\u{20}   rustscreen start     Start the host in the background; streams when the phone app opens\n\
\u{20}   rustscreen stop      Stop the host and remove the virtual display\n\
\u{20}   rustscreen status    Show whether the host is running\n"
        .to_string()
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

    #[test]
    fn start_state_no_pid_is_not_running() {
        assert_eq!(start_state(None, |_| true), StartState::NotRunning);
    }

    #[test]
    fn start_state_live_pid_is_running() {
        assert_eq!(start_state(Some(99), |p| p == 99), StartState::Running(99));
    }

    #[test]
    fn start_state_dead_pid_is_stale() {
        assert_eq!(start_state(Some(99), |_| false), StartState::Stale(99));
    }

    #[test]
    fn parse_command_maps_known_verbs() {
        assert_eq!(parse_command(Some("start")), Command::Start);
        assert_eq!(parse_command(Some("stop")), Command::Stop);
        assert_eq!(parse_command(Some("status")), Command::Status);
        assert_eq!(parse_command(Some("__serve")), Command::Serve);
    }

    #[test]
    fn parse_command_no_arg_or_help_is_help() {
        assert_eq!(parse_command(None), Command::Help);
        assert_eq!(parse_command(Some("--help")), Command::Help);
    }

    #[test]
    fn parse_command_unknown_is_reported() {
        assert_eq!(
            parse_command(Some("frobnicate")),
            Command::Unknown("frobnicate".into())
        );
    }

    #[test]
    fn permission_help_names_the_exact_settings_path() {
        let msg = permission_help();
        assert!(msg.contains("Screen & System Audio Recording"));
        assert!(msg.contains("System Settings"));
        assert!(msg.contains("Privacy & Security"));
    }

    #[test]
    fn usage_lists_the_three_verbs() {
        let u = usage();
        assert!(u.contains("start"));
        assert!(u.contains("stop"));
        assert!(u.contains("status"));
    }
}
