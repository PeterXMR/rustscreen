# Requirements (extracted)

Source document: `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md`
Derived from the phased roadmap (§4, P0-P8). Each phase's "Acceptance" / "Gate to next"
becomes the acceptance criteria. Build order note (from §7): agreed sequencing is
P0 -> P2 -> P1 (front-load the P2 keystone-risk before P1 transport).

---

## REQ-p0-workspace-scaffold
- source: docs/superpowers/plans/...roadmap.md (P0)
- type: Build phase
- status: COMPLETE (delivered as PR #1 — workspace + Kotlin shell + CI)
- description: Cargo workspace with protocol, macos-host, cg-virtual-display, and
  android-client crates; cross-compilation to aarch64-linux-android via cargo-ndk;
  thin Kotlin shell (D7) loading the Rust cdylib; GitHub Actions CI.
- acceptance: A blank app launches on the Pixel (Kotlin shell, Rust core, "hello from
  Rust" in logcat) and a binary runs on the Mac. Both targets build & run; CI green.

## REQ-p1-usb-byte-roundtrip
- source: docs/superpowers/plans/...roadmap.md (P1)
- type: Spike (resolves D1)
- description: Prove bytes move both directions Mac<->Pixel over USB-C. Sub-spike A:
  AOA via nusb + jni UsbManager.openAccessory() (lead). Sub-spike B: network-over-USB
  (NCM/TCP) fallback. Decide D1 and record rationale.
- acceptance: 1 MB echoes correctly both directions over USB-C, reproducibly across
  replug, with the chosen mechanism. Measured throughput documented (need >= ~200
  Mbit/s headroom for 1080p H.264).

## REQ-p2-virtual-display-from-rust
- source: docs/superpowers/plans/...roadmap.md (P2)
- type: Spike (highest risk, R1 keystone)
- description: Create a macOS virtual display entirely from Rust via the private
  CGVirtualDisplay API (ObjC++ .mm shim, simplest-now adapter) behind a safe
  VirtualDisplay Rust port.
- acceptance: A phantom display appears in macOS Displays settings, created from Rust,
  and its CGDirectDisplayID is obtainable (non-zero, enumerable via
  CGGetActiveDisplayList) for P3 capture.

## REQ-p3-capture-and-encode
- source: docs/superpowers/plans/...roadmap.md (P3)
- type: Spike
- description: Capture the virtual display (objc2-screen-capture-kit; CGDisplayStream
  fallback) and hardware-encode to H.264 (videotoolbox, realtime/low-latency, no
  B-frames) zero-copy via IOSurface/CVPixelBuffer.
- acceptance: A .h264 file captured from the virtual display plays back correctly
  (ffplay/VLC). SPS/PPS extracted and logged; per-frame encode latency logged.

## REQ-p4-decode-and-present
- source: docs/superpowers/plans/...roadmap.md (P4)
- type: Spike
- description: Decode H.264 on the Pixel via a thin ndk-sys AMediaCodec wrapper,
  decode-to-surface (D3) onto ANativeWindow from the shell's SurfaceView.
- acceptance: Hardware-decoded H.264 renders full-screen on the Pixel via
  ANativeWindow, with no Java decode wrapper.

## REQ-p5-live-pipeline-and-latency
- source: docs/superpowers/plans/...roadmap.md (P5)
- type: Build (TDD for protocol pieces)
- description: Wire P1-P4 live: protocol framing (length-prefixed, TDD), handshake /
  resolution negotiation, live frame thread Mac->phone, VideoConfig (SPS/PPS) on
  connect and on keyframe, latency harness.
- acceptance: Live extended desktop visible on the Pixel; measured glass-to-glass
  < 50 ms (per-stage timings logged; §2 budget revised with real numbers).

## REQ-p6-touch-backchannel-injection
- source: docs/superpowers/plans/...roadmap.md (P6)
- type: Build (TDD for coordinate mapping)
- description: Capture AInputEvent touch on Android, normalize, send TouchEvent;
  on macOS map normalized->global CG coords and inject via CGEvent
  (mouseMoved/leftMouseDown/leftMouseUp), gated on AXIsProcessTrusted().
- acceptance: Tapping/dragging on the Pixel moves and clicks the macOS cursor at the
  correct location on the virtual display.

## REQ-p7-robustness-ux-purity
- source: docs/superpowers/plans/...roadmap.md (P7)
- type: Build
- description: Robustness/UX + Rust-purity adapter swaps: Kotlin shell -> NativeActivity;
  ObjC++ shim -> pure objc2; capture Swift bridge -> objc2-screen-capture-kit; hotplug/
  reconnect; rotation renegotiation; HEVC toggle (D2); adaptive bitrate; menu-bar app
  (D5); code-signing + hardened runtime + notarization.
- acceptance: Survives 20 replug cycles; HEVC works on Pixel 6a; notarized host
  launches without Gatekeeper warnings.

## REQ-p8-packaging-distribution
- source: docs/superpowers/plans/...roadmap.md (P8)
- type: Build
- description: macOS .app bundle (signed, notarized, DMG); Android release .apk
  (cargo apk build --release, signing); README/LICENSE/CONTRIBUTING, CI badges, OSS hygiene.
- acceptance: A new user can clone, build both halves, install, plug in a Pixel 6a,
  and get a second screen following only the README.
