# Phase P5 (cable-free slice): Protocol messages + handshake negotiation — Context

**Gathered:** 2026-06-03
**Status:** Ready for planning
**Source:** Orchestrator-synthesized from ROADMAP.md P5 + master roadmap §301 (Frame enum / wire format) + RESEARCH.md + the session constraint (no cable, no Pixel). Interactive discuss-phase skipped: the `Frame` enum is already locked by the roadmap, and the remaining field-set choices are taken as the researcher's recommended defaults (confirmable later — append-only protocol, unused, pre-merge).

**Requirement:** PIPE-01 (partial — only criterion #3's logic; criteria #1/#2 need the cable).

<domain>
## Phase Boundary

**Delivers (cable-free):** P5 success criterion #3's *logic* — a `protocol::messages` module with the `Frame` message enum, a codec that **round-trips** over the existing length-prefixed `framing`, and a pure **handshake/resolution-negotiation** function. All unit-testable on the dev host with no cable, no Pixel, no transport.

**Does NOT include (cable/device-blocked, deferred):** the live frame thread, real USB/socket transport (P1), runtime "send VideoConfig on connect/keyframe", the phone RX/decode loop (P4), glass-to-glass latency measurement (criteria #1/#2), backpressure/jitter, and `coords.rs`/touch (P6).
</domain>

<locked_decisions>
## Locked Decisions

1. **Frame enum (roadmap §301, locked):** `Handshake(Handshake)` · `VideoConfig { codec, sps_pps: Vec<u8> }` · `Video { pts_us: u64, keyframe: bool, nal: Vec<u8> }` · `Touch(TouchEvent)` · `Control(Control)`. Each variant gets an explicit `u8` tag constant (1–5).

2. **Layer on existing `framing`:** messages encode as `framing` frames — tag byte = message kind, payload = the variant's bytes. Reuse `framing::{write_frame, read_frame}`; do NOT reinvent length-prefixing. `MAX_FRAME_LEN` guard already protects decode allocations.

3. **Video payload is raw, NOT serde** (perf — no serde over big NAL buffers): `[pts_us u64 BE][keyframe u8][nal bytes verbatim]`. Control/Touch/Handshake/VideoConfig payloads use `postcard`.

4. **Deps (gated):** `serde = { version = "1", default-features = false, features = ["derive","alloc"] }` and `postcard = { version = "1", default-features = false, features = ["alloc"] }`, to keep `protocol` `no_std`-friendly (P7 purity). Both are `[ASSUMED]` (slopcheck unavailable) → the `cargo add` is gated behind one `checkpoint:human-verify` task.

5. **Negotiation is a pure function:** `negotiate(host: &Handshake, client: &ClientCaps) -> Result<AgreedConfig, NegotiationError>` — version-equality → host-preference-ordered codec intersection → resolution-fits → refresh clamped to client max. Typed errors (`VersionMismatch`, `NoCommonCodec`, `ResolutionUnsupported`). Fully table-testable.

6. **Robustness:** unknown tag on decode → **error, not skip** (single trusted peer); protocol-version check in handshake; malformed/truncated/oversized input rejected (no panic).

7. **Recommended field sets (researcher defaults, confirmable):** `Handshake { protocol_version: u32, width: u32, height: u32, refresh_hz: u32, codecs: Vec<VideoCodec> }`; `ClientCaps { protocol_version, max_width, max_height, max_refresh_hz, codecs }`; `Control { RequestKeyframe, Pause, Resume, Bye }`; `TouchEvent { pointer_id, phase: Down/Move/Up, nx: f32, ny: f32 }` (matches P6 §340); `VideoCodec { H264, Hevc }` (HEVC reserved, H264 only used now per D2).

8. **TDD** for all logic (round-trip every Frame variant, negotiation matrix, malformed/unknown-tag rejection). Reuse `protocol::protocol_version()` (=1) as the handshake version source of truth.
</locked_decisions>

<existing_work>
## Builds on (already in tree, do not duplicate)
- `crates/protocol/src/framing.rs` — `write_frame`/`read_frame`, `MAX_FRAME_LEN`. The message codec sits directly on this.
- `crates/protocol/src/nal.rs` — `CodecConfig` (SPS/PPS) feeds `VideoConfig`.
- `crates/protocol/src/lib.rs` — `protocol_version()`.
</existing_work>

<scope_split>
## Cable-free (this slice) vs deferred
- **Cable-free / CI now:** `messages.rs` (types + tag constants + encode/decode codec on `framing` + `negotiate()`), full TDD, the gated `Cargo.toml` dep add. Gate: `cargo test -p protocol` green; default build pulls only the two vetted-at-checkpoint deps.
- **Deferred (cable/devices):** live transport, send-on-connect/keyframe wiring, decode loop, latency harness, touch/coords.
</scope_split>

<discretion>
## Claude's Discretion
- Exact encode/decode API shape (e.g. `Frame::encode(&self, w: &mut dyn Write)` + `Frame::read(r: &mut dyn Read) -> io::Result<Frame>`, or free functions).
- Internal module organization within `messages.rs`.
- Whether `AgreedConfig`/`ClientCaps` live in `messages` or a submodule.
</discretion>
