---
phase: P1-usb-byte-round-trip
plan: 01
type: tdd
wave: 1
depends_on: []
files_modified:
  - crates/macos-host/src/transport.rs
  - crates/macos-host/src/aoa.rs
  - crates/macos-host/src/lib.rs
  - crates/macos-host/src/bin/p1_echo.rs
  - crates/macos-host/Cargo.toml
  - crates/android-client/src/transport.rs
  - crates/android-client/src/lib.rs
  - crates/android-client/Cargo.toml
  - android/app/src/main/AndroidManifest.xml
  - android/app/src/main/res/xml/accessory_filter.xml
  - android/app/src/main/java/com/rustscreen/client/MainActivity.kt
autonomous: false
requirements: [XPORT-01]

user_setup:
  - service: android-usb-accessory
    why: "On first attach the Pixel shows a one-time OS permission dialog (\"Allow RustScreen to access the USB accessory?\"). Until the user taps OK, openAccessory() returns null and no fd reaches Rust (RESEARCH Pitfall 4 / Runtime State Inventory). Hands-on only — Wave B."
    dashboard_config:
      - task: "Tap OK on the on-phone USB-accessory permission dialog when the cable is attached"
        location: "Pixel 6a — on-screen dialog fired by the USB_ACCESSORY_ATTACHED intent"
  - service: macos-usb-claim
    why: "nusb may fail to claim the Pixel's interface without root (RESEARCH Pitfall 1 / A2 — the central HIGH hardware unknown). If plain claim_interface returns Access denied / Resource busy, re-run the spike under sudo and record whether root was required (feeds D1 friction + the P7 entitlement task)."
    dashboard_config:
      - task: "If claim fails, re-run `sudo target/debug/p1_echo`; record whether root was required"
        location: "macOS terminal (the user's own machine)"

must_haves:
  truths:
    - "A 1 MiB deterministic test pattern generates and verifies byte-for-byte, reporting the first differing offset on mismatch (pure, tested)"
    - "echo_roundtrip drives any Transport (Read + Write) — write_all the pattern, read_exact it back, assert equality — over an in-memory LoopbackTransport fake with no hardware (pure, tested)"
    - "Chunked/partial reads are reconciled: a ChunkedLoopback that returns at most 16 KiB per read still round-trips the full 1 MiB (pure, tested — RESEARCH Pitfall 3)"
    - "Throughput math converts bytes + elapsed Duration into Mbit/s and is asserted against a known value (pure, tested — RESEARCH Pitfall 5)"
    - "protocol::framing still round-trips over a Transport (regression — the seam stays framing-compatible for P5)"
    - "1 MB echoes Mac→phone→Mac byte-for-byte over USB-C, reproducibly across one cable replug, with measured throughput printed and the D1 verdict recorded in ROADMAP §1 (hands-on)"
  artifacts:
    - path: "crates/macos-host/src/transport.rs"
      provides: "Transport: Read + Write + Send seam (blanket impl), echo_roundtrip, make_pattern, first_mismatch, mbit_per_sec, EchoStats, LoopbackTransport + ChunkedLoopback test fakes (pure, tested)"
      contains: "pub trait Transport"
      min_lines: 60
    - path: "crates/macos-host/src/aoa.rs"
      provides: "AOA handshake (control req 51/52/53), re-enumeration retry loop, AoaTransport over nusb endpoints, NcmTransport over TcpStream — all cfg-gated behind the live-usb feature so Wave A CI stays clean"
      contains: "AOA_GET_PROTOCOL"
    - path: "crates/macos-host/src/bin/p1_echo.rs"
      provides: "spike binary: AOA handshake → re-enumerate → claim → echo 1 MiB → print throughput; --ncm subcommand for the fallback; cfg-gated behind live-usb (hands-on-Mac)"
    - path: "crates/android-client/src/transport.rs"
      provides: "AccessoryFdTransport (File over raw fd, impls Read + Write) + echo_loop (read a chunk, write it straight back); echo_loop tested against the shared loopback fake; the fd wrap is cfg(target_os=android)"
      contains: "fn echo_loop"
    - path: "android/app/src/main/res/xml/accessory_filter.xml"
      provides: "manufacturer/model/version filter matching the control-req-52 identity strings exactly (RESEARCH Pitfall 4)"
      contains: "usb-accessory"
  key_links:
    - from: "crates/macos-host/src/aoa.rs"
      to: "macos_host::transport::Transport"
      via: "AoaTransport and NcmTransport satisfy Transport (Read + Write + Send); the spike calls echo_roundtrip over either — the D1 swap point"
      pattern: "echo_roundtrip"
    - from: "crates/android-client/src/lib.rs"
      to: "crate::transport::echo_loop"
      via: "nativeOnUsbFd(fd) JNI entry wraps the fd as AccessoryFdTransport and runs echo_loop"
      pattern: "nativeOnUsbFd"
    - from: "android/app/src/main/java/com/rustscreen/client/MainActivity.kt"
      to: "nativeOnUsbFd"
      via: "Kotlin openAccessory() → ParcelFileDescriptor → pfd.detachFd() → nativeOnUsbFd(fd); strings in control-req-52 must equal accessory_filter.xml verbatim"
      pattern: "nativeOnUsbFd"
    - from: "crates/macos-host/src/transport.rs"
      to: "protocol::framing::write_frame"
      via: "regression test frames a payload through a LoopbackTransport and reads it back, proving the seam is framing-compatible for P5"
      pattern: "write_frame"
---

<objective>
Prove XPORT-01: bytes move both ways Mac↔Pixel over one USB-C cable — a 1 MB echo verified byte-for-byte, reproducible across replug, with measured throughput — behind a swappable `Transport` seam, and **resolve D1 (AOA vs NCM/TCP) with recorded rationale**.

Purpose: This is the P1 transport spike. The hard parts are not code — they are the AOA handshake sequence (well-specified, RESEARCH Pattern 1) and the two genuinely-uncertain hardware facts: (A2) whether macOS lets `nusb` claim the Pixel's interface without root, and (A3) whether AOA bulk clears ~200 Mbit/s. The whole transport reduces to "get a `Read + Write` handle on each end, then echo." All the verification/throughput/chunking logic is pure and TDD-able now; only the live enumeration, on-phone permission, real echo, replug, and throughput numbers need hardware.

Output:
- **Wave A (cable-free, CI-green now — TDD):** the `Transport: Read + Write + Send` seam, `echo_roundtrip`, `make_pattern`/`first_mismatch`, `mbit_per_sec`, the `LoopbackTransport`/`ChunkedLoopback` fakes, partial-read reconciliation, a framing-reuse regression, and the Android `echo_loop` tested against the shared loopback. Every task ends `cargo test --workspace` green, no hardware, cross-platform.
- **Wave B (hands-on, ONE gated human session):** install `nusb` (its own blocking dep checkpoint), the Mac AOA host (`nusb` enumerate → req 51/52/53 → re-enumerate → claim → bulk echo), the Android accessory glue (Kotlin `openAccessory`→`detachFd`→`nativeOnUsbFd` JNI + manifest intent filter + `accessory_filter.xml` + Rust echo over `File::from_raw_fd`), all cfg-gated so Wave A CI stays clean and cross-platform, then a `checkpoint:human-verify` for the LIVE 1 MB echo + replug + throughput + **the D1 verdict**.

Locked-seam contract (CONTEXT D2, RESEARCH Pattern 4): `Transport` IS `Read + Write + Send` via a blanket impl — no new abstraction. `nusb` endpoints, the Android accessory `File`, and `TcpStream` all already qualify. Do NOT introduce `send`/`recv` methods; that would reinvent `std::io` and break `protocol::framing` reuse. Do NOT break the existing `protocol::framing` public API or the existing `nativeInit` JNI entry.

Crate-path note: `macos-host` ships BOTH a library crate (`macos_host`, the tested seam + the cfg-gated AOA/NCM adapters) and binary crates (`main.rs` plus the new `bin/p1_echo.rs` spike). From `p1_echo.rs`, lib items are reached as `macos_host::transport::echo_roundtrip`, `macos_host::aoa::AoaTransport`, etc. — NOT `crate::...`.
</objective>

<execution_context>
@$HOME/.claude/get-shit-done/workflows/execute-plan.md
@$HOME/.claude/get-shit-done/templates/summary.md
</execution_context>

<context>
@.planning/PROJECT.md
@.planning/ROADMAP.md
@.planning/STATE.md
@.planning/phases/P1-usb-byte-round-trip/CONTEXT.md
@.planning/phases/P1-usb-byte-round-trip/RESEARCH.md

# Reuse — do NOT break these public APIs:
@crates/protocol/src/framing.rs
@crates/android-client/src/lib.rs
@crates/macos-host/src/lib.rs

# Existing platform glue to extend:
@android/app/src/main/AndroidManifest.xml
@android/app/src/main/java/com/rustscreen/client/MainActivity.kt
@crates/android-client/Cargo.toml
@crates/macos-host/Cargo.toml

<interfaces>
<!-- Contracts the executor uses directly — no codebase exploration needed. -->

From crates/protocol/src/framing.rs (REUSE as-is, do not modify):
```rust
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;
pub fn write_frame(w: &mut dyn Write, tag: u8, payload: &[u8]) -> io::Result<()>;
pub fn read_frame(r: &mut dyn Read) -> io::Result<(u8, Vec<u8>)>;
```
Both already take `&mut dyn Read` / `&mut dyn Write` — a `&mut dyn Transport` (or any `&mut T: Transport`) is drop-in.

From crates/android-client/src/lib.rs (existing JNI pattern to mirror — keep `nativeInit` intact):
```rust
#[cfg(target_os = "android")]
mod android {
    use jni::objects::JClass;
    use jni::JNIEnv;
    #[no_mangle]
    pub extern "system" fn Java_com_rustscreen_client_MainActivity_nativeInit(
        _env: JNIEnv, _class: JClass) { /* android_logger init + log::info! */ }
}
```
The new entry mirrors this: `Java_com_rustscreen_client_MainActivity_nativeOnUsbFd(env, class, fd: jint)`.

From crates/macos-host/src/lib.rs (add modules; existing `pub mod` lines stay):
```rust
pub mod capture; pub mod capture_select; pub mod encode; pub mod encode_vt;
// ADD: pub mod transport;  + (cfg-gated)  pub mod aoa;
```

nusb 0.2 control/bulk shape (RESEARCH Pattern 1 / A5 — VERIFY exact field names against docs.rs at execution; nusb is moving 0.2.x):
```rust
// use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};
// ControlIn  { control_type: ControlType::Vendor, recipient: Recipient::Device,
//              request: 51, value: 0, index: 0, length: 2 }.wait()? -> Vec<u8> (u16 LE proto ver)
// ControlOut { control_type: ControlType::Vendor, recipient: Recipient::Device,
//              request: 52, value: 0, index: <0..=5>, data: &zero_terminated_utf8 }.wait()?
// ControlOut { request: 53, ... data: &[] }.wait()?   // START -> device re-enumerates
// After START: nusb::list_devices() until VID 0x18D1 && PID in 0x2D00..=0x2D05; open();
//   claim_interface(0); endpoint::<Bulk, Out>(addr); endpoint::<Bulk, In>(addr) (addrs from descriptor)
```
</interfaces>
</context>

<tasks>

<!-- ========================= WAVE A — cable-free, TDD, CI-green now ========================= -->

<task type="tdd" tdd="true">
  <name>Task A1: Transport seam + echo/pattern/throughput logic (TDD, cable-free)</name>
  <files>crates/macos-host/src/transport.rs, crates/macos-host/src/lib.rs</files>
  <behavior>
    Write these tests FIRST (RED), then implement until GREEN (RESEARCH Code Examples):
    - make_pattern(1 << 20) returns 1 MiB where byte i == (i % 256) as u8; len == 1<<20.
    - first_mismatch(a, b) -> Some(offset) at the first differing index; Some(min_len) on length mismatch; None when equal.
    - loopback_echo_roundtrips_1mib: echo_roundtrip(&mut LoopbackTransport::default(), &make_pattern(1<<20)) returns Ok(EchoStats { bytes: 1<<20, .. }); the bytes read back equal the pattern.
    - echo_roundtrip_detects_corruption: a fake that flips one byte on echo makes echo_roundtrip return Err (length-or-content mismatch surfaced via first_mismatch).
    - mbit_per_sec(1 << 20, Duration::from_millis(40)) is within 1.0 of 209.7.
    - chunked_loopback_roundtrips_1mib: a ChunkedLoopback whose read() yields at most 16 KiB per call still round-trips the full 1 MiB via read_exact (RESEARCH Pitfall 3).
  </behavior>
  <action>
    Add `pub mod transport;` to crates/macos-host/src/lib.rs (leave existing `pub mod` lines).
    In transport.rs implement, per RESEARCH Pattern 4 + Code Examples (CONTEXT D2):
    - `pub trait Transport: std::io::Read + std::io::Write + Send {}` plus a blanket `impl<T: Read + Write + Send> Transport for T {}`. NO send/recv methods — the seam IS std::io (this is the D1 swap point and the framing-reuse guarantee).
    - `pub fn make_pattern(len: usize) -> Vec<u8>` (byte i = i % 256).
    - `pub fn first_mismatch(a: &[u8], b: &[u8]) -> Option<usize>`.
    - `pub struct EchoStats { pub bytes: usize, pub elapsed: std::time::Duration }`.
    - `pub fn echo_roundtrip<T: Transport>(t: &mut T, pattern: &[u8]) -> io::Result<EchoStats>`: time only the transfer span; write_all(pattern); read_exact into a buffer sized to pattern.len() (bounded allocation — V5/DoS guard T-P1-02, never allocate from an unbounded wire length); on first_mismatch(..).is_some() return io::Error(InvalidData) naming the offset.
    - `pub fn mbit_per_sec(bytes: usize, elapsed: Duration) -> f64` = bytes*8 / 1e6 / secs.
    - In `#[cfg(test)]`: `LoopbackTransport` (in-memory VecDeque/Cursor that returns on read what was written) and `ChunkedLoopback` (same, but read() returns at most 16 KiB per call).
  </action>
  <verify>
    <automated>cargo test -p macos-host transport && cargo test --workspace</automated>
  </verify>
  <done>RED tests written and committed first; all six behaviors pass; `Transport` blanket impl exists; cargo test --workspace green on macOS/Linux CI with no new runtime deps and no hardware.</done>
</task>

<task type="tdd" tdd="true">
  <name>Task A2: Android echo_loop + framing-reuse regression (TDD, cable-free)</name>
  <files>crates/android-client/src/transport.rs, crates/android-client/src/lib.rs, crates/macos-host/src/transport.rs</files>
  <behavior>
    Write tests FIRST (RED), then implement until GREEN:
    - echo_loop_echoes_until_eof: echo_loop(&mut fake) where the fake's read side is pre-loaded with make_pattern(1<<20) split into ≤16 KiB chunks then EOF; assert the fake's write side accumulated exactly the same 1 MiB, and the loop returns Ok on EOF (read a chunk, write it straight back — RESEARCH Pattern 3, never assume the whole payload in one read).
    - framing_round_trips_over_transport (in macos-host transport tests): write_frame(&mut loopback, tag, payload) then read_frame(&mut loopback) returns the same (tag, payload) — proves the seam stays framing-compatible for P5 (regression, do not modify protocol::framing).
  </behavior>
  <action>
    android-client/src/transport.rs:
    - `pub fn echo_loop<T: Read + Write>(t: &mut T) -> io::Result<u64>`: loop { read into a ≤16 KiB buf; on 0 bytes return Ok(total); write_all(&buf[..n]); accumulate }. This is the pure, platform-agnostic core (Architectural Responsibility Map: echo belongs in Rust core).
    - `#[cfg(target_os = "android")] pub struct AccessoryFdTransport(pub std::fs::File);` with a constructor `from_raw_fd(fd: RawFd) -> Self` that does the single `unsafe { File::from_raw_fd(fd) }` wrap (T-P1-03: single-ownership — the fd was detached on the Kotlin side; never double-close). File already impls Read+Write so AccessoryFdTransport derefs/delegates to it. Keep echo_loop itself NON-cfg-gated so it is tested on CI via an in-memory fake.
    - In transport.rs tests use an in-memory duplex fake (separate read-queue + write-sink) to drive echo_loop with no fd.
    Add `mod transport;` (and the android-only re-export) to android-client/src/lib.rs without disturbing the existing `nativeInit`.
    In macos-host/src/transport.rs tests add `framing_round_trips_over_transport` importing `protocol::framing`.
    Add `protocol = { path = "../protocol" }` to android-client/Cargo.toml ONLY if echo_loop needs nothing from it — prefer NOT adding a dep; the framing regression lives in macos-host which already depends on protocol.
  </action>
  <verify>
    <automated>cargo test -p android-client echo_loop && cargo test -p macos-host framing && cargo test --workspace</automated>
  </verify>
  <done>RED tests committed first; echo_loop round-trips 1 MiB through chunked reads to EOF; framing regression passes over a Transport; cargo test --workspace green; no hardware; android cfg-gated code compiles for the host target (the non-gated echo_loop is what CI tests).</done>
</task>

<!-- ========================= WAVE B — hands-on, ONE gated human session ========================= -->

<task type="checkpoint:human-verify" gate="blocking-human">
  <name>Task B0: nusb dependency legitimacy gate ([ASSUMED] — blocking, NOT auto-approvable)</name>
  <what-built>Nothing yet — this gate precedes the first new install. `nusb` is the ONLY new dependency this phase introduces (CONTEXT locked decision 4; RESEARCH Package Legitimacy Audit). slopcheck was sandbox-blocked, so `nusb` is tagged `[ASSUMED]` and must be human-verified before `cargo add`.</what-built>
  <how-to-verify>
    1. Open https://crates.io/crates/nusb and https://docs.rs/nusb/latest — confirm version 0.2.x, repo github.com/kevinmehall/nusb, healthy download count (~771k+), recent update.
    2. Confirm the repo is the real kevinmehall/nusb (not a typosquat) and there is no `build.rs` pulling a surprise C/libusb dependency.
    3. After approval, the executor runs `cargo add nusb --dry-run -p macos-host` and `cargo tree -p macos-host | grep -i nusb` to confirm 0.2.x resolves with no unexpected transitive C deps.
  </how-to-verify>
  <resume-signal>Type "approved" to allow `cargo add nusb`, or describe concerns (then we fall back to NCM/TCP which needs no USB crate).</resume-signal>
</task>

<task type="auto">
  <name>Task B1: Mac AOA host adapter + spike binary (cfg-gated behind live-usb)</name>
  <files>crates/macos-host/src/aoa.rs, crates/macos-host/src/lib.rs, crates/macos-host/src/bin/p1_echo.rs, crates/macos-host/Cargo.toml</files>
  <action>
    Acceptance is the Task B3 human checkpoint — these are the hands-on parts that cannot be unit-tested (real enumeration, claim, bulk I/O).
    Cargo.toml: add a `[features] live-usb = ["dep:nusb"]` feature and `nusb = { version = "0.2", optional = true }` under `[target.'cfg(target_os = "macos")'.dependencies]` (RESEARCH Installation). The `p1_echo` bin and the `aoa` module compile ONLY under `--features live-usb` so Wave A CI (default features) stays clean and cross-platform.
    lib.rs: `#[cfg(feature = "live-usb")] pub mod aoa;`
    aoa.rs (cfg(feature="live-usb")), per RESEARCH Patterns 1+2 (VERIFY nusb 0.2 control field names against docs.rs at execution — A5):
    - Constants AOA_GET_PROTOCOL=51, AOA_SEND_STRING=52, AOA_START=53.
    - `IDENTITY: [(u16, &str); 6]` = manufacturer "RustScreen", model "RustScreen Host", description "USB second-monitor link", version "1.0", uri "https://github.com/PeterXMR/rustscreen", serial "rs-0001". These MUST equal accessory_filter.xml verbatim (RESEARCH Pitfall 4) — define them ONCE here and reference in a doc-comment pointing at the XML.
    - `fn handshake(iface) -> Result<()>`: control_in req 51 (assert proto >= 1); control_out req 52 for each IDENTITY string zero-terminated; control_out req 53 (no data).
    - `fn reacquire(timeout) -> Result<Device>`: retry loop (~5 s) polling list_devices() for VID 0x18D1 && PID 0x2D00..=0x2D05; open the NEW handle (never reuse the old one — RESEARCH Pitfall 2). claim_interface(0); read bulk IN/OUT endpoint addresses from the interface descriptor (A6); build `AoaTransport { ep_out, ep_in }` that impls Read (ep_in, chunked) + Write (ep_out, chunked at ≤16 KiB — Pitfall 3) + Send → satisfies Transport.
    - `NcmTransport(TcpStream)` newtype (TcpStream already Read+Write+Send) for the fallback (RESEARCH Pattern 5).
    bin/p1_echo.rs (cfg(feature="live-usb")): default path = AOA (find Pixel by attrs → handshake → reacquire → AoaTransport); `--ncm <host:port>` subcommand = connect a TcpStream → NcmTransport. Either way call `macos_host::transport::echo_roundtrip(&mut t, &make_pattern(1<<20))` repeated enough times to amortize per-transfer overhead (≥ several MB total — Pitfall 5), print Mbit/s via mbit_per_sec, print first_mismatch offset on failure. Use blocking nusb `.wait()` (no tokio — CONTEXT discretion, RESEARCH Anti-Patterns). log:: each handshake step + byte counts for `adb logcat`/stderr observability.
  </action>
  <verify>
    <automated>cargo build -p macos-host --features live-usb && cargo test --workspace</automated>
  </verify>
  <done>`cargo build -p macos-host --features live-usb` compiles on macOS; default-feature `cargo test --workspace` stays green and cross-platform (aoa/p1_echo excluded). Live behavior is verified in Task B3.</done>
</task>

<task type="auto">
  <name>Task B2: Android accessory glue — Kotlin openAccessory + nativeOnUsbFd JNI + manifest/filter</name>
  <files>crates/android-client/src/lib.rs, android/app/src/main/AndroidManifest.xml, android/app/src/main/res/xml/accessory_filter.xml, android/app/src/main/java/com/rustscreen/client/MainActivity.kt</files>
  <action>
    Acceptance is the Task B3 human checkpoint (on-phone permission dialog + live echo cannot be unit-tested).
    android-client/src/lib.rs — inside the existing `#[cfg(target_os = "android")] mod android`, ADD a new entry mirroring the existing `nativeInit` JNI pattern (do not touch nativeInit):
    `Java_com_rustscreen_client_MainActivity_nativeOnUsbFd(mut env, _class, fd: jni::sys::jint)` → `let mut t = crate::transport::AccessoryFdTransport::from_raw_fd(fd as RawFd); let _ = crate::transport::echo_loop(&mut t);` with log:: around it. echo_loop is the Wave-A-tested core; this entry is just the fd-to-Rust seam (RESEARCH Pattern 3).
    accessory_filter.xml (NEW, res/xml/): `<resources><usb-accessory manufacturer="RustScreen" model="RustScreen Host" version="1.0" /></resources>` — manufacturer/model/version MUST equal the aoa.rs IDENTITY strings verbatim (RESEARCH Pitfall 4; the description/uri/serial are not matched by the filter).
    AndroidManifest.xml — add to the existing MainActivity activity a second intent-filter for `android.hardware.usb.action.USB_ACCESSORY_ATTACHED` plus a `<meta-data android:name="android.hardware.usb.action.USB_ACCESSORY_ATTACHED" android:resource="@xml/accessory_filter" />` (keep the existing MAIN/LAUNCHER filter).
    MainActivity.kt — in onCreate, mirror RESEARCH Pattern 3: get UsbManager, read EXTRA_ACCESSORY from intent; if present, `usb.openAccessory(accessory)` → ParcelFileDescriptor → `pfd.detachFd()` → `nativeOnUsbFd(fd)`. Declare `external fun nativeOnUsbFd(fd: Int)` in the companion object alongside nativeInit. Keep echo logic OUT of Kotlin (CONTEXT decision 3 / RESEARCH Anti-Patterns — Kotlin is glue only).
  </action>
  <verify>
    <automated>cargo ndk -t arm64-v8a build -p android-client 2>/dev/null || cargo build -p android-client --target aarch64-linux-android; cargo test --workspace</automated>
  </verify>
  <done>android-client cdylib cross-compiles with the new `nativeOnUsbFd` entry; manifest + accessory_filter.xml + MainActivity glue in place; identity strings match aoa.rs verbatim; `cargo test --workspace` (host) stays green. Live behavior verified in Task B3.</done>
</task>

<task type="checkpoint:human-verify" gate="blocking">
  <name>Task B3: LIVE 1 MB echo + replug + throughput + record the D1 verdict</name>
  <what-built>The Mac AOA host (`p1_echo`, built with `--features live-usb`) and the Android accessory glue (manifest intent filter + `openAccessory`→`nativeOnUsbFd`→Rust echo). All cable-free logic is already CI-green. This checkpoint runs the link on real hardware (M1 ↔ Pixel 6a over USB-C) — the only steps that genuinely need the phone (CONTEXT scope split; RESEARCH Validation: these are human-verify by necessity).</what-built>
  <how-to-verify>
    Build/install:
    1. Rebuild + install the apk after the manifest change so the OS registers the accessory intent filter (RESEARCH Runtime State Inventory): `./gradlew assembleDebug` (or the project's apk task) then `adb install -r`.
    2. Build the host spike: `cargo build -p macos-host --features live-usb` (yields `target/debug/p1_echo`).

    Run AOA (lead path):
    3. Plug the Pixel 6a into the M1 with the USB-C cable. Run `target/debug/p1_echo`.
    4. On the phone, tap OK on the "Allow USB accessory?" dialog (RESEARCH Pitfall 4; first connect only).
    5. EXPECTED: handshake logs (req 51 returns a version), device re-enumerates as 0x18D1/0x2D0x, interface claims, the 1 MB echo completes BYTE-FOR-BYTE (no mismatch offset printed), and a throughput number in Mbit/s prints.
    6. macOS-claim unknown (A2): if step 5 fails at claim with Access denied / Resource busy, re-run `sudo target/debug/p1_echo`. RECORD whether root was required (feeds D1 friction + the P7 entitlement task).

    Replug reproducibility:
    7. Unplug, replug, re-run `p1_echo` once — confirm it still echoes 1 MB byte-for-byte (proves no stale-handle dependence; Pitfall 2).

    D1 decision + the throughput bar (A3):
    8. Read the throughput number. Bar = ≥ ~200 Mbit/s headroom for 1080p H.264 (CONTEXT / D6).
       - If AOA echoes correctly AND clears the bar → D1 = AOA. Record the number + that root was/wasn't needed.
       - If AOA FAILS at the OS level (cannot claim even with sudo) OR misses the throughput bar → DO NOT mark P1 failed. Run the documented NCM/TCP fallback: enable Pixel USB tethering (Settings ▸ Network ▸ Hotspot & tethering), confirm a macOS NCM interface appears (`ifconfig | grep -iE "en[0-9]|ncm"`), bind a TcpListener on the phone, and run `target/debug/p1_echo --ncm <phone-ip:port>`. Measure that throughput. D1 = NCM/TCP, with rationale = the AOA failure mode + the NCM number (CONTEXT locked decision 6; RESEARCH Open Question 3).
    9. RECORD the D1 verdict + rationale + measured throughput in ROADMAP.md §1 (the D1 row / Phase P1 detail) and in this phase's SUMMARY. This is a D1 *decision*, not a silent bug — surface the verdict explicitly.
  </how-to-verify>
  <resume-signal>Report: did AOA echo 1 MB byte-for-byte? throughput number? root required? replug OK? the D1 verdict (AOA or NCM/TCP) + rationale. Or describe the failure so we adjust.</resume-signal>
</task>

</tasks>

<threat_model>
## Trust Boundaries

| Boundary | Description |
|----------|-------------|
| USB wire → host (`nusb` bulk IN) | Bytes echoed back by the phone cross into the Mac process; length/content untrusted on the wire. |
| Android accessory fd → Rust | The raw fd handed from Kotlin into `nativeOnUsbFd` crosses the FFI/ownership boundary. |
| USB host ↔ accessory (physical link) | A single-purpose, physically-local, user-consented dev link. |

## STRIDE Threat Register

| Threat ID | Category | Component | Disposition | Mitigation Plan |
|-----------|----------|-----------|-------------|-----------------|
| T-P1-01 | Tampering | npm/cargo installs (`nusb`) | mitigate | `nusb` is `[ASSUMED]` (slopcheck sandbox-blocked) → Task B0 blocking-human checkpoint verifies crates.io/docs.rs + `cargo tree` (no surprise C deps) before `cargo add`. Not auto-approvable. |
| T-P1-02 | Denial of Service | `echo_roundtrip` / `read_frame` read path | mitigate | Read into a buffer sized to the KNOWN 1 MiB (`echo_roundtrip` allocates `pattern.len()`); never allocate from an unbounded wire length. `protocol::framing` already guards with `MAX_FRAME_LEN` for any framed path (RESEARCH V5). |
| T-P1-03 | Tampering (memory safety) | `File::from_raw_fd` in `AccessoryFdTransport` | mitigate | Single-ownership: Kotlin `detachFd()` relinquishes the fd; Rust wraps it once via `from_raw_fd` and owns it — no double-close. The single `unsafe` is localized to the constructor (RESEARCH V5 / Security Domain). |
| T-P1-04 | Spoofing/Tampering | malicious USB host/accessory (BadUSB) | accept | Out of scope for a wired single-purpose dev link; the Android USB-accessory permission dialog is the user's OS-enforced consent gate (RESEARCH Security Domain — note, do not mitigate in code). |
| T-P1-05 | Information Disclosure | bytes on the physical USB link | accept | No PII, no secrets, no crypto in scope (RESEARCH V6 N/A) — opaque test pattern over a physically-local cable the user controls. |
</threat_model>

<verification>
Cable-free (Wave A — CI, every commit / wave merge):
- `cargo test -p macos-host transport` — Transport seam, echo_roundtrip, pattern, throughput, chunked.
- `cargo test -p android-client echo_loop` — chunked echo to EOF.
- `cargo test -p macos-host framing` — framing-reuse regression over a Transport.
- `cargo test --workspace` — full suite green, default features, cross-platform, no hardware.

Live build (Wave B — macOS only):
- `cargo build -p macos-host --features live-usb` — AOA host + spike compile (cfg-gated, excluded from CI).
- android-client cross-compiles with the new `nativeOnUsbFd` entry.

Hands-on (Wave B — Task B3 human-verify, by necessity):
- Real 1 MB AOA echo byte-for-byte; replug reproducibility; measured throughput vs the ≥200 Mbit/s bar; D1 verdict (AOA, or documented NCM/TCP fallback) recorded in ROADMAP §1.

Full Req→Test map: see `.planning/phases/P1-usb-byte-round-trip/P1-VALIDATION.md`.
</verification>

<success_criteria>
XPORT-01 met when:
1. **1 MB echoes correctly Mac→phone→Mac (byte-for-byte) over USB-C** — Task B3 step 5 (live), backed by the CI-green `echo_roundtrip`/`make_pattern`/`first_mismatch` logic (Tasks A1/A2).
2. **Round-trip reproduces across cable replug** — Task B3 step 7 (live), backed by the no-stale-handle re-enumeration loop (Task B1, RESEARCH Pitfall 2).
3. **Measured throughput documented (≥ ~200 Mbit/s headroom) AND D1 decided with recorded rationale** — Task B3 steps 8–9; if AOA misses the bar or can't claim, the documented NCM/TCP fallback is run and recorded as the D1 verdict (never a silent fail).

Plus: `Transport` seam is `Read + Write + Send` (D1 swap point intact); `protocol::framing` unchanged and still round-trips over it; `nativeInit` JNI entry intact; `cargo test --workspace` green; all artifacts committed on branch `feat/p1-usb-roundtrip` (no merge without user confirmation).
</success_criteria>

<output>
Create `.planning/phases/P1-usb-byte-round-trip/P1-01-SUMMARY.md` when done.
Record the D1 verdict + measured throughput + whether root was required both in the SUMMARY and in ROADMAP.md §1 / the Phase P1 detail.
Artifacts are committed on branch `feat/p1-usb-roundtrip`; do NOT merge without user confirmation.
</output>
