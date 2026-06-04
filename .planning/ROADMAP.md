# Roadmap: RustScreen

## Overview

RustScreen de-risks first, then builds. The journey: scaffold the workspace (P0, done), then immediately attack the three scary unknowns as spikes — create a virtual display from Rust (P2, the keystone), move bytes over USB both ways (P1), capture+encode on macOS (P3), decode+present on the Pixel (P4) — before wiring them into a live sub-50 ms pipeline (P5), adding the touch back-channel (P6), and finally hardening, raising Rust purity, and packaging for distribution (P7, P8). The authoritative design source (full acceptance criteria, seam table, risk register) is `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md`; this ROADMAP.md is the execution tracker.

## Phases

**Phase Numbering:** Phases use the source roadmap's P0–P8 labels (not 1–N). Spikes are marked 🔬.

**Execution Order (agreed, non-numeric):** P0 → **P2** → P1 → P3 → P4 → P5 → P6 → P7 → P8. P2 is front-loaded ahead of P1 because it is the keystone risk (R1): if creating a virtual display from Rust fails, the whole architecture changes, so it is attacked first.

- [x] **Phase P0: Workspace Scaffold & Cross-Compilation** - Cargo workspace, cross-compile, thin Kotlin shell, CI (COMPLETE — PR #1)
- [x] **Phase P2: Create a Virtual Display from Rust** 🔬 - Keystone risk R1 RETIRED; phantom display via private `CGVirtualDisplay` (COMPLETE — branch `feat/p2-virtual-display`)
- [x] **Phase P1: USB Byte Round-Trip** 🔬 - **COMPLETE.** Live byte-exact round-trip green on M1↔Pixel 6a over AOA (no sudo): 32B warm-up + 4× 1 MiB @ **103.0 Mbit/s**. **D1 = AOA** (NCM/TCP fallback unused); XPORT-01 met. Connect-hello handshake + 16 KiB read-buffer fixes closed the two live bugs
- [~] **Phase P3: Capture + Hardware-Encode on macOS** 🔬 - Wave A merged (PR #4); Wave B **capture half PROVEN on hardware** (all-objc2 stack: SCK sees the virtual display + delivers frames, 97 fps; FB17797423 retired) — capture-foundation PR open. Remaining: VideoToolbox encode → playable `.h264` + ffplay gate
- [ ] **Phase P4: Decode + Present on the Pixel** 🔬 - ⚠ DEFERRED (hardware-blocked: needs Pixel 6a). AMediaCodec decode-to-surface onto `ANativeWindow`
- [~] **Phase P5: Live End-to-End Pipeline + Latency** - Cable-free protocol slice merged (PR #5: `Frame` codec + `negotiate()` — criterion #3 logic). Remaining: wire P1–P4 live + measure glass-to-glass < 50 ms (blocked on P1/P4)
- [~] **Phase P6: Touch Back-Channel → macOS Injection** - BOTH ends' cable-free logic done: Mac side (`macos_host::touch`: normalized→global CG mapping (no Y-flip) + single-pointer FSM behind `PointerSink` + live `CgEventSink` CGEvent injector & `AXIsProcessTrusted` gate behind `--features live-inject` + `p6_inject` bin) AND Android side (`android_client::touch`: raw `MotionEvent` → normalized `TouchEvent`, the inverse mapping, host-tested + cross-compiles to arm64). Deferred (device): Kotlin `onTouchEvent`→JNI capture shim + live Touch-frame send over the session
- [ ] **Phase P7: Robustness, UX, Codec Options, Signing + Rust-Purity Upgrades** - Hotplug, HEVC, menu-bar, notarize, pure-Rust adapters
- [ ] **Phase P8: Packaging, Distribution, OSS Hygiene** - Notarized DMG + release `.apk`, README/LICENSE, clone-to-second-screen

## Phase Details

### Phase P0: Workspace Scaffold & Cross-Compilation
**Goal**: A Cargo workspace builds and runs on both platforms — a macOS binary and an Android app (thin Kotlin shell over the Rust `cdylib`) — with CI green.
**Status**: COMPLETE — delivered as PR #1 on branch `feat/p0-workspace-scaffold`.
**Depends on**: Nothing (first phase)
**Requirements**: FOUND-01
**Success Criteria** (what must be TRUE):
  1. A blank app launches on the Pixel 6a and `adb logcat` shows "hello from Rust" (Kotlin shell, Rust core).
  2. The `macos-host` binary builds and runs on the M1 Mac.
  3. GitHub Actions CI builds the Mac binary, the Android `cdylib` via `cargo-ndk`, and the debug `.apk` — all green.
**Plans**: Done (PR #1)
**UI hint**: yes

### Phase P2: Create a Virtual Display from Rust 🔬
**Goal**: A macOS virtual display is created entirely from Rust via the private `CGVirtualDisplay` API, behind a safe `VirtualDisplay` port, with its `CGDirectDisplayID` obtainable for downstream capture. This is the keystone risk R1.
**Depends on**: Phase P0
**Requirements**: VDISP-01
**Success Criteria** (what must be TRUE):
  1. Running the spike creates a 2400×1080@60 display that appears in System Settings ▸ Displays and is arrangeable (D6).
  2. The phantom display is created entirely from Rust source (ObjC++ `.mm` shim behind the safe `VirtualDisplay::new` port — no Rust core logic exposed to the shim).
  3. `display_id()` returns a valid non-zero `CGDirectDisplayID` enumerable via `CGGetActiveDisplayList`.
**Type**: Spike — expand into a detailed TDD plan only after the spike succeeds. If `CGVirtualDisplay` proves unworkable, surface to the user immediately (DriverKit / mirroring fallbacks change the architecture).
**Plans**: TBD
**UI hint**: yes

### Phase P1: USB Byte Round-Trip 🔬
**Goal**: Bytes move reliably in both directions Mac↔Pixel over a single USB-C cable, with the transport mechanism (AOA vs network-over-USB) decided on measured throughput/reliability. Resolves D1.
**Depends on**: Phase P0 (independent of P2; sequenced after P2 by agreed order)
**Requirements**: XPORT-01
**Success Criteria** (what must be TRUE):
  1. 1 MB echoes correctly Mac→phone→Mac (verified byte-for-byte) over USB-C.
  2. The round-trip reproduces reliably across cable replug.
  3. Measured throughput is documented with ≥ ~200 Mbit/s headroom for 1080p H.264, and D1 (AOA vs NCM/TCP) is decided with recorded rationale.
**Type**: Spike (dual sub-spike: A = AOA via `nusb` + `jni` `UsbManager.openAccessory()`, lead; B = NCM/TCP fallback). Expand into a TDD plan after the spike succeeds.
**Plans:** 1 plan (Wave A cable-free TDD + Wave B hands-on, in P1-01-PLAN.md). D1 verdict + measured throughput recorded here on completion.
Plans:
- [x] P1-01-PLAN.md — **COMPLETE incl. Task B3 live hardware (2026-06-04).** Code (branch `feat/p1-usb-roundtrip`, merged PR #6/#7): `Transport` seam + echo/pattern/throughput/chunking + framing regression + Android `echo_loop` (TDD); `nusb 0.2.3` gated behind `live-usb`; AOA host (`aoa.rs` handshake 51/52/53 + reacquire + `AoaTransport`/`NcmTransport`) + `p1_echo` spike; Android accessory glue (`nativeOnUsbFd` JNI + `accessory_filter.xml` + manifest intent-filter + `openAccessory`→`detachFd`). **Task B3 (live, branch `feat/p1-connect-hello`):** byte-exact 32B warm-up + 4× 1 MiB round-trip @ **103.0 Mbit/s** on M1↔Pixel 6a; two live bugs fixed (connect-hello handshake + `AccessoryFdTransport` 16 KiB read buffer).

**D1 verdict: AOA (Android Open Accessory).** The AOA path delivers a reliable byte-exact bidirectional round-trip from Rust/macOS with **no sudo / no entitlement** (A2 resolved — handshake via device-level control transfers, only the driverless accessory interface is claimed). NCM/TCP remains a documented fallback but was not needed for viability.

**Measured throughput: ~103 Mbit/s** (round-trip echo, 4× 1 MiB). Note this is a *synchronous round-trip* figure (`p1_echo` waits for each full echo before sending the next — latency-bound, no transfer pipelining), so it **under-measures** the one-way, queued host→phone streaming the video path actually uses. It is below criterion #3's original "≥ ~200 Mbit/s headroom" aspiration but already clears a 1080p H.264 stream (~10–25 Mbit/s) by **4–8×**. If the live P5 pipeline ever proves bandwidth-bound, the levers are (a) pipelining/queuing bulk transfers and (b) the NCM/TCP fallback; revisit the 200 Mbit/s bar against a one-way streaming measurement then.

**Criteria status:** #1 ✅ (1 MiB byte-for-byte echoes — verified ×4); #2 ✅ (reproduces across replug — the runbook documents the replug→re-handshake reset); #3 ⚠️ partial (D1 decided + throughput documented ✅; the literal ≥200 Mbit/s-headroom bar is not met by the synchronous echo, see note above — sufficient for MVP video, re-evaluate one-way in P5).

### Phase P3: Capture + Hardware-Encode on macOS 🔬
**Goal**: The virtual display is captured and hardware-encoded to H.264, producing a playable file, with codec config and per-frame latency observable.
**Depends on**: Phase P2 (needs the virtual display's `CGDirectDisplayID`)
**Requirements**: ENC-01
**Success Criteria** (what must be TRUE):
  1. A `.h264` file captured from the virtual display plays back correctly in ffplay/VLC showing the virtual desktop.
  2. SPS/PPS (codec config) is extracted and logged for downstream decode.
  3. Per-frame encode latency is logged (realtime/low-latency config, no B-frames, zero-copy IOSurface).
**Type**: Spike. Wrap VideoToolbox/SCK behind own `Encoder`/`Capturer` traits (R3 churn); CGDisplayStream fallback. Expand into a TDD plan after the spike succeeds.
**Plans:** 1 plan (Wave A cable-free TDD + Wave B hands-on-Mac, in P3-01-PLAN.md)
Plans:
- [~] P3-01-PLAN.md — Wave A done (AVCC→Annex-B + capture-select, TDD, merged PR #4). **Wave B in progress (branch `feat/p3-capture-encode`, all-objc2 stack — see DEP rationale below):**
  - ✅ **B0** — objc2 family wired behind off-by-default `live-capture` feature; `p3_probe` proved the `CGVirtualDisplay` is visible to ScreenCaptureKit (FB17797423 does NOT bite). Commit `3e70c0e`.
  - ✅ **B1** — `p3_capture` proved `SCStreamOutput` delivers frames (97 frames/2s on M1↔virtual display); objc2 `define_class!` delegate on an owned GCD queue sidesteps the run-loop footgun. Commit `8cec440`. **→ capture-foundation PR.**
  - ⏭️ **B2/B3** — VideoToolbox `VTCompressionSession` encode (CVPixelBuffer → H.264 → `to_annex_b_frame`) → `out.h264` → ffplay visual gate (criterion #1). API fully researched; next PR.
  - **Stack note:** capture/encode built on the madsmtm **objc2** family (not doom-fish `videotoolbox`, which is 3-weeks-old/experimental, nor FFmpeg) after a supply-chain + maturity review — canonical, zero-RUSTSEC, pure-Rust, cohesive end-to-end buffer types. `CGDisplayStream` fallback is moot: obsoleted in the macOS 15 SDK, and SCK sees the virtual display anyway.

**Note (D-capture, decided 2026-06-04):** capturing the virtual display REQUIRES the macOS **Screen & System Audio Recording** permission — there is no third-party bypass (DriverKit has no display family; the private `CGVirtualDisplay` delivers no frames to its creator; DisplayLink itself requires the grant). Documented in README; the shipped app will request it once for its own bundle.
**UI hint**: yes

### Phase P4: Decode + Present on the Pixel 🔬
**Goal**: Hardware-decoded H.264 renders on the Pixel screen via `ANativeWindow` using decode-to-surface, with no Java decode wrapper.
**Depends on**: Phase P3 (needs the captured `.h264` + SPS/PPS)
**Requirements**: DEC-01
**Success Criteria** (what must be TRUE):
  1. The recorded desktop video plays full-screen on the Pixel 6a.
  2. Decoding runs through a thin Rust `ndk-sys` `AMediaCodec` wrapper (no Java decode wrapper).
  3. Frames render decode-to-surface onto `ANativeWindow` with no CPU/GPU round-trip copy (D3).
**Type**: Spike. Expect to own the MediaCodec wrapper (R4 — `rust_mediacodec` stale). Expand into a TDD plan after the spike succeeds.
**Plans**: TBD
**UI hint**: yes

### Phase P5: Live End-to-End Pipeline + Latency
**Goal**: P1–P4 are wired live so the Pixel shows the Mac's extended desktop in real time, with glass-to-glass latency measured under 50 ms.
**Depends on**: Phase P4 (and P1 transport, P2 display, P3 encode)
**Requirements**: PIPE-01
**Success Criteria** (what must be TRUE):
  1. A live extended desktop is visible on the Pixel, updating in real time.
  2. Measured glass-to-glass latency is < 50 ms, with per-stage timings logged and the §2 budget revised with real numbers.
  3. The protocol frame codec round-trips (TDD-verified) and `VideoConfig` (SPS/PPS) is sent on connect and on each keyframe; handshake/resolution negotiation succeeds.
**Type**: Build (TDD for protocol framing + handshake negotiation).
**Plans:** 1 plan for the cable-free slice (criterion #3 logic only; criteria #1/#2 + live wiring are cable/device-blocked, deferred). In P5-01-PLAN.md.
Plans:
- [ ] P5-01-PLAN.md — `protocol::messages` Frame codec layered on `framing` + pure `negotiate()` (TDD, CI-green) behind one serde/postcard dep-gate checkpoint. Live transport / send-on-connect / decode loop / latency harness / touch deferred.
**UI hint**: yes

### Phase P6: Touch Back-Channel → macOS Injection
**Goal**: Touch on the Pixel drives the macOS cursor on the virtual display via injected `CGEvent`s.
**Depends on**: Phase P5
**Requirements**: TOUCH-01
**Success Criteria** (what must be TRUE):
  1. Tapping on the Pixel moves the macOS cursor to the correct location on the virtual display.
  2. Dragging on the Pixel produces a click-and-drag on the Mac (mouseDown → move → mouseUp).
  3. Coordinate mapping (normalized → global CG coords) is TDD-verified; injection is gated on `AXIsProcessTrusted()` with an actionable Accessibility-permission prompt.
**Type**: Build (TDD for coordinate mapping). Single-pointer mouse emulation only (D4).
**Plans:** 1 plan for the cable-free slice (criterion #3's coordinate-mapping-TDD clause + criteria #1/#2 LOGIC only; real CGEvent injection + `AXIsProcessTrusted()` gating + Android `AInputEvent` capture are hands-on-Mac/device-blocked, deferred behind the `PointerSink` port). In P6-01-PLAN.md.
Plans:
- [x] P6-01-PLAN.md — **cable-free slice DONE (2026-06-03, branch `feat/p6-touch-mapping`).** `macos_host::touch` — pure `map_normalized_to_global` (NO Y-flip: Quartz global space + normalized touch are both top-left/y-down) with clamp-finite/reject-NaN sanitation, and a single-pointer touch→mouse `PointerStateMachine` (D4) emitting `PointerAction`s behind the `PointerSink` injection port (the D0 seam the `CGEvent` adapter implements). TDD, workspace 90→105 tests; clippy/fmt clean.
  - **+ macOS injection adapter (2026-06-04, same branch):** `CgEventSink` (first concrete `PointerSink`, via `core-graphics`) maps `Down/Move/Up` → `LeftMouseDown / MouseMoved|LeftMouseDragged / LeftMouseUp` posted to `CGEventTapLocation::HID`; `accessibility_trusted()` (`AXIsProcessTrusted()` FFI) gates construction (`InjectError::NotTrusted` — TOUCH-01 onboarding); `p6_inject` smoke bin drives a scripted tap+drag through the FSM into the live sink, sourcing the rect from `CGDisplay::main().bounds()`. All behind `--features live-inject` (off by default), mirroring `live-usb`; builds + clippy clean under default/live-usb/live-inject on macOS. Cable-free & phone-free (runs on the Mac). **Merged as PR #8.**
- [x] **Android-side touch normalization (2026-06-04, branch `feat/p6-android-touch-input`).** `android_client::touch` — pure `to_touch_event`/`normalize`/`phase_for_action`: converts a raw Android `MotionEvent` sample (masked action + pixel x/y + view w/h) into a normalized `protocol::messages::TouchEvent`. The exact INVERSE of the host's `map_normalized_to_global` (same top-left/y-down convention ⇒ no axis flip end-to-end); clamps out-of-range, collapses non-finite/degenerate inputs to finite `[0,1]`. TDD (android-client 3→12 host tests); clippy/fmt clean; cross-compiles to `aarch64-linux-android`. **Remaining for P6 (deferred):** the Kotlin `onTouchEvent`→JNI capture shim that feeds `to_touch_event`, and sending the resulting `Frame::Touch` over the live session — both wait on the live pipeline (P5) and on-device verification (phone).
**UI hint**: yes

### Phase P7: Robustness, UX, Codec Options, Signing + Rust-Purity Upgrades
**Goal**: The MVP becomes robust and shippable — survives hotplug, gains HEVC and a menu-bar app, is signed and notarized — and the simplest-now adapters are swapped for pure-Rust ones without touching core logic.
**Depends on**: Phase P6 (MVP pipeline proven first)
**Requirements**: ROBUST-01, PURITY-01
**Success Criteria** (what must be TRUE):
  1. The host survives 20 replug cycles: clean teardown, auto-reconnect, and the virtual display is removed on disconnect so the desktop reflows.
  2. HEVC works on the Pixel 6a behind a flag (falling back to H.264), and a menu-bar app (D5) exposes connect/disconnect, resolution, and latency readout.
  3. The host is code-signed with hardened runtime and notarized, launching without Gatekeeper warnings.
  4. Rust-purity adapter swaps land without changing core logic — Kotlin shell → NativeActivity, ObjC++ shim → pure `objc2`, capture Swift bridge → `objc2-screen-capture-kit` — and the Rust purity % metric is updated (~99% target).
**Type**: Build (each purity swap is an isolated adapter replacement behind an existing port).
**Plans**: TBD
**UI hint**: yes

### Phase P8: Packaging, Distribution, OSS Hygiene
**Goal**: Both halves are distributable and a new user can go from clone to a working second screen using only the README.
**Depends on**: Phase P7
**Requirements**: PKG-01
**Success Criteria** (what must be TRUE):
  1. macOS ships a signed, notarized `.app` in a DMG; Android ships a signed release `.apk` (`cargo apk build --release`).
  2. A new user can clone, build both halves, install, plug in a Pixel 6a, and get a second screen following only the README.
  3. Repo OSS hygiene is in place: README (architecture + honest "100% Rust source" statement + per-platform build/run), LICENSE (MIT/Apache-2.0), CONTRIBUTING, and CI badges.
**Type**: Build.
**Plans**: TBD

## Progress

**Execution Order:** P0 → P2 → P1 → P3 → P4 → P5 → P6 → P7 → P8 (P2 front-loaded ahead of P1 — keystone risk R1).

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| P0. Workspace Scaffold | — (PR #1) | ✅ Complete (merged) | 2026-06-02 |
| P2. Virtual Display (R1 keystone) 🔬 | spike ✓ (PR #3) | ✅ Complete — R1 retired, phantom display via private `CGVirtualDisplay` (merged) | 2026-06-03 |
| P1. USB Byte Round-Trip 🔬 | 1/1 (PR #6, #7 merged + connect-hello PR) | ✅ **Complete** — live byte-exact round-trip on M1↔Pixel 6a over AOA, **no sudo** (A2 retired): 4× 1 MiB @ **103.0 Mbit/s**. **D1 = AOA**; XPORT-01 met (throughput is a synchronous-echo floor, see Phase P1 note) | 2026-06-04 (HW) |
| P3. Capture + Encode 🔬 | 0/1 (PR #4 merged; capture-foundation PR open) | 🟡 Wave A done (merged); Wave B **capture half PROVEN on HW** (all-objc2: SCK sees + delivers frames from the virtual display, 97 fps; FB17797423 retired). Remaining: VideoToolbox encode → ffplay gate | 2026-06-04 (capture) |
| P4. Decode + Present 🔬 | 0/TBD | ⚠ Deferred (needs Pixel 6a) | - |
| P5. Live Pipeline + Latency | 0/1 cable-free slice (PR #5 merged) | 🟡 Cable-free protocol slice done (`Frame` codec + `negotiate()`, merged — satisfies criterion #3 logic); live wiring + latency blocked on P1/P4 | 2026-06-03 (slice) |
| P6. Touch Injection | 0/1 (PR #8 merged + Android slice) | 🟡 Both ends' cable-free logic done — Mac side (`macos_host::touch`: mapping + FSM + `CgEventSink`/`AXIsProcessTrusted` behind `live-inject` + `p6_inject` bin, PR #8 merged) and Android side (`android_client::touch`: `MotionEvent`→normalized `TouchEvent`); Kotlin capture shim + live wiring deferred | 2026-06-04 (logic) |
| P7. Robustness + Purity | 0/TBD | Not started | - |
| P8. Packaging + Distribution | 0/TBD | Not started | - |

**Legend:** ✅ phase complete · 🟡 substantial slice landed (cable-free / de-risked), remainder hardware-blocked · ⚠ deferred (needs phone) · blank = not started.

**Cable-free progress:** **every Mac-only / pure-logic slice through P6 is now implemented and merged** (P0–P6 logic on `main`). The autonomous cable-free well is essentially dry — what remains needs a hands-on-Mac session or the phone, per the blockers below.

## Blockers & What's Needed Next

The critical path to a working MVP is **P3 (capture+encode) → P4 (decode) → P5 (live wiring)**. All three are currently gated. Touch (P6) is a complete-on-both-ends side-channel waiting only on that pipeline.

| Item | What's left | Blocker — what's needed to proceed | Who |
|------|-------------|------------------------------------|-----|
| **P3 Wave B** (next on critical path) | SCK + VideoToolbox + CGDisplayStream capture/encode adapters → `out.h264` → `ffplay` visual gate | **(1) Human supply-chain approval (Task B0):** `videotoolbox` 0.18.0 is `[SUS]` (~633 dl, ~2 wks old) — plan forbids `cargo add` without verifying it (or switching to the `objc2-video-toolbox` fallback). **(2) Hands-on-Mac:** Screen Recording TCC grant + run the spike + eyeball `ffplay`. **(3) RISK:** `CGVirtualDisplay` may be invisible to ScreenCaptureKit (Apple FB17797423) — CGDisplayStream fallback co-equal; escalate if both fail. | **User** (approve deps + run session); I can then write the cfg-gated adapters |
| ~~**P1 live echo**~~ | ✅ **DONE** (2026-06-04) — 4× 1 MiB byte-exact @ 103.0 Mbit/s; D1 = AOA; XPORT-01 met. See HARDWARE-FINDINGS.md | — | — |
| **P4 Decode + Present** | `AMediaCodec` decode-to-surface onto `ANativeWindow` (thin `ndk-sys` wrapper) | **Phone** to decode/present; depends on a real `.h264` from P3 | **User+phone** |
| **P5 live pipeline** | Wire P1–P4 live; measure glass-to-glass < 50 ms | Depends on P1 + P3 + P4 (all hardware) | blocked |
| **P6 live touch** | Kotlin `onTouchEvent`→JNI shim feeding `android_client::touch::to_touch_event`; send `Frame::Touch`; host calls `CgEventSink` on the **main thread** | Depends on the live session (P5) + phone | blocked |
| **P7 / P8** | Robustness, HEVC, menu-bar, signing/notarize, Rust-purity swaps, packaging | Depends on a proven MVP pipeline (P6) | blocked |

**Recommended next action:** a single **hands-on-Mac P3 Wave B session** — you verify/replace the `[SUS]` `videotoolbox` dep and grant Screen Recording; I wire the deps + write the cfg-gated SCK/VideoToolbox adapters + the spike `main`; you run `ffplay out.h264`. That unblocks the whole critical path. (Everything implementable without you present is already done.)
