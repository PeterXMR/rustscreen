# RustScreen

Turn a Google Pixel 6a into a wired USB-C secondary monitor for a MacBook Pro M1.

**Status:** Early scaffolding (Phase 0 complete) — not yet functional.

---

## What it does

RustScreen creates a macOS virtual display, hardware-encodes it to H.264, and streams it over a single USB-C cable to a Pixel 6a, which hardware-decodes the frames directly onto the screen. Touch events on the phone travel back over the same link and are injected as mouse events on the Mac. Both the macOS host and the Android client are written in Rust.

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
                                          │  (AOA bulk  OR  NCM/TCP)
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

## "100% Rust"? — the honest version

The goal is **100% Rust *source code*** — not zero non-Rust bytes on the system. Three unavoidable platform boundaries exist where Rust calls the platform rather than a foreign source file being compiled:

| Boundary | Why it is unavoidable | What we write |
|---|---|---|
| Android `NativeActivity` | The OS model is "JVM calls native." The `NativeActivity` glue ships inside Android — no Java/Kotlin to compile. | `android_main()` in Rust + `AndroidManifest.xml` (config, not code). |
| macOS virtual display | The only API to create an arbitrary display is the private `CGVirtualDisplay` ObjC class. | Hand-written `objc2` message-send bindings to private symbols. |
| Android USB accessory fd | Getting the USB file descriptor requires `UsbManager.openAccessory()`. | A few `jni`-crate calls from Rust, returning an `fd` read with `std::fs`. |

The MVP uses the *simplest adapter that works* behind each boundary — a thin Kotlin Activity (~50 LOC of glue, no logic) and a small ObjC++ shim — so that development moves quickly. Every adapter sits behind a Rust trait seam. The P7–P8 phases replace each adapter with a pure-Rust equivalent without touching core logic. Target: ~85% Rust source by end of MVP (P6), ~99% by end of P8.

See §0 and §0.1 of the [architecture roadmap](docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md) for the full rationale.

---

## Repository layout

```
rustscreen/
├── Cargo.toml                        # workspace root (resolver = "2")
├── rust-toolchain.toml               # pins Rust stable
├── crates/
│   ├── protocol/                     # shared pure-Rust framing & messages (no platform deps)
│   ├── macos-host/                   # macOS binary — orchestrates display, capture, encode, USB
│   ├── cg-virtual-display/           # isolated bindings to the private CGVirtualDisplay API
│   └── android-client/               # Android cdylib — JNI entry points, decode, USB, touch
├── android/                          # Gradle project: thin Kotlin shell (D7) + jniLibs
└── docs/
    └── superpowers/plans/            # architecture roadmap and per-phase detailed plans
```

---

## Prerequisites

**All platforms:**
- Rust stable (`rustup` installs it; `rust-toolchain.toml` pins the version)

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

## Building and running

### Mac host

```bash
# Build all workspace crates (Mac-side; Android deps are target-gated and skipped)
cargo build

# Run the tests
cargo test

# Run the host binary (prints version; full pipeline comes in P5)
cargo run -p macos-host
```

### Android native library (.so)

```bash
export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/25.2.9519653"

cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p android-client
```

This places `libandroid_client.so` into `android/app/src/main/jniLibs/arm64-v8a/`.

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

---

## Roadmap

Full detail (decisions, architecture diagram, risks, acceptance criteria per phase) is in
[docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md](docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md).

| Phase | Type | Goal |
|---|---|---|
| **P0** | Build | Workspace + cross-compile + hello-world both platforms *(complete)* |
| **P1** | Spike | USB byte round-trip Mac ↔ Pixel (resolve transport mechanism) |
| **P2** | Spike | Create virtual display from Rust (highest-risk unknown) |
| **P3** | Spike | Capture virtual display + HW-encode to a playable `.h264` file |
| **P4** | Spike | Decode `.h264` on Pixel and render to screen |
| **P5** | Build | Wire full live pipeline; measure glass-to-glass latency |
| **P6** | Build | Touch back-channel and CGEvent injection on Mac |
| **P7** | Build | Robustness, UX, menu-bar app, HEVC, signing, Rust-purity upgrades |
| **P8** | Build | Packaging, distribution, OSS hygiene |

---

## License

TBD (to be finalized in P8; intended open-source).
