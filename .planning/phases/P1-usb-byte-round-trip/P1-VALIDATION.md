# P1 Validation — USB Byte Round-Trip

**Phase:** P1-usb-byte-round-trip
**Requirement:** XPORT-01
**Generated:** 2026-06-03
**Nyquist validation:** enabled (no `.planning/config.json` → treated as enabled).

> Every XPORT-01 success criterion maps either to an automated test (Wave A, cable-free, CI)
> or — where the behavior is inherently un-headless-testable (real USB enumeration, the on-phone
> permission dialog, the live 1 MB echo, replug, throughput) — to a hands-on `checkpoint:human-verify`
> gate (Wave B). The pure logic (pattern gen/verify, echo, chunking, throughput math, framing reuse)
> is fully covered in CI; only the three hardware criteria are human-verify by necessity.

## Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[cfg(test)]` / `cargo test` (no external test crate) |
| Config file | none — inline test modules (as in `framing.rs`) |
| Quick run | `cargo test -p macos-host transport` |
| Android logic | `cargo test -p android-client echo_loop` |
| Full suite | `cargo test --workspace` |
| Live build (macOS only) | `cargo build -p macos-host --features live-usb` |

## Requirement → Criterion → Test Map

| Req | Criterion | Behavior | Test type | Automated command / gate | Task | Artifact |
|-----|-----------|----------|-----------|--------------------------|------|----------|
| XPORT-01 | #1 (logic) | 1 MiB deterministic pattern generates + verifies byte-for-byte; first-mismatch offset on failure | unit (TDD, new) | `cargo test -p macos-host transport` | A1 | `macos-host/src/transport.rs` |
| XPORT-01 | #1 (logic) | echo_roundtrip over a LoopbackTransport returns Ok with the bytes intact; corruption → Err | unit (TDD, new) | `cargo test -p macos-host transport` | A1 | `macos-host/src/transport.rs` |
| XPORT-01 | #1 (logic) | partial/chunked reads (≤16 KiB/read) still round-trip the full 1 MiB (Pitfall 3) | unit (TDD, new) | `cargo test -p macos-host transport` | A1 | `macos-host/src/transport.rs` |
| XPORT-01 | #1 (logic) | Android echo_loop reads chunks and writes them straight back to EOF, accumulating the full 1 MiB | unit (TDD, new) | `cargo test -p android-client echo_loop` | A2 | `android-client/src/transport.rs` |
| XPORT-01 | #3 (logic) | throughput math: bytes + elapsed → Mbit/s asserted against a known value (Pitfall 5) | unit (TDD, new) | `cargo test -p macos-host transport` | A1 | `macos-host/src/transport.rs` |
| XPORT-01 | seam | protocol::framing still round-trips over a Transport (P5 compatibility regression) | unit (TDD, new) | `cargo test -p macos-host framing` | A2 | `macos-host/src/transport.rs` + `protocol/src/framing.rs` |
| XPORT-01 | seam (cross-platform) | default-feature workspace stays green; aoa/p1_echo excluded so CI needs no USB/hardware | suite | `cargo test --workspace` | A1/A2/B1/B2 | workspace |
| XPORT-01 | dep gate | `nusb` ([ASSUMED]) verified on crates.io/docs.rs + `cargo tree` before install | manual (blocking-human) | gate — not auto-approvable | B0 | `macos-host/Cargo.toml` |
| XPORT-01 | live build | AOA host + spike compile under the live-usb feature (macOS) | build | `cargo build -p macos-host --features live-usb` | B1 | `macos-host/src/aoa.rs`, `bin/p1_echo.rs` |
| XPORT-01 | live build | android-client cdylib cross-compiles with the new `nativeOnUsbFd` JNI entry | build | `cargo ndk -t arm64-v8a build -p android-client` | B2 | `android-client/src/lib.rs` |
| XPORT-01 | **#1** | **real 1 MB echo Mac→phone→Mac byte-for-byte over USB-C (AOA)** | manual (hands-on) | run `p1_echo` with phone attached | B3 | n/a — `checkpoint:human-verify` |
| XPORT-01 | **#2** | **reproducible across one cable replug** | manual (hands-on) | replug, re-run `p1_echo` | B3 | n/a — `checkpoint:human-verify` |
| XPORT-01 | **#3** | **throughput ≥ ~200 Mbit/s documented + D1 (AOA vs NCM/TCP) decided + recorded in ROADMAP §1** | manual (hands-on) | read `p1_echo` output; NCM fallback if AOA fails/misses bar; record verdict | B3 | n/a — `checkpoint:human-verify` |

## Hardware Unknowns Settled by This Phase (feed D1)

| # | Unknown | Where settled | Recorded as |
|---|---------|---------------|-------------|
| A2 | Can `nusb` claim the Pixel interface on macOS without root? | Task B3 step 6 (try plain claim, then `sudo`) | "root required: yes/no" in SUMMARY + D1 friction column; feeds P7 entitlement task |
| A3 | Does AOA bulk clear ~200 Mbit/s? | Task B3 step 8 (measured echo throughput) | the Mbit/s number in SUMMARY + ROADMAP §1; primary D1 input |
| A4 | Does the Pixel 6a present a usable NCM interface on this M1? | Task B3 step 8 fallback (only if AOA fails) | confirmed/not in SUMMARY if the fallback is exercised |

## Sampling Rate
- **Per task commit (Wave A):** `cargo test -p macos-host transport` (+ `-p android-client echo_loop` for A2).
- **Per wave merge:** `cargo test --workspace`.
- **Phase gate:** full default-feature suite green (cable-free) AND `cargo build -p macos-host --features live-usb` compiles, BEFORE the Task B3 live run; then the three live human-verify criteria + the D1 verdict.

## Fallback Policy (do NOT silently fail)
If AOA cannot claim the interface even under `sudo`, or misses the ~200 Mbit/s bar, the executor runs the documented NCM/TCP path (`p1_echo --ncm`) and records **NCM/TCP** as the D1 verdict with the AOA failure mode + the NCM throughput as rationale. P1 is only "blocked" if BOTH AOA and the NCM fallback fail on hardware — and that is surfaced to the user as a D1 decision, not a bug (CONTEXT locked decision 6).
