# Constraints (extracted)

Source document: `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md`
SPEC-like technical constraints embedded in the DOC (§0, §0.1, §2, §3, §5).

---

## C-ffi-boundaries — Three unavoidable platform boundaries (honest scope)
- source: docs/superpowers/plans/...roadmap.md (§0)
- type: protocol / platform-boundary
- content: True 0% non-Rust is impossible. Achievable target is 100% Rust *source code*
  with exactly three platform boundaries that are Rust-calling-platform-APIs, not
  foreign source files:
  1. Android NativeActivity — OS model is "JVM calls into native"; glue ships inside
     Android. We write android_main() in Rust + an AndroidManifest.xml (config, not code).
  2. macOS virtual display — only API is the private CGVirtualDisplay ObjC class; no
     crate covers it. Hand-written objc2 message-send bindings to private symbols.
  3. Android USB accessory FD — requires UsbManager.openAccessory(); a few jni-crate
     calls from Rust return an fd read via std::fs.

## C-ports-and-adapters — Replaceability strategy (the core never changes on adapter swap)
- source: docs/superpowers/plans/...roadmap.md (§0.1)
- type: nfr / architecture
- content: Each boundary is a port (Rust trait or stable FFI surface); adapters start
  simple and become more-Rust over time. Seam table:
  - macOS virtual display: port `cg-virtual-display::VirtualDisplay`; MVP = ~40-line
    ObjC++ .mm shim via cc crate (extern "C"); pure-Rust target = objc2 msg-sends.
  - Android shell: stable Rust FFI (Surface/ANativeWindow, USB fd, raw touch); MVP =
    thin Kotlin Activity ~50 LOC (glue only, all logic in Rust cdylib); target =
    NativeActivity (0 Kotlin) + jni for UsbManager.
  - macOS capture: `Capturer` trait; MVP = screencapturekit-rs (Swift bridge);
    target = objc2-screen-capture-kit (no bridge).
  - macOS encode: `Encoder` trait; videotoolbox crate (already Rust source).
  - Android decode: `Decoder` trait; ndk-sys AMediaCodec (already Rust source).
  - Touch injection (macOS): inject.rs over objc2-core-graphics (already Rust source).
  Living metric: MVP end-P6 ~85% Rust source; stretch end-P8 ~99% (only
  AndroidManifest.xml + Google's in-OS NativeActivity glue remain non-Rust).

## C-protocol-wire-format — Length-prefixed frame codec
- source: docs/superpowers/plans/...roadmap.md (P5.1)
- type: api-contract / wire-format
- content: Wire format `u8 tag · u32 len · payload`. Frame enum:
  Handshake{..}, VideoConfig{ codec, sps_pps: Vec<u8> },
  Video{ pts_us: u64, keyframe: bool, nal: Vec<u8> }, Touch(TouchEvent), Control(Control).
  Control/Touch/Handshake payloads via postcard; Video payload written raw (no serde
  over large buffers — perf). Lives in protocol crate (pure Rust, no_std-friendly,
  no platform deps).

## C-workspace-structure — Cargo workspace layout
- source: docs/superpowers/plans/...roadmap.md (§3)
- type: schema / project-structure
- content: Single Cargo workspace (resolver "2"). Members:
  crates/protocol (shared, pure Rust: framing.rs, messages.rs, coords.rs),
  crates/macos-host (bin: main, virtual_display, capture, encode, transport, inject),
  crates/cg-virtual-display (isolated hand-bindings to PRIVATE CoreGraphics; build.rs),
  crates/android-client (cdylib: lib, decode, transport, usb_jni, input;
  AndroidManifest.xml; cargo-apk metadata). cg-virtual-display is a separate crate to
  quarantine the only private/undocumented API surface (one-crate blast radius).

## C-latency-budget — Glass-to-glass target (to validate in P5)
- source: docs/superpowers/plans/...roadmap.md (§2)
- type: nfr / performance
- content: Per-stage target budget: capture <=2 ms, encode <=8 ms, USB <=3 ms,
  decode <=8 ms, present <=16 ms (1 vsync) -> target glass-to-glass < 50 ms.
  UNMEASURED in research (open question); P5 must prove it.

## C-target-architecture — Data flow / pipeline topology
- source: docs/superpowers/plans/...roadmap.md (§2)
- type: protocol / architecture
- content: Host: CGVirtualDisplay (private, 2400x1080@60) -> ScreenCaptureKit capture
  -> VideoToolbox HW H.264 encode (IOSurface zero-copy) -> protocol framer -> USB TX
  (nusb or socket). Return path: USB RX -> touch decode -> coord map -> CGEvent inject.
  Client (Pixel cdylib): USB RX (fd->std::fs / nusb) -> protocol deframer ->
  AMediaCodec decode (ndk-sys) -> render-to-surface (ANativeWindow); android_main
  (NativeActivity) -> AInputEvent touch -> touch encode -> USB TX. Link is USB-C<->USB-C
  (AOA bulk OR NCM/TCP).

## C-risk-register — Project risk register
- source: docs/superpowers/plans/...roadmap.md (§5)
- type: nfr / risk
- content:
  - R1 CGVirtualDisplay private API unbindable/broken from Rust — Med likelihood /
    Critical impact — isolate in cg-virtual-display crate; spike first (P2); DriverKit
    fallback. (P2)
  - R2 macOS NCM absent -> tethering transport dead — Med/Med — AOA is lead path
    anyway (P1 dual-spike). (P1)
  - R3 videotoolbox / objc2-screen-capture-kit experimental churn — Med/Med — wrap
    behind own traits; pin versions; CGDisplayStream fallback. (P3)
  - R4 rust_mediacodec stale -> must own decoder — High/Low — vendor a thin ndk-sys
    wrapper from the start. (P4)
  - R5 Latency budget unmet (>50 ms) — Med/High — decode-to-surface (D3), low-latency
    encode, jitter tuning, measure early (P5.4). (P5)
  - R6 Accessibility permission friction for injection — High/Low — clear onboarding +
    AXIsProcessTrusted gating. (P6)
  - R7 AOA accessory permission dialog per-connect — Med/Low — "remember" intent flag;
    document UX. (P1/P7)
  - R8 Notarization rejects private-API binary — Low/Med — notarization checks signing/
    malware, not API use; test early. (P7)
