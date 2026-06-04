---
phase: P1-usb-byte-round-trip
plan: 01
subsystem: infra
tags: [usb, aoa, nusb, transport, jni, android-accessory, ncm, rust]

# Dependency graph
requires:
  - phase: P5-protocol-messages
    provides: "protocol::framing::{write_frame, read_frame} over dyn Read/Write — rides the Transport seam unchanged"
provides:
  - "Transport: Read + Write + Send seam (blanket impl) — the D1 swap point shared by AOA, accessory-fd, and NCM/TCP"
  - "Pure cable-free echo core: make_pattern, first_mismatch, echo_roundtrip, mbit_per_sec, EchoStats"
  - "Android echo_loop (chunked read→write-back to EOF) + AccessoryFdTransport (single-ownership fd wrap)"
  - "Mac AOA host adapter (handshake 51/52/53, re-enumeration retry, AoaTransport over nusb bulk) + NcmTransport — cfg live-usb"
  - "p1_echo spike binary (AOA default + --ncm fallback) — cfg live-usb, compiles, not yet run on hardware"
  - "Android accessory glue: nativeOnUsbFd JNI, accessory_filter.xml, manifest intent-filter, MainActivity openAccessory→detachFd"
affects: [P4-decode, P5-live-pipeline, P6-touch, P7-purity-hardening]

# Tech tracking
tech-stack:
  added: ["nusb 0.2.3 (optional, macOS-only, behind live-usb feature)"]
  patterns:
    - "Transport seam IS std::io (Read + Write + Send) — no send/recv reinvention; framing reuse for free"
    - "Live USB code cfg-gated behind a live-usb feature so default CI stays cross-platform and nusb-free (mirrors P3 macOS gating)"
    - "Identity strings defined ONCE (aoa.rs IDENTITY) and mirrored verbatim into accessory_filter.xml"

key-files:
  created:
    - crates/macos-host/src/transport.rs
    - crates/macos-host/src/aoa.rs
    - crates/macos-host/src/bin/p1_echo.rs
    - crates/android-client/src/transport.rs
    - android/app/src/main/res/xml/accessory_filter.xml
  modified:
    - crates/macos-host/src/lib.rs
    - crates/macos-host/Cargo.toml
    - crates/android-client/src/lib.rs
    - android/app/src/main/AndroidManifest.xml
    - android/app/src/main/java/com/rustscreen/client/MainActivity.kt

key-decisions:
  - "Transport = Read + Write + Send blanket impl (CONTEXT D2 / RESEARCH Pattern 4) — drop-in for protocol::framing, satisfied by nusb endpoints, accessory File, and TcpStream alike"
  - "nusb gated as an optional macOS-only dep behind a live-usb feature; default workspace build pulls no nusb"
  - "AoaTransport wraps nusb EndpointRead/EndpointWrite at a 16 KiB transfer size (Pitfall 3); endpoint addresses read from the interface descriptor, never hard-coded"

patterns-established:
  - "Cable-free TDD core + cfg-gated live adapters: the same echo_roundtrip drives loopback fakes (CI), AOA bulk, and NCM/TCP"
  - "JNI fd handoff: Kotlin detachFd() → nativeOnUsbFd → single unsafe from_raw_fd → Rust echo_loop (single-ownership, T-P1-03)"

requirements-completed: []  # XPORT-01 NOT yet complete — pending the live B3 hardware checkpoint (D1 verdict + throughput unmeasured)

# Metrics
duration: ~45min
completed: 2026-06-03
---

# Phase P1 Plan 01: USB Byte Round-Trip Summary

**Cable-free `Transport: Read + Write + Send` seam with TDD'd 1 MiB echo/verify/throughput core, plus the cfg-gated Mac AOA host (nusb handshake + bulk transport) and Android accessory glue (nativeOnUsbFd JNI + manifest/filter) — all compiling, awaiting the live hardware echo (Task B3).**

## Status: PARTIAL — Wave A + Wave B code complete; Task B3 (live hardware) deliberately NOT executed

This session executed the **buildable** parts of P1-01: Wave A (cable-free, TDD) and the Wave B code that compiles without the phone. The plan's success criteria #1–#3 (live 1 MB byte-for-byte echo, replug reproducibility, measured throughput + D1 verdict) require the Pixel 6a on USB-C and are gated behind **Task B3 (`checkpoint:human-verify`)**, which was intentionally left for the user. **XPORT-01 is therefore not yet satisfied** and the plan is not marked complete.

## Performance

- **Duration:** ~45 min
- **Started:** 2026-06-03
- **Completed (buildable scope):** 2026-06-03
- **Tasks executed:** 5 of 6 (A1, A2, B0, B1, B2) — B3 stopped before, by instruction
- **Files created/modified:** 10

## Accomplishments
- **A1 — Transport seam + echo core (TDD):** `Transport: Read + Write + Send` blanket impl, `make_pattern`, `first_mismatch`, `echo_roundtrip` (bounded `pattern.len()` allocation — DoS guard T-P1-02), `mbit_per_sec`, `EchoStats`; 9 tests incl. 1 MiB loopback echo, corruption detection, 16 KiB chunked partial-read reconciliation, throughput math, and a `protocol::framing` regression over the seam.
- **A2 — Android echo_loop (TDD):** chunked read→write-back to EOF, host-tested via an in-memory duplex fake; `AccessoryFdTransport` (cfg android) wrapping the detached fd once.
- **B0 — nusb dep gate:** added `nusb 0.2.3` as an optional, macOS-only dep behind a `live-usb` feature. Confirmed default build pulls NO nusb; `cargo tree` shows only IOKit/CoreFoundation FFI (no bundled C/libusb).
- **B1 — Mac AOA host (cfg live-usb):** `handshake` (req 51 proto≥1 / 52 six identity strings / 53 start), `reacquire` (poll for 0x18D1/0x2D00..=0x2D05, open a NEW handle), `open_transport` (claim iface 0, read bulk endpoint addrs from descriptor), `AoaTransport` (Read+Write+Send over nusb bulk), `NcmTransport(TcpStream)` fallback, and the `p1_echo` spike (AOA default + `--ncm <host:port>`). Compiles under `--features live-usb`.
- **B2 — Android glue:** `nativeOnUsbFd(fd)` JNI (mirrors `nativeInit`, runs `echo_loop`), `accessory_filter.xml` (manufacturer/model/version = aoa.rs IDENTITY verbatim), manifest `USB_ACCESSORY_ATTACHED` intent-filter + matching `<meta-data>` (accessory constant, `@xml/accessory_filter`), and `MainActivity.kt` `openAccessory`→`detachFd`→`nativeOnUsbFd`. Cross-compiles for arm64-v8a.

## Task Commits

1. **A1 RED** — `cc69951` (test): failing transport seam + echo/pattern/throughput tests
2. **A1 GREEN** — `b0e0f85` (feat): Transport seam + echo/pattern/throughput impl (9 tests pass)
3. **A2 RED** — `78bb8a3` (test): failing android echo_loop tests
4. **A2 GREEN** — `ba948db` (feat): android echo_loop core impl (2 tests pass)
5. **B0** — `e20c4dc` (chore): gate nusb behind live-usb feature (macOS-only, optional)
6. **B1** — `573514f` (feat): Mac AOA host adapter + p1_echo spike (cfg live-usb)
7. **B2** — `53b1ec4` (feat): Android accessory glue — nativeOnUsbFd JNI + manifest/filter

## TDD Gate Compliance
A1 and A2 each followed RED → GREEN: failing tests committed first (`cc69951`, `78bb8a3`), watched fail (all panicked "not implemented: RED"), then the implementation committed (`b0e0f85`, `ba948db`). No REFACTOR commits were needed.

## Test Counts (before → after)
| Crate | Before | After | Delta |
|-------|--------|-------|-------|
| macos-host | 26 | 35 | +9 (transport + framing regression) |
| android-client | 0 | 2 | +2 (echo_loop) |
| protocol | 51 | 51 | 0 (unchanged — framing untouched) |
| **workspace total** | **77** | **88** | **+11** |

## Verification Status
- `cargo test --workspace`: **green** (88 tests, default features, cross-platform, no hardware).
- `cargo build --workspace` (default): **clean, pulls no nusb** (`cargo tree` confirms).
- `cargo build -p macos-host --features live-usb`: **compiles** (AOA host + p1_echo).
- `cargo clippy --workspace --all-targets -- -D warnings`: **clean** (default).
- `cargo clippy -p macos-host --features live-usb --all-targets -- -D warnings`: **clean**.
- `cargo ndk -t arm64-v8a build -p android-client` + android clippy: **clean** (Android cdylib cross-compiles with the new `nativeOnUsbFd`).
- `cargo fmt --check`: **clean**.

## Decisions Made
- None beyond the plan-specified seam shape. The plan's RESEARCH illustrative ControlIn example omitted the `timeout: Duration` argument that nusb 0.2.3 actually requires; verified against the vendored nusb 0.2.3 source and used `control_in(ControlIn{..}, CONTROL_TIMEOUT)`. This is faithful to the locked API, not a deviation.

## Deviations from Plan
None — plan executed exactly as written for the in-scope tasks (A1, A2, B0, B1, B2). protocol::framing and nativeInit were left untouched; no `git add .`, no push, no merge.

## Issues Encountered
- nusb 0.2.3 `MaybeFuture` requires the trait in scope for `.wait()`, and `control_in/out` take an explicit `timeout`. Both resolved by reading the vendored nusb 0.2.3 source directly (ctx7 unavailable in this environment); the live-usb build then compiled clean.

## User Setup Required
See plan `user_setup` and the Task B3 checkpoint:
- **android-usb-accessory:** on first attach, tap OK on the on-phone "Allow USB accessory?" dialog (openAccessory returns null until granted).
- **macos-usb-claim:** if `claim_interface` fails with Access denied / Resource busy, re-run `sudo target/debug/p1_echo` and record whether root was required (feeds D1 friction + P7 entitlement task).

## Next Phase Readiness / Remaining Work (Task B3 — hands-on, requires the phone)
The live verification was deliberately NOT run. To complete XPORT-01 / resolve D1, the user (or a hardware session) must:
1. `cargo build -p macos-host --features live-usb`; build+install the apk (`./gradlew assembleDebug` then `adb install -r`) so the OS registers the accessory intent filter.
2. Plug in the Pixel 6a, run `target/debug/p1_echo`, tap OK on the permission dialog.
3. Confirm the 1 MB echo completes byte-for-byte and read the printed Mbit/s.
4. Re-run after a replug to prove reproducibility.
5. If AOA can't claim even with sudo, or misses ~200 Mbit/s → run the NCM/TCP fallback (`p1_echo --ncm <phone-ip:port>`).
6. **Record the D1 verdict (AOA or NCM/TCP) + rationale + measured throughput + whether root was required** in ROADMAP §1 (Phase P1 detail) and update this SUMMARY.

Until B3 is done, P1 stays in progress and XPORT-01 is unsatisfied.

## Self-Check: PASSED
All 6 created files verified present; all 7 task commits verified in git history.

---
*Phase: P1-usb-byte-round-trip*
*Buildable scope completed: 2026-06-03 — Task B3 (live hardware) pending user*
