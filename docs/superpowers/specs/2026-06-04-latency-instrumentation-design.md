# Design: Glass-to-Glass Latency Instrumentation (PR #24 / ladder item 6, Step 1)

**Status:** approved (brainstorming), pending spec review
**Phase:** P5 (Live End-to-End Pipeline + Latency) — closes Success Criterion #2
**Ladder item:** 6 ("Latency proof + stream tuning"), Step 1 only
**Branch:** `feat/p5-latency-instrumentation`

## Goal

Produce a real, automated, per-run measurement of **glass-to-glass latency** (capture on
the Mac → present on the Pixel) with a **per-stage breakdown**, logged every session, and
revise the §2 latency budget with the measured numbers. This closes the open P5
Success Criterion #2 ("Measured glass-to-glass latency is < 50 ms, with per-stage timings
logged").

**This PR measures. It does not tune.** Jitter buffer, drop-to-keyframe, and adaptive
bitrate are explicitly OUT of scope — they are follow-up PRs opened only against the
bottleneck the measurement actually reveals. PR #24 is the *baseline*: without it, no later
optimization can be proven "drastic" because there is no before/after.

## Why a baseline-only PR

To prove an optimization works you need a *before* number and an *after* number produced by
the **same harness**. Folding a speed fix into this PR captures only the post-fix state and
confounds which change caused what. Sequence: **#24 measures → #25 fixes the confirmed
bottleneck → re-run #24's harness → "X ms → Y ms".**

## The clock problem

The pipeline spans two devices with independent clocks:

| Stage | Clock | Timestamp |
|-------|-------|-----------|
| Capture (ScreenCaptureKit delivers frame) | Mac | `t_cap` |
| Encode done (VideoToolbox) | Mac | `t_enc` |
| Send done (USB write returns) | Mac | `t_send` |
| Arrive (full access unit received) | Phone | `t_arr` |
| Decode done (AMediaCodec output) | Phone | `t_dec` |
| Present (released to surface) | Phone | `t_pres` |

Glass-to-glass = `t_pres − t_cap`, but those are on different clocks. We bridge them with a
**clock-offset handshake** (SNTP-style) run once on connect:

```
host → ClockPing { t0 }                  (t0 = host send time)
phone records t1 = arrival (phone clock)
phone → ClockPong { t0, t1, t2 }         (t2 = phone send time)
host records t3 = arrival (host clock)

offset = ((t1 − t0) + (t2 − t3)) / 2     // phone_clock − host_clock
rtt    = (t3 − t0) − (t2 − t1)
```

Conversion: `present_host = t_pres − offset`, so `glass_to_glass = present_host − t_cap`.
Offset error is bounded by `±rtt/2`; over USB the RTT is small. The error bound is **logged
alongside** the number so it is never presented as more precise than it is.

## Components

Each unit has one purpose, a defined interface, and is testable in isolation.

### 1. `protocol::clock` (new module) — pure offset math
- `estimate(t0, t1, t2, t3) -> ClockOffset { offset_micros: i64, rtt_micros: u64 }`
- No I/O, no time source. Fully unit-tested with synthetic timestamps.

### 2. `protocol::messages` (extend) — new wire frames
- `Frame::ClockPing { t0: u64 }`
- `Frame::ClockPong { t0: u64, t1: u64, t2: u64 }`
- `Frame::Stats { frame_id: u64, arrive_micros: u64, decode_micros: u64, present_micros: u64 }` (phone → host)
- Extend `Frame::Video` with `frame_id: u64` and `capture_micros: u64`.
- This is a deliberate protocol bump; both ends are built from this repo and updated in
  lockstep. Bump the protocol version constant if one exists.
- Every new/changed frame gets a codec round-trip test (existing `messages` test pattern).

### 3. `latency` accumulator (new; generalizes today's `LatencyStats`)
- Ingests per-frame stage records, emits a `LatencyReport`: for each stage gap
  (capture→encode, encode→send, send→arrive, arrive→decode, decode→present) and for
  glass-to-glass, report avg / min / max / p50 / p95.
- Pure: synthetic records in → aggregates out. Unit-tested, including anomaly clamping.
- The report **isolates `send→arrive` and `arrive→decode→present`** so the known suspects
  (USB transfer granularity; Android MediaCodec output buffering) are unmissable on the
  first run.

### 4. Host wiring (`macos-host` `session.rs` / `p5_stream`, behind `live-*` features)
- Assign each encoded frame a monotonic `frame_id` and stamp `capture_micros`.
- Run the clock handshake on connect (before the stream loop).
- Consume inbound `Frame::Stats`, fuse with the offset + the host's own `t_cap/t_enc/t_send`,
  feed the accumulator.
- Print the `LatencyReport` periodically and on exit.

### 5. Android wiring (`android-client`)
- Pure timestamp bookkeeping + `Stats` builder are TDD'd host-side (no device).
- The arrive/decode/present hooks attach to the **existing live decode path** shipped in
  item 1. Respond to `ClockPing` with `ClockPong`; emit `Frame::Stats` per presented frame.

## Data flow

```
connect
  → clock handshake (host Ping → phone Pong → host computes offset)
per frame
  → host stamps (frame_id, capture_micros) into Frame::Video
  → phone records arrive / decode / present for that frame_id
  → phone sends Frame::Stats(frame_id, …)  (off the video hot path)
  → host fuses (t_cap/t_enc/t_send + offset-corrected phone times) into the accumulator
periodically + on exit
  → host logs LatencyReport (per-stage + glass-to-glass, with rtt/offset caveat)
```

## Error handling & non-interference

- **Handshake fails / times out** → degrade gracefully: host-only per-stage timings
  (capture→encode→send), no fused glass-to-glass; log a warning. The stream still runs.
- **`Stats` for an unknown/evicted `frame_id`** → drop. The in-flight map is bounded and
  evicts oldest entries (a slow/lost stats report must not leak memory).
- **Negative or non-monotonic computed latency** (clock jitter) → clamp to zero and count as
  an anomaly in the report, rather than skewing aggregates.
- **Stats must never block the video path.** The phone sends stats on its own light cadence;
  decode/present never waits on stats I/O. On the host, consuming stats must not stall the
  send loop.

## Testing strategy (TDD-first)

- `clock::estimate` — symmetric and asymmetric delay cases; zero-offset; known offset/rtt.
- `messages` codec round-trip — `ClockPing`, `ClockPong`, `Stats`, and extended `Video`.
- `latency` accumulator — synthetic per-frame records → asserted per-stage and glass-to-glass
  aggregates; anomaly clamping; bounded eviction.
- Android bookkeeping/`Stats` builder — record arrive/decode/present, build `Stats` (host test).
- Host session — fake transport + injected `Stats` stream → assert the assembled
  `LatencyReport`; extend existing `run_stream_session` tests.
- Live verification (hands-on Mac + phone, deferred per-run): run `p5_stream`, read the real
  numbers, write them into the revised §2 budget.

## Scope boundary

**IN:** clock-offset handshake, frame stamping, per-stage + glass-to-glass aggregation,
per-run logging, revised §2 latency budget with real numbers.

**OUT (follow-up PRs, gated on the measured bottleneck):** jitter buffer, drop-to-keyframe,
adaptive bitrate. Leading suspects to confirm: Android MediaCodec output buffering
(arrive→decode→present), USB transfer granularity / per-frame flush (send→arrive), capture
pacing (capture→encode).
