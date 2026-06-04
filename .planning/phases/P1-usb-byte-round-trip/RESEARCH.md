# Phase P1: USB Byte Round-Trip — Research

**Researched:** 2026-06-03
**Domain:** USB transport Mac↔Android — Android Open Accessory (AOA) over `nusb` (Mac host side) + `UsbManager.openAccessory()` via `jni` (Android side); network-over-USB (NCM/TCP) fallback. The `Transport` seam.
**Confidence:** MEDIUM. The AOA *protocol* (control reqs 51/52/53, VID/PID re-enumeration) is HIGH-confidence (official AOSP spec). The `nusb` 0.2 API surface is HIGH (docs.rs verified). What is genuinely unverifiable without the cable plugged in: (a) whether macOS lets `nusb` claim the Pixel's bulk interface without an entitlement, (b) the actual throughput, (c) whether the Pixel 6a's NCM tethering presents a usable interface on this M1. These are exactly the things sub-spike A/B exist to settle on hardware.

## Summary

P1 proves bytes move both ways Mac↔Pixel over one USB-C cable and resolves **D1** (AOA vs network-over-USB) on measured throughput/reliability. The lead path (sub-spike A) is **Android Open Accessory**: the Mac acts as USB *host*, drives the AOA handshake (control requests 51/52/53) over `nusb` 0.2.3 (pure-Rust libusb alternative), the Pixel re-enumerates with Google's VID `0x18D1` / PID `0x2D00`–`0x2D05` exposing two bulk endpoints, and the Mac bulk-transfers a 1 MB known pattern that the phone echoes back. On the phone, the unavoidable JNI touch (D0/§0.1) is `UsbManager.openAccessory()` → `ParcelFileDescriptor` → a raw `fd` that Rust reads/writes with `std::fs::File`.

The single most important design output of this phase is the **`Transport` seam**: a Rust trait that both AOA and NCM implement so D1's verdict is a one-line swap and the rest of the codebase (framing, encode TX, decode RX) never sees USB. The recommendation is to make `Transport` simply be **`Read + Write`** — because the project's existing `protocol::framing::{write_frame, read_frame}` already operate over `&mut dyn Read`/`&mut dyn Write`, an `EndpointRead`/`EndpointWrite` pair (nusb) and a `std::fs::File` (Android accessory fd) and a `TcpStream` (NCM) are *all* already `Read + Write`. No new abstraction is needed beyond a thin newtype that owns the device handles.

A large fraction of P1 is **cable-free and TDD-able today**: the echo verification logic, the 1 MB pattern generator/checker, throughput-measurement math, partial-read/chunking loops, and the whole thing exercised against an in-memory loopback `Transport` fake — all unit-testable in CI with no phone. Only the AOA enumeration, the on-phone permission dialog, the real 1 MB echo, replug reproducibility, and throughput numbers genuinely need hardware. The executor should TDD the cable-free core green, then gate the live steps behind a `checkpoint:human-verify`.

**Primary recommendation:** Lead with **AOA via `nusb` 0.2.3** on the Mac and a **thin Kotlin glue** (`openAccessory()` → fd handed to Rust) on the phone for the MVP — consistent with D0 (Kotlin shell is the simplest-now adapter; pure `jni`→`UsbManager` is the P7 purity swap). Implement the **NCM/TCP fallback** as a second `Transport` impl, attempted only if AOA fails to claim the interface on macOS or misses the ~200 Mbit/s bar. Decide D1 on measured throughput + setup reliability and record the rationale.

## User Constraints

> No CONTEXT.md exists for this phase yet. The constraints below are the locked decisions from the authoritative roadmap (`docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md` §1) and standing user preferences from project memory. A later `discuss-phase` may write a CONTEXT.md that supersedes these.

### Locked Decisions (roadmap §1)
- **D0 — Simplest-now, shrink-to-Rust-later (LOCKED):** every platform boundary sits behind a Rust trait/FFI seam; ship the simplest adapter that works for the MVP; swap to pure-Rust later **without touching core logic**. For P1: the `Transport` trait is the stable port; AOA and NCM are swappable adapters; on Android the **Kotlin shell doing `openAccessory()` and handing Rust the fd is the simplest-now adapter** (the pure `jni`→`UsbManager` call is the P7 purity target per §0.1).
- **D1 — USB transport (LOCKED to "spike both, lead AOA"):** Spike AOA *and* network-over-USB in P1, **lead with AOA**, pick on measured throughput/reliability. This phase **resolves** D1 and must record the rationale.
- **D6 — Display geometry (default):** 2400×1080 @ 60 Hz. Drives the throughput bar: 1080p H.264 at 60 fps needs ≥ ~200 Mbit/s headroom — the metric the echo throughput must clear.

### Claude's Discretion (within the locked seams)
- The exact `Transport` trait shape (`Read + Write` super-trait vs explicit `send`/`recv`) — recommendation below (lead: `Read + Write`).
- AOA identity strings (manufacturer/model/description) sent in control request 52 — must match the Android `accessory_filter.xml` exactly; specific values are ours to pick.
- Bulk transfer chunk size (bounded by the 16 KiB AOA buffer — see Pitfalls).
- Whether the on-phone echo loop lives in Kotlin (simplest) or Rust-over-fd (closer to D0 end state) for the *spike specifically*.
- Sync (`.wait()`) vs async (`tokio`) nusb usage for the spike (lead: blocking `.wait()` — no runtime needed for a spike).

### Deferred Ideas (OUT OF SCOPE for P1)
- Pure `jni`→`UsbManager` accessory open with zero Kotlin (P7, PURITY-01).
- Hotplug / auto-reconnect / 20-replug robustness (P7, ROBUST-01) — P1 only needs *one* manual replug to prove reproducibility.
- Streaming real H.264 frames (P5). P1 echoes an opaque 1 MB byte pattern only.
- The "remember this accessory" permission UX polish (P7, R7).
- Touch back-channel over the same link (P6).

## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| XPORT-01 | 1 MB echoes correctly both directions Mac↔Pixel over USB-C, reproducibly across replug, with the chosen mechanism (AOA lead, NCM/TCP fallback), measured throughput documented (≥ ~200 Mbit/s headroom for 1080p H.264). Resolves D1. | AOA handshake + bulk transfer (Standard Stack §nusb, Architecture Pattern 1–2), Android accessory fd (Pattern 3), `Transport` seam (Pattern 4), echo + throughput logic (cable-free, §Cable-Free split), NCM fallback (Pattern 5), D1 decision rationale (§State of the Art + Open Questions), pitfalls (re-enumeration race, 16 KiB buffer, partial reads, macOS interface claim). |

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Enumerate USB devices, find the Pixel | macOS host (`nusb`) | — | The Mac is the USB *host*; enumeration is a host-side libusb-class operation. |
| Drive AOA handshake (control 51/52/53) | macOS host (`nusb` control transfers) | — | Only the host issues control requests; the phone responds passively. |
| Re-acquire device after re-enumeration | macOS host (`nusb` `list_devices`/`watch_devices`) | — | The VID/PID changes; the host must re-find and re-open the accessory-mode device. |
| Bulk read/write the byte stream | macOS host (`nusb` `Endpoint<Bulk, In/Out>`) | — | Bulk endpoints are exposed by the device; host owns the transfers. |
| Receive the accessory connection, get the fd | Android Kotlin shell (`UsbManager.openAccessory`) | Rust-via-`jni` (P7 target) | `UsbManager` is a Java/Android API; D0 puts the simplest adapter (Kotlin glue) here for MVP. |
| Read/write the accessory fd | Android Rust (`std::fs::File` over the fd) | — | Once Rust holds the raw fd, transport I/O is plain `std::io` — platform-agnostic. |
| Echo loop (read N, write N back) | Android Rust core (over the `Transport`) | Kotlin (spike-only shortcut) | Echo is trivial logic; belongs in the Rust core behind the `Transport` seam. |
| 1 MB pattern generate + verify byte-for-byte | platform-agnostic Rust (pure, testable) | — | No platform dependency — pure function, TDD in CI. |
| Throughput measurement (bytes/elapsed) | platform-agnostic Rust (pure math) | — | Timing wraps the transfer; the math is pure and testable. |
| NCM fallback: bring up tether, TCP socket | Android (toggle tether) + host (`TcpStream`) | — | Network-over-USB uses the OS tether stack + std sockets; minimal custom code. |
| The `Transport` seam (trait) | platform-agnostic `protocol` or per-crate `transport.rs` | — | The whole point of D1 swappability; the trait is pure Rust. |

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `nusb` | 0.2.3 `[VERIFIED: crates.io — 771k downloads, repo github.com/kevinmehall/nusb, updated 2026-03-10]` | Mac host-side USB: enumerate, control transfers (AOA handshake), bulk IN/OUT. Pure Rust, no libusb C dependency. | The de-facto pure-Rust libusb alternative; explicitly named in roadmap §Tech Stack. Avoids the C `libusb` + macOS kext/entitlement pain where possible by using IOKit/IOUSBHost directly. |
| `jni` | 0.21 (in repo) → 0.22.4 available `[VERIFIED: crates.io — 121M downloads]` | Android side: call `UsbManager.openAccessory()` from Rust (the P7 pure path) and, today, the existing `nativeInit` JNI bridge. | Already a dependency in `crates/android-client/Cargo.toml`. The standard Rust↔JVM FFI crate. Keep pinned at 0.21 unless a 0.22 API is needed (avoid churn this phase). |
| `std::fs::File` / `std::io` | std | Android: read/write the accessory `ParcelFileDescriptor`'s raw fd; the universal `Read + Write` surface the `Transport` trait builds on. | No dependency; `OwnedFd`/`FromRawFd` is the idiomatic way to wrap an OS fd. |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| `std::net::{TcpListener, TcpStream}` | std | NCM/TCP fallback (sub-spike B) transport. | Only if AOA fails to claim on macOS or misses the throughput bar. Zero USB code. |
| `ndk` / `ndk-sys` | 0.9 / 0.6 (in repo) | Android native glue already present; not strictly needed for P1 transport but available if reading the fd needs ALooper integration later. | Probably not needed for the P1 echo spike. |
| `log` / `android_logger` | 0.4 / 0.14 (in repo) | Log AOA handshake steps, byte counts, throughput on both ends; `adb logcat` is the phone-side observability. | Throughout the spike for diagnosing the re-enumeration race and partial reads. |
| `tokio` (optional, nusb feature) | — | Async nusb if a runtime is wanted. | **Not recommended for the spike** — use nusb's blocking `.wait()` instead; no runtime, simpler. |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| `nusb` 0.2 | `rusb`/`libusb` (C bindings) | `rusb` is mature but pulls a C dependency and hits the documented macOS "cannot claim interface" kext story harder; contradicts the project's pure-Rust goal. Keep as a last-resort if `nusb` can't claim the Pixel on macOS. |
| Kotlin `openAccessory()` glue | Pure `jni`→`UsbManager` from Rust now | Pure-Rust is the *end state* (P7 PURITY-01) but adds JNI plumbing risk to a spike whose job is to prove the *transport*, not purity. D0 says ship the simplest adapter — Kotlin glue — first. |
| AOA (host-drives-phone) | NCM/TCP (network-over-USB) | This *is* the D1 fork. AOA is lower-level/lower-overhead (raw bulk) but has permission-dialog + macOS-claim risk; NCM is dead-simple (sockets) but depends on macOS bringing up the tether interface and adds a TCP/IP stack between the apps. P1 measures both. |

**Installation (Mac host crate — add to `crates/macos-host/Cargo.toml`):**
```toml
[target.'cfg(target_os = "macos")'.dependencies]
nusb = "0.2"          # [ASSUMED until checkpoint] verify: cargo add nusb && cargo tree
```
The Android side needs **no new crate** for the simplest-now (Kotlin glue) path — `jni`/`ndk` are already present.

**Version verification (run at planning/execution, gate installs behind a checkpoint):**
```bash
cargo add nusb --dry-run -p macos-host    # confirm 0.2.x resolves
cargo tree -p macos-host | grep -i nusb   # confirm no surprise C deps
```

## Package Legitimacy Audit

> slopcheck was **installed successfully** (`slopcheck 0.6.1`) but its execution was **blocked by the environment sandbox** (it performs its own outbound network calls, which the harness denied). Per the graceful-degradation rule, packages that could not be machine-verified are tagged `[ASSUMED]` and the planner must gate each *new* install behind a `checkpoint:human-verify`. Note both packages were independently confirmed on crates.io via WebFetch (download counts + repo URLs), which is corroborating but not a substitute for slopcheck.

| Package | Registry | Age / Updated | Downloads | Source Repo | slopcheck | Disposition |
|---------|----------|---------------|-----------|-------------|-----------|-------------|
| `nusb` | crates.io | updated 2026-03-10 | 771,407 lifetime | github.com/kevinmehall/nusb | not run (sandbox-blocked) | Approved as `[ASSUMED]` — planner gates install behind `checkpoint:human-verify` |
| `jni` | crates.io | since 2016 | 121,292,262 lifetime | (madsmtm/jni-rs lineage) | not run (sandbox-blocked) | Already in repo (0.21) — no new install; low risk |

**Packages removed due to slopcheck [SLOP] verdict:** none.
**Packages flagged as suspicious [SUS]:** none (slopcheck did not run; both packages have strong corroborating signals — high download counts, named in the project's own roadmap, public repos).

*The only genuinely new dependency this phase introduces is `nusb` on the Mac host. The planner must add a `checkpoint:human-verify` task before `cargo add nusb` (confirm version, no unexpected C/transitive deps, repo is kevinmehall/nusb).*

## Architecture Patterns

### System Architecture Diagram

```
                          MacBook M1 (macos-host, USB HOST)
   ┌─────────────────────────────────────────────────────────────────────────┐
   │  spike_main                                                               │
   │     │ 1. nusb::list_devices() ── find Pixel (any VID/PID, by attrs)       │
   │     ▼                                                                     │
   │  AOA handshake (Pattern 1)                                                │
   │     ├─ control_in  req 51  → protocol version (u16 LE)                    │
   │     ├─ control_out req 52  → strings[0..5] (mfr/model/desc/ver/uri/serial)│
   │     └─ control_out req 53  → START accessory                              │
   │     │                                                                     │
   │     ▼  device DROPS off the bus, RE-ENUMERATES  (Pattern 2 — the race)    │
   │  re-acquire: list_devices()/watch_devices() until VID 0x18D1 PID 0x2D0x   │
   │     │ claim interface → Endpoint<Bulk,Out> + Endpoint<Bulk,In>            │
   │     ▼                                                                     │
   │  AoaTransport { ep_out, ep_in }  ── impls Read + Write ──┐                │
   │     │                                                    │                │
   │  echo_roundtrip(&mut transport, 1 MiB pattern):          │                │
   │     write_all(pattern) ─chunked ≤16 KiB─► [USB bulk OUT] │                │
   │     read_exact(&mut got) ◄─chunked──────── [USB bulk IN] │  (Pattern 4    │
   │     assert got == pattern  (byte-for-byte)               │   seam: same   │
   │     throughput = bytes / elapsed  (pure, tested)         │   code for     │
   └──────────────────────────────────────────────────────────┘   NCM path)   │
                                  │  USB-C ↔ USB-C
   ┌──────────────────────────────▼────────────────────────── Pixel 6a (device)┐
   │  Kotlin MainActivity (simplest-now adapter, D0)                            │
   │     USB_ACCESSORY_ATTACHED intent (accessory_filter.xml)                   │
   │        │ UsbManager.openAccessory(accessory) → ParcelFileDescriptor        │
   │        ▼ fd                                                                │
   │     nativeOnUsbFd(fd)  ──JNI──►  Rust android-client                       │
   │        │                                                                   │
   │     File::from_raw_fd(fd)  ── impls Read + Write ──┐  (same Transport seam)│
   │        ▼                                           │                       │
   │     echo loop:  loop { n = read(buf); write_all(&buf[..n]) }               │
   │        (read a chunk off the OUT pipe, write it straight back to IN)       │
   └───────────────────────────────────────────────────────────────────────────┘

   FALLBACK (sub-spike B, Pattern 5):  enable USB tethering on Pixel → macOS
   brings up an NCM interface → phone binds TcpListener on usb0, Mac TcpStream
   connects → identical echo_roundtrip() over the TcpStream (also Read+Write).
```

### Recommended Project Structure
```
crates/
├── macos-host/src/
│   ├── transport.rs        # NEW: Transport trait + AoaTransport (nusb) + NcmTransport (TcpStream)
│   └── bin/p1_echo.rs      # NEW: spike binary — handshake, echo, throughput print, D1 notes
├── android-client/src/
│   ├── transport.rs        # NEW: AccessoryFdTransport (File over raw fd) + NcmTransport (TcpStream)
│   ├── usb_jni.rs          # NEW (stub for now): future pure-jni openAccessory; P1 = doc only
│   └── lib.rs              # ADD: nativeOnUsbFd(fd) JNI entry → hands fd to transport+echo loop
└── protocol/src/
    └── framing.rs          # REUSE as-is: write_frame/read_frame already take &mut dyn Read/Write
android/app/src/main/
├── AndroidManifest.xml     # ADD: USB_ACCESSORY_ATTACHED intent-filter + meta-data resource
└── res/xml/accessory_filter.xml   # NEW: manufacturer/model/version matching control-req-52 strings
```

### Pattern 1: AOA handshake from Rust (`nusb` 0.2, blocking)
**What:** The Mac, as host, asks the phone its AOA version, sends six identity strings, then commands START.
**When to use:** sub-spike A, once per connect, before any bulk I/O.
**Example (illustrative — `nusb` 0.2 API; verify exact signatures against docs.rs at execution):**
```rust
// Source pattern: AOSP AOA spec (source.android.com/docs/core/interaction/accessories/aoa)
//                 + nusb 0.2 docs.rs (Interface::control_in / control_out, ControlType::Vendor)
// [CITED: source.android.com/docs/core/interaction/accessories/aoa] for req numbers/strings.
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};

const AOA_GET_PROTOCOL: u8 = 51; // USB_DIR_IN  | VENDOR
const AOA_SEND_STRING:  u8 = 52; // USB_DIR_OUT | VENDOR ; index = string id 0..=5
const AOA_START:        u8 = 53; // USB_DIR_OUT | VENDOR ; no data

// 51 — get protocol version (returns u16 little-endian; must be >= 1)
let ver = iface.control_in(ControlIn {
    control_type: ControlType::Vendor, recipient: Recipient::Device,
    request: AOA_GET_PROTOCOL, value: 0, index: 0, length: 2,
}).wait()?;            // .wait() = blocking; no async runtime needed for the spike
let proto = u16::from_le_bytes([ver[0], ver[1]]);

// 52 — send the six identity strings (index = string id). These MUST match accessory_filter.xml.
for (idx, s) in [
    (0u16, "RustScreen"),                 // manufacturer
    (1,    "RustScreen Host"),            // model
    (2,    "USB second-monitor link"),    // description
    (3,    "1.0"),                        // version
    (4,    "https://github.com/PeterXMR/rustscreen"), // uri
    (5,    "rs-0001"),                    // serial
] {
    let mut data = s.as_bytes().to_vec(); data.push(0); // zero-terminated UTF-8
    iface.control_out(ControlOut {
        control_type: ControlType::Vendor, recipient: Recipient::Device,
        request: AOA_SEND_STRING, value: 0, index: idx, data: &data,
    }).wait()?;
}

// 53 — start accessory mode; the device now re-enumerates (see Pattern 2).
iface.control_out(ControlOut {
    control_type: ControlType::Vendor, recipient: Recipient::Device,
    request: AOA_START, value: 0, index: 0, data: &[],
}).wait()?;
```
> Note: AOA 1.0 uses string ids 0–5 as above. AOA 2.0 adds audio/HID; **we do not need 2.0** — plain bulk accessory is enough. `[CITED: source.android.com/docs/core/interaction/accessories/aoa]`

### Pattern 2: Handle the re-enumeration race
**What:** After request 53 the phone disconnects and reappears as VID `0x18D1` / PID `0x2D00`–`0x2D05`. There is a window (typically <2 s) where the device is absent from the bus.
**When to use:** immediately after START, before claiming bulk endpoints.
**Example:**
```rust
// Poll list_devices() (or use watch_devices() stream) until the accessory-mode device appears.
// PID meanings: 0x2D00 = accessory; 0x2D01 = accessory+ADB; 0x2D02..0x2D05 add audio/HID combos.
let accessory = retry_until(Duration::from_secs(5), || {
    nusb::list_devices()?.find(|d|
        d.vendor_id() == 0x18D1 && (0x2D00..=0x2D05).contains(&d.product_id()))
})?;
let dev = accessory.open()?;
let iface = dev.claim_interface(0)?;           // <-- macOS claim risk lives here (see Pitfall 1)
let mut ep_out = iface.endpoint::<Bulk, Out>(/*bulk OUT addr*/)?;
let mut ep_in  = iface.endpoint::<Bulk, In >(/*bulk IN  addr*/)?;
```

### Pattern 3: Android accessory side — fd to Rust (simplest-now, Kotlin glue)
**What:** Kotlin receives the attach intent, opens the accessory, gets a `ParcelFileDescriptor`, passes the raw fd down to Rust; Rust wraps it as a `File` and echoes.
**When to use:** the MVP path (D0). Pure-`jni` openAccessory is the P7 swap.
**Kotlin (glue only — ~15 LOC added to MainActivity):**
```kotlin
// Source pattern: developer.android.com USB accessory guide. [CITED: developer.android.com/develop/connectivity/usb/accessory]
val usb = getSystemService(Context.USB_SERVICE) as UsbManager
val accessory = intent.getParcelableExtra<UsbAccessory>(UsbManager.EXTRA_ACCESSORY)
val pfd: ParcelFileDescriptor = usb.openAccessory(accessory)   // null if permission denied
nativeOnUsbFd(pfd.detachFd())   // hand the raw fd to Rust; Rust now owns it
```
**Rust (`android-client/src/transport.rs` + a new `nativeOnUsbFd` in `lib.rs`):**
```rust
use std::os::fd::{FromRawFd, OwnedFd};
use std::fs::File;
// The accessory fd is bidirectional: writing to it sends to the host's bulk-IN,
// reading from it receives the host's bulk-OUT. So File is both our reader and writer.
let file = unsafe { File::from_raw_fd(raw_fd) }; // takes ownership of the fd
let mut transport = AccessoryFdTransport(file);  // impls Read + Write trivially via the File
echo_loop(&mut transport)?;                       // read a chunk, write it straight back
```

### Pattern 4: The `Transport` seam (the D1 swap point)
**What:** One trait both AOA and NCM satisfy; the recommendation is to make it `Read + Write` so the existing `protocol::framing` works unchanged and every concrete transport (`EndpointRead`/`EndpointWrite`, `File`, `TcpStream`) already qualifies.
**When to use:** define once; both crates' `transport.rs` use it.
**Example:**
```rust
// Lead recommendation: a marker super-trait over std::io. Zero ceremony; framing already
// uses &mut dyn Read / &mut dyn Write, so a `&mut dyn Transport` is drop-in.
pub trait Transport: std::io::Read + std::io::Write + Send {}
impl<T: std::io::Read + std::io::Write + Send> Transport for T {}

// Pure, cable-free, fully testable echo verifier — the heart of XPORT-01:
pub fn echo_roundtrip<T: Transport>(t: &mut T, pattern: &[u8]) -> std::io::Result<EchoStats> {
    let start = std::time::Instant::now();
    t.write_all(pattern)?;                 // chunking handled by the concrete transport
    let mut got = vec![0u8; pattern.len()];
    t.read_exact(&mut got)?;               // read_exact loops over partial bulk reads
    let elapsed = start.elapsed();
    if got != pattern { return Err(/* MismatchAt(first differing index) */); }
    Ok(EchoStats { bytes: pattern.len(), elapsed })  // throughput = bytes / elapsed (pure math)
}
```
> Why `Read + Write` over `send(&[u8])`/`recv(&mut [u8])`: the latter reinvents what std::io already gives, and would force a shim to reuse `protocol::framing`. The only reason to choose explicit `send`/`recv` is if a transport is *message-oriented* (datagram); both AOA bulk and TCP are byte-streams, so `Read + Write` is the honest model. **Document this as the D-decision for the seam.**

### Pattern 5: NCM/TCP fallback (sub-spike B)
**What:** Enable USB tethering on the Pixel; if macOS brings up an NCM network interface, run a `TcpListener` on the phone and connect a `TcpStream` from the Mac — then call the *same* `echo_roundtrip`.
**When to use:** only if AOA can't claim on macOS or misses the throughput bar.
**Key checks (mostly manual, hands-on):**
```bash
# On Mac, after enabling Pixel USB tethering (Settings ▸ Network ▸ Hotspot & tethering):
ifconfig | grep -A3 -iE "en[0-9]|ncm"   # look for a new interface with a 192.168.x / link-local addr
# NOTE: modern Android (12+, which the Pixel 6a runs) tethers via NCM, which macOS supports
# NATIVELY — unlike the old RNDIS path that needed the now-broken HoRNDIS kext on Apple Silicon.
```

### Anti-Patterns to Avoid
- **Hand-rolling a length protocol for the spike:** P1 echoes a fixed 1 MB; just `write_all` + `read_exact`. The framed protocol already exists (`protocol::framing`) for P5 — don't duplicate it here.
- **Putting echo logic in Kotlin:** keep the Kotlin to glue (`openAccessory` + fd hand-off); the echo belongs in Rust behind the `Transport` seam (D0). Kotlin-side echo is a *spike-only* shortcut, not the design.
- **Assuming the device address is stable across the handshake:** it is NOT — the VID/PID/address all change at re-enumeration. Always re-`list_devices()` after request 53.
- **One giant 1 MiB bulk transfer:** AOA buffers are 16 KiB; chunk writes/reads (see Pitfall 3).
- **`async` nusb + tokio for a spike:** unnecessary; blocking `.wait()` is simpler and sufficient.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| USB control/bulk transfers on macOS | Raw IOKit/IOUSBHost FFI | `nusb` 0.2 | nusb wraps IOKit correctly incl. async submission and endpoint typing; hand-rolling IOKit is weeks of work and the exact thing nusb exists to avoid. |
| Getting the accessory file descriptor | A custom USB stack on Android | `UsbManager.openAccessory()` (via Kotlin glue now, `jni` later) | Android *requires* this API for accessory mode; there is no lower-level user-space path. |
| Length-prefixed framing (for P5) | A new frame codec in P1 | `protocol::framing::{write_frame, read_frame}` (already built + tested) | P1 doesn't even need framing (fixed 1 MB echo), and P5's is done. |
| Partial-read loop over a byte stream | Manual read-accumulate logic | `std::io::Read::read_exact` / `Write::write_all` | std handles short reads/writes; the `Transport: Read + Write` seam gives this for free. |
| NCM/RNDIS driver on Apple Silicon | Building/patching HoRNDIS kext | Native macOS NCM (Android 12+ tethers via NCM) | HoRNDIS is broken on Apple Silicon; modern Android NCM is natively supported — no kext. |

**Key insight:** The entire P1 *transport* reduces to "get a `Read + Write` handle on each end, then echo." `nusb` provides it on the Mac, `openAccessory()`→fd→`File` provides it on the phone, and `TcpStream` provides the fallback. The hard parts are not code — they're the AOA handshake sequence (well-specified) and the two genuinely-uncertain hardware facts (macOS interface claim; throughput).

## Runtime State Inventory

> P1 is greenfield transport code, not a rename/refactor. This section is included only to record the small amount of *new* OS-registered/config state P1 introduces (relevant because it touches the Android manifest and USB permission state).

| Category | Items Found | Action Required |
|----------|-------------|------------------|
| Stored data | None — P1 stores nothing. | None. |
| Live service config | Android **USB-accessory permission grant** is stored by the OS per-app once the user taps "OK" (and "use by default" if offered). This lives in Android's settings DB, not git. | Manual: user taps the permission dialog on first connect (R7). Documented as a hands-on step. |
| OS-registered state | New `AndroidManifest.xml` intent-filter (`USB_ACCESSORY_ATTACHED`) + `res/xml/accessory_filter.xml` — these are *config in git*, but the OS reads them at install time to decide whether to launch the app on attach. | Reinstall the apk after manifest change for the OS to register the intent filter. |
| Secrets/env vars | None. | None. |
| Build artifacts | The Mac host gains a new `nusb` dependency → `Cargo.lock` changes; the apk must be rebuilt after manifest edit. | `cargo build` regenerates lock; `./gradlew assembleDebug` rebuilds apk. |

**Nothing found in category:** Stored data and Secrets — verified: the echo spike is stateless and uses no credentials.

## Common Pitfalls

### Pitfall 1: macOS won't let `nusb` claim the device's interface
**What goes wrong:** `claim_interface()` fails because macOS has bound a kernel driver to the (composite) Android device, or the capture/detach path needs the `com.apple.vm.device-access` entitlement (and a CLI binary can't hold a provisioning-profile entitlement → must run as root).
**Why it happens:** Documented macOS libusb limitation: you can't attach to an interface a kext already claims; detaching needs root or the entitlement, and works per-*device* not per-interface. `[CITED: github.com/libusb/libusb wiki FAQ; libusb PR #911]`
**How to avoid:** For *AOA specifically* the risk is lower than the generic case — a phone in normal MTP/charging mode is not driven by an exclusive Apple class kext the way a keyboard/storage device is, so `nusb` can usually claim it. Mitigation order: (1) try plain `claim_interface` first; (2) if it fails, run the spike binary with `sudo`; (3) document whether root was required (feeds the P7 signing/entitlement task and the D1 friction column). Detect early: this fails at `claim_interface`, before any bulk I/O.
**Warning signs:** `Access denied`/`Resource busy` at open or claim.

### Pitfall 2: The re-enumeration race after request 53
**What goes wrong:** The Mac tries to re-open the device immediately after START and either finds the *old* (pre-accessory) device or finds nothing — the phone is mid-reset.
**Why it happens:** USB re-enumeration is asynchronous; the bus drops then re-adds the device with new VID/PID over ~0.5–2 s.
**How to avoid:** Don't reuse the old handle. Poll `list_devices()` (or consume `watch_devices()` events) filtering for VID `0x18D1` / PID `0x2D00`–`0x2D05`, with a retry loop + timeout (~5 s). Only then `open()`/`claim`.
**Warning signs:** Intermittent "device not found" right after handshake; works on the *second* run (stale-handle smell).

### Pitfall 3: Partial reads/writes and the 16 KiB AOA buffer
**What goes wrong:** A single 1 MiB bulk read returns far fewer bytes; or a large write stalls/errors.
**Why it happens:** The Android accessory framework uses a **16 KiB** transfer buffer; bulk endpoints are packetized; one transfer ≠ one logical message.
**How to avoid:** Chunk transfers at ≤16 KiB; on the read side loop until you've collected the expected length (`read_exact` does this). On the phone echo loop, read whatever chunk arrives and write it straight back — never assume the 1 MB arrives in one read.
**Warning signs:** Echo verification fails with a *length* mismatch (got 16384, expected 1048576) — the classic "I only read one buffer" bug.

### Pitfall 4: Identity strings don't match `accessory_filter.xml`
**What goes wrong:** After the handshake, Android never shows the permission dialog / never launches the app on attach.
**Why it happens:** Android matches the manufacturer/model/version sent in control request 52 against `res/xml/accessory_filter.xml`; a mismatch means no intent fires.
**How to avoid:** Keep the strings in one place (a shared const list) and copy them verbatim into the XML. Version string match is exact.
**Warning signs:** Mac handshake succeeds (req 51 returns a version) but the phone shows no dialog and the app doesn't open.

### Pitfall 5: Throughput measured wrong
**What goes wrong:** Reported throughput includes handshake/permission-dialog time, or measures a single tiny transfer, giving a number that doesn't reflect sustained streaming.
**Why it happens:** Timing the wrong span.
**How to avoid:** Measure only the bulk-echo span (after endpoints are claimed), over a payload large enough to amortize per-transfer overhead (≥ several MB total, or repeat the 1 MB echo N times). Report Mbit/s = bytes·8 / seconds. Compare against the ≥200 Mbit/s bar. Note USB 2.0 caps ~480 Mbit/s raw (~280–320 effective) and USB 3.x is far higher — record which the cable/phone negotiated.
**Warning signs:** Suspiciously round or suspiciously low numbers; throughput that changes 10× between runs.

## Code Examples

### Generate + verify the 1 MB pattern (pure, TDD in CI)
```rust
// Cable-free, fully unit-testable. Deterministic pattern so a mismatch points to the offset.
pub fn make_pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 256) as u8).collect()
}
pub fn first_mismatch(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
        .or_else(|| (a.len() != b.len()).then_some(a.len().min(b.len())))
}
#[test]
fn loopback_echo_roundtrips_1mib() {
    // LoopbackTransport = an in-memory VecDeque that returns on read what was written.
    let mut t = LoopbackTransport::default();
    let p = make_pattern(1 << 20);
    let stats = echo_roundtrip(&mut t, &p).unwrap();
    assert_eq!(stats.bytes, 1 << 20);
}
```

### Throughput math (pure)
```rust
pub fn mbit_per_sec(bytes: usize, elapsed: std::time::Duration) -> f64 {
    (bytes as f64 * 8.0) / 1_000_000.0 / elapsed.as_secs_f64()
}
#[test] fn one_mib_in_40ms_is_about_209_mbit() {
    let v = mbit_per_sec(1 << 20, std::time::Duration::from_millis(40));
    assert!((v - 209.7).abs() < 1.0);
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `rusb`/C `libusb` for USB in Rust | `nusb` (pure Rust, no C dep) | nusb 0.1→0.2 (2024–2026) | Aligns with the project's pure-Rust goal; nusb 0.2 typed `Endpoint<Bulk, In/Out>` + `Read`/`Write` adapters make the `Transport` seam trivial. |
| Android USB tethering via RNDIS (HoRNDIS kext) | NCM, natively supported by macOS | Android 11/12+ moved default tether to NCM | The NCM fallback is *viable on Apple Silicon* (no broken kext) — strengthens sub-spike B as a real contingency, not a dead end. |
| nusb 0.1 `Interface::bulk_in/out` | nusb 0.2 `endpoint::<Bulk,Dir>()` + `nusb::io` Read/Write adapters | nusb 0.2 (2025) | Use 0.2 API; older tutorials show 0.1 signatures — verify against docs.rs at execution. |

**Deprecated/outdated:**
- HoRNDIS / RNDIS tethering on Apple Silicon: broken; do not pursue. Use NCM (native) if testing the fallback.
- AOA 2.0 audio/HID: not needed; plain bulk accessory (AOA 1.0 strings 0–5) suffices.

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `nusb` 0.2.x is the right Mac-host USB crate and resolves cleanly with no surprise C deps | Standard Stack | Low — named in roadmap, 771k downloads; checkpoint:human-verify before install catches it. |
| A2 | `nusb` can `claim_interface` the Pixel on macOS *without* the `com.apple.vm.device-access` entitlement (possibly needs `sudo`) | Pitfall 1 | **HIGH** — if it needs the entitlement (CLI can't hold one → root-only), AOA setup friction rises and may tip D1 toward NCM. This is *the* hardware unknown; the spike settles it. |
| A3 | Pixel 6a USB-C negotiates ≥ ~200 Mbit/s effective over AOA bulk | Success criteria | **HIGH** — if USB 2.0-only / AOA overhead caps it below the bar, D1 favors NCM (or H.264 bitrate must drop). Measured in the spike. |
| A4 | The Pixel 6a tethers via NCM that macOS brings up natively (fallback viability) | Pattern 5 / SOTA | Medium — if no interface appears, sub-spike B is dead and AOA must work. |
| A5 | `nusb` 0.2 control API is `ControlIn`/`ControlOut` with `ControlType::Vendor` / `Recipient::Device` and a blocking `.wait()` | Pattern 1 | Low — API shape verified on docs.rs; exact field names confirmed at execution. |
| A6 | The exact bulk endpoint addresses are discoverable from the accessory-mode device's descriptor (not hard-coded) | Pattern 2 | Low — standard USB; read endpoints from the interface descriptor. |
| A7 | AOA accessory framework buffer is 16 KiB (chunk size guidance) | Pitfall 3 | Low — long-documented AOA constant; even if larger, chunking is still correct. |
| A8 | Keeping `jni` at the repo's 0.21 (not bumping to 0.22.4) is fine for P1 | Standard Stack | Low — P1 adds no new JNI calls beyond `nativeOnUsbFd`; 0.21 is sufficient. |

## Open Questions

1. **Does macOS let `nusb` claim the Pixel's bulk interface without an entitlement?** (A2)
   - What we know: generic libusb-on-macOS hits kext-claim issues; the entitlement is root-or-provisioning-profile and CLI binaries can't hold it.
   - What's unclear: whether a phone in accessory mode is claimed by an exclusive Apple kext at all.
   - Recommendation: spike tries plain claim first, then `sudo`; record which worked (feeds D1 friction + P7 entitlement task).

2. **Measured AOA throughput vs the ≥200 Mbit/s bar.** (A3)
   - What we know: USB 2.0 raw 480 Mbit/s (~280–320 effective); USB 3.x much higher.
   - What's unclear: what the Pixel 6a + this cable negotiate, and AOA framework overhead.
   - Recommendation: measure sustained echo throughput; this is the primary D1 input.

3. **D1 verdict.** Decide AOA vs NCM on: throughput (both), setup friction (AOA permission dialog + possible sudo vs NCM tether toggle), robustness across replug, and latency. **Record the rationale in the roadmap §1 D1 row and in this phase's completion notes.** Default expectation: AOA wins on latency/overhead if it claims cleanly and clears the bar; NCM is the safety net.

4. **Spike echo on phone — Kotlin or Rust-over-fd?** For the *spike only*, a Kotlin echo loop is fastest to prove the link; but doing it in Rust (over the fd handed via `nativeOnUsbFd`) is closer to the D0 end-state and exercises the `Transport` seam on both ends. Recommendation: Rust-over-fd, since that's the seam P5 will use.

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| `nusb` crate | sub-spike A (Mac USB host) | ✗ (not yet added) | target 0.2.3 | NCM/TCP path needs no USB crate |
| Pixel 6a on USB-C | live echo, AOA, replug, throughput | ✓ (user states hardware now available) | Android 12+/13+ | none — these criteria are hardware-gated |
| `adb` + `cargo-ndk` + Gradle | build/install apk, logcat (already used in P0) | ✓ (P0 established) | — | — |
| macOS NCM interface | sub-spike B only | ? (unverified — test on hardware) | — | AOA is the lead anyway |
| root/`sudo` on Mac | *possibly* for `nusb` claim (Pitfall 1) | ✓ (user's own machine) | — | none if entitlement route taken later |

**Missing dependencies with no fallback:**
- The phone itself for criteria #1/#2/#3 (the live echo, replug, throughput) — these are the `checkpoint:human-verify` steps.

**Missing dependencies with fallback:**
- `nusb` not yet added — trivially added (gated by checkpoint); NCM path avoids it entirely.

## Validation Architecture

> No `.planning/config.json` exists, so `workflow.nyquist_validation` is absent → treated as **enabled**. The repo already TDDs its pure-Rust crates (33+ tests in `protocol`/`macos-host` per P3 notes), so this fits the established pattern.

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[cfg(test)]` / `cargo test` |
| Config file | none (standard cargo) |
| Quick run command | `cargo test -p macos-host transport` (and `-p protocol` for framing reuse) |
| Full suite command | `cargo test --workspace` |

### Phase Requirements → Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| XPORT-01 | 1 MB pattern generate + byte-for-byte verify | unit (pure) | `cargo test -p macos-host loopback_echo_roundtrips_1mib` | ❌ Wave 0 |
| XPORT-01 | echo over a loopback `Transport` fake | unit (pure) | `cargo test -p macos-host echo_roundtrip` | ❌ Wave 0 |
| XPORT-01 | throughput math (Mbit/s) | unit (pure) | `cargo test -p macos-host mbit_per_sec` | ❌ Wave 0 |
| XPORT-01 | partial-read chunking handled correctly | unit (pure, chunked loopback fake) | `cargo test -p macos-host chunked_loopback` | ❌ Wave 0 |
| XPORT-01 | framing reuse still round-trips (regression) | unit (existing) | `cargo test -p protocol framing` | ✅ |
| XPORT-01 | **real 1 MB echo over USB, byte-for-byte** | manual (hands-on) | run `p1_echo` binary with phone attached | n/a — `checkpoint:human-verify` |
| XPORT-01 | **reproducible across cable replug** | manual | replug, re-run `p1_echo` | n/a — `checkpoint:human-verify` |
| XPORT-01 | **throughput ≥200 Mbit/s documented + D1 decided** | manual | read `p1_echo` output, record in roadmap | n/a — `checkpoint:human-verify` |

### Sampling Rate
- **Per task commit:** `cargo test -p macos-host transport`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** full suite green (cable-free) before the hands-on `p1_echo` run; then the live checkpoints.

### Wave 0 Gaps
- [ ] `crates/macos-host/src/transport.rs` — `Transport` trait, `echo_roundtrip`, `make_pattern`, `first_mismatch`, `mbit_per_sec`, `LoopbackTransport` test fake — covers XPORT-01 cable-free.
- [ ] `crates/android-client/src/transport.rs` — `AccessoryFdTransport` (File over fd) + echo loop (compiled under `cfg(target_os="android")`; logic tested via the shared loopback fake where possible).
- [ ] No framework install needed (cargo built-in).

## Security Domain

> No `.planning/config.json`, so `security_enforcement` is absent → treated as **enabled**. P1 is a local USB byte echo with no network exposure (except the *fallback's* loopback-over-USB TCP) and no auth/session/crypto surface — most ASVS categories are N/A, but input handling on the wire matters.

### Applicable ASVS Categories
| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | no | No accounts; the only "auth" is the Android USB-accessory permission dialog (OS-enforced). |
| V3 Session Management | no | No sessions. |
| V4 Access Control | partial | Android USB-accessory permission grant is the access gate; macOS device access governed by OS/entitlement (Pitfall 1). |
| V5 Input Validation | yes | The frame-length guard (`MAX_FRAME_LEN`) already exists in `protocol::framing`; for the raw P1 echo, bound the read length to the known 1 MB so a malformed/oversized transfer can't trigger an unbounded allocation. |
| V6 Cryptography | no | No crypto — wired, physically-local link; encryption is out of scope (and not in requirements). |

### Known Threat Patterns for USB transport
| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Oversized/garbage length on the wire → OOM | Denial of Service | Bound allocations (`read_exact` into a fixed-size buffer; reuse `MAX_FRAME_LEN` for any framed path). |
| Malicious USB host/accessory (BadUSB-style) | Spoofing/Tampering | Out of scope for a wired single-purpose dev link; the OS permission dialog is the user's consent gate. Note, don't mitigate in code. |
| Unwrapping a raw fd unsafely | Tampering (memory safety) | `File::from_raw_fd` is `unsafe`; ensure single-ownership (`detachFd()` on Kotlin side, `OwnedFd` on Rust side) so the fd isn't double-closed. |

## Sources

### Primary (HIGH confidence)
- `[CITED: source.android.com/docs/core/interaction/accessories/aoa]` — AOA 1.0 protocol: control requests 51/52/53, string ids 0–5, VID `0x18D1`, PID `0x2D00`/`0x2D01`, re-enumeration behavior.
- `[CITED: source.android.com/docs/core/interaction/accessories/protocol]` — AOA overview, two bulk endpoints, device-mode behavior.
- `[CITED: developer.android.com/develop/connectivity/usb/accessory]` — `UsbManager.openAccessory()` → `ParcelFileDescriptor`, `accessory_filter.xml`, `USB_ACCESSORY_ATTACHED` intent.
- docs.rs/nusb (latest, 0.2) — `list_devices`, `DeviceInfo::open`, `claim_interface`, `Interface::control_in/control_out`, `endpoint::<Bulk, In/Out>`, `EndpointRead`/`EndpointWrite` Read/Write, `watch_devices`, blocking `.wait()`.
- crates.io API — `nusb` 0.2.3 (771,407 downloads, repo kevinmehall/nusb, updated 2026-03-10); `jni` 0.22.4 (121,292,262 downloads).
- Repo code: `crates/protocol/src/framing.rs` (write_frame/read_frame over `&mut dyn Read/Write` — the seam fits), `crates/android-client/{Cargo.toml, src/lib.rs}` (jni 0.21 present, `nativeInit` JNI pattern), `android/app/src/main/{AndroidManifest.xml, java/.../MainActivity.kt}`.

### Secondary (MEDIUM confidence)
- `[CITED: github.com/libusb/libusb wiki FAQ; libusb PR #911]` — macOS interface-claim limitation, `com.apple.vm.device-access` entitlement, root requirement for CLI apps (informs Pitfall 1 / A2).
- WebSearch (multiple) — modern Android (12+) tethers via NCM which macOS supports natively; HoRNDIS broken on Apple Silicon (informs NCM fallback viability).

### Tertiary (LOW confidence)
- Community AOA + libusb host examples (Fire30/SimpleAOA, once2go/AoA-app-and-hosts) — corroborate the handshake/echo shape; not the basis for any specific claim.

## Metadata

**Confidence breakdown:**
- AOA protocol (req 51/52/53, VID/PID, re-enum): HIGH — official AOSP spec.
- `nusb`/`jni` choice + API shape: HIGH — docs.rs + crates.io verified; `jni` already in repo.
- `Transport` seam design: HIGH — `Read + Write` fits the existing `framing` API directly.
- macOS interface-claim feasibility (A2): LOW — the central hardware unknown; spike settles it.
- Throughput vs 200 Mbit/s bar (A3): LOW — measured only on hardware.
- NCM fallback viability (A4): MEDIUM — modern-Android-via-NCM is well-attested but unverified on this exact M1+Pixel.

**Research date:** 2026-06-03
**Valid until:** 2026-07-03 (nusb is moving 0.2.x; re-verify the control API field names against docs.rs at execution).
