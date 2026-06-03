# P3 Validation — Capture + Hardware-Encode on macOS

**Phase:** P3-capture-hardware-encode-on-macos
**Requirement:** ENC-01
**Generated:** 2026-06-03
**Nyquist validation:** enabled (no `.planning/config.json` → treated as enabled).

> Every ENC-01 success criterion maps to at least one automated test (Wave A, cable-free, CI)
> or, where the behavior is inherently un-headless-testable (live capture + visual playback),
> a hands-on-Mac human-verify gate (Wave B). The pure-Rust logic for criteria #2/#3 was already
> tested by the prior spike (33 tests in `protocol::nal` + `macos-host::encode`); this plan adds
> the two new pure units and the live adapters.

## Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[cfg(test)]` / `cargo test` (no external test crate) |
| Config file | none — inline test modules (as in `nal.rs`, `encode.rs`) |
| Quick run | `cargo test -p macos-host -p protocol` |
| Full suite | `cargo test --workspace` |
| Live build (macOS only) | `cargo build -p macos-host --features live-capture` |

## Requirement → Criterion → Test Map

| Req | Criterion | Behavior | Test type | Automated command / gate | Task | Artifact |
|-----|-----------|----------|-----------|--------------------------|------|----------|
| ENC-01 | #2 (precondition) | Annex-B NAL split + SPS/PPS extraction + keyframe detection | unit (existing) | `cargo test -p protocol nal` | (prior spike) | `protocol/src/nal.rs` |
| ENC-01 | #3 (precondition) | capture→encode→sink loop, latency stats, codec-config capture | unit (existing) | `cargo test -p macos-host encode` | (prior spike) | `macos-host/src/encode.rs` |
| ENC-01 | #2 | AVCC→Annex-B conversion produces parseable Annex-B (round-trips through `extract_codec_config`); length-prefix bounds guard (T-P3-01) | unit (TDD, new) | `cargo test -p macos-host avcc` | A1 | `macos-host/src/encode_vt.rs` |
| ENC-01 | #2 | SPS/PPS injected in-band on keyframes so `extract_codec_config` returns `Some(..)` even when the AVCC payload carried none | unit (TDD, new) | `cargo test -p macos-host annex_b` | A1 | `macos-host/src/encode_vt.rs` |
| ENC-01 | #1/#3 (robustness) | capture-adapter selection picks SCK when display id shareable, else CGDisplayStream fallback (incl. empty list); matches by id never index (T-P3-03) | unit (TDD, new) | `cargo test -p macos-host capture_select` | A2 | `macos-host/src/capture_select.rs` |
| ENC-01 | all | full workspace green incl. prior 33 tests, default features compile no macOS/external crate | unit (regression) | `cargo test --workspace` | A1, A2 | workspace |
| ENC-01 | (hygiene) | adapter deps + `cg-virtual-display` optional & off by default; `protocol` gains no deps | build check | `cargo tree -p macos-host` (none) vs `--features live-capture` (all); `cargo tree -p protocol` unchanged | B0 | `macos-host/Cargo.toml` |
| ENC-01 | #1 | `out.h264` captured from the virtual display plays in ffplay showing the virtual desktop | manual (human-verify, blocking) | `cargo run -p macos-host --features live-capture -- capture-spike` then `ffplay out.h264` | B4 | `out.h264` (gitignored) |
| ENC-01 | #2 | live encode emits non-empty SPS/PPS, printed to stdout/stderr (`println!`/`eprintln!`, not the silent `log` crate) | manual (human-verify, blocking) | read spike stdout/stderr during B4 run | B3 → B4 | spike stdout |
| ENC-01 | #3 | per-frame encode latency min/mean/max printed from a live, realtime/no-B-frames/zero-copy encode | manual (human-verify, blocking) | read spike stdout/stderr during B4 run | B2/B3 → B4 | spike stdout |

## Sampling Rate
- **Per task commit (Wave A):** `cargo test -p macos-host -p protocol`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** full workspace suite green AND the hands-on-Mac checklist (Screen Recording grant → Pitfall-1 probe → `ffplay out.h264` → SPS/PPS + latency on stdout) complete (B4) before `/gsd:verify-work`.

## Coverage Assessment
- **Criterion #1 (playback):** inherently visual + requires a live virtual display and Screen Recording TCC grant → not headless-automatable; covered by the B4 blocking human-verify gate. This is the only criterion with no automated test, by necessity (RESEARCH "Cable-Free vs Hands-on-Mac" split, A4 keystone risk).
- **Criterion #2 (SPS/PPS):** logic fully automated (A1 + prior `nal` tests); live confirmation in B3/B4.
- **Criterion #3 (latency / no-B-frames / zero-copy):** stats logic fully automated (prior `encode` tests); the no-B-frames config and zero-copy IOSurface path are confirmed in B2/B4 (config inspection + first-byte/latency check).

## Wave 0 Gaps (closed by this plan)
- [x] `avcc_to_annex_b` + `to_annex_b_frame` + tests (A1) — ENC-01 #2 output reconciliation.
- [x] `select_backend` display-id/fallback selection + tests (A2) — ENC-01 capture robustness.
- [x] No test-framework install needed (built-in `cargo test`).

## Notes
- Per CONTEXT D8, this VALIDATION.md and all P3 code/docs stay uncommitted this session (working-tree diff for user review).
