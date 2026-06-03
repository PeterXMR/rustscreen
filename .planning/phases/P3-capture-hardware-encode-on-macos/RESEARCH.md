# Phase P3: Capture + Hardware-Encode on macOS - Research

**Researched:** 2026-06-03
**Domain:** macOS screen capture (ScreenCaptureKit) + hardware H.264 encode (VideoToolbox), zero-copy IOSurface path
**Confidence:** MEDIUM-HIGH (crate APIs verified via docs.rs; the single biggest unknown — whether a `CGVirtualDisplay` virtual display is even visible to ScreenCaptureKit — is a confirmed *risk*, not a confirmed *capability*, and can only be settled on a hands-on Mac run)

## Summary

P3 captures the P2 virtual display (a `CGDirectDisplayID` from `cg-virtual-display`) and hardware-encodes it to H.264, producing a `.h264` file that plays in ffplay, with SPS/PPS and per-frame encode latency logged. The pure-Rust logic for criteria #2 (SPS/PPS extraction via `protocol::nal`) and #3 (latency stats + the `run_session` pipeline in `macos-host::encode`) is **already implemented and unit-tested (33 tests passing)**. What remains is two platform adapters behind the existing `Capturer` and `Encoder` traits, plus a spike binary that wires them to the P2 display and dumps `out.h264`.

The ecosystem has shifted favorably since the roadmap was written. The roadmap §0.1 assumed `screencapturekit-rs` was "a small Swift bridge" and listed pure `objc2-screen-capture-kit` as the P7 target. In fact the `screencapturekit` crate (renamed from `screencapturekit-rs`, same doom-fish repo) reached **v7.0.0 on 2026-06-02** and its companion **`videotoolbox` v0.18.0** exposes a builder-style H.264 encoder whose `encode()` accepts an `IOSurface` directly — giving a clean zero-copy SCK→VideoToolbox path from one maintainer. However, `videotoolbox` v0.18.0 has only ~633 lifetime downloads (brand new, unproven — matches the roadmap's "experimental ~69%" flag), so it must stay behind the `Encoder` trait, and the lower-level `objc2-video-toolbox` (madsmtm, well-established) is the swap target if it disappoints.

The dominant risk is **capture of a virtual display specifically**: confirmed reports show displays created by the private `CGVirtualDisplay` API are frequently **absent from `SCShareableContent.displays`** and that SCK/`CGDisplayStream` "confuse" virtual displays when more than one exists. This means the spike must treat the **`CGDisplayStream` fallback as a co-equal path, not an afterthought**, and the very first thing to verify on the Mac is whether the P2 display even appears in `SCShareableContent`.

**Primary recommendation:** Build two adapters behind the existing traits — a `ScreenCaptureKit` `Capturer` (crate `screencapturekit` 7.x) matching the P2 display by `SCDisplay.displayID == display_id`, and a `videotoolbox` `Encoder` feeding the captured `IOSurface` zero-copy into `CompressionSession::encode`. Gate the first Mac run on a "does the virtual display appear in `SCShareableContent`?" probe; if not, fall through to a `CGDisplayStream` (via `objc2-core-graphics`) capture adapter behind the same `Capturer` trait. Write Annex-B (matching existing `nal.rs`) to `out.h264`; verify with `ffplay out.h264`.

## User Constraints

> No CONTEXT.md exists for this phase (this is the first planning artifact). Constraints below are the locked decisions from the authoritative roadmap (`docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md` §1) and the user's standing preferences from project memory. The discuss-phase step may add a CONTEXT.md that supersedes these.

### Locked Decisions (roadmap §1)
- **D0 — Simplest-now, shrink-to-Rust-later (LOCKED):** every platform boundary sits behind a Rust trait/FFI seam; ship the simplest adapter that works for the MVP; swap to pure-Rust later **without touching core logic**. For P3 this means: the `Capturer`/`Encoder` traits in `macos-host` are the stable ports; the SCK + VideoToolbox crates are the swappable adapters behind them.
- **D2 — Codec = H.264 (LOCKED):** H.264 only for MVP (universal HW support on M1 + Pixel 6a). HEVC is a P7 toggle. Do not research/plan HEVC here.
- **D6 — Display geometry (default):** 2400×1080 @ 60 Hz (Pixel 6a native, landscape). The capture/encode dimensions should match the P2 display.

### Claude's Discretion (within the locked seams)
- Capture adapter crate choice (`screencapturekit` 7.x vs lower-level `objc2-screen-capture-kit`) — recommendation below.
- Encode adapter crate choice (`videotoolbox` high-level vs `objc2-video-toolbox` low-level) — recommendation below.
- Whether `CapturedFrame` gains an IOSurface-carrying field, or the zero-copy hand-off is kept entirely inside a fused capture+encode adapter (architectural decision discussed below — this is the one signature question the planner must resolve).
- Bitrate target, exact keyframe interval value, realtime/low-latency property combination.

### Deferred Ideas (OUT OF SCOPE for P3)
- HEVC (P7, D2).
- Pure-`objc2-screen-capture-kit` swap to drop the (now non-existent) Swift bridge (P7, PURITY-01) — though see note: the recommended `screencapturekit` 7.x is *already* objc2-based, so this swap may be a no-op.
- USB transport / framing of the encoded stream (P1/P5). P3 dumps to a file only.
- Live pipeline, latency-to-glass measurement (P5). P3 logs *encode* latency only.
- Touch back-channel (P6).

## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| ENC-01 | A `.h264` file captured from the virtual display (ScreenCaptureKit; CGDisplayStream fallback) and hardware-encoded via VideoToolbox (realtime/low-latency, no B-frames, zero-copy IOSurface) plays in ffplay/VLC; SPS/PPS extracted and logged; per-frame encode latency logged. | Capture adapter (Standard Stack §SCK), encode adapter (§VideoToolbox), zero-copy wiring (§Architecture Patterns Pattern 1–2), Annex-B output reconciliation (§Output / Pattern 3), fallback path (§Common Pitfalls Pitfall 1). SPS/PPS + latency logic already done in `protocol::nal` + `macos-host::encode`. |

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Enumerate displays, match P2 display by ID | macOS host adapter (SCK) | — | `SCShareableContent` is a macOS API; matching is `SCDisplay.displayID == display_id`. |
| Pull frames off the virtual display | macOS host adapter (SCK `Capturer`) | CGDisplayStream fallback adapter | Capture is platform-owned; behind the `Capturer` trait (D0 seam). |
| Hold/own the frame `IOSurface` zero-copy | macOS host adapter | — | The IOSurface is a CoreVideo object; must never cross into the platform-agnostic pipeline as raw pixels. |
| Hardware H.264 encode | macOS host adapter (VideoToolbox `Encoder`) | — | VideoToolbox is platform-owned; behind the `Encoder` trait (D0 seam). |
| Drive capture→encode→sink loop, count frames, stop at N | platform-agnostic pipeline (`encode::run_session`) | — | **Already implemented + tested.** Pure Rust, no platform deps. |
| Extract & log SPS/PPS | platform-agnostic (`protocol::nal::extract_codec_config`) | — | **Already implemented + tested.** Pure Rust. |
| Per-frame encode-latency stats | platform-agnostic (`encode::LatencyStats`) | — | **Already implemented + tested.** Adapter supplies `encode_micros`; pipeline aggregates. |
| Write Annex-B to `out.h264` | platform-agnostic (`std::io::Write` sink) | — | `run_session` already takes `&mut dyn Write`; a `File` is a valid sink today. |
| Visual playback verification | human + ffplay | — | Cannot be automated headlessly; hands-on-Mac step. |

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `screencapturekit` | 7.0.0 | SCK `Capturer` adapter — `SCStream` filtered to the P2 display, yields `CMSampleBuffer`→`IOSurface` | [VERIFIED: crates.io API] Renamed from `screencapturekit-rs`; same repo (doom-fish). 609k total / 254k recent downloads; "powering 50+ OSS projects incl. Cap, AFFiNE" [CITED: github.com/doom-fish/screencapturekit-rs]. objc2-based, **no Swift bridge in v7** (the roadmap's "Swift bridge" note is stale). |
| `videotoolbox` | 0.18.0 | VideoToolbox `Encoder` adapter — H.264 `CompressionSession`, accepts `IOSurface` directly | [VERIFIED: crates.io API] Same maintainer as `screencapturekit` (doom-fish), so the SCK→VT IOSurface hand-off is a designed pairing. **Caveat: only ~633 lifetime downloads, first published 2026-05-18 — unproven.** Keep strictly behind the `Encoder` trait (R3). |
| `protocol` (local) | 0.1.0 | `nal::extract_codec_config`, `is_keyframe`, Annex-B NAL iteration | Already in-tree, 33 tests passing. Encoder output must be Annex-B to feed it. |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| `objc2-core-video` | 0.3.2 | `CVPixelBuffer`/`CVImageBuffer` types, `io_surface()` accessor | If the SCK crate's frame accessors are insufficient and you need to reach the `IOSurface` directly. madsmtm canonical family. |
| `objc2-io-surface` | 0.3.2 | `IOSurface` type | Type plumbing for the zero-copy hand-off if adapters are split. 19M downloads (rock solid). |
| `objc2-core-media` | 0.3.2 | `CMSampleBuffer`, `CMTime` | If you parse the sample buffer manually rather than via the SCK crate. |
| `objc2-core-graphics` | 0.3.x | `CGDisplayStream`, `CGGetActiveDisplayList` | **The fallback capture adapter** (see Pitfall 1) and to re-verify the P2 display ID is enumerable. |
| `objc2-video-toolbox` | 0.3.2 | Low-level VT `VTCompressionSession` bindings | Swap target if `videotoolbox` 0.18 proves too immature. madsmtm family (12k downloads, stable API surface). |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| `screencapturekit` 7.x | `objc2-screen-capture-kit` 0.3.2 (madsmtm) | Lower-level, no builder ergonomics — you assemble `SCContentFilter`/`SCStream`/delegate by hand. This is the *original* roadmap P7 "pure-Rust" target, but `screencapturekit` 7.x is *already* objc2-based, so choosing it makes P7's purity swap a near no-op. Use madsmtm directly only if the high-level crate's churn (7 majors in weeks) bites. |
| `videotoolbox` 0.18 (high-level) | `objc2-video-toolbox` 0.3.2 (low-level VT) | High-level crate is far easier (builder, `encode(&surface, ...)`) but unproven (633 dl). Low-level is stable but you write the `VTCompressionSession` property dance and pixel-buffer plumbing yourself. Lead with high-level behind the trait; fall back to low-level if it churns/breaks (R3). |
| `CGDisplayStream` fallback | (none — it *is* the fallback) | Deprecated since macOS 14.0 but still functional; simpler API; historically also "confuses" virtual displays (same bug family). It is the fallback precisely because SCK may not see the virtual display at all. |

**Installation (add to `crates/macos-host/Cargo.toml`):**
```bash
# Lead adapters:
cargo add -p macos-host screencapturekit@7
cargo add -p macos-host videotoolbox@0.18
# Likely needed for IOSurface/CVPixelBuffer plumbing and the fallback:
cargo add -p macos-host objc2-core-video@0.3 objc2-core-media@0.3 objc2-io-surface@0.3 objc2-core-graphics@0.3
```
> All `cargo add` commands target the `macos-host` crate only — these are macOS-platform deps and must NOT leak into the shared `protocol` crate (which is pure, no_std-friendly, no platform deps per C-workspace-structure).

**Version verification (done this session):** all versions above confirmed live on crates.io via the registry API on 2026-06-03 with publish dates and download counts as listed. `videotoolbox` and `screencapturekit` are very fresh — re-run `cargo info <crate>` at plan time; the SCK crate had 7 major releases in ~2 weeks, so 7.x may not be 7.0 by then.

## Package Legitimacy Audit

> slopcheck could **not** be run this session — installing it was denied by the environment's supply-chain sandbox (it would execute an agent-chosen external package). Per the legitimacy protocol's graceful-degradation rule, every external package below is therefore tagged **`[ASSUMED]`** and the planner **must gate each install behind a `checkpoint:human-verify` task** before `cargo add`. The registry signals (age, downloads, source repo) below are from the crates.io API and stand in for the automated check.

| Package | Registry | Latest | Age | Downloads (total / recent) | Source Repo | slopcheck | Disposition |
|---------|----------|--------|-----|----------------------------|-------------|-----------|-------------|
| `screencapturekit` | crates.io | 7.0.0 (2026-06-02) | lineage 2+ yrs (37 releases); v7 brand new | 609,877 / 254,493 | github.com/doom-fish/screencapturekit-rs | not run | `[ASSUMED]` — strong signals (high downloads, named OSS users), but verify before install |
| `videotoolbox` | crates.io | 0.18.0 (2026-05-20) | ~2 weeks | **633 / 633** | github.com/doom-fish/videotoolbox-rs | not run | `[ASSUMED]` — **very low downloads, very new**; legit author but unproven. Human-verify + keep behind trait |
| `objc2-screen-capture-kit` | crates.io | 0.3.2 (2025-10-04) | ~2 yrs | 93,115 / 40,530 | github.com/madsmtm/objc2 | not run | `[ASSUMED]` — canonical madsmtm family; low risk |
| `objc2-video-toolbox` | crates.io | 0.3.2 (2025-10-04) | ~1.5 yrs | 12,670 / 6,092 | github.com/madsmtm/objc2 | not run | `[ASSUMED]` — madsmtm family; low risk |
| `objc2-core-video` | crates.io | 0.3.2 (2025-10-04) | ~1.5 yrs | 4,579,385 / 2,464,913 | github.com/madsmtm/objc2 | not run | `[ASSUMED]` — extremely high downloads; very low risk |
| `objc2-io-surface` | crates.io | 0.3.2 (2025-10-04) | ~1.5 yrs | 19,087,190 / 10,748,221 | github.com/madsmtm/objc2 | not run | `[ASSUMED]` — extremely high downloads; very low risk |
| `objc2-core-media` | crates.io | 0.3.2 (2025-10-04) | ~1.5 yrs | 447,411 / — | github.com/madsmtm/objc2 | not run | `[ASSUMED]` — madsmtm family; low risk |
| `objc2-core-graphics` | crates.io | 0.3.x | ~1.5 yrs | (madsmtm family) | github.com/madsmtm/objc2 | not run | `[ASSUMED]` — madsmtm family; low risk |

**Packages removed due to slopcheck [SLOP] verdict:** none (slopcheck unavailable).
**Packages flagged [SUS]:** `videotoolbox` 0.18.0 is flagged by *manual* judgment (633 downloads, 2 weeks old) even though the author is legitimate and also publishes `screencapturekit`. Planner: add a `checkpoint:human-verify` before installing it, and ensure the `Encoder` trait isolation is airtight so it can be swapped for `objc2-video-toolbox` without touching pipeline code.

## Architecture Patterns

### System Architecture Diagram

```
                 P2 output: CGDirectDisplayID (u32)
                              │
                              ▼
        ┌──────────────── probe: is this display in SCShareableContent.displays? ───────────────┐
        │ YES                                                          NO (known virtual-display │
        ▼                                                              bug — see Pitfall 1)      │
  ┌─────────────────────────┐                                          ▼                          │
  │ SCK Capturer adapter    │                              ┌────────────────────────────┐        │
  │ screencapturekit 7.x    │                              │ CGDisplayStream fallback   │        │
  │  SCShareableContent.get │                              │ Capturer adapter           │        │
  │  find SCDisplay where    │                              │ objc2-core-graphics        │        │
  │   displayID == id        │                              │ CGDisplayStreamCreate(id…) │        │
  │  SCContentFilter(display)│                              └─────────────┬──────────────┘        │
  │  SCStreamConfiguration   │                                            │                       │
  │  SCStream + output handler│                                           │                       │
  └──────────┬──────────────┘                                            │                       │
             │  per frame: CMSampleBuffer ──► image_buffer ──► IOSurface  │ per frame: IOSurface  │
             └───────────────────────────┬──────────────────────────────┘                       │
                                          ▼  (zero-copy — IOSurface stays GPU-side)               │
                              ┌───────────────────────────────┐                                  │
                              │ VideoToolbox Encoder adapter   │                                  │
                              │ videotoolbox 0.18              │                                  │
                              │  CompressionSession::builder    │                                  │
                              │   .with_real_time(true)         │                                  │
                              │   .with_max_keyframe_interval   │                                  │
                              │   .with_average_bit_rate        │                                  │
                              │  encode(&iosurface, (pts,scale))│                                  │
                              │  ► EncodedFrame { data, … }     │ measures encode_micros          │
                              └───────────────┬─────────────────┘                                 │
                                              ▼  Annex-B bytes + keyframe flag + encode_micros     │
        ════════════════════════ trait seam (Encoder) — below is ALREADY BUILT ════════════════════
                                              ▼
                              ┌───────────────────────────────┐
                              │ encode::run_session (pure Rust)│  ◄── drives Capturer + Encoder
                              │  ├ write Annex-B ► sink (File)  │       counts frames, stop at N
                              │  ├ LatencyStats.record(micros)  │
                              │  └ nal::extract_codec_config()  │  ► SessionSummary{codec_config, latency}
                              └───────────────┬─────────────────┘
                                              ▼
                              out.h264 ──►  ffplay out.h264   (human visual check)
                              SessionSummary ──► log SPS/PPS + latency min/mean/max
```

### Recommended Project Structure
```
crates/macos-host/src/
├── capture.rs        # EXISTS — Capturer trait + CapturedFrame (the SEAM; keep stable)
├── encode.rs         # EXISTS — Encoder trait, run_session, LatencyStats (the SEAM + pipeline)
├── capture_sck.rs    # NEW — ScreenCaptureKit adapter impl Capturer  [hands-on-Mac]
├── capture_cgds.rs   # NEW — CGDisplayStream fallback adapter impl Capturer  [hands-on-Mac]
├── encode_vt.rs      # NEW — VideoToolbox adapter impl Encoder  [hands-on-Mac]
├── main.rs           # EXTEND — spike subcommand: create P2 display → wire adapters → run_session → out.h264
└── lib.rs            # EXTEND — module decls
```
> Keeping `capture.rs`/`encode.rs` as trait-only (as they are now) and putting adapters in sibling `*_sck.rs`/`*_vt.rs` files preserves the D0 seam and keeps the platform code physically isolated from the tested pipeline.

### Pattern 1: Match the P2 virtual display by `displayID` (not by index)
**What:** Find the `SCDisplay` whose `displayID` equals the `CGDirectDisplayID` returned by `cg-virtual-display`. Never assume `displays()[0]`.
**When to use:** Always, in the SCK adapter constructor.
**Example:**
```rust
// Source: docs.rs/screencapturekit/7.0.0 + developer.apple.com/.../scdisplay/displayid
// [CITED] SCDisplay.displayID is the CGDirectDisplayID. [ASSUMED] exact Rust accessor names — confirm on Mac.
let content = SCShareableContent::get()?;                 // async/blocking per crate API
let target = content
    .displays()
    .into_iter()
    .find(|d| d.display_id() == p2_display_id)            // <-- the critical match
    .ok_or(CaptureError::VirtualDisplayNotShareable)?;    // <-- triggers CGDisplayStream fallback
let filter = SCContentFilter::new().with_display(&target).with_excluding_windows(&[]).build();
```

### Pattern 2: Zero-copy IOSurface hand-off SCK → VideoToolbox
**What:** Pull the `IOSurface` out of the captured `CMSampleBuffer` and feed it straight into the encoder without ever locking/copying pixels.
**When to use:** The encode hot path (criterion #3 requires "zero-copy IOSurface").
**Example:**
```rust
// Source: docs.rs/screencapturekit/7.0.0 (sample.image_buffer().io_surface())
//       + docs.rs/videotoolbox/0.18.0 (CompressionSession::encode accepts an IOSurface)
// In the SCStream output handler:
let image_buffer = sample.image_buffer().ok_or(...)?;     // CVPixelBuffer/CVImageBuffer
let surface = image_buffer.io_surface().ok_or(...)?;      // IOSurface — GPU-side, no copy
let t0 = Instant::now();
let encoded = compression_session.encode(&surface, (pts_us, 1_000_000))?; // (pts, timescale)
let encode_micros = t0.elapsed().as_micros() as u64;      // feeds LatencyStats
// encoded.data -> Annex-B (see Pattern 3); build EncodedFrame and hand to run_session
```
> **Do NOT** call `buffer.lock(READ_ONLY)` / `as_slice()` in the hot path — that is the CPU-copy path and violates criterion #3. The lock path is only acceptable in a debugging/diagnostic mode.

### Pattern 3: Ensure encoder output is Annex-B before it reaches the pipeline
**What:** VideoToolbox natively emits **AVCC/length-prefixed** NAL units with parameter sets carried *out-of-band* in the `CMFormatDescription`, not Annex-B. The existing `protocol::nal` assumes **Annex-B (start codes) with SPS/PPS in-band**. The adapter must reconcile this.
**When to use:** In the `encode_vt.rs` adapter, for every output frame.
**Two options (decide in plan):**
- **(a) Crate does it:** `videotoolbox`'s `EncodedFrame.data` *may* already be Annex-B — **verify on the Mac** by inspecting the first bytes (`00 00 00 01`?) and whether SPS/PPS appear in-band. If yes, just write `encoded.data`.
- **(b) Adapter converts:** if `EncodedFrame.data` is AVCC, convert each 4-byte length prefix to a `00 00 00 01` start code, and on keyframes prepend the SPS/PPS pulled from the format description (also start-code-prefixed). This is small, pure logic and **should be unit-tested in the cable-free tier** (feed a synthetic AVCC buffer, assert Annex-B output that `nal::extract_codec_config` can parse).
**Example (AVCC→Annex-B, the pure-testable part):**
```rust
// Convert AVCC (4-byte BE length prefixes) to Annex-B start codes. Pure — UNIT TEST THIS.
fn avcc_to_annex_b(avcc: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(avcc.len());
    let mut i = 0;
    while i + 4 <= avcc.len() {
        let len = u32::from_be_bytes([avcc[i], avcc[i+1], avcc[i+2], avcc[i+3]]) as usize;
        i += 4;
        if i + len > avcc.len() { break; }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&avcc[i..i + len]);
        i += len;
    }
    out
}
```

### Anti-Patterns to Avoid
- **Capturing by display index (`displays()[0]`):** the virtual display is rarely index 0; match by `displayID`.
- **Locking the pixel buffer in the hot path:** breaks the zero-copy requirement (criterion #3).
- **Letting VideoToolbox/SCK types leak across the `Capturer`/`Encoder` traits:** the whole point of the D0 seam is that `run_session` never sees an `IOSurface`. Keep platform types inside the adapter (see "signature question" in Open Questions).
- **Writing AVCC to `out.h264` and expecting ffplay to show the desktop:** raw `.h264` for ffplay must be Annex-B; AVCC needs an MP4 container or it won't play as `.h264`.
- **Putting any of these crates in the `protocol` crate:** they are macOS-only; `protocol` stays pure.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| H.264 hardware encoding | A VTCompressionSession property-key dance from raw FFI | `videotoolbox` 0.18 builder (behind `Encoder` trait), or `objc2-video-toolbox` if it churns | Dozens of `kVTCompressionPropertyKey_*` flags, callback lifetime management, format-description handling — all easy to get subtly wrong. |
| Screen capture stream | Raw `SCStream`/delegate via FFI | `screencapturekit` 7.x (behind `Capturer` trait) | Delegate/handler memory management + `SCStreamConfiguration` surface is large; the crate wraps it idiomatically. |
| Annex-B NAL parsing / SPS/PPS extraction | A new start-code scanner | `protocol::nal` (ALREADY BUILT, 18 tests) | Already handles 3/4-byte start codes, leading zeros, back-to-back codes, first-SPS/PPS selection. |
| Capture→encode loop, latency stats, codec-config capture | A new orchestration loop | `encode::run_session` + `LatencyStats` (ALREADY BUILT, tests) | Already drives the traits, counts frames, stops at N, records latency, extracts codec config. |
| Length-prefixed framing (later) | — | `protocol::framing` (ALREADY BUILT) | Not needed in P3 (file dump only), but exists for P5. |

**Key insight:** ~70% of this phase's *logic* is already written and tested. P3 is almost entirely two thin adapters + the AVCC→Annex-B reconciliation + a spike `main`. Resist re-architecting the seam.

## Runtime State Inventory

> P3 is greenfield adapter code, not a rename/refactor. This section is included only to record one near-adjacent state concern: the **P2 virtual display lifecycle**.

| Category | Items Found | Action Required |
|----------|-------------|------------------|
| Stored data | None — P3 writes only `out.h264` (a build artifact, gitignore it). | none |
| Live service config | The P2 `VirtualDisplay` must be **alive for the whole capture** — it is torn down on `Drop` (see `cg-virtual-display::VirtualDisplay::drop`). The spike `main` must hold the `VirtualDisplay` value in scope until after `run_session` returns. | spike main: keep `VirtualDisplay` binding alive |
| OS-registered state | The virtual display registers with the window server (verifiable via `active_display_count()` / `CGGetActiveDisplayList`). Confirm it is present *before* starting capture. | probe at startup |
| Secrets/env vars | None. | none |
| Build artifacts | `out.h264` (spike output). Add to `.gitignore` if not already covered. | gitignore |

## Common Pitfalls

### Pitfall 1: The virtual display is invisible to ScreenCaptureKit (THE headline risk)
**What goes wrong:** `SCShareableContent.displays` may **not contain** the `CGVirtualDisplay`-created display at all, or SCK streams the *wrong* display when more than one virtual display exists — even with a correctly-configured `SCContentFilter` (correct `displayID`, frame, `contentRect`, `pointPixelScale`).
**Why it happens:** A longstanding, confirmed SCK/CGDisplayStream limitation with virtual displays created via the undocumented `CGVirtualDisplay` API (Apple bug FB17797423). It predates SCK (affected `CGDisplayStream` too).
**How to avoid:**
1. **Probe first:** before building the stream, log every `SCDisplay.displayID` and assert the P2 id is present. If absent, do not proceed with SCK.
2. **First-class fallback:** implement the `CGDisplayStream` adapter (`objc2-core-graphics`, `CGDisplayStreamCreate(displayID, …)`) behind the same `Capturer` trait. Selecting capture-by-ID directly may behave differently than SCK's content-filter path.
3. **Single virtual display during the spike:** the multi-virtual-display confusion bug is avoided by ensuring exactly one virtual display exists during P3.
4. If neither path sees the display, **surface to the user immediately** — like P2's R1, this is a keystone capability; the architecture (capture path) changes if a `CGVirtualDisplay` display is fundamentally uncapturable on macOS 26.x.
**Warning signs:** `displays()` does not contain the id; black/empty frames; frames showing the *wrong* (physical/primary) display's content.
**Confidence:** HIGH that the risk is real (confirmed forum + bug report); UNKNOWN whether it bites on this exact macOS 26.4 + single-display setup — only the Mac run settles it.

### Pitfall 2: VideoToolbox emits AVCC, not Annex-B
**What goes wrong:** Output won't parse with `protocol::nal` (no start codes) and won't play as a raw `.h264` in ffplay; SPS/PPS are out-of-band in the format description, so `extract_codec_config` returns `None`.
**Why it happens:** VideoToolbox's native NAL format is length-prefixed (AVCC) with parameter sets in the `CMFormatDescription`.
**How to avoid:** See Pattern 3 — verify the crate's output format on the Mac; if AVCC, convert to Annex-B and inject SPS/PPS in-band on keyframes. Unit-test the converter (cable-free).
**Warning signs:** `out.h264` first bytes are not `00 00 00 01`; ffplay errors / no video; `SessionSummary.codec_config` is `None`.

### Pitfall 3: `screencapturekit` 7.x version churn / API drift
**What goes wrong:** 7 major releases in ~2 weeks; method names in this research (`with_display`, `image_buffer`, `io_surface`) may shift.
**Why it happens:** Very actively developed, pre-stabilization crate.
**How to avoid:** Pin the exact version in `Cargo.toml`; confirm method names against `cargo doc --open` at plan time; the `Capturer` trait quarantines any drift to `capture_sck.rs`.
**Warning signs:** Compile errors on the documented method names.

### Pitfall 4: Screen Recording TCC permission
**What goes wrong:** ScreenCaptureKit (and CGDisplayStream) require the user to grant **Screen Recording** permission (System Settings ▸ Privacy & Security ▸ Screen Recording). Without it, capture yields black frames or fails silently.
**Why it happens:** macOS TCC gate on all screen-capture APIs.
**How to avoid:** On first run, the system prompts; the spike must be re-run after granting (the host process must be re-launched on some macOS versions). Document this in the hands-on-Mac run steps.
**Warning signs:** All-black frames; `SCShareableContent.get()` returns an empty/short display list.

### Pitfall 5: Dropping the P2 `VirtualDisplay` mid-capture
**What goes wrong:** Capture stops / display vanishes if the `VirtualDisplay` value is dropped (its `Drop` tears down the display).
**How to avoid:** Bind it in the spike `main` and keep it alive across the entire `run_session` call. (See Runtime State Inventory.)

## Code Examples

### Wiring the spike (cable-free skeleton + hands-on-Mac adapters)
```rust
// crates/macos-host/src/main.rs (spike subcommand) — wiring is mostly cable-free;
// only the two adapter constructors touch the GPU/display.
// Source: composed from cg-virtual-display::VirtualDisplay + existing encode::run_session.
let vdisp = cg_virtual_display::VirtualDisplay::new(2400, 1080, 60.0)?; // D6 geometry
let id = vdisp.display_id();
log::info!("virtual display id={id}, active_displays={}", cg_virtual_display::active_display_count());

// Adapter selection (hands-on-Mac): SCK if the display is shareable, else CGDisplayStream.
let mut capturer: Box<dyn Capturer> = match SckCapturer::for_display(id) {
    Ok(c) => Box::new(c),
    Err(CaptureError::VirtualDisplayNotShareable) => {
        log::warn!("virtual display not in SCShareableContent — falling back to CGDisplayStream");
        Box::new(CgDisplayStreamCapturer::for_display(id)?)
    }
    Err(e) => return Err(e.into()),
};
let mut encoder = VtEncoder::h264_realtime(2400, 1080, /*bitrate*/ 12_000_000, /*kf_interval*/ 120)?;

let mut file = std::fs::File::create("out.h264")?;        // run_session takes &mut dyn Write
let summary = encode::run_session(&mut *capturer, &mut encoder, &mut file, /*max_frames*/ 600)?; // ~10s @60

// Criterion #2 + #3 logging (uses ALREADY-BUILT types):
match summary.codec_config {
    Some(cfg) => log::info!("SPS={:02x?} PPS={:02x?}", cfg.sps, cfg.pps),
    None => log::error!("no SPS/PPS captured — output likely AVCC not Annex-B (see Pitfall 2)"),
}
log::info!("encode latency: min={:?} mean={:?} max={:?} frames={}",
    summary.latency.min(), summary.latency.mean(), summary.latency.max(), summary.frames);
drop(vdisp); // explicit: keep alive until here (Pitfall 5)
```

### VideoToolbox encoder config (hands-on-Mac)
```rust
// Source: docs.rs/videotoolbox/0.18.0 CompressionSession builder.
// realtime + low-latency + no B-frames + ~2s keyframe interval (120 @ 60fps).
let session = CompressionSession::builder(width, height, Codec::H264)
    .with_real_time(true)               // realtime
    .with_average_bit_rate(bitrate)     // e.g. 12 Mbit/s for 2400x1080
    .with_expected_frame_rate(60.0)
    .with_max_keyframe_interval(120)    // ~2s @ 60fps
    // [ASSUMED] low-latency / no-B-frames: docs mention support but exact builder method
    // names not confirmed. On Mac, look for .with_allow_frame_reordering(false) /
    // a low-latency/prioritize-speed property. Verify against `cargo doc`.
    .build()?;
```

## State of the Art

| Old Approach (roadmap assumption) | Current Approach (verified 2026-06) | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `screencapturekit-rs` is a "small Swift bridge" (roadmap §0.1) | `screencapturekit` 7.x is **objc2-based, no Swift bridge** | by v7.0.0 (2026-06-02) | The P7 "swap Swift bridge → objc2-screen-capture-kit" purity task is largely **already satisfied** by choosing this crate. Update the purity ledger. |
| `objc2-screen-capture-kit` listed as the *future* pure-Rust target | It exists (0.3.2) but the high-level `screencapturekit` 7.x is also objc2-based and far more ergonomic | — | Lead with the high-level crate; the low-level madsmtm crate is the deeper fallback, not a required P7 migration. |
| `CGDisplayStream` "deprecated but simple" fallback | Still works on macOS 26.4 but deprecated since macOS 14; **same virtual-display confusion bug family** as SCK | macOS 14.0 | Fallback is real but not a magic bullet — it may share the virtual-display visibility problem. |

**Deprecated/outdated:**
- `CGDisplayStream` — deprecated since macOS 14.0 (Sonoma). Acceptable as a spike fallback; not for the shipped app.
- The roadmap's "Swift bridge" characterization of the capture crate — stale; the recommended crate is pure objc2.

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `videotoolbox` 0.18's `EncodedFrame.data` format (Annex-B vs AVCC) | Pattern 3 / Pitfall 2 | If AVCC and we assume Annex-B, ffplay shows nothing and SPS/PPS extraction returns `None`. Mitigated by Pattern 3 converter + Mac verification. |
| A2 | Exact builder methods for low-latency / disable-B-frames in `videotoolbox` 0.18 | Code Examples (VT config) | If the methods differ, B-frames stay on → reordering latency. Verify via `cargo doc`; criterion #3 requires no B-frames. |
| A3 | Exact accessor names `display_id()`, `image_buffer()`, `io_surface()`, `with_display()` in `screencapturekit` 7.x | Pattern 1 & 2 | Compile errors only; quarantined to the adapter. Confirm via `cargo doc` at plan time. |
| A4 | A `CGVirtualDisplay` display *can* be captured at all on macOS 26.4 (single-display case) | Pitfall 1 | If fundamentally uncapturable, P3's capture path changes and must be escalated to the user (keystone-level). Only the Mac run resolves this. |
| A5 | The SCK→VideoToolbox IOSurface hand-off is truly zero-copy through these crates | Pattern 2 | If a hidden copy exists, encode latency rises but criteria still pass at file level; revisit in P5 latency work. |
| A6 | All packages are legitimate (slopcheck could not run) | Package Legitimacy Audit | Supply-chain risk. Mitigated: planner gates each install behind `checkpoint:human-verify`; registry signals are strong for all except `videotoolbox` (633 dl). |
| A7 | `with_average_bit_rate(12_000_000)` (~12 Mbit/s) is a sane target for 2400×1080@60 | Code Examples | Wrong bitrate only affects quality/size, not pass/fail; tune during the Mac run. |

## Open Questions

1. **Does `CapturedFrame` need an IOSurface-carrying field, or should capture+encode be fused?** (The one signature question the planner must resolve.)
   - What we know: `CapturedFrame` currently carries only `pts_us/width/height` and is `Clone + PartialEq + Eq` (deliberately platform-free so the pipeline stays testable). The encoder needs the actual `IOSurface`, which cannot be `Eq`/`Clone`-cheap and must not leak into the pipeline.
   - What's unclear: whether to (a) add a non-pipeline-visible IOSurface handle to the adapter's own frame type and keep `CapturedFrame` as pure metadata, with the SCK adapter and VT adapter sharing a private channel; or (b) **fuse** capture+encode into one adapter that implements both traits and passes the IOSurface internally, with `run_session` still orchestrating via the trait-level metadata.
   - Recommendation: **Option (b) — a fused `SckVtSession` for the spike** is simplest-now and avoids changing the tested `CapturedFrame` type or the `Capturer`/`Encoder` signatures at all. The IOSurface never crosses a trait boundary; `next_frame()` returns metadata, `encode(&CapturedFrame)` looks up the matching internal surface by pts. If a cleaner split is wanted later, the traits already allow it. **Do not** add a raw-pointer IOSurface field to the public `CapturedFrame` (breaks its derives and the seam's purity). Flag for discuss-phase.

2. **Annex-B vs AVCC at the crate level** — resolve by inspecting real output on the Mac (Pattern 3). If the crate offers a format toggle, prefer Annex-B; else use the converter.

3. **Which capture path wins for a virtual display** — SCK vs CGDisplayStream — only the Mac probe answers this. Build both; let the runtime probe choose (Pitfall 1).

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| macOS | whole phase | ✓ | 26.4 (build 25E246) | — |
| Rust toolchain | build | ✓ | 1.95.0 | — |
| `ffplay` | criterion #1 visual check | ✓ | 8.1.1 (homebrew) | VLC (not installed) / `ffmpeg` (present) |
| `ffmpeg` | optional stream inspection | ✓ | present (~/.darkbloom/bin) | — |
| P2 `cg-virtual-display` crate | provides `CGDirectDisplayID` | ✓ | in-tree (P2 complete) | — |
| Screen Recording TCC grant | SCK/CGDisplayStream capture | ✗ (must be granted on first run) | — | **No fallback — blocking for the Mac run.** User must grant in System Settings and re-launch. |
| Physical Pixel 6a | NOT needed for P3 | n/a | — | P3 is Mac-only |

**Missing dependencies with no fallback:**
- Screen Recording permission must be granted interactively on the hands-on-Mac run (one-time, blocking until granted). Document as the first step of the Mac run.

**Missing dependencies with fallback:**
- VLC absent → use ffplay (present) for criterion #1.

## Cable-Free vs Hands-on-Mac Split (the key planning axis)

> Per the phase brief: P3 is Mac-only (not cable-blocked) but the live capture+encode path cannot be unit-tested headlessly. This is the planner's primary work-partitioning guide.

### (a) Cable-free / headless-testable NOW (pure Rust, CI-green, no GPU/display/phone)
- **Already done (33 tests):** `protocol::nal` (SPS/PPS extraction, NAL split, keyframe detection); `encode::run_session`, `LatencyStats`, `SessionSummary`; `protocol::framing`.
- **New cable-free units to add (TDD):**
  - `avcc_to_annex_b()` converter (Pattern 3) — feed synthetic AVCC, assert Annex-B that `nal::extract_codec_config` parses. **Pure, fully testable.**
  - SPS/PPS-injection-on-keyframe logic (if option (b) of Pattern 3 is needed) — pure, testable with synthetic format-description bytes.
  - Adapter-selection / display-id matching *logic* — testable by abstracting "list of (displayID)" behind a tiny trait and asserting the right id is chosen / fallback triggers when absent.
  - The spike `main` wiring can be structured so everything except the two adapter constructors is exercised by a fake capturer/encoder (the existing test pattern in `encode.rs`).
- **CI stays green** with zero hardware: all of the above run on Linux/macOS CI without a display.

### (b) Hands-on-Mac (requires a live Mac with a display; cannot be CI-automated)
- Run the spike binary, **grant Screen Recording permission**, re-launch.
- **Probe:** confirm the P2 virtual display appears in `SCShareableContent.displays` (Pitfall 1 — the make-or-break check).
- Real `SckCapturer` against the live virtual display; if invisible, real `CgDisplayStreamCapturer`.
- Real `VtEncoder` H.264 encode of live `IOSurface`s; **verify output is Annex-B** (Pattern 3 / A1).
- Produce `out.h264`; **`ffplay out.h264`** → confirm the virtual desktop is recognizable (criterion #1 — inherently a human visual check).
- Confirm logged SPS/PPS are non-empty (criterion #2) and per-frame latency min/mean/max are logged (criterion #3).
- Record real encode-latency numbers (feeds the P5 §2 latency budget).

**Planner guidance:** Structure P3 so all (a) work lands first as TDD tasks (CI-verifiable), then a single clearly-marked `[hands-on-Mac]` task block performs (b). The (b) block is one human session; gate it with a checklist (permission, probe, ffplay) rather than automated assertions. If the Pitfall-1 probe fails, escalate to the user before writing more adapter code.

## Validation Architecture

> `nyquist_validation` is not configured (no `.planning/config.json`) → treated as enabled.

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[cfg(test)]` / `cargo test` (no external test crate in use) |
| Config file | none — inline test modules (as in `nal.rs`, `encode.rs`, `framing.rs`) |
| Quick run command | `cargo test -p macos-host -p protocol` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements → Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| ENC-01 | SPS/PPS extracted from Annex-B | unit | `cargo test -p protocol nal` | ✅ (`protocol/src/nal.rs`) |
| ENC-01 | Capture→encode→sink loop, latency, codec-config capture | unit | `cargo test -p macos-host encode` | ✅ (`macos-host/src/encode.rs`) |
| ENC-01 | AVCC→Annex-B conversion produces parseable Annex-B | unit | `cargo test -p macos-host avcc` | ❌ Wave 0 (`encode_vt.rs` or `nal` helper) |
| ENC-01 | Adapter selection falls back when display id absent | unit | `cargo test -p macos-host capture_select` | ❌ Wave 0 |
| ENC-01 | `out.h264` plays in ffplay showing the virtual desktop | manual (hands-on-Mac) | n/a — `ffplay out.h264` + human | n/a |
| ENC-01 | SPS/PPS + per-frame latency logged from a *live* encode | manual (hands-on-Mac) | n/a — inspect spike logs | n/a |

### Sampling Rate
- **Per task commit:** `cargo test -p macos-host -p protocol`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** full suite green AND the hands-on-Mac checklist (permission → probe → ffplay → logs) complete before `/gsd:verify-work`.

### Wave 0 Gaps
- [ ] `avcc_to_annex_b` + its tests (in `macos-host/src/encode_vt.rs`, or a pure helper in `protocol::nal` if it should be shared with P4) — covers ENC-01 output reconciliation.
- [ ] Display-id-matching / fallback-selection logic behind a tiny testable trait — covers ENC-01 capture robustness.
- [ ] (No framework install needed — built-in `cargo test`.)

## Security Domain

> `security_enforcement` is not configured (no `.planning/config.json`) → treated as enabled. P3 is a local capture/encode spike with no network, no auth, no user-supplied input, no persistence beyond a local file — most ASVS categories are N/A.

### Applicable ASVS Categories
| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | no | No auth surface in P3 |
| V3 Session Management | no | No sessions |
| V4 Access Control | yes (OS-level) | macOS **Screen Recording TCC permission** is the access gate; do not attempt to bypass it. P7 handles signing/notarization. |
| V5 Input Validation | partial | The only "input" is the encoder's byte stream into `protocol::nal`/the AVCC converter — already bounds-checked (`framing` enforces `MAX_FRAME_LEN`; the AVCC converter must guard `len` against buffer overrun, see Pattern 3 `if i + len > avcc.len() break`). |
| V6 Cryptography | no | No crypto in P3 |

### Known Threat Patterns for this stack
| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Malformed encoder output overruns the AVCC parser | Tampering / DoS | Bounds-check length prefixes (Pattern 3); the existing `nal` iterator is already slice-safe. |
| Unverified third-party crate executes build/proc-macro code (`videotoolbox` is very new) | Supply chain (Tampering) | Pin versions; `checkpoint:human-verify` before each install (slopcheck unavailable); prefer madsmtm `objc2-*` family where possible. |
| Capturing more than the intended display (privacy) | Information disclosure | Match strictly by `displayID`; the virtual-display-confusion bug (Pitfall 1) could cause capturing the *wrong* display — verify on-screen content during the Mac run. |

## Sources

### Primary (HIGH confidence)
- crates.io registry API — `screencapturekit`, `videotoolbox`, `objc2-screen-capture-kit`, `objc2-video-toolbox`, `objc2-core-video`, `objc2-io-surface`, `objc2-core-media`, `objc2-core-graphics`: versions, publish dates, download counts (queried 2026-06-03).
- docs.rs/videotoolbox/0.18.0 — `CompressionSession` builder API, `encode(&IOSurface, …)`, `EncodedFrame`.
- docs.rs/screencapturekit/7.0.0 — `SCShareableContent`, `SCDisplay`, `SCContentFilter`, `SCStream`, `image_buffer()`/`io_surface()`.
- github.com/doom-fish/screencapturekit-rs — objc2-based (no Swift bridge), maturity/users (Cap, AFFiNE).
- In-tree source: `crates/protocol/src/nal.rs`, `crates/macos-host/src/{capture,encode}.rs`, `crates/protocol/src/framing.rs`, `crates/cg-virtual-display/src/lib.rs` (read directly).
- Local environment probes: `sw_vers` (macOS 26.4), `rustc`/`cargo` 1.95.0, `ffplay` 8.1.1.

### Secondary (MEDIUM confidence)
- developer.apple.com/documentation/screencapturekit/scdisplay/displayid — `displayID` is the `CGDirectDisplayID` (the matching key).
- developer.apple.com/documentation/screencapturekit/sccontentfilter — `init(display:…)` filter construction.

### Tertiary (LOW confidence — flagged for the Mac run)
- developer.apple.com/forums/thread/786829 — ScreenCaptureKit confuses virtual displays (bug FB17797423); virtual displays may be absent from `SCShareableContent`. (Confirms the risk; exact behavior on macOS 26.4 single-display is unverified.)
- WebSearch results on SCK + `CGVirtualDisplay` not listed in `SCShareableContent`.

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — versions/APIs verified on crates.io + docs.rs this session; the SCK↔VT IOSurface pairing is from one maintainer.
- Architecture / wiring: HIGH for the cable-free seam (it's already built + tested); MEDIUM for the adapter internals (exact method names assumed pending `cargo doc`).
- Pitfalls: HIGH that the virtual-display capture risk is real (confirmed bug report); the per-machine outcome is the genuine unknown that only the Mac run settles.
- `videotoolbox` crate maturity: LOW (633 downloads, 2 weeks old) — hence strict trait isolation + human-verify gate.

**Research date:** 2026-06-03
**Valid until:** 2026-06-17 (~14 days — `screencapturekit`/`videotoolbox` are fast-moving; re-check versions at plan time).
