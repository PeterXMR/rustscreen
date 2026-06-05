# rustscreen Terminal CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `rustscreen start` / `stop` / `status` terminal CLI that runs the capture→encode→send host as a detached background daemon, waits for the phone, auto-streams, and tears down cleanly — Roadmap Active-Priority 1.

**Architecture:** One binary (`rustscreen`) is both the controller you type and the worker it spawns. `start` preflights the Screen-Recording grant, spawns `rustscreen __serve` detached (own process group, stdio → log file), and records a PID file. `stop`/`status` act on that PID file. `__serve` runs the host pipeline — extracted from `p5_stream::main` into a reusable `serve::run_host(opts, &stop)` — with a wait-for-phone loop and a SIGTERM handler that flips a stop flag for clean teardown (RAII `Drop` removes the virtual display). The control plane (PID files, arg parsing, stale detection, messages) lives in a pure, cross-platform `daemon` module that is fully TDD'd cable-free; the objc2/VideoToolbox pipeline is verified on-device.

**Tech Stack:** Rust (macos-host crate, MSRV 1.80); existing objc2 / ScreenCaptureKit / VideoToolbox / nusb stack behind `live-capture` + `live-usb`; `objc2-core-graphics` for the permission preflight; a small gated `libc` dep for the signal handler + `kill(pid,0)` liveness check.

**Working directory:** the PR #25 worktree at `/Users/accountname/Documents/projects/rustscreen-pr25-cursor` (branch `feat/p5-cursor-on-external-display`). All commits land on PR #25.

---

## File Structure

| File | Responsibility | Tested |
|---|---|---|
| `crates/macos-host/src/daemon.rs` (create) | Pure control plane: runtime-dir/PID/log path resolution, PID file read/write/parse, stale-PID decision, subcommand parsing, permission-help text. Cross-platform, **no feature gates**. | host unit tests (TDD) |
| `crates/macos-host/src/serve.rs` (create) | `HostOpts` + `run_host(opts, &AtomicBool)` — the extracted pipeline + wait-for-phone loop + stop-flag teardown. Gated `#[cfg(all(feature = "live-capture", feature = "live-usb"))]`. | on-device |
| `crates/macos-host/src/bin/rustscreen.rs` (create) | CLI: dispatch, permission preflight, detached spawn, PID lifecycle, `__serve` entry. `required-features = ["live-capture","live-usb"]`. | on-device |
| `crates/macos-host/src/lib.rs` (modify) | Add `pub mod daemon;` and gated `pub mod serve;`. | — |
| `crates/macos-host/src/session.rs` (modify) | Add `stop: &AtomicBool` param to `run_stream_session_instrumented`, checked per iteration. | host unit tests |
| `crates/macos-host/src/bin/p5_stream.rs` (modify) | `main` becomes a thin wrapper over `serve::run_host`. | on-device (identical behavior) |
| `crates/macos-host/Cargo.toml` (modify) | Add gated `libc` dep; add `[[bin]] rustscreen`; add `libc` to both live features. | — |
| `README.md` (modify) | Install (`cargo install … rustscreen`) + `start`/`stop`/`status` usage; U3 permission note. | — |

---

## Task 1: `daemon` module scaffold + path resolution

**Files:**
- Create: `crates/macos-host/src/daemon.rs`
- Modify: `crates/macos-host/src/lib.rs` (add `pub mod daemon;` near the other unconditional `pub mod` lines, ~line 20)
- Test: inline `#[cfg(test)]` in `daemon.rs`

- [ ] **Step 1: Write the failing test**

In `crates/macos-host/src/daemon.rs`:

```rust
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
        assert_eq!(pid_path("/Users/me"), PathBuf::from("/Users/me/.rustscreen/rustscreen.pid"));
        assert_eq!(log_path("/Users/me"), PathBuf::from("/Users/me/.rustscreen/rustscreen.log"));
    }
}
```

Add to `crates/macos-host/src/lib.rs` (with the other unconditional modules):

```rust
pub mod daemon;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p macos-host daemon::tests::paths_live_under_dot_rustscreen_in_home`
Expected: FAIL to compile until the module is wired, then PASS once it is — if it compiles and passes immediately, that is also acceptable (the code above is complete). The point of this step is to confirm the test exists and exercises `runtime_dir`/`pid_path`/`log_path`.

- [ ] **Step 3: (implementation already shown in Step 1)**

No further code needed — Step 1 contains the complete implementation.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p macos-host daemon::`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/daemon.rs crates/macos-host/src/lib.rs
git commit -m "feat(cli): daemon runtime path resolution (TDD)"
```

---

## Task 2: PID-file write / read / parse round-trip

**Files:**
- Modify: `crates/macos-host/src/daemon.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test**

Add to `daemon.rs` (above the `tests` module):

```rust
use std::fs;
use std::io;

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
```

Add these tests inside the `tests` module:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p macos-host daemon::tests::parse_pid_accepts_clean_positive_and_rejects_junk`
Expected: PASS (code complete) — confirm all three new tests are present and green.

- [ ] **Step 3: (implementation shown in Step 1)**

- [ ] **Step 4: Run test to verify all daemon tests pass**

Run: `cargo test -p macos-host daemon::`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/daemon.rs
git commit -m "feat(cli): PID-file write/read/parse round-trip (TDD)"
```

---

## Task 3: Stale-PID decision (`start_state`)

**Files:**
- Modify: `crates/macos-host/src/daemon.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test**

Add to `daemon.rs`:

```rust
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
```

Add tests:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p macos-host daemon::tests::start_state_dead_pid_is_stale`
Expected: PASS (code complete) — confirm all three present.

- [ ] **Step 3: (implementation shown in Step 1)**

- [ ] **Step 4: Run daemon tests**

Run: `cargo test -p macos-host daemon::`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/daemon.rs
git commit -m "feat(cli): stale-PID start-state decision (TDD)"
```

---

## Task 4: Subcommand parsing

**Files:**
- Modify: `crates/macos-host/src/daemon.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test**

Add to `daemon.rs`:

```rust
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
```

Add tests:

```rust
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
        assert_eq!(parse_command(Some("frobnicate")), Command::Unknown("frobnicate".into()));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p macos-host daemon::tests::parse_command_maps_known_verbs`
Expected: PASS (code complete) — confirm all three present.

- [ ] **Step 3: (implementation shown in Step 1)**

- [ ] **Step 4: Run daemon tests**

Run: `cargo test -p macos-host daemon::`
Expected: PASS (10 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/daemon.rs
git commit -m "feat(cli): subcommand parsing (TDD)"
```

---

## Task 5: User-facing messages (permission help + usage)

**Files:**
- Modify: `crates/macos-host/src/daemon.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test**

Add to `daemon.rs`:

```rust
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
```

Add tests:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p macos-host daemon::tests::permission_help_names_the_exact_settings_path`
Expected: PASS (code complete) — confirm both present.

- [ ] **Step 3: (implementation shown in Step 1)**

- [ ] **Step 4: Full daemon + clippy check**

Run: `cargo test -p macos-host daemon:: && cargo clippy -p macos-host --lib -- -D warnings`
Expected: PASS (12 daemon tests); clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/daemon.rs
git commit -m "feat(cli): permission-help + usage messages (TDD)"
```

---

## Task 6: Thread an external stop flag into `run_stream_session_instrumented`

This is the one change to proven streaming code; it lets `__serve` break out of an *active* stream on SIGTERM (not just on phone-disconnect) so teardown runs.

**Files:**
- Modify: `crates/macos-host/src/session.rs` (`run_stream_session_instrumented`, ~line 447 + its loop)
- Modify: existing unit tests for that function in `session.rs`
- Test: existing `session.rs` tests, plus one new stop-flag test

- [ ] **Step 1: Write the failing test**

Find the existing `#[cfg(test)]` tests for `run_stream_session_instrumented` in `session.rs`. Add a test that passes an already-set stop flag and asserts the function returns promptly without consuming the whole input. Use the same fake `rx`/sink/`Write` harness the existing tests use. Skeleton (adapt names to the existing harness in the file):

```rust
    #[test]
    fn instrumented_session_returns_when_stop_flag_set() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = AtomicBool::new(true); // already requested
        // ...build the same fake rx / write sink / agreed config / stats_rx the
        // other run_stream_session_instrumented tests build...
        let result = run_stream_session_instrumented(
            rx, &mut sink, agreed, None, &stats_rx,
            &mut pipeline, hb, report_every, clock, |_r| {},
            &stop, // NEW trailing arg
        );
        assert!(result.is_ok());
        // The loop must observe stop before draining all frames:
        // assert that not every queued frame was written.
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p macos-host session::`
Expected: FAIL to compile — `run_stream_session_instrumented` doesn't take a `&AtomicBool` yet.

- [ ] **Step 3: Add the parameter and the check**

In `run_stream_session_instrumented` (line 447): add a final parameter `stop: &std::sync::atomic::AtomicBool`. At the **top of the per-frame loop body**, add:

```rust
    if stop.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
```

Update every existing call site and test of `run_stream_session_instrumented` to pass a stop flag. For existing tests that should run to completion, pass `&AtomicBool::new(false)`. (The only non-test caller is `p5_stream.rs`, which Task 7 rewrites — leave it until then or pass `&AtomicBool::new(false)` temporarily so the crate keeps compiling; Task 7 replaces it.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p macos-host session::`
Expected: PASS (existing tests + the new stop-flag test).

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/session.rs
git commit -m "feat(cli): external stop flag for instrumented stream session (TDD)"
```

---

## Task 7: Extract `serve::run_host`; make `p5_stream` a thin wrapper

Pure extract-function refactor of proven latency code + the two new behaviors (wait-for-phone loop, stop-flag teardown). **No per-frame hot-path logic changes.**

**Files:**
- Create: `crates/macos-host/src/serve.rs`
- Modify: `crates/macos-host/src/lib.rs` (add gated `pub mod serve;`)
- Modify: `crates/macos-host/src/bin/p5_stream.rs` (`main` → thin wrapper)

- [ ] **Step 1: Create `serve.rs` with the extracted body**

Create `crates/macos-host/src/serve.rs`:

```rust
//! The live host pipeline (`run_host`) — virtual display → ScreenCaptureKit capture →
//! VideoToolbox H.264 → AOA transport → phone — extracted from the `p5_stream` spike so
//! both `p5_stream` and the `rustscreen` daemon drive the identical code path.
#![cfg(all(feature = "live-capture", feature = "live-usb"))]

use std::sync::atomic::AtomicBool;

/// Host geometry/rate. Defaults to the locked 2400×1080@60 virtual display (D6).
pub struct HostOpts {
    pub width: usize,
    pub height: usize,
    pub fps: u32,
}

impl Default for HostOpts {
    fn default() -> Self {
        Self { width: 2400, height: 1080, fps: 60 }
    }
}

/// Run the host until the phone disconnects or `stop` is set. On return (any path),
/// the virtual display has been dropped so the Mac desktop reflows.
pub fn run_host(opts: &HostOpts, stop: &AtomicBool) -> std::io::Result<()> {
    // <body moved verbatim from p5_stream::main, lines ~332..end, with the
    //  transformations in Step 2 applied>
    todo!("filled in Step 2")
}
```

Add to `lib.rs` (near the other gated module, after the `aoa` block ~line 25):

```rust
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
pub mod serve;
```

- [ ] **Step 2: Move the pipeline body and apply the transformations**

Move the entire body of `p5_stream::main` (the big `main` at line 331, lines ~332 through the end of teardown) into `run_host`. Also move the file-local helpers it uses — the capture delegate `define_class!`, `now_us`, `h264_params`, `block_buffer_bytes`, `print_report`, and `bring_up_aoa` — into `serve.rs` (or a private submodule), since `run_host` now owns them. Then apply exactly these transformations:

1. **Geometry from `opts`:** replace `const W: usize = 2400; const H: usize = 1080;` with `let (W, H) = (opts.width, opts.height);` (keep the `setMinimumFrameInterval(CMTime::new(1, 60))` tied to `opts.fps`: `CMTime::new(1, opts.fps as i64)`).

2. **`std::process::exit(n)` → `return Err(...)`** for every one (exits 1–8). Each becomes:
   ```rust
   return Err(std::io::Error::other(format!("…the existing eprintln! text…")));
   ```
   (`std::io::Error::other` is stable since 1.74 ≥ MSRV 1.80.) Keep the message text identical so logs are unchanged.

3. **Wait-for-phone loop** around `bring_up_aoa()`. Replace the single `let transport = bring_up_aoa()?;` (and its exit-on-error) with:
   ```rust
   let transport = loop {
       if stop.load(std::sync::atomic::Ordering::Relaxed) {
           drop(vdisplay); // dropped explicitly on the early-out path
           return Ok(());
       }
       match bring_up_aoa() {
           Ok(t) => break t,
           Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
       }
   };
   ```
   (Note: `vdisplay` is created earlier; on the normal path it is dropped at teardown as today.)

4. **Stop-flag into the stream loop:** pass `stop` as the new trailing arg to `session::run_stream_session_instrumented(...)` added in Task 6.

5. **Return `Ok(())`** at the end (after the existing teardown: stop capture, `drop(vdisplay)`, `drop(write_half)`, bounded reader join).

Keep everything else byte-for-byte: the VT session setup, the encoder spec, the delegate, clock-sync, the report cadence.

- [ ] **Step 3: Rewrite `p5_stream::main` as a thin wrapper**

Replace the entire big `main` (line 331) and the now-moved helpers in `p5_stream.rs` with:

```rust
fn main() {
    // The spike entry point now just drives the shared host pipeline with a stop
    // flag that is never set here (Ctrl-C kills the foreground process as before).
    let stop = std::sync::atomic::AtomicBool::new(false);
    if let Err(e) = macos_host::serve::run_host(&macos_host::serve::HostOpts::default(), &stop) {
        eprintln!("p5_stream: {e}");
        std::process::exit(1);
    }
}
```

Remove from `p5_stream.rs` every `use`/helper now living in `serve.rs` so there are **no unused imports** (clippy/CLAUDE.md commit-hygiene rule). Keep the file's `//!` doc header.

- [ ] **Step 4: Build under the live features**

Run: `cargo build -p macos-host --features live-capture,live-usb --bin p5_stream`
Expected: compiles clean. Then:
Run: `cargo clippy -p macos-host --features live-capture,live-usb --bin p5_stream -- -D warnings`
Expected: no warnings, no unused imports.

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/serve.rs crates/macos-host/src/lib.rs crates/macos-host/src/bin/p5_stream.rs
git commit -m "refactor(cli): extract serve::run_host; p5_stream becomes a thin wrapper"
```

---

## Task 8: `libc` dep + `rustscreen` bin wiring

**Files:**
- Modify: `crates/macos-host/Cargo.toml`
- Create: `crates/macos-host/src/bin/rustscreen.rs`

- [ ] **Step 1: Add `libc` and the bin to `Cargo.toml`**

In `[dependencies]`:

```toml
libc = { version = "0.2", optional = true }
```

Add `"dep:libc"` to **both** the `live-usb` and `live-capture` feature lists (so any build that compiles the daemon worker pulls it; the bin requires both anyway):

```toml
live-usb = ["dep:nusb", "dep:libc"]
# ...and append "dep:libc" to the live-capture = [ ... ] array.
```

Add the bin:

```toml
# rustscreen terminal CLI (Active-Priority 1) — controller + detached worker.
[[bin]]
name = "rustscreen"
required-features = ["live-capture", "live-usb"]
```

- [ ] **Step 2: Write `rustscreen.rs`**

Create `crates/macos-host/src/bin/rustscreen.rs`:

```rust
//! `rustscreen` — install-once terminal control for the USB-C second-monitor host
//! (Roadmap Active-Priority 1). `start` spawns a detached worker; `stop`/`status`
//! act on its PID file; `__serve` is the hidden worker that runs the host pipeline.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use macos_host::daemon::{self, Command as Cmd, StartState};
use macos_host::serve::{run_host, HostOpts};

/// Worker stop flag, flipped by the SIGTERM/SIGINT handler.
static STOP: AtomicBool = AtomicBool::new(false);
static HOME: OnceLock<String> = OnceLock::new();

fn home() -> &'static str {
    HOME.get_or_init(|| std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

/// Liveness via `kill(pid, 0)`: Ok or EPERM ⇒ alive; ESRCH ⇒ gone.
fn process_alive(pid: i32) -> bool {
    // SAFETY: kill with signal 0 performs only permission/existence checks.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

extern "C" fn on_term(_sig: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    // SAFETY: installing a trivial async-signal-safe handler (atomic store only).
    unsafe {
        libc::signal(libc::SIGTERM, on_term as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as libc::sighandler_t);
    }
}

fn main() {
    let arg = std::env::args().nth(1);
    match daemon::parse_command(arg.as_deref()) {
        Cmd::Start => cmd_start(),
        Cmd::Stop => cmd_stop(),
        Cmd::Status => cmd_status(),
        Cmd::Serve => cmd_serve(),
        Cmd::Help => println!("{}", daemon::usage()),
        Cmd::Unknown(other) => {
            eprintln!("rustscreen: unknown command '{other}'\n");
            eprintln!("{}", daemon::usage());
            std::process::exit(2);
        }
    }
}

fn cmd_start() {
    // 1. Permission preflight (U3) — fail loudly at the prompt, never a silent black screen.
    let granted = unsafe { objc2_core_graphics::CGPreflightScreenCaptureAccess() };
    if !granted {
        unsafe { objc2_core_graphics::CGRequestScreenCaptureAccess() };
        eprintln!("{}", daemon::permission_help());
        std::process::exit(1);
    }

    // 2. Already running?
    let pid_path = daemon::pid_path(home());
    match daemon::start_state(daemon::read_pid(&pid_path), process_alive) {
        StartState::Running(pid) => {
            println!("rustscreen: already running (pid {pid}). Use `rustscreen stop` first.");
            return;
        }
        StartState::Stale(pid) => {
            eprintln!("rustscreen: clearing stale pid {pid}.");
        }
        StartState::NotRunning => {}
    }

    // 3. Spawn the detached worker (own process group; stdio → log file).
    let log_path = daemon::log_path(home());
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("open log file");
    let exe = std::env::current_exe().expect("current_exe");
    use std::os::unix::process::CommandExt;
    let child = Command::new(exe)
        .arg("__serve")
        .stdin(Stdio::null())
        .stdout(log.try_clone().expect("clone log"))
        .stderr(log)
        .process_group(0) // detach from the terminal's process group (no SIGHUP on close)
        .spawn()
        .expect("spawn worker");

    daemon::write_pid(&pid_path, child.id() as i32).expect("write pid");
    println!(
        "rustscreen: host running (pid {}). Waiting for the phone — open the app to start streaming.",
        child.id()
    );
    println!("rustscreen: logs → {}", log_path.display());
}

fn cmd_stop() {
    let pid_path = daemon::pid_path(home());
    match daemon::read_pid(&pid_path) {
        Some(pid) if process_alive(pid) => {
            // SAFETY: sending SIGTERM to our own daemon pid.
            unsafe { libc::kill(pid, libc::SIGTERM) };
            // Brief wait for graceful teardown.
            for _ in 0..50 {
                if !process_alive(pid) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = std::fs::remove_file(&pid_path);
            println!("rustscreen: stopped (pid {pid}).");
        }
        Some(pid) => {
            let _ = std::fs::remove_file(&pid_path);
            println!("rustscreen: not running (cleared stale pid {pid}).");
        }
        None => println!("rustscreen: not running."),
    }
}

fn cmd_status() {
    let pid_path = daemon::pid_path(home());
    match daemon::read_pid(&pid_path) {
        Some(pid) if process_alive(pid) => println!("rustscreen: running (pid {pid})."),
        Some(_) | None => println!("rustscreen: not running."),
    }
}

fn cmd_serve() {
    install_signal_handlers();
    println!("rustscreen[serve]: starting host pipeline.");
    if let Err(e) = run_host(&HostOpts::default(), &STOP) {
        eprintln!("rustscreen[serve]: host exited with error: {e}");
        std::process::exit(1);
    }
    println!("rustscreen[serve]: clean shutdown.");
}
```

- [ ] **Step 3: Build the bin**

Run: `cargo build -p macos-host --features live-capture,live-usb --bin rustscreen`
Expected: compiles clean.

- [ ] **Step 4: Clippy + the full daemon test suite**

Run: `cargo clippy -p macos-host --features live-capture,live-usb --bin rustscreen -- -D warnings && cargo test -p macos-host daemon::`
Expected: no warnings; 12 daemon tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/Cargo.toml crates/macos-host/src/bin/rustscreen.rs
git commit -m "feat(cli): rustscreen start/stop/status + detached worker + permission preflight"
```

---

## Task 9: README install + usage

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add an install + usage section**

Add (or update the existing run/usage section of) `README.md` with:

````markdown
## Install & run (macOS host)

Install the host CLI once (release build — debug worsens latency):

```bash
cargo install --path crates/macos-host --bin rustscreen --features live-capture,live-usb
```

Then:

```bash
rustscreen start    # runs the host in the background; streams when the phone app opens
rustscreen status   # is it running?
rustscreen stop     # stop and remove the virtual display
```

The first `start` requires the macOS **Screen & System Audio Recording** permission
(System Settings ▸ Privacy & Security). `rustscreen start` checks for it and tells you
exactly what to do if it's missing. Logs go to `~/.rustscreen/rustscreen.log`.
````

- [ ] **Step 2: Verify the doc renders / links**

Run: `grep -n "rustscreen start" README.md`
Expected: the new usage lines are present.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(cli): rustscreen install + start/stop/status usage"
```

---

## Task 10: On-device verification

**Files:** none (verification).

- [ ] **Step 1: Confirm the workspace is green (cable-free)**

Run: `cargo test -p macos-host daemon:: && cargo fmt --check && cargo clippy -p macos-host --lib -- -D warnings`
Expected: all green.

- [ ] **Step 2: Install the release binary**

Run: `cargo install --path crates/macos-host --bin rustscreen --features live-capture,live-usb --locked`
Expected: `rustscreen` installed to `~/.cargo/bin`.

- [ ] **Step 3: Permission-missing path (if testable)**

If a process without the grant is available, run `rustscreen start` from it and confirm it prints the actionable message and exits non-zero (no detached spawn, no PID file).

- [ ] **Step 4: Happy path on device (use the `run-on-device` skill context)**

With the Pixel 6a connected:
1. `rustscreen start` → returns the prompt, prints the pid + log path.
2. `rustscreen status` → "running (pid …)".
3. Open the phone app → confirm the desktop streams (drag a window onto the external display per U1).
4. Read the on-device per-stage + glass-to-glass latency report from `~/.rustscreen/rustscreen.log`; confirm numbers match the `p5_stream` baseline (~80 ms p50) — proves the extraction is behavior-identical.
5. `rustscreen stop` → the host exits within ~1–2 s, the virtual display disappears (Mac desktop reflows), PID file removed, `rustscreen status` → "not running".

- [ ] **Step 5: Wait-for-phone + restart cycle**

1. `rustscreen stop` (if running). 2. Unplug the phone. 3. `rustscreen start` → idles waiting. 4. Plug the phone in + open the app → streaming begins on its own. 5. `rustscreen stop`. Confirm no leaked process (`rustscreen status`) and the log shows the wait loop then a clean shutdown.

- [ ] **Step 6: Final commit / notes**

Record the measured numbers + verification result in `.planning/STATE.md` (Last activity line) and commit:

```bash
git add .planning/STATE.md
git commit -m "docs(state): rustscreen CLI verified on device (Priority 1)"
```

---

## Self-Review notes

- **Spec coverage:** start/stop/status (Tasks 4,8), permission preflight U3 (Tasks 5,8), detached spawn + process group (Task 8), PID lifecycle + stale handling (Tasks 2,3,8), wait-for-phone single (Task 7), clean SIGTERM teardown (Tasks 6,7,8), `run_host` extraction + p5_stream wrapper (Task 7), release install (Tasks 9,10), control-plane TDD (Tasks 1–5), on-device verify (Task 10). U2 (one-click launch) = the CLI itself. ✓ All spec sections mapped.
- **Type consistency:** `Command`/`StartState`/`HostOpts`/`run_host(opts,&AtomicBool)`/`process_alive`/`parse_command`/`start_state` names are identical across Tasks 1–8. ✓
- **No placeholders:** the only `todo!()` is a scaffold in Task 7 Step 1 explicitly filled in Step 2 (verbatim move). ✓
- **Latency:** no per-frame hot-path change; release-build install is the one latency-positive item. ✓
