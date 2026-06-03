# Requirements: RustScreen

**Defined:** 2026-06-02
**Core Value:** A user plugs a Pixel 6a into an M1 MacBook with one USB-C cable, launches the host app, and within seconds gets a usable, touch-capable extended display with glass-to-glass latency < 50 ms.

Each requirement corresponds to one phase (P0–P8) of the master architecture roadmap (`docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md`). Phase types: 🔬 = de-risking spike (acceptance criteria, expanded to a TDD plan after it succeeds); otherwise a build phase.

## v1 Requirements

### Foundation

- [x] **FOUND-01**: Cargo workspace (`protocol`, `macos-host`, `cg-virtual-display`, `android-client`) cross-compiles to `aarch64-linux-android` via `cargo-ndk`; a thin Kotlin shell loads the Rust `cdylib`; a blank app launches on the Pixel ("hello from Rust" in logcat) and a binary runs on the Mac; GitHub Actions CI is green. *(COMPLETE — PR #1, branch `feat/p0-workspace-scaffold`)*

### Transport

- [ ] **XPORT-01** 🔬: 1 MB echoes correctly in both directions Mac↔Pixel over USB-C, reproducibly across replug, with the chosen mechanism (AOA lead, NCM/TCP fallback), with measured throughput documented (≥ ~200 Mbit/s headroom for 1080p H.264). Resolves D1.

### Virtual Display

- [ ] **VDISP-01** 🔬: A phantom display appears in macOS Displays settings, created entirely from Rust via the private `CGVirtualDisplay` API (ObjC++ `.mm` shim behind a safe `VirtualDisplay` port), and its `CGDirectDisplayID` is obtainable (non-zero, enumerable via `CGGetActiveDisplayList`) for P3 capture. Keystone risk R1.

### Capture & Encode

- [ ] **ENC-01** 🔬: A `.h264` file captured from the virtual display (ScreenCaptureKit; CGDisplayStream fallback) and hardware-encoded via VideoToolbox (realtime/low-latency, no B-frames, zero-copy IOSurface) plays back correctly in ffplay/VLC; SPS/PPS extracted and logged; per-frame encode latency logged.

### Decode & Present

- [ ] **DEC-01** 🔬: Hardware-decoded H.264 renders full-screen on the Pixel via `ANativeWindow` (thin `ndk-sys` `AMediaCodec` wrapper, decode-to-surface per D3) with no Java decode wrapper.

### Live Pipeline

- [ ] **PIPE-01**: A live extended desktop is visible on the Pixel with measured glass-to-glass latency < 50 ms; protocol framing (length-prefixed, TDD), handshake/resolution negotiation, a live frame thread Mac→phone, and `VideoConfig` (SPS/PPS) on connect and on keyframe all work; per-stage timings logged.

### Touch Injection

- [ ] **TOUCH-01**: Tapping/dragging on the Pixel moves and clicks the macOS cursor at the correct location on the virtual display — `AInputEvent` touch captured and normalized on Android, mapped normalized→global CG coords (TDD), injected via `CGEvent`, gated on `AXIsProcessTrusted()` with actionable onboarding.

### Robustness & Purity

- [ ] **ROBUST-01**: The host survives 20 replug cycles (clean teardown, auto-reconnect, virtual display removed on disconnect); HEVC works on the Pixel 6a behind a flag (D2); rotation renegotiation, adaptive bitrate, and a menu-bar app (D5) work; the host is code-signed with hardened runtime and notarized, launching without Gatekeeper warnings.
- [ ] **PURITY-01**: Rust-purity adapter swaps land without changing core logic — Kotlin shell → NativeActivity, ObjC++ shim → pure `objc2`, capture Swift bridge → `objc2-screen-capture-kit`; the Rust purity % metric is updated (~99% target).

### Packaging

- [ ] **PKG-01**: A new user can clone, build both halves, install (notarized `.app`/DMG on macOS, release `.apk` on Android), plug in a Pixel 6a, and get a second screen following only the README; LICENSE/CONTRIBUTING/CI badges and OSS hygiene are in place.

## v2 Requirements

Deferred to future. Tracked but not in the current roadmap.

### Input

- **INPUT-V2-01**: Multitouch and pen/stylus input injection on macOS.

### Devices

- **DEV-V2-01**: Support for Android models beyond the Pixel 6a.

## Out of Scope

| Feature | Reason |
|---------|--------|
| Mac App Store distribution | Private `CGVirtualDisplay` API bars MAS; use notarized DMG. |
| True 0% non-Rust | Impossible — `AndroidManifest.xml` + in-OS NativeActivity glue are irreducible; target is 100% Rust source + 3 FFI boundaries. |
| Display mirroring as primary mode | Product extends the desktop (D6), not mirrors it. |
| HEVC at MVP | H.264 is universal on M1 + Pixel 6a; HEVC is a P7 toggle (D2). |
| Play Store distribution | Optional; not committed for v1. |

## Traceability

Phases execute in agreed (non-numeric) order: **P0 → P2 → P1 → P3 → P4 → P5 → P6 → P7 → P8**. P2 (VDISP-01, keystone risk R1) is front-loaded before P1.

| Requirement | Phase | Status |
|-------------|-------|--------|
| FOUND-01 | P0 | Complete |
| VDISP-01 | P2 | Pending |
| XPORT-01 | P1 | Pending |
| ENC-01 | P3 | Pending |
| DEC-01 | P4 | Pending |
| PIPE-01 | P5 | Pending |
| TOUCH-01 | P6 | Pending |
| ROBUST-01 | P7 | Pending |
| PURITY-01 | P7 | Pending |
| PKG-01 | P8 | Pending |

**Coverage:**
- v1 requirements: 10 total
- Mapped to phases: 10
- Unmapped: 0 ✓

---
*Requirements defined: 2026-06-02*
*Last updated: 2026-06-02 after initial ingest*
