# RustScreen — Architecture & Implementation Roadmap

> **For agentic workers:** This is the master architecture TODO. Each **Phase** below is its own deliverable. Spike phases (P1–P4) are de-risking experiments with *acceptance criteria* rather than full TDD steps — once a spike succeeds, it gets converted into a detailed bite-sized plan via `superpowers:writing-plans` before execution. Build phases (P0, P5–P8) use checkbox (`- [ ]`) tracking. Implement with `superpowers:subagent-driven-development`.

**Goal:** Turn a Google Pixel 6a into a wired secondary monitor for a MacBook Pro M1 over a single USB-C↔USB-C cable, with both the macOS host and the Android client written in Rust.

**Architecture:** A Cargo workspace with three crates — a macOS host binary, an Android client `cdylib`, and a shared `protocol` crate. The Mac creates a *virtual display* (private CoreGraphics API), captures + hardware-encodes it (ScreenCaptureKit + VideoToolbox), streams length-prefixed H.264 NAL units over USB to the phone, which hardware-decodes them straight onto its native window surface (NDK MediaCodec → ANativeWindow). Touch events travel back over the same link and are injected on macOS as mouse/pen events (CGEvent).

**Tech Stack:** Rust 1.85+ · `objc2` family · `videotoolbox` · `nusb` · `android-activity` (NativeActivity) · `ndk` / `ndk-sys` (MediaCodec) · `wgpu` (optional overlay path) · `postcard` (control messages) · H.264 (MVP) / HEVC (later).

---

## 0. Honest Scope: What "100% Rust" Means Here

This is the single most important thing to agree on before implementation. The research (28 sources, 23 verified claims) confirms **true 0% non-Rust is impossible**; the achievable target is **100% Rust *source code*** with three platform boundaries that are *Rust calling platform APIs*, not foreign source files:

| Boundary | Why it's unavoidable | What we write |
|---|---|---|
| Android `NativeActivity` | The OS model is "JVM calls into native," not vice-versa. `NativeActivity` glue ships *inside Android* — no Java/Kotlin to compile. | `android_main()` in Rust + an `AndroidManifest.xml` (config, not code). |
| macOS virtual display | The only API to create an arbitrary display is the **private** `CGVirtualDisplay` ObjC class. No crate covers it. | Hand-written `objc2` message-send bindings to private symbols. |
| Android USB accessory FD | Getting the USB accessory file descriptor requires `UsbManager.openAccessory()`. | A few `jni`-crate calls from Rust → returns an `fd` we read with `std::fs`. |

**D0 — DECIDED (simplest-now, shrink-to-Rust-later):** We do **not** chase maximum Rust purity up front. Instead we put every platform boundary behind a Rust trait/FFI seam, ship the *simplest adapter that works* for the MVP (even if it's Kotlin or an ObjC++ shim), and replace adapters with pure-Rust ones over P7–P8 **without touching core logic**. See §0.1.

## 0.1 Replaceability Strategy (Ports & Adapters)

Principle: **the Rust core never changes when we swap an adapter.** Each boundary is a *port* (a Rust trait or stable FFI surface); the *adapter* behind it starts simple and gets more-Rust over time. We track a **Rust purity %** and drive it up — but only at seams where a swap is actually likely (YAGNI: no ceremony around already-pure crates).

| Boundary | Port (seam) | Simplest-now adapter (MVP) | Pure-Rust target (P7–P8) | Swap cost |
|---|---|---|---|---|
| macOS virtual display | `cg-virtual-display::VirtualDisplay` safe API | ~40-line **ObjC++ `.mm` shim** via `cc` crate, `extern "C"` fns | Pure `objc2` msg-sends to private symbols | Low — one crate, callers unaffected |
| Android shell (window + permissions + USB fd + touch) | Stable Rust FFI: shell passes in `Surface`/`ANativeWindow`, USB `fd`, raw touch | Thin **Kotlin Activity (~50 LOC)** — glue only; **all logic in Rust `cdylib`** | **NativeActivity** (0 Kotlin) + `jni` for `UsbManager` | Low — shell swap, Rust core identical |
| macOS capture | `Capturer` trait | `screencapturekit-rs` (small Swift bridge) | `objc2-screen-capture-kit` (no bridge) | Low |
| macOS encode | `Encoder` trait | `videotoolbox` crate — *already Rust source* | (already there) | n/a |
| Android decode | `Decoder` trait | `ndk-sys` `AMediaCodec` — *already Rust source* | (already there) | n/a |
| Touch injection (macOS) | `inject.rs` over `objc2-core-graphics` | already Rust source | (already there) | n/a |

**Consequence for sequencing:** because the Rust core (protocol, decode, transport, encode, capture-orchestration, input mapping) is shell-agnostic, the MVP can use a Kotlin shell + ObjC++ shim to move fast, and the "make it 100% Rust source" work in P7–P8 becomes a contained, low-risk follow-up rather than a rewrite.

**Living metric — Rust purity (update each phase):**
- MVP target (end P6): ~85% Rust source (Kotlin shell + ObjC++ shim + Swift bridge are the non-Rust deltas).
- Stretch target (end P8): ~99% Rust source (only `AndroidManifest.xml` + Google's in-OS NativeActivity glue remain).

---

## 1. Decisions (✅ locked from review; others have safe defaults)

| ID | Decision | Choice | Affects |
|---|---|---|---|
| **D0** | ✅ "100% Rust" meaning | **Simplest-now, shrink-to-Rust-later** via ports & adapters (§0.1). MVP may use Kotlin shell + ObjC++ shim; pure-Rust by P8. | whole project |
| **D1** | ✅ USB transport | **Spike both AOA + network-over-USB in P1, lead with AOA.** Pick on measured throughput/reliability. | P1, transport |
| **D2** | ✅ Video codec | **H.264** for MVP (universal HW support M1 + Pixel 6a). HEVC = later toggle (P7). | P3, P4, protocol |
| **D3** | ✅ Android render path | **Decode-to-surface** (MediaCodec → `ANativeWindow`, no GPU round-trip). `wgpu` sampling only if overlays needed later. | P4, render |
| **D4** | Touch scope (default) | **Mouse emulation (single pointer)** via `CGEvent`; multitouch + pen post-MVP. | P6 |
| **D5** | Mac app shape (default) | **CLI for spikes**, **menu-bar status item** for the shippable app. | P7, P8 |
| **D6** | Display geometry (default) | **Extend** at **2400×1080 @ 60 Hz** (Pixel 6a native, landscape); HiDPI 2× optional. | P2, P5 |
| **D7** | Android MVP shell (from D0) | **Thin Kotlin Activity** (glue only) for the MVP → swap to NativeActivity in P7. | P0, P4, P6 |

---

## 2. Target Architecture

```
┌──────────────────────── MacBook Pro M1 (host binary) ─────────────────────────┐
│                                                                                │
│  CGVirtualDisplay (private)      ScreenCaptureKit            VideoToolbox       │
│  create 2400x1080@60  ─────►  capture virtual display  ──►  HW H.264 encode     │
│  [objc2 hand-binding]          [objc2-screen-capture-kit]   [videotoolbox]      │
│                                       │ IOSurface (zero-copy)                   │
│                                       ▼                                         │
│                              frame framer (protocol) ──► USB TX (nusb / socket) │
│                                                                                 │
│  CGEvent inject ◄── coord map ◄── touch decode ◄──────────  USB RX             │
│  [objc2-core-graphics]            [protocol]                                    │
└────────────────────────────────────────┬───────────────────────────────────────┘
                                          │  USB-C ↔ USB-C
                                          │  (AOA bulk  OR  NCM/TCP)
┌─────────────────────────────────────────▼─────────────── Pixel 6a (cdylib) ───┐
│                                                                                │
│  USB RX (fd→std::fs / nusb) ──► deframer (protocol) ──► AMediaCodec decode      │
│                                                          [ndk-sys libmediandk]  │
│                                                                │ render-to-     │
│                                                                ▼ surface        │
│  android_main (NativeActivity)  ◄── ANativeWindow ◄── decoded frames on screen  │
│        │ AInputEvent (touch)                                                    │
│        ▼                                                                        │
│  touch encode (protocol) ──► USB TX                                             │
└────────────────────────────────────────────────────────────────────────────────┘
```

**Data flow latency budget (target, to validate in P5):** capture ≤2 ms · encode ≤8 ms · USB ≤3 ms · decode ≤8 ms · present ≤16 ms (1 vsync) → **target glass-to-glass < 50 ms**. This is unmeasured in research (open question) and P5 must prove it.

---

## 3. Workspace / File Structure

A single Cargo workspace. Files that change together live together; each crate has one responsibility.

```
rustscreen/
├── Cargo.toml                      # [workspace] members
├── README.md
├── LICENSE                         # (P8) MIT or Apache-2.0 — confirm
├── docs/
│   └── superpowers/plans/          # this file + per-phase detailed plans
├── crates/
│   ├── protocol/                   # SHARED — pure Rust, no_std-friendly, no platform deps
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── framing.rs          # length-prefixed frame read/write
│   │       ├── messages.rs         # Handshake, VideoConfig, VideoFrame, TouchEvent, Control
│   │       └── coords.rs           # normalized↔display coordinate mapping (pure, unit-tested)
│   │
│   ├── macos-host/                 # macOS binary
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs             # CLI/menu-bar entry, wires pipeline
│   │       ├── virtual_display.rs  # uses cg-virtual-display, lifecycle mgmt
│   │       ├── capture.rs          # ScreenCaptureKit of the virtual display
│   │       ├── encode.rs           # VideoToolbox H.264, IOSurface zero-copy
│   │       ├── transport.rs        # USB TX/RX (impl chosen in D1)
│   │       └── inject.rs           # CGEvent mouse/pen injection + Accessibility check
│   │
│   ├── cg-virtual-display/         # macOS — isolated hand-bindings to PRIVATE CoreGraphics
│   │   ├── Cargo.toml
│   │   ├── build.rs                # link CoreGraphics
│   │   └── src/lib.rs              # CGVirtualDisplay/Descriptor/Settings/Mode via objc2
│   │
│   └── android-client/             # Android cdylib (crate-type = ["cdylib"])
│       ├── Cargo.toml
│       ├── src/
│       │   ├── lib.rs              # #[no_mangle] android_main via android-activity
│       │   ├── decode.rs          # AMediaCodec wrapper (ndk-sys), decode-to-surface
│       │   ├── transport.rs       # USB RX/TX (accessory fd or socket)
│       │   ├── usb_jni.rs         # jni calls: UsbManager.openAccessory() → fd
│       │   └── input.rs           # AInputEvent touch → protocol::TouchEvent
│       ├── AndroidManifest.xml     # config: NativeActivity, USB-accessory intent filter
│       └── cargo-apk metadata (in Cargo.toml [package.metadata.android])
```

**Why a separate `cg-virtual-display` crate:** it quarantines the only *private/undocumented* API surface in the whole project. If a macOS update breaks `CGVirtualDisplay`, the blast radius is one crate with one job, and it can be feature-gated/version-shimmed in isolation.

---

## 4. Phased Roadmap (de-risk first, then build)

Ordering principle: **prove the three scary unknowns before building anything that depends on them.** The unknowns are (1) creating a virtual display from Rust, (2) moving bytes over USB from Rust on both ends, (3) the VideoToolbox→MediaCodec codec handshake. Everything else is conventional Rust.

| Phase | Type | Goal | Gate to next |
|---|---|---|---|
| **P0** | Build | Workspace + cross-compile + hello-world both platforms | Both targets build & run |
| **P1** | 🔬 Spike | USB byte round-trip Mac↔Pixel (resolve D1) | 1 MB echoes both directions |
| **P2** | 🔬 Spike | Create virtual display from Rust | Display appears in Settings |
| **P3** | 🔬 Spike | Capture virtual display + HW-encode to a playable `.h264` | File plays in VLC |
| **P4** | 🔬 Spike | Decode `.h264` on Pixel → on screen | Frames visible on phone |
| **P5** | Build | Wire full live pipeline; measure latency | Live desktop on phone < 50 ms |
| **P6** | Build | Touch back-channel → CGEvent injection | Tap on phone moves Mac cursor |
| **P7** | Build | Robustness, UX, menu-bar app, HEVC, signing | Survives hotplug; notarized |
| **P8** | Build | Packaging, distribution, OSS hygiene | Installable `.app` + `.apk` |

### 4.1 Delivery PR Ladder (functional → usable → shippable)

The phases above are de-risk-ordered. Once the spikes (P1–P4) are merged, the *remaining*
build work ships as a priority-ordered ladder of PRs — earlier = more essential to a working,
usable product. Each names the **feature a user gets** and the phase it draws from. (Kept in
sync with the execution tracker `.planning/ROADMAP.md`.)

Items marked ⭐NEW were added on user request (2026-06-04). They are largely **verification +
polish on capability that already exists**, surfaced only once the live pipeline runs: the Mac
cursor is already composited into the capture (`setShowsCursor(true)` in the P3 encode/capture
path), and the virtual display already presents as a named, arrangeable monitor with a stable
identity (P2 — `desc.name = @"RustScreen"`, vendor/product/serial, `sizeInMillimeters` ≈ 110 ppi).

| Item | GH PR | Title | Phase | Feature it brings | Depends on |
|---|---|---|---|---|---|
| 1 | **#21** (open) | **Live video pipeline** | P5 | The Mac desktop appears live on the phone — it becomes a second screen | P4 decode (merged PR #20) |
| 2 | #22 | **⭐NEW Phone presents as a real, arrangeable external display** | P2→P5/P7 | The phone sits in System Settings ▸ Displays as a named monitor you arrange (choose which side); held alive for the whole session; stable identity so macOS remembers its position; HiDPI scaling option (D6); removed cleanly on disconnect | item 1; coordinate with the `cg-virtual-display` objc2 work (merged PR #18) — same crate |
| 3 | #23 | **⭐NEW Mouse cursor visible on the external screen** | P5 | Your Mac cursor shows on the phone when you move it onto that display (carry `setShowsCursor(true)` into the live capture path; verify on device; handle HiDPI cursor scaling + cursor-only-update frames) | item 1 (rides directly on it) |
| 4 | #24 | **Live touch back-channel** | P6 | Tap/drag on the phone moves and clicks the Mac cursor (the inverse direction of item 3) | item 1 |
| 5 | #25 | **Hotplug / reconnect / clean teardown** | P7 | Survive cable pulls; phantom display vanishes on disconnect; auto-reconnect | item 1 (shares `session.rs`/`lib.rs` with item 4 — serialize) |
| 6 | #26 | **Latency proof + stream tuning** | P5 | Smooth, measured-responsive stream (jitter buffer, drop-to-keyframe, adaptive bitrate); proves glass-to-glass < 50 ms | item 1 |
| 7 | #27 | **Menu-bar app + onboarding** | P7/D5 | Launch from the menu bar; connect/disconnect, resolution picker, latency readout; permission prompts | item 1 (ideally item 4) |
| 8 | #28 | **Resolution / orientation / landscape lock** | P7 | Rotate the phone and the display follows; landscape lock; pick resolution | item 1/item 4 |
| 9 | #29 | **HEVC codec toggle** | P7 | Optional HEVC for better quality / lower bandwidth, H.264 fallback | item 1 |
| 10 | #30 | **NativeActivity purity swap** | P7 | No user-facing change; drops the Kotlin shell (Rust-source purity ~99%) | all Android items (1, 4, 8) merged — rewrites their files |
| 11 | #31 | **Signing, notarization & release** | P7/P8 | A new user can clone, install, plug in, and get a second screen following only the README | items 1–8 functional |

**PR-number anchor (2026-06-04):** latest merged = **PR #20**; ladder item 1 = **open PR #21**. The GH PR numbers for items 2–11 (**#22–#31**) are *projected* — they assume the items are opened in ladder order with nothing interleaved. GitHub assigns the real number at creation, so if other PRs land between, shift these accordingly. The **Item** column is the stable identifier; **Depends on** references ladder items as "item N" and real GitHub PRs as "#N".

**Milestones:** item 1 = "works as a screen." items 1–3 = a real, arrangeable second monitor you can
see your cursor on. items 1–4 = the full README promise (touch-capable live monitor). Through item 7 =
daily-usable app. Through item 11 = shippable to other people.

**Two usability gaps folded into the ladder** (not previously explicit above): keep-the-phone-screen-awake
(`FLAG_KEEP_SCREEN_ON`, fold into item 1) and landscape orientation lock (item 8).

---

### P0 — Workspace Scaffold & Cross-Compilation

**Files:** `Cargo.toml`, `crates/*/Cargo.toml`, `crates/protocol/src/lib.rs`, `crates/macos-host/src/main.rs`, `crates/android-client/src/lib.rs`, `rust-toolchain.toml`, `.github/workflows/ci.yml`

- [ ] **Step 1: Init repo & workspace**
  ```bash
  cd /Users/accountname/Documents/projects/rustscreen
  git init
  cargo new --lib crates/protocol
  cargo new --bin crates/macos-host
  cargo new --lib crates/cg-virtual-display
  cargo new --lib crates/android-client
  ```
- [ ] **Step 2: Author root `Cargo.toml` workspace**
  ```toml
  [workspace]
  resolver = "2"
  members = ["crates/protocol", "crates/macos-host", "crates/cg-virtual-display", "crates/android-client"]
  ```
- [ ] **Step 3: Pin toolchain & add Android target**
  ```bash
  rustup target add aarch64-linux-android
  cargo install cargo-ndk     # builds the Rust cdylib into jniLibs for the Kotlin shell (D7)
  # NDK r26+ required; set ANDROID_NDK_HOME
  ```
- [ ] **Step 4: `android-client` cdylib + thin Kotlin shell (D7, simplest-now)**
  - `crates/android-client/Cargo.toml`: `crate-type = ["cdylib"]`; deps `ndk`, `ndk-sys`, `jni`, `log`, `android_logger`.
  - `lib.rs`: stable JNI entry points the shell calls — `Java_..._nativeInit`, `nativeOnSurface(Surface)`, `nativeOnUsbFd(fd)`, `nativeOnTouch(...)`. For P0, just `nativeInit` logging "hello from Rust".
  - `android/` Gradle project: a ~50-LOC `MainActivity.kt` (SurfaceView + `System.loadLibrary`), built by `cargo-ndk -o android/app/src/main/jniLibs build`. **All logic stays in Rust** — Kotlin is glue only. (Swap this shell for NativeActivity in P7.)
- [ ] **Step 5: Verify both build**
  - Run: `cargo build -p macos-host` → Expected: builds, prints hello.
  - Run: `cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p android-client && (cd android && ./gradlew assembleDebug)` → Expected: debug `.apk`.
  - Install on Pixel 6a (dev mode) → Expected: app launches, `adb logcat` shows "hello from Rust".
- [ ] **Step 6: CI** — GitHub Actions: `cargo build` (macOS runner) + `cargo ndk` + `gradlew assembleDebug`. Commit.

**Acceptance:** A blank app launches on the Pixel (Kotlin shell, Rust core) and a binary runs on the Mac. *Note: the Kotlin shell is the deliberate "simplest-now" adapter per D0/D7; P7 replaces it with NativeActivity to remove Gradle/Kotlin.*

---

### P1 — 🔬 Spike: USB Byte Round-Trip (resolves **D1**)

**Why first:** every other phase assumes a working pipe. Until bytes move both ways, nothing else matters.

**Sub-spike A — AOA (lead candidate):**
- Mac host (`nusb`): enumerate the Pixel, issue AOA handshake — control req **51** (`GetProtocol`, `USB_DIR_IN | VENDOR`), **52** (send identity strings, `OUT`), **53** (`StartAccessory`). Phone re-enumerates as VID `0x18D1` / PID `0x2D00`–`0x2D01`; re-acquire handle, open bulk IN/OUT endpoints.
- Android (`usb_jni.rs`): register `usb-accessory` intent filter in manifest; on attach, call `UsbManager.openAccessory()` via `jni` → `ParcelFileDescriptor` → raw `fd` → read/write with `std::fs::File`. (This is the one unavoidable JNI touch — D0.)
- Echo test: Mac sends 1 MB of known bytes → phone echoes → Mac verifies.

**Sub-spike B — network-over-USB (fallback):**
- Enable USB tethering on the Pixel; check whether macOS brings up an NCM interface (`ifconfig`). If yes: phone runs a `std::net::TcpListener`, Mac connects, echo test. Zero USB code.

- [ ] Implement Sub-spike A; measure throughput + setup reliability.
- [ ] Implement Sub-spike B; check macOS NCM support on this M1 + Pixel 6a.
- [ ] **Decide D1.** Record rationale (throughput, latency, setup friction, robustness) in this doc.

**Acceptance:** 1 MB echoes correctly in both directions over USB-C, reproducibly across replug, with the chosen mechanism. Document measured throughput (need ≥ ~200 Mbit/s headroom for 1080p H.264).

**Risk:** AOA accessory permission UX (per-connect dialog); NCM may be absent on macOS. Mitigation: having both spikes means one will work.

---

### P2 — 🔬 Spike: Create a Virtual Display from Rust (highest risk)

**Files:** `crates/cg-virtual-display/src/lib.rs`, `crates/cg-virtual-display/build.rs`

The private API (from reverse-engineered headers, confirmed used by Chromium/BetterDisplay):
```
CGVirtualDisplay : NSObject
  - (id)initWithDescriptor:(CGVirtualDisplayDescriptor *)desc;
  - (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
CGVirtualDisplayDescriptor : NSObject   // name, max width/height, ppi, vendor/product/serial, queue
CGVirtualDisplaySettings  : NSObject    // modes (array of CGVirtualDisplayMode), hiDPI
CGVirtualDisplayMode      : NSObject    // initWithWidth:height:refreshRate:
```

- [ ] **Step 1: build.rs** — compile a ~40-line **ObjC++ `.mm` shim** via the `cc` crate (simplest-now adapter, D0/§0.1) and `println!("cargo:rustc-link-lib=framework=CoreGraphics");`. The shim wraps `alloc → initWithDescriptor: → applySettings:` and exposes `extern "C" fn rs_vdisplay_create(w,h,hz) -> u32 /*CGDirectDisplayID*/` + `rs_vdisplay_destroy(handle)`. *(Pure-`objc2` rewrite of this shim is a P7 task — the safe Rust API in Step 2 stays identical.)*
- [ ] **Step 2: Safe Rust wrapper** — `VirtualDisplay::new(width, height, refresh) -> Result<Self>` and `display_id() -> CGDirectDisplayID` over the shim's `extern "C"` fns. This is the stable port; callers never see the shim.
- [ ] **Step 3: Spike binary** — create a 2400×1080@60 display (D6), sleep 30 s, drop.
- [ ] **Step 4: Verify** — Run it; open **System Settings ▸ Displays** → Expected: a new display appears and is arrangeable. `display_id()` returns a valid non-zero ID enumerable via `CGGetActiveDisplayList`.

**Acceptance:** A phantom display appears in macOS Displays settings, created entirely from Rust, and its `CGDirectDisplayID` is obtainable (needed by P3 capture).

**Risks (HIGH):** Private API — may need entitlements/signing even in dev; refresh capped ~60 Hz; can break across macOS versions; barred from Mac App Store (distribute via notarized DMG, not MAS — feeds P8). If `CGVirtualDisplay` proves unworkable, fallback options to evaluate: a CoreMediaIO/DriverKit virtual display extension (much heavier) or mirroring an existing display (loses "extend"). **Surface this result to the user immediately if it fails** — it's the project's keystone.

---

### P3 — 🔬 Spike: Capture + Hardware-Encode on macOS

**Files:** `crates/macos-host/src/capture.rs`, `crates/macos-host/src/encode.rs`

- [ ] **Step 1: Capture** — with `objc2-screen-capture-kit` (pure-objc2, no Swift bridge), create an `SCStream` filtered to the virtual display's `SCDisplay` (matched by the P2 `display_id`). Pull `CMSampleBuffer`/`IOSurface` frames. Fallback if SCK binding is rough: `CGDisplayStream` via `objc2-core-graphics` (deprecated but simple).
- [ ] **Step 2: Encode** — feed the capture `IOSurface`/`CVPixelBuffer` zero-copy into the `videotoolbox` crate's H.264 encoder (D2). Configure: realtime, low-latency (no B-frames), keyframe interval ~2 s, bitrate target.
- [ ] **Step 3: Dump** — write the Annex-B/length-prefixed NAL stream to `out.h264` for 10 s.
- [ ] **Step 4: Verify** — Run: `ffplay out.h264` (or VLC) → Expected: recognizable video of the virtual desktop.

**Acceptance:** A `.h264` file captured from the virtual display plays back correctly. SPS/PPS (codec config) extracted and logged (P4/P5 need it). Encode latency per frame logged.

**Risks:** `videotoolbox` is experimental (~69% documented) — wrap behind our own `Encoder` trait so it's swappable. SCK of a *virtual* display ID may behave differently than a physical one — validate explicitly.

---

### P4 — 🔬 Spike: Decode + Present on the Pixel

**Files:** `crates/android-client/src/decode.rs`, plus a temporary feeder.

- [ ] **Step 1: AMediaCodec wrapper** — thin Rust over `ndk-sys` `libmediandk` (`AMediaCodec_createDecoderByType("video/avc")`, `AMediaCodec_configure` with an `ANativeWindow`, `…_dequeueInputBuffer`/`…_queueInputBuffer`/`…_dequeueOutputBuffer` with `render=true`). Reference `rust_mediacodec` but expect to own the code (it's stale, v0.1.2/2022). **Decode-to-surface (D3):** the `ANativeWindow` comes from the Kotlin shell's `SurfaceView` via `nativeOnSurface(Surface)` → `ANativeWindow_fromSurface` (D7); pass it to `configure` so frames render with no CPU copy. *(When P7 swaps to NativeActivity, the window comes from `app.native_window()` instead — `decode.rs` is unchanged.)*
- [ ] **Step 2: Feed** — push the P3 `out.h264` to the phone (adb push for the spike), read it, split into NAL units (using `protocol::framing` if already framed, else simple Annex-B start-code split), feed `csd-0` (SPS/PPS) first, then frames.
- [ ] **Step 3: Verify** — Run on Pixel → Expected: the recorded desktop video plays full-screen on the phone.

**Acceptance:** Hardware-decoded H.264 renders on the Pixel screen via `ANativeWindow`, no Java decode wrapper.

**Risks:** Color/format mismatch (NV12 vs surface), codec-specific-data (`csd-0`) plumbing, rotation/aspect. Validate the exact `AMediaFormat` keys the Pixel 6a's decoder expects.

---

### P5 — Build: Live End-to-End Pipeline + Latency

**Files:** `crates/protocol/src/{framing,messages}.rs`, `crates/macos-host/src/{main,transport}.rs`, `crates/android-client/src/{lib,transport}.rs`

Now connect P1–P4 live. This phase *does* use TDD for the pure-Rust protocol pieces.

#### Task P5.1 — Protocol framing (TDD)
**Files:** Create `crates/protocol/src/framing.rs`, `crates/protocol/src/messages.rs`; Test inline `#[cfg(test)]`.

- [ ] **Step 1: Write the failing test**
  ```rust
  #[test]
  fn roundtrip_video_frame() {
      let f = Frame::Video { pts_us: 123, keyframe: true, nal: vec![0,1,2,3] };
      let mut buf = Vec::new();
      f.write_to(&mut buf).unwrap();
      let (decoded, consumed) = Frame::read_from(&buf).unwrap();
      assert_eq!(decoded, f);
      assert_eq!(consumed, buf.len());
  }
  ```
- [ ] **Step 2: Run, verify it fails** — `cargo test -p protocol roundtrip_video_frame` → Expected: FAIL (no `Frame`).
- [ ] **Step 3: Implement** — `enum Frame { Handshake{..}, VideoConfig{ codec, sps_pps: Vec<u8> }, Video{ pts_us:u64, keyframe:bool, nal:Vec<u8> }, Touch(TouchEvent), Control(Control) }`. Wire format: `u8 tag · u32 len · payload`. Control/Touch/Handshake payloads via `postcard`; `Video` payload written raw (no serde over big buffers — perf).
- [ ] **Step 4: Run, verify it passes** — Expected: PASS.
- [ ] **Step 5: Commit** — `feat(protocol): length-prefixed frame codec`.

#### Task P5.2 — Handshake & resolution negotiation
- [ ] Mac sends `Handshake { width, height, fps, codec }` (D6 values); phone acks, sizes its decoder + window. Test the negotiation logic as a pure function.

#### Task P5.3 — Live wiring
- [ ] Mac: `virtual_display → capture → encode → framing → transport.tx` on a frame thread.
- [ ] Phone: `transport.rx → deframe → decode-to-surface` loop in `android_main`.
- [ ] Send `VideoConfig` (SPS/PPS) on connect and on each keyframe request.

#### Task P5.4 — Latency harness
- [ ] Render a millisecond timer/QR on the Mac virtual display, photograph the phone showing it, compute glass-to-glass delta. Log per-stage timings.
- [ ] **Acceptance:** Live extended desktop visible on the Pixel; **measured glass-to-glass < 50 ms** (revise budget in §2 with real numbers). Commit.

**Risk:** First real backpressure/jitter. Add a 1-frame jitter buffer; drop-to-keyframe on overrun. Keep it minimal (YAGNI) until measured.

---

### P6 — Build: Touch Back-Channel → macOS Injection

**Files:** `crates/protocol/src/coords.rs`, `crates/android-client/src/input.rs`, `crates/macos-host/src/inject.rs`

#### Task P6.1 — Coordinate mapping (TDD, pure)
- [ ] **Step 1: Failing test**
  ```rust
  #[test]
  fn maps_normalized_touch_to_display_pixels() {
      // virtual display 2400x1080 at global origin (1512, 0)
      let m = DisplayMap::new(/*origin*/ (1512.0, 0.0), /*size*/ (2400.0, 1080.0));
      assert_eq!(m.to_global(0.5, 0.5), (1512.0 + 1200.0, 540.0));
  }
  ```
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement `DisplayMap::to_global(nx, ny)`** mapping normalized [0,1] touch to global CG coordinates using the virtual display's bounds (from `CGDisplayBounds`).
- [ ] **Step 4: Run → PASS. Step 5: Commit.**

#### Task P6.2 — Android touch capture
- [ ] In `android_main`, read `AInputEvent` motion events (down/move/up), normalize to [0,1] against window size, send `protocol::TouchEvent { id, phase, nx, ny }`.

#### Task P6.3 — macOS injection
- [ ] In `inject.rs`, on `TouchEvent`: `DisplayMap::to_global` → `CGEventCreateMouseEvent` (`mouseMoved`/`leftMouseDown`/`leftMouseUp`) → `CGEventPost(kCGHIDEventTap, …)` via `objc2-core-graphics`.
- [ ] Permission: check `AXIsProcessTrusted()`; if false, prompt the user to grant **Accessibility** (System Settings ▸ Privacy & Security ▸ Accessibility). Surface clear onboarding.
- [ ] **Acceptance:** Tapping/dragging on the Pixel moves and clicks the macOS cursor at the correct location *on the virtual display*. Commit.

**Risk:** Accessibility permission is mandatory and easy to misconfigure — make the failure message actionable. Multitouch/pen deferred (D4).

---

### P7 — Build: Robustness, UX, Codec Options, Signing, + Rust-purity upgrades

**Rust-purity upgrades (D0/§0.1 — each is an isolated adapter swap, core logic untouched; do only after the MVP pipeline is proven):**
- [ ] **Android shell → NativeActivity:** replace the Kotlin Activity with `android-activity` (native-activity), package via `cargo-apk`, drop Gradle. Window from `app.native_window()`, USB fd via `jni`→`UsbManager`, touch via `AInputEvent`. `decode.rs`/`transport.rs`/`protocol` unchanged. Bumps purity toward ~99%.
- [ ] **Virtual display shim → pure `objc2`:** reimplement `cg-virtual-display` with `objc2` msg-sends to the private classes; delete the `.mm` shim + `cc` dep. Safe `VirtualDisplay` API unchanged.
- [ ] **Capture bridge → pure `objc2`:** swap `screencapturekit-rs` (Swift bridge) for `objc2-screen-capture-kit` behind the `Capturer` trait.
- [ ] Update the **Rust purity %** metric in §0.1.

**Robustness & UX:**
- [ ] **Hotplug/disconnect:** detect cable pull on both ends; tear down cleanly; auto-reconnect; remove the virtual display on disconnect so the Mac desktop reflows.
- [ ] **Resolution/orientation:** handle Pixel rotation; renegotiate on geometry change.
- [ ] **HEVC toggle (D2):** add `video/hevc` path on both encoder and decoder behind a flag; fall back to H.264 if unsupported.
- [ ] **Adaptive bitrate:** drop bitrate/request keyframe on RX backpressure.
- [ ] **Mac menu-bar app (D5):** status item — connect/disconnect, resolution picker, latency readout. (Pure-Rust tray, e.g. `tray-icon` + `objc2` status item.)
- [ ] **Code-signing & hardened runtime:** sign the host with entitlements; confirm the private `CGVirtualDisplay` calls survive hardened runtime; **notarize** (required because MAS is out — P2 risk). Document the entitlement set (research open question — empirically determine).
- [ ] **Acceptance:** Survives 20 replug cycles; HEVC works on Pixel 6a; notarized host launches without Gatekeeper warnings.

---

### P8 — Build: Packaging, Distribution, OSS Hygiene

- [ ] **macOS:** bundle a `.app`, sign + notarize, ship a DMG. Document Accessibility + (if needed) virtual-display permission steps.
- [ ] **Android:** release `.apk` via `cargo apk build --release`; signing config; document enabling the USB-accessory permission. (Play Store distribution optional — confirm.)
- [ ] **Repo:** `README.md` (architecture diagram, the honest "100% Rust source" statement from §0, build/run instructions per platform), `LICENSE` (confirm MIT/Apache-2.0), `CONTRIBUTING.md`, issue templates, CI badges.
- [ ] **Acceptance:** A new user can clone, build both halves, install, plug in a Pixel 6a, and get a second screen following only the README.

---

## 5. Risk Register

| # | Risk | Likelihood | Impact | Mitigation | Phase |
|---|---|---|---|---|---|
| R1 | `CGVirtualDisplay` private API unbindable/broken from Rust | Med | **Critical** | Isolate in `cg-virtual-display` crate; spike first (P2); DriverKit fallback | P2 |
| R2 | macOS NCM absent → tethering transport dead | Med | Med | AOA is the lead path anyway (P1 dual-spike) | P1 |
| R3 | `videotoolbox`/`objc2-screen-capture-kit` experimental churn | Med | Med | Wrap behind own traits; pin versions; CGDisplayStream fallback | P3 |
| R4 | `rust_mediacodec` stale → must own decoder | High | Low | Plan to vendor a thin `ndk-sys` wrapper from the start | P4 |
| R5 | Latency budget unmet (>50 ms) | Med | High | Decode-to-surface (D3a), low-latency encode, jitter tuning, measure early (P5.4) | P5 |
| R6 | Accessibility permission friction for injection | High | Low | Clear onboarding + `AXIsProcessTrusted` gating | P6 |
| R7 | AOA accessory permission dialog per-connect annoyance | Med | Low | "remember" intent flag; document UX | P1/P7 |
| R8 | Notarization rejects private-API binary | Low | Med | Notarization checks signing/malware, not API use; test early in P7 | P7 |

---

## 6. Definition of Done (project)

Plug a Pixel 6a into an M1 MacBook with one USB-C cable, launch the Mac app, and within seconds the phone becomes a usable, touch-capable extended display with sub-50 ms latency — with every line of source in this repo written in Rust, save for an `AndroidManifest.xml` and documented FFI to the three platform boundaries in §0.

---

## 7. Per-Phase Plan Expansion

Spike phases (P1–P4) are intentionally *not* full bite-sized TDD plans — they're exploratory and their exact code depends on what the platform actually does. **After your review and after each spike succeeds, I'll expand the next build phase into a detailed `superpowers:writing-plans` document** (full TDD steps, exact code) before executing it. Build phases P5–P8 are partially specified above and will be completed the same way.

**Suggested first implementation target after your approval:** **P0 → P2 → P1** (scaffold, then immediately attack the keystone risk R1, then transport), because if P2 fails the whole architecture changes and we want to know on day one.
