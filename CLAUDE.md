# RustScreen — Project Instructions

RustScreen turns a Pixel 6a into a USB-C second monitor for an M1 MacBook. The pipeline is:
**Mac screen capture → VideoToolbox H.264 encode → AOA/USB send → phone MediaCodec decode →
present on the Pixel panel.** This is a latency-critical real-time system.

## Prime directive: optimize for screen-to-screen latency

Every fix, feature, refactor, or change MUST be made with **glass-to-glass (screen-to-screen)
latency in mind** — the wall-clock time from a pixel changing on the Mac to that change appearing
on the phone. When in doubt, the lower-latency option wins.

Apply this on every task:

- **Lowest-latency correct option wins.** When more than one correct implementation exists,
  pick the one that adds the least latency to the capture→encode→send→decode→present path.
- **Never add unbounded buffering on the hot path.** Queues between stages must be bounded.
  Prefer drop-newest-with-resync over growing latency. A dropped *post-encode* frame breaks the
  H.264 reference chain, so any such drop MUST force the next frame to an IDR (keyframe) so the
  decoder resyncs — see `crates/macos-host/src/bin/p5_stream.rs` (`needs_keyframe`). Pre-encode
  (pre-VideoToolbox-submit) drops are safe and need no resync.
- **Keep the per-frame hot path lean.** In capture/encode/send/decode/present code, avoid
  blocking calls, heap allocation, lock contention, and dynamic dispatch (vtables) where a
  cheaper alternative exists. `Ordering::Relaxed` is the default for standalone hot-path counters
  and flags (no companion memory to publish).
- **Measure, don't guess.** This is an instrumented pipeline (per-stage + glass-to-glass
  latency reports). Prefer changes whose latency impact can be verified against real numbers on
  the connected M1 Mac + Pixel 6a, not assumed.
- **Call out latency tradeoffs explicitly.** If a fix trades latency for another property
  (correctness, simplicity, robustness), state the tradeoff and prefer the low-latency path
  unless the user says otherwise. Correctness still wins over latency when they genuinely
  conflict — but say so.

### Targets (context)
- P5 goal: **< 50 ms** glass-to-glass.
- Practical floor: **~43 ms** (the Pixel's 60 Hz panel + locked 60 Hz `CGVirtualDisplay` cap;
  ~30 ms is not reachable without new hardware). Don't chase sub-floor numbers.

## Commit hygiene
- Before any commit/push (including `--amend`), remove unused imports from every changed file.
  (Rust: clippy/rustc enforce this anyway.)
