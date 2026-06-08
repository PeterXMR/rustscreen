# RustScreen

## What This Is

RustScreen turns a Google Pixel 6a into a wired USB-C secondary monitor for a MacBook Pro M1. Both halves — a macOS host binary and an Android client `cdylib` — are written in Rust, organized as a single Cargo workspace. The Mac creates a private virtual display, hardware-encodes it (H.264), and streams length-prefixed NAL units over a single USB-C cable to the phone, which hardware-decodes them straight onto its native window surface. Touch events travel back over the same link and are injected on macOS as mouse events. It is for one developer (the user) building a personal, high-performance, mostly-pure-Rust screen extension.

## Core Value

A user plugs a Pixel 6a into an M1 MacBook with one USB-C cable, launches the host app, and within seconds gets a usable, touch-capable extended display with glass-to-glass latency < 50 ms.

## Requirements

### Validated

<!-- Shipped and confirmed valuable. -->

- ✓ **P0** Workspace scaffold + cross-compile + thin Kotlin shell + CI (PR #1).
- ✓ **P1** USB byte round-trip Mac↔Pixel over AOA — byte-exact, no sudo, ~103 Mbit/s; **D1 = AOA**.
- ✓ **P2** Virtual display from Rust via private `CGVirtualDisplay` (keystone risk R1 retired); arrangeable HiDPI external display.
- ✓ **P3** Capture virtual display + VideoToolbox H.264 encode (RealTime, no B-frames, ~10 ms/frame).
- ✓ **P4** Decode-to-surface on the Pixel via `AMediaCodec` → `ANativeWindow`.
- ✓ **P5** Live end-to-end pipeline — the Mac's extended desktop renders on the phone; glass-to-glass instrumented (SNTP clock-sync) + tuned to **~34 ms p50 (PR #27)** on device, under the < 50 ms target.

### Active — the three user-set priorities (2026-06-05)

<!-- Current scope, in priority order. `ROADMAP.md` → "Active Priorities" is the single source of truth for sub-tasks. -->

- [x] **1. `rustscreen` terminal app** ✅ (PR #25) — installable release binary; `rustscreen start` runs the host + auto-streams when the phone app opens; `rustscreen stop` kills it.
- [x] **2. Automatic reconnect** ✅ (PR #26) — `start` is a supervisor loop that re-handshakes on phone-app close/reopen, host restart, or cable replug.
- [x] **3. Native-feel latency** ✅ (PR #27) — glass-to-glass p50 ~34 ms, under the < 50 ms target. Remaining latency work is the p95 tail / behaviour under sustained load.

### Deferred (behind the three priorities)

- [ ] **Live touch** — wire the existing CI-tested touch FSM into the live session (today it's a read-only monitor) — completes **P6**.
- [ ] **P7** Robustness, cursor visibility, orientation, HEVC toggle, code-signing, Rust-purity adapter swaps.
- [ ] **P8** Packaging, distribution, OSS hygiene.

### Out of Scope

<!-- Explicit boundaries. Includes reasoning to prevent re-adding. -->

- Mac App Store distribution — the private `CGVirtualDisplay` API bars MAS; distribute via notarized DMG instead.
- True 0% non-Rust — physically impossible; the OS-supplied `AndroidManifest.xml` and Google's in-OS NativeActivity glue are irreducible. Target is 100% Rust *source code* with three documented FFI boundaries (§0).
- Multitouch and pen input (MVP) — single-pointer mouse emulation only for the MVP (D4); multitouch/pen are post-MVP.
- Display mirroring as the primary mode — the product extends the desktop, not mirrors it (D6).
- Other Android models (MVP) — Pixel 6a is the primary device; broader device support comes later.
- HEVC (MVP) — H.264 is the MVP codec (universal HW support on M1 + Pixel 6a); HEVC is a P7 toggle (D2).

## Context

- **Authoritative design source:** `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md` is the master architecture document — full per-phase acceptance criteria, the seam/adapter table, the data-flow diagram, the latency budget, and the risk register live there. This `.planning/` set is the *execution tracker* layered on top of it.
- **Tech stack:** Rust 1.85+ · `objc2` family · `videotoolbox` · `nusb` · `android-activity` (NativeActivity, target) · `ndk` / `ndk-sys` (MediaCodec) · `wgpu` (optional overlay path) · `postcard` (control messages) · H.264 (MVP) / HEVC (later).
- **Architecture is de-risk-first:** P1–P4 are spikes (de-risking experiments with acceptance criteria, expanded into detailed TDD plans only after each spike succeeds). P0 and P5–P8 are build phases. Pure-Rust pieces (protocol framing, coordinate mapping) use TDD.
- **Three unavoidable FFI boundaries (honest scope, §0):** (1) Android `NativeActivity` — `android_main()` in Rust + `AndroidManifest.xml` config; (2) macOS virtual display — hand-written bindings to the private `CGVirtualDisplay` ObjC class; (3) Android USB accessory FD — a few `jni` calls to `UsbManager.openAccessory()` returning an `fd` read via `std::fs`.
- **Rust purity is a living metric:** MVP target (end P6) ~85% Rust source (Kotlin shell + ObjC++ shim + Swift bridge are the deltas); stretch (end P8) ~99% (only `AndroidManifest.xml` + in-OS NativeActivity glue remain).
- **Latency budget (measured in P5, tuned through PR #27):** glass-to-glass **~34 ms p50** on device (Pixel 6a + M1, 2400×1080@60) — under the **< 50 ms** target, at the practical floor **~43 ms** (one 60 Hz vsync ~16.7 ms + Tensor decode ~12 ms are immovable on this hardware). Phone input-queue bufferbloat solved; host encoder pacing + VideoToolbox low-latency mode + the phone decode-drain fix (PR #27) shipped. (Note: non-blocking USB writes were tried and **reverted** — they regressed to ~100 ms; the decode-drain fix was the real win.) Remaining work is the p95 tail / behaviour under load.

## Constraints

- **Tech stack**: 100% Rust *source code* with exactly three documented FFI boundaries — Why: D0 locks "simplest-now, shrink-to-Rust-later" via ports & adapters; the core never changes on an adapter swap.
- **Architecture (ports & adapters)**: Every platform boundary sits behind a Rust trait or stable FFI seam; MVP ships the simplest adapter that works (Kotlin shell + ObjC++ shim + Swift bridge), replaced with pure-Rust adapters over P7–P8 without touching core logic — Why: lets the MVP move fast while making the purity work a contained follow-up, not a rewrite.
- **Target architecture**: Two artifacts in one Cargo workspace — a macOS host binary (Apple Silicon / M1) and an Android client `cdylib` (Pixel 6a, `aarch64-linux-android`) — plus a shared pure-Rust `protocol` crate — Why: shell-agnostic Rust core is the whole point of the seam strategy.
- **Wire format**: Length-prefixed frame codec `u8 tag · u32 len · payload`; `Frame { Handshake, VideoConfig{codec, sps_pps}, Video{pts_us, keyframe, nal}, Touch, Control }`; control/touch/handshake via `postcard`, `Video` payload written raw (no serde over big buffers) — Why: performance; lives in the pure-Rust `protocol` crate.
- **Performance**: Glass-to-glass latency < 50 ms — Why: the Core Value; a laggy second screen is unusable.
- **Compatibility**: Single USB-C ↔ USB-C cable only; primary device Pixel 6a; host is M1 macOS — Why: the product's defining constraint.
- **Distribution**: Notarized DMG (host) + release `.apk` (client), no Mac App Store — Why: the private virtual-display API bars MAS.
- **Quarantine private API**: `cg-virtual-display` is its own crate isolating the only private/undocumented surface — Why: one-crate blast radius if a macOS update breaks `CGVirtualDisplay`.

## Key Decisions

<!-- D0-D3 are LOCKED. D4-D7 are proposed defaults (overridable). -->

<decisions>

| Decision | Status | Rationale | Outcome |
|----------|--------|-----------|---------|
| **D0** — "100% Rust" = simplest-now, shrink-to-Rust-later via ports & adapters: every platform boundary behind a Rust trait/FFI seam; MVP may ship Kotlin shell + ObjC++ shim + Swift bridge; replace with pure-Rust adapters over P7–P8 without touching core logic. | **LOCKED** | True 0% non-Rust is impossible; achievable target is 100% Rust source + 3 documented FFI boundaries. Shrinking at seams keeps the purity work a contained follow-up, not a rewrite. | — Pending |
| **D1** — USB transport: spike both AOA and network-over-USB (NCM/TCP) in P1, **lead with AOA**; final mechanism picked on measured throughput/reliability. | **LOCKED** | Two spikes guarantee one works; AOA is the lead candidate, NCM the fallback. | ✅ **AOA** (P1, ~103 Mbit/s, no sudo; NCM unused) |
| **D2** — Video codec: **H.264** for MVP; HEVC is a later toggle (P7). | **LOCKED** | Universal HW support on M1 + Pixel 6a for H.264. | ✅ H.264 live; HEVC deferred (anti-latency on Tensor) |
| **D3** — Android render path: **decode-to-surface** (MediaCodec → `ANativeWindow`, no GPU round-trip); `wgpu` sampling only if overlays needed later. | **LOCKED** | Lowest-latency path; avoids a GPU copy. | ✅ decode-to-surface live (P4/P5) |
| **D4** — Touch scope: single-pointer **mouse emulation** via `CGEvent`; multitouch + pen post-MVP. | Proposed default | Keeps the MVP back-channel simple. | Logic done both ends; live wiring deferred |
| **D5** — Mac app shape: **terminal CLI** (`rustscreen start`/`stop`) is the shippable interface. | **Decided — CLI** (user, 2026-06-05) | User prefers terminal control; the menu-bar app is dropped. | Priority 1 |
| **D6** — Display geometry: **extend** at **2400×1080 @ 60 Hz** (Pixel 6a native, landscape); HiDPI 2× optional. | Proposed default | Matches the Pixel 6a panel; extend (not mirror) is the product. | ✅ 2400×1080@60 + arrangeable HiDPI (P2) |
| **D7** — Android MVP shell: **thin Kotlin Activity** (glue only, ~50 LOC) → swap to **NativeActivity** in P7. | Proposed default (follows from D0) | Fastest path to a working shell; all logic stays in the Rust `cdylib`. | Kotlin shell in use; NativeActivity deferred (P7) |

</decisions>

---
*Last updated: 2026-06-05 — P0–P5 complete (live pipeline works on device). Now executing the three user-set priorities in `ROADMAP.md` → "Active Priorities": CLI → auto-reconnect → finish latency.*
