---
phase: P3-capture-hardware-encode-on-macos
plan: 01
type: tdd
wave: 1
depends_on: []
files_modified:
  - crates/macos-host/src/encode_vt.rs
  - crates/macos-host/src/capture_select.rs
  - crates/macos-host/src/capture_sck.rs
  - crates/macos-host/src/capture_cgds.rs
  - crates/macos-host/src/lib.rs
  - crates/macos-host/src/main.rs
  - crates/macos-host/Cargo.toml
autonomous: false
requirements: [ENC-01]

user_setup:
  - service: macos-screen-recording
    why: "ScreenCaptureKit and CGDisplayStream both require the Screen Recording TCC grant; without it capture yields black frames or an empty display list (RESEARCH Pitfall 4). Hands-on-Mac only — Wave B."
    dashboard_config:
      - task: "Grant Screen Recording to the spike binary (or terminal), then re-launch it"
        location: "System Settings ▸ Privacy & Security ▸ Screen Recording"

must_haves:
  truths:
    - "AVCC length-prefixed encoder output is converted to Annex-B start-code framing that protocol::nal can parse"
    - "SPS/PPS are injected in-band on keyframes so extract_codec_config returns Some(..) from the on-disk stream"
    - "Capture-adapter selection picks SCK when the P2 display id is shareable, and falls back to CGDisplayStream when it is absent"
    - "out.h264 plays in ffplay showing the virtual desktop (hands-on-Mac)"
    - "SPS/PPS and per-frame encode latency min/mean/max are printed to stdout/stderr from a live encode (hands-on-Mac)"
  artifacts:
    - path: "crates/macos-host/src/encode_vt.rs"
      provides: "avcc_to_annex_b converter + SPS/PPS keyframe in-band injection (pure, tested) and the VideoToolbox Encoder adapter (hands-on-Mac, cfg-gated)"
      contains: "fn avcc_to_annex_b"
    - path: "crates/macos-host/src/capture_select.rs"
      provides: "DisplaySource trait + select_backend fallback logic (pure, tested)"
      contains: "fn select_backend"
    - path: "crates/macos-host/src/capture_sck.rs"
      provides: "ScreenCaptureKit Capturer adapter (hands-on-Mac, cfg-gated)"
    - path: "crates/macos-host/src/capture_cgds.rs"
      provides: "CGDisplayStream fallback Capturer adapter (hands-on-Mac, cfg-gated)"
  key_links:
    - from: "crates/macos-host/src/encode_vt.rs"
      to: "protocol::nal::extract_codec_config"
      via: "avcc_to_annex_b output parsed back by extract_codec_config in the converter tests"
      pattern: "extract_codec_config"
    - from: "crates/macos-host/src/main.rs"
      to: "macos_host::encode::run_session"
      via: "spike (binary crate) wires the selected Capturer + macos_host::encode_vt::VtEncoder into the lib pipeline, writes out.h264"
      pattern: "macos_host::encode::run_session"
    - from: "crates/macos-host/src/main.rs"
      to: "cg_virtual_display::VirtualDisplay"
      via: "spike creates the P2 display and holds it alive across run_session (Pitfall 5); cg-virtual-display is an optional dep enabled by the live-capture feature"
      pattern: "VirtualDisplay::new"
---

<objective>
Close the remaining gap for ENC-01: turn the already-tested pure-Rust pipeline (`protocol::nal`, `encode::run_session`, `LatencyStats`) into a working capture→hardware-encode→playable-file spike for the P2 virtual display.

Purpose: Success criteria #2 (SPS/PPS extraction) and #3 (latency/pipeline) are already met at the logic level by prior cable-free work. This plan adds the two missing pure-logic units (AVCC→Annex-B reconciliation, capture-adapter selection) as TDD, then the macOS adapters (SCK capture, CGDisplayStream fallback, VideoToolbox encode) and the spike `main`, finishing criterion #1 (ffplay plays the virtual desktop).

Output:
- Wave A (cable-free, CI-green now): `avcc_to_annex_b()` + keyframe SPS/PPS in-band injection in `encode_vt.rs`; `DisplaySource`/`select_backend` in `capture_select.rs`. All TDD, `cargo test --workspace` green, no hardware.
- Wave B (hands-on-Mac, ONE gated human session): gated dependency installs (incl. the in-tree `cg-virtual-display` P2 crate), the SCK + VideoToolbox + CGDisplayStream adapters (cfg-gated so Wave A CI stays clean), the spike `main`, and the `ffplay out.h264` visual verification with SPS/PPS + per-frame latency printed to stdout/stderr.

Locked-port contract: `Capturer`/`CapturedFrame` (capture.rs) and `Encoder`/`EncodedFrame`/`LatencyStats`/`run_session` (encode.rs) signatures are NOT changed. `CapturedFrame` stays pure metadata (`Clone + Eq`); no `IOSurface` field is added (per CONTEXT D1). Capture+encode are fused inside the adapter so the `IOSurface` never crosses a trait boundary (CONTEXT D5; RESEARCH Open Question 1 recommendation (b)).

Crate-path note: `macos-host` ships BOTH a library crate (`macos_host`, the tested pipeline + adapter modules) and a binary crate (`main.rs`, the spike). From the binary, lib items are reached as `macos_host::encode::run_session`, `macos_host::encode_vt::VtEncoder`, `macos_host::capture_select::select_backend`, `macos_host::capture_sck::SckCapturer`, `macos_host::capture_cgds::CgDisplayStreamCapturer` — NOT `crate::...`.

Logging: this spike uses `println!`/`eprintln!` (no `log` crate / backend is declared, so `log::*` macros would emit nothing). Criteria #2 (SPS/PPS) and #3 (latency) are verified by reading stdout/stderr.

No git commits this session: all artifacts and code stay as uncommitted working-tree diff for user review (CONTEXT D8). Do NOT run `git commit`/`git push` for this phase.
</objective>

<execution_context>
@$HOME/.claude/get-shit-done/workflows/execute-plan.md
@$HOME/.claude/get-shit-done/templates/summary.md
</execution_context>

<context>
@.planning/phases/P3-capture-hardware-encode-on-macos/CONTEXT.md
@.planning/phases/P3-capture-hardware-encode-on-macos/RESEARCH.md
@.planning/ROADMAP.md
@.planning/REQUIREMENTS.md

# Existing, already-passing modules — BUILD ON these, do not re-plan them:
@crates/macos-host/src/capture.rs
@crates/macos-host/src/encode.rs
@crates/macos-host/src/lib.rs
@crates/macos-host/src/main.rs
@crates/protocol/src/nal.rs

<interfaces>
<!-- Locked ports the executor must consume without modifying. Extracted from the codebase. -->

From crates/macos-host/src/capture.rs (DO NOT change these signatures):
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFrame { pub pts_us: u64, pub width: u32, pub height: u32 }

pub trait Capturer {
    fn next_frame(&mut self) -> Option<CapturedFrame>;
}
```

From crates/macos-host/src/encode.rs (DO NOT change these signatures):
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame { pub pts_us: u64, pub keyframe: bool, pub encode_micros: u64, pub annex_b: Vec<u8> }

pub trait Encoder {
    fn encode(&mut self, frame: &CapturedFrame) -> EncodedFrame;
}

pub fn run_session(
    capturer: &mut dyn Capturer,
    encoder: &mut dyn Encoder,
    sink: &mut dyn std::io::Write,
    max_frames: u64,
) -> std::io::Result<SessionSummary>;

pub struct SessionSummary {
    pub frames: u64,
    pub bytes_written: usize,
    pub codec_config: Option<protocol::nal::CodecConfig>,
    pub latency: LatencyStats,
}
```
<!-- From the BINARY crate (main.rs) these are reached as macos_host::encode::{run_session, SessionSummary}, NOT crate::encode::* -->

From crates/protocol/src/nal.rs (consume; already tested):
```rust
pub fn extract_codec_config(stream: &[u8]) -> Option<CodecConfig>; // Annex-B in, SPS+PPS out
pub fn is_keyframe(stream: &[u8]) -> bool;
pub fn nal_unit_type(nal: &[u8]) -> Option<u8>;
pub mod nal_type { pub const SPS: u8 = 7; pub const PPS: u8 = 8; pub const IDR_SLICE: u8 = 5; }
pub struct CodecConfig { pub sps: Vec<u8>, pub pps: Vec<u8> }
```

From crates/cg-virtual-display/src/lib.rs (P2, complete — add as an OPTIONAL dep of macos-host, see Task B0):
```rust
impl VirtualDisplay {
    pub fn new(width: u32, height: u32, refresh: f64) -> Result<Self, VirtualDisplayError>;
    pub fn display_id(&self) -> u32; // the CGDirectDisplayID to match in capture
}
pub fn active_display_count() -> u32;
// Dropping VirtualDisplay tears down the display — keep it alive across run_session (Pitfall 5).
```
</interfaces>
</context>

<tasks>

<!-- ============================ WAVE A — CABLE-FREE / CI-GREEN NOW ============================ -->
<!-- Pure Rust, TDD (red→green→refactor), fake-driven like the existing encode.rs tests.
     Each task ends with `cargo test --workspace` green and zero new hardware deps. -->

<task type="tdd" tdd="true">
  <name>Task A1: AVCC→Annex-B converter + keyframe SPS/PPS in-band injection (cable-free, TDD)</name>
  <files>crates/macos-host/src/encode_vt.rs, crates/macos-host/src/lib.rs</files>
  <behavior>
    Implements RESEARCH Pattern 3 / Pitfall 2 as pure, slice-safe logic. Write the failing tests FIRST and watch each fail before implementing.

    `avcc_to_annex_b(avcc: &[u8]) -> Vec<u8>`:
    - Test (empty): empty input → empty output.
    - Test (single NAL): `[00 00 00 05][67 42 1F 00 01]` (4-byte BE length prefix = 5) → `[00 00 00 01][67 42 1F 00 01]`.
    - Test (multi NAL): two length-prefixed NALs → two start-code-prefixed NALs concatenated, in order.
    - Test (bounds guard, V5): a length prefix claiming more bytes than remain (`i + len > avcc.len()`) → stop without panic/overrun (RESEARCH Pattern 3 guard, ASVS V5).
    - Test (round-trip): feed the converter an AVCC buffer containing SPS+PPS+IDR, then assert `protocol::nal::extract_codec_config(&out)` returns the expected SPS and PPS bytes — proves the output is parseable Annex-B (key_link to extract_codec_config).

    `to_annex_b_frame(avcc: &[u8], is_keyframe: bool, sps: &[u8], pps: &[u8]) -> Vec<u8>` (the in-band injection):
    - Test (keyframe injects params): with `is_keyframe=true`, output begins with start-code-prefixed SPS then PPS then the converted picture NAL(s); `extract_codec_config` on the output returns those SPS/PPS even though the AVCC picture payload carried none in-band.
    - Test (non-keyframe no inject): with `is_keyframe=false`, output is just the converted picture NAL(s), no SPS/PPS prepended.
    - Test (idempotent params): SPS/PPS are NOT double-prepended if already present (decide simplest: caller passes raw picture AVCC + out-of-band SPS/PPS, so injection always prepends on keyframes — document this contract in the fn doc).
  </behavior>
  <action>
    Create `crates/macos-host/src/encode_vt.rs` with the two PURE functions above and their `#[cfg(test)]` module (mirror the synthetic-buffer test style of `nal.rs`/`encode.rs`). These functions are platform-free and MUST compile and test on any host — do NOT import any macOS/objc2/videotoolbox crate in the non-`cfg`-gated part of this file. Implements RESEARCH Pattern 3; bounds-check every length prefix (`if i + len > avcc.len() { break; }`) per ASVS V5 / threat T-P3-01. Use `protocol::nal::nal_type::{SPS,PPS}` only for test assertions; the converter itself is byte-level. Add `pub mod encode_vt;` to `lib.rs`. Do NOT touch `encode.rs` or `capture.rs` signatures. The VideoToolbox `Encoder` adapter struct comes later in Task B2 and will live behind `#[cfg(...)]` in this same file — leave a clearly-commented placeholder section for it but add NO macOS imports yet.
  </action>
  <verify>
    <automated>cargo test -p macos-host avcc &amp;&amp; cargo test -p macos-host annex_b &amp;&amp; cargo test --workspace</automated>
  </verify>
  <done>avcc_to_annex_b and to_annex_b_frame exist with the behaviors above; all new tests pass; `cargo test --workspace` is green; no macOS/external crate is imported in the compiled (non-cfg-gated) code; lib.rs declares the module.</done>
</task>

<task type="tdd" tdd="true">
  <name>Task A2: Capture-adapter selection + virtual-display fallback logic (cable-free, TDD)</name>
  <files>crates/macos-host/src/capture_select.rs, crates/macos-host/src/lib.rs</files>
  <behavior>
    Implements RESEARCH Pitfall 1 selection logic as PURE, testable code — abstract "the list of shareable display IDs" behind a tiny trait so no real SCK call is needed in tests. Write failing tests FIRST.

    Define `trait DisplaySource { fn shareable_display_ids(&self) -> Vec<u32>; }` and an enum `CaptureBackend { ScreenCaptureKit, CgDisplayStream }`.

    `select_backend(source: &dyn DisplaySource, target_id: u32) -> CaptureBackend`:
    - Test (present → SCK): target_id is in the shareable list → `ScreenCaptureKit`.
    - Test (absent → fallback): target_id NOT in the list → `CgDisplayStream` (the co-equal fallback, CONTEXT D4).
    - Test (empty list → fallback): empty shareable list (e.g. TCC not granted / virtual display invisible) → `CgDisplayStream`.
    - Test (multiple displays, target present): list has several ids incl. target → `ScreenCaptureKit` (matches by id, never by index — anti-pattern guard).

    Optionally `enum SelectError { NoCapturePathAvailable }` reserved for the spike to escalate when BOTH paths fail at runtime (documented; the keystone-escalation case from Pitfall 1.4 is a hands-on-Mac runtime decision, not unit-testable here).
  </behavior>
  <action>
    Create `crates/macos-host/src/capture_select.rs` with `DisplaySource`, `CaptureBackend`, and `select_backend` plus the `#[cfg(test)]` module using a `FakeDisplaySource { ids: Vec<u32> }`. PURE logic only — no macOS imports in compiled code. Match strictly by display id (never index) per the RESEARCH anti-pattern and threat T-P3-03 (capturing the wrong display). Add `pub mod capture_select;` to `lib.rs`. The real SCK/CGDisplayStream adapters that this selection chooses between are built in Wave B; this task only delivers the decision logic and the trait they will implement.
  </action>
  <verify>
    <automated>cargo test -p macos-host capture_select &amp;&amp; cargo test --workspace</automated>
  </verify>
  <done>select_backend chooses SCK when target id is shareable and CgDisplayStream otherwise (incl. empty list); all matching is by id not index; tests pass; `cargo test --workspace` green; no external/macOS imports in compiled code.</done>
</task>

<!-- ============================ WAVE B — HANDS-ON-MAC (ONE GATED HUMAN SESSION) ============================ -->
<!-- NOT CI-automatable. Requires a live Mac + Screen Recording TCC grant. Adapters are
     cfg-gated (e.g. behind a `live-capture` feature / `#[cfg(all(target_os = "macos", feature = "live-capture"))]`)
     so Wave A CI stays green and the [ASSUMED] crates are not compiled until installed.
     Each dependency install is gated behind the B0 checkpoint:human-verify (CONTEXT D6 — slopcheck unavailable). -->

<task type="checkpoint:human-verify" gate="blocking-human">
  <what-built>NOTHING auto-installed yet — this is the supply-chain legitimacy + dependency-wiring gate before any `cargo add`. RESEARCH's Package Legitimacy Audit could not run slopcheck (sandbox denied it), so every external crate is `[ASSUMED]` and `videotoolbox` 0.18.0 is `[SUS]` (633 downloads, ~2 weeks old). Threat T-P3-SC. This gate also wires in the in-tree `cg-virtual-display` P2 crate that the spike `main` (Task B3) needs but which `crates/macos-host/Cargo.toml` does not yet depend on (BLOCKER fix).</what-built>
  <how-to-verify>
    For each external crate below, open its crates.io page and confirm it is the legitimate, expected package (publisher, repo link to the stated GitHub org, download count, recent release date), then approve installs one group at a time:
    1. doom-fish lead adapters (verify github.com/doom-fish):
       - `screencapturekit` — https://crates.io/crates/screencapturekit  (expect lineage 2+ yrs, ~600k downloads; v7.x — run `cargo info screencapturekit` to confirm latest 7.x at install time, churn warning RESEARCH Pitfall 3)
       - `videotoolbox` — https://crates.io/crates/videotoolbox  ([SUS]: confirm ~633 downloads / ~2 weeks old / author = doom-fish; accept the risk knowingly, it stays behind the Encoder trait)
    2. madsmtm objc2 family (verify github.com/madsmtm/objc2, all low-risk):
       - `objc2-core-video`, `objc2-core-media`, `objc2-io-surface`, `objc2-core-graphics` — https://crates.io/crates/objc2-core-video etc.

    Then wire dependencies into `crates/macos-host/Cargo.toml` so that ALL of them are macOS-only and OFF by default (default build + any Linux CI leg compile none of them):
    a. Add each external crate as an OPTIONAL dependency, e.g.:
       `cargo add -p macos-host --optional screencapturekit@7 videotoolbox@0.18 objc2-core-video@0.3 objc2-core-media@0.3 objc2-io-surface@0.3 objc2-core-graphics@0.3`
    b. BLOCKER FIX — add the in-tree P2 crate the spike main needs, also OPTIONAL:
       `cargo add -p macos-host --optional cg-virtual-display --path ../cg-virtual-display`
       (or hand-edit Cargo.toml: `cg-virtual-display = { path = "../cg-virtual-display", optional = true }`).
    c. Define the feature in `[features]` of `crates/macos-host/Cargo.toml`, enabling every optional dep:
       `live-capture = ["dep:screencapturekit", "dep:videotoolbox", "dep:objc2-core-video", "dep:objc2-core-media", "dep:objc2-io-surface", "dep:objc2-core-graphics", "dep:cg-virtual-display"]`
    d. All adapter/spike code that touches these crates must be `#[cfg(all(target_os = "macos", feature = "live-capture"))]`.

    Expected: default `cargo build -p macos-host` succeeds and compiles NONE of these crates (verify with `cargo tree -p macos-host` — none listed; with `cargo tree -p macos-host --features live-capture` — all listed); `cargo build -p macos-host --features live-capture` pulls them in on macOS; `Cargo.lock` updated; no new deps appear in the pure `protocol` crate's subtree (`cargo tree -p protocol` unchanged).
  </how-to-verify>
  <resume-signal>Type "approved" after verifying each external crate on crates.io, adding `cg-virtual-display` as an optional dep, defining the `live-capture` feature, and confirming the default build pulls in none of them — or "reject &lt;crate&gt;" to swap to the madsmtm low-level fallback (`objc2-video-toolbox` / `objc2-screen-capture-kit`) before continuing.</resume-signal>
</task>

<task type="auto">
  <name>Task B1: SCK + CGDisplayStream capture adapters behind the Capturer seam (hands-on-Mac, cfg-gated)</name>
  <files>crates/macos-host/src/capture_sck.rs, crates/macos-host/src/capture_cgds.rs, crates/macos-host/src/lib.rs</files>
  <action>
    Behind `#[cfg(all(target_os = "macos", feature = "live-capture"))]` (so Wave A CI is untouched), implement two adapters. Confirm exact crate method names against `cargo doc --open` first (RESEARCH A3: `display_id()`, `image_buffer()`, `io_surface()`, `with_display()` are [ASSUMED]); the `Capturer` trait quarantines any drift.

    capture_sck.rs — `SckCapturer`: `SCShareableContent::get()`, find the `SCDisplay` whose `displayID == target_id` (RESEARCH Pattern 1 — match by id, NEVER `displays()[0]`); on absence return a typed error (`VirtualDisplayNotShareable`) so the spike falls back. Build `SCContentFilter` for that display, `SCStreamConfiguration` at the P2 geometry (2400×1080, D6), start an `SCStream` with an output handler that exposes each frame's `IOSurface`. Implement `impl DisplaySource for ...` (from `crate::capture_select`) returning the live shareable display ids (consumed by `select_backend` from Task A2). The adapter retains the `IOSurface` internally for zero-copy hand-off to the encoder; `next_frame()` returns only `CapturedFrame` metadata (pts/width/height) — the surface NEVER crosses the trait (CONTEXT D1/D5).

    capture_cgds.rs — `CgDisplayStreamCapturer`: `CGDisplayStreamCreate(target_id, …)` via `objc2-core-graphics` as the co-equal fallback (CONTEXT D4); same contract — yields `CapturedFrame` metadata, retains the `IOSurface` internally. Deprecated since macOS 14 but functional on 26.4 (RESEARCH State of the Art).

    Both adapters expose an internal accessor (crate-private) the fused encode path uses to pull the current frame's `IOSurface` by matching pts (RESEARCH Open Question 1 option (b) — fused session, no signature change). Add `#[cfg(all(target_os = "macos", feature = "live-capture"))] pub mod capture_sck; ... pub mod capture_cgds;` to `lib.rs`. Do NOT modify `capture.rs`. Use `eprintln!` (not `log::*`) for the Pitfall-1 probe output.
  </action>
  <verify>
    <automated>cargo build -p macos-host --features live-capture &amp;&amp; cargo test --workspace</automated>
    <human-check>On the Mac, after granting Screen Recording (System Settings ▸ Privacy &amp; Security ▸ Screen Recording) and re-launching: the spike prints (eprintln!) every SCDisplay.displayID and reports whether the P2 id is present (the Pitfall-1 probe). If absent, it prints the CGDisplayStream fallback selection. If NEITHER path sees the display, STOP and escalate to the user (keystone-level, Pitfall 1.4) before B2/B3.</human-check>
  </verify>
  <done>Both adapters compile under `--features live-capture` on macOS; default `cargo test --workspace` (no feature) stays green; SckCapturer matches by displayID and errors with VirtualDisplayNotShareable when absent; the IOSurface is retained internally and never appears in CapturedFrame or any trait signature; the Pitfall-1 probe prints the display list to stderr.</done>
</task>

<task type="auto">
  <name>Task B2: VideoToolbox Encoder adapter, fused zero-copy with capture (hands-on-Mac, cfg-gated)</name>
  <files>crates/macos-host/src/encode_vt.rs, crates/macos-host/src/lib.rs</files>
  <action>
    In the cfg-gated section of `encode_vt.rs` (`#[cfg(all(target_os = "macos", feature = "live-capture"))]`, alongside the pure Task-A1 converter), implement `VtEncoder` (and, if cleaner, a fused `SckVtSession`) behind the existing `Encoder` trait — NO trait-signature change. Confirm builder method names via `cargo doc` first (RESEARCH A2: low-latency / disable-B-frames method names are [ASSUMED]).

    Configure the `videotoolbox` `CompressionSession`: `Codec::H264`, real-time = true, average bitrate ~12 Mbit/s for 2400×1080 (RESEARCH A7 — tune during the run), expected frame rate 60, max keyframe interval 120 (~2 s), and disable B-frames / frame reordering (`with_allow_frame_reordering(false)` or the equivalent low-latency property — criterion #3 "no B-frames"). Keep ALL videotoolbox types inside this adapter (R3 isolation so it can be swapped for `objc2-video-toolbox`).

    Encode path (RESEARCH Pattern 2, zero-copy): pull the current frame's `IOSurface` from the capture adapter (by matching `CapturedFrame.pts_us`), `t0 = Instant::now()`, `session.encode(&surface, (pts_us, 1_000_000))`, `encode_micros = t0.elapsed()`. Do NOT lock/copy the pixel buffer in the hot path (RESEARCH anti-pattern, criterion #3). Take the encoder's output and run it through the Task-A1 `to_annex_b_frame()` (verify on the Mac whether the crate already emits Annex-B per RESEARCH Pattern 3 option (a); if it emits AVCC, the converter + on-keyframe SPS/PPS injection from the format description handles it). Return an `EncodedFrame { pts_us, keyframe, encode_micros, annex_b }`. Add the module gating to `lib.rs` as needed.
  </action>
  <verify>
    <automated>cargo build -p macos-host --features live-capture &amp;&amp; cargo test --workspace</automated>
    <human-check>On the Mac, inspect the first bytes of a produced frame: confirm `00 00 00 01` start codes (Annex-B). If the bytes are AVCC, confirm the Task-A1 converter path is engaged. Confirm B-frames are disabled (no reordering) in the encoder config.</human-check>
  </verify>
  <done>VtEncoder implements the existing Encoder trait with no signature change; compiles under `--features live-capture`; videotoolbox types do not leak past the adapter; output is Annex-B (directly or via the A1 converter); encode_micros is measured per frame; default `cargo test --workspace` stays green.</done>
</task>

<task type="auto">
  <name>Task B3: Spike main — wire P2 display → selected capturer → VtEncoder → run_session → out.h264 (hands-on-Mac, cfg-gated)</name>
  <files>crates/macos-host/src/main.rs</files>
  <action>
    Behind `#[cfg(all(target_os = "macos", feature = "live-capture"))]`, add a spike entry path to `main.rs` (e.g. a `capture-spike` subcommand; the default `main` keeps its current behavior so the non-feature build is unchanged). Reach all lib items via the `macos_host::` crate path (this is the BINARY crate, not the lib — `crate::...` is wrong here). Wiring is mostly orchestration over the existing tested pipeline:
    1. `let vdisp = cg_virtual_display::VirtualDisplay::new(2400, 1080, 60.0)?;` (D6 geometry); `println!` the `display_id()` and `active_display_count()`.
    2. Build the SCK adapter (`macos_host::capture_sck::SckCapturer`); call `macos_host::capture_select::select_backend` (Task A2) with the SCK adapter's `shareable_display_ids()` to choose SCK vs CGDisplayStream; on `VirtualDisplayNotShareable` fall back to `macos_host::capture_cgds::CgDisplayStreamCapturer` (CONTEXT D4). If neither path yields a working capturer, `eprintln!` an error and escalate (Pitfall 1.4) — do not silently produce a black file.
    3. `let mut encoder = macos_host::encode_vt::VtEncoder::h264_realtime(2400, 1080, 12_000_000, 120)?;` fused with the chosen capturer's IOSurface source.
    4. `let mut file = std::fs::File::create("out.h264")?;` then `let summary = macos_host::encode::run_session(&mut *capturer, &mut encoder, &mut file, 600)?;` (~10 s @ 60 fps).
    5. Criterion #2 — `println!` `summary.codec_config` SPS/PPS (use `eprintln!` for an error if `None` — likely AVCC-not-converted, Pitfall 2). Criterion #3 — `println!` `summary.latency.min()/mean()/max()` and frame count. Use `println!`/`eprintln!` ONLY (no `log` crate is wired, so `log::*` would print nothing).
    6. Keep `vdisp` alive until AFTER `run_session` returns (`drop(vdisp)` explicit at the end — Pitfall 5).
    `out.h264` is already in `.gitignore`. Do NOT commit anything (CONTEXT D8).
  </action>
  <verify>
    <automated>cargo build -p macos-host --features live-capture &amp;&amp; cargo test --workspace</automated>
    <human-check>See Task B4 — the spike run + ffplay is the end-to-end visual gate; the SPS/PPS + latency lines must appear on stdout.</human-check>
  </verify>
  <done>The spike subcommand compiles under `--features live-capture` and reaches lib items via `macos_host::*`; default `main` unchanged for the non-feature build; it creates the P2 display, selects a capturer via select_backend with CGDisplayStream fallback, runs run_session into out.h264, holds vdisp alive across the call, and prints SPS/PPS + latency to stdout/stderr (not via the silent `log` macros). Default `cargo test --workspace` stays green.</done>
</task>

<task type="checkpoint:human-verify" gate="blocking-human">
  <what-built>The full hands-on-Mac end-to-end run: P2 virtual display → live capture (SCK or CGDisplayStream fallback) → VideoToolbox H.264 encode → `out.h264`. This is criterion #1 (visual playback), which is inherently a human check, plus confirmation of criteria #2/#3 from a live encode (printed to stdout/stderr).</what-built>
  <how-to-verify>
    1. Ensure exactly ONE virtual display exists during the run (RESEARCH Pitfall 1.3 avoids the multi-virtual-display confusion bug).
    2. Run the spike: `cargo run -p macos-host --features live-capture -- capture-spike`.
    3. If macOS prompts for Screen Recording, grant it (System Settings ▸ Privacy &amp; Security ▸ Screen Recording) and re-launch the spike (some macOS versions require relaunch — Pitfall 4).
    4. Read the program's STDOUT/STDERR (the spike uses `println!`/`eprintln!`, not the `log` crate): confirm the P2 display id appears in the printed SCShareableContent list (or that the CGDisplayStream fallback was selected); confirm printed SPS/PPS are non-empty (criterion #2); confirm per-frame encode latency min/mean/max are printed (criterion #3). Record the real latency numbers (feeds P5).
    5. Play the output: `ffplay out.h264`. Expected: recognizable video of the virtual desktop (criterion #1). If ffplay shows nothing / errors, check the first bytes are `00 00 00 01` (Annex-B, not AVCC — Pitfall 2) and that SPS/PPS were injected on the keyframe.
    6. If the P2 display is invisible to BOTH SCK and CGDisplayStream, STOP and report — this is keystone-level (Pitfall 1.4 / RESEARCH A4); the capture path may need to change.
  </how-to-verify>
  <resume-signal>Type "approved" once ffplay shows the virtual desktop AND stdout/stderr shows non-empty SPS/PPS + per-frame latency; or describe the failure (no display in SCShareableContent / AVCC output / black frames / wrong display / silent output) so the path can be reworked. Do NOT commit — leave the diff for review (CONTEXT D8).</resume-signal>
</task>

</tasks>

<threat_model>
## Trust Boundaries

| Boundary | Description |
|----------|-------------|
| encoder output → protocol::nal / avcc converter | Untrusted-length byte buffers (AVCC length prefixes) cross into pure parsing logic |
| third-party crate → build/proc-macro | New [ASSUMED]/[SUS] crates execute build code at install/compile |
| macOS Screen Recording (TCC) → app | OS access gate for all screen capture |

## STRIDE Threat Register

| Threat ID | Category | Component | Disposition | Mitigation Plan |
|-----------|----------|-----------|-------------|-----------------|
| T-P3-01 | Tampering / DoS | `avcc_to_annex_b` in `encode_vt.rs` | mitigate | Bounds-check every length prefix (`if i + len > avcc.len() { break; }`); explicit unit test for the overrun case (Task A1). ASVS V5. |
| T-P3-03 | Information disclosure | capture adapter display selection | mitigate | Match capture target strictly by `displayID`, never by index; human visually confirms the on-screen content is the virtual display during B4 (RESEARCH Pitfall 1 wrong-display bug). |
| T-P3-SC | Tampering (supply chain) | `cargo add` of `screencapturekit` / `videotoolbox` / `objc2-*` | mitigate | slopcheck unavailable → blocking-human checkpoint (B0) before install; crates.io verification of publisher/repo/age/downloads; `videotoolbox` [SUS] kept strictly behind the `Encoder` trait with `objc2-video-toolbox` as the swap target; all adapter deps are optional + macos-host-only (never `protocol`). |
| T-P3-04 | Elevation of privilege | macOS Screen Recording TCC | accept | Do not attempt to bypass the TCC gate; user grants it interactively (B4). Signing/notarization is P7 scope. |
</threat_model>

<verification>
## Wave A (cable-free, must pass in CI with no hardware)
- `cargo test -p macos-host avcc` — AVCC→Annex-B converter, incl. bounds-guard (T-P3-01) and round-trip through `extract_codec_config`.
- `cargo test -p macos-host annex_b` — keyframe SPS/PPS in-band injection.
- `cargo test -p macos-host capture_select` — SCK-vs-CGDisplayStream fallback selection by id.
- `cargo test --workspace` — all 33 prior tests + the new ones green; no external/macOS crate compiled (default features).

## Wave B (hands-on-Mac, one human session — not CI)
- `cargo tree -p macos-host` (default) lists none of the adapter crates; `cargo tree -p macos-host --features live-capture` lists all of them incl. `cg-virtual-display`; `cargo tree -p protocol` unchanged.
- `cargo build -p macos-host --features live-capture` compiles the adapters + spike on macOS.
- Pitfall-1 probe: P2 display id present in the printed SCShareableContent list, OR CGDisplayStream fallback selected.
- `cargo run -p macos-host --features live-capture -- capture-spike` produces `out.h264` and prints SPS/PPS + latency to stdout/stderr.
- `ffplay out.h264` shows the virtual desktop (criterion #1).

## Success-criteria → task map (ENC-01) — see also P3-VALIDATION.md
| Criterion | Status before this plan | Closed by |
|-----------|-------------------------|-----------|
| #1 out.h264 plays in ffplay showing the virtual desktop | open | B1+B2+B3 → B4 (visual gate) |
| #2 SPS/PPS extracted and logged | met at logic level (`protocol::nal`) | A1 (in-band injection so it survives AVCC), printed live in B3, confirmed in B4 |
| #3 per-frame encode latency, realtime/no-B-frames/zero-copy | met at logic level (`encode::run_session`/`LatencyStats`) | B2 (real encode_micros, no-B-frame config, zero-copy IOSurface), printed live in B3, confirmed in B4 |
</verification>

<success_criteria>
- Wave A: `avcc_to_annex_b`, `to_annex_b_frame`, and `select_backend` exist, are TDD-tested (red→green→refactor), and `cargo test --workspace` is green with no new external/macOS dependency compiled.
- Locked ports unchanged: `Capturer`/`CapturedFrame` and `Encoder`/`EncodedFrame`/`LatencyStats`/`run_session` signatures are byte-for-byte unchanged; `CapturedFrame` has no IOSurface field; the IOSurface never crosses a trait boundary (fused adapter).
- Dependency hygiene: `cg-virtual-display` + the external adapter crates are OPTIONAL deps of macos-host enabled only by the `live-capture` feature; the default build and Linux CI compile none of them; `protocol` gains no new deps.
- Spike observability: criteria #2/#3 are emitted via `println!`/`eprintln!` (no silent `log` macros) so the B4 reviewer can read SPS/PPS + latency on stdout/stderr.
- Wave B: adapters + spike compile under `--features live-capture`; the hands-on-Mac run yields `out.h264` that plays in ffplay (criterion #1) with printed non-empty SPS/PPS (criterion #2) and per-frame latency (criterion #3).
- Every install of an [ASSUMED]/[SUS] crate was approved through the B0 blocking-human checkpoint; `videotoolbox` stays isolated behind the `Encoder` trait.
- Nothing is committed: all code/docs remain an uncommitted working-tree diff for user review (CONTEXT D8).
</success_criteria>

<output>
Create `.planning/phases/P3-capture-hardware-encode-on-macos/P3-01-SUMMARY.md` when done.
Note: per CONTEXT D8, do NOT git-commit the SUMMARY, the plan, the P3-VALIDATION.md, or the code this session — leave them as a working-tree diff for the user to review.
</output>
