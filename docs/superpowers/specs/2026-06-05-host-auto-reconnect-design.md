# Host auto-reconnect loop (self-healing handshake) — design

**Date:** 2026-06-05
**Roadmap item:** C1 (Tier 1, item #2) — *Host auto-reconnect loop*, with the reconnect-scoped
slice of C5 (*force-keyframe on (re)connect*).
**Target file:** `crates/macos-host/src/serve.rs` (`run_host`).
**Branch / PR:** `feat/p5-auto-reconnect` (PR 26).

## Problem

Today a phone disconnect (replug, USB-C jostle, sleep/wake) ends the host session permanently.
`run_host` runs exactly one connection and, on every return path, `drop`s the virtual display and
returns — so the Mac desktop reflows and the daemon must be manually restarted before the phone
works again. Warm replug **never auto-recovers**.

Per the prime directive, recovery must also be **low-latency**: the encoder and virtual display
should stay warm across reconnects (no cold-start cost, no desktop reflow), and the freshly
reconnected phone decoder must get a clean image immediately rather than artifacts until the next
periodic keyframe.

## Goal

Wrap the per-connection half of `run_host` in a reconnect loop so that an unplug/replug (or
sleep/wake) recovers automatically in ~1–2 s, with:

- the **virtual display kept alive** across reconnects (desktop arrangement preserved, no reflow);
- the **capture → encode half kept warm** (no encoder cold-start on replug);
- a **clean image within ~16 ms** of each reconnect (forced IDR + drained stale frames);
- **no nusb handle / thread leak** per reconnect iteration;
- **`rustscreen stop` (SIGTERM) still tears down cleanly** — stop must end the process, not trigger
  a reconnect.

Out of scope (separate roadmap items): nusb hotplug instead of poll (C2), foreground service (C4),
wiring `Control::RequestKeyframe` end-to-end (C5 remainder), idle-frame skip (P1).

## Current structure (`run_host`, as of `da65f30`)

Linear, single-session:

1. **Warm setup** — create `vdisplay`; locate `SCDisplay`; build `SCContentFilter` +
   `SCStreamConfiguration`; create the VideoToolbox session; set rate control.
2. **Wait-for-phone** — `loop { if stop { drop(vdisplay); return } match bring_up_aoa() { Ok→break,
   Err→sleep(200ms) } }` (already `stop`-aware, 200 ms backoff).
3. **Capture wiring + start** — create the bounded depth-2 `tx/rx` `EncodedFrame` channel; build the
   `FrameSink` (holds `tx` and the `needs_keyframe` flag); `addStreamOutput`; `startCapture`.
4. **Per-session** — `perform_handshake`; `transport.split()`; spawn the inbound reader thread
   (owns `pong`/`stats` channels); clock-sync (bounded 2 s, degrades to `offset=None`);
   `run_stream_session_instrumented(rx, &mut write_half, …, stop)` until disconnect or stop.
5. **Teardown** — `stopCapture`; **`drop(vdisplay)`** (← the desktop reflow); `drop(write_half)`;
   bounded reader-thread join/detach.

The key seam: steps 1 and 3 are *warm* (must survive reconnects); steps 2 and 4 are *per-connection*
(rebuilt each time). Step 5's `drop(vdisplay)` is what we must defer to a single final teardown.

## Approach

Restructure `run_host` into **warm setup (once) → reconnect loop → final teardown (once)**:

```
// 1. warm setup: vdisplay, SCDisplay, filter, config, VT session, rate control      [once]
// 3. capture wiring + start: channel + sink + addStreamOutput + startCapture         [once]
//    (started BEFORE the loop — simplest control flow; encoder warm. Pre-first-phone
//     idle burn is negligible given always-connected hardware, and is the domain of
//     the separate idle-frame-skip item, not this loop.)

loop {
    if stop.load(Relaxed) { break }

    // 2. wait-for-phone (the existing retry loop, factored into a helper)
    let transport = match wait_for_phone(stop) {
        Some(t) => t,
        None    => break,            // stop requested while waiting
    };

    // reconnect hygiene (applies on first connect too — uniform, harmless there)
    sink.ivars().needs_keyframe.store(true, Relaxed);   // clean IDR for the fresh decoder
    while rx.try_recv().is_ok() {}                       // drop stale pre-disconnect frames

    // 4. per-connection half
    match run_one_connection(transport, &rx, &sink, W, H, fps, stop) {
        ConnEnd::Stopped      => break,      // rustscreen stop / SIGTERM → clean shutdown
        ConnEnd::Disconnected => continue,   // phone gone → reconnect; vdisplay + capture stay warm
    }
}

// 5. final teardown: stopCapture; drop(vdisplay)   [once — the ONLY desktop reflow]
```

### New/changed units

- **`wait_for_phone(stop: &AtomicBool) -> Option<AoaTransport>`** — the existing step-2 retry loop,
  extracted verbatim. Returns `Some(transport)` on connect, `None` if `stop` was set while waiting.
  No `drop(vdisplay)` inside — teardown is centralized.

- **`run_one_connection(transport, rx: &Receiver<EncodedFrame>, sink, w, h, fps, stop) -> ConnEnd`**
  — owns the per-session half: `perform_handshake` → `split` (+ `set_num_transfers(4)`,
  `set_write_timeout(5s)`) → spawn reader thread (fresh `pong`/`stats` channels, fresh
  `reader_local_stop`) → clock-sync → **fresh `PipelineLatency`** →
  `run_stream_session_instrumented(rx, …)` → bounded reader join/detach + `drop(write_half)`.
  Returns:
  - `ConnEnd::Stopped` if `stop` is set after the stream returns (clean shutdown), else
  - `ConnEnd::Disconnected` (phone gone / write error / handshake failure → reconnect).

  A failed `perform_handshake` returns `Disconnected` (reconnect) rather than erroring out — a phone
  that connects USB but hasn't opened the app yet should be retried, not fatal.

- **`enum ConnEnd { Stopped, Disconnected }`** — local to `serve.rs`.

### Required signature change

`session::run_stream_session_instrumented` currently takes `frames: Receiver<EncodedFrame>` **by
value** (consumes it). Since `rx` is now reused on every reconnect, it becomes
`frames: &Receiver<EncodedFrame>`. `Receiver::recv_timeout` already takes `&self`, so the body is
unchanged; only the signature, the one production caller, and the cable-free unit tests update to
pass `&rx`.

### Reconnect hygiene (correctness for the fresh decoder)

On every (re)connect, before streaming:

1. **Force IDR** — `sink.ivars().needs_keyframe = true`. The capture delegate forces the next
   submitted frame to an IDR, so the fresh `MediaCodec` gets `VideoConfig` (SPS/PPS) + IDR as its
   first delivered frame and shows a correct image within ~16 ms instead of garbage until the next
   periodic keyframe (≤1 s). Reuses the existing post-drop resync mechanism — no new machinery.
2. **Drain stale `rx`** — while the phone was gone, capture kept filling the depth-2 channel
   (`try_send` dropping surplus), so ≤2 stale pre-disconnect P-frames sit in `rx`. Drain them
   (`while rx.try_recv().is_ok() {}`) so the IDR is genuinely the first frame the phone receives.
   Without this, ≤2 undecodable P-frames precede the IDR → a brief flicker. Both steps are applied
   on the first connect too (uniform, harmless — the first frame is an IDR regardless).

## Decisions (resolved during brainstorming)

| Decision | Choice | Rationale |
|---|---|---|
| Structure | Refactor `serve::run_host` in place; capture stays warm | Lowest-latency replug, no desktop reflow; reuses the existing warm/cold split |
| Retry policy | Retry **forever** with short backoff (existing 200 ms); only `stop` exits | Daily-driver UX — plug in any time and it connects |
| Reconnect IDR | **Yes** — force IDR via `needs_keyframe` each (re)connect | Clean image in ~16 ms vs ≤1 s of artifacts; one-line reuse |
| `PipelineLatency` | **Per-session reset** (fresh per `run_one_connection`) | Clean per-connection numbers; each uses that session's clock offset |
| Stale frames | **Drain `rx`** on (re)connect | Perfectly clean reconnect image for negligible cost |
| Capture start | **Before the loop** (unconditional) | Simplest control flow; idle burn negligible on always-connected hardware |

## Error handling / edge cases

- **Disconnect vs. stop:** disconnect surfaces as `run_stream_session_instrumented` returning (write
  error / peer gone) with `stop` *not* set → `Disconnected` → reconnect. `rustscreen stop` sets
  `stop` (SIGTERM handler) → the stream loop unwinds and `run_one_connection` returns `Stopped` →
  loop breaks → single final teardown (`stopCapture` + `drop(vdisplay)`). Stop is honored both while
  *waiting* for a phone and while *streaming*.
- **No leaks per iteration:** each `run_one_connection` fully drops its `write_half`/`read_half`
  (transport halves) and joins-or-detaches its reader thread before returning — the same bounded
  teardown that exists today, relocated. No nusb handle or thread accumulates across reconnects.
- **Handshake failure:** treated as `Disconnected` (reconnect), not fatal — covers "USB up, app not
  yet open."
- **Clock-sync failure:** unchanged — degrades to `offset=None` (host-only stage timings) per
  connection.

## Latency tradeoff (explicit)

Latency-**neutral on the steady-state hot path** (no new per-frame work) and latency-**positive on
reconnect**: the encoder and virtual display stay warm (no cold-start on replug) and the forced IDR
yields a clean image within ~16 ms instead of up to ~1 s of artifacts. No correctness-for-latency
trade is made; the only added work runs once per reconnect, off the per-frame path.

## Testing

- **Cable-free unit tests** (no hardware): the `&Receiver` signature change is exercised by the
  existing `run_stream_session_instrumented` tests (update to pass `&rx`). Add a focused test for
  the reconnect hygiene helper (force-IDR flag set + `rx` drained) and, where practical, the
  `ConnEnd` stop-vs-disconnect decision logic.
- **On-device** (M1 + Pixel 6a, per measure-don't-guess): unplug/replug and sleep/wake recover
  automatically in ~1–2 s with a clean image, the desktop arrangement is preserved (no reflow), and
  `rustscreen stop` still exits cleanly. Confirm no handle/thread growth across repeated replugs.

## Files touched

- `crates/macos-host/src/serve.rs` — restructure `run_host`; add `wait_for_phone`,
  `run_one_connection`, `ConnEnd`.
- `crates/macos-host/src/session.rs` — `run_stream_session_instrumented` takes `&Receiver`; update
  its tests.
- (`crates/macos-host/src/bin/p5_stream.rs` — left as legacy; not kept in sync unless requested.)
