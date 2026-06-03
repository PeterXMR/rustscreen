# Context (extracted)

Source document: `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md`
Running notes keyed by topic, with source attribution.

---

## Project goal
- source: docs/superpowers/plans/...roadmap.md (header, §6)
Turn a Google Pixel 6a into a wired secondary monitor for a MacBook Pro M1 over a
single USB-C<->USB-C cable, with both the macOS host and the Android client written
in Rust. Definition of Done: plug in, launch the Mac app, and within seconds the phone
becomes a usable, touch-capable extended display with sub-50 ms latency — every line
of source in the repo in Rust save for AndroidManifest.xml and documented FFI to the
three §0 platform boundaries.

## Tech stack
- source: docs/superpowers/plans/...roadmap.md (header)
Rust 1.85+; objc2 family; videotoolbox; nusb; android-activity (NativeActivity);
ndk / ndk-sys (MediaCodec); wgpu (optional overlay path); postcard (control messages);
H.264 (MVP) / HEVC (later).

## Current status — P0 COMPLETE
- source: ingest prompt + docs/superpowers/plans/...roadmap.md (P0)
Phase P0 is already COMPLETE: the Cargo workspace + Kotlin shell + CI were delivered
as PR #1. The P0 checkboxes in the roadmap are not yet ticked in the doc, but the
deliverable is done. Downstream roadmapping should mark P0 done and start from the
next phase.

## Build order — P2 front-loaded before P1
- source: docs/superpowers/plans/...roadmap.md (§7) + ingest prompt
The agreed implementation sequence is P0 -> P2 -> P1 (not numeric order). Rationale:
P2 (create virtual display from Rust) is the keystone risk R1 — if it fails, the whole
architecture changes, so it is attacked on day one, before P1 (transport). Downstream
roadmap ordering must reflect P2 ahead of P1.

## Phase taxonomy — spikes vs build phases
- source: docs/superpowers/plans/...roadmap.md (header, §7)
Spike phases (P1-P4) are de-risking experiments with acceptance criteria rather than
full TDD steps; once a spike succeeds it is expanded into a detailed bite-sized plan
before execution. Build phases (P0, P5-P8) use checkbox tracking and TDD for pure-Rust
pieces (protocol framing, coordinate mapping).

## Distribution constraint — no Mac App Store
- source: docs/superpowers/plans/...roadmap.md (P2 risks, P8)
The private CGVirtualDisplay API bars the host from the Mac App Store. Distribution is
via a notarized DMG, not MAS. Notarization checks signing/malware (not API use), so the
private-API binary is expected to pass (R8). Android Play Store distribution optional.

## Open questions / unmeasured items
- source: docs/superpowers/plans/...roadmap.md (§2, P2, P7)
- Glass-to-glass latency (<50 ms) is unmeasured; P5 must prove it.
- The exact entitlement set needed for CGVirtualDisplay under hardened runtime is an
  open question (empirically determined in P7).
- LICENSE choice (MIT vs Apache-2.0) to be confirmed (P8).
- Whether macOS brings up an NCM interface on this M1 + Pixel 6a (resolved in P1).
