# Phase P3: Capture + Hardware-Encode on macOS — Context

**Gathered:** 2026-06-03
**Status:** Ready for planning
**Source:** Orchestrator-synthesized from ROADMAP.md P3 + the master architecture roadmap (§0.1 seam table, P3 spike section) + the session's hard constraint (no Android device on cable) + RESEARCH.md. Interactive discuss-phase was skipped because the phase is already tightly specified (success criteria + steps + risks in the roadmap) and the scope boundary is dictated by the cable constraint.

**Requirement:** ENC-01

<domain>
## Phase Boundary

**Delivers:** The P2 virtual display (`CGDirectDisplayID`) is captured and hardware-encoded to H.264, producing a playable `.h264` file, with SPS/PPS codec config and per-frame encode latency observable. Capture and encode each sit behind a Rust port (R3 seam) so the experimental macOS-API adapters are swappable.

**Does NOT include:** USB transport (P1), Android decode/present (P4), the live pipeline (P5), HEVC (P7), or the pure-objc2 purity swap beyond what the chosen crate already provides (P7).
</domain>

<locked_decisions>
## Locked Decisions

1. **Seams stay (R3):** `Capturer` and `Encoder` remain Rust traits; adapters behind them are swappable. The existing `crates/macos-host/src/capture.rs` (`Capturer`/`CapturedFrame`) and `encode.rs` (`Encoder`/`EncodedFrame`/`LatencyStats`/`run_session`) are the locked port shapes — **do not break these signatures.** `CapturedFrame` stays pure metadata (`Clone + Eq`); no raw `IOSurface` field is added to it.

2. **Codec = H.264** (D2). HEVC is deferred to P7.

3. **Annex-B is the canonical on-the-wire / on-disk byte format** (matches `protocol::nal`). VideoToolbox emits AVCC, so the adapter MUST convert AVCC→Annex-B and inject SPS/PPS in-band. The converter is pure and cable-free-testable.

4. **CGDisplayStream fallback is co-equal, not optional** (RESEARCH headline risk: `CGVirtualDisplay` displays are frequently absent from `SCShareableContent`, Apple bug FB17797423). The first hands-on-Mac action is a probe: is the P2 display in `SCShareableContent.displays`? If not → fallback; if neither works → escalate like the P2 keystone risk.

5. **Fuse capture+encode in the hardware adapter** so the `IOSurface` never crosses a trait boundary (zero-copy), while still exposing the two traits for testing/swap.

6. **External crates are [ASSUMED]** — slopcheck supply-chain verification was denied by the sandbox. Every new dependency install MUST be gated behind a `checkpoint:human-verify` task. `videotoolbox` 0.18.0 is [SUS] (2 weeks old, ~633 downloads) — keep it strictly behind the `Encoder` trait with `objc2-video-toolbox` as the swap target.

7. **TDD for all cable-free logic** (the phase type says "expand into a TDD plan"; the user explicitly requested TDD). Red→green→refactor, watch each test fail first.

8. **No git commits this session** — planning docs and code stay as uncommitted working-tree diff for user review (standing session instruction). `commit_docs` config is overridden by this.
</locked_decisions>

<existing_work>
## Already built (prior cable-free spike, 33 tests passing, uncommitted)

The plan must BUILD ON these, not duplicate them:
- `crates/protocol/src/nal.rs` — Annex-B NAL split, `nal_unit_type`, `extract_codec_config` (SPS/PPS), `is_keyframe`. **Satisfies success criterion #2 at the logic level.**
- `crates/macos-host/src/encode.rs` — `Encoder` trait, `EncodedFrame`, `LatencyStats`, `run_session` capture→encode→sink pipeline. **Satisfies criterion #3 at the logic level.**
- `crates/macos-host/src/capture.rs` — `Capturer` trait + `CapturedFrame`.
- `crates/macos-host/src/lib.rs` — lib target exposing the seams.
- `crates/protocol/src/framing.rs` — length-prefixed framing (P5 foundation, ahead of phase but done).
</existing_work>

<scope_split>
## Cable-free vs hands-on-Mac (the planning axis)

**(a) Cable-free / CI-verifiable now** — pure Rust, fake-driven TDD like the existing `encode.rs` tests:
- `avcc_to_annex_b()` converter (length-prefixed AVCC → Annex-B start codes).
- SPS/PPS in-band injection on keyframes (using `protocol::nal`).
- Display selection: match P2 `display_id` against a candidate list; fallback selection logic (SCK-present → SCK, else CGDisplayStream).
- Any wiring/orchestration expressible behind the existing traits with fakes.

**(b) Hands-on-Mac — ONE human session, not CI-automatable** (Screen Recording TCC grant required):
- Add deps (gated `checkpoint:human-verify`): `screencapturekit` 7.0.0, `videotoolbox` 0.18.0 (or fallbacks).
- Probe display visibility → live SCK (or CGDisplayStream) capture → live VideoToolbox encode → confirm Annex-B output → `ffplay out.h264` visual check → confirm logged SPS/PPS + per-frame latency.

The plan should front-load wave (a) so it lands green now, and isolate (b) as a clearly gated block the user runs interactively.
</scope_split>

<discretion>
## Claude's Discretion
- Exact module layout for the AVCC→Annex-B converter (likely `crates/macos-host/src/encode.rs` or a small new module).
- Whether the hardware adapter is one fused struct or two cooperating structs internally (traits stay either way).
- Test data shapes for the converter (synthetic AVCC buffers).
</discretion>
