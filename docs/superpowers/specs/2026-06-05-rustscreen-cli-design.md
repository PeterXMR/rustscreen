# Design: `rustscreen` terminal CLI (Roadmap Priority 1)

**Date:** 2026-06-05
**Status:** Approved (brainstorm)
**Roadmap:** `.planning/ROADMAP.md` → Active Priorities → **Priority 1 — `rustscreen` terminal app: install once, drive it with `start` / `stop`**
**PR:** #25 (`feat/p5-cursor-on-external-display`)

## Goal

Install the host on the Mac once, then control it entirely from the terminal:

- `rustscreen start` — launch the capture→encode→send host as a **background** process and return
  the prompt. The moment the phone app is open, streaming begins on its own — no extra steps.
- `rustscreen stop` — terminate the running host and tear down the virtual display cleanly so the
  Mac desktop reflows.

This reframes the old menu-bar app (ladder item 7) into a **terminal-first** interface and folds in
usability gaps **U2** (one-click launch) and **U3** (Screen-Recording permission clarity).

**Done when:** from a fresh terminal, `rustscreen start` turns the phone into a second screen with no
other steps, and `rustscreen stop` cleanly ends it.

## Scope

**In scope (full Priority 1):** the `rustscreen` CLI (`start` / `stop` / `status`), a release install
path, Screen-Recording permission preflight (U3), a **single** wait-for-phone-then-stream, and clean
teardown on stop.

**Out of scope (deferred to Priority 2):** the *repeating* auto-reconnect supervisor loop. This design
builds the wait-for-phone as a reusable primitive so Priority 2 is a small follow-up, not a rewrite.

**Out of scope (Priority 3):** latency levers (non-blocking USB writes, RT thread scheduling, decoder
hints). The one latency-adjacent item folded in is documenting/installing a **release** build (Priority 1
sub-task), since debug materially worsens encode/copy latency.

## Architecture — one binary, controller + worker

A new bin `rustscreen` (`crates/macos-host/src/bin/rustscreen.rs`), with the same
`required-features = ["live-capture", "live-usb"]` as `p5_stream`. It is both the controller you type
and the background worker it spawns. Subcommand dispatch is a hand-rolled `match` on `args().nth(1)` —
no `clap` dependency (keeps the crate's minimal-deps house style).

### `rustscreen start` (controller — returns the prompt)

1. **Preflight the Screen-Recording grant (U3)** *before anything else* via
   `CGPreflightScreenCaptureAccess()` (non-prompting). If the grant is missing, print an actionable
   message — the exact **System Settings ▸ Privacy & Security ▸ Screen & System Audio Recording** path,
   and trigger the system prompt via `CGRequestScreenCaptureAccess()` — then exit non-zero. This surfaces
   at *your* terminal, never as a silent black screen buried in a log.
2. If a host is already running (a live PID file), report it and exit cleanly.
3. Spawn `rustscreen __serve` **detached**: its own process group
   (`std::os::unix::process::CommandExt::process_group(0)` — pure std, so closing the terminal's SIGHUP
   doesn't reach it), with stdio redirected to `~/.rustscreen/rustscreen.log`. Do not wait on it.
4. Write the child PID to `~/.rustscreen/rustscreen.pid`, print `host running, waiting for phone…`, and
   return the prompt.

### `rustscreen stop`

Read the PID file → send `SIGTERM` → brief wait for exit → remove the PID file → report. Idempotent:
"not running" is a clean message, not an error.

### `rustscreen status`

Report running/stopped plus the PID. ~10 lines reusing the same PID-alive check; the way you confirm the
detached daemon actually came up.

### `rustscreen __serve` (hidden worker)

Runs the extracted host pipeline (`run_host`) with the **wait-for-phone loop** and a **SIGTERM/SIGINT
handler** for clean teardown.

## The prerequisite refactor — extract `run_host`

The ~500-line pipeline currently inside `p5_stream.rs::main` moves into a library function:

```rust
// crates/macos-host/src/serve.rs  (gated on both live features)
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
pub fn run_host(opts: &HostOpts, stop: &AtomicBool) -> std::io::Result<()>;
```

- `p5_stream.rs::main` becomes a thin wrapper that calls `run_host` (the spike keeps working unchanged —
  the `run-on-device` skill still invokes `p5_stream` — and there is **zero code duplication**).
- `rustscreen __serve` calls the same `run_host`.

This is a pure **extract-function** refactor of proven latency-critical code, so `p5_stream` must behave
**identically** afterward and be **verified on-device**. No per-frame hot-path logic changes.

## The two new behaviors

### Wait-for-phone (single, reusable)

Today `bring_up_aoa()` tries once and `std::process::exit`s on failure. The worker wraps it in a loop:
retry every ~200 ms until the AOA accessory appears **or** the stop flag is set, then auto-start
streaming. This same primitive becomes Priority 2's supervisor loop (wrap the whole
bring-up→handshake→stream cycle and re-arm on disconnect).

### Clean teardown

The worker installs a `SIGTERM`/`SIGINT` handler that flips a `static AtomicBool` stop flag
(`Ordering::Relaxed` — a standalone flag with no companion memory to publish, per the latency rules in
CLAUDE.md). The wait loop and stream loop poll it and exit; the `VirtualDisplay`'s RAII `Drop` then tears
down the phantom display so the Mac desktop reflows. A tiny `libc` dependency (gated to the live
features) provides the signal handler.

## Files & runtime layout

- `~/.rustscreen/` — created on `start`; holds `rustscreen.pid` and `rustscreen.log`.
- **Stale-PID handling:** if the PID file points at a dead process (`kill(pid, 0)` returns `ESRCH`),
  treat it as stale and overwrite rather than refusing to start.
- **Install (release, not debug):** documented in the README —
  `cargo install --path crates/macos-host --bin rustscreen --features live-capture,live-usb`
  (release by default) puts `rustscreen` on `PATH`.

## Error handling

| Condition | Behavior |
|---|---|
| Screen-Recording grant missing | `start` prints actionable System-Settings path + triggers prompt, exits non-zero (no detached spawn) |
| Already running (live PID) | `start` reports the running PID and exits 0 |
| Stale PID file (dead process) | `start` overwrites and proceeds |
| `stop` with no/dead PID | Clean "not running" message, exit 0 |
| Worker can't create virtual display / find SCDisplay / VT session | Logged to `rustscreen.log`, worker exits non-zero; PID file left for `stop`/`status` to clean (next `start` detects the dead PID as stale) |
| Phone never appears | Worker idles in the wait loop until `stop` (SIGTERM) |

## Testing — TDD the control plane, on-device for the pipeline

The objc2 / VideoToolbox path can't be unit-tested cable-free, but the **control plane can** and will be
TDD'd in a pure module behind a thin filesystem seam (`crates/macos-host/src/daemon.rs`, cross-platform,
no features):

- PID-file write / read / parse (round-trip; malformed file → treated as absent).
- Stale-PID detection logic (`process_alive(pid)` seam; pure decision function tested with a fake).
- Subcommand arg dispatch (`start` / `stop` / `status` / `__serve` / unknown → usage).
- Actionable permission-message formatting (exact, asserted string).
- Runtime-dir / PID-path resolution from `$HOME`.

The `run_host` extraction + the two new behaviors (wait-for-phone, SIGTERM teardown) are verified on the
connected Pixel 6a via the `run-on-device` skill. This mirrors the project's established
cable-free-TDD + on-device-verify split.

## Latency note (prime directive)

Nothing here touches the per-frame hot path — `run_host` is byte-for-byte the existing pipeline. The one
latency-positive change folded in is documenting/installing a **release** build (Priority 1 sub-task).
Priority 3's latency levers stay out of scope.

## Follow-ups enabled (not in this PR)

- **Priority 2 (auto-reconnect):** wrap `run_host`'s bring-up→handshake→stream in the supervisor loop;
  re-arm on disconnect; keep the virtual display alive across reconnects.
- **P8 packaging:** the `rustscreen` bin is the thing a Homebrew formula / signed `.app` will wrap.
