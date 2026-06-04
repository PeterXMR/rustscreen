# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-06-02)

**Core value:** Plug a Pixel 6a into an M1 MacBook with one USB-C cable, launch the host, and within seconds get a touch-capable extended display at < 50 ms glass-to-glass.
**Current focus:** **P3 COMPLETE — full capture→encode→playable H.264 PROVEN on hardware.** All three criteria met with measured numbers (ffplay plays the virtual desktop; SPS/PPS extracted+in-band; ~10 ms/frame encode latency). Encoder PR open on `feat/p3-encode`. Critical path now → P4 (decode on Pixel, hardware-blocked).

**Authoritative design source:** `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md` (full acceptance criteria, seam table, risk register). This `.planning/` set is the execution tracker.

## Current Position

Phase: P1 **COMPLETE** · **P3 COMPLETE (capture+encode proven on HW)** · **P5 protocol layer done (cable-free slice)** · **P6 touch done on BOTH ends' cable-free logic** (macOS mapping+FSM+CGEvent injection merged PR #8; Android `MotionEvent`→normalized `TouchEvent` on branch). Cable-free autonomous work now essentially exhausted — see ROADMAP "Blockers & What's Needed Next"; critical path is now **P4 (decode on the Pixel)** — phone-gated, with `out.h264` ready as test input.
Plan: P1-01 (Wave A + Wave B code executed, B3 hardware checkpoint pending); P3-01 (Wave A done); P5-01 planned & verified, cable-free Tasks 0–2 executed; P6-01 planned/verified/executed (cable-free slice — mapping + single-pointer FSM)
Status: P1 **COMPLETE — live byte round-trip GREEN.** D1 = **AOA chosen, NO sudo** (XPORT-01 closed). Full bidirectional byte-exact echo on real HW: 32B warm-up + 4× 1 MiB @ **103.0 Mbit/s**. The two live bugs are fixed (connect-hello handshake for the startup-ordering deadlock; `AccessoryFdTransport` 16 KiB read buffer for the host→device OUT truncation). See HARDWARE-FINDINGS.md. P3 Wave B + P5 live pipeline/latency still await hardware.
Last activity (P3 Wave B encode): 2026-06-04 — **P3 COMPLETE: full capture→VideoToolbox H.264→playable file PROVEN on hardware** (branch `feat/p3-encode`, commit `a884de5`, behind `live-capture`). `p3_encode` brings up the virtual display, captures it via SCK (420v/NV12), and hardware-encodes to H.264 with `VTCompressionSession` (RealTime, `AllowFrameReordering=false` → no B-frames → output order = input order), extracting the AVCC `CMBlockBuffer` + SPS/PPS from the format description in a `block2` output-handler and converting to Annex-B via the Wave-A `avcc_to_annex_b` (in-band SPS/PPS before the first IDR so the elementary stream self-describes). **All 3 criteria met with measured numbers:** (#1) `out.h264` decodes in **ffplay as `h264 (High) yuv420p 2400×1080`** — matches the virtual display exactly (proves SPS/PPS placement + Annex-B framing); (#2) codec config extracted+logged (**SPS 12 B, PPS 4 B, nal_len=4**); (#3) **per-frame encode latency avg 10.29 ms / min 8.60 / max 53.66 ms** (max = first-frame IDR + session warm-up, a one-time cost — exclude from the P5 budget average). 57 frames/3s ≈ 19 fps *arrival* cadence (SCK coalesces a near-static desktop); encode itself (~10 ms) sustains ~100 fps, so encode is NOT the pipeline bottleneck and has ample headroom vs the 50 ms glass-to-glass target. Default `cargo build --workspace` still pulls no objc2; clippy `-D warnings` + fmt clean. **ENC-01 met; P3 done.** Earlier in P3 Wave B (capture half): 2026-06-04 — **macOS capture half PROVEN on hardware** (branch `feat/p3-capture-encode`, behind off-by-default `live-capture` feature). Chose the **all-objc2** (madsmtm) Apple-FFI stack after a supply-chain + maturity review (rejected doom-fish `videotoolbox` 0.18.0 — 3 wks old / 636 dl / experimental — and an FFmpeg dep; objc2 is canonical, zero-RUSTSEC, pure-Rust, cohesive end-to-end buffer types). **B0** (`3e70c0e`): objc2 family wired optional/feature-gated; `p3_probe` confirmed the `CGVirtualDisplay` IS visible to ScreenCaptureKit → **FB17797423 retired** (the keystone capture risk). **B1** (`8cec440`): `p3_capture` confirmed `SCStreamOutput` delivers frames (97 frames/2s) — implemented the delegate as a custom `NSObject` via objc2 `define_class!` with `Arc<AtomicU64>` ivars, frames serviced on an owned GCD queue (sidesteps the run-loop footgun). Default `cargo build --workspace` still pulls no objc2 crate; clippy/fmt clean. Decided **D-capture**: Screen & System Audio Recording permission is unavoidable (no third-party bypass — DriverKit has no display family, private `CGVirtualDisplay` yields no frames to its creator, DisplayLink itself requires it); documented in README. Remaining: VideoToolbox `VTCompressionSession` encode → `out.h264` → ffplay (criterion #1), as a follow-up PR (API fully researched).
Last activity (P6 Android): 2026-06-04 — **Android-side touch normalization** (branch `feat/p6-android-touch-input`): `android_client::touch` — pure `to_touch_event`/`normalize`/`phase_for_action` converting a raw Android `MotionEvent` sample (masked action + pixel x/y + view w/h) into a normalized `protocol::messages::TouchEvent`. The exact INVERSE of the host's `map_normalized_to_global` — same top-left/y-down convention, so NO axis flip end-to-end. Clamps out-of-range, collapses non-finite/degenerate (zero/neg/NaN extent, NaN/inf coord) to finite `[0,1]` (host then accepts rather than rejects as NotFinite); `CANCEL`→`Up` (no stuck button); single-pointer policy (D4) deliberately left to the host FSM (one place). TDD: android-client 3→12 host tests; clippy `-D warnings` + fmt clean; cross-compiles to `aarch64-linux-android`. Deferred (device): Kotlin `onTouchEvent`→JNI capture shim + live `Frame::Touch` send (waits on the P5 live session + phone). With this, BOTH ends of the touch path have their cable-free logic; the cable-free autonomous backlog is now essentially empty (next real work is the hands-on-Mac P3 Wave B session — see ROADMAP Blockers table).
Last activity (P6): 2026-06-03 — **P6 cable-free slice planned via GSD** (RESEARCH HIGH-confidence; planner + plan-checker PASS, one D4 boundary test folded in) **and executed** (TDD) on branch `feat/p6-touch-mapping`: `macos_host::touch` — pure `map_normalized_to_global` (NO Y-flip: Quartz global display space + normalized touch are BOTH top-left/y-down; the AppKit/`NSScreen` bottom-left flip is never touched — a grep gate + corner regression test forbid `1.0 -` in source) with clamp-finite/reject-NaN sanitation (`MapError::NotFinite` — protocol's `TouchEvent` is intentionally unvalidated on decode), and a single-pointer touch→mouse `PointerStateMachine::step` (D4) folding `TouchEvent`s into `PointerAction`s behind the `PointerSink` injection port (D0 seam mirroring `transport::Transport`; a test-only `RecordingSink` proves the end-to-end fold in CI with no macOS API). Zero new deps; MSRV-1.80-safe. Workspace 90→105 tests (15 new: 7 map + 8 fsm); clippy `-D warnings` + fmt clean. Satisfies TOUCH-01 criterion #3's coordinate-mapping-TDD clause + criteria #1/#2 LOGIC. **Then extended (2026-06-04, same branch/PR #8) with the macOS injection adapter** (no longer deferred): `touch::CgEventSink` (first concrete `PointerSink`, via `core-graphics`) maps `Down/Move/Up` → `LeftMouseDown / MouseMoved|LeftMouseDragged / LeftMouseUp` posted to `CGEventTapLocation::HID`; `accessibility_trusted()` (`AXIsProcessTrusted()` FFI to ApplicationServices, D0 simplest-now) gates `CgEventSink::new()` with `InjectError::NotTrusted`; `p6_inject` smoke bin drives a scripted tap+drag through the FSM into the live sink, rect from `CGDisplay::main().bounds()`. All behind `--features live-inject` (off by default; bin gated via `required-features`), mirroring `live-usb` — default cross-platform CI never pulls `core-graphics`. Verified on macOS: builds + clippy `-D warnings` clean under default / live-usb / live-inject (incl. bin), fmt clean, workspace 105 tests green. Cable-free & phone-free (runs on the Mac). DEFERRED device-side only: Android `AInputEvent` capture + live Touch-frame send over USB. See P6-01-SUMMARY.md.
Last activity (P1 hardware): 2026-06-04 — **GREEN end-to-end byte echo** on branch `feat/p1-connect-hello`. Full bidirectional byte-exact round-trip M1↔Pixel 6a over AOA bulk: `device hello received` → `warm-up OK (32B)` → 4× `1048576 bytes ok` → **103.0 Mbit/s**. Closed the two remaining live bugs: (1) **startup-ordering deadlock** (host wrote bulk-OUT before the app opened the accessory; gadget dropped it) → **connect-hello handshake** (device sends a one-frame hello the instant it owns the fd; host reads it before writing — `transport::{HELLO_TAG,recv_frame,send_frame}`, `p1_echo` reads hello, `nativeOnUsbFd` sends hello); (2) **host→device OUT truncation** (codec's small `read_exact(5)` sized the accessory OUT URB too small, truncating the host's larger packet) → **`AccessoryFdTransport` 16 KiB read buffer** (`ACCESSORY_READ_CHUNK ≥ host BULK_CHUNK`; serves the codec's small reads from one large fd read). `p1_echo` debug scaffolding trimmed (`ITERS` 4→16, reacquire 20s→5s, quiet warm-up). 53 macos-host lib tests green; android arm64 clippy clean; fmt clean. **XPORT-01 met; D1 = AOA.**
Last activity (P1 hardware, earlier): 2026-06-03 — live M1↔Pixel 6a session. PROVEN on real HW: AOA handshake from Rust (protocol v2), **no sudo/entitlement** (A2 resolved via device-level control transfers), re-enumeration into accessory mode (18d1:2d01), Mac claims accessory interface, app receives fd + starts echo loop. Bugs found+fixed: macOS exclusive-access (device-level handshake), **16 KB ELF alignment** (.cargo/config.toml), UI-thread-blocking echo (background thread), missing accessory permission request (MainActivity now requestPermission). Interference: Android Auto intercepts the handshake (~10s) → disable during test; charge-only cables enumerate nothing. **Remaining:** device-side handoff race (option B: delay/retry the host interface claim). Code on `feat/p1-usb-roundtrip` + a new validation PR; 90 tests, clippy/fmt clean.
Last activity (P1): 2026-06-03 — **P1-01 buildable scope executed (TDD)** on branch `feat/p1-usb-roundtrip` (NOT pushed/merged). Wave A: `macos_host::transport` Transport seam (Read+Write+Send) + echo_roundtrip/make_pattern/first_mismatch/mbit_per_sec + 16 KiB chunked partial-read + framing regression (9 tests); `android_client::transport` echo_loop + AccessoryFdTransport (2 tests). Wave B (compiles, not run): `nusb 0.2.3` gated behind `live-usb` feature (default build pulls none); `macos_host::aoa` AOA handshake 51/52/53 + reacquire + AoaTransport(nusb bulk) + NcmTransport; `p1_echo` spike; Android `nativeOnUsbFd` JNI + accessory_filter.xml + manifest intent-filter + MainActivity openAccessory→detachFd. Workspace 77→88 tests; default+live-usb clippy clean; android arm64 cross-build clean; fmt clean. **Task B3 (live hardware) deliberately STOPPED before — needs user + phone; D1 verdict + throughput unrecorded.** See P1-01-SUMMARY.md.
Last activity (P5): 2026-06-03 — **P5 cable-free slice planned via GSD** (RESEARCH/CONTEXT/PLAN/P5-VALIDATION; plan-checker PASS after one revision) **and executed** (TDD) on branch `feat/p5-protocol-messages` (stacked on `feat/p3-capture-encode`): `protocol::messages` — `Frame` enum (Handshake/VideoConfig/Video/Touch/Control) + codec layered on `framing` (postcard for structured, raw `[pts u64 BE][keyframe u8][nal]` for Video) + pure `negotiate()` handshake/resolution negotiation. Added `serde`+`postcard` (verified legit, no_std-friendly). Workspace 52→75 tests; clippy/fmt clean. Satisfies PIPE-01 criterion #3 *logic*; criteria #1/#2 (live pipeline + latency) remain hardware-blocked.
Last activity: 2026-06-03 — **P3 planned via GSD** (RESEARCH.md, CONTEXT.md, PLAN.md, P3-VALIDATION.md; plan-checker PASS after one revision). **P3 Wave A executed** (TDD, cable-free): `encode_vt.rs` (`avcc_to_annex_b` + keyframe SPS/PPS in-band injection) and `capture_select.rs` (`DisplaySource`/`select_backend` SCK↔CGDisplayStream fallback). Workspace 33→46 tests, clippy/fmt clean. **Uncommitted** (no-commit session). Earlier cable-free spike also present: `protocol::nal` (SPS/PPS), `macos-host` `Capturer`/`Encoder` seams + `run_session`/`LatencyStats`, `protocol::framing`.
  - **P3 Wave B (NEEDS CABLE-FREE BUT HANDS-ON-MAC):** B0 gated dep installs (`screencapturekit` 7.0.0, `videotoolbox` 0.18.0 [SUS], in-tree `cg-virtual-display`), SCK + CGDisplayStream capture adapters, VideoToolbox encode adapter (fused zero-copy), spike `main` → `out.h264`, then `ffplay` visual gate (criterion #1). Risk: virtual display may be invisible to SCK *and* CGDisplayStream (Apple FB17797423) — escalate if both fail.

**Execution order (agreed, non-numeric):** P0 ✓ → P2 ✓ → ~~P1~~ (deferred, hardware-blocked) → **P3** → P4 → P5 → P6 → P7 → P8.

**⚠ Hardware-blocked (need Pixel 6a on USB — defer until user is back with cable):**
- **P1** (USB byte round-trip): requires the phone as a USB device for AOA/accessory testing; no emulator substitute.
- **P4** (decode on Pixel): requires the phone to decode/present.
Both stay deferred. Continue on Mac-only phases (P3 next, depends on P2's virtual display ID — now available). P5 (live pipeline) and P6 (touch) ultimately need P1/P4, so they wait too.

Progress: [█████░░░░░] 4 of 9 phases fully complete (P0, P2, **P1**, **P3**); cable-free slices landed for P5 (protocol), P6 (touch, both ends). Every Mac-only / pure-logic slice through P6 is implemented, and the two Mac-side hardware spikes (USB round-trip + capture/encode) are PROVEN — remaining work is overwhelmingly **phone**-gated (P4 decode on Pixel, then P5 live wiring + latency, P6 live touch).

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

Last session: 2026-06-04
Stopped at: **P3 COMPLETE.** Full capture→VideoToolbox H.264→playable `out.h264` proven on M1 (ffplay: `h264 High yuv420p 2400×1080`; SPS 12 B/PPS 4 B; encode latency avg 10.29 ms). `p3_encode` on branch `feat/p3-encode` (commit `a884de5`), encoder PR being opened. **Next critical-path work is P4 (decode on the Pixel)** — hardware-blocked (needs phone); `out.h264` is the ready-made test input. Then P5 (wire P1+P3+P4 live, measure <50 ms).
Resume file: .planning/phases/P3-capture-hardware-encode-on-macos/PLAN.md (+ P3-VALIDATION.md) · HARDWARE-FINDINGS.md (D1)
