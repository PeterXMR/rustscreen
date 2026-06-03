# Phase P5: Live End-to-End Pipeline + Latency — Research (protocol layer, cable-free slice)

**Researched:** 2026-06-03
**Domain:** Binary wire protocol design in Rust — message enum + length-prefixed codec + handshake/resolution negotiation as pure, unit-testable functions (`postcard`/`serde`)
**Confidence:** HIGH (design layers on already-shipped `framing.rs`/`nal.rs`; `postcard`/`serde` versions verified against the registry; the only LOW items are field-set choices the user should confirm)

## Summary

P5's success criterion #3 has two halves: a *logic* half that is fully implementable and TDD-verifiable on a dev host with no cable, no Pixel, and no live transport — and a *wiring* half (live frame thread, real socket/USB send, glass-to-glass latency) that is hardware-blocked. This research scopes **only the logic half**: the `Frame` message enum and a codec that round-trips every variant, plus the handshake / resolution-negotiation decision logic as pure functions. Everything that needs a wire, a phone, or a stopwatch is explicitly deferred (see the In-Scope vs Deferred section).

The design the roadmap specifies is sound and should be implemented as written, with concrete field sets filled in. The new `messages.rs` layers directly on the existing `framing::{write_frame, read_frame}` — the framing module already moves opaque `(u8 tag, payload)` frames with `MAX_FRAME_LEN` guarding against hostile lengths, so `messages.rs` only has to (a) assign a tag per `Frame` variant, (b) encode the payload — `postcard` for the small structured variants, **raw bytes for `Video`** to keep the hot path off serde — and (c) decode by dispatching on the tag. The `Video` payload is framed as a tiny fixed header (`pts_us: u64 BE` + `keyframe: u8`) followed by the NAL bytes verbatim, so no serde or copy touches the large buffer.

For negotiation, the cleanest testable shape is a single pure function `negotiate(host: &HostOffer, client: &ClientCaps) -> Result<AgreedConfig, NegotiationError>` with no I/O — the transport later just serializes the offer/caps as `Frame::Handshake` payloads and feeds the decoded structs into this function. This makes the entire negotiation matrix (version mismatch, resolution agreement, codec intersection, refresh clamping) a table-driven unit test.

**Primary recommendation:** Add `serde` (derive, `default-features = false`) + `postcard` (`alloc` feature) to the `protocol` crate; implement `Frame` with explicit per-variant tag constants layered on `framing`; encode `Video` with a raw 9-byte header + verbatim NAL (no serde); implement `negotiate()` as a pure function; TDD round-trip for every variant + a negotiation matrix + malformed/unknown-tag rejection. Defer all live-wire and latency work to the hardware-unblocked continuation of P5.

## User Constraints

> No CONTEXT.md exists for this phase yet (this is standalone research via `--research-phase P5`). The constraints below are the locked decisions from the authoritative roadmap (`docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md` §1, P5 §279-317) and standing project preferences from memory. A later discuss-phase may add a CONTEXT.md that supersedes these.

### Locked Decisions (roadmap §1 + P5 spec)
- **Wire format (LOCKED, roadmap §301):** `u8 tag · u32 BE len · payload`. **Already implemented** in `framing.rs` (verified: tests pin `[tag][be u32 len][payload]`). Do not redesign framing.
- **`Frame` enum shape (LOCKED, roadmap §301):** `enum Frame { Handshake{..}, VideoConfig{ codec, sps_pps: Vec<u8> }, Video{ pts_us:u64, keyframe:bool, nal:Vec<u8> }, Touch(TouchEvent), Control(Control) }`.
- **Encoding split (LOCKED, roadmap §301):** Control/Touch/Handshake payloads via `postcard`; **`Video` payload written raw** (no serde over big buffers — perf).
- **`postcard` as the control-message serializer (LOCKED, roadmap Tech Stack §9).**
- **D2 — Codec = H.264 for MVP (LOCKED).** `VideoCodec` enum still carries an HEVC variant for forward-compat negotiation, but H.264 is the only value exercised.
- **D6 — Display geometry default 2400×1080 @ 60 Hz** (Pixel 6a native landscape). These are the default `HostOffer` values; negotiation must not hard-code them.
- **Protocol = pure Rust, `no_std`-friendly, no platform deps (LOCKED, roadmap §107).** Constrains dep features (see Standard Stack).
- **D0 — simplest-now behind seams.** `protocol` is already the shared seam; this work stays inside it.
- **Project memory — "simplest working solution behind seams, swap later."** Favor the smallest correct design; do not over-engineer the negotiation/versioning.

### Claude's Discretion (within the locked design)
- Exact field sets for `Handshake` and `Control` (recommended below — flagged `[ASSUMED]`, needs user confirmation).
- The precise encode/decode API surface (`Frame::encode(&self, w)` / `Frame::decode(tag, payload)` vs `write_to`/`read_from`) — recommendation below.
- Tag-byte assignment values (recommended below).
- `TouchEvent` field set (a P6 type, but it must be *namable and serializable* now so `Frame::Touch` compiles; recommend the minimal forward-compatible shape).
- Unknown-tag handling policy (error vs skip) — recommendation below.

### Deferred Ideas (OUT OF SCOPE for this research / cable-blocked)
- The live frame thread on the Mac (`capture → encode → framing → transport.tx`).
- The phone RX→deframe→decode loop in `android_main`.
- Actual socket/USB transport (`nusb`, accessory fd) — that is P1's deliverable, still hardware-blocked.
- Sending `VideoConfig` "on connect / on keyframe" at runtime (the *policy logic* of when to resend can be a pure helper if desired, but the *act of sending* is wire-blocked).
- Glass-to-glass latency harness, QR/timer photograph, per-stage timing (criteria #1 and #2).
- Jitter buffer / backpressure / drop-to-keyframe (roadmap P5 "Risk" — YAGNI until measured).

## Phase Requirements

| ID | Description | Research Support (cable-free slice only) |
|----|-------------|-------------------------------------------|
| PIPE-01 (partial) | "...protocol framing (length-prefixed, TDD), handshake/resolution negotiation... all work..." | This research covers the **TDD-able protocol codec** (`Frame` enum + per-variant round-trip on top of `framing`) and the **handshake/resolution negotiation** as a pure `negotiate()` function. The remainder of PIPE-01 — live extended desktop visible, glass-to-glass < 50 ms, live frame thread, VideoConfig sent on connect/keyframe, per-stage timings — is hardware-blocked and deferred. |

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Define `Frame` / `Handshake` / `Control` / `TouchEvent` / `VideoCodec` types | `protocol` crate (`messages.rs`) | — | Shared, platform-agnostic; both host and client depend on identical wire types. |
| Encode a `Frame` to a writer | `protocol::messages` over `protocol::framing::write_frame` | — | Tag+payload framing already owned by `framing`; `messages` only chooses tag + payload bytes. |
| Decode a `Frame` from a reader | `protocol::messages` over `protocol::framing::read_frame` | — | `framing` returns `(tag, payload)`; `messages` dispatches on tag. |
| Serialize structured payloads (Handshake/Control/Touch) | `postcard` (in `messages.rs`) | — | Locked serializer; compact, `no_std`/`alloc`-friendly, serde-derive. |
| Carry the `Video` NAL payload without serde/copy | `messages.rs` raw header + verbatim slice | — | Perf: never run serde over a multi-hundred-KB keyframe (locked §301). |
| Negotiate agreed mode from host offer + client caps | `protocol` pure fn `negotiate()` | — | No I/O → fully unit-testable; transport just supplies decoded structs. |
| Bound payload size / reject hostile lengths | `protocol::framing` (`MAX_FRAME_LEN`) | `messages` decode (unknown tag) | **Already implemented + tested** in `framing.rs`. `messages` adds tag validation. |
| Live send/recv, latency, frame thread | **DEFERRED — host/client transport tiers, hardware-blocked** | — | Out of scope for the cable-free slice. |

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `serde` | `1.0.228` | `Serialize`/`Deserialize` derives for the structured `Frame` payloads | The de-facto Rust serialization framework; `postcard` is built on it. `[VERIFIED: cargo search / registry index]` name+existence; `[ASSUMED]` exact patch (registry not slopcheck-verified). |
| `postcard` | `1.1.3` | Compact binary encoding of Handshake/Control/Touch payloads | Locked by roadmap; the standard `no_std`+serde wire format (Rust Embedded WG lineage); stable 1.x. `[VERIFIED: cargo search / registry index]` name+existence+version. |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| (none required) | — | — | The `Video` raw header uses only `u64::to_be_bytes` / slices from `core`/`std` — no extra dep. `framing` already pulls `std::io`. |
| `heapless` | `0.9.3` | Fixed-capacity `Vec` for true `no_std`-no-alloc encode | Only if the crate must compile with **no allocator at all**. Not needed today — `protocol` already uses `std` (`framing.rs` imports `std::io`), so prefer postcard's `alloc` path. Listed for the P7/P8 purity note. `[ASSUMED]` |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| `postcard` | `bincode` 2.x | bincode is also serde-based but the roadmap locked `postcard`; postcard is more `no_std`-friendly and produces smaller varint-encoded integers. No reason to deviate. |
| `postcard` for `Video` | postcard over the whole `Frame` | Rejected (locked): serializing a `Vec<u8>` NAL through serde adds a length prefix + a full copy on the hot path. Raw header + verbatim slice is the point of the design. |
| serde derive | hand-written `to_be_bytes` for every struct | More code, more bug surface, no `no_std` benefit over postcard+serde. Use derive. |

**Installation (`crates/protocol/Cargo.toml`):**
```toml
[dependencies]
serde = { version = "1.0.228", default-features = false, features = ["derive", "alloc"] }
postcard = { version = "1.1.3", default-features = false, features = ["alloc"] }
```

**Why these features (verified against postcard 1.1.x docs):**
- `serde` `default-features = false` drops serde's own `std` so the *types* stay `no_std`-compatible (roadmap §107 "no_std-friendly"); `features = ["derive"]` gives `#[derive(Serialize, Deserialize)]`; `features = ["alloc"]` lets serde derive `Vec<u8>`/`String` impls without `std`.
- `postcard` `features = ["alloc"]` enables `postcard::to_allocvec()` (returns `alloc::vec::Vec<u8>`) and `postcard::from_bytes::<T>(&[u8])` — the exact pair `messages.rs` needs, without dragging in `use-std`. `[CITED: docs.rs/postcard/1.1.3]`
- **Note on `no_std` reality:** `framing.rs` currently imports `std::io::{Read, Write}`, so the crate is *not* `no_std` today. The dep features above keep the *new message types* `no_std`-ready (a P7/P8 purity win) without forcing a `framing` rewrite now. This is the simplest-now choice — do not refactor `framing` to `no_std` in P5. `[ASSUMED]` that preserving no_std-readiness is worth the `default-features = false` ceremony; confirm with user (roadmap says "no_std-friendly" but the crate already uses std).

**Version verification performed:** `cargo search postcard` → `postcard = "1.1.3"`; `cargo search serde` → `serde = "1.0.228"`; toolchain `rustc 1.95.0` / `cargo 1.95.0`; edition `2021` (matches existing `protocol/Cargo.toml`). The crates.io HTTP API was unreachable from the sandbox, so download-count/repo confirmation came from the registry index via `cargo search` rather than the web API.

## Package Legitimacy Audit

> slopcheck was **not available** in this environment (`pip install slopcheck` not run / binary absent). Per protocol, packages are therefore tagged `[ASSUMED]` for the planner to gate. Both are foundational, universally-used Rust crates (serde underpins the entire Rust serialization ecosystem; postcard is the locked roadmap choice and the standard no_std serde format), so the practical risk is negligible — but the formal tag stands.

| Package | Registry | Age | Downloads | Source Repo | slopcheck | Disposition |
|---------|----------|-----|-----------|-------------|-----------|-------------|
| `serde` | crates.io | ~9 yrs | billions (ecosystem standard) | github.com/serde-rs/serde | unavailable | Approved — `[ASSUMED]`, planner may add a `checkpoint:human-verify` |
| `postcard` | crates.io | ~6 yrs | very high | github.com/jamesmunns/postcard | unavailable | Approved — `[ASSUMED]`, locked by roadmap |

**Packages removed due to slopcheck [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** none
**Ecosystem-confusion check:** both verified to exist on **crates.io** (the correct Rust registry) via `cargo search`, not merely assumed from another ecosystem.

*Because slopcheck was unavailable, the planner should gate the `Cargo.toml` dependency-add behind a single `checkpoint:human-verify` task confirming the two crate names + versions before `cargo build`.*

## Architecture Patterns

### System Architecture Diagram (the cable-free protocol slice)

```
                  ┌──────────────────────── protocol crate (pure Rust) ───────────────────────┐
  in-scope here → │                                                                            │
                  │   Frame (enum)                                                             │
                  │     ├─ Handshake(Handshake)  ┐                                             │
                  │     ├─ VideoConfig{codec,..} ├─► postcard::to_allocvec ─┐                  │
                  │     ├─ Touch(TouchEvent)     │                          │  payload bytes   │
                  │     ├─ Control(Control)      ┘                          ▼                  │
                  │     └─ Video{pts_us,kf,nal} ─► raw 9-byte hdr + nal ────► framing::write_frame(w, TAG, payload)
                  │                                  (NO serde, NO copy)        │              │
                  │                                                             ▼              │
                  │                                                   [tag][u32 BE len][payload]  ── bytes ──►  (transport, DEFERRED)
                  │                                                                            │
   decode path:   │   bytes ──► framing::read_frame(r) ─► (tag, payload) ─► Frame::decode(tag, payload)         │
                  │                                                  │ match tag:                               │
                  │                                                  ├─ structured → postcard::from_bytes       │
                  │                                                  ├─ Video      → split hdr / borrow nal     │
                  │                                                  └─ unknown    → Err(UnknownTag)            │
                  │                                                                            │
                  │   negotiate(host: &HostOffer, client: &ClientCaps)                         │
                  │       ─► Result<AgreedConfig, NegotiationError>   (pure fn, no I/O)         │
                  │          version check ▸ codec intersect ▸ resolution agree ▸ refresh clamp │
                  └────────────────────────────────────────────────────────────────────────────┘
```
Trace the primary use case: a `Frame` is encoded → framed by the existing `framing` → handed to a writer (the writer itself, and the live thread that produces frames, are deferred). On the other side a reader yields `(tag, payload)` which `Frame::decode` turns back into a `Frame`. Negotiation is a side function consuming the *decoded* `Handshake` payloads.

### Recommended file layout
```
crates/protocol/src/
├── lib.rs        # add `pub mod messages;`  (framing, nal already present)
├── framing.rs    # DONE — write_frame/read_frame, MAX_FRAME_LEN
├── nal.rs        # DONE — CodecConfig, NAL iteration (Video payloads reuse this)
└── messages.rs   # NEW — Frame, Handshake, Control, TouchEvent, VideoCodec,
                  #       tag constants, encode/decode, negotiate(), errors
```
(`coords.rs` from roadmap §114 is a P6 deliverable — out of scope here.)

### Pattern 1: Tag assignment + dispatch over the existing framing
**What:** Each `Frame` variant owns a stable `u8` tag constant. `encode` picks the tag and builds the payload; `decode(tag, payload)` matches on the tag. Tags are an explicit, append-only registry (never renumber — forward/back compat).
**When to use:** Always — this is the spine of the codec.
**Example:**
```rust
// Source: layered on existing crates/protocol/src/framing.rs (verified in-repo)
mod tag {
    pub const HANDSHAKE: u8    = 1;
    pub const VIDEO_CONFIG: u8 = 2;
    pub const VIDEO: u8        = 3;
    pub const TOUCH: u8        = 4;
    pub const CONTROL: u8      = 5;
    // Append new kinds with the next integer. Never reuse/renumber.
}
```

### Pattern 2: `encode` / `decode` API shape (recommended)
**What:** Two ergonomic methods that delegate to `framing`. Keep the symmetry with `framing`'s `write_frame`/`read_frame` but expose a `Frame`-level surface.
**Recommendation:** Provide BOTH a streaming pair and a buffer pair, because the roadmap's failing test (`write_to`/`read_from` returning `(Frame, consumed)`) and the live transport want different shapes:
```rust
// Source: design recommendation; matches roadmap §289 test signature
impl Frame {
    /// Encode self as one framed message into `w` (delegates to framing::write_frame).
    pub fn write_to(&self, w: &mut dyn std::io::Write) -> Result<(), MessageError> { /* ... */ }

    /// Read one framed message from `r` and decode it.
    pub fn read_from(r: &mut dyn std::io::Read) -> Result<Frame, MessageError> { /* ... */ }

    /// Pure tag+payload decode (no I/O) — the unit-test seam.
    pub fn decode(tag: u8, payload: &[u8]) -> Result<Frame, MessageError> { /* match tag */ }

    /// Pure encode to (tag, payload_bytes) — the other unit-test seam.
    fn to_tag_payload(&self) -> Result<(u8, Vec<u8>), MessageError> { /* ... */ }
}
```
Keeping `decode(tag, payload)` and `to_tag_payload()` as pure (no-I/O) functions is what makes the round-trip test trivial and cable-free — you can assert on bytes without any `Read`/`Write`. The roadmap's example test uses `write_to`/`read_from` returning `(decoded, consumed)`; note that `framing::read_frame` consumes exactly one frame from a `Read`, so a `(Frame, usize)` "consumed" count is naturally available if you decode from a slice via a `Cursor` — recommend matching the roadmap test signature for `read_from` on a slice.

### Pattern 3: Zero-serde `Video` payload
**What:** The `Video` payload is `[pts_us: u64 BE (8)] [keyframe: u8 (1)] [nal bytes...]`. Encode appends the 9-byte header then `extend_from_slice(nal)`. Decode splits the first 9 bytes, borrows/copies the rest.
**When to use:** The `Video` variant only.
**Example:**
```rust
// Source: design recommendation (implements locked roadmap §301 "Video written raw")
// encode:
let mut payload = Vec::with_capacity(9 + nal.len());
payload.extend_from_slice(&pts_us.to_be_bytes());      // 8 bytes
payload.push(keyframe as u8);                            // 1 byte
payload.extend_from_slice(nal);                          // verbatim, no serde
// decode:
if payload.len() < 9 { return Err(MessageError::ShortVideoHeader); }
let pts_us = u64::from_be_bytes(payload[0..8].try_into().unwrap());
let keyframe = payload[8] != 0;
let nal = payload[9..].to_vec();
```
(One `Vec` allocation + one `memcpy` of the NAL is unavoidable when copying into an owned `Frame`; to go truly zero-copy a borrowed `Frame<'a>` variant could hold `&'a [u8]`, but that complicates the enum — recommend the owned form for the MVP per "simplest-now," and note the borrowed variant as a P7 perf option.)

### Anti-Patterns to Avoid
- **Running `postcard` over the `Video` NAL:** doubles the copy and adds a varint length prefix on the hottest path — explicitly forbidden by §301.
- **Renumbering tags or `#[repr]`-ordering the enum for the wire:** the wire tag must be an explicit constant, decoupled from Rust enum discriminant order, so reordering variants in source can't silently change the protocol.
- **`postcard::to_stdvec` / `use-std`:** pulls in `std`, defeating the `no_std`-ready intent. Use `to_allocvec` with the `alloc` feature.
- **Baking D6 (2400×1080@60) into `negotiate()`:** negotiation must read the offered values; hard-coding them makes the function untestable for other geometries and breaks P7 rotation renegotiation.
- **Panicking on malformed input:** decode of attacker-controlled bytes must return `Err`, never `unwrap`/panic (the `try_into` above is on a length-checked slice, so it's safe).

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Serializing structured messages | Manual `to_be_bytes` + field-by-field readers for Handshake/Control/Touch | `serde` derive + `postcard` | Hand-rolled parsers are where off-by-one and malformed-input bugs live; postcard handles varints, options, enums correctly. |
| Length-prefixed framing | A second framing layer in `messages` | existing `framing::{write_frame,read_frame}` | Already implemented, tested (8 tests), and guards `MAX_FRAME_LEN`. `messages` only adds the tag/payload mapping. |
| NAL splitting / SPS-PPS extraction for `VideoConfig` | New Annex-B parser | existing `nal::{iter_nal_units, extract_codec_config, is_keyframe}` | Already implemented + tested (24 tests). `VideoConfig.sps_pps` and the `Video.keyframe` flag can be derived from it. |
| Bounded allocation on decode | New size guard | `framing::MAX_FRAME_LEN` (16 MiB) | The framing layer already rejects oversized/short frames before `messages` ever sees the payload. |

**Key insight:** P5's protocol layer is almost entirely *composition* of already-tested pieces (`framing` + `nal`) plus `postcard`. The genuinely new logic is small: tag dispatch, the `Video` raw header, the message types, and `negotiate()`. Keep it that small.

## Recommended concrete field sets (research question 1)

> All field sets below are `[ASSUMED]` design recommendations — they are not in the roadmap verbatim and **need user confirmation in discuss-phase** before they become locked. They are chosen to be minimal, forward-compatible, and sufficient for D6.

```rust
// Source: design recommendation (postcard-serialized; all #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)])

/// Codec identity negotiated end-to-end. H.264 only for MVP (D2); Hevc reserved for P7 (variant spelling locked by CONTEXT decision 7).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec { H264, Hevc }

/// Sent by the Mac on connect (the host *offer*) AND used as the reply container.
/// Field set = everything the client needs to size its decoder + window.
pub struct Handshake {
    pub protocol_version: u32,   // must match protocol_version() (lib.rs -> 1) — see Pattern 4
    pub width: u32,              // D6 default 2400
    pub height: u32,             // D6 default 1080
    pub refresh_hz: u32,         // D6 default 60
    pub codecs: Vec<VideoCodec>, // host's *supported* set, preference-ordered (MVP: [H264])
    // pixel format intentionally OMITTED: H.264 output is Annex-B NAL; the decoder's
    // surface format (NV12) is negotiated by MediaCodec/the surface, not our protocol.
    // Add a `PixelFormat` field only if a raw/uncompressed path is ever introduced.
}

/// What the client (Pixel) reports back so the host can finalize the mode.
pub struct ClientCaps {
    pub protocol_version: u32,
    pub max_width: u32,          // panel/decoder cap (Pixel 6a: 2400)
    pub max_height: u32,         // (1080)
    pub max_refresh_hz: u32,     // (60)
    pub codecs: Vec<VideoCodec>, // client's supported decoders (MVP: [H264])
}

/// Control back-channel — keep tiny and append-only.
pub enum Control {
    RequestKeyframe,             // client lost sync / first connect → host forces an IDR + resends VideoConfig
    Pause,                       // client backgrounded
    Resume,
    Bye,                         // graceful disconnect (clean teardown, P7 hotplug)
    // (Reserved for P7: SetBitrate(u32), Rotated{ width, height })
}

/// Touch is a P6 deliverable, but the type must exist now so Frame::Touch compiles.
/// Minimal forward-compatible shape (normalized coords + phase), matches roadmap P6.2.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct TouchEvent {
    pub pointer_id: u32,
    pub phase: TouchPhase,       // Down/Move/Up
    pub nx: f32,                 // normalized [0,1]
    pub ny: f32,                 // normalized [0,1]
}
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPhase { Down, Move, Up }
```
Rationale for the choices:
- **`Handshake` carries version + geometry + codec set** because those are exactly the values the client needs to size its `AMediaCodec` + `ANativeWindow` (P4) and what negotiation operates on. `refresh_hz` is included for completeness/logging even though MediaCodec doesn't strictly need it.
- **Pixel format is deliberately excluded** — the only thing on the wire is compressed Annex-B; surface/sample format is a decoder concern, not a protocol concern. Including it would invite a field that's never meaningfully negotiated. (Flag for user: if they anticipate a future raw path, add it now.)
- **`Control` is a tiny append-only enum.** `RequestKeyframe` is the one the live pipeline genuinely needs (drives the "resend VideoConfig on keyframe" policy). The rest support P7 hotplug/UX and cost nothing to reserve.
- **`TouchEvent` uses normalized `f32` coords** so the host maps them via P6's `coords.rs` — this matches roadmap §340 exactly and keeps the touch type stable across the wire regardless of either side's resolution.

## Handshake / resolution negotiation (research question 4)

**Recommended pure function:**
```rust
// Source: design recommendation — no I/O, fully unit-testable
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgreedConfig {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub codec: VideoCodec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NegotiationError {
    VersionMismatch { host: u32, client: u32 },
    NoCommonCodec,
    ResolutionUnsupported { offered: (u32, u32), client_max: (u32, u32) },
}

/// Pure negotiation: host proposes a mode + codec preference; client reports caps.
/// Returns the agreed mode or a typed error. NO sockets, NO global state.
pub fn negotiate(host: &Handshake, client: &ClientCaps) -> Result<AgreedConfig, NegotiationError> {
    if host.protocol_version != client.protocol_version {
        return Err(NegotiationError::VersionMismatch {
            host: host.protocol_version, client: client.protocol_version,
        });
    }
    // First host-preferred codec the client also supports.
    let codec = host.codecs.iter().copied()
        .find(|c| client.codecs.contains(c))
        .ok_or(NegotiationError::NoCommonCodec)?;
    // Resolution: host's offer must fit the client's caps; clamp refresh down.
    if host.width > client.max_width || host.height > client.max_height {
        return Err(NegotiationError::ResolutionUnsupported {
            offered: (host.width, host.height),
            client_max: (client.max_width, client.max_height),
        });
    }
    Ok(AgreedConfig {
        width: host.width,
        height: host.height,
        refresh_hz: host.refresh_hz.min(client.max_refresh_hz), // clamp, never exceed panel
        codec,
    })
}
```
Why this shape:
- **Single pure function, typed error** → the entire negotiation matrix is a table test (version equal/unequal × codec intersect empty/nonempty × resolution fits/overflows × refresh clamp). No transport needed.
- **Codec intersection is host-preference-ordered** so a future HEVC toggle (P7) "just works": host offers `[Hevc, H264]`, a Pixel that supports both gets HEVC, an older client gets H.264.
- **Refresh is clamped, not rejected** (60→60 for D6; a 90 Hz panel still gets 60) — matches "extend at 60 Hz" while tolerating better clients.
- **Resolution is reject-on-overflow** for the MVP (simplest-now). A downscale-negotiation is a deliberate non-goal; flag as a P7 option if the user wants it.

`[ASSUMED]`: clamp-refresh / reject-resolution policy is a design choice, not in the roadmap — confirm in discuss-phase. The reverse (host adapts to client's max resolution) is equally valid and arguably better UX; left as an open question.

## Versioning / robustness (research question 5)

### Pattern 4: Protocol version check
`lib.rs` already exposes `protocol_version() -> u32` (= 1, verified). `Handshake.protocol_version` is set to that on send and compared in `negotiate()` (`VersionMismatch`). Keep it a single `u32`; bump on any wire-incompatible change. No semver range logic for the MVP (YAGNI).

### Unknown-tag handling — recommendation: **error, do not skip**
`Frame::decode(tag, payload)` returns `Err(MessageError::UnknownTag(tag))` for any tag outside the registry. Rationale: this is a single trusted USB peer, not a multi-party broadcast; an unknown tag means a version/impl mismatch or corruption, and silently skipping it would desync the stream worse than failing fast. (Framing already delimits frames, so the *reader* can in principle resync by reading the next frame — but the safe MVP behavior is to surface the error to the connection-management layer, which can then renegotiate or disconnect.) `[ASSUMED]` — confirm error-vs-skip with user; error is the safer default.

### Bounded / malformed input
- **Oversized payload:** already rejected by `framing` (`MAX_FRAME_LEN` 16 MiB; tested). 16 MiB comfortably exceeds a 1080p H.264 keyframe.
- **Truncated frame:** already rejected by `framing::read_frame` (`UnexpectedEof`; tested).
- **Short `Video` header (<9 bytes):** new `MessageError::ShortVideoHeader`.
- **Malformed postcard payload:** `postcard::from_bytes` returns `Err`; map to `MessageError::Decode`.
- **Empty / nonsense payload for a known tag:** surfaced as a `Decode` error from postcard or the explicit length check.

### Threat notes (STRIDE)
| Pattern | STRIDE | Mitigation |
|---------|--------|------------|
| Oversized length prefix → unbounded alloc | Denial of Service | `MAX_FRAME_LEN` guard in `framing` (done, tested). |
| Malformed postcard bytes → panic | DoS / Tampering | Decode returns `Result`; never `unwrap` on wire data. Round-trip + fuzz-style malformed tests pin this. |
| Unknown/forged tag → desync | Tampering | `decode` errors on unknown tag; connection layer decides. |
| Truncated `Video` header → OOB read | Tampering | Length check before `try_into`. |
The transport is a single physical USB peer (not network-exposed), so confidentiality/auth threats are out of scope for the MVP — note for P7/P8 if a TCP-over-USB (NCM) path is chosen in P1, since that *is* network-reachable.

## Code Examples

### Round-trip test for every variant (the cable-free verification)
```rust
// Source: design recommendation; extends roadmap §289 example
#[test]
fn roundtrip_video_frame() {
    let f = Frame::Video { pts_us: 123, keyframe: true, nal: vec![0,1,2,3] };
    let mut buf = Vec::new();
    f.write_to(&mut buf).unwrap();
    let mut cur = std::io::Cursor::new(&buf);
    assert_eq!(Frame::read_from(&mut cur).unwrap(), f);
}

#[test]
fn roundtrip_handshake() {
    let f = Frame::Handshake(Handshake {
        protocol_version: protocol_version(),
        width: 2400, height: 1080, refresh_hz: 60,
        codecs: vec![VideoCodec::H264],
    });
    let mut buf = Vec::new();
    f.write_to(&mut buf).unwrap();
    assert_eq!(Frame::read_from(&mut std::io::Cursor::new(&buf)).unwrap(), f);
}

#[test]
fn decode_rejects_unknown_tag() {
    assert!(matches!(Frame::decode(99, &[]), Err(MessageError::UnknownTag(99))));
}

#[test]
fn decode_rejects_short_video_header() {
    assert!(matches!(Frame::decode(tag::VIDEO, &[0,0,0]), Err(MessageError::ShortVideoHeader)));
}
```

### Negotiation matrix test
```rust
// Source: design recommendation
#[test]
fn negotiates_d6_default() {
    let host = Handshake { protocol_version: 1, width: 2400, height: 1080, refresh_hz: 60, codecs: vec![VideoCodec::H264] };
    let client = ClientCaps { protocol_version: 1, max_width: 2400, max_height: 1080, max_refresh_hz: 60, codecs: vec![VideoCodec::H264] };
    assert_eq!(negotiate(&host, &client).unwrap(),
        AgreedConfig { width: 2400, height: 1080, refresh_hz: 60, codec: VideoCodec::H264 });
}
#[test]
fn rejects_version_mismatch() {
    let host = Handshake { protocol_version: 2, ../* as above */ };
    /* assert VersionMismatch */
}
#[test]
fn clamps_refresh_to_client_max() { /* host 60, client max 90 -> 60 ; host 60 client 30 -> 30 */ }
#[test]
fn errors_when_no_common_codec() { /* host [Hevc], client [H264] -> NoCommonCodec */ }
#[test]
fn errors_when_resolution_exceeds_client() { /* host 3840x2160, client max 2400x1080 */ }
```

## Verification Strategy (research question 6)

| What | Automatable now (CI, no cable)? | How |
|------|---------------------------------|-----|
| Round-trip encode/decode of **every** `Frame` variant | ✅ Yes | `cargo test -p protocol` — inline `#[cfg(test)]`, byte-exact asserts (Pattern 1–3). |
| `Video` raw-header correctness (pts/keyframe/nal split) | ✅ Yes | Encode then byte-inspect the payload + decode-equality. |
| Negotiation matrix (version/codec/resolution/refresh) | ✅ Yes | Table test on the pure `negotiate()`. |
| Malformed input rejection (unknown tag, short header, bad postcard, oversized via framing) | ✅ Yes | Error-variant asserts; reuse `framing`'s existing oversized/truncated tests. |
| `VideoConfig` carries SPS/PPS faithfully | ✅ Yes | Round-trip a `VideoConfig` whose `sps_pps` came from `nal::extract_codec_config`. |
| Live send/recv over the wire | ❌ No — **DEFERRED** | Needs P1 transport + cable. |
| `VideoConfig` actually sent on connect / on keyframe | ❌ No (act of sending) — DEFERRED | The *policy helper* (`should_resend_config(is_keyframe, first_frame) -> bool`) COULD be a pure, testable fn now; the send is wire-blocked. |
| Glass-to-glass latency < 50 ms, per-stage timings | ❌ No — DEFERRED | Needs Mac + Pixel + cable + camera (criteria #1, #2). |

**CI gate for this slice:** `cargo test -p protocol` green (existing 32 tests + new `messages` tests). No new CI infrastructure needed — `protocol` is already pure-Rust and runs on every runner.

## In-Scope vs Deferred (the explicit split this research was asked for)

### IN SCOPE — implementable + unit-testable now, no cable/device
1. `messages.rs`: the `Frame` enum + `Handshake`, `ClientCaps`, `Control`, `TouchEvent`, `TouchPhase`, `VideoCodec`, `AgreedConfig`, `NegotiationError`, `MessageError` types.
2. `Frame` encode/decode layered on `framing` (tag registry + postcard for structured payloads + raw header for `Video`).
3. `negotiate()` pure function.
4. Version check + unknown-tag/malformed-input rejection.
5. TDD: round-trip every variant, negotiation matrix, malformed-input tests.
6. `Cargo.toml` dep add (`serde` + `postcard` with the features above).

### OUT OF SCOPE — deferred until the cable/devices are available (continuation of P5)
1. The live frame thread on the Mac (`capture → encode → framing → transport.tx`).
2. The phone RX→deframe→decode loop in `android_main`.
3. The actual socket/USB transport (P1 deliverable, hardware-blocked).
4. Runtime sending of `VideoConfig` on connect / on keyframe (the *act*; the policy predicate could optionally be a pure helper).
5. Glass-to-glass latency harness + per-stage timing (criteria #1, #2).
6. Jitter buffer / backpressure / drop-to-keyframe.
7. `coords.rs` and touch *capture/injection* (P6).

## State of the Art

| Old Approach | Current Approach | Impact |
|--------------|------------------|--------|
| Hand-rolled binary protocols with manual byte cursors | serde-derive + `postcard` for structured msgs; raw bytes only for the bulk payload | Less parser-bug surface; the bulk path stays copy-minimal. This is exactly the locked §301 design. |
| `bincode` for Rust binary serde | `postcard` for `no_std`/embedded-adjacent | postcard is smaller (varints) and `no_std`-first; roadmap locked it. |

**Deprecated/outdated:** none relevant — `serde` 1.x and `postcard` 1.x are both current, stable major versions.

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `Handshake`/`ClientCaps`/`Control`/`TouchEvent` field sets as proposed | Field sets | Low — types are internal to the protocol crate; changing a field is a localized edit + test update. Confirm in discuss-phase. |
| A2 | Pixel format does NOT belong in `Handshake` (compressed-only wire) | Field sets | Medium — if a raw/uncompressed path is ever wanted, the field must be added early to avoid a wire bump. Confirm intent with user. |
| A3 | Negotiation policy: clamp refresh, reject (not downscale) over-large resolution | Negotiation | Medium — reject vs downscale is a real UX choice; host-adapts-to-client may be preferable. Open question. |
| A4 | Unknown tag → error (not skip) | Robustness | Low-Medium — error is the safer default for a single trusted peer; skip could be chosen for forward-compat tolerance. Confirm. |
| A5 | Tag values 1–5 as listed | Pattern 1 | Low — arbitrary but must be stable once chosen; pin before any wire is exercised. |
| A6 | Preserving `no_std`-readiness (via `default-features=false`) is worth it given `framing` already uses `std` | Standard Stack | Low — purely a feature-flag choice; no behavioral risk. |
| A7 | `serde`/`postcard` legitimacy (slopcheck unavailable) | Package Audit | Very low — both are ecosystem-foundational; planner should still add a verify checkpoint. |
| A8 | Owned `Frame` (copy NAL into `Vec`) is acceptable for MVP vs a borrowed `Frame<'a>` | Pattern 3 | Low — one memcpy per frame; borrowed variant is a P7 perf option if profiling shows it matters. |

## Open Questions

1. **Resolution negotiation direction.** Reject-on-overflow (recommended, simplest) vs host downscales to client's max. — Recommendation: ship reject for MVP; revisit in P7 with rotation handling.
2. **`VideoConfig` resend policy as a pure helper.** Could lift `should_resend_config(...)` into the cable-free slice for free test coverage. — Recommendation: include it as a tiny pure fn if the planner wants extra in-scope coverage; otherwise defer with the live wiring.
3. **Whether to expose a borrowed `Frame<'a>` for true zero-copy `Video`.** — Recommendation: defer to P7 perf pass; owned form now.

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| Rust toolchain | building/testing `protocol` | ✓ | rustc 1.95.0 / cargo 1.95.0 | — |
| `serde` crate | message derives | ✓ (registry) | 1.0.228 | none needed |
| `postcard` crate | structured payload codec | ✓ (registry) | 1.1.3 | `bincode` (not recommended; roadmap locked postcard) |
| crates.io HTTP API | download-count verification | ✗ (sandbox-blocked) | — | `cargo search` against the registry index (used) |
| slopcheck | package legitimacy scan | ✗ | — | tag packages `[ASSUMED]` + planner checkpoint (applied) |
| USB cable / Pixel 6a / live transport | the deferred half of P5 | ✗ | — | **none — that work is out of scope for this slice** |

**Missing with no fallback (blocking the deferred half only):** USB cable, Pixel 6a, P1 transport. These do **not** block the in-scope protocol/negotiation work.

## Validation Architecture

> `.planning/config.json` is absent → `nyquist_validation` treated as enabled.

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[test]` / `cargo test` (no external test dep) |
| Config file | none — workspace `Cargo.toml` + per-crate; inline `#[cfg(test)] mod tests` (matches existing `framing.rs`/`nal.rs`) |
| Quick run command | `cargo test -p protocol` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements → Test Map (in-scope slice)
| Req | Behavior | Test Type | Automated Command | File Exists? |
|-----|----------|-----------|-------------------|--------------|
| PIPE-01 | every `Frame` variant round-trips | unit | `cargo test -p protocol roundtrip_` | ❌ Wave 0 (`messages.rs`) |
| PIPE-01 | `negotiate()` matrix | unit | `cargo test -p protocol negotiat` | ❌ Wave 0 |
| PIPE-01 | malformed/unknown-tag rejection | unit | `cargo test -p protocol -- decode_rejects` | ❌ Wave 0 |
| PIPE-01 | framing bounds (oversized/truncated) | unit | `cargo test -p protocol` | ✅ done (`framing.rs`) |

### Sampling Rate
- **Per task commit:** `cargo test -p protocol`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate (this slice):** `cargo test -p protocol` green; `cargo build --workspace` green.

### Wave 0 Gaps
- [ ] `crates/protocol/src/messages.rs` — new module, all in-scope types + codec + `negotiate()` + inline tests (covers PIPE-01 protocol/negotiation slice)
- [ ] `crates/protocol/Cargo.toml` — add `serde` + `postcard` deps (gate behind verify checkpoint per Package Audit)
- [ ] `crates/protocol/src/lib.rs` — `pub mod messages;`
- [ ] Framework install: none — `cargo test` is built in.

## Security Domain

> `security_enforcement` config absent → treated as enabled. Single trusted USB peer; no network surface in the MVP transport.

### Applicable ASVS Categories
| ASVS Category | Applies | Standard Control |
|---------------|---------|------------------|
| V2 Authentication | no | Single physical peer over a private USB link; no auth in MVP (note for P7 if NCM/TCP transport chosen). |
| V3 Session Management | no | Connectionless framing; no sessions. |
| V4 Access Control | no | N/A. |
| V5 Input Validation | **yes** | Bounded frame length (`MAX_FRAME_LEN`, done), tag validation (new), length-checked `Video` header, `Result`-returning postcard decode — no panics on wire data. |
| V6 Cryptography | no | No crypto on a local USB link in MVP; revisit only if a network transport is chosen. |

### Known Threat Patterns
| Pattern | STRIDE | Mitigation |
|---------|--------|------------|
| Oversized length → unbounded alloc | DoS | `MAX_FRAME_LEN` guard (done). |
| Malformed payload → panic | DoS/Tampering | All decode paths return `Result`; round-trip + malformed tests. |
| Unknown/forged tag → stream desync | Tampering | `decode` errors on unknown tag. |
| Truncated `Video` header → OOB | Tampering | Length check before slice/`try_into`. |

## Sources

### Primary (HIGH confidence)
- In-repo: `crates/protocol/src/framing.rs`, `nal.rs`, `lib.rs` (read in full — the codec layers on these, verified APIs + `MAX_FRAME_LEN` behavior).
- `docs.superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md` §0.1, §1 (D2/D6), §279-317 (P5 spec, locked `Frame`/wire design).
- `docs.rs/postcard/1.1.3` — feature model (`alloc` → `to_allocvec`/`from_bytes`; `no_std`-first; avoid `use-std`).
- Registry index via `cargo search`: `postcard = "1.1.3"`, `serde = "1.0.228"`, `heapless = "0.9.3"`.
- Local toolchain: `rustc 1.95.0`, `cargo 1.95.0`.

### Secondary (MEDIUM confidence)
- `.planning/REQUIREMENTS.md` (PIPE-01), `.planning/ROADMAP.md` (P5 success criteria) — corroborate scope.
- Existing `P3-capture-hardware-encode-on-macos/RESEARCH.md` — confirms project RESEARCH.md format + the cable-free/hands-on split convention.

### Tertiary (LOW confidence)
- crates.io download-count / repo metadata — could NOT be fetched (HTTP API unreachable in sandbox); existence confirmed via registry index only. slopcheck unavailable.

## Metadata

**Confidence breakdown:**
- Standard stack (serde/postcard + features): HIGH — versions verified against the registry, feature model from official docs; only download-count metadata unverified.
- Architecture (codec layering, tag dispatch, raw `Video` header, `negotiate()`): HIGH — composes already-tested in-repo modules; design follows the locked roadmap spec.
- Field sets + negotiation policy: MEDIUM — sound and minimal but `[ASSUMED]`; need user confirmation (Assumptions A1–A5).
- Pitfalls/security: HIGH — input-validation surface already largely covered by `framing`'s tested guards.

**Research date:** 2026-06-03
**Valid until:** 2026-07-03 (stable — serde/postcard 1.x are mature; the in-repo foundation is fixed)
