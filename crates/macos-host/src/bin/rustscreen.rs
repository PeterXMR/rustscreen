//! `rustscreen` — install-once terminal control for the USB-C second-monitor host
//! (Roadmap Active-Priority 1).
//!
//! - `rustscreen start`  — preflight the Screen-Recording grant, then spawn a **detached**
//!   worker (`__serve`) in its own process group with stdio → `~/.rustscreen/rustscreen.log`,
//!   record its PID, and return the prompt. The worker waits for the phone and auto-streams.
//! - `rustscreen stop`   — SIGTERM the recorded worker; it tears down the virtual display cleanly.
//! - `rustscreen status` — report whether the worker is running.
//! - `rustscreen __serve` — hidden worker entry: install a SIGTERM/SIGINT handler and run the
//!   host pipeline ([`macos_host::serve::run_host`]) until the phone disconnects or stop is set.
//!
//! Built only with `--features live-capture,live-usb` (it drives the full host pipeline). Install:
//! `cargo install --path crates/macos-host --bin rustscreen --features live-capture,live-usb`.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use macos_host::daemon::{self, Command as Cmd, StartState};
use macos_host::serve::{run_host, HostOpts};

/// Worker stop flag, flipped by the SIGTERM/SIGINT handler. Standalone flag → `Relaxed`.
static STOP: AtomicBool = AtomicBool::new(false);
static HOME: OnceLock<String> = OnceLock::new();

fn home() -> &'static str {
    HOME.get_or_init(|| std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

/// Liveness via `kill(pid, 0)`: success or `EPERM` ⇒ the process exists; `ESRCH` ⇒ it is gone.
fn process_alive(pid: i32) -> bool {
    // SAFETY: `kill` with signal 0 performs only permission/existence checks, sends no signal.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

extern "C" fn on_term(_sig: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    // SAFETY: the handler is async-signal-safe (an atomic store only).
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
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
    let granted = objc2_core_graphics::CGPreflightScreenCaptureAccess();
    if !granted {
        // Trigger the system prompt so the user can grant it immediately.
        objc2_core_graphics::CGRequestScreenCaptureAccess();
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
    let log = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("rustscreen: cannot open log {}: {e}", log_path.display());
            std::process::exit(1);
        }
    };
    let exe = std::env::current_exe().expect("current_exe");
    use std::os::unix::process::CommandExt;
    let child = match Command::new(exe)
        .arg("__serve")
        .stdin(Stdio::null())
        .stdout(log.try_clone().expect("clone log"))
        .stderr(log)
        .process_group(0) // detach from the terminal's process group (no SIGHUP on close)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rustscreen: failed to spawn worker: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = daemon::write_pid(&pid_path, child.id() as i32) {
        eprintln!("rustscreen: warn — could not write pid file: {e}");
    }
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
            // SAFETY: sending SIGTERM to our own daemon pid for graceful teardown.
            unsafe { libc::kill(pid, libc::SIGTERM) };
            // Grace period (~3s) for clean teardown (display drop + USB close).
            // On macOS `libc::signal` uses BSD/SA_RESTART semantics, so SIGTERM does
            // NOT interrupt a blocking USB write/flush — the worker may not observe the
            // stop flag until that syscall returns. If it never exits, escalate below.
            for _ in 0..30 {
                if !process_alive(pid) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if process_alive(pid) {
                // SIGKILL cannot be caught/restarted, so it always wins. RAII teardown is
                // skipped, but the CGVirtualDisplay is owned by the process and is removed
                // on exit, so the desktop still reflows — safe.
                println!("rustscreen: worker {pid} didn't exit on SIGTERM; sending SIGKILL.");
                // SAFETY: sending SIGKILL to our own daemon pid to force exit.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                for _ in 0..10 {
                    if !process_alive(pid) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
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
