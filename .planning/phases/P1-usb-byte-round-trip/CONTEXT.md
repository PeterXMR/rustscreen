# Phase P1: USB Byte Round-Trip — Context

**Gathered:** 2026-06-03
**Status:** Ready for planning
**Source:** Orchestrator-synthesized from ROADMAP.md P1 + master roadmap §215 + the D1 decision + RESEARCH.md. Hardware now available (M1 ↔ Pixel 6a over USB-C). discuss-phase skipped: the spike is well-specified and the open questions (macOS USB claim, AOA throughput) are exactly what the spike resolves, not things to pre-decide.

**Requirement:** XPORT-01

<domain>
## Phase Boundary

**Delivers:** A raw byte transport over the single USB-C cable, proven by a **1 MB echo Mac→phone→Mac, verified byte-for-byte**, reproducible across replug, with **measured throughput**, behind a swappable `Transport` seam — and **D1 (AOA vs NCM/TCP) decided with recorded rationale**.

**Does NOT include:** sending video/protocol frames for real (that's P5-live), capture/encode (P3), decode (P4). P1 moves opaque bytes only. The `protocol::framing`/`messages` codec already exists and will ride this transport later — P1 just proves the pipe.
</domain>

<locked_decisions>
## Locked Decisions

1. **Lead = AOA (Android Open Accessory)** via **`nusb` 0.2.3** (pure-Rust, host side on the Mac). Handshake: control req 51 (get protocol) → req 52 (six identity strings) → req 53 (start) → device re-enumerates as VID `0x18D1` / PID `0x2D00`–`0x2D05`; re-open the new handle (do not reuse), `claim_interface`, bulk IN/OUT. **Fallback = NCM/TCP** (viable on Apple Silicon — Pixel 6a tethers via NCM, natively supported by macOS; avoids the dead RNDIS/HoRNDIS kext path).

2. **`Transport` seam:** `trait Transport: Read + Write + Send` (thin newtype/blanket). Reuse `protocol::framing::{write_frame, read_frame}` (already `dyn Read`/`dyn Write`). `nusb` endpoints, the Android accessory `File`, and `TcpStream` all already satisfy `Read + Write`. Lives in `macos-host/src/transport.rs` and `android-client/src/transport.rs`. This is the D1 swap point.

3. **Android accessory side = thin Kotlin glue (D0 simplest-now):** Kotlin registers the `USB_ACCESSORY_ATTACHED` intent + `accessory_filter.xml`, calls `UsbManager.openAccessory()` → `ParcelFileDescriptor` → `pfd.detachFd()` → `nativeOnUsbFd(fd)` JNI into Rust, which wraps it `File::from_raw_fd` and runs the echo. Pure-`jni`→`UsbManager` is the explicit P7 (PURITY-01) swap, NOT now. `jni` already at 0.21 — no bump.

4. **`nusb` is the only new dep** → gated behind one `checkpoint:human-verify` (slopcheck execution was sandbox-blocked, so `[ASSUMED]`; verified on crates.io: 771k downloads, kevinmehall/nusb).

5. **TDD** for all cable-free logic. **The live AOA parts are `checkpoint:human-verify`** (real enumeration, on-phone permission dialog, the live 1 MB echo, replug, throughput numbers) — they cannot be unit-tested.

6. **Two HIGH hardware unknowns the spike must settle (feed D1):**
   - **macOS interface claim:** can `nusb` claim the Pixel interface without the `com.apple.vm.device-access` entitlement, or does the CLI spike need `sudo`? (RESEARCH A2)
   - **Throughput:** does AOA bulk clear the ~200 Mbit/s bar? If not → NCM/TCP. (RESEARCH A3)
   If AOA is unworkable at the OS level, fall back to NCM/TCP before declaring P1 blocked (do NOT silently fail like a normal bug — surface it, it's a D1 decision).
</locked_decisions>

<existing_work>
## Builds on
- `crates/protocol/src/framing.rs` — `write_frame`/`read_frame` over `dyn Read`/`Write` (the echo can use raw bytes or framed; spike uses raw 1 MB + a length check).
- `crates/android-client/src/lib.rs` — JNI entry pattern (`#[no_mangle] extern "system"`, `#[cfg(target_os="android")]`); add `nativeOnUsbFd`.
- `android/app/.../MainActivity.kt` + `AndroidManifest.xml` — add the accessory intent filter + `openAccessory` glue.
- `crates/macos-host/src/main.rs` — wire the host echo driver.
</existing_work>

<scope_split>
## Cable-free (TDD, CI-now) vs hands-on (you + phone)
- **Cable-free:** 1 MB test-pattern generation + byte-for-byte verification; `echo_roundtrip` over an in-memory `LoopbackTransport` fake; chunked/partial bulk-read handling; throughput-measurement math (bytes/elapsed → Mbit/s); `Transport` trait + framing-reuse regression. All `cargo test` green, no hardware.
- **Hands-on (`checkpoint:human-verify`):** install `nusb` (gated); the real AOA enumeration + handshake on the Mac (maybe `sudo`); the on-phone "Allow USB accessory?" dialog; the live 1 MB echo; replug reproducibility; record throughput + the **D1 verdict** into ROADMAP §1.
</scope_split>

<discretion>
## Claude's Discretion
- Exact `Transport` trait shape (newtype vs blanket impl) and whether the spike echoes raw 1 MB or length-framed.
- Whether the Mac host spike is a `--example` or a subcommand of `macos-host`.
- Loopback fake design for the cable-free tests.
</discretion>
