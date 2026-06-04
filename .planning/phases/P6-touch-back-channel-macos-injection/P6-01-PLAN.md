---
phase: P6-touch-back-channel-macos-injection
plan: 01
type: tdd
wave: 1
depends_on: []
files_modified:
  - crates/macos-host/src/touch.rs
  - crates/macos-host/src/lib.rs
autonomous: true
requirements: [TOUCH-01]

user_setup: []

must_haves:
  truths:
    - "map_normalized_to_global maps a normalized top-left (nx,ny) onto a DisplayRect with NO Y-flip: ny=0 yields rect.y (the top edge), ny=1 yields rect.y+rect.h (the bottom edge)"
    - "Mapping handles a non-zero AND a negative-origin DisplayRect (display arranged left/above the main display) by adding rect.x/rect.y — it never assumes origin (0,0)"
    - "Finite out-of-range nx/ny are clamped to [0,1] before scaling; NaN/±inf nx/ny are rejected with MapError::NotFinite (never produce a CgPoint, never panic)"
    - "A tap (Down then Up at the same point) emits PointerAction::Down then PointerAction::Up at the mapped coords (= a click)"
    - "A drag (Down, Move*, Up) emits Down, then Move per Move event, then Up — each carrying the mapped CgPoint"
    - "Single-pointer policy (D4): a second pointer_id arriving mid-gesture is ignored (step returns Ok(None)); a stray Move/Up with no active Down is a no-op (Ok(None))"
    - "The pure layer consumes protocol::messages::{TouchEvent, TouchPhase} and never redefines them; it names NO macOS/Core Graphics FFI type; the CGEvent injection adapter is a deferred impl of the PointerSink port"
  artifacts:
    - path: "crates/macos-host/src/touch.rs"
      provides: "DisplayRect, CgPoint, MapError, map_normalized_to_global, PointerAction, PointerStateMachine, PointerSink port, + inline TDD tests"
      contains: "pub fn map_normalized_to_global"
      min_lines: 150
    - path: "crates/macos-host/src/lib.rs"
      provides: "pub mod touch; declaration"
      contains: "pub mod touch"
  key_links:
    - from: "crates/macos-host/src/touch.rs"
      to: "protocol::messages::TouchEvent"
      via: "PointerStateMachine::step consumes a &protocol::messages::TouchEvent (the shipped wire INPUT type — NOT redefined)"
      pattern: "protocol::messages::\\{?\\s*TouchEvent"
    - from: "crates/macos-host/src/touch.rs"
      to: "PointerSink"
      via: "the injection port the deferred CGEvent adapter implements (mirrors the Transport seam); a test RecordingSink drives the end-to-end fold in CI"
      pattern: "trait PointerSink"
    - from: "crates/macos-host/src/lib.rs"
      to: "crates/macos-host/src/touch.rs"
      via: "module declaration"
      pattern: "pub mod touch"
---

<objective>
Deliver the cable-free half of P6 success criterion #3: a `macos_host::touch` module containing the normalized→global Core Graphics coordinate mapping, an input-sanitation policy (clamp finite / reject NaN), and a single-pointer touch→mouse state machine that emits abstract `PointerAction`s behind a `PointerSink` injection port — all TDD, all unit-testable on the dev host with no Mac graphics APIs, no Pixel, and no cable.

Purpose: TOUCH-01's coordinate-mapping + pointer-FSM logic is fully implementable now. This plan closes that logic at the unit-test level so the deferred hands-on-Mac adapter only has to (a) source the `DisplayRect` from `CGDisplayBounds(virtual_display_id)`, (b) implement `PointerSink` with real `CGEvent`/`CGWarp` injection, and (c) gate it on `AXIsProcessTrusted()`. The genuinely new code is small: two multiplies + two adds with a clamp/finite guard, and a 3-state total FSM — composed behind the project's established port/adapter pattern (mirroring `transport::Transport` and `encode_vt`'s pure-fn-plus-cfg-gated-adapter precedent).

Output:
- `crates/macos-host/src/touch.rs`: `DisplayRect`, `CgPoint`, `MapError`, `map_normalized_to_global`, `PointerAction`, `PointerState`, `PointerStateMachine`, `PointerSink` (the injection port), + a full inline `#[cfg(test)]` TDD suite (incl. a `RecordingSink` that proves the end-to-end fold in CI without any macOS API).
- `crates/macos-host/src/lib.rs`: `pub mod touch;`.

EXPLICITLY DEFERRED (Mac/device-blocked — NOT in this plan): the real `CGEvent`/`CGEventCreateMouseEvent`/`CGEventPost`/`CGWarpMouseCursorPosition` injection adapter (the macOS impl of `PointerSink`); `AXIsProcessTrusted()` gating + the Accessibility-permission onboarding prompt (criterion #3's gating clause); Android `AInputEvent` capture/normalize → `Frame::Touch` over the live transport; sourcing the live `CGDisplayBounds` rect at runtime. These need macOS + an Accessibility grant, the phone, or the live pipeline (P1/P5); none are touched here. The deferred adapter will live in this same `touch.rs` behind `#[cfg(all(target_os = "macos", feature = "live-inject"))]`, exactly as `encode_vt.rs` co-locates its cfg-gated VideoToolbox adapter with the pure `avcc_to_annex_b` logic.

Layering / consumption contract: this module CONSUMES `protocol::messages::{TouchEvent, TouchPhase}` (the shipped P5 wire INPUT type) and MUST NOT redefine or shadow them. It owns input validation because `protocol::TouchEvent` is deliberately NOT range-validated on decode (documented at `messages.rs` ~115–131). Do NOT modify `messages.rs`, `transport.rs`, `encode_vt.rs`, or any existing module.

THE load-bearing correctness fact (RESEARCH, HIGH confidence, pinned to three Apple sources): Core Graphics global display space and normalized Android touch are BOTH top-left origin (y down), so the mapping is a straight scale+offset with **NO Y-flip**. The `(1.0 - ny)` inversion belongs only to AppKit/`NSScreen` (bottom-left), which this phase never touches. Introducing a flip is the single most likely bug; the corner tests are the regression guard.

Zero new dependencies, pure `std` Rust, runs on any CI runner — NO `cfg`-gating of the in-scope code (only the deferred adapter is gated). MSRV 1.80: use `f64::clamp` (stable 1.50), `is_finite`/`is_nan` (long-stable), and arithmetic `(a+b)*0.5` for centers — do NOT use `f64::midpoint` (1.85) or `is_multiple_of` (1.87).

Branch / no-merge: all artifacts and code are committed on the current `feat/p5-protocol-messages` branch (or a P6 branch per the user's preference) only; do NOT merge to `main` without explicit user confirmation. This planning session itself commits nothing.
</objective>

<execution_context>
@$HOME/.claude/get-shit-done/workflows/execute-plan.md
@$HOME/.claude/get-shit-done/templates/summary.md
</execution_context>

<context>
@.planning/phases/P6-touch-back-channel-macos-injection/P6-RESEARCH.md
@.planning/ROADMAP.md
@.planning/REQUIREMENTS.md

# Precedents — MIRROR these patterns; do NOT modify them:
@crates/macos-host/src/transport.rs
@crates/macos-host/src/encode_vt.rs
@crates/macos-host/src/lib.rs
@crates/macos-host/Cargo.toml

# The INPUT type — CONSUME, do NOT redefine:
@crates/protocol/src/messages.rs

<interfaces>
<!-- Contracts the executor consumes without modifying. Extracted from the codebase. -->

From crates/protocol/src/messages.rs (consume; shipped P5 — DO NOT redefine or modify):
```rust
pub enum TouchPhase { Down, Move, Up }   // Copy, Eq, Serialize, Deserialize
pub struct TouchEvent {
    pub pointer_id: u32,
    pub phase: TouchPhase,
    pub nx: f32,   // normalized 0.0..=1.0, NOT range-validated on decode (may be out-of-range or NaN)
    pub ny: f32,   // same — this layer owns clamp/reject
}
// TouchEvent is PartialEq but NOT Eq (f32 fields) — tests compare exactly-representable values only.
```

From crates/macos-host/src/transport.rs (the PORT precedent to mirror — DO NOT modify):
```rust
// The seam pattern: a small trait the live adapter implements; pure logic drives it.
pub trait Transport: Read + Write + Send {}   // aoa.rs is the first (cfg-gated) impl
// A test-only in-memory impl (LoopbackTransport) exercises the end-to-end logic with no hardware.
```

<!-- Recommended type shapes (RESEARCH Patterns 1–3; A1/A2/A4/A5/A6 defaults). Exact names/shape
     are Claude's Discretion within these contracts; the must_haves/tests pin the BEHAVIOR.
       struct DisplayRect { x: f64, y: f64, w: f64, h: f64 }   // CG GLOBAL space, top-left origin; (Copy, PartialEq)
       struct CgPoint { x: f64, y: f64 }                       // same space; feeds CGPoint at the deferred FFI edge; (Copy, PartialEq)
       enum MapError { NotFinite }                             // nx/ny was NaN/±inf — rejected; (Copy, PartialEq, Eq)
       fn map_normalized_to_global(nx: f32, ny: f32, rect: DisplayRect) -> Result<CgPoint, MapError>
       enum PointerAction { Down { at: CgPoint }, Move { at: CgPoint }, Up { at: CgPoint } }   // (Copy, PartialEq)
       enum PointerState { Idle, Down, Dragging }             // private; (Copy, Eq, Default = Idle)
       struct PointerStateMachine { state: PointerState, active: Option<u32> }                 // (Default)
       fn PointerStateMachine::step(&mut self, ev: &TouchEvent, rect: DisplayRect) -> Result<Option<PointerAction>, MapError>
       trait PointerSink { fn dispatch(&mut self, action: PointerAction); }   // the deferred CGEvent adapter is the first impl
     CORE FACT: gx = rect.x + nx*rect.w ; gy = rect.y + ny*rect.h  — NO (1.0 - ny) inversion. -->
</interfaces>
</context>

<tasks>

<!-- ============================ TASK 1 — MAPPING + SANITATION (TDD) ============================ -->

<task type="tdd" tdd="true">
  <name>Task 1: Coordinate mapping (no Y-flip) + clamp/NaN sanitation (TDD)</name>
  <read_first>
    - crates/macos-host/src/touch.rs (the file being created — start empty)
    - crates/protocol/src/messages.rs (TouchEvent/TouchPhase ~104–131 — the INPUT type + the "not range-validated on decode" contract; CONSUME, do not redefine)
    - crates/macos-host/src/transport.rs (the pure-logic + port precedent; inline #[cfg(test)] style to mirror)
    - crates/macos-host/src/encode_vt.rs (pure-fn + cfg-gated-adapter precedent; bounds-checked-no-panic style)
    - crates/macos-host/src/lib.rs (module declarations — `pub mod touch;` is added here)
    - crates/macos-host/Cargo.toml (MSRV rust-version = 1.80 — clippy is MSRV-aware; no new deps)
  </read_first>
  <behavior>
    TASK SEAM (Task 1 vs Task 2): Task 1 adds ONLY the mapping types + `map_normalized_to_global` (and `DisplayRect`, `CgPoint`, `MapError`). It adds NO `PointerStateMachine`, `PointerAction`, `PointerState`, or `PointerSink` — not even stubs. Task 1's green bar depends solely on the pure mapping. The FSM + port are introduced test-first in Task 2. This keeps both red→green cycles honest.

    Write the failing tests FIRST and watch each fail before implementing (red → green → refactor). All tests are pure, host-only, no macOS API, no transport. Mirror the synthetic-buffer / inline `#[cfg(test)] mod tests` style of `transport.rs` and `encode_vt.rs`. Name the mapping tests so the verify filter token `map` matches them all (e.g. `map_corners_no_y_flip`, `map_center`, `map_negative_origin_rect`, `map_non_unit_scale_rect`, `map_clamps_finite_out_of_range`, `map_rejects_nan`, `map_rejects_inf`).

    Mapping — the no-Y-flip core (RESEARCH "The Y-origin decision, pinned" + Code Examples). Use exactly-representable f64 values so `assert_eq!` on `CgPoint` is safe:
    - Test `map_corners_no_y_flip`: with `rect { x: 1512.0, y: 0.0, w: 2400.0, h: 1080.0 }` — `(0.0,0.0)` → `CgPoint { x: 1512.0, y: 0.0 }` (top-left, NOT bottom-left); `(1.0,1.0)` → `{ x: 3912.0, y: 1080.0 }` (bottom-right); and CRUCIALLY `(0.5,0.0).y == 0.0` (ny=0 ⇒ top edge ⇒ NO `(1.0 - ny)` inversion). This is THE regression guard against the Y-flip bug.
    - Test `map_center`: `(0.5,0.5)` → `{ x: 1512.0 + 1200.0, y: 540.0 }` (compute the center with `*0.5`, never `f64::midpoint` — MSRV 1.80).
    - Test `map_negative_origin_rect`: with `rect { x: -2400.0, y: 0.0, w: 2400.0, h: 1080.0 }` (virtual display arranged LEFT of main ⇒ negative origin) — `(0.0,0.0).x == -2400.0` and `(1.0,0.0).x == 0.0`. Proves `rect.x` is added (origin is never assumed `(0,0)`).
    - Test `map_non_unit_scale_rect`: a rect whose `w != h` (e.g. `{ x: 0.0, y: 0.0, w: 2400.0, h: 1080.0 }`) maps `(0.25,0.75)` → `{ x: 600.0, y: 810.0 }` (independent per-axis scaling).

    Input sanitation (RESEARCH Pattern 1 / Assumptions A1; this layer owns it because the protocol does not validate):
    - Test `map_clamps_finite_out_of_range`: with `rect { 0.0, 0.0, 2400.0, 1080.0 }` — `(-0.2, 1.3)` → `{ x: 0.0, y: 1080.0 }` (each axis clamped into [0,1] BEFORE scaling — an edge touch / sensor noise still lands on the edge, not an error).
    - Test `map_rejects_nan`: `(f32::NAN, 0.5)` → `Err(MapError::NotFinite)`; `(0.5, f32::NAN)` → `Err(MapError::NotFinite)`. (Never assert float equality on NaN — assert the error.)
    - Test `map_rejects_inf`: `(f32::INFINITY, 0.5)` and `(0.5, f32::NEG_INFINITY)` → `Err(MapError::NotFinite)`.
  </behavior>
  <action>
    Create `crates/macos-host/src/touch.rs` with a module doc comment in the style of `transport.rs`/`encode_vt.rs` that: states it is the pure, cable-free, CI-green touch-mapping + pointer-FSM core; cites the no-Y-flip coordinate-space fact (CG global = top-left, same as normalized touch); and notes the deferred `CGEvent`/`AXIsProcessTrusted` injection adapter will live in this same file behind `#[cfg(all(target_os = "macos", feature = "live-inject"))]` (mirror `encode_vt.rs`'s deferred-adapter note). Define `DisplayRect { x, y, w, h: f64 }` (derive Debug, Clone, Copy, PartialEq), `CgPoint { x, y: f64 }` (same derives), and `MapError { NotFinite }` (derive Debug, Clone, Copy, PartialEq, Eq) with a doc comment explaining `DisplayRect` is the CG GLOBAL display rect (sourced at runtime by the deferred adapter from `CGDisplayBounds(display_id)`, whose `.origin` already offsets it — possibly negatively — within the same space the cursor APIs consume). Implement `pub fn map_normalized_to_global(nx: f32, ny: f32, rect: DisplayRect) -> Result<CgPoint, MapError>`: (1) if `!nx.is_finite() || !ny.is_finite()` → `Err(MapError::NotFinite)`; (2) `let cx = (nx as f64).clamp(0.0, 1.0);` and same for `cy`; (3) `Ok(CgPoint { x: rect.x + cx * rect.w, y: rect.y + cy * rect.h })`. NO `(1.0 - ny)` anywhere on either axis. No `unwrap`/panic on any input path (input is attacker-influencable). Add `pub mod touch;` to `lib.rs`. Do NOT add the FSM, `PointerAction`, `PointerState`, `PointerSink`, or any stub of them in this task (Task 2, test-first). Do NOT modify `messages.rs`, `transport.rs`, `encode_vt.rs`, or `Cargo.toml`. Per the global commit-hygiene rule, add no unused imports.
  </action>
  <verify>
    <automated>cargo test -p macos-host map &amp;&amp; cargo clippy -p macos-host -- -D warnings &amp;&amp; cargo fmt --check &amp;&amp; cargo test --workspace</automated>
  </verify>
  <acceptance_criteria>
    - `crates/macos-host/src/touch.rs` exists and contains `pub fn map_normalized_to_global` and `pub struct DisplayRect` and `pub struct CgPoint` and `pub enum MapError`.
    - `crates/macos-host/src/lib.rs` contains `pub mod touch;`.
    - `cargo test -p macos-host map` passes and the `map` filter matches ≥1 test (no silent zero-match pass).
    - The corner test asserts `(0.5, 0.0)` maps to `y == rect.y` (proves NO Y-flip); a negative-origin rect and a non-unit-scale rect each have a passing test.
    - `(-0.2, 1.3)` clamps to the rect corner; `f32::NAN`/`f32::INFINITY` on either axis return `Err(MapError::NotFinite)`.
    - `grep -v '^#' crates/macos-host/src/touch.rs | grep -c '1.0 -'` is `0` (no Y-flip / `1.0 - ny` inversion in the source).
    - NO `PointerStateMachine`/`PointerAction`/`PointerSink` exist yet (Task 2 introduces them).
    - `cargo clippy -p macos-host -- -D warnings` is clean (incl. no post-1.80 stdlib API suggestions); `cargo fmt --check` is clean; `cargo test --workspace` is green (existing tests still pass).
  </acceptance_criteria>
  <done>touch.rs exists with `DisplayRect`, `CgPoint`, `MapError`, and `map_normalized_to_global`; corners/center/negative-origin/non-unit-scale all map with NO Y-flip (ny=0 ⇒ rect.y); finite out-of-range clamps and NaN/±inf reject with `MapError::NotFinite` (no panic); the FSM/port are absent; `lib.rs` declares the module; clippy `-D warnings` + fmt clean; `cargo test --workspace` green.</done>
</task>

<!-- ============================ TASK 2 — POINTER FSM + INJECTION PORT (TDD) ============================ -->

<task type="tdd" tdd="true">
  <name>Task 2: Single-pointer touch→mouse FSM + PointerSink port (TDD)</name>
  <read_first>
    - crates/macos-host/src/touch.rs (the file from Task 1 — add the FSM + port here)
    - crates/protocol/src/messages.rs (TouchEvent/TouchPhase — consumed by `step`; do not redefine)
    - crates/macos-host/src/transport.rs (the `Transport` PORT + test-only `LoopbackTransport` precedent to mirror with `PointerSink` + a `RecordingSink`)
    - .planning/phases/P6-touch-back-channel-macos-injection/P6-RESEARCH.md (Pattern 2 FSM reference impl + the drag/tap/second-pointer/stray-move test set)
  </read_first>
  <behavior>
    TASK SEAM: this task INTRODUCES `PointerAction`, `PointerState`, `PointerStateMachine`, and the `PointerSink` trait for the first time — none existed after Task 1. Write the failing tests FIRST and watch them fail (the types/function are absent ⇒ compile-fail is the red bar), then implement. `step` consumes `protocol::messages::{TouchEvent, TouchPhase}` and maps via `map_normalized_to_global` from Task 1. Single-pointer only (D4 / RESEARCH A2). Name tests so the verify filter token `fsm` matches them (e.g. put them in a `mod fsm_tests` or prefix each `fsm_`): `fsm_tap_is_down_then_up`, `fsm_drag_emits_down_moves_up`, `fsm_second_pointer_ignored_mid_gesture`, `fsm_second_pointer_down_ignored_mid_gesture`, `fsm_stray_move_without_down_is_noop`, `fsm_up_without_down_is_noop`, `fsm_propagates_map_error`, `fsm_recording_sink_drives_end_to_end`.

    A small `ev(id, phase, nx, ny) -> TouchEvent` test helper keeps the cases terse (RESEARCH Code Examples). Use `rect { 0.0, 0.0, 2400.0, 1080.0 }` and exactly-representable normalized values so mapped coords are exact.

    - Test `fsm_tap_is_down_then_up` (criterion #1 = click): `Down(0.25,0.75)` then `Up(0.25,0.75)` on a fresh machine → first step `Ok(Some(PointerAction::Down { at }))`, second `Ok(Some(PointerAction::Up { at }))`, both `at` = the SAME mapped point (a click at one location).
    - Test `fsm_drag_emits_down_moves_up` (criterion #2 = click-and-drag): `Down(0.0,0.0)`, `Move(0.5,0.5)`, `Move(1.0,1.0)`, `Up(1.0,1.0)` → `Some(Down{at:(0,0)})`, `Some(Move{at:(1200,540)})`, `Some(Move{at:(2400,1080)})`, `Some(Up{at:(2400,1080)})` in order.
    - Test `fsm_second_pointer_ignored_mid_gesture` (D4): after `Down(id=1)`, a `Move(id=2, ...)` → `Ok(None)` (a different finger must NOT steal the cursor mid-gesture).
    - Test `fsm_second_pointer_down_ignored_mid_gesture` (D4, plan-check WARNING-1): after `Down(id=1)`, a second `Down(id=2, ...)` mid-gesture → `Ok(None)` AND the active pointer stays `id=1` (a subsequent `Move(id=1, ...)` still emits `Some(Move{..})`). Pins the more aggressive hijack path that the `(Down|Dragging, Down)` → `Ok(None)` arm already handles.
    - Test `fsm_stray_move_without_down_is_noop`: on a fresh machine, `Move(id=1,...)` → `Ok(None)`.
    - Test `fsm_up_without_down_is_noop`: on a fresh machine, `Up(id=1,...)` → `Ok(None)`.
    - Test `fsm_propagates_map_error`: a `Down` whose `nx` is `f32::NAN` → `Err(MapError::NotFinite)` (the FSM surfaces the mapping error; it does not panic and does not silently swallow it — note: state is not advanced on a rejected event).
    - Test `fsm_recording_sink_drives_end_to_end`: define a test-only `RecordingSink { actions: Vec<PointerAction> }` impl of `PointerSink` (mirrors `transport.rs`'s `LoopbackTransport`); feed a Down→Move→Up sequence through `step`, push each emitted action into the sink via `dispatch`, and assert the recorded `Vec` is exactly `[Down, Move, Up]` with the expected mapped coords. This proves the whole fold is CI-testable WITHOUT any macOS API — the same way `LoopbackTransport` tests `echo_roundtrip`.
  </behavior>
  <action>
    In `touch.rs`, add (NEW in this task — Task 1 deliberately omitted them): `PointerAction` (`Down { at: CgPoint }`, `Move { at: CgPoint }`, `Up { at: CgPoint }`; derive Debug, Clone, Copy, PartialEq), a PRIVATE `PointerState` (`Idle`, `Down`, `Dragging`; derive Debug, Clone, Copy, PartialEq, Eq, Default with `#[default] Idle`), `PointerStateMachine { state: PointerState, active: Option<u32> }` (derive Debug, Default), and `pub trait PointerSink { fn dispatch(&mut self, action: PointerAction); }` with a doc comment naming it the injection port (D0 swap point) that the DEFERRED macOS `CGEvent` adapter implements — exactly as `aoa.rs` is the first impl of the `Transport` seam. Implement `pub fn step(&mut self, ev: &protocol::messages::TouchEvent, rect: DisplayRect) -> Result<Option<PointerAction>, MapError>` per RESEARCH Pattern 2: (a) if a gesture is active (`self.active == Some(id)`) and `ev.pointer_id != id` and `ev.phase != Down` → return `Ok(None)` (ignore the second pointer, D4) WITHOUT mapping; (b) compute `let at = map_normalized_to_global(ev.nx, ev.ny, rect)?;` (this `?` propagates `MapError` and leaves state unchanged); (c) `match (self.state, ev.phase)`: `(Idle, Down)` → set `active = Some(id)`, `state = Down`, emit `Down{at}`; `(Down | Dragging, Move)` → `state = Dragging`, emit `Move{at}`; `(Down | Dragging, Up)` → `state = Idle`, `active = None`, emit `Up{at}`; all other `(state, phase)` pairs → `Ok(None)` (stray Move/Up with no active pointer, or a second pointer's Down while active). The match MUST be total over every `(PointerState, TouchPhase)` pair (no `unwrap`, no panic — hostile/out-of-order input is a no-op, not a crash). Import `protocol::messages::{TouchEvent, TouchPhase}` (used by the match) — and no unused imports (global commit-hygiene rule). Do NOT redefine `TouchEvent`/`TouchPhase`. Do NOT name any macOS/Core Graphics type in this pure code. Do NOT modify `messages.rs`/`transport.rs`/`encode_vt.rs`/`Cargo.toml`.
  </action>
  <verify>
    <automated>cargo test -p macos-host fsm &amp;&amp; cargo test -p macos-host touch &amp;&amp; cargo clippy -p macos-host -- -D warnings &amp;&amp; cargo fmt --check &amp;&amp; cargo test --workspace</automated>
  </verify>
  <acceptance_criteria>
    - `crates/macos-host/src/touch.rs` contains `pub trait PointerSink`, `pub enum PointerAction`, and `impl PointerStateMachine` with `pub fn step`.
    - `cargo test -p macos-host fsm` passes and the `fsm` filter matches ≥1 test.
    - Tap → Down then Up at the same mapped point; drag → Down, Move(s), Up at mapped coords; a second pointer_id mid-gesture and a stray Move/Up each return `Ok(None)`; a NaN-coord Down returns `Err(MapError::NotFinite)`.
    - A test `RecordingSink` (impl of `PointerSink`) records exactly `[Down, Move, Up]` for a Down→Move→Up sequence (end-to-end fold tested with no macOS API).
    - `grep -c 'protocol::messages' crates/macos-host/src/touch.rs` is `>= 1` (consumes the shipped type) and `grep -v '^#' crates/macos-host/src/touch.rs | grep -c 'struct TouchEvent\|enum TouchPhase'` is `0` (does NOT redefine it).
    - `cargo clippy -p macos-host -- -D warnings` clean; `cargo fmt --check` clean; `cargo test --workspace` green.
  </acceptance_criteria>
  <done>`PointerAction`, `PointerState`, `PointerStateMachine::step`, and the `PointerSink` port exist (all introduced test-first this task); tap=click, drag=down/moves/up, second-pointer-ignored, stray-move/up no-op, and NaN-propagation all pass; a `RecordingSink` proves the end-to-end fold in CI; the FSM consumes (never redefines) `protocol::messages::{TouchEvent, TouchPhase}` and names no macOS type; the `(state, phase)` match is total (no panic on hostile input); clippy `-D warnings` + fmt clean; `cargo test --workspace` green.</done>
</task>

</tasks>

<threat_model>
## Trust Boundaries

| Boundary | Description |
|----------|-------------|
| wire `TouchEvent` (peer) → `PointerStateMachine::step` / `map_normalized_to_global` | Attacker- or corruption-controlled `nx`/`ny` (NOT range-validated on decode) and `pointer_id`/`phase` cross into the pure mapping/FSM logic |
| pure `PointerAction` → `PointerSink` port (DEFERRED) | The injection port boundary; the deferred macOS `CGEvent` adapter renders actions as real cursor events, gated by the OS |

Note: the transport is a single physical USB peer (not network-exposed) in the MVP, so confidentiality/auth threats are out of scope here (flagged for P7/P8 if an NCM/TCP-over-USB path is chosen). `pointer_id` is a typed `u32` and `phase` a typed enum — no parsing, so no injection/overflow surface beyond the floats.

## STRIDE Threat Register

| Threat ID | Category | Component | Disposition | Mitigation Plan |
|-----------|----------|-----------|-------------|-----------------|
| T-P6-01 | Tampering | NaN/±inf `nx,ny` → garbage `CgPoint` / cursor to an undefined location | mitigate | `map_normalized_to_global` rejects non-finite input with `MapError::NotFinite` BEFORE any arithmetic; the FSM propagates the error and does not advance state. Unit-tested (`map_rejects_nan`/`map_rejects_inf`, `fsm_propagates_map_error`, Tasks 1/2). |
| T-P6-02 | Tampering | out-of-range `nx,ny` → cursor escapes the virtual display onto another display | mitigate | `(nx,ny)` clamped to `[0,1]` before scaling, so the mapped point can never leave the supplied `DisplayRect`. Unit-tested (`map_clamps_finite_out_of_range`, Task 1). |
| T-P6-03 | Denial of Service | hostile / out-of-order `TouchEvent` → panic taking down the host | mitigate | Mapping returns `Result` (no `unwrap`); the FSM `(state, phase)` match is TOTAL (every pair handled, strays are `Ok(None)`); no indexing/`unwrap` on wire data. Unit-tested (stray-move/up no-op, total-match coverage, Task 2). |
| T-P6-04 | Tampering / DoS | multi-pointer flood → two fingers fight for the cursor (cursor teleports) | mitigate | Single-pointer policy (D4): one `active: Option<u32>`; a second `pointer_id` mid-gesture is ignored (`Ok(None)`). Unit-tested (`fsm_second_pointer_ignored_mid_gesture`, Task 2). |
| T-P6-05 | Spoofing | wrong-vertical-position cursor (silent correctness failure) from an erroneous Y-flip | mitigate | The mapping stays entirely in CG global (top-left) space — no `(1.0 - ny)` inversion; the `map_corners_no_y_flip` test (ny=0 ⇒ rect.y) is the regression guard, plus a `grep` gate asserting no `1.0 -` in source. (Task 1.) |
| T-P6-EoP | Elevation of Privilege | unauthorized cursor control once wired | accept (deferred) | The real injection (deferred macOS `CGEvent` adapter) is gated by macOS **Accessibility** trust via `AXIsProcessTrusted()` — the OS denies injection without an explicit user grant. Out of scope for this pure slice; noted as the seam the deferred adapter must honor. |

Note: there is NO supply-chain threat (T-*-SC) for this plan — it adds ZERO external packages (pure `std` Rust in the existing `macos-host` crate). No package-legitimacy checkpoint is required; RESEARCH's Package Legitimacy Audit confirms "Not applicable to this slice." The deferred `core-graphics`/`.mm`-shim decision is vetted in the hands-on-Mac wave that writes the injection adapter.
</threat_model>

<verification>
## Cable-free (CI-green now — no Mac graphics API, no Pixel, no transport)
- `cargo test -p macos-host map` — corners with NO Y-flip (ny=0 ⇒ rect.y), center, negative-origin rect, non-unit-scale rect, clamp finite out-of-range, reject NaN/±inf.
- `cargo test -p macos-host fsm` — tap=click, drag=down/moves/up, second-pointer ignored, stray Move/Up no-op, NaN propagation, and the `RecordingSink` end-to-end fold.
- `cargo test -p macos-host touch` — the whole module (both `map` and `fsm` suites) green.
- `cargo clippy -p macos-host -- -D warnings` — clean, incl. MSRV-aware (no post-1.80 stdlib API).
- `cargo fmt --check` — clean.
- `cargo test --workspace` — existing tests (transport/encode/protocol) stay green alongside the new `touch` tests.

Note: the filtered tokens `map` and `fsm` each match ≥1 test by the naming convention pinned in Task 1/Task 2, so no filtered `cargo test` can silently pass on zero matches.

## Deferred (Mac / Accessibility grant / phone — NOT verified here, by necessity)
- Real `CGEvent`/`CGEventCreateMouseEvent`/`CGEventPost`/`CGWarpMouseCursorPosition` injection (the macOS impl of `PointerSink`) → needs macOS + the live virtual display.
- `AXIsProcessTrusted()` gating + the actionable Accessibility-permission onboarding prompt → needs macOS + a user grant.
- Android `AInputEvent` capture/normalize → `Frame::Touch` over the live transport → needs the Pixel 6a + the P1/P5 live pipeline.
- Sourcing the live `DisplayRect` from `CGDisplayBounds(virtual_display_id)` at runtime → the deferred adapter's job (the pure fn takes the rect as a parameter).

## TOUCH-01 / criterion #3 → task map
| Success criterion clause | Cable-free now? | Closed by |
|--------------------------|-----------------|-----------|
| #3 "Coordinate mapping (normalized → global CG coords) is TDD-verified" | yes | Task 1 (corners/center/negative-origin/non-unit-scale, NO Y-flip + clamp/NaN) |
| #1 "Tapping moves the cursor to the correct location" — the LOGIC (tap → click at mapped coords) | yes (logic only) | Task 2 (`fsm_tap_is_down_then_up`); the actual cursor movement is deferred |
| #2 "Dragging produces click-and-drag (mouseDown → move → mouseUp)" — the LOGIC | yes (logic only) | Task 2 (`fsm_drag_emits_down_moves_up`); the actual drag on the Mac is deferred |
| #3 "injection is gated on `AXIsProcessTrusted()` with an actionable Accessibility prompt" | NO — Mac + grant-blocked | deferred (macOS `CGEvent` adapter wave) |
| #1/#2 actual cursor movement / click-drag on a real Mac | NO — Mac-blocked | deferred |
</verification>

<success_criteria>
- `crates/macos-host/src/touch.rs` exists with `DisplayRect`, `CgPoint`, `MapError`, `map_normalized_to_global`, `PointerAction`, `PointerState` (private), `PointerStateMachine::step`, and the `PointerSink` port; `lib.rs` declares `pub mod touch;`.
- Coordinate mapping is TDD-verified (red→green→refactor) with NO Y-flip: corners (ny=0 ⇒ rect.y), center, a negative-origin rect, and a non-unit-scale rect all map correctly; the source contains no `(1.0 - ny)` inversion (grep-gated).
- Input sanitation: finite out-of-range `nx,ny` clamp into `[0,1]` before scaling; NaN/±inf reject with `MapError::NotFinite`; no panic on hostile input.
- The single-pointer FSM is TDD-verified: tap = Down then Up at one mapped point (= click, criterion #1 logic); drag = Down, Move(s), Up at mapped coords (criterion #2 logic); a second `pointer_id` mid-gesture is ignored (D4); stray Move/Up are no-ops; a NaN-coord event propagates `MapError`; a `RecordingSink` proves the end-to-end fold in CI with no macOS API.
- Layering preserved: the module CONSUMES `protocol::messages::{TouchEvent, TouchPhase}` and never redefines them; it names no macOS/Core Graphics FFI type; the `PointerSink` port is the documented swap point the deferred `CGEvent` adapter implements; `messages.rs`/`transport.rs`/`encode_vt.rs`/`Cargo.toml` are unmodified.
- Task seam honored: after Task 1, NO FSM/`PointerAction`/`PointerSink` exist; Task 2 introduces them test-first — both TDD cycles have an honest red bar.
- Zero new dependencies; MSRV 1.80 respected (no post-1.80 stdlib API); no unused imports (global commit-hygiene rule); `cargo clippy -p macos-host -- -D warnings` + `cargo fmt --check` clean; `cargo test --workspace` green.
- Branch hygiene: all code/docs committed on the feature branch only; NO merge to `main` without explicit user confirmation.
</success_criteria>

<output>
Create `.planning/phases/P6-touch-back-channel-macos-injection/P6-01-SUMMARY.md` when done.
Commit `crates/macos-host/src/touch.rs`, the `lib.rs` change, and the SUMMARY on the feature branch. Do NOT merge to `main` without explicit user confirmation.
</output>
