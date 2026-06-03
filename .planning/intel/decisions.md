# Decisions (extracted)

Source document: `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md`
Classified as: DOC (medium confidence) — embeds an ADR-like locked decisions table (D0-D7).

Locked status legend: D0-D3 are marked "locked from review" in §1 and carry ADR-like
force. D4-D7 are stated as "safe defaults" (proposed, overridable). Treated accordingly.

---

## D0 — "100% Rust" meaning
- source: docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md (§0, §0.1, §1)
- status: LOCKED ("locked from review")
- decision: Simplest-now, shrink-to-Rust-later via ports & adapters. Every platform
  boundary sits behind a Rust trait / stable FFI seam. MVP may ship the simplest
  adapter that works (Kotlin shell + ObjC++ shim + Swift bridge); adapters are
  replaced with pure-Rust implementations over P7-P8 without touching core logic.
- scope: whole project
- metric: Rust purity % tracked per phase (MVP end P6 ~85%; stretch end P8 ~99%).

## D1 — USB transport
- source: docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md (§1, P1)
- status: LOCKED ("locked from review") — strategy locked; final mechanism decided in P1
- decision: Spike both AOA and network-over-USB (NCM/TCP) in P1; lead with AOA.
  Final pick is by measured throughput / reliability and recorded in the doc.
- scope: P1, transport

## D2 — Video codec
- source: docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md (§1, P3, P4)
- status: LOCKED ("locked from review")
- decision: H.264 for MVP (universal HW support on M1 + Pixel 6a). HEVC is a later
  toggle (P7).
- scope: P3, P4, protocol

## D3 — Android render path
- source: docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md (§1, P4)
- status: LOCKED ("locked from review")
- decision: Decode-to-surface (MediaCodec -> ANativeWindow, no GPU round-trip).
  wgpu sampling only if overlays are needed later.
- scope: P4, render

## D4 — Touch scope (default)
- source: docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md (§1, P6)
- status: proposed (safe default)
- decision: Mouse emulation (single pointer) via CGEvent for MVP; multitouch + pen
  are post-MVP.
- scope: P6

## D5 — Mac app shape (default)
- source: docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md (§1, P7, P8)
- status: proposed (safe default)
- decision: CLI for spikes; menu-bar status item for the shippable app.
- scope: P7, P8

## D6 — Display geometry (default)
- source: docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md (§1, P2, P5)
- status: proposed (safe default)
- decision: Extend at 2400x1080 @ 60 Hz (Pixel 6a native, landscape); HiDPI 2x optional.
- scope: P2, P5

## D7 — Android MVP shell (derived from D0)
- source: docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md (§1, P0, P4, P6)
- status: proposed (safe default, follows from LOCKED D0)
- decision: Thin Kotlin Activity (glue only) for the MVP; swap to NativeActivity in P7.
- scope: P0, P4, P6
