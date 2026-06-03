# Roadmap: RustScreen

## Overview

RustScreen de-risks first, then builds. The journey: scaffold the workspace (P0, done), then immediately attack the three scary unknowns as spikes — create a virtual display from Rust (P2, the keystone), move bytes over USB both ways (P1), capture+encode on macOS (P3), decode+present on the Pixel (P4) — before wiring them into a live sub-50 ms pipeline (P5), adding the touch back-channel (P6), and finally hardening, raising Rust purity, and packaging for distribution (P7, P8). The authoritative design source (full acceptance criteria, seam table, risk register) is `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md`; this ROADMAP.md is the execution tracker.

## Phases

**Phase Numbering:** Phases use the source roadmap's P0–P8 labels (not 1–N). Spikes are marked 🔬.

**Execution Order (agreed, non-numeric):** P0 → **P2** → P1 → P3 → P4 → P5 → P6 → P7 → P8. P2 is front-loaded ahead of P1 because it is the keystone risk (R1): if creating a virtual display from Rust fails, the whole architecture changes, so it is attacked first.

- [x] **Phase P0: Workspace Scaffold & Cross-Compilation** - Cargo workspace, cross-compile, thin Kotlin shell, CI (COMPLETE — PR #1)
- [x] **Phase P2: Create a Virtual Display from Rust** 🔬 - Keystone risk R1 RETIRED; phantom display via private `CGVirtualDisplay` (COMPLETE — branch `feat/p2-virtual-display`)
- [ ] **Phase P1: USB Byte Round-Trip** 🔬 - ⚠ DEFERRED (hardware-blocked: needs Pixel 6a on USB). 1 MB echoes both directions; resolves D1 (AOA vs NCM)
- [ ] **Phase P3: Capture + Hardware-Encode on macOS** 🔬 - Capture virtual display, VideoToolbox H.264 to a playable file (NEXT — Mac-only, unblocked)
- [ ] **Phase P4: Decode + Present on the Pixel** 🔬 - ⚠ DEFERRED (hardware-blocked: needs Pixel 6a). AMediaCodec decode-to-surface onto `ANativeWindow`
- [ ] **Phase P5: Live End-to-End Pipeline + Latency** - Wire P1–P4 live; measure glass-to-glass < 50 ms (cable-free protocol slice planned)
- [ ] **Phase P6: Touch Back-Channel → macOS Injection** - Tap on phone moves/clicks the Mac cursor via `CGEvent`
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
**Plans**: TBD

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
- [ ] P3-01-PLAN.md — AVCC→Annex-B + capture-select (TDD, CI-green) then SCK/VideoToolbox/CGDisplayStream adapters + spike main + ffplay verify (hands-on-Mac)
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
**Plans**: TBD
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
| P0. Workspace Scaffold | — (PR #1) | Complete | 2026-06-02 |
| P2. Virtual Display (R1 keystone) 🔬 | spike ✓ | Complete | 2026-06-03 |
| P1. USB Byte Round-Trip 🔬 | 0/TBD | ⚠ Deferred (needs phone) | - |
| P3. Capture + Encode 🔬 | 0/1 | Planned (Mac-only, next) | - |
| P4. Decode + Present 🔬 | 0/TBD | ⚠ Deferred (needs phone) | - |
| P5. Live Pipeline + Latency | 0/1 cable-free slice | Cable-free slice planned (live wiring blocked on P1/P4) | - |
| P6. Touch Injection | 0/TBD | Blocked on P5 | - |
| P7. Robustness + Purity | 0/TBD | Not started | - |
| P8. Packaging + Distribution | 0/TBD | Not started | - |
