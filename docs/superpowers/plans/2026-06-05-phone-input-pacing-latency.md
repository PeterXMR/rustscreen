# Phone-Side Input Pacing for Glass-to-Glass Latency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut glass-to-glass latency from ~500 ms toward the <50 ms target by bounding the Android MediaCodec **input** queue and dropping stale frames *before* they are fed to the decoder (the stage where the latency actually accumulates), plus enabling the Exynos vendor low-latency key the Pixel 6a's decoder actually honors.

**Architecture:** The root cause (see `docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md`) is that the single-threaded receive loop feeds *every* frame into the codec with no bound on undecoded depth, and `arrive→decode` measures exactly that input-queue residence — so the existing *output*-side drop cannot reduce it. We add a pure, host-tested `InputPacer` that admits a frame only while the decoder's in-flight depth is under a small cap, otherwise dropping forward to the next keyframe (drop-to-keyframe). The `VideoDecoder` port gains `in_flight()` (current depth) and `pump()` (drain output without submitting) so the pacer can read depth and keep the codec draining on a dropped frame. The concrete `AMediaCodec` adapter implements real depth counting and sets the Exynos vendor key. All decision logic is platform-agnostic and CI-tested against a fake; only the FFI counters + vendor key are device-only.

**Tech Stack:** Rust, `ndk-sys` (AMediaCodec FFI, `#[cfg(target_os = "android")]` + `live-decode` feature), the existing `protocol`/`decode`/`session` crates, on-device build via `cargo ndk` + Gradle.

---

## Background the implementer needs

- The receive loop lives in `crates/android-client/src/session.rs` (`run_session_with_clock`, the loop starts at line 299). It is platform-agnostic and fully unit-tested with an in-memory transport + a fake `RecordingDecoder` — **no device needed for CI**.
- The `VideoDecoder` port + `DecodeSession` orchestrator are in `crates/android-client/src/decode.rs`. The port has two methods today: `configure` and `decode`. `decode` returns `Vec<PresentedFrame>` (frames that decoded *and* rendered this call).
- The concrete decoder is `crates/android-client/src/mediacodec.rs` (`MediaCodecDecoder`), `#[cfg(all(target_os = "android", feature = "live-decode"))]`. It is **only type-checked on the host** (it never runs in host CI); it runs on the Pixel 6a.
- `crate::now_us()` (in `lib.rs`) is the monotonic phone clock used for all latency stamps.
- **Build commands** (from memory `rustscreen-apk-build.md`): the on-device build needs `--features live-decode` and JDK 17–21. The `android/local.properties` (gitignored) must point `sdk.dir` at the SDK. Host CI is `cargo test --workspace`.
- **Commit hygiene (global CLAUDE.md):** before every commit, remove unused imports from every changed file. Never commit an unused import.
- A pure host-side analogue already exists: `coalesce_to_latest_keyframe` in `crates/macos-host/src/session.rs`. The `InputPacer` is the streaming (one-frame-at-a-time) equivalent for the phone.

## File Structure

- **Create** `crates/android-client/src/pacing.rs` — the pure `InputPacer` admission/drop-to-keyframe state machine. One responsibility: decide feed-vs-drop from (keyframe flag, current in-flight depth). Fully CI-tested.
- **Modify** `crates/android-client/src/lib.rs` — add `pub mod pacing;`; log the new input-drop count in `run_usb_fd`.
- **Modify** `crates/android-client/src/decode.rs` — extend the `VideoDecoder` trait with defaulted `in_flight()` and `pump()`; (tests) prove the defaults.
- **Modify** `crates/android-client/src/session.rs` — wire the `InputPacer` + `pump()` into the receive loop; add `input_frames_dropped` to `SessionSummary`; add a depth-scripted fake + tests.
- **Modify** `crates/android-client/src/mediacodec.rs` — real `in_flight()`/`pump()` via queued/dequeued counters; set the Exynos vendor low-latency key. Device-compiled only.
- **Modify** `docs/superpowers/plans/...`, `.planning/ROADMAP.md`, the roadmap plan doc — record the measured result (Task 5).

---

### Task 1: Pure `InputPacer` admission state machine

**Files:**
- Create: `crates/android-client/src/pacing.rs`
- Modify: `crates/android-client/src/lib.rs` (register the module)

- [ ] **Step 1: Register the module**

In `crates/android-client/src/lib.rs`, add after the `pub mod session;` block (around line 36):

```rust
/// Input-side frame pacing (latency item A): a pure state machine that admits a frame only
/// while the decoder's in-flight depth is under a small cap, else drops forward to the next
/// keyframe. Platform-agnostic and CI-tested; the receive loop in `session` drives it.
pub mod pacing;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/android-client/src/pacing.rs` with ONLY the tests first (the type doesn't exist yet, so it won't compile — that is the "failing test"):

```rust
//! Input-side frame pacing for the live decode path (latency root-cause fix, category A).
//!
//! The Pixel 6a's hardware H.264 decoder accumulates frames in its **input** queue when fed
//! faster than it drains; `arrive→decode` measures exactly that residence, so dropping frames
//! on the output side cannot reduce it (see
//! `docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md`). [`InputPacer`]
//! decides, per incoming frame, whether to feed it to the decoder or drop it — keeping the
//! in-flight depth bounded. Because H.264 deltas depend on prior frames, once we drop a delta
//! we must drop every frame until the next keyframe (drop-to-keyframe), where the stream
//! self-resyncs. Keyframes are always admitted so a backlog is always recoverable within one
//! GOP. This is the streaming analogue of the host's `coalesce_to_latest_keyframe`.

/// Bounds the decoder's in-flight input depth by dropping forward to the next keyframe.
pub struct InputPacer {
    /// Admit while in-flight depth is strictly below this. A small cap (1–2) keeps latency
    /// at a couple of frames; matches Moonlight's `OUTPUT_BUFFER_QUEUE_LIMIT = 2`.
    max_in_flight: usize,
    /// True after we dropped a non-keyframe: stay in drop mode until a keyframe resyncs.
    dropping: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_everything_while_depth_is_under_cap() {
        let mut p = InputPacer::new(2);
        assert!(p.admit(false, 0));
        assert!(p.admit(false, 1));
        assert!(p.admit(true, 1));
    }

    #[test]
    fn drops_delta_at_or_above_cap_and_enters_drop_mode() {
        let mut p = InputPacer::new(2);
        // depth == cap, non-keyframe → drop, and we are now in drop mode.
        assert!(!p.admit(false, 2));
        // Even if depth falls back under the cap, we keep dropping deltas until a keyframe.
        assert!(!p.admit(false, 0));
    }

    #[test]
    fn keyframe_always_admitted_and_clears_drop_mode() {
        let mut p = InputPacer::new(2);
        assert!(!p.admit(false, 5)); // enter drop mode
        assert!(p.admit(true, 5)); // keyframe admitted even over cap, resyncs
        assert!(p.admit(false, 0)); // back to normal admission after resync
    }

    #[test]
    fn keyframe_over_cap_when_not_dropping_is_still_admitted() {
        let mut p = InputPacer::new(2);
        // Not in drop mode, depth high, but it's a keyframe → admit (it's the resync point).
        assert!(p.admit(true, 9));
        // Drop mode was never entered, so the next under-cap delta is admitted.
        assert!(p.admit(false, 0));
    }

    #[test]
    fn dropped_then_keyframe_then_delta_sequence() {
        // A realistic burst: cap=1, depth spikes, deltas drop until the keyframe.
        let mut p = InputPacer::new(1);
        assert!(p.admit(false, 0)); // depth 0 < 1 → feed
        assert!(!p.admit(false, 1)); // depth 1 >= 1, delta → drop
        assert!(!p.admit(false, 1)); // still dropping
        assert!(p.admit(true, 1)); // keyframe → resync
        assert!(p.admit(false, 0)); // depth 0 → feed
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p android-client pacing`
Expected: FAIL to compile — `no function or associated item named 'new'` / `'admit'` for `InputPacer`.

- [ ] **Step 4: Implement `InputPacer`**

Insert the impl in `crates/android-client/src/pacing.rs` **above** the `#[cfg(test)] mod tests` block (right after the `struct InputPacer { ... }` definition):

```rust
impl InputPacer {
    /// New pacer admitting while in-flight depth is `< max_in_flight`. A `max_in_flight` of 0
    /// is clamped to 1 (always allow at least the frame that is about to be decoded).
    pub fn new(max_in_flight: usize) -> Self {
        Self {
            max_in_flight: max_in_flight.max(1),
            dropping: false,
        }
    }

    /// Decide whether to feed the next frame to the decoder.
    ///
    /// - In drop mode: drop everything until a `keyframe`, which resyncs and clears the mode.
    /// - Otherwise: drop a non-keyframe once `in_flight >= max_in_flight` (and enter drop mode);
    ///   admit keyframes unconditionally (they are the resync point and bound the backlog).
    ///
    /// Returns `true` to feed, `false` to drop.
    pub fn admit(&mut self, keyframe: bool, in_flight: usize) -> bool {
        if self.dropping {
            if keyframe {
                self.dropping = false;
                true
            } else {
                false
            }
        } else if !keyframe && in_flight >= self.max_in_flight {
            self.dropping = true;
            false
        } else {
            true
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p android-client pacing`
Expected: PASS (5 tests).

- [ ] **Step 6: Lint + commit**

Run: `cargo clippy -p android-client --all-targets -- -D warnings` (expected: clean), then check `pacing.rs` and `lib.rs` for unused imports (there should be none).

```bash
git add crates/android-client/src/pacing.rs crates/android-client/src/lib.rs
git commit -m "feat(P5): pure InputPacer drop-to-keyframe state machine (latency item A)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Extend the `VideoDecoder` port with `in_flight()` + `pump()`

**Files:**
- Modify: `crates/android-client/src/decode.rs:67-79` (the `VideoDecoder` trait) and its test module.

These are **defaulted** methods so every existing implementor (the two `RecordingDecoder` fakes) keeps compiling unchanged. `in_flight()` reports the codec's current undecoded depth; `pump()` drains ready output without submitting new input (used when the pacer drops a frame but we still want the codec to make progress).

- [ ] **Step 1: Write the failing test**

Add this test inside the `#[cfg(test)] mod tests` block at the end of `crates/android-client/src/decode.rs` (after `drives_a_realistic_frame_sequence`, before the closing `}` of the module):

```rust
    #[test]
    fn video_decoder_defaults_are_zero_depth_and_empty_pump() {
        // A decoder that doesn't override the new methods reports no backlog and pumps nothing,
        // so non-overriding fakes (and any future simple adapter) behave inertly.
        let mut dec = RecordingDecoder::default();
        assert_eq!(VideoDecoder::in_flight(&dec), 0);
        assert_eq!(dec.pump().unwrap(), Vec::new());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p android-client video_decoder_defaults_are_zero_depth_and_empty_pump`
Expected: FAIL to compile — `no method named 'in_flight'` / `'pump'` on `VideoDecoder`.

- [ ] **Step 3: Add the defaulted trait methods**

In `crates/android-client/src/decode.rs`, extend the `VideoDecoder` trait. Replace the closing of the trait (currently ends after the `decode` method at line 78-79):

```rust
    /// Submit one decoder-ready access unit for decode (decode-to-surface in Wave B) and
    /// return any frames that became decoded-and-presented as a result.
    ///
    /// A hardware decoder pipelines: queuing one input may release 0..N ready output
    /// buffers. Each released (rendered) buffer yields one [`PresentedFrame`] stamped with
    /// the phone clock, so the caller can report per-frame latency `Stats` to the host.
    fn decode(&mut self, input: &DecoderInput) -> Result<Vec<PresentedFrame>, DecodeError>;

    /// Current in-flight input depth: access units submitted but not yet emitted as output.
    /// The pacer reads this to bound the codec's input queue (latency item A). Defaults to 0
    /// for adapters/fakes that don't pipeline.
    fn in_flight(&self) -> usize {
        0
    }

    /// Drain any ready decoded output WITHOUT submitting new input, returning the frames that
    /// presented. Used when the pacer drops an incoming frame but the codec should keep
    /// draining (so its output queue and the surface stay current). Defaults to a no-op.
    fn pump(&mut self) -> Result<Vec<PresentedFrame>, DecodeError> {
        Ok(Vec::new())
    }
}
```

(Leave `configure` and the start of the trait unchanged — only append the two methods before the trait's closing brace.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p android-client video_decoder_defaults_are_zero_depth_and_empty_pump`
Expected: PASS. Also run `cargo test -p android-client` — all existing decode tests still pass (defaults didn't change behavior).

- [ ] **Step 5: Lint + commit**

Run: `cargo clippy -p android-client --all-targets -- -D warnings` (expected: clean). Check `decode.rs` for unused imports.

```bash
git add crates/android-client/src/decode.rs
git commit -m "feat(P5): add VideoDecoder::in_flight + pump port methods (latency item A seam)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Wire the `InputPacer` into the receive loop

**Files:**
- Modify: `crates/android-client/src/session.rs` — the receive loop (line 299), `SessionSummary` (lines 172-184), the `Video` arm (lines 312-336), and the test fake + new tests.

- [ ] **Step 1: Write the failing tests**

First, extend the test fake `RecordingDecoder` in `crates/android-client/src/session.rs` (around lines 402-438) so a test can script the depth it reports and observe `pump` calls. Add a `Pump` variant and two fields, and implement the two new trait methods. Replace the `DecoderCall` enum and `RecordingDecoder` struct + impl with:

```rust
    /// Recorded call on the fake decoder.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum DecoderCall {
        Configure(VideoCodec, protocol::nal::CodecConfig),
        Decode(crate::decode::DecoderInput),
        Pump,
    }

    /// Fake [`VideoDecoder`] that records every call and returns scripted present timing so
    /// the latency orchestration (arrive→decode→present → `Frame::Stats`) is provable with
    /// no device.
    #[derive(Debug, Default)]
    struct RecordingDecoder {
        calls: std::cell::RefCell<Vec<DecoderCall>>,
        /// If `Some(idx)`, the `idx`-th `decode` call returns an Adapter error.
        fail_on_decode_index: Option<u64>,
        decode_index: u64,
        /// Scripted `PresentedFrame`s each `decode` call returns, popped front-to-back.
        present_script: VecDeque<Vec<crate::decode::PresentedFrame>>,
        /// Depth returned by successive `in_flight()` calls, popped front-to-back; 0 when empty.
        in_flight_script: std::cell::RefCell<VecDeque<usize>>,
    }

    impl VideoDecoder for RecordingDecoder {
        fn configure(
            &mut self,
            codec: VideoCodec,
            config: &protocol::nal::CodecConfig,
        ) -> Result<(), DecodeError> {
            self.calls
                .borrow_mut()
                .push(DecoderCall::Configure(codec, config.clone()));
            Ok(())
        }

        fn decode(
            &mut self,
            input: &crate::decode::DecoderInput,
        ) -> Result<Vec<crate::decode::PresentedFrame>, DecodeError> {
            let idx = self.decode_index;
            self.decode_index += 1;
            if self.fail_on_decode_index == Some(idx) {
                return Err(DecodeError::Adapter("simulated codec failure".into()));
            }
            self.calls.borrow_mut().push(DecoderCall::Decode(input.clone()));
            Ok(self.present_script.pop_front().unwrap_or_default())
        }

        fn in_flight(&self) -> usize {
            self.in_flight_script.borrow_mut().pop_front().unwrap_or(0)
        }

        fn pump(&mut self) -> Result<Vec<crate::decode::PresentedFrame>, DecodeError> {
            self.calls.borrow_mut().push(DecoderCall::Pump);
            Ok(Vec::new())
        }
    }
```

> Note: `calls` became a `RefCell` so `decode`/`pump` (which now run through `&self` borrows in some asserts) and the existing `&mut self` calls both compile. Every existing test that reads `dec.calls` must change `dec.calls` → `dec.calls.borrow()` and `dec.calls.iter()` → `dec.calls.borrow().iter()`. Update those reads in the existing tests `video_config_then_video_frames_drive_decoder` and any other that inspects `dec.calls` (search the file for `dec.calls`).

Then add the two new behavior tests at the end of the `mod tests` block (before its closing brace):

```rust
    #[test]
    fn high_in_flight_delta_is_dropped_and_pumps_instead_of_decoding() {
        // VideoConfig + keyframe (admitted, depth 0) + delta reported at high depth (dropped).
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Video {
                pts_us: 0,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
            Frame::Video {
                pts_us: 16_666,
                keyframe: false,
                nal: delta_nal(),
            },
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        // in_flight() is consulted once per Video frame, before feeding. Keyframe sees depth 0
        // (admitted); the delta sees depth 99 (>= the cap) → dropped → pump() instead.
        let mut dec = RecordingDecoder {
            in_flight_script: std::cell::RefCell::new(VecDeque::from(vec![0usize, 99usize])),
            ..RecordingDecoder::default()
        };
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        assert_eq!(summary.input_frames_dropped, 1, "the high-depth delta was dropped");
        // The keyframe was decoded; the delta was NOT decoded but pump() ran.
        let calls = dec.calls.borrow();
        let decodes = calls.iter().filter(|c| matches!(c, DecoderCall::Decode(_))).count();
        let pumps = calls.iter().filter(|c| matches!(c, DecoderCall::Pump)).count();
        assert_eq!(decodes, 1, "only the keyframe is decoded");
        assert_eq!(pumps, 1, "the dropped delta still pumps the decoder");
        assert_eq!(ds.decoded_count(), 1);
    }

    #[test]
    fn keyframe_is_admitted_even_at_high_depth() {
        // A keyframe must always feed (it's the resync point), even when in_flight is huge.
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Video {
                pts_us: 0,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder {
            in_flight_script: std::cell::RefCell::new(VecDeque::from(vec![999usize])),
            ..RecordingDecoder::default()
        };
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        assert_eq!(summary.input_frames_dropped, 0);
        assert_eq!(ds.decoded_count(), 1, "keyframe decoded despite high depth");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p android-client high_in_flight_delta_is_dropped_and_pumps_instead_of_decoding keyframe_is_admitted_even_at_high_depth`
Expected: FAIL to compile — `no field 'input_frames_dropped' on SessionSummary` and the loop doesn't yet pace.

- [ ] **Step 3: Add the `input_frames_dropped` field to `SessionSummary`**

In `crates/android-client/src/session.rs`, in the `SessionSummary` struct (lines 172-184), add a field before `agreed`:

```rust
    /// Whether the decoder was configured at least once.
    pub configured: bool,
    /// Number of Video frames dropped on the input side by the pacer (drop-to-keyframe under
    /// in-flight depth pressure) — the latency-item-A shed-load counter.
    pub input_frames_dropped: u64,
    /// The negotiated streaming configuration (present when the handshake succeeded).
    pub agreed: Option<AgreedConfig>,
```

- [ ] **Step 4: Add the import + pacer state + paced `Video` arm**

At the top of `crates/android-client/src/session.rs`, add to the `use crate::...` imports (near line 44):

```rust
use crate::decode::{DecodeError, DecodeSession, VideoDecoder};
use crate::pacing::InputPacer;
```

Add the in-flight cap constant next to `STATS_TRACKER_CAPACITY` (around line 207):

```rust
/// Max decoder in-flight input depth before the pacer drops forward to the next keyframe.
/// Small (Moonlight uses 2) so latency stays at a couple of frames. See
/// `docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md`.
const MAX_IN_FLIGHT: usize = 2;
```

In `run_session_with_clock`, just before the `loop {` at line 299, add the pacer + counter (after `let mut stats = StatsTracker::new(STATS_TRACKER_CAPACITY);`):

```rust
    let mut pacer = InputPacer::new(MAX_IN_FLIGHT);
    let mut input_frames_dropped: u64 = 0;
```

Replace the `Frame::Video { .. }` arm (lines 312-336) with the paced version:

```rust
            Frame::Video { pts_us, keyframe, .. } => {
                // Stamp arrival on the phone clock BEFORE the feed/drop decision, so the
                // arrive→decode→present ordering the host expects holds for admitted frames.
                stats.on_arrive(*pts_us, now_us());
                // Input-side pacing (latency item A): admit only while the decoder's in-flight
                // depth is under the cap; otherwise drop forward to the next keyframe but still
                // pump the decoder so its output/surface stay current.
                let presented = if pacer.admit(*keyframe, decoder.in_flight()) {
                    decode_session
                        .feed(&frame, decoder)
                        .map_err(SessionError::Decode)?
                } else {
                    input_frames_dropped += 1;
                    decoder.pump().map_err(SessionError::Decode)?
                };
                // Each presented output frame completes a per-frame record; emit its Stats.
                let mut wrote_stats = false;
                for pf in presented {
                    stats.on_decode(pf.pts_us, pf.decode_us);
                    if let Some(stats_frame) = stats.on_present(pf.pts_us, pf.present_us) {
                        stats_frame
                            .write_to(transport)
                            .map_err(SessionError::from)?;
                        wrote_stats = true;
                    }
                }
                // Flush so the host's stats-reader thread sees these promptly even behind a
                // buffered transport (the live `AccessoryFdTransport` is an unbuffered File,
                // so this is a no-op there).
                if wrote_stats {
                    transport.flush().map_err(SessionError::from)?;
                }
            }
```

Finally, populate the new field in the returned `SessionSummary` (lines 372-378):

```rust
    Ok(SessionSummary {
        frames_received,
        decoded_count: decode_session.decoded_count(),
        keyframe_count: decode_session.keyframe_count(),
        configured: decode_session.configured(),
        input_frames_dropped,
        agreed: Some(agreed),
    })
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p android-client`
Expected: PASS (all session tests, including the two new ones; existing tests updated for `dec.calls.borrow()` also pass).

- [ ] **Step 6: Surface the drop count in the JNI summary log**

In `crates/android-client/src/lib.rs`, in the `#[cfg(feature = "live-decode")] fn run_usb_fd` (the `log::info!` at lines 174-179), add the drop count:

```rust
        log::info!(
            "nativeOnUsbFd: {} frames received, {} decoded, {} keyframes, {} input-dropped",
            summary.frames_received,
            summary.decoded_count,
            summary.keyframe_count,
            summary.input_frames_dropped
        );
```

- [ ] **Step 7: Lint + commit**

Run: `cargo clippy -p android-client --all-targets -- -D warnings` (expected: clean). Check `session.rs` and `lib.rs` for unused imports.

```bash
git add crates/android-client/src/session.rs crates/android-client/src/lib.rs
git commit -m "feat(P5): pace decoder input by in-flight depth, drop-to-keyframe (latency item A)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Real depth counting + Exynos vendor key in the AMediaCodec adapter

**Files:**
- Modify: `crates/android-client/src/mediacodec.rs` — add `queued`/`dequeued` counters, implement `in_flight()`/`pump()`, set the Exynos vendor low-latency key.

This file is **device-only** (`#[cfg(all(target_os = "android", feature = "live-decode"))]`) — it is not exercised by host CI. Verification here is a **cross-compile type-check + clippy**, then the live run in Task 5.

- [ ] **Step 1: Add the counter imports + fields**

In `crates/android-client/src/mediacodec.rs`, add to the imports at the top (near line 35):

```rust
use std::cell::Cell;
use std::ffi::CStr;
use std::ptr::NonNull;
```

Add two counter fields to `MediaCodecDecoder` (struct at lines 122-128):

```rust
pub struct MediaCodecDecoder {
    /// The decode-to-surface target, taken by `configure`. `Some` until the first
    /// `configure`; `None` afterwards (the window is bound into the codec).
    window: Option<NativeWindow>,
    /// The running decoder, created lazily on the first `configure`.
    codec: Option<Codec>,
    /// Access units handed to `queueInputBuffer` so far (latency item A depth accounting).
    queued: Cell<u64>,
    /// Output buffers pulled from `dequeueOutputBuffer` so far (rendered or dropped).
    dequeued: Cell<u64>,
}
```

Update `MediaCodecDecoder::new` (lines 133-138):

```rust
    pub fn new(window: NativeWindow) -> Self {
        MediaCodecDecoder {
            window: Some(window),
            codec: None,
            queued: Cell::new(0),
            dequeued: Cell::new(0),
        }
    }
```

- [ ] **Step 2: Set the Exynos vendor low-latency key (item B)**

In `configure`, right after the generic `KEY_LOW_LATENCY` line (line 238), add the vendor key:

```rust
                sys::AMediaFormat_setInt32(format, sys::AMEDIAFORMAT_KEY_LOW_LATENCY, 1);
                // The Pixel 6a's decoder is `c2.exynos.h264.decoder` (Google Tensor), which
                // IGNORES the generic KEY_LOW_LATENCY above — the measured ~290+ ms input-queue
                // residence is the symptom. The Exynos C2 component honours this *vendor* key
                // instead (Moonlight sets per-SoC vendor keys for exactly this reason). Setting
                // an unknown vendor key on another decoder is silently ignored, so this is safe
                // to set unconditionally. Verify on-device via logcat (CCodec component name).
                const VENDOR_LOW_LATENCY_KEY: &CStr = c"vendor.rtc-ext-dec-low-latency.enable";
                sys::AMediaFormat_setInt32(format, VENDOR_LOW_LATENCY_KEY.as_ptr(), 1);
```

- [ ] **Step 3: Count queued inputs**

In `decode`, immediately after the successful `queueInputBuffer` check (`check(status, "queueInputBuffer")?;` at line 356), add:

```rust
            check(status, "queueInputBuffer")?;
            self.queued.set(self.queued.get() + 1);
```

- [ ] **Step 4: Count dequeued outputs in `drain_output`**

In `drain_output`, after the collection loop fills `ready` and before the `split_last` (just before line 431), add:

```rust
        // Depth accounting (latency item A): every dequeued output — whether we render it or
        // drop it as stale — leaves the codec's pipeline, so it counts against in-flight.
        self.dequeued.set(self.dequeued.get() + ready.len() as u64);
```

- [ ] **Step 5: Implement `in_flight()` and `pump()`**

Add these two methods to the `impl VideoDecoder for MediaCodecDecoder` block. Place them after `decode` (after line 364, before the closing `}` of the impl at line 365):

```rust
    fn in_flight(&self) -> usize {
        // Saturating: dequeued can briefly equal queued; never underflow.
        self.queued.get().saturating_sub(self.dequeued.get()) as usize
    }

    fn pump(&mut self) -> Result<Vec<PresentedFrame>, DecodeError> {
        // Drain ready output without submitting input — used when the session's pacer drops an
        // incoming frame but the codec should keep draining so the surface stays current.
        let codec = self
            .codec
            .as_ref()
            .ok_or_else(|| DecodeError::Adapter("pump called before configure".into()))?
            .ptr
            .as_ptr();
        let mut presented = Vec::new();
        self.drain_output(codec, &mut presented)?;
        Ok(presented)
    }
```

- [ ] **Step 6: Verify the host build is unaffected, then cross-compile type-check**

Run: `cargo test -p android-client` (host build — the mediacodec module is cfg'd out, so this must still pass with no change).
Expected: PASS.

Run the Android cross-compile type-check (the only way to type-check `mediacodec.rs`):

```bash
cargo ndk -t arm64-v8a check -p android-client --features live-decode
```

Expected: compiles clean. If `cargo ndk` isn't installed, fall back to:
`cargo check -p android-client --features live-decode --target aarch64-linux-android`
Expected: no errors.

Run: `cargo clippy -p android-client --features live-decode --target aarch64-linux-android -- -D warnings`
Expected: clean. Check `mediacodec.rs` for unused imports (the `Cell`/`CStr` additions must all be used).

- [ ] **Step 7: Commit**

```bash
git add crates/android-client/src/mediacodec.rs
git commit -m "feat(P5): AMediaCodec in_flight/pump depth counters + Exynos vendor low-latency key (items A,B)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Device build, measured run, and documentation

**Files:**
- Modify: `docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md` (append the result), `.planning/ROADMAP.md`, `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md` (§2 budget + P5.4 acceptance).

> The user runs `p5_stream` from their own terminal (Screen Recording grant) and reads the per-stage report. A process launched by Claude cannot capture the screen. So the build/install is automatable, but the **run is the user's** — present the build command and ask them to run it and paste the report.

- [ ] **Step 1: Build + install the APK with the decode feature**

Per memory `rustscreen-apk-build.md` (needs `--features live-decode`, JDK 17–21, and `android/local.properties` with `sdk.dir`):

```bash
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build --release -p android-client --features live-decode \
  && (cd android && ./gradlew assembleDebug) \
  && adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

Expected: `.so` built, Gradle `BUILD SUCCESSFUL`, `Success` from `adb install`.

- [ ] **Step 2: Verify the new code is in the installed `.so`**

Confirm the binary contains the new symbols/strings (sanity that the right build is installed):

```bash
adb shell run-as com.rustscreen.client cat lib/arm64/librustscreen_client.so 2>/dev/null | strings | grep -i "rtc-ext-dec-low-latency" \
  || strings android/app/src/main/jniLibs/arm64-v8a/librustscreen_client.so | grep -i "rtc-ext-dec-low-latency"
```

Expected: the vendor key string is present (proves the live-decode build with item B is installed, not the echo fallback).

- [ ] **Step 3: Ask the user to run the live capture and paste the report**

The user runs (from their own terminal):

```bash
cargo run --release -p macos-host --features "live-capture live-usb" --bin p5_stream
```

Then, in a second terminal, capture the phone-side drop/depth log during the run:

```bash
adb logcat -s RustScreen:* | grep -i "input-dropped"
```

Ask the user to paste: the `p5_stream` per-stage report (especially `arrive→decode` and `GLASS→GLASS`) **and** the `input-dropped` count. Success criterion: `arrive→decode` collapses from ~500 ms toward single/low-double-digit ms, and `GLASS→GLASS` p50 < 50 ms.

- [ ] **Step 4: Record the measured result in the research doc**

Append a "## 6. Result after input pacing (item A+B)" section to `docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md` with the before/after per-stage table from the run and whether <50 ms was met.

- [ ] **Step 5: Update the roadmap acceptance + budget**

- In `.planning/ROADMAP.md`: set P5 criterion #2 to MET (if <50 ms) or update the partial figure with the new number; mark item-6 step-2 done.
- In `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md` §2: add the post-pacing row to the latency budget table; update P5.4 acceptance for criterion #2.

- [ ] **Step 6: Commit the docs**

```bash
git add docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md .planning/ROADMAP.md docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md
git commit -m "docs(P5): record glass-to-glass result after input pacing (item 6 step 2)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Fallback if `arrive→decode` is still high after Task 5

If pacing bounds `input_frames_dropped` (it climbs) but `arrive→decode` is *still* high, the depth is not where we think (or `in_flight()` is mis-counting). Do NOT add a sixth guess — return to the research doc's categories **D** (vsync decouple: switch `releaseOutputBuffer(idx, true)` to the timestamp form `releaseOutputBuffer(idx, nanoTime)`) and **C** (split receive/decode/render into threads with bounded drop-oldest queues). Those are a separate plan; capture the new measurement first so the next step is evidence-driven.

---

## Self-Review

**Spec coverage (vs the research doc's recommended order H → A → B → G):**
- **H** (instrument in-flight depth): Task 3 adds `input_frames_dropped` to the summary + JNI log; Task 4 adds real `in_flight()` depth counting. ✓
- **A** (bound input queue + input-side drop-to-keyframe): Task 1 (pure pacer) + Task 3 (wiring) + Task 4 (real depth + `pump`). ✓
- **B** (Exynos vendor key): Task 4 Step 2. ✓
- **G** (drain startup burst): handled implicitly by A — at session start `in_flight` rises with the burst, the pacer trips at the cap and drops to the next keyframe (≤ 1 s away given IDR/60). Noted, no separate task needed. ✓

**Placeholder scan:** No TBD/TODO; every code step shows complete code; commands have expected output. ✓

**Type consistency:** `InputPacer::new(usize)` / `admit(bool, usize) -> bool` used identically in Tasks 1 and 3. `VideoDecoder::in_flight(&self) -> usize` and `pump(&mut self) -> Result<Vec<PresentedFrame>, DecodeError>` defined in Task 2, overridden with the same signatures in Task 4, scripted in the Task 3 fake. `SessionSummary.input_frames_dropped: u64` added in Task 3 and read in Task 3 Step 6. `MAX_IN_FLIGHT: usize`. ✓
