# Contributing to RustScreen

Thanks for your interest in RustScreen — a wired USB-C second monitor for an M1
MacBook, with both halves (the macOS host and the Android client) written in
Rust. This guide covers how to build and test both halves, the project's
non-negotiable conventions, and what we expect in a pull request.

If anything here is unclear or out of date, open an issue — keeping this document
honest is itself a welcome contribution.

---

## Toolchain & MSRV

- **Rust:** stable, pinned by `rust-toolchain.toml` (channel = `stable`). `rustup`
  picks this up automatically.
- **MSRV: 1.80.** The `protocol` and `macos-host` crates set `rust-version = "1.80"`
  (`messages.rs` uses `slice::split_at_checked`, stable since 1.80). The pin also
  keeps Clippy MSRV-aware so it won't suggest stdlib APIs newer than 1.80. Do not
  raise the MSRV without flagging it explicitly in your PR.

---

## Workspace layout

RustScreen is a single Cargo workspace (`resolver = "2"`) with four crates:

| Crate | Role |
|---|---|
| `crates/protocol` | Pure-Rust shared core — framing, control/touch/video messages, coordinate-mapping logic. **No platform dependencies.** Shared verbatim by both halves so the wire format stays in sync. |
| `crates/macos-host` | The macOS host binary + library — orchestrates the virtual display, capture, encode, USB, and cursor injection. |
| `crates/android-client` | The Android client `cdylib` (`crate-type = ["cdylib"]`) — cross-compiles to `aarch64-linux-android`; JNI entry points, decode, USB, touch normalization. |
| `crates/cg-virtual-display` | Isolated bindings to the private macOS `CGVirtualDisplay` API — quarantined in its own crate so a macOS update that breaks it has a one-crate blast radius. |

The shared pure-Rust core is the whole point: the Rust logic is shell-agnostic and
sits behind seams (see "Ports & adapters" below).

---

## Building & testing

### The default build is cross-platform

The **default** `cargo build --workspace` compiles on any host (macOS or Linux CI)
and pulls **no** macOS-only or hardware-only dependencies. Every platform/hardware
integration sits behind an **off-by-default `live-*` feature**:

| Feature (on `macos-host`) | Pulls | Purpose |
|---|---|---|
| `live-usb` | `nusb` | Live AOA USB host path + the `p1_echo` spike bin. `nusb` is pure-Rust and cross-platform, so this feature *builds* anywhere but is only *run* on macOS. |
| `live-inject` | `core-graphics` | Live macOS cursor injection — the `touch::CgEventSink` adapter + the `p6_inject` smoke bin. macOS-only. |
| `live-capture` | the `objc2` Apple-FFI family + `cg-virtual-display` | Live macOS capture + VideoToolbox H.264 encode — the `p3_probe` / `p3_capture` / `p3_encode` spike bins. macOS-only (the `objc2` framework crates do not compile off macOS). |

**Why the default stays cross-platform:** it keeps CI fast and green on any runner,
lets contributors without a Mac or a phone build and test all the pure-Rust logic,
and guarantees the shared core never accidentally grows a platform dependency. The
`live-*` features are *additive* — never enable them in a `--all-features` build on
a non-macOS host (the macOS-only crates won't compile there).

### macOS host

```bash
# Default build — cross-platform, no live-* features (Android deps are target-gated and skipped)
cargo build --workspace
cargo test  --workspace

# Run the host binary (prints version; the full live pipeline is not wired yet — see status)
cargo run -p macos-host

# Live features (each requires a Mac, and live-capture also the
# Screen & System Audio Recording permission — see the README):
cargo build -p macos-host --features live-usb
cargo build -p macos-host --features live-inject
cargo build -p macos-host --features live-capture
```

The `live-capture` spikes additionally require the macOS **Screen & System Audio
Recording** permission granted to the terminal you launch them from. See the
README's "macOS capture spikes" section for the full procedure — this is
unavoidable for any screen-capture app on current macOS.

### Android client

```bash
# Host-side: the cross-platform parts (protocol reuse, touch normalization) build
# and test on any machine — the Android-only deps are target-gated:
cargo test -p android-client

# Build the .so for the device (requires the Android NDK + cargo-ndk):
export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/25.2.9519653"
rustup target add aarch64-linux-android   # one-time
cargo install cargo-ndk                    # one-time
# --features live-decode is REQUIRED for a working device build: it compiles the
# AMediaCodec decode-to-surface adapter. WITHOUT it the .so falls back to the P1 echo
# loop, which links and connects but decodes nothing → the phone shows a black screen.
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p android-client --features live-decode
```

The APK (Gradle) build needs a pinned JDK (17–21); see the README for details.

---

## Mandatory conventions

These are enforced in CI (`.github/workflows/ci.yml`) and a PR will not be merged
until they pass. Run them locally before pushing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test  --workspace
```

1. **Strict TDD.** All pure-Rust logic is written test-first: write a failing test,
   make it pass, refactor. The protocol framing, message codecs, and coordinate
   mapping all grew this way and the test count is tracked across phases. New
   pure-logic code is expected to arrive with its tests.
2. **`clippy -D warnings`.** Zero warnings. The workspace builds clean under Clippy
   with warnings denied, for `--all-targets`.
3. **`cargo fmt --check`.** Code must be `rustfmt`-clean. No exceptions.
4. **No unused imports.** Remove unused imports before committing, in every file you
   touch, in every language.

CI runs the full Rust gate (build / test / clippy / fmt) on `macos-latest` and a
separate Android-native leg that cross-compiles the `.so` via `cargo-ndk`. The
Gradle APK build is intentionally **not** in CI yet (it needs a pinned JDK and a
full Android SDK; that is deferred to a later phase).

---

## Architecture: ports & adapters ("simplest-now, replaceable-later")

RustScreen follows a **ports-and-adapters** model with one guiding principle:
**ship the simplest adapter that works now, behind a seam, and swap it for the
ideal one later without touching core logic.**

- A **port** is a Rust trait (or stable FFI seam) that the core logic depends on —
  e.g. `Transport` (move bytes), `Capturer` / `Encoder` (frames in, NAL units out),
  `PointerSink` (inject a pointer action), `VirtualDisplay` (create a display).
- An **adapter** is a concrete implementation of a port — e.g. `AoaTransport`
  (USB over AOA), `CgEventSink` (CGEvent cursor injection), the `objc2`
  ScreenCaptureKit capturer.

This is also how "100% Rust" is honestly achieved: the three unavoidable platform
boundaries (Android `NativeActivity`, the private macOS `CGVirtualDisplay`, the
Android USB-accessory file descriptor) each sit behind a port. The MVP may use a
thin shim (a small Kotlin Activity, an ObjC++ shim) as the adapter; later phases
replace it with a pure-Rust adapter behind the *same* port, so the core never
changes. See the README's Architecture section and the architecture roadmap
(`docs/superpowers/plans/`) for the full seam table.

**What this means for a contribution:**
- Put platform/hardware code in an adapter behind a port; keep the core pure and
  testable.
- Prefer the simplest implementation that satisfies the port's contract now; note
  in your PR where a richer adapter could replace it later.
- Never leak platform types into the `protocol` crate.

---

## Commit & PR expectations

- **Branch** off `main` (e.g. `feat/p9-foo`, `fix/bar`). Do not commit directly to
  `main`.
- **Small, focused commits** with clear messages explaining *why*, not just *what*.
- **Keep `.planning/` out of feature PRs** unless the change is specifically about
  planning — code review PRs should be code, docs, and tests.
- Before opening a PR, run the full local gate (fmt / clippy / test) and confirm it
  passes — do not rely on CI to catch fmt/clippy failures.
- The PR description should summarize the change, call out any assumptions or
  follow-ups, and be honest about what is and isn't verified (this project has a
  strong norm against overclaiming — describe real status, never aspirational).

---

## License of contributions

The project's license is **not finalized yet** — see [`LICENSE`](LICENSE) for the
current status and the intended direction (a Voluntaryism-compatible license: GPL-like
but without mandatory source disclosure). By submitting a contribution you agree that
it may be licensed under the project's eventual license, consistent with that stated
intent. If you are not comfortable contributing under an as-yet-unpublished license,
please hold your contribution until the license is finalized.
