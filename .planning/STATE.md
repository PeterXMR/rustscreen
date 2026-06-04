# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-06-02)

**Core value:** Plug a Pixel 6a into an M1 MacBook with one USB-C cable, launch the host, and within seconds get a touch-capable extended display at < 50 ms glass-to-glass.
**Current focus:** Phase P2 — Create a virtual display from Rust (keystone risk R1).

**Authoritative design source:** `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md` (full acceptance criteria, seam table, risk register). This `.planning/` set is the execution tracker.

## Current Position

Phase: P1 (Wave A done + Wave B code done; live B3 gated) · P3 (Wave A done, Wave B gated) · **P5 protocol layer done (cable-free slice)**
Plan: P1-01 (Wave A + Wave B code executed, B3 hardware checkpoint pending); P3-01 (Wave A done); P5-01 planned & verified, cable-free Tasks 0–2 executed
Status: P1 buildable scope landed — XPORT-01 NOT yet met (live 1 MB echo + replug + throughput + D1 verdict all gated on Task B3, which needs the Pixel 6a); P3 Wave B + P5 live pipeline/latency await hardware
Last activity (P1): 2026-06-03 — **P1-01 buildable scope executed (TDD)** on branch `feat/p1-usb-roundtrip` (NOT pushed/merged). Wave A: `macos_host::transport` Transport seam (Read+Write+Send) + echo_roundtrip/make_pattern/first_mismatch/mbit_per_sec + 16 KiB chunked partial-read + framing regression (9 tests); `android_client::transport` echo_loop + AccessoryFdTransport (2 tests). Wave B (compiles, not run): `nusb 0.2.3` gated behind `live-usb` feature (default build pulls none); `macos_host::aoa` AOA handshake 51/52/53 + reacquire + AoaTransport(nusb bulk) + NcmTransport; `p1_echo` spike; Android `nativeOnUsbFd` JNI + accessory_filter.xml + manifest intent-filter + MainActivity openAccessory→detachFd. Workspace 77→88 tests; default+live-usb clippy clean; android arm64 cross-build clean; fmt clean. **Task B3 (live hardware) deliberately STOPPED before — needs user + phone; D1 verdict + throughput unrecorded.** See P1-01-SUMMARY.md.
Last activity (P5): 2026-06-03 — **P5 cable-free slice planned via GSD** (RESEARCH/CONTEXT/PLAN/P5-VALIDATION; plan-checker PASS after one revision) **and executed** (TDD) on branch `feat/p5-protocol-messages` (stacked on `feat/p3-capture-encode`): `protocol::messages` — `Frame` enum (Handshake/VideoConfig/Video/Touch/Control) + codec layered on `framing` (postcard for structured, raw `[pts u64 BE][keyframe u8][nal]` for Video) + pure `negotiate()` handshake/resolution negotiation. Added `serde`+`postcard` (verified legit, no_std-friendly). Workspace 52→75 tests; clippy/fmt clean. Satisfies PIPE-01 criterion #3 *logic*; criteria #1/#2 (live pipeline + latency) remain hardware-blocked.
Last activity: 2026-06-03 — **P3 planned via GSD** (RESEARCH.md, CONTEXT.md, PLAN.md, P3-VALIDATION.md; plan-checker PASS after one revision). **P3 Wave A executed** (TDD, cable-free): `encode_vt.rs` (`avcc_to_annex_b` + keyframe SPS/PPS in-band injection) and `capture_select.rs` (`DisplaySource`/`select_backend` SCK↔CGDisplayStream fallback). Workspace 33→46 tests, clippy/fmt clean. **Uncommitted** (no-commit session). Earlier cable-free spike also present: `protocol::nal` (SPS/PPS), `macos-host` `Capturer`/`Encoder` seams + `run_session`/`LatencyStats`, `protocol::framing`.
  - **P3 Wave B (NEEDS CABLE-FREE BUT HANDS-ON-MAC):** B0 gated dep installs (`screencapturekit` 7.0.0, `videotoolbox` 0.18.0 [SUS], in-tree `cg-virtual-display`), SCK + CGDisplayStream capture adapters, VideoToolbox encode adapter (fused zero-copy), spike `main` → `out.h264`, then `ffplay` visual gate (criterion #1). Risk: virtual display may be invisible to SCK *and* CGDisplayStream (Apple FB17797423) — escalate if both fail.

**Execution order (agreed, non-numeric):** P0 ✓ → P2 ✓ → ~~P1~~ (deferred, hardware-blocked) → **P3** → P4 → P5 → P6 → P7 → P8.

**⚠ Hardware-blocked (need Pixel 6a on USB — defer until user is back with cable):**
- **P1** (USB byte round-trip): requires the phone as a USB device for AOA/accessory testing; no emulator substitute.
- **P4** (decode on Pixel): requires the phone to decode/present.
Both stay deferred. Continue on Mac-only phases (P3 next, depends on P2's virtual display ID — now available). P5 (live pipeline) and P6 (touch) ultimately need P1/P4, so they wait too.

Progress: [██░░░░░░░░] ~22% (2 of 9 phases: P0, P2 complete)

## Performance Metrics

**Velocity:**
- Total plans completed: 0 (P0 delivered outside this tracker as PR #1)
- Average duration: -
- Total execution time: -

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| P0 | — | — | — (PR #1) |

**Recent Trend:**
- Last 5 plans: -
- Trend: -

*Updated after each plan completion*

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table (D0–D7). Recent decisions affecting current work:

- **D0 (LOCKED):** "100% Rust" = simplest-now, shrink-to-Rust-later via ports & adapters; core never changes on adapter swap.
- **D2 (LOCKED):** H.264 for MVP; HEVC is a P7 toggle.
- **D3 (LOCKED):** Decode-to-surface on Android.
- **D6 (proposed default):** Extend at 2400×1080@60 — directly drives the P2 virtual-display geometry.
- **D7 (proposed default):** Thin Kotlin shell for MVP → NativeActivity in P7.

### Pending Todos

None yet.

### Blockers/Concerns

- **R1 (keystone, P2):** `CGVirtualDisplay` is a private API — may need entitlements/signing even in dev, refresh capped ~60 Hz, can break across macOS versions, barred from MAS. If it proves unworkable, surface to the user immediately (DriverKit / mirroring fallbacks change the architecture).
- **Open question:** Glass-to-glass < 50 ms is unmeasured (P5 must prove it).
- **Open question:** Exact entitlement set for `CGVirtualDisplay` under hardened runtime (empirically determined in P7).

## Deferred Items

Items carried forward:

| Category | Item | Status | Deferred At |
|----------|------|--------|-------------|
| Input | Multitouch + pen injection (INPUT-V2-01) | v2 | 2026-06-02 |
| Devices | Android models beyond Pixel 6a (DEV-V2-01) | v2 | 2026-06-02 |

## Session Continuity

Last session: 2026-06-03
Stopped at: P1-01 Wave A + Wave B code executed on `feat/p1-usb-roundtrip` (not pushed). STOPPED before Task B3 (live 1 MB echo / replug / throughput / D1 verdict) — requires the Pixel 6a on USB-C. XPORT-01 not yet met; plan intentionally NOT advanced to complete.
Resume file: .planning/phases/P1-usb-byte-round-trip/P1-01-PLAN.md (Task B3, hands-on)
