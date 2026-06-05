# RustScreen

**Turn a Google Pixel 6a into a wired USB-C, touch-capable second monitor for an M1 MacBook — both halves written in Rust.**

[![CI](https://github.com/PeterXMR/rustscreen/actions/workflows/ci.yml/badge.svg)](https://github.com/PeterXMR/rustscreen/actions/workflows/ci.yml)
[![License: not finalized](https://img.shields.io/badge/license-not%20finalized-orange.svg)](#license)

> **Status: live pipeline works.** The Mac's extended desktop renders on the Pixel 6a
> over USB, with glass-to-glass latency instrumented and tuned (best ~80 ms p50; target
> < 50 ms). What's left to make it a daily tool is three steps — see
> [Status & roadmap](#status--roadmap) below.

---

## What it does

RustScreen creates a macOS virtual display, hardware-encodes it to H.264, and streams
it over a single USB-C cable to a Pixel 6a, which hardware-decodes the frames directly
onto its native window surface. Touch events on the phone travel back over the same
link and are injected as mouse events on the Mac. Both the macOS host and the Android
client are written in Rust, organized as one Cargo workspace. The goal is a usable,
touch-capable extended display with glass-to-glass latency under 50 ms.

High-level data flow:

```
┌──────────────────────── MacBook Pro M1 (host binary) ─────────────────────────┐
│                                                                                │
│  CGVirtualDisplay (private)      ScreenCaptureKit            VideoToolbox      │
│  create 2400x1080@60  ─────►  capture virtual display  ──►  HW H.264 encode   │
│  [objc2 hand-binding]          [objc2-screen-capture-kit]   [videotoolbox]     │
│                                       │ IOSurface (zero-copy)                  │
│                                       ▼                                        │
│                              frame framer (protocol) ──► USB TX (nusb)         │
│                                                                                │
│  CGEvent inject ◄── coord map ◄── touch decode ◄──────────  USB RX            │
│  [objc2-core-graphics]            [protocol]                                   │
└────────────────────────────────────────┬───────────────────────────────────────┘
                                          │  USB-C ↔ USB-C
                                          │  (AOA bulk; NCM/TCP documented fallback)
┌─────────────────────────────────────────▼─────────────── Pixel 6a (cdylib) ───┐
│                                                                                │
│  USB RX (fd→std::fs / nusb) ──► deframer (protocol) ──► AMediaCodec decode   │
│                                                          [ndk-sys libmediandk] │
│                                                                │ render-to-    │
│                                                                ▼ surface       │
│  android_main (NativeActivity)  ◄── ANativeWindow ◄── decoded frames on screen│
│        │ AInputEvent (touch)                                                   │
│        ▼                                                                       │
│  touch encode (protocol) ──► USB TX                                            │
└────────────────────────────────────────────────────────────────────────────────┘
```

---

## Architecture

### Workspace crates

RustScreen is a single Cargo workspace (`resolver = "2"`):

```
rustscreen/
├── Cargo.toml                        # workspace root (resolver = "2")
├── rust-toolchain.toml               # pins Rust stable
├── crates/
│   ├── protocol/                     # shared pure-Rust framing & messages (no platform deps)
│   ├── macos-host/                   # macOS binary + lib — display, capture, encode, USB, inject
│   ├── cg-virtual-display/           # isolated bindings to the private CGVirtualDisplay API
│   └── android-client/               # Android cdylib — JNI entry points, decode, USB, touch
├── android/                          # Gradle project: thin Kotlin shell + jniLibs
└── docs/
    └── superpowers/plans/            # architecture roadmap and per-phase detailed plans
```

| Crate | Role |
|---|---|
| `protocol` | Pure-Rust shared core — length-prefixed frame codec, control/touch/video messages, coordinate mapping. No platform dependencies; reused verbatim by both halves so the wire format stays in sync. |
| `macos-host` | The Mac host binary + library. Orchestrates the virtual display, capture, encode, USB, and cursor injection. |
| `android-client` | The Android client `cdylib` (`crate-type = ["cdylib"]`); cross-compiles to `aarch64-linux-android`. |
| `cg-virtual-display` | Quarantines the **only** private/undocumented surface (the `CGVirtualDisplay` ObjC class) in its own crate, so a macOS update that breaks it has a one-crate blast radius. |

### Ports & adapters — "simplest-now, replaceable-later"

Every platform boundary sits behind a **port** (a Rust trait or stable FFI seam); the
core logic depends only on the port, never on the platform. The MVP ships the
**simplest adapter that works now** behind each port and swaps it for a purer one
later **without touching core logic**. Examples of ports already in the tree:
`Transport` (move bytes), `Capturer` / `Encoder` (frames in, NAL units out),
`PointerSink` (inject a pointer action), `VirtualDisplay` (create a display).

### "100% Rust source" — the honest version

The goal is **100% Rust *source code*** — **not** zero non-Rust bytes on the system.
That is impossible: the OS-supplied `AndroidManifest.xml` and Google's in-OS
`NativeActivity` glue are irreducible. There are three unavoidable platform
boundaries where Rust calls the platform, and each sits behind a port rather than
being a foreign source file we compile:

| Boundary | Why it is unavoidable | What we write |
|---|---|---|
| Android entry point | The OS model is "JVM calls native." | **MVP today:** a thin Kotlin `MainActivity` (glue only, no logic) loads the Rust `cdylib`. **P7 target:** swap to `NativeActivity` so the entry point is `android_main()` in Rust, leaving only `AndroidManifest.xml` (config, not code). |
| macOS virtual display | The only API to create an arbitrary display is the private `CGVirtualDisplay` ObjC class. | Hand-written `objc2` message-send bindings to private symbols, isolated in `cg-virtual-display`. |
| Android USB-accessory fd | Getting the USB file descriptor requires `UsbManager.openAccessory()`. | A few `jni`-crate calls from Rust, returning an `fd` read with `std::fs`. |

For the MVP the adapter behind a boundary may be a **thin shim** (a small Kotlin
Activity — glue only, no logic; an ObjC++ shim). These shims sit behind the same
Rust ports and are scheduled for replacement with pure-Rust adapters in later phases,
which is why the Rust-purity figure is a *living metric* (~85% Rust source targeted at
MVP, ~99% later) rather than a fixed claim. See §0 / §0.1 of the
[architecture roadmap](docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md)
for the full rationale and seam table.

---

## Prerequisites

**All platforms:**
- Rust stable (`rustup` installs it; `rust-toolchain.toml` pins the channel). MSRV is **1.80**.

**macOS host (display capture) only:**
- The **Screen & System Audio Recording** permission, granted to the terminal you run the
  capture spikes from (any terminal app). See [macOS capture spikes](#macos-capture-spikes--requires-the-screen--system-audio-recording-permission).

**Android native library only:**
- Android NDK r25 (component `25.2.9519653`)
- `cargo-ndk`: `cargo install cargo-ndk`
- Add the Android target: `rustup target add aarch64-linux-android`

**Android APK (Gradle) only:**
- JDK 17–21 (AGP 8.5.2 requires this range; JDK 22+ causes Gradle to fail)
- If your system JDK is newer (e.g. Java 25), point Gradle at a 17–21 JDK without touching the
  committed repo files. Add this to your personal `~/.gradle/gradle.properties`:
  ```
  org.gradle.java.home=/path/to/your/jdk-21
  ```

---

## Build & run

The **default** `cargo build --workspace` is fully cross-platform and pulls **no**
macOS-only or hardware-only crates — all platform integration is behind off-by-default
`live-*` features (see [Feature flags](#feature-flags)). This keeps the build and CI
green on any host and lets you build/test all the pure-Rust logic without a Mac or a
phone.

### macOS host

```bash
# Default build — cross-platform; Android deps are target-gated and skipped
cargo build --workspace

# Run the tests
cargo test --workspace

# Run the host binary (the live streaming host; launch from a terminal holding the Screen Recording grant)
cargo run -p macos-host
```

#### macOS capture spikes — requires the **Screen & System Audio Recording** permission

The display-capture/encode tools are built behind the off-by-default `live-capture` feature:

```bash
cargo build -p macos-host --features live-capture   # builds p3_probe + p3_capture + p3_encode
target/debug/p3_probe      # is the virtual display visible to ScreenCaptureKit?
target/debug/p3_capture    # does ScreenCaptureKit deliver frames from it?
target/debug/p3_encode     # capture → VideoToolbox H.264 → out.h264 (play in ffplay/VLC)
```

**Required permission:** macOS gates *all* display-pixel access (ScreenCaptureKit) behind
**System Settings → Privacy & Security → Screen & System Audio Recording**. Without it these
tools fail with a TCC error / empty display list. This is unavoidable for any screen-capture or
second-monitor app (DisplayLink, Duet, etc. all need it); there is no third-party way around it
on current macOS. The app only ever captures the *virtual* display it creates — never your real
screen — but macOS's permission is coarse (one toggle for any display capture).

- Grant it to **the terminal app you run the binary from** — *any* terminal works (Terminal.app,
  iTerm, Warp, …). Nothing in the code is terminal-specific; macOS attributes the grant to
  whichever process launches the binary.
- **The grant only takes effect after you fully quit (⌘Q) and reopen that terminal.**
- In the shipped product this permission is requested by `RustScreen.app` itself (once), not a
  terminal.

### Android native library (.so)

```bash
export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/25.2.9519653"

cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p android-client --features live-decode
```

This places `libandroid_client.so` into `android/app/src/main/jniLibs/arm64-v8a/`.
`--features live-decode` is required: it compiles the `AMediaCodec` decode-to-surface
adapter and the `nativeOnSurface` JNI entry the Kotlin shell calls. Building without it
produces a `.so` missing those symbols, so the app crashes on launch with
`UnsatisfiedLinkError`.

### Android APK

```bash
cd android
./gradlew assembleDebug
```

The debug APK is written to `android/app/build/outputs/apk/debug/app-debug.apk`.
Install on a Pixel 6a in developer mode:
```bash
adb install android/app/build/outputs/apk/debug/app-debug.apk
```
`adb logcat` should show `hello from Rust` on launch.

### Feature flags

All live/hardware paths are off-by-default `live-*` features on `macos-host`, so the
default build stays cross-platform:

| Feature | Pulls | What it builds |
|---|---|---|
| `live-usb` | `nusb` | Live AOA USB host path + `p1_echo`. `nusb` is pure-Rust/cross-platform, so this *builds* anywhere but is only *run* on macOS. |
| `live-inject` | `core-graphics` | Live macOS cursor injection (`touch::CgEventSink`) + `p6_inject`. macOS-only. |
| `live-capture` | the `objc2` Apple-FFI family + `cg-virtual-display` | Live capture + VideoToolbox H.264 encode (`p3_probe` / `p3_capture` / `p3_encode`). macOS-only. |

Do not enable `live-inject` / `live-capture` in a `--all-features` build on a
non-macOS host — the macOS-only crates won't compile there.

---

## Packaging & distribution

> **Scaffold only.** The packaging path is wired and lint-clean, but it is **not yet
> validated end-to-end** (no Apple Developer signing identity / Android release
> keystore has been run through it), and because the live pipeline isn't finished, a
> packaged app launches but does **not** yet produce a second screen. The scripts
> exist so distribution is ready the moment the pipeline lands.

A release build uses a size-optimized, LTO'd, stripped profile
(`[profile.release]` in the workspace [`Cargo.toml`](Cargo.toml)).

### macOS — `.app` bundle + notarized DMG

Scripts live in [`packaging/macos/`](packaging/macos/) (see its
[README](packaging/macos/README.md) for the full flow and credentials setup):

```bash
packaging/macos/make_app.sh --features live-capture,live-usb,live-inject  # → dist/RustScreen.app
SIGN_IDENTITY="Developer ID Application: … (TEAMID)" NOTARY_PROFILE="rustscreen-notary" \
  packaging/macos/sign_and_notarize.sh                                     # sign (hardened runtime) + notarize + staple
packaging/macos/make_dmg.sh                                                # → dist/RustScreen-<version>.dmg
```

No secrets are committed — the signing identity and notary credentials are supplied
at run time via env vars / a keychain profile.

- **Distribution is via a notarized DMG, not the Mac App Store**: the virtual display
  uses the **private** `CGVirtualDisplay` API, which MAS forbids. Notarization checks
  signing + malware (not private-API use), so a signed binary is expected to notarize
  (risk register R8).
- **Open question — the hardened-runtime entitlement set.**
  [`packaging/macos/entitlements.plist`](packaging/macos/entitlements.plist) is a
  documented *first guess* to be pinned down empirically once the host runs under the
  hardened runtime (does the private `CGVirtualDisplay` symbol resolve?). Screen
  recording and Accessibility are runtime **TCC permissions**, not entitlements.

### Android — release APK + signing

Scripts live in [`packaging/android/`](packaging/android/) (see its
[README](packaging/android/README.md)):

```bash
packaging/android/build_release_apk.sh   # release .so via cargo-ndk → assembleRelease
```

Release signing reads credentials from environment variables (CI) or a git-ignored
`android/keystore.properties` (see
[`keystore.properties.example`](packaging/android/keystore.properties.example)); with
neither configured it falls back to the debug key (smoke-test only, never
distribute). No keystore or password is committed.

The **USB-accessory permission** is already declared in the manifest
(`<uses-feature android:hardware.usb.accessory required="true">` + the
`USB_ACCESSORY_ATTACHED` intent-filter). There is no install-time `<uses-permission>`
to add — Android grants access through a runtime per-connection dialog; tick "use by
default" to suppress it on reconnect. Details in the
[Android packaging README](packaging/android/README.md).

---

## Status & roadmap

The live end-to-end pipeline **works**: the Mac's extended desktop renders on the
Pixel 6a over a single USB-C cable, with glass-to-glass latency instrumented and tuned
(best ~80 ms p50 measured on device; target < 50 ms). What remains to make it a
daily-usable tool is **three steps, in priority order:**

1. **`rustscreen` terminal app** — install once, then `rustscreen start` runs the host
   locally and auto-streams the moment the phone app is open; `rustscreen stop` ends it.
2. **Automatic reconnect** — close and reopen the phone app, restart the host, or unplug
   and replug the cable, and the connection re-establishes itself (handshake) with no
   manual restart, as long as the app is running on both sides.
3. **Native-feel latency** — drive glass-to-glass below 50 ms so moving the mouse or a
   window on the Mac appears on the phone with no perceptible lag.

Full detail and per-step sub-tasks are in the execution tracker
[`.planning/ROADMAP.md`](.planning/ROADMAP.md) (the "Active Priorities" section).

---

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to build and
test both halves, the mandatory conventions (strict TDD, `clippy -D warnings`,
`cargo fmt --check`, MSRV 1.80), the off-by-default `live-*` feature convention, and
the ports-and-adapters model.

---

## License

**Not yet finalized.** See [LICENSE](LICENSE).

The project's license has not been written yet. Until it is, **all rights are reserved
by default** — please contact the author before copying, modifying, or redistributing.

The author intends a license as compatible with **Voluntaryism** as possible: similar
in spirit to the GPL but **without mandatory source disclosure** (you may keep source
private), while you **may not prevent others from copying, inspecting, and modifying
the binary** unless they agreed not to in a written private contract. Modification,
redistribution, and commercial use are intended to be permitted. This is a statement of
intent only and confers no rights until the license is published.
