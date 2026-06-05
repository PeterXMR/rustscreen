# Glass-to-Glass Latency Instrumentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure real per-stage and glass-to-glass latency of the live RustScreen pipeline (Mac capture → Pixel present), logged every run, to close P5 Success Criterion #2 (< 50 ms). Measurement only — no tuning.

**Architecture:** A one-time SNTP-style clock-offset handshake bridges the Mac and phone clocks. Each `Frame::Video` is correlated by its existing `pts_us` (no wire change to `Video`). The host keeps its per-frame stage timestamps in a bounded map keyed by `pts_us`; the phone reports its arrive/decode/present times back in a new `Frame::Stats`; the host fuses them (offset-corrected) into a per-stage + glass-to-glass report. Three new *additive* protocol frames (`ClockPing`/`ClockPong`/`Stats`).

**Tech Stack:** Rust workspace (`protocol`, `macos-host`, `android-client`), `postcard` for structured frame payloads, existing `framing` length-prefix codec, existing `encode::LatencyStats` (count/min/max/mean/percentile). Cargo tests in CI; live wiring behind the existing `live-capture`/`live-usb` features, verified hands-on (Mac + Pixel 6a, now permanently connected).

**Refinements vs. the approved spec** (all reduce scope, none change behavior):
- Correlate frames by the **existing `pts_us`** instead of adding `frame_id`+`capture_micros` to `Frame::Video`. The phone already carries `pts_us` through `DecoderInput`/MediaCodec `presentationTimeUs`, and the host retains its own capture/encode/send times locally — so nothing new needs to ride on the hot `Video` frame.
- **Reuse** `encode::LatencyStats` (it already has p50/p95) for each stage, rather than writing a new stats type.

**Test commands:**
- Protocol: `cargo test -p protocol`
- Host (pure): `cargo test -p macos-host`
- Android (host-side pure tests): `cargo test -p android-client`
- Whole workspace: `cargo test --workspace`
- Lint gate (must stay clean): `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

---

## File Structure

**Create:**
- `crates/protocol/src/clock.rs` — pure SNTP offset math (`ClockOffset`, `estimate`).

**Modify:**
- `crates/protocol/src/lib.rs` — add `pub mod clock;`.
- `crates/protocol/src/messages.rs` — add tags 6/7/8; `Frame::{ClockPing, ClockPong, Stats}`; encode/decode arms; round-trip tests.
- `crates/macos-host/src/latency.rs` — add `PipelineLatency` (per-stage `LatencyStats` + bounded `pts_us` map + `record_host`/`record_stats`/`report`).
- `crates/macos-host/src/session.rs` — add `perform_clock_sync` (ping/pong exchange, injectable clock) + `ClockSyncError`.
- `crates/android-client/src/session.rs` — add `StatsTracker` (pure per-`pts_us` arrive/decode/present bookkeeping → `Frame::Stats`) + a `pong_for_ping` helper.
- `crates/macos-host/src/bin/p5_stream.rs` — **live wiring** (behind `live-capture,live-usb`): split transport, clock-sync on connect, stats-reader thread, host-side per-frame timing, print report.
- `crates/android-client` live decode path (`rendezvous.rs`/`mediacodec.rs`/`session.rs`) — **live wiring**: stamp arrive/decode/present, answer `ClockPing`, send `Frame::Stats`.
- `docs/superpowers/plans/...` architecture §2 budget — record measured numbers (final task).

---

## Task 1: `protocol::clock` — pure offset math

**Files:**
- Create: `crates/protocol/src/clock.rs`
- Modify: `crates/protocol/src/lib.rs` (add `pub mod clock;` after `pub mod nal;`)

- [ ] **Step 1: Declare the module**

In `crates/protocol/src/lib.rs`, add after line `pub mod nal;`:

```rust
pub mod clock;
```

- [ ] **Step 2: Write the failing test**

Create `crates/protocol/src/clock.rs` with ONLY the type, a `todo!()` body, and tests:

```rust
//! Pure SNTP-style clock-offset estimation between the host (Mac) and client (Pixel).
//!
//! The two devices run independent monotonic clocks. To express a phone-clock timestamp
//! in host-clock terms (needed for glass-to-glass latency), we estimate the offset with a
//! four-timestamp exchange, exactly like NTP:
//!
//! ```text
//! host sends ClockPing at t0 (host clock)
//! phone receives it at   t1 (phone clock)
//! phone sends ClockPong at t2 (phone clock)
//! host receives it at    t3 (host clock)
//! ```
//!
//! `offset = ((t1 - t0) + (t2 - t3)) / 2`  (phone_clock - host_clock)
//! `rtt    = (t3 - t0) - (t2 - t1)`
//!
//! To convert a phone timestamp `p` to host time: `p - offset`.

/// The result of one clock-offset estimation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockOffset {
    /// phone_clock − host_clock, in microseconds. Add to a host time to get phone time;
    /// subtract from a phone time to get host time. Signed: the phone may be behind.
    pub offset_us: i64,
    /// Round-trip time of the exchange, in microseconds. The offset error is bounded by
    /// `±rtt_us / 2`; callers log it as the precision caveat on any fused number.
    pub rtt_us: u64,
}

/// Estimate the clock offset from the four exchange timestamps (all microseconds).
///
/// Computed in `i128` so neither the subtraction nor the sum can overflow/underflow for
/// any `u64` inputs. `rtt` is clamped to `>= 0` (a non-monotonic capture would otherwise
/// produce a negative round-trip, which is meaningless).
pub fn estimate(t0: u64, t1: u64, t2: u64, t3: u64) -> ClockOffset {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_delay_zero_offset() {
        // Up delay == down delay, phone clock == host clock → offset 0, rtt = 2*delay.
        // t0=0, t1=100, t2=110 (10us server hold), t3=210.
        let o = estimate(0, 100, 110, 210);
        assert_eq!(o.offset_us, 0);
        assert_eq!(o.rtt_us, 200);
    }

    #[test]
    fn known_offset_symmetric_delay() {
        // Delay D=100 each way, phone ahead by O=50, zero server hold:
        // t0=0, t1=D+O=150, t2=150, t3=2D=200.
        let o = estimate(0, 150, 150, 200);
        assert_eq!(o.offset_us, 50);
        assert_eq!(o.rtt_us, 200);
    }

    #[test]
    fn phone_behind_negative_offset() {
        // Mirror of the above with phone behind by 50: O=-50.
        // t0=0, t1=D+O=50, t2=50, t3=200.
        let o = estimate(0, 50, 50, 200);
        assert_eq!(o.offset_us, -50);
        assert_eq!(o.rtt_us, 200);
    }

    #[test]
    fn rtt_excludes_server_hold() {
        // Server holds 40us (t2-t1=40); rtt must subtract it: (t3-t0)-(t2-t1).
        // t0=0, t1=100, t2=140, t3=200 → rtt = 200 - 40 = 160.
        let o = estimate(0, 100, 140, 200);
        assert_eq!(o.rtt_us, 160);
    }

    #[test]
    fn non_monotonic_clamps_rtt_to_zero() {
        // Degenerate inputs (hold longer than the whole round-trip) must not produce a
        // negative rtt; clamp to 0 rather than panic or wrap.
        let o = estimate(0, 100, 500, 200);
        assert_eq!(o.rtt_us, 0);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p protocol clock::`
Expected: FAIL — `todo!()` panics in every test.

- [ ] **Step 4: Implement `estimate`**

Replace the `todo!()` body with:

```rust
    let (t0, t1, t2, t3) = (t0 as i128, t1 as i128, t2 as i128, t3 as i128);
    let offset = ((t1 - t0) + (t2 - t3)) / 2;
    let rtt = (t3 - t0) - (t2 - t1);
    ClockOffset {
        offset_us: offset as i64,
        rtt_us: rtt.max(0) as u64,
    }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p protocol clock::`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/protocol/src/clock.rs crates/protocol/src/lib.rs
git commit -m "feat(protocol): pure SNTP clock-offset estimation for latency instrumentation"
```

---

## Task 2: `protocol::messages` — `ClockPing`/`ClockPong`/`Stats` frames

**Files:**
- Modify: `crates/protocol/src/messages.rs`

- [ ] **Step 1: Write the failing round-trip test**

In `crates/protocol/src/messages.rs`, inside `mod tests`, add (the `roundtrip` helper already exists there):

```rust
    #[test]
    fn roundtrip_clock_ping() {
        let frame = Frame::ClockPing { t0_us: 0x0102_0304_0506_0708 };
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn roundtrip_clock_pong() {
        let frame = Frame::ClockPong { t0_us: 1, t1_us: 2, t2_us: 3 };
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn roundtrip_stats() {
        let frame = Frame::Stats {
            pts_us: 42,
            arrive_us: 1000,
            decode_us: 1010,
            present_us: 1025,
        };
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn new_tags_do_not_collide_with_existing() {
        // Append-only registry: the three new tags must be distinct from 1..=5.
        let tags = [
            tag::HANDSHAKE, tag::VIDEO_CONFIG, tag::VIDEO, tag::TOUCH, tag::CONTROL,
            tag::CLOCK_PING, tag::CLOCK_PONG, tag::STATS,
        ];
        let mut seen = std::collections::HashSet::new();
        for t in tags {
            assert!(seen.insert(t), "duplicate tag {t}");
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p protocol messages::tests::roundtrip_clock_ping`
Expected: FAIL — `Frame::ClockPing` and `tag::CLOCK_PING` do not exist (compile error).

- [ ] **Step 3: Add the tag constants**

In the `pub mod tag` block, after `pub const CONTROL: u8 = 5;`:

```rust
    /// [`Frame::ClockPing`]
    pub const CLOCK_PING: u8 = 6;
    /// [`Frame::ClockPong`]
    pub const CLOCK_PONG: u8 = 7;
    /// [`Frame::Stats`]
    pub const STATS: u8 = 8;
```

- [ ] **Step 4: Add the payload structs and `Frame` variants**

After the `TouchEvent` struct, add the two structured payloads (postcard-encoded, like `VideoConfigPayload`):

```rust
/// On-wire payload of [`Frame::ClockPong`]. Named struct so the postcard wire shape is
/// pinned in one place (same rationale as [`VideoConfigPayload`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ClockPongPayload {
    t0_us: u64,
    t1_us: u64,
    t2_us: u64,
}

/// On-wire payload of [`Frame::Stats`] — the client's per-frame timing report, keyed by the
/// `pts_us` it received on the corresponding [`Frame::Video`]. All times are in the client's
/// (phone) clock; the host converts them with the estimated [`crate::clock::ClockOffset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct StatsPayload {
    pts_us: u64,
    arrive_us: u64,
    decode_us: u64,
    present_us: u64,
}
```

In the `pub enum Frame` block, after the `Control(Control)` variant:

```rust
    /// Host → client clock-sync probe. `t0_us` is the host send time (host clock).
    ClockPing {
        /// Host send time, microseconds (host clock).
        t0_us: u64,
    },
    /// Client → host clock-sync reply carrying the host's `t0_us` plus the client's
    /// receive (`t1_us`) and send (`t2_us`) times (client clock). Feeds
    /// [`crate::clock::estimate`].
    ClockPong {
        /// Echoed host send time.
        t0_us: u64,
        /// Client receive time, microseconds (client clock).
        t1_us: u64,
        /// Client send time, microseconds (client clock).
        t2_us: u64,
    },
    /// Client → host per-frame timing report (client clock), correlated by `pts_us`.
    Stats {
        /// The `pts_us` of the `Frame::Video` this report is for.
        pts_us: u64,
        /// Time the full access unit finished arriving (client clock).
        arrive_us: u64,
        /// Time decode produced the output frame (client clock).
        decode_us: u64,
        /// Time the frame was released to the surface for display (client clock).
        present_us: u64,
    },
```

- [ ] **Step 5: Add the encode arms**

In `to_tag_payload`, before the closing `}` of the `match self`, add:

```rust
            Frame::ClockPing { t0_us } => {
                let payload = postcard::to_allocvec(t0_us).map_err(MessageError::Encode)?;
                Ok((tag::CLOCK_PING, payload))
            }
            Frame::ClockPong { t0_us, t1_us, t2_us } => {
                let payload = postcard::to_allocvec(&ClockPongPayload {
                    t0_us: *t0_us,
                    t1_us: *t1_us,
                    t2_us: *t2_us,
                })
                .map_err(MessageError::Encode)?;
                Ok((tag::CLOCK_PONG, payload))
            }
            Frame::Stats { pts_us, arrive_us, decode_us, present_us } => {
                let payload = postcard::to_allocvec(&StatsPayload {
                    pts_us: *pts_us,
                    arrive_us: *arrive_us,
                    decode_us: *decode_us,
                    present_us: *present_us,
                })
                .map_err(MessageError::Encode)?;
                Ok((tag::STATS, payload))
            }
```

- [ ] **Step 6: Add the decode arms**

In `decode`, before the `other => Err(MessageError::UnknownTag(other))` arm:

```rust
            tag::CLOCK_PING => Ok(Frame::ClockPing {
                t0_us: decode_canonical(payload)?,
            }),
            tag::CLOCK_PONG => {
                let p: ClockPongPayload = decode_canonical(payload)?;
                Ok(Frame::ClockPong {
                    t0_us: p.t0_us,
                    t1_us: p.t1_us,
                    t2_us: p.t2_us,
                })
            }
            tag::STATS => {
                let p: StatsPayload = decode_canonical(payload)?;
                Ok(Frame::Stats {
                    pts_us: p.pts_us,
                    arrive_us: p.arrive_us,
                    decode_us: p.decode_us,
                    present_us: p.present_us,
                })
            }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p protocol messages::`
Expected: PASS — the 4 new tests plus all existing `messages` tests still green.

- [ ] **Step 8: Commit**

```bash
git add crates/protocol/src/messages.rs
git commit -m "feat(protocol): add ClockPing/ClockPong/Stats frames (tags 6-8) for latency instrumentation"
```

---

## Task 3: `macos-host::latency` — `PipelineLatency` accumulator

**Files:**
- Modify: `crates/macos-host/src/latency.rs`

The pipeline has six measured intervals. The host knows `capture`/`encode_done`/`send_done` (host clock); the phone reports `arrive`/`decode`/`present` (phone clock, converted via offset). Glass-to-glass = `present(host) - capture`.

- [ ] **Step 1: Write the failing test**

In `crates/macos-host/src/latency.rs`, inside `mod tests`, add:

```rust
    use crate::encode::LatencyStats; // percentile-capable per-stage stats

    fn offset(us: i64) -> protocol::clock::ClockOffset {
        protocol::clock::ClockOffset { offset_us: us, rtt_us: 0 }
    }

    #[test]
    fn fuses_host_and_phone_into_glass_to_glass() {
        let mut p = PipelineLatency::new(16);
        // Host stages for pts 1000 (host clock, µs): cap=0, enc=8, send=10.
        p.record_host(1000, 0, 8, 10);
        // Phone stages (phone clock): arrive=110, decode=120, present=130, offset=+100.
        // Converted to host clock: arrive=10, decode=20, present=30.
        p.record_stats(1000, 110, 120, 130, offset(100));
        let r = p.report();
        assert_eq!(r.glass_to_glass.count(), 1);
        // present_host(30) - capture(0) = 30 µs.
        assert_eq!(r.glass_to_glass.max(), Some(30));
        // capture→encode = 8, encode→send = 2, send→arrive = 10-? send=10 host, arrive_host=10 → 0
        assert_eq!(r.capture_to_encode.max(), Some(8));
        assert_eq!(r.encode_to_send.max(), Some(2));
        assert_eq!(r.send_to_arrive.max(), Some(0));
        assert_eq!(r.arrive_to_decode.max(), Some(10));
        assert_eq!(r.decode_to_present.max(), Some(10));
    }

    #[test]
    fn stats_without_matching_host_record_is_dropped() {
        let mut p = PipelineLatency::new(16);
        p.record_stats(999, 100, 110, 120, offset(0)); // no host record for 999
        assert_eq!(p.report().glass_to_glass.count(), 0);
    }

    #[test]
    fn bounded_map_evicts_oldest() {
        let mut p = PipelineLatency::new(2); // capacity 2 in-flight
        p.record_host(1, 0, 1, 2);
        p.record_host(2, 0, 1, 2);
        p.record_host(3, 0, 1, 2); // evicts pts 1
        // Stats for the evicted pts 1 finds nothing → dropped.
        p.record_stats(1, 10, 11, 12, offset(0));
        // Stats for the retained pts 3 fuses.
        p.record_stats(3, 10, 11, 12, offset(0));
        assert_eq!(p.report().glass_to_glass.count(), 1);
    }

    #[test]
    fn negative_interval_clamps_to_zero_and_counts_anomaly() {
        let mut p = PipelineLatency::new(16);
        // present_host (50-100 = -50) is before capture (0) due to clock jitter → clamp.
        p.record_host(1, 0, 1, 2);
        p.record_stats(1, 50, 50, 50, offset(100)); // present_host = 50-100 = -50
        let r = p.report();
        assert_eq!(r.glass_to_glass.max(), Some(0), "negative G2G clamps to 0");
        assert_eq!(r.anomalies, 1);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p macos-host latency::tests::fuses_host_and_phone`
Expected: FAIL — `PipelineLatency` does not exist (compile error).

- [ ] **Step 3: Implement `PipelineLatency`**

At the top of `crates/macos-host/src/latency.rs`, after the existing module docs / before `mod tests`, add:

```rust
use std::collections::VecDeque;

use crate::encode::LatencyStats;
use protocol::clock::ClockOffset;

/// Host-side per-frame stage timestamps (host clock, microseconds), held until the
/// matching phone [`protocol::messages::Frame::Stats`] arrives.
#[derive(Debug, Clone, Copy)]
struct HostStamps {
    pts_us: u64,
    capture_us: u64,
    encode_done_us: u64,
    send_done_us: u64,
}

/// A completed per-stage + glass-to-glass latency report (each field µs).
#[derive(Debug, Clone, Default)]
pub struct LatencyReport {
    pub capture_to_encode: LatencyStats,
    pub encode_to_send: LatencyStats,
    pub send_to_arrive: LatencyStats,
    pub arrive_to_decode: LatencyStats,
    pub decode_to_present: LatencyStats,
    pub glass_to_glass: LatencyStats,
    /// Count of samples with a negative computed interval (clock jitter), clamped to 0.
    pub anomalies: u64,
}

/// Fuses host-side and phone-side per-frame timestamps (correlated by `pts_us`) into a
/// per-stage + glass-to-glass [`LatencyReport`]. Host timestamps wait in a bounded FIFO
/// keyed by `pts_us` until the phone's `Frame::Stats` arrives (or they are evicted).
#[derive(Debug)]
pub struct PipelineLatency {
    inflight: VecDeque<HostStamps>,
    capacity: usize,
    report: LatencyReport,
}

impl PipelineLatency {
    /// New accumulator retaining at most `capacity` un-matched host records.
    pub fn new(capacity: usize) -> Self {
        Self {
            inflight: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
            report: LatencyReport::default(),
        }
    }

    /// Record the host-side stage times for one frame (host clock, µs). Evicts the oldest
    /// un-matched record when over capacity.
    pub fn record_host(&mut self, pts_us: u64, capture_us: u64, encode_done_us: u64, send_done_us: u64) {
        if self.inflight.len() >= self.capacity {
            self.inflight.pop_front();
        }
        self.inflight.push_back(HostStamps { pts_us, capture_us, encode_done_us, send_done_us });
    }

    /// Record the phone's `Frame::Stats` for one frame, converting its (phone-clock) times
    /// to host time via `offset`, then fusing with the matching host record. A report whose
    /// `pts_us` has no in-flight host record is dropped.
    pub fn record_stats(&mut self, pts_us: u64, arrive_us: u64, decode_us: u64, present_us: u64, offset: ClockOffset) {
        let Some(idx) = self.inflight.iter().position(|h| h.pts_us == pts_us) else {
            return;
        };
        let h = self.inflight.remove(idx).expect("index from position is valid");

        // Phone clock → host clock: host = phone - offset.
        let to_host = |phone: u64| phone as i128 - offset.offset_us as i128;
        let arrive_h = to_host(arrive_us);
        let decode_h = to_host(decode_us);
        let present_h = to_host(present_us);

        let mut anomaly = false;
        let mut gap = |stats: &mut LatencyStats, from: i128, to: i128| {
            let d = to - from;
            if d < 0 {
                anomaly = true;
                stats.record(0);
            } else {
                stats.record(d as u64);
            }
        };

        gap(&mut self.report.capture_to_encode, h.capture_us as i128, h.encode_done_us as i128);
        gap(&mut self.report.encode_to_send, h.encode_done_us as i128, h.send_done_us as i128);
        gap(&mut self.report.send_to_arrive, h.send_done_us as i128, arrive_h);
        gap(&mut self.report.arrive_to_decode, arrive_h, decode_h);
        gap(&mut self.report.decode_to_present, decode_h, present_h);
        gap(&mut self.report.glass_to_glass, h.capture_us as i128, present_h);

        if anomaly {
            self.report.anomalies += 1;
        }
    }

    /// The accumulated report so far (cheap clone of the stage stats).
    pub fn report(&self) -> LatencyReport {
        self.report.clone()
    }
}
```

Note: `LatencyReport` derives `Default`, which requires `LatencyStats: Default` — it already does (`#[derive(... Default ...)]` in `encode.rs`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p macos-host latency::`
Expected: PASS — the 4 new tests plus the existing `LatencyAccum` tests.

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/latency.rs
git commit -m "feat(macos-host): PipelineLatency fuses host+phone per-frame times into per-stage/glass-to-glass report"
```

---

## Task 4: `macos-host::session::perform_clock_sync`

**Files:**
- Modify: `crates/macos-host/src/session.rs`

The host writes `ClockPing { t0 }`, reads `ClockPong { t0, t1, t2 }`, stamps `t3`, returns the `ClockOffset`. The clock is injected (`now_us: impl FnMut() -> u64`) so the exchange is deterministically testable with an in-memory transport.

- [ ] **Step 1: Write the failing test**

In `crates/macos-host/src/session.rs` `mod tests`, add:

```rust
    use protocol::clock::ClockOffset;

    #[test]
    fn clock_sync_round_trips_and_estimates_offset() {
        // The "phone" pre-writes the ClockPong it will reply with; the host reads it.
        // We script the host clock to return t0=0 on the ping, t3=200 on receipt.
        let mut pong = Vec::new();
        Frame::ClockPong { t0_us: 0, t1_us: 150, t2_us: 150 }
            .write_to(&mut pong)
            .unwrap();

        // A duplex fake: reads come from `pong`, writes are discarded into `sink`.
        let mut transport = DuplexFake::new(pong);

        let mut times = [0u64, 200].into_iter();
        let offset = perform_clock_sync(&mut transport, || times.next().unwrap()).unwrap();

        assert_eq!(offset, ClockOffset { offset_us: 50, rtt_us: 200 });
        // The host must have written exactly one ClockPing carrying t0=0.
        let mut cur = std::io::Cursor::new(transport.written());
        assert_eq!(
            Frame::read_from(&mut cur).unwrap(),
            Frame::ClockPing { t0_us: 0 }
        );
    }

    #[test]
    fn clock_sync_rejects_unexpected_reply() {
        let mut not_a_pong = Vec::new();
        Frame::Control(Control::Heartbeat).write_to(&mut not_a_pong).unwrap();
        let mut transport = DuplexFake::new(not_a_pong);
        let mut times = [0u64, 1].into_iter();
        let err = perform_clock_sync(&mut transport, || times.next().unwrap());
        assert!(matches!(err, Err(ClockSyncError::UnexpectedReply)));
    }
```

Add this minimal duplex fake to the test module (if no equivalent already exists there):

```rust
    /// A `Read + Write` that serves canned bytes on read and captures writes.
    struct DuplexFake {
        to_read: std::io::Cursor<Vec<u8>>,
        written: Vec<u8>,
    }
    impl DuplexFake {
        fn new(to_read: Vec<u8>) -> Self {
            Self { to_read: std::io::Cursor::new(to_read), written: Vec::new() }
        }
        fn written(&self) -> &[u8] {
            &self.written
        }
    }
    impl std::io::Read for DuplexFake {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.to_read.read(buf)
        }
    }
    impl std::io::Write for DuplexFake {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
```

(If `mod tests` already imports `std::io::Read`/`Write` via `use`, drop the redundant `std::io::` qualifiers to satisfy clippy.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p macos-host session::tests::clock_sync_round_trips`
Expected: FAIL — `perform_clock_sync` / `ClockSyncError` do not exist.

- [ ] **Step 3: Implement `perform_clock_sync` + `ClockSyncError`**

In `crates/macos-host/src/session.rs` (top-level, near `perform_handshake`):

```rust
/// Why a clock-sync exchange failed.
#[derive(Debug)]
pub enum ClockSyncError {
    /// The reply frame was not a `ClockPong`.
    UnexpectedReply,
    /// A framing/codec error reading or writing the exchange.
    Message(protocol::messages::MessageError),
}

impl std::fmt::Display for ClockSyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClockSyncError::UnexpectedReply => write!(f, "clock-sync reply was not a ClockPong"),
            ClockSyncError::Message(e) => write!(f, "clock-sync I/O: {e}"),
        }
    }
}
impl std::error::Error for ClockSyncError {}
impl From<protocol::messages::MessageError> for ClockSyncError {
    fn from(e: protocol::messages::MessageError) -> Self {
        ClockSyncError::Message(e)
    }
}

/// Run one SNTP-style clock-sync exchange over `transport`, returning the estimated
/// [`ClockOffset`]. `now_us` supplies host-clock microseconds (injected for testability):
/// called once for `t0` (before the ping) and once for `t3` (after the pong).
pub fn perform_clock_sync(
    transport: &mut (impl std::io::Read + std::io::Write),
    mut now_us: impl FnMut() -> u64,
) -> Result<protocol::clock::ClockOffset, ClockSyncError> {
    let t0 = now_us();
    Frame::ClockPing { t0_us: t0 }.write_to(transport)?;
    transport.flush().map_err(|e| ClockSyncError::Message(e.into()))?;
    let reply = Frame::read_from(transport)?;
    let t3 = now_us();
    let Frame::ClockPong { t0_us, t1_us, t2_us } = reply else {
        return Err(ClockSyncError::UnexpectedReply);
    };
    Ok(protocol::clock::estimate(t0_us, t1_us, t2_us, t3))
}
```

Ensure `Frame` and `Control` are in scope in the test module (they are used by existing session tests; reuse the existing imports).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p macos-host session::`
Expected: PASS — the 2 new tests plus all existing session tests.

- [ ] **Step 5: Commit**

```bash
git add crates/macos-host/src/session.rs
git commit -m "feat(macos-host): perform_clock_sync exchange returning estimated ClockOffset"
```

---

## Task 5: Android pure stats bookkeeping (`StatsTracker` + `pong_for_ping`)

**Files:**
- Modify: `crates/android-client/src/session.rs`

Pure, host-testable logic the live decode loop will call: track per-`pts_us` arrive/decode/present times, emit a `Frame::Stats`, and turn a `ClockPing` + receive/send times into a `ClockPong`.

- [ ] **Step 1: Write the failing test**

In `crates/android-client/src/session.rs` `mod tests`, add:

```rust
    use protocol::messages::Frame;

    #[test]
    fn stats_tracker_builds_stats_frame_for_pts() {
        let mut t = StatsTracker::new(8);
        t.on_arrive(1000, 50);
        t.on_decode(1000, 60);
        let frame = t.on_present(1000, 75).expect("complete record yields a Stats frame");
        assert_eq!(
            frame,
            Frame::Stats { pts_us: 1000, arrive_us: 50, decode_us: 60, present_us: 75 }
        );
    }

    #[test]
    fn present_without_arrive_yields_none() {
        let mut t = StatsTracker::new(8);
        // No on_arrive for 2000.
        assert!(t.on_present(2000, 75).is_none());
    }

    #[test]
    fn pong_for_ping_carries_receive_and_send_times() {
        let pong = pong_for_ping(Frame::ClockPing { t0_us: 7 }, 100, 105);
        assert_eq!(pong, Some(Frame::ClockPong { t0_us: 7, t1_us: 100, t2_us: 105 }));
        // A non-ping frame yields None.
        assert!(pong_for_ping(Frame::Control(protocol::messages::Control::Bye), 1, 2).is_none());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p android-client session::tests::stats_tracker_builds`
Expected: FAIL — `StatsTracker` / `pong_for_ping` do not exist.

- [ ] **Step 3: Implement `StatsTracker` + `pong_for_ping`**

In `crates/android-client/src/session.rs` (top-level):

```rust
use std::collections::VecDeque;

/// Phone-side per-frame timing, keyed by `pts_us`, accumulated as a frame moves through
/// arrive → decode → present. On `present` it produces the `Frame::Stats` to send to the
/// host. Bounded FIFO so a frame that never presents (dropped by the decoder) cannot leak.
pub struct StatsTracker {
    inflight: VecDeque<(u64, u64, Option<u64>)>, // (pts_us, arrive_us, decode_us)
    capacity: usize,
}

impl StatsTracker {
    /// New tracker retaining at most `capacity` in-flight frames.
    pub fn new(capacity: usize) -> Self {
        Self { inflight: VecDeque::with_capacity(capacity), capacity: capacity.max(1) }
    }

    /// Record arrival of the full access unit for `pts_us` (phone clock, µs).
    pub fn on_arrive(&mut self, pts_us: u64, arrive_us: u64) {
        if self.inflight.len() >= self.capacity {
            self.inflight.pop_front();
        }
        self.inflight.push_back((pts_us, arrive_us, None));
    }

    /// Record decode completion for `pts_us`.
    pub fn on_decode(&mut self, pts_us: u64, decode_us: u64) {
        if let Some(e) = self.inflight.iter_mut().find(|(p, ..)| *p == pts_us) {
            e.2 = Some(decode_us);
        }
    }

    /// Record present for `pts_us` and, if arrive+decode were seen, produce the `Frame::Stats`.
    pub fn on_present(&mut self, pts_us: u64, present_us: u64) -> Option<protocol::messages::Frame> {
        let idx = self.inflight.iter().position(|(p, ..)| *p == pts_us)?;
        let (_, arrive_us, decode) = self.inflight.remove(idx)?;
        let decode_us = decode?;
        Some(protocol::messages::Frame::Stats { pts_us, arrive_us, decode_us, present_us })
    }
}

/// If `frame` is a `ClockPing`, build the matching `ClockPong` from the client's receive
/// time `t1_us` and send time `t2_us` (client clock, µs). Otherwise `None`.
pub fn pong_for_ping(
    frame: protocol::messages::Frame,
    t1_us: u64,
    t2_us: u64,
) -> Option<protocol::messages::Frame> {
    match frame {
        protocol::messages::Frame::ClockPing { t0_us } => {
            Some(protocol::messages::Frame::ClockPong { t0_us, t1_us, t2_us })
        }
        _ => None,
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p android-client session::`
Expected: PASS — the 3 new tests plus existing session tests.

- [ ] **Step 5: Verify cross-compile + lint, then commit**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test --workspace
git add crates/android-client/src/session.rs
git commit -m "feat(android-client): pure StatsTracker + pong_for_ping for latency reporting"
```

Expected: clippy clean, fmt clean, whole workspace green. This is the end of the **cable-free, CI-green** core (Tasks 1–5).

---

## Task 6: Host live wiring (`p5_stream`) — behind `live-capture,live-usb`

**Files:**
- Modify: `crates/macos-host/src/bin/p5_stream.rs`
- Possibly Modify: `crates/macos-host/src/aoa.rs` (expose split read/write halves if not already)

This is **hands-on-Mac** glue (the pure logic it calls is already tested in Tasks 1–4). The phone reports `Stats` while the host streams video, so the host must read inbound frames concurrently with writing — use a dedicated reader thread on the transport's read half.

- [ ] **Step 1: Ensure the AOA transport can split into read + write halves**

In `crates/macos-host/src/aoa.rs`, confirm `AoaTransport` wraps separate `nusb` `EndpointRead`/`EndpointWrite` (it does — see `BULK_TRANSFER_SIZE` doc in `protocol/lib.rs`). Add a method:

```rust
/// Split into independent read and write halves so the host can read `Frame::Stats`
/// on one thread while streaming video on another. Each half owns its own endpoint.
pub fn split(self) -> (AoaReadHalf, AoaWriteHalf) { /* move the two endpoints out */ }
```

If the endpoints are not separable without a larger refactor, fall back to the simpler design in Step 4's note.

- [ ] **Step 2: Run clock-sync immediately after the handshake**

After `perform_handshake` succeeds in `main`, before the capture loop:

```rust
let offset = macos_host::session::perform_clock_sync(&mut transport, monotonic_now_us)
    .map(Some)
    .unwrap_or_else(|e| {
        eprintln!("p5_stream: clock-sync failed ({e}); host-only stage timings, no glass-to-glass");
        None
    });
```

where `monotonic_now_us` reads a process-monotonic clock (`std::time::Instant` since a fixed start, as microseconds).

- [ ] **Step 3: Spawn the stats-reader thread**

Move the read half into a thread that loops `Frame::read_from`; for each `Frame::Stats`, send it down an `mpsc::Sender<Frame>` to the main loop; for a `Frame::ClockPing` (re-sync), reply via a shared write handle. Exit on transport error.

- [ ] **Step 4: Record host timestamps + drain stats in the stream loop**

In the existing `run_stream_session` send loop (or a thin wrapper in the bin), for each encoded frame stamp `capture_us`/`encode_done_us`/`send_done_us` (monotonic) and call `pipeline.record_host(frame.pts_us, …)`. After each send, non-blockingly drain the stats channel: `while let Ok(Frame::Stats{..}) = stats_rx.try_recv() { pipeline.record_stats(.., offset?) }`.

> **Fallback (if Step 1 split is infeasible):** keep the host write-only and have the **phone** compute glass-to-glass locally (host sends `capture_us` alongside each frame via a side `Frame` keyed by `pts_us`, phone logs the fused number to logcat). Record this deviation in the spec if taken.

- [ ] **Step 5: Print the report periodically and on exit**

Every ~2 s and once after the loop ends, print `pipeline.report()` formatted as: per-stage `avg/min/max/p50/p95` (µs→ms) and the glass-to-glass line with the `rtt`/offset caveat. Build behind the live features.

- [ ] **Step 6: Build to verify it compiles (no device needed to compile)**

Run: `cargo build -p macos-host --release --features live-capture,live-usb`
Expected: builds clean.

- [ ] **Step 7: Commit**

```bash
git add crates/macos-host/src/bin/p5_stream.rs crates/macos-host/src/aoa.rs
git commit -m "feat(macos-host): wire live clock-sync + stats-reader + latency report into p5_stream"
```

---

## Task 7: Android live wiring — stamp times, answer pings, send Stats

**Files:**
- Modify: `crates/android-client/src/session.rs` / `mediacodec.rs` / `rendezvous.rs` (the live decode loop)

**Hands-on-device** glue calling the Task-5 pure helpers. Requires the Pixel 6a (connected).

- [ ] **Step 1:** In the live receive loop, capture a monotonic phone clock (µs). On a fully-received `Frame::Video`, call `tracker.on_arrive(pts_us, now)`. On `ClockPing`, reply with `pong_for_ping(frame, recv_now, send_now)` written back over the transport.
- [ ] **Step 2:** After `AMediaCodec` produces an output buffer for a `pts_us`, call `tracker.on_decode(pts_us, now)`. When releasing the buffer to the surface (present), call `tracker.on_present(pts_us, now)`; if it returns a `Frame::Stats`, write it to the host (off the decode hot path — e.g. a small bounded channel to the writer).
- [ ] **Step 3:** Cross-compile check: `cargo ndk -t arm64-v8a build -p android-client --features live-decode` (or the project's documented APK build — see memory "RustScreen APK build": needs `--features live-decode` and JDK 17–21).
- [ ] **Step 4: Commit**

```bash
git add crates/android-client/src/
git commit -m "feat(android-client): live arrive/decode/present timing, ClockPong reply, Stats send"
```

---

## Task 8: Live run + record measured numbers (closes P5 criterion #2)

**Files:**
- Modify: `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md` (§2 latency budget)
- Modify: `.planning/ROADMAP.md` (P5 criterion #2 status)

- [ ] **Step 1:** Build host (`--release --features live-capture,live-usb`) and the APK; run `p5_stream` from a terminal with Screen Recording permission; open the app on the connected Pixel; drag a window onto the new display so frames flow.
- [ ] **Step 2:** Capture the printed `LatencyReport`: per-stage `avg/p95` and glass-to-glass `avg/p95`, plus the `rtt`/offset caveat.
- [ ] **Step 3:** Write the real numbers into the architecture doc §2 budget (replace the estimated budget) and mark P5 criterion #2 in `.planning/ROADMAP.md` as met (or, if > 50 ms, record the number and which stage dominates — that names the PR #25 target).
- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md .planning/ROADMAP.md
git commit -m "docs(P5): record measured glass-to-glass latency; revise §2 budget with real numbers"
```

---

## Self-Review

**Spec coverage:**
- Clock-offset handshake → Task 1 (math) + Task 4 (host exchange) + Task 7 Step 1 (phone reply). ✓
- New wire frames → Task 2. ✓
- Per-stage + glass-to-glass aggregation isolating `send→arrive` and `arrive→decode→present` → Task 3. ✓ (those exact stage fields exist)
- Host wiring (handshake on connect, frame stamping, consume Stats off hot path) → Task 6. ✓
- Android wiring (pure TDD'd + live hooks) → Task 5 (pure) + Task 7 (live). ✓
- Graceful degradation (handshake failure → host-only timings) → Task 6 Step 2. ✓
- Bounded in-flight map + anomaly clamping → Task 3 Steps 1/3. ✓
- Revised §2 budget with real numbers → Task 8. ✓

**Placeholder scan:** Tasks 1–5 contain complete, runnable code. Tasks 6–8 are hands-on integration (objc2/JNI/device) and give exact call sites, the helper signatures from Tasks 1–5, exact commands, and a named fallback — consistent with how this repo specifies its Wave-B hands-on tasks.

**Type consistency:** `ClockOffset { offset_us: i64, rtt_us: u64 }`, `estimate(u64×4)`, `Frame::ClockPing{t0_us}`, `Frame::ClockPong{t0_us,t1_us,t2_us}`, `Frame::Stats{pts_us,arrive_us,decode_us,present_us}`, `PipelineLatency::{new,record_host,record_stats,report}`, `LatencyReport` stage fields, `StatsTracker::{new,on_arrive,on_decode,on_present}`, `pong_for_ping` — names match across Tasks 1→7. Conversion direction (host = phone − offset) is consistent in Task 3 impl and its test.
