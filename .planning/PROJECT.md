# RustScreen

## What This Is

RustScreen turns a Google Pixel 6a into a wired USB-C secondary monitor for a MacBook Pro M1. Both halves — a macOS host binary and an Android client `cdylib` — are written in Rust, organized as a single Cargo workspace. The Mac creates a private virtual display, hardware-encodes it (H.264), and streams length-prefixed NAL units over a single USB-C cable to the phone, which hardware-decodes them straight onto its native window surface. Touch events travel back over the same link and are injected on macOS as mouse events. It is for one developer (the user) building a personal, high-performance, mostly-pure-Rust screen extension.

## Core Value

A user plugs a Pixel 6a into an M1 MacBook with one USB-C cable, launches the host app, and within seconds gets a usable, touch-capable extended display with glass-to-glass latency < 50 ms.

## Requirements

### Validated

<!-- Shipped and confirmed valuable. -->

- ✓ **P0** Workspace scaffold + cross-compilation + thin Kotlin shell + CI — delivered as PR #1 (branch `feat/p0-workspace-scaffold`).

### Active

<!-- Current scope. Building toward these. See REQUIREMENTS.md for full detail. -->

- [ ] **P1** USB byte round-trip Mac↔Pixel (resolves D1)
- [ ] **P2** Create a virtual display from Rust (keystone risk R1) — NEXT
- [ ] **P3** Capture virtual display + hardware-encode to playable H.264
- [ ] **P4** Decode H.264 on the Pixel → on screen (decode-to-surface)
- [ ] **P5** Wire the full live pipeline; measure glass-to-glass latency
- [ ] **P6** Touch back-channel → CGEvent injection on macOS
- [ ] **P7** Robustness, UX, HEVC, code-signing, and Rust-purity adapter swaps
- [ ] **P8** Packaging, distribution, OSS hygiene

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
- **Latency budget (target, validate in P5):** capture ≤2 ms · encode ≤8 ms · USB ≤3 ms · decode ≤8 ms · present ≤16 ms → glass-to-glass < 50 ms. Unmeasured in research; P5 must prove it.

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
| **D1** — USB transport: spike both AOA and network-over-USB (NCM/TCP) in P1, **lead with AOA**; final mechanism picked on measured throughput/reliability. | **LOCKED** (strategy; mechanism decided in P1) | Two spikes guarantee one works; AOA is the lead candidate, NCM the fallback. | — Pending |
| **D2** — Video codec: **H.264** for MVP; HEVC is a later toggle (P7). | **LOCKED** | Universal HW support on M1 + Pixel 6a for H.264. | — Pending |
| **D3** — Android render path: **decode-to-surface** (MediaCodec → `ANativeWindow`, no GPU round-trip); `wgpu` sampling only if overlays needed later. | **LOCKED** | Lowest-latency path; avoids a GPU copy. | — Pending |
| **D4** — Touch scope: single-pointer **mouse emulation** via `CGEvent`; multitouch + pen post-MVP. | Proposed default | Keeps the MVP back-channel simple. | — Pending |
| **D5** — Mac app shape: **CLI** for spikes, **menu-bar status item** for the shippable app. | Proposed default | CLI is enough to de-risk; menu-bar is the shippable UX. | — Pending |
| **D6** — Display geometry: **extend** at **2400×1080 @ 60 Hz** (Pixel 6a native, landscape); HiDPI 2× optional. | Proposed default | Matches the Pixel 6a panel; extend (not mirror) is the product. | — Pending |
| **D7** — Android MVP shell: **thin Kotlin Activity** (glue only, ~50 LOC) → swap to **NativeActivity** in P7. | Proposed default (follows from D0) | Fastest path to a working shell; all logic stays in the Rust `cdylib`. | — Pending |

</decisions>

---
*Last updated: 2026-06-02 after initial ingest (P0 complete; P2 next per agreed build order)*
