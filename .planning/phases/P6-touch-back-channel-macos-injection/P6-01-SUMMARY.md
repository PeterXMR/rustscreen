---
phase: P6-touch-back-channel-macos-injection
plan: 01
subsystem: input
tags: [touch, coordinate-mapping, pointer-fsm, core-graphics, ports-adapters, tdd, rust]

# Dependency graph
requires:
  - phase: P5-protocol-messages
    provides: "protocol::messages::{TouchEvent, TouchPhase} — the shipped wire INPUT type the mapping/FSM consume (never redefine)"
provides:
  - "macos_host::touch::map_normalized_to_global — normalized top-left → CG global affine map, NO Y-flip, clamp/NaN sanitation"
  - "DisplayRect / CgPoint / MapError — CG-global coordinate value types (rect sourced later from CGDisplayBounds)"
  - "PointerStateMachine::step — single-pointer (D4) touch→mouse FSM emitting PointerAction (Down/Move/Up); total (state,phase) match, no panic"
  - "PointerSink — the injection PORT (D0 swap point) the deferred macOS CGEvent adapter implements"
affects: [P6-deferred-cgevent-adapter, P7-purity-hardening]

# Tech tracking
tech-stack:
  added: []  # zero new dependencies — pure std Rust in the existing macos-host crate
  patterns:
    - "Cable-free pure logic + deferred cfg-gated adapter co-located in one file (mirrors encode_vt.rs / transport.rs)"
    - "Coordinate math stays entirely in CG global (top-left) space — NO (1.0 - ny) inversion; grep-gated"
    - "Input sanitation owned at the consumer boundary (clamp finite / reject NaN-inf) because protocol::TouchEvent is unvalidated on decode"
    - "Single-pointer FSM with a TOTAL (state, phase) match — hostile/out-of-order input is Ok(None), never a panic"

key-files:
  created:
    - crates/macos-host/src/touch.rs
  modified:
    - crates/macos-host/src/lib.rs

key-decisions:
  - "NO Y-flip: gx = rect.x + nx*rect.w, gy = rect.y + ny*rect.h — CG global and normalized touch are both top-left (RESEARCH HIGH-confidence, 3 Apple sources); ny=0 ⇒ rect.y is the regression guard"
  - "Clamp finite out-of-range nx/ny into [0,1] before scaling; reject NaN/±inf with MapError::NotFinite (A1)"
  - "Single-pointer policy (D4): a second pointer_id mid-gesture is ignored (Ok(None)); active id is held in Option<u32> (A2)"
  - "step() returns Result<Option<PointerAction>, MapError> (testable fold); PointerSink is a SEPARATE injection port the deferred adapter drives (A5)"
  - "Pure module placed in macos-host (not protocol) so it co-locates with its CGEvent adapter and keeps protocol macOS-coordinate-free (A3)"

patterns-established:
  - "RecordingSink (test-only PointerSink impl) proves the end-to-end Down→Move→Up fold in CI with no macOS API — mirrors transport::LoopbackTransport"
  - "Match enumerates every (PointerState, TouchPhase) pair explicitly (no _ wildcard) for compile-time totality"

requirements-completed: [TOUCH-01]  # cable-free coordinate-mapping + pointer-FSM clause only; CGEvent injection + AXIsProcessTrusted gating remain deferred

# Metrics
duration: ~25min
completed: 2026-06-03
---

# Phase P6 Plan 01: Touch Coordinate Mapping + Single-Pointer FSM Summary

**Pure, CI-green `macos_host::touch` module: a no-Y-flip normalized→CG-global coordinate map with clamp/NaN input sanitation, plus a single-pointer (D4) touch→mouse state machine emitting abstract `PointerAction`s behind a `PointerSink` injection port — 15 TDD tests, zero new deps, MSRV-1.80 clean; the real `CGEvent`/`AXIsProcessTrusted` injection adapter stays deferred behind the port.**

## What Was Built

### Task 1 — Coordinate mapping (no Y-flip) + clamp/NaN sanitation
- `crates/macos-host/src/touch.rs` created with `DisplayRect`, `CgPoint`, `MapError`, and `map_normalized_to_global(nx, ny, rect) -> Result<CgPoint, MapError>`.
- Affine scale+offset in CG global (top-left) space: `x = rect.x + cx*rect.w`, `y = rect.y + cy*rect.h`, with **no `(1.0 - ny)` inversion** (the regression guard test asserts `ny=0 ⇒ rect.y`).
- Non-finite `nx/ny` rejected with `MapError::NotFinite` before any arithmetic; finite out-of-range values clamped to `[0,1]` before scaling.
- 7 tests (`map_*`): corners/no-flip, center (`*0.5`, not `f64::midpoint`), negative-origin rect, non-unit-scale rect, clamp, NaN reject, inf reject.
- `pub mod touch;` added to `lib.rs`. Commit `628c832`.

### Task 2 — Single-pointer touch→mouse FSM + PointerSink port
- Added `PointerAction` (`Down`/`Move`/`Up` carrying a mapped `CgPoint`), a private `PointerState` (`Idle`/`Down`/`Dragging`, `#[default] Idle`), `PointerStateMachine { state, active: Option<u32> }`, and `pub trait PointerSink { fn dispatch(&mut self, PointerAction); }`.
- `step(&mut self, ev: &protocol::messages::TouchEvent, rect) -> Result<Option<PointerAction>, MapError>`: ignores a different `pointer_id` mid-gesture (D4) before mapping; maps via Task-1's fn (`?` propagates `MapError` without advancing state); folds via a TOTAL `(state, phase)` match (every pair enumerated, no `_` wildcard, no panic).
- 8 tests (`fsm_*`): tap=click, drag=down/moves/up, second-pointer **Move** ignored, second-pointer **Down** ignored (active id stays 1), stray Move no-op, stray Up no-op, MapError propagation (state unchanged), and a `RecordingSink` proving the end-to-end `[Down, Move, Up]` fold with no macOS API. Commit `85f522c`.

## Verification (all green)

| Check | Result |
|-------|--------|
| `cargo test -p macos-host map` | 7 passed |
| `cargo test -p macos-host fsm` | 8 passed |
| `cargo test -p macos-host touch` | 15 passed |
| `cargo clippy -p macos-host --all-targets -- -D warnings` | clean (MSRV-aware) |
| `cargo fmt --check` | clean |
| `cargo test --workspace` | 105 passed (90 baseline + 15 new; +15 delta) |
| grep gate `1.0 -` (non-`#` lines) | 0 |
| `grep -c protocol::messages` | 6 (consumes the shipped type) |
| redefines `struct TouchEvent`/`enum TouchPhase` | 0 (never redefined) |

## Layering / Constraints Honored
- Consumes `protocol::messages::{TouchEvent, TouchPhase}`; never redefines them; names no macOS/Core Graphics FFI type.
- `messages.rs`, `transport.rs`, `encode_vt.rs`, and `Cargo.toml` are unmodified; zero new dependencies.
- MSRV 1.80: `f64::clamp` / `is_finite` / `*0.5` only — no `f64::midpoint` / `is_multiple_of`.
- Task seam honored: after Task 1 no FSM/`PointerAction`/`PointerSink` existed; Task 2 introduced them test-first (an honest compile-fail RED, confirmed before GREEN).
- Threat register (T-P6-01..05) mitigations all unit-tested: NaN/inf reject, out-of-range clamp, total-match no-panic, single-pointer policy, no-Y-flip guard.

## Added after the initial slice (same branch/PR #8, 2026-06-04)
- The real `CGEvent`/`CGEventPost` injection adapter (`CgEventSink`, the macOS `impl PointerSink`, via `core-graphics`), gated on `AXIsProcessTrusted()` (`accessibility_trusted()` FFI) with `InjectError::NotTrusted` onboarding — now lives in `touch.rs` behind `#[cfg(all(target_os = "macos", feature = "live-inject"))]`, plus the `p6_inject` smoke bin. Compile-checked on macOS; live cursor motion verified by hand. Threading contract (post from main thread) documented on `CgEventSink`.

## Still Deferred (device-blocked — NOT in this PR)
- Android `AInputEvent` capture/normalize → `Frame::Touch` over the live transport.
- Sourcing the live `DisplayRect` from `CGDisplayBounds(virtual_display_id)` at runtime (the `p6_inject` bin currently uses `CGDisplay::main().bounds()`).

## Deviations from Plan
None — both tasks executed exactly as written. The `(state, phase)` match was written with every pair enumerated explicitly (rather than a trailing `_ => None`) to get compile-time totality; this is a strictly stronger form of the plan's "total match, no panic" requirement, not a behavioral deviation.

## Known Stubs
None. The `PointerSink` trait is an intentional, documented port (its sole production impl is the explicitly-deferred macOS adapter); the in-CI `RecordingSink` is a real, exercised impl. No placeholder data flows to any output.

## Self-Check: PASSED
- `crates/macos-host/src/touch.rs` — FOUND
- `crates/macos-host/src/lib.rs` contains `pub mod touch` — FOUND
- `.planning/phases/P6-touch-back-channel-macos-injection/P6-01-SUMMARY.md` — FOUND
- Commit `628c832` (Task 1) — FOUND
- Commit `85f522c` (Task 2) — FOUND
