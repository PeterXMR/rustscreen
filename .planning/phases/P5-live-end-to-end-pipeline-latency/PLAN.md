---
phase: P5-live-end-to-end-pipeline-latency
plan: 01
type: tdd
wave: 1
depends_on: []
files_modified:
  - crates/protocol/Cargo.toml
  - crates/protocol/src/messages.rs
  - crates/protocol/src/lib.rs
autonomous: false
requirements: [PIPE-01]

user_setup: []

must_haves:
  truths:
    - "Every Frame variant (Handshake, VideoConfig, Video, Touch, Control) encodes to a framing frame and decodes back to an equal value"
    - "The Video payload is written raw as [pts_us u64 BE][keyframe u8][nal bytes verbatim] with no serde/postcard framing over the NAL buffer"
    - "Decoding an unknown tag returns a typed error (not a panic, not a silent skip); a truncated Video header returns a typed error; malformed postcard payloads return a typed error"
    - "negotiate(host, client) returns AgreedConfig on the happy path with the host-preferred common codec and refresh clamped to the client maximum"
    - "negotiate returns VersionMismatch on version inequality, NoCommonCodec when codec sets do not intersect, and ResolutionUnsupported when the offer exceeds the client's max dimensions"
    - "negotiate accepts a Handshake + ClientCaps whose versions both come from protocol_version() (the lib function is wired in, not a hardcoded 1)"
    - "serde + postcard are added to crates/protocol/Cargo.toml with default-features = false (no_std-friendly) and the crate still builds + tests green"
  artifacts:
    - path: "crates/protocol/src/messages.rs"
      provides: "Frame enum + per-variant u8 tag constants + encode/decode codec layered on framing + pure negotiate() + message/negotiation error types + inline TDD tests"
      contains: "pub fn negotiate"
      min_lines: 200
    - path: "crates/protocol/Cargo.toml"
      provides: "serde + postcard dependency declarations (default-features = false, alloc)"
      contains: "postcard"
    - path: "crates/protocol/src/lib.rs"
      provides: "pub mod messages; export"
      contains: "pub mod messages"
  key_links:
    - from: "crates/protocol/src/messages.rs"
      to: "crates/protocol/src/framing.rs"
      via: "Frame::write_to / read_from delegate to framing::write_frame / read_frame (no second length-prefix layer)"
      pattern: "framing::(write_frame|read_frame)"
    - from: "crates/protocol/src/messages.rs"
      to: "crates/protocol/src/lib.rs"
      via: "Handshake.protocol_version is checked against protocol_version() (= 1) as the handshake version source of truth"
      pattern: "protocol_version\\(\\)"
    - from: "crates/protocol/src/messages.rs"
      to: "postcard"
      via: "structured payloads (Handshake/VideoConfig/Touch/Control) encode via postcard::to_allocvec and decode via postcard::from_bytes"
      pattern: "postcard::(to_allocvec|from_bytes)"
---

<objective>
Deliver the cable-free half of P5 success criterion #3: a `protocol::messages` module containing the `Frame` message enum, a codec that round-trips every variant over the existing length-prefixed `framing`, and a pure `negotiate()` handshake/resolution-negotiation function — all TDD, all unit-testable on the dev host with no cable, no Pixel, and no transport.

Purpose: PIPE-01's protocol/negotiation logic is fully implementable now. This plan closes that logic at the unit-test level so the live wiring (deferred — see below) only has to serialize/deserialize `Frame`s and feed decoded `Handshake`/`ClientCaps` structs into `negotiate()`. The genuinely new code is small: tag dispatch, the raw `Video` header, the message types, and `negotiate()` — everything else is composition over the already-tested `framing` and `nal` modules.

Output:
- `crates/protocol/Cargo.toml`: `serde` + `postcard` added (both `default-features = false` to keep the new types `no_std`-ready per roadmap §107), gated behind ONE blocking-human supply-chain checkpoint (Task 0) because slopcheck was unavailable (RESEARCH Package Legitimacy Audit → both `[ASSUMED]`).
- `crates/protocol/src/messages.rs`: `Frame`, `Handshake`, `ClientCaps`, `Control`, `TouchEvent`, `TouchPhase`, `VideoCodec`, `AgreedConfig`, `NegotiationError`, `MessageError`; tag constants 1–5; `Frame::write_to`/`read_from`/`decode`/`to_tag_payload`; `negotiate()`; full inline `#[cfg(test)]` TDD suite.
- `crates/protocol/src/lib.rs`: `pub mod messages;`.

EXPLICITLY DEFERRED (cable/device-blocked — NOT in this plan; CONTEXT scope_split): the live frame thread (capture→encode→framing→transport), the real USB/socket transport (P1), runtime "send VideoConfig on connect/keyframe", the phone RX/deframe/decode loop (P4), glass-to-glass latency measurement and per-stage timing (PIPE-01 criteria #1/#2), jitter buffer / backpressure / drop-to-keyframe, and `coords.rs` / touch capture+injection (P6). These need a wire, a phone, or a stopwatch; none are touched here.

Layering contract: this module sits ON `framing::{write_frame, read_frame}` and reuses `MAX_FRAME_LEN`. Do NOT reinvent length-prefixing, do NOT modify `framing.rs`, `nal.rs`, or `protocol_version()`. `nal::CodecConfig` (SPS/PPS) feeds `VideoConfig`. `protocol_version()` (= 1) is the handshake version source of truth.

Single-crate, pure-Rust, runs on any host — NO `cfg`-gating needed (unlike P3's macOS adapters).

Branch / no-merge: all artifacts and code are committed on the `feat/p5-protocol-messages` branch only; do NOT merge to `main` without explicit user confirmation. This planning session itself commits nothing.
</objective>

<execution_context>
@$HOME/.claude/get-shit-done/workflows/execute-plan.md
@$HOME/.claude/get-shit-done/templates/summary.md
</execution_context>

<context>
@.planning/phases/P5-live-end-to-end-pipeline-latency/CONTEXT.md
@.planning/phases/P5-live-end-to-end-pipeline-latency/RESEARCH.md
@.planning/ROADMAP.md
@.planning/REQUIREMENTS.md

# Existing modules — BUILD ON these, do NOT re-plan or modify them:
@crates/protocol/src/framing.rs
@crates/protocol/src/nal.rs
@crates/protocol/src/lib.rs
@crates/protocol/Cargo.toml

<interfaces>
<!-- Contracts the executor consumes without modifying. Extracted from the codebase. -->

From crates/protocol/src/framing.rs (consume; already tested — DO NOT modify):
```rust
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;
/// Errors with io::ErrorKind::InvalidInput if payload > MAX_FRAME_LEN.
pub fn write_frame(w: &mut dyn std::io::Write, tag: u8, payload: &[u8]) -> std::io::Result<()>;
/// Returns (tag, payload). Errors on EOF (truncated header/payload) or len > MAX_FRAME_LEN.
pub fn read_frame(r: &mut dyn std::io::Read) -> std::io::Result<(u8, Vec<u8>)>;
```

From crates/protocol/src/nal.rs (consume; already tested):
```rust
pub struct CodecConfig { pub sps: Vec<u8>, pub pps: Vec<u8> } // feeds VideoConfig.sps_pps
pub fn extract_codec_config(stream: &[u8]) -> Option<CodecConfig>;
pub fn is_keyframe(stream: &[u8]) -> bool;
```

From crates/protocol/src/lib.rs (consume; the handshake version source of truth):
```rust
pub fn protocol_version() -> u32; // returns 1
```

<!-- Tag registry to define in messages.rs (RESEARCH Pattern 1 — explicit, append-only, never renumber): -->
<!-- HANDSHAKE = 1, VIDEO_CONFIG = 2, VIDEO = 3, TOUCH = 4, CONTROL = 5 -->

<!-- Recommended type shapes (RESEARCH "Recommended concrete field sets" + locked decision 7).
     VideoCodec variant names are LOCKED by CONTEXT decision 7: H264, Hevc (NOT H265).
     All structured types derive (Serialize, Deserialize, Debug, Clone, PartialEq) — Eq where no f32:
       enum VideoCodec { H264, Hevc }                          (Copy, Eq)   // Hevc reserved; H264 only exercised now (D2)
       struct Handshake { protocol_version: u32, width: u32, height: u32, refresh_hz: u32, codecs: Vec<VideoCodec> }
       struct ClientCaps { protocol_version: u32, max_width: u32, max_height: u32, max_refresh_hz: u32, codecs: Vec<VideoCodec> }
       enum Control { RequestKeyframe, Pause, Resume, Bye }     (Eq)
       struct TouchEvent { pointer_id: u32, phase: TouchPhase, nx: f32, ny: f32 }   (no Eq — f32)
       enum TouchPhase { Down, Move, Up }                      (Copy, Eq)
       struct AgreedConfig { width: u32, height: u32, refresh_hz: u32, codec: VideoCodec }  (Eq)
       enum NegotiationError { VersionMismatch{host,client}, NoCommonCodec, ResolutionUnsupported{offered:(u32,u32), client_max:(u32,u32)} }  (Eq)
     Frame variants (roadmap §301, locked decision 1):
       Handshake(Handshake) | VideoConfig { codec: VideoCodec, sps_pps: Vec<u8> } | Video { pts_us: u64, keyframe: bool, nal: Vec<u8> } | Touch(TouchEvent) | Control(Control)
     Exact API surface (write_to/read_from + pure decode/to_tag_payload seams) and submodule layout are Claude's Discretion (CONTEXT discretion). -->
</interfaces>
</context>

<tasks>

<!-- ============================ TASK 0 — DEPENDENCY GATE (BLOCKS THE CODE TASKS) ============================ -->
<!-- Must run and be approved BEFORE Task 1/Task 2, which import serde/postcard. -->

<task type="checkpoint:human-verify" gate="blocking-human">
  <what-built>NOTHING auto-installed yet — this is the supply-chain legitimacy gate before adding `serde` + `postcard` to `crates/protocol/Cargo.toml`. RESEARCH's Package Legitimacy Audit could not run slopcheck (binary absent / sandbox), so both crates are formally `[ASSUMED]` (CONTEXT decision 4). Threat T-P5-SC. Both are ecosystem-foundational (serde underpins Rust serialization; postcard is the roadmap-locked no_std serde wire format), so practical risk is negligible — but the formal verification step stands and must be approved before any code imports them.</what-built>
  <how-to-verify>
    For EACH crate, open its crates.io page and confirm it is the legitimate, expected package on the correct registry (crates.io, NOT a typosquat from another ecosystem) — publisher, repo link, age, download count, and that the pinned version exists:
    1. `serde` — https://crates.io/crates/serde — expect ~9 yrs old, billions of downloads, repo github.com/serde-rs/serde. Confirm version `1.0.228` exists (or note the current 1.0.x; any 1.0.x is acceptable since the dep is pinned as `"1"`).
    2. `postcard` — https://crates.io/crates/postcard — expect ~6 yrs old, high downloads, repo github.com/jamesmunns/postcard. Confirm version `1.1.3` exists (or current 1.1.x; dep pinned as `"1"`).

    Then add BOTH to `crates/protocol/Cargo.toml` `[dependencies]` with the EXACT features from CONTEXT decision 4 (keeps the new message types `no_std`-friendly per roadmap §107 — do NOT enable postcard's `use-std`):
    ```toml
    serde = { version = "1", default-features = false, features = ["derive", "alloc"] }
    postcard = { version = "1", default-features = false, features = ["alloc"] }
    ```
    (Equivalent: `cargo add -p protocol serde --no-default-features --features derive,alloc` and `cargo add -p protocol postcard --no-default-features --features alloc`.)

    Expected after the add: `cargo build -p protocol` succeeds; `Cargo.lock` updated; `cargo tree -p protocol` shows exactly `serde` + `postcard` (+ their transitive deps, e.g. `serde_derive`, `cobs`/`postcard` internals) and NOTHING else surprising; no other workspace crate gains a dep from this change.
  </how-to-verify>
  <resume-signal>Type "approved" after verifying both crates on crates.io and adding them to crates/protocol/Cargo.toml with `default-features = false` and the exact features above — or "reject &lt;crate&gt;" to halt and re-evaluate (e.g. swap postcard→bincode, though the roadmap locked postcard). This is a blocking-human gate (never auto-approved) per the package legitimacy protocol. Tasks 1 and 2 are BLOCKED until this resume-signal is given.</resume-signal>
</task>

<!-- ============================ TASK 1 — CODEC (TDD) ============================ -->

<task type="tdd" tdd="true">
  <name>Task 1: Frame message types + codec layered on framing (TDD)</name>
  <files>crates/protocol/src/messages.rs, crates/protocol/src/lib.rs</files>
  <behavior>
    BLOCKED ON TASK 0: do not start until Task 0's resume-signal ("approved") is given — this task imports `serde`/`postcard`, which Task 0 adds to `Cargo.toml`. Starting earlier will not compile.

    TASK SEAM (Task 1 vs Task 2): Task 1 adds the `Frame` codec + message TYPES ONLY. It adds NO `negotiate`, `AgreedConfig`, or `NegotiationError` code — not even empty stubs. Task 1's green bar depends solely on the Frame codec + types round-tripping. `negotiate()` and its two types are introduced test-first in Task 2. This keeps both red→green cycles honest (Task 1 cannot accidentally "pre-pass" Task 2's behavior).

    Write the failing tests FIRST and watch each fail before implementing (red → green → refactor). All tests are pure, host-only, no transport. Mirror the synthetic-buffer test style of `framing.rs`/`nal.rs`. Name codec tests with the prefixes the verify commands filter on: round-trip tests start with `roundtrip_` (e.g. `roundtrip_handshake`), the raw-layout tests start with `video_payload_` (e.g. `video_payload_raw_layout`, `video_payload_keyframe_false_byte`), and the malformed tests contain `decode_rejects` (e.g. `decode_rejects_unknown_tag`).

    Tag registry (RESEARCH Pattern 1 — explicit `u8` constants, append-only, never renumber): HANDSHAKE=1, VIDEO_CONFIG=2, VIDEO=3, TOUCH=4, CONTROL=5.

    Codec round-trip (encode → framing bytes → decode → equal value), one test per variant (all named `roundtrip_*`):
    - Test `roundtrip_handshake`: `Frame::Handshake(Handshake { protocol_version: protocol_version(), width: 2400, height: 1080, refresh_hz: 60, codecs: vec![VideoCodec::H264] })` round-trips to an equal value via `write_to` into a `Vec<u8>` then `read_from` a `Cursor`.
    - Test `roundtrip_video_config`: a `Frame::VideoConfig { codec: VideoCodec::H264, sps_pps }` whose `sps_pps` is taken from `nal::extract_codec_config` output round-trips (key_link: SPS/PPS carried faithfully).
    - Test `roundtrip_video_normal`: `Frame::Video { pts_us: 123, keyframe: true, nal: vec![0,1,2,3] }` round-trips.
    - Test `roundtrip_video_empty_nal`: `nal: vec![]` round-trips (header-only payload, 9 bytes).
    - Test `roundtrip_video_large_nal`: `nal` of e.g. 100_000 bytes round-trips (exercises the no-serde bulk path; stays well under MAX_FRAME_LEN).
    - Test `roundtrip_touch`: `Frame::Touch(TouchEvent { pointer_id: 1, phase: TouchPhase::Move, nx: 0.5, ny: 0.25 })` round-trips (PartialEq on f32 — use exact representable values like 0.5/0.25).
    - Test `roundtrip_control_variants`: each of `Control::{RequestKeyframe, Pause, Resume, Bye}` round-trips.
    - Test `roundtrip_multi_frame_stream`: write three different frames into ONE `Vec<u8>`, then `read_from` the same `Cursor` three times and assert each decodes back in order (proves self-delimiting framing is reused, not reinvented).

    Video payload is RAW (locked decision 3) — assert exact bytes, not just round-trip (tests named `video_payload_*`):
    - Test `video_payload_raw_layout`: encode `Frame::Video { pts_us: 0x0102030405060708, keyframe: true, nal: vec![0xAA, 0xBB] }`; inspect the framing payload (via the pure `to_tag_payload()` seam, or by reading the frame back at the framing layer) and assert it equals `[01 02 03 04 05 06 07 08][01][AA BB]` — i.e. pts_us as u64 BE, then keyframe as one byte (1), then the NAL verbatim. NO postcard/serde length prefix anywhere in this payload.
    - Test `video_payload_keyframe_false_byte`: same with `keyframe: false` → the 9th byte is `00`.

    Malformed / robustness (decode returns typed errors — never panic, never silently skip; locked decision 6; tests contain `decode_rejects`):
    - Test `decode_rejects_unknown_tag`: `Frame::decode(99, &[])` → `Err(MessageError::UnknownTag(99))`.
    - Test `decode_rejects_short_video_header`: `Frame::decode(VIDEO, &[0,0,0])` (< 9 bytes) → `Err(MessageError::ShortVideoHeader)`.
    - Test `decode_rejects_malformed_postcard`: `Frame::decode(HANDSHAKE, &[0xFF; 3])` (garbage for the Handshake struct) → `Err(MessageError::Decode)` (no panic).
    - Test (oversized handled by framing): document/assert that oversized payloads are rejected by `framing` BEFORE `messages` sees them — reuse the existing `framing` MAX_FRAME_LEN guarantee (no new guard in `messages`; a comment + the framing tests cover this; optionally a test that `write_to` of a `Video` with a > MAX_FRAME_LEN nal surfaces the framing InvalidInput error).
  </behavior>
  <action>
    Create `crates/protocol/src/messages.rs`. Define the tag constants (a `mod tag { pub const ... }` or associated consts), the message types from the `<interfaces>` block (Frame + Handshake/ClientCaps/Control/TouchEvent/TouchPhase/VideoCodec, all with the serde derives noted there; `VideoCodec` variants are `H264` and `Hevc` per CONTEXT decision 7 — never `H265`), and a `MessageError` enum with at least `UnknownTag(u8)`, `ShortVideoHeader`, `Decode`, and an `Io(std::io::Error)`-style variant for framing errors (map framing's `io::Error` into it). Implement:
    - `Frame::to_tag_payload(&self) -> Result<(u8, Vec<u8>), MessageError>` — PURE (no I/O): structured variants via `postcard::to_allocvec` (NOT `to_stdvec`/`use-std`); the `Video` variant builds the raw 9-byte header (`pts_us.to_be_bytes()`, then `keyframe as u8`) + `extend_from_slice(nal)` per RESEARCH Pattern 3 (one alloc, no serde over the NAL).
    - `Frame::decode(tag: u8, payload: &[u8]) -> Result<Frame, MessageError>` — PURE (no I/O): `match tag` → structured via `postcard::from_bytes` (map errors to `MessageError::Decode`); `VIDEO` → length-check `payload.len() >= 9` (else `ShortVideoHeader`), split header, `from_be_bytes` for pts (on the length-checked slice — safe `try_into`), `keyframe = payload[8] != 0`, `nal = payload[9..].to_vec()`; unknown tag → `Err(UnknownTag(tag))`.
    - `Frame::write_to(&self, w: &mut dyn std::io::Write) -> Result<(), MessageError>` — call `to_tag_payload`, then `framing::write_frame(w, tag, &payload)`, mapping the io error into `MessageError`.
    - `Frame::read_from(r: &mut dyn std::io::Read) -> Result<Frame, MessageError>` — call `framing::read_frame(r)`, then `Frame::decode(tag, &payload)`.
    Keep `to_tag_payload`/`decode` as the pure unit-test seams so byte assertions need no Read/Write. Do NOT add a second length-prefix layer — `write_frame`/`read_frame` own that. Do NOT use `postcard::to_stdvec` or enable `use-std` (defeats the no_std-ready intent). Do NOT modify `framing.rs`, `nal.rs`, or `protocol_version()`. Add `pub mod messages;` to `lib.rs`. DO NOT add `negotiate`, `AgreedConfig`, or `NegotiationError` in this task — not even stubs (they are Task 2, test-first). The file compiles fine without them.
  </action>
  <verify>
    <automated>cargo test -p protocol roundtrip &amp;&amp; cargo test -p protocol video_payload &amp;&amp; cargo test -p protocol -- decode_rejects &amp;&amp; cargo test --workspace</automated>
  </verify>
  <done>messages.rs exists with the tag registry, all message types (VideoCodec = {H264, Hevc}), MessageError, and the four codec functions; every Frame variant round-trips (incl. empty/large nal and a multi-frame single-buffer stream); the Video payload byte layout is asserted exactly as `[pts u64 BE][keyframe u8][nal...]` with no serde framing; unknown tag / short Video header / malformed postcard all return typed errors (no panic); NO `negotiate`/`AgreedConfig`/`NegotiationError` exist yet; `framing.rs`/`nal.rs`/`protocol_version()` are unmodified; lib.rs declares the module; each filtered `cargo test` token (`roundtrip`, `video_payload`, `decode_rejects`) matches ≥1 test; `cargo test --workspace` is green (existing 32 framing+nal+version tests still pass).</done>
</task>

<!-- ============================ TASK 2 — NEGOTIATION (TDD) ============================ -->

<task type="tdd" tdd="true">
  <name>Task 2: Pure negotiate() handshake/resolution negotiation (TDD)</name>
  <files>crates/protocol/src/messages.rs</files>
  <behavior>
    BLOCKED ON TASK 0: do not start until Task 0's resume-signal ("approved") is given — the message types this function consumes live in the serde/postcard-dependent `messages.rs`. (Task 2 also naturally follows Task 1, which creates the `Handshake`/`ClientCaps`/`VideoCodec` types `negotiate` consumes.)

    TASK SEAM: this task INTRODUCES `negotiate`, `AgreedConfig`, and `NegotiationError` for the first time — they did not exist after Task 1. Write the failing tests FIRST and watch them fail (the function/types are absent → compile-fail is the red bar), then implement. `negotiate` is a PURE function — no I/O, no global state — so the whole matrix is a table test (locked decision 5; RESEARCH "Handshake / resolution negotiation"). Signature: `pub fn negotiate(host: &Handshake, client: &ClientCaps) -> Result<AgreedConfig, NegotiationError>`. Name tests so they match the `negotiat` filter token used in verify (e.g. `negotiate_happy_path`, `negotiate_rejects_version_mismatch`, ...).

    - Test `negotiate_happy_path` (D6 default): host `{ protocol_version: 1, 2400, 1080, 60, codecs: [H264] }`, client `{ 1, max 2400, 1080, 60, codecs: [H264] }` → `Ok(AgreedConfig { width: 2400, height: 1080, refresh_hz: 60, codec: H264 })`.
    - Test `negotiate_uses_protocol_version_source_of_truth`: build BOTH `Handshake.protocol_version` AND `ClientCaps.protocol_version` from `protocol_version()` (the lib function — NOT a literal `1`), everything else compatible → `Ok(..)`. This pins that the version field is wired to the lib source of truth and that two equal `protocol_version()` values negotiate cleanly (if `protocol_version()` is ever bumped, both sides move together and this test still passes).
    - Test `negotiate_rejects_version_mismatch`: host version 2, client version 1 → `Err(VersionMismatch { host: 2, client: 1 })`. (Version check happens first, before codec/resolution.)
    - Test `negotiate_rejects_no_common_codec`: host `codecs: [Hevc]`, client `codecs: [H264]` → `Err(NoCommonCodec)`.
    - Test `negotiate_host_preferred_codec_wins`: host `codecs: [Hevc, H264]`, client `codecs: [H264, Hevc]` → agreed codec is `Hevc` (first host-preferred the client also supports — proves intersection is host-preference-ordered, NOT client-ordered).
    - Test `negotiate_rejects_resolution_too_big`: host `3840x2160`, client `max 2400x1080` → `Err(ResolutionUnsupported { offered: (3840,2160), client_max: (2400,1080) })`.
    - Test `negotiate_clamps_refresh_down`: host `refresh_hz: 60`, client `max_refresh_hz: 30` → agreed `refresh_hz: 30` (clamped, never exceeds the panel).
    - Test `negotiate_clamp_up_does_not_exceed_host`: host `refresh_hz: 60`, client `max_refresh_hz: 90` → agreed `refresh_hz: 60` (the min of the two; a better client still gets the offered 60).
    - Test `negotiate_boundary_fit`: host `2400x1080`, client `max 2400x1080` (exactly equal) → Ok (the dimension check is `>` not `>=`, so equal fits).
  </behavior>
  <action>
    In `messages.rs`, add `AgreedConfig` (fields width/height/refresh_hz/codec; derive Debug, Clone, PartialEq, Eq) and `NegotiationError` (variants `VersionMismatch { host: u32, client: u32 }`, `NoCommonCodec`, `ResolutionUnsupported { offered: (u32, u32), client_max: (u32, u32) }`; derive Debug, Clone, PartialEq, Eq) — these are NEW in this task; Task 1 deliberately left them out. Then implement `negotiate` in this order (RESEARCH reference impl): (1) version equality → else `VersionMismatch`; (2) first host-preferred codec the client also supports via `host.codecs.iter().copied().find(|c| client.codecs.contains(c))` → else `NoCommonCodec`; (3) `host.width > client.max_width || host.height > client.max_height` → `ResolutionUnsupported`; (4) `Ok(AgreedConfig { width: host.width, height: host.height, refresh_hz: host.refresh_hz.min(client.max_refresh_hz), codec })`. Use `VideoCodec::Hevc` (never `H265`) wherever the HEVC variant is referenced in tests. Do NOT hard-code D6's 2400×1080@60 inside `negotiate` — it must read the offered values (anti-pattern guard: hard-coding breaks P7 rotation renegotiation and the other-geometry tests). No `unwrap`/panic on any input path. The version comparison uses the same `u32` space as `protocol_version()`; the source-of-truth test above pins that wiring.
  </action>
  <verify>
    <automated>cargo test -p protocol negotiat &amp;&amp; cargo test --workspace</automated>
  </verify>
  <done>negotiate exists as a pure function with AgreedConfig + NegotiationError (all newly introduced in this task, test-first); the full matrix passes (happy path, protocol_version() source-of-truth wiring, version mismatch, no common codec, host-preference-ordered codec selection using Hevc, resolution overflow, refresh clamp down and clamp-up-capped-at-host, exact-fit boundary); the `negotiat` filter token matches ≥1 test; D6 geometry is never hard-coded; `cargo test --workspace` green.</done>
</task>

</tasks>

<threat_model>
## Trust Boundaries

| Boundary | Description |
|----------|-------------|
| wire bytes (peer) → `Frame::decode` / `read_from` | Attacker- or corruption-controlled tag + payload bytes cross into pure decode logic |
| third-party crate → build/proc-macro | New `[ASSUMED]` crates (`serde` derive proc-macro, `postcard`) execute build/derive code at compile time |

Note: the transport is a single physical USB peer (not network-exposed) in the MVP, so confidentiality/auth threats are out of scope here (flagged for P7/P8 only if an NCM/TCP-over-USB path is chosen). The `Video` `nal` payload is opaque application data, not parsed by `messages` beyond the 9-byte header split.

## STRIDE Threat Register

| Threat ID | Category | Component | Disposition | Mitigation Plan |
|-----------|----------|-----------|-------------|-----------------|
| T-P5-01 | Tampering / DoS | oversized length prefix → unbounded alloc | mitigate | `framing::MAX_FRAME_LEN` (16 MiB) guard rejects the frame before `messages` ever allocates the payload — already implemented + tested in `framing.rs`; `messages` adds no second guard (Task 1 documents/asserts this). |
| T-P5-02 | Tampering / DoS | malformed payload → panic | mitigate | All decode paths return `Result`; structured payloads via `postcard::from_bytes` (mapped to `MessageError::Decode`), never `unwrap` on wire data. Round-trip + malformed-postcard tests pin this (Task 1). |
| T-P5-03 | Tampering | unknown / forged tag → stream desync | mitigate | `Frame::decode` returns `Err(MessageError::UnknownTag(tag))` on any tag outside the 1–5 registry — error, never silent skip (locked decision 6); connection layer (deferred) decides renegotiate/disconnect. Unit-tested (Task 1). |
| T-P5-04 | Tampering | truncated `Video` header → OOB read | mitigate | Length check `payload.len() >= 9` before any slice/`try_into`; `MessageError::ShortVideoHeader` on failure. Unit-tested (Task 1). |
| T-P5-SC | Tampering (supply chain) | `cargo add` of `serde` + `postcard` | mitigate | slopcheck unavailable → blocking-human checkpoint (Task 0) before install; crates.io verification of publisher/repo/age/downloads on the correct registry; both kept `default-features = false` (no extra surface). Never auto-approved. |
</threat_model>

<verification>
## Cable-free (CI-green now — no cable, no Pixel, no transport)
- `cargo test -p protocol roundtrip` — every `Frame` variant encodes→decodes back to an equal value (Handshake, VideoConfig w/ real SPS-PPS, Video normal/empty/large, Touch, Control), incl. the multi-frame single-buffer stream.
- `cargo test -p protocol video_payload` — the raw `Video` payload byte layout `[pts u64 BE][keyframe u8][nal...]` with no serde framing.
- `cargo test -p protocol -- decode_rejects` — unknown tag, short Video header, malformed postcard → typed errors (no panic).
- `cargo test -p protocol negotiat` — the full negotiation matrix (version / codec / resolution / refresh-clamp / protocol_version() source-of-truth).
- `cargo test --workspace` — the existing 32 framing+nal+version tests stay green alongside the new `messages` tests.
- `cargo tree -p protocol` — shows exactly the two vetted-at-checkpoint deps (`serde`, `postcard`) + their transitive deps and nothing surprising; default build pulls only those.

Note (INFO 5): the filtered test-name tokens above (`roundtrip`, `video_payload`, `decode_rejects`, `negotiat`) each match ≥1 test by the naming convention pinned in Task 1/Task 2 behaviors, so no filtered `cargo test` can silently pass on zero matches.

## Deferred (cable / devices — NOT verified here, by necessity)
- Live send/recv over the wire, the live frame thread, the phone RX/decode loop → need P1 transport + cable + Pixel.
- `VideoConfig` actually sent on connect / on keyframe (the *act* of sending) → wire-blocked.
- Glass-to-glass latency < 50 ms + per-stage timings (PIPE-01 criteria #1, #2) → need Mac + Pixel + cable + camera.

## PIPE-01 criterion #3 → task map — see also P5-VALIDATION.md
| Criterion #3 clause | Cable-free now? | Closed by |
|---------------------|-----------------|-----------|
| "protocol frame codec round-trips (TDD-verified)" | yes | Task 1 (round-trip every variant + raw Video layout + malformed rejection) |
| "handshake/resolution negotiation succeeds" | yes | Task 2 (pure `negotiate()` matrix + version source-of-truth) |
| "`VideoConfig` (SPS/PPS) sent on connect and on each keyframe" | NO — the *act of sending* is wire-blocked (deferred). The `VideoConfig` *type* + its faithful SPS/PPS round-trip ARE delivered (Task 1). | deferred (live wiring) |
| criteria #1 (live desktop visible) & #2 (latency < 50 ms) | NO — hardware-blocked | deferred |
</verification>

<success_criteria>
- `crates/protocol/src/messages.rs` exists with `Frame` + tag constants 1–5 + `write_to`/`read_from`/`decode`/`to_tag_payload` + all message types (`VideoCodec = {H264, Hevc}`) + `MessageError`, and `negotiate()` + `AgreedConfig`/`NegotiationError`.
- Every `Frame` variant round-trips (TDD red→green→refactor): Handshake, VideoConfig (SPS/PPS faithful), Video (normal, empty `nal`, large `nal`), Touch, Control; plus a multi-frame stream decoded from one buffer.
- The `Video` payload is provably raw — a byte-exact test asserts `[pts_us u64 BE][keyframe u8][nal verbatim]` with no serde/postcard framing over the NAL.
- Malformed input is rejected with typed errors, never a panic or silent skip: unknown tag → `UnknownTag`, truncated Video header → `ShortVideoHeader`, bad postcard → `Decode`; oversized handled upstream by `framing::MAX_FRAME_LEN`.
- `negotiate()` is pure and passes the full matrix: happy path (host-preferred common codec + refresh clamped to client max), `VersionMismatch`, `NoCommonCodec`, `ResolutionUnsupported`, and a `protocol_version()` source-of-truth test; D6 geometry is never hard-coded.
- Task seam honored: after Task 1, NO `negotiate`/`AgreedConfig`/`NegotiationError` exist; Task 2 introduces them test-first — both TDD cycles have an honest red bar.
- Layering preserved: `messages` sits on `framing::{write_frame, read_frame}` (no second length-prefix layer); `framing.rs`, `nal.rs`, and `protocol_version()` are unmodified; `protocol_version()` (=1) is the handshake version source of truth.
- Deps: `serde` + `postcard` added with `default-features = false` (no_std-ready), approved through the Task 0 blocking-human checkpoint (Tasks 1/2 blocked until then); `cargo test --workspace` green (existing 32 tests + new ones).
- Branch hygiene: all code/docs are committed on `feat/p5-protocol-messages` only; NO merge to `main` without explicit user confirmation.
</success_criteria>

<output>
Create `.planning/phases/P5-live-end-to-end-pipeline-latency/P5-01-SUMMARY.md` when done.
Commit the module, `Cargo.toml`/`Cargo.lock` changes, the SUMMARY, and `P5-VALIDATION.md` on the `feat/p5-protocol-messages` branch. Do NOT merge to `main` without explicit user confirmation.
</output>
