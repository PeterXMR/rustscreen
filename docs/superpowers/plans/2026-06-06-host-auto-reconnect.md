# Host Auto-Reconnect Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the host self-heal across phone disconnects — an unplug/replug or sleep/wake auto-recovers in ~1–2 s with a clean image, keeping the virtual display and capture/encoder warm (no desktop reflow, no cold start) until `rustscreen stop`.

**Architecture:** Restructure `serve::run_host` from a single-connection linear flow into **warm setup (once) → reconnect `loop` → single final teardown (once)**. The virtual display + ScreenCaptureKit capture + VideoToolbox session + bounded `EncodedFrame` channel are built once before the loop; each loop iteration waits for the phone, forces a keyframe + drains stale frames, then runs one connection (handshake → clock-sync → stream) until it ends. A `stop` flag breaks the loop into the single teardown; a plain disconnect loops back. The reconnect logic is implemented **inline** (not as an extracted `run_one_connection` fn) so the many `objc2` retained objects stay in one scope and don't have to be threaded through a borrow-heavy signature — the spec's `wait_for_phone`/`run_one_connection`/`ConnEnd` names map to an inline wait-loop + an inline per-connection block + a tested `ConnEnd` decision helper.

**Tech Stack:** Rust, `nusb` (AOA/USB), `objc2` ScreenCaptureKit/VideoToolbox/CoreGraphics, `std::sync::mpsc`. The host pipeline lives behind the `live-capture` + `live-usb` cargo features (macOS-only; built and run on the connected M1 + Pixel 6a).

---

## File Structure

- **`crates/macos-host/src/session.rs`** — `run_stream_session_instrumented` changes from consuming `frames: Receiver<EncodedFrame>` to borrowing `frames: &Receiver<EncodedFrame>`, so the warm channel survives across reconnects. Unconditional module → its unit tests run in default cross-platform CI.
- **`crates/macos-host/src/serve.rs`** — the reconnect restructure of `run_host`, plus three small pure helpers (`ConnEnd`, `classify_connection_end`, `drain_frames`) with unit tests. Gated behind `live-capture` + `live-usb` → built/tested on the Mac with `--features live-capture,live-usb`.
- **`crates/macos-host/src/bin/p5_stream.rs`** — left untouched (legacy spike; the `rustscreen` daemon uses `serve::run_host`).

**Per-task build/test commands:**
- Default (cross-platform, covers Task 1): `cargo test -p macos-host`
- Live (macOS, covers Tasks 2–3): `cargo test -p macos-host --features live-capture,live-usb` and `cargo build -p macos-host --features live-capture,live-usb`

---

## Task 1: Borrow the frame receiver in `run_stream_session_instrumented`

Make the stream function borrow its frame receiver so one warm channel can drive many reconnects. The body already uses `frames` only via `recv_timeout(&self)` / `try_recv(&self)`, so only the signature and call sites change.

**Files:**
- Modify: `crates/macos-host/src/session.rs:448` (signature) and its 6 in-file test call sites (`:1050`, `:1113`, `:1276`, `:1358`, `:1421`, `:1496`)
- Test: `crates/macos-host/src/session.rs` (`#[cfg(test)] mod` already present)

- [ ] **Step 1: Write the failing test (proves the receiver is reusable)**

Add this test inside the existing `#[cfg(test)] mod tests { ... }` in `session.rs`. It calls the function twice with the **same** receiver — which only compiles if the parameter is `&Receiver`, not an owned `Receiver`.

```rust
#[test]
fn instrumented_borrows_receiver_so_it_is_reusable_across_connections() {
    // Drop the sender up front: with no live sender, each call sees `Disconnected`
    // immediately and returns a zero-frame summary. The point of the test is that
    // `rx` is still usable for the SECOND call — i.e. the param is borrowed, not moved.
    let (tx, rx) = std::sync::mpsc::sync_channel::<EncodedFrame>(2);
    drop(tx);
    let (_stats_tx, stats_rx) = std::sync::mpsc::channel();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let now = || 0u64;

    let mut pipeline = PipelineLatency::new(16);
    let mut sink: Vec<u8> = Vec::new();
    let s1 = run_stream_session_instrumented(
        &rx, &mut sink, default_agreed(), None, &stats_rx, &mut pipeline,
        std::time::Duration::from_millis(1), std::time::Duration::from_secs(3600),
        now, |_r| {}, &stop,
    )
    .unwrap();

    let mut pipeline2 = PipelineLatency::new(16);
    let mut sink2: Vec<u8> = Vec::new();
    let s2 = run_stream_session_instrumented(
        &rx, &mut sink2, default_agreed(), None, &stats_rx, &mut pipeline2,
        std::time::Duration::from_millis(1), std::time::Duration::from_secs(3600),
        now, |_r| {}, &stop,
    )
    .unwrap();

    assert_eq!(s1.frames, 0, "no sender → zero frames");
    assert_eq!(s2.frames, 0, "receiver reusable for a second connection");
}
```

- [ ] **Step 2: Run the test to verify it fails (compile error)**

Run: `cargo test -p macos-host instrumented_borrows_receiver -- --nocapture`
Expected: FAIL — compile error `use of moved value: rx` (the current by-value signature moves `rx` on the first call, so the second call can't compile).

- [ ] **Step 3: Change the signature to borrow, and update the 5 existing in-file callers**

In `session.rs:448`, change:

```rust
    frames: std::sync::mpsc::Receiver<EncodedFrame>,
```
to:
```rust
    frames: &std::sync::mpsc::Receiver<EncodedFrame>,
```

The body is unchanged (`frames.recv_timeout(...)` and `frames.try_recv()` already take `&self`).

Then at each of the 5 existing test call sites (`:1050`, `:1113`, `:1276`, `:1358`, `:1421`, `:1496` — note `:1496` may be a sixth), change the first argument from `rx` to `&rx`. Example (site at ~`:1050`):

```rust
        let summary = run_stream_session_instrumented(
            &rx,            // was: rx
            &mut sink,
            default_agreed(),
            ...
```

If any test used `rx` again *after* the call (it relied on the move ending its borrow), that still works — `&rx` borrows end at the call. No other change needed.

- [ ] **Step 4: Run the full session test suite to verify it passes**

Run: `cargo test -p macos-host`
Expected: PASS — all existing `run_stream_session_instrumented` tests plus the new reuse test.

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/session.rs
git commit -m "refactor(session): borrow frame receiver in run_stream_session_instrumented

So one warm EncodedFrame channel can drive multiple reconnect cycles instead of
being consumed by the first connection. Body unchanged (recv_timeout/try_recv
already take &self); callers pass &rx. Prereq for the host auto-reconnect loop.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

> Note: `serve.rs`'s production call to this fn is updated in Task 3. Default CI (which does not compile `serve.rs`) stays green after Task 1; the `--features live-capture,live-usb` build goes green again at the end of Task 3.

---

## Task 2: Add tested reconnect-decision helpers in `serve.rs`

Three small pure units the inline loop will use. Keeping them as named, tested functions gives a unit-test surface for the otherwise hardware-gated loop logic.

**Files:**
- Modify: `crates/macos-host/src/serve.rs` (add helpers near the top of the module, after the `use` block; add a `#[cfg(test)] mod tests`)
- Test: `crates/macos-host/src/serve.rs`

- [ ] **Step 1: Write the failing tests**

Add at the end of `serve.rs`:

```rust
/// Why a single connection ended — drives the reconnect loop.
#[derive(Debug, PartialEq, Eq)]
enum ConnEnd {
    /// `rustscreen stop` / SIGTERM was requested → break the loop into final teardown.
    Stopped,
    /// The phone went away (replug, sleep, write error) → loop and wait for it again.
    Disconnected,
}

/// Classify a finished connection. `stop` is the only thing that means "shut down":
/// whether the stream returned `Ok` or `Err`, an unset `stop` means the phone is gone
/// and we should reconnect.
fn classify_connection_end(stop_requested: bool) -> ConnEnd {
    if stop_requested {
        ConnEnd::Stopped
    } else {
        ConnEnd::Disconnected
    }
}

/// Drain every buffered item from `rx` without blocking; returns how many were discarded.
/// Used on (re)connect to drop stale pre-disconnect frames so the forced keyframe is the
/// first frame the phone actually receives.
fn drain_frames<T>(rx: &std::sync::mpsc::Receiver<T>) -> usize {
    let mut n = 0;
    while rx.try_recv().is_ok() {
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_requested_means_stopped_else_disconnected() {
        assert_eq!(classify_connection_end(true), ConnEnd::Stopped);
        assert_eq!(classify_connection_end(false), ConnEnd::Disconnected);
    }

    #[test]
    fn drain_frames_discards_all_buffered_and_counts_them() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<u8>(4);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        assert_eq!(drain_frames(&rx), 2, "drains both buffered items");
        assert_eq!(drain_frames(&rx), 0, "nothing left to drain");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p macos-host --features live-capture,live-usb classify_connection_end drain_frames`
Expected: FAIL to compile first time only if names collide; otherwise the tests run. (They are written to pass once the helpers above exist — the "failing" state is that the helpers/tests don't exist yet before this step is applied. If you applied Step 1 in one edit, run Step 4 directly.)

- [ ] **Step 3: (No separate impl step — helpers were added with the tests in Step 1.)**

The helpers are pure and complete as written.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p macos-host --features live-capture,live-usb classify_connection_end drain_frames`
Expected: PASS — `stop_requested_means_stopped_else_disconnected` and `drain_frames_discards_all_buffered_and_counts_them`.

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/serve.rs
git commit -m "feat(serve): add reconnect-decision helpers (ConnEnd, drain_frames)

Pure, unit-tested building blocks for the host auto-reconnect loop: classify a
finished connection as Stopped (rustscreen stop) vs Disconnected (replug), and
drain stale buffered frames so the post-reconnect keyframe lands first.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: Restructure `run_host` into warm-setup → reconnect loop → single teardown

The core change. This is one careful structural edit of `serve.rs` `run_host` (`:328`–`:756`). It is hardware-gated, so it is verified by `cargo build` + on-device replug (Task 4), not by a cable-free unit test.

**Files:**
- Modify: `crates/macos-host/src/serve.rs:328`–`:756` (`run_host` body)

- [ ] **Step 1: Move the capture wiring + start block above the wait-for-phone loop**

Currently the order is: warm setup (`:347`–`:502`) → **wait-for-phone loop** (`:509`–`:519`) → **capture wiring + start** (`:521`–`:564`) → per-session. Move the capture block (`:521`–`:564`: the `let (tx, rx) = mpsc::sync_channel...` through the `startCapture` `match start_rx.recv_timeout(...)` block) so it runs **before** the wait-for-phone loop — i.e. immediately after the rate-control `println!` at `:502`.

Rationale (capture-start-before-loop): simplest control flow, encoder stays warm, and the bounded depth-2 channel harmlessly drops surplus while no consumer is reading. Keep `sink` bound in `run_host` scope (it already is) so the loop can reach `sink.ivars().needs_keyframe`.

- [ ] **Step 2: Wrap the wait-for-phone loop + per-connection half in a single `loop`, and add reconnect hygiene**

Replace the existing `let mut transport = loop { ... bring_up_aoa ... };` (`:509`–`:519`) and the per-connection body that follows with a single outer reconnect loop. The new skeleton (the large inner blocks — handshake `:578`-`:588`, split/reader-thread `:598`-`:650`, clock-sync `:656`-`:690`, stream call `:693`-`:707`, per-connection teardown — stay **verbatim** except the four marked changes):

```rust
println!("rustscreen: waiting for the phone (open the app to start streaming)…");
'reconnect: loop {
    // (A) Honor stop while waiting for a phone.
    if stop.load(Ordering::Relaxed) {
        break 'reconnect;
    }

    // Wait-for-phone: retry bring-up until the accessory appears or stop is set.
    let mut transport = loop {
        if stop.load(Ordering::Relaxed) {
            break 'reconnect; // stop requested mid-wait → final teardown
        }
        match bring_up_aoa() {
            Ok(t) => break t,
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    };

    // (B) Reconnect hygiene: the fresh phone decoder needs a clean IDR as its first
    // frame, and any frames captured while it was gone are stale. Force a keyframe and
    // drop the stale backlog so the IDR is genuinely first on the wire. Harmless on the
    // first connect (the first frame is an IDR anyway).
    sink.ivars().needs_keyframe.store(true, Ordering::Relaxed);
    let drained = drain_frames(&rx);
    if drained > 0 {
        println!("rustscreen: reconnect — dropped {drained} stale frame(s) before keyframe.");
    }

    // --- per-connection: handshake + clock-sync + stream (verbatim blocks) ---------
    // ... existing handshake block (:578-:588) UNCHANGED ...
    // ... existing split + reader-thread block (:598-:650) UNCHANGED ...
    // ... existing clock-sync block (:656-:690) UNCHANGED ...

    // (C) Fresh per-connection latency accumulator (per-session reset).
    let mut pipeline = crate::latency::PipelineLatency::new(256);
    let result = session::run_stream_session_instrumented(
        &rx,                 // (D) borrow, not move — warm channel reused next reconnect
        &mut write_half,
        agreed,
        offset,
        &stats_rx,
        &mut pipeline,
        Duration::from_secs(1),
        Duration::from_secs(2),
        now_us,
        |report| print_report(report, offset),
        stop,
    );

    // per-connection summary + final report for THIS connection (verbatim :709-:755,
    // MINUS the `drop(vdisplay)` at :723 and the `stopCapture` block at :717-:722,
    // which move to the single final teardown after the loop).
    print_report(&pipeline.report(), offset);
    match &result {
        Ok(summary) => println!(
            "rustscreen: connection ended cleanly — {} frames, {} bytes sent; encode mean {:.2} ms.",
            summary.frames, summary.bytes_sent,
            summary.latency.mean().map_or(0.0, |m| m / 1000.0),
        ),
        Err(e) => eprintln!("rustscreen: connection ended (likely disconnect): {e}"),
    }

    // Per-connection teardown: drop the transport write half + stop/join the reader,
    // but LEAVE vdisplay + capture running so reconnect is warm. (Verbatim from the
    // existing teardown :724-:742: drop(write_half); reader_local_stop.store(true);
    // bounded join-or-detach.)
    drop(write_half);
    reader_local_stop.store(true, Ordering::Relaxed);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !reader.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if reader.is_finished() {
        let _ = reader.join();
    } else {
        eprintln!(
            "rustscreen: reader thread still parked on a bulk-IN read at teardown; detaching it \
             (it will die with the process) so exit does not hang."
        );
    }

    // (E) Stop vs. disconnect: stop → break to final teardown, else reconnect.
    match classify_connection_end(stop.load(Ordering::Relaxed)) {
        ConnEnd::Stopped => break 'reconnect,
        ConnEnd::Disconnected => {
            println!("rustscreen: phone disconnected — waiting for replug (display kept alive)…");
            continue 'reconnect;
        }
    }
}
```

Note the `reader`, `reader_local_stop`, `write_half`, `read_half`, `pong_rx`, `stats_rx`, `agreed`, `offset` bindings are all created **inside** the loop body (in the verbatim split/handshake/clock-sync blocks), so they are fresh per iteration and dropped at iteration end — no leak across reconnects.

- [ ] **Step 3: Move `stopCapture` + `drop(vdisplay)` to a single final teardown after the loop**

After the `'reconnect: loop { ... }`, add the one-time teardown (the `stopCapture` block from old `:717`–`:722` and `drop(vdisplay)` from old `:723`):

```rust
// --- Final teardown (runs once, on stop) ----------------------------------
let (stop_tx, stop_rx) = mpsc::channel::<()>();
let stop_handler = block2::RcBlock::new(move |_e: *mut NSError| {
    let _ = stop_tx.send(());
});
unsafe { stream.stopCaptureWithCompletionHandler(Some(&stop_handler)) };
let _ = stop_rx.recv_timeout(Duration::from_secs(5));
drop(vdisplay); // remove the virtual display — the ONLY desktop reflow, on shutdown
println!("rustscreen: host stopped — virtual display removed.");
Ok(())
```

Also update the early-out inside the wait-for-phone sub-loop: the old code did `drop(vdisplay); return Ok(())` on stop (`:511`-`:513`). With the centralized teardown, that path now `break 'reconnect`s (shown in Step 2) and falls through to this single teardown — so `vdisplay` is dropped exactly once on every exit path. Delete the old standalone `drop(vdisplay); return Ok(());` and the old standalone `drop(vdisplay)` at `:581` (handshake-failure path) — handshake failure now becomes a `continue 'reconnect` (it is inside the loop, and `run_one_connection`-equivalent treats a failed handshake as disconnect). Concretely, change the handshake error arm (`:580`-`:583`) from returning an error to:

```rust
        Err(e) => {
            eprintln!("rustscreen: handshake failed ({e}); treating as disconnect, will retry…");
            // tear down this transport before looping (reader not yet spawned here)
            continue 'reconnect;
        }
```

- [ ] **Step 4: Update the doc comment on `run_host`**

Change the `:326`-`:327` doc comment from "On every return path the virtual display has been dropped so the Mac desktop reflows." to reflect the new lifecycle:

```rust
/// Run the host, auto-reconnecting across phone disconnects until `stop` is set. The virtual
/// display and capture/encoder stay warm across reconnects (no desktop reflow, no cold start);
/// the display is dropped exactly once, in the final teardown when `stop` is requested.
```

- [ ] **Step 5: Build under the live features to verify it compiles**

Run: `cargo build -p macos-host --features live-capture,live-usb`
Expected: PASS — no errors. (Borrow checker confirms `rx`/`sink`/`stream`/`vdisplay` are all reachable from inside the loop and the single teardown.)

- [ ] **Step 6: Run the full live test + clippy**

Run: `cargo test -p macos-host --features live-capture,live-usb` then `cargo clippy -p macos-host --features live-capture,live-usb -- -D warnings`
Expected: PASS — all tests green, no clippy warnings (no unused imports — commit hygiene).

- [ ] **Step 7: Commit**

```bash
git add crates/macos-host/src/serve.rs
git commit -m "feat(P2): host auto-reconnect loop — self-healing handshake (C1)

run_host now wraps the per-connection half (bring-up → handshake → clock-sync →
stream) in a reconnect loop. The virtual display + ScreenCaptureKit capture +
VideoToolbox session + EncodedFrame channel are built once and kept warm; only
the AOA transport, reader thread, and clock-sync are rebuilt per connection. On
each (re)connect we force an IDR (needs_keyframe) and drain stale frames so the
fresh phone decoder shows a clean image in ~16ms. rustscreen stop breaks the
loop into a single teardown (the only desktop reflow); a plain disconnect loops
back. No nusb handle / thread leak per iteration.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: On-device verification (M1 + Pixel 6a)

The reconnect behavior is hardware-gated; verify it live per the project's measure-don't-guess rule.

**Files:** none (verification only)

- [ ] **Step 1: Start the host and confirm first connect streams**

Run the live host (via the `run-on-device` skill or `rustscreen start`) with the Pixel connected and the app open. Confirm video renders on the phone and a latency report prints.

- [ ] **Step 2: Replug test — auto-recovery**

Physically unplug the USB-C cable while streaming, wait ~3 s, replug. Expected:
- Host logs `phone disconnected — waiting for replug (display kept alive)…` then re-connects automatically within ~1–2 s.
- The Mac desktop arrangement is **unchanged** (no reflow / window shuffle) — the virtual display stayed alive.
- The phone shows a **clean image immediately** on reconnect (forced IDR), not artifacts.
- Expect a `reconnect — dropped N stale frame(s) before keyframe.` log line.

- [ ] **Step 3: Sleep/wake test**

Let the Mac (or phone) sleep briefly and wake. Expected: same auto-recovery as replug.

- [ ] **Step 4: Repeated-replug leak check**

Replug 5–10 times. Expected: each recovers; no growth in reader threads / USB handles (host stays responsive, no slowdown). Optionally watch `ps`/Activity Monitor thread count for the worker pid.

- [ ] **Step 5: Clean-stop test**

Run `rustscreen stop` (SIGTERM). Expected: the worker exits cleanly, logs `host stopped — virtual display removed.`, and the Mac desktop reflows exactly once (display removed). No reconnect attempt after stop.

- [ ] **Step 6: Record the result**

Note recovery time and any anomalies. If recovery is slower than ~2 s or the image is dirty on reconnect, debug before merging (use superpowers:systematic-debugging).

---

## Self-Review

**Spec coverage:**
- vdisplay kept alive across reconnects → Task 3 Steps 2–3 (teardown moved out of loop). ✓
- capture/encoder warm → Task 3 Step 1 (capture start before loop). ✓
- clean image in ~16ms (force IDR) → Task 3 Step 2 (B). ✓
- drain stale frames → Task 2 (`drain_frames`) + Task 3 Step 2 (B). ✓
- per-session `PipelineLatency` reset → Task 3 Step 2 (C). ✓
- retry forever + 200ms backoff → Task 3 Step 2 (inner wait-for-phone loop, verbatim backoff). ✓
- stop vs disconnect → Task 2 (`classify_connection_end`) + Task 3 Step 2 (E) + Step 3 (handshake-fail = disconnect). ✓
- no leak per iteration → Task 3 Step 2 (per-iteration bindings + per-connection reader teardown). ✓
- `&Receiver` signature change → Task 1. ✓
- clock-sync degrade unchanged → Task 3 (clock-sync block verbatim). ✓
- on-device verification → Task 4. ✓
- p5_stream left untouched → File Structure note. ✓

**Placeholder scan:** No TBD/TODO; all code shown. Large verbatim ObjC blocks are referenced by exact current line ranges with explicit "UNCHANGED" / move instructions (appropriate for an existing 845-line gated file rather than re-pasting hundreds of lines).

**Type consistency:** `ConnEnd { Stopped, Disconnected }`, `classify_connection_end(bool) -> ConnEnd`, `drain_frames(&Receiver<T>) -> usize`, and `frames: &Receiver<EncodedFrame>` are used identically in Tasks 1–3. `sink.ivars().needs_keyframe` and `crate::latency::PipelineLatency::new` match the existing code read from `serve.rs`. ✓
