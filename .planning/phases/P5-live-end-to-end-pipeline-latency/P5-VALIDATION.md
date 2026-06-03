# P5 Validation — Protocol Messages + Handshake Negotiation (cable-free slice)

**Phase:** P5-live-end-to-end-pipeline-latency
**Requirement:** PIPE-01 (partial — criterion #3's logic only)
**Generated:** 2026-06-03
**Nyquist validation:** enabled (no `.planning/config.json` → treated as enabled).

> This slice covers ONLY PIPE-01 criterion #3's *logic*: the `Frame` codec round-trip and the
> pure `negotiate()` function. Both are fully unit-testable on the dev host with no cable, no
> Pixel, and no transport, so every in-scope behavior maps to an automated `cargo test`.
> Criteria #1 (live desktop visible) and #2 (glass-to-glass latency < 50 ms), plus the *act* of
> sending `VideoConfig` on connect/keyframe and per-stage timing, are hardware/cable-blocked and
> explicitly deferred (CONTEXT scope_split) — they have NO test here, by necessity.

## Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[cfg(test)]` / `cargo test` (no external test crate) |
| Config file | none — inline test modules (as in `framing.rs`, `nal.rs`) |
| Quick run | `cargo test -p protocol` |
| Full suite | `cargo test --workspace` |
| Dep hygiene check | `cargo tree -p protocol` (only `serde` + `postcard` + transitives) |

## Requirement → Criterion → Test Map

| Req | Criterion #3 clause | Behavior | Test type | Automated command / gate | Task | Artifact |
|-----|---------------------|----------|-----------|--------------------------|------|----------|
| PIPE-01 | codec round-trips (TDD) | every `Frame` variant encode→decode→equal (Handshake, VideoConfig w/ real SPS-PPS, Video normal/empty/large, Touch, Control) | unit (TDD, new) | `cargo test -p protocol roundtrip` | 1 | `protocol/src/messages.rs` |
| PIPE-01 | codec round-trips (TDD) | multi-frame stream written into and read back from ONE buffer in order (framing reused, not reinvented) | unit (TDD, new) | `cargo test -p protocol roundtrip` | 1 | `protocol/src/messages.rs` |
| PIPE-01 | codec round-trips (TDD) | `Video` payload is RAW `[pts u64 BE][keyframe u8][nal verbatim]` — byte-exact, no serde framing (locked decision 3) | unit (TDD, new) | `cargo test -p protocol video_payload` | 1 | `protocol/src/messages.rs` |
| PIPE-01 | robustness (locked decision 6) | unknown tag → `UnknownTag`; truncated Video header → `ShortVideoHeader`; malformed postcard → `Decode`; no panic / no silent skip | unit (TDD, new) | `cargo test -p protocol -- decode_rejects` | 1 | `protocol/src/messages.rs` |
| PIPE-01 | robustness (V5 input validation) | oversized payload rejected upstream by `framing::MAX_FRAME_LEN` (T-P5-01) | unit (existing) | `cargo test -p protocol` (framing tests) | (prior) | `protocol/src/framing.rs` |
| PIPE-01 | handshake/resolution negotiation succeeds | `negotiate()` happy path: host-preferred common codec + refresh clamped to client max (D6 default) | unit (TDD, new) | `cargo test -p protocol negotiat` | 2 | `protocol/src/messages.rs` |
| PIPE-01 | handshake/resolution negotiation succeeds | `negotiate()` matrix: `VersionMismatch`, `NoCommonCodec`, host-preference-ordered codec, `ResolutionUnsupported`, refresh clamp down/up, exact-fit boundary | unit (TDD, new) | `cargo test -p protocol negotiat` | 2 | `protocol/src/messages.rs` |
| PIPE-01 | (handshake version source of truth) | `Handshake.protocol_version` compared against `protocol_version()` (=1) | unit (covered in negotiate + roundtrip) | `cargo test -p protocol` | 1, 2 | `protocol/src/lib.rs` (consumed) |
| PIPE-01 | (regression) | existing 32 framing+nal+version tests stay green; no API broken | unit (regression) | `cargo test --workspace` | 1, 2 | workspace |
| PIPE-01 | (supply-chain hygiene) | `serde` + `postcard` added `default-features = false`; only those (+ transitives) appear in the `protocol` subtree | build check / human gate | Task 0 checkpoint + `cargo tree -p protocol` | 0 | `protocol/Cargo.toml` |

## Sampling Rate
- **Per task commit:** `cargo test -p protocol`
- **Per plan completion:** `cargo test --workspace`
- **Phase gate (this slice):** `cargo test -p protocol` green AND `cargo build --workspace` green AND `cargo tree -p protocol` shows only the two vetted deps.

## Coverage Assessment
- **codec round-trip (criterion #3, logic):** fully automated — every variant + raw-Video byte layout + malformed rejection (Task 1). No coverage gap in scope.
- **handshake/resolution negotiation (criterion #3, logic):** fully automated — pure-function matrix (Task 2). No coverage gap in scope.
- **`VideoConfig` sent on connect/keyframe (criterion #3, act of sending):** the *type* + faithful SPS/PPS round-trip are tested (Task 1); the *act of sending* is wire-blocked → DEFERRED, no test here.
- **criteria #1 (live desktop) and #2 (latency < 50 ms):** hardware/cable-blocked → DEFERRED, no test here, by necessity (CONTEXT scope_split).

## Wave 0 Gaps (closed by this plan)
- [x] `crates/protocol/src/messages.rs` — Frame + codec + `negotiate()` + inline TDD tests (Task 1, Task 2).
- [x] `crates/protocol/src/lib.rs` — `pub mod messages;` (Task 1).
- [x] `crates/protocol/Cargo.toml` — `serde` + `postcard` deps, gated behind the Task 0 blocking-human checkpoint.
- [x] No test-framework install needed (built-in `cargo test`).

## Notes
- Single crate (`protocol`), pure Rust, runs on any host — NO `cfg`-gating needed (unlike P3's macOS adapters).
- All artifacts and code are committed on the `feat/p5-protocol-messages` branch; do NOT merge to `main` without explicit user confirmation.
