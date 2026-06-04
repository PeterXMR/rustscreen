# Phase P6: Touch Back-Channel → macOS Injection — Research (coordinate-mapping + pointer state machine, cable-free slice)

**Researched:** 2026-06-03
**Domain:** Pure coordinate-space mapping (normalized top-left touch → Core Graphics global display coordinates) + a single-pointer touch→mouse state machine, as platform-agnostic, TDD-verifiable Rust logic behind a port the future `CGEvent` adapter implements.
**Confidence:** HIGH — the one load-bearing correctness fact (CG global display coords are **top-left** origin, the same as normalized Android touch, so **no Y-flip is required**) is confirmed against three independent Apple sources; the rest is pure arithmetic + a small FSM composed behind the project's established port/adapter pattern.

## Summary

P6's success criteria split the same way P5's did: a **logic** half (criterion #3's coordinate mapping — "normalized → global CG coords is TDD-verified") that is fully implementable and unit-testable on a dev host with no Mac graphics APIs, no phone, and no cable; and an **injection** half (criteria #1/#2's *actual* cursor movement and click-drag via `CGEvent`, gated on `AXIsProcessTrusted()` with an onboarding prompt) that needs macOS, Accessibility permission, and ultimately live touch capture on Android. This research scopes **only the logic half**.

The deliverable is a new **pure module** — recommended `crates/macos-host/src/touch.rs` — containing two cooperating pieces: (1) a `map_normalized_to_global(nx, ny, rect) -> CgPoint` function that places a normalized touch point onto a specific virtual-display rectangle expressed in the Core Graphics global display coordinate space; and (2) a single-pointer `PointerStateMachine` that folds a `TouchEvent { phase: Down|Move|Up }` sequence into abstract `PointerAction`s (`MouseDown` → `MouseMoved`* → `MouseUp`) that a later `CGEvent` adapter consumes. Both sit behind a small `PointerSink`/injection **port** (mirroring the existing `Transport`, `Capturer`, `Encoder` seams) so the macOS `CGEvent` adapter — and `AXIsProcessTrusted()` gating — drop in later without touching the tested core.

**The key correctness decision (research questions 1–3), resolved with authoritative sources:** `CGWarpMouseCursorPosition`, `CGEventCreateMouseEvent`/`CGEventPost`, and `CGDisplayBounds` **all** operate in the *Quartz global display coordinate space*, whose origin is the **top-left** corner of the **main** display, with **y increasing downward**. Normalized Android/protocol touch is *also* top-left origin (ny=0 at top). Therefore the mapping is a straight affine scale-and-offset with **no Y-axis inversion**: `gx = rect.x + nx*rect.w`, `gy = rect.y + ny*rect.h`. The infamous macOS Y-flip applies only when crossing between Core Graphics (top-left) and **AppKit/`NSScreen`** (bottom-left) — and this phase never touches AppKit, so the flip must **not** be introduced. The multi-display subtlety is handled *for free* by sourcing `rect` from `CGDisplayBounds(virtual_display_id)`, whose `origin` is already the virtual display's offset within the same global space the cursor functions consume.

**Primary recommendation:** Add `crates/macos-host/src/touch.rs` (no new external dependencies). Define a plain `DisplayRect { x, y, w, h: f64 }` (CG global, top-left) and `CgPoint { x, y: f64 }`; implement `map_normalized_to_global` with **clamp** (not reject) of `nx,ny` to `[0,1]` and explicit **NaN→reject** (`Result`/`Option`), since `protocol::TouchEvent` does not validate on decode. Implement a `PointerStateMachine` emitting `PointerAction` (`Down{at}`, `Move{at}`, `Up{at}`) with drag semantics (Down→Move*→Up). Define a `PointerSink` trait (the injection port). TDD every branch: corners/center mapping (no flip), clamp, NaN, the Down→Move→Up drag sequence, and out-of-order/defensive transitions. Defer all `CGEvent`/`CGWarp`/`AXIsProcessTrusted` work and Android `AInputEvent` capture to the hands-on-Mac/hands-on-device continuation of P6.

## User Constraints

> No CONTEXT.md exists for this phase yet (this is standalone research). The constraints below are the locked project decisions (PROJECT.md D0–D7), the ROADMAP P6 spec, the session slicing constraint (no Mac APIs / no phone / no cable / CI-green), and standing project memory. A later discuss-phase may add a CONTEXT.md that supersedes these.

### Locked Decisions (PROJECT.md + ROADMAP P6 + session constraint)
- **D4 (LOCKED):** **Single-pointer mouse emulation only.** Map exactly one active pointer to the system cursor (mouseDown → move(s) → mouseUp). Multitouch/pen is explicitly v2 (INPUT-V2-01, deferred). The state machine tracks a single active pointer; additional `pointer_id`s are ignored/queued-out, not multiplexed.
- **D0 (LOCKED):** "100% Rust" = simplest-now, shrink-to-Rust-later via **ports & adapters**; core logic never changes on adapter swap. → Pure coordinate/FSM logic behind a port; the `CGEvent` injection adapter is swappable later (and is the macOS-only adapter, mirroring how `aoa.rs`/`encode_vt.rs` sit behind `Transport`/`Encoder`).
- **TOUCH-01 (the requirement):** the cable-free slice satisfies *only* its TDD coordinate-mapping clause ("mapped normalized→global CG coords (TDD)"). The `AInputEvent` capture/normalize on Android, `CGEvent` injection, and `AXIsProcessTrusted()` gating are deferred.
- **MSRV = Rust 1.80** (`macos-host` pins `rust-version = "1.80"`; clippy is MSRV-aware). Do **not** use stdlib APIs stabilized after 1.80 (e.g. avoid `f64::midpoint` [1.85], `{integer}::is_multiple_of` [1.87], `f32::next_up`/`next_down` [1.86]). `f64::clamp`/`f32::clamp` (stable 1.50), `is_nan`/`is_finite` (long-stable), `f64::round`/`mul_add` are all fine.
- **TouchEvent fields use `f32`** (already shipped in `protocol::messages`); `nx,ny` are **not** range-validated on decode (a peer may send out-of-range or `NaN`). Because of `f32`, `TouchEvent` is `PartialEq` but **not** `Eq` — tests compare only exactly-representable values; never assert equality on NaN.
- **TDD, clippy `-D warnings`, fmt clean** for all logic (project testing convention).
- **`protocol` is `no_std`-friendly; `TouchEvent` is the INPUT type — do NOT redefine it.** The mapping *consumes* `protocol::messages::{TouchEvent, TouchPhase}`; it must not duplicate or shadow them.

### Claude's Discretion (within the locked design)
- Crate placement of the pure module (recommended: `crates/macos-host/src/touch.rs` — rationale in the Architectural Responsibility Map). A `protocol::coords` placement is the considered alternative.
- Exact type names/shape of `DisplayRect` / `CgPoint` / `PointerAction` / `PointerSink`, and whether the rect uses `f64` (recommended — CG uses `CGFloat` = `f64` on arm64) or integer pixels.
- Whether out-of-range input is **clamped** vs **rejected** (recommended: clamp finite out-of-range; reject NaN — flagged `[ASSUMED]`, see Assumptions A1).
- Whether the state machine is a value-returning fold (`step(&mut self, ev) -> Option<PointerAction>` / `SmallVec`) or pushes into a `PointerSink`. Recommendation below.
- Single-pointer multiplexing policy when a *second* `pointer_id` arrives mid-gesture (recommended: ignore the second; flagged `[ASSUMED]`, A2).

### Deferred Ideas (OUT OF SCOPE — Mac/device-blocked)
- Actual `CGEvent`/`CGEventCreateMouseEvent`/`CGEventPost` and `CGWarpMouseCursorPosition` injection (needs macOS + Accessibility permission).
- `AXIsProcessTrusted()` gating + the actionable Accessibility-permission onboarding prompt (criterion #3's gating clause).
- Capturing `AInputEvent` on Android, normalizing to `nx,ny`, and wiring `Frame::Touch` over the live transport (needs the phone + P1/P5 live pipeline).
- Acquiring the virtual display's live `CGDisplayBounds` at runtime (the adapter does this; the pure mapping receives the rect as a parameter).
- Multitouch / pen / scroll / right-click / modifier keys (v2, INPUT-V2-01).

## Phase Requirements

| ID | Description | Research Support (cable-free slice only) |
|----|-------------|-------------------------------------------|
| TOUCH-01 (partial) | "...normalized on Android, **mapped normalized→global CG coords (TDD)**, injected via `CGEvent`, gated on `AXIsProcessTrusted()` with actionable onboarding." | This research covers the **TDD-able coordinate mapping** (normalized top-left → CG global top-left, with the Y-origin decision pinned to authoritative Apple docs, clamp/NaN policy) **and** the **pure single-pointer touch→mouse state machine** (Down→Move*→Up drag semantics) behind an injection **port**. The remainder of TOUCH-01 — `AInputEvent` capture/normalize on Android, real `CGEvent` injection, `AXIsProcessTrusted()` gating + onboarding — is Mac/device-blocked and deferred (matches success criteria #1/#2 + #3's gating clause). |

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Define `TouchEvent`/`TouchPhase` (the INPUT type) | `protocol` crate (`messages.rs`) — **already shipped** | — | Shared wire type; both host and (future) client depend on it. **Do not redefine.** |
| Normalized→global CG coordinate math (`map_normalized_to_global`) | `macos-host` pure module (`touch.rs`) | — | Platform-agnostic arithmetic, but its *target* coordinate space is a macOS (Core Graphics) concept; it is consumed only by the macOS host. Pure (no FFI) → unit-testable in CI. |
| Clamp/reject of out-of-range & NaN `nx,ny` | `macos-host::touch` (mapping fn) | `protocol` (does NOT validate — documented) | The protocol deliberately leaves validation to the consumer (see `messages.rs` lines ~115–131). The mapping layer is the consumer. |
| Single-pointer touch→mouse state machine (`PointerStateMachine`) | `macos-host::touch` | — | Pure FSM, no I/O → fully testable; produces abstract `PointerAction`s the adapter renders. |
| Drag detection (Down→Move*→Up) | `macos-host::touch` (FSM) | — | Emergent from the FSM; tested as a sequence. |
| Inject `CGEvent`/warp the cursor | **DEFERRED — macOS `CGEvent` adapter behind `PointerSink` port** | — | Needs macOS + Accessibility permission; the adapter implements the port. Mirrors `aoa.rs`/`encode_vt.rs` behind `Transport`/`Encoder`. |
| `AXIsProcessTrusted()` gate + onboarding | **DEFERRED — macOS adapter** | — | Permission/UX, hands-on-Mac. |
| Obtain the virtual display's `CGDisplayBounds` rect | **DEFERRED — macOS adapter** (feeds the pure mapping a `DisplayRect`) | `cg-virtual-display` exposes `display_id()` | The pure fn takes the rect as a parameter; the adapter sources it from `CGDisplayBounds(display_id)` at runtime. |
| Capture `AInputEvent`, normalize to `nx,ny` | **DEFERRED — `android-client` tier** | — | Needs the phone; produces the `Frame::Touch` the host consumes. |

**Crate-placement recommendation (research-question-adjacent): `crates/macos-host/src/touch.rs`.**
Rationale: although the coordinate *arithmetic* is platform-agnostic, its semantics are macOS-specific — the target is the **Core Graphics global display coordinate space**, and the only consumer is the macOS host (the host injects; the phone only captures/sends normalized coords). Placing it in `macos-host` (a) keeps `protocol` free of macOS-coordinate-space concepts and truly `no_std`/cross-platform, (b) co-locates the pure logic with the `CGEvent` adapter that will implement the `PointerSink` port (exactly as `transport.rs` co-locates the pure echo logic with the `aoa.rs` live adapter, and `encode_vt.rs` co-locates pure AVCC→Annex-B with the VideoToolbox adapter), and (c) means CI already builds/tests it on every runner (the pure code is **not** `#[cfg(target_os = "macos")]`-gated; only the future adapter is). `protocol::coords` is the considered alternative but is rejected: it would push a CG-coordinate concept into the cross-platform wire crate and split the logic from its adapter. `[ASSUMED]` — confirm placement in discuss-phase; A3.

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| (none — `core`/`std` only) | — | The mapping is `f64` arithmetic + `clamp`/`is_nan`; the FSM is a plain enum match. | Zero new dependencies for the cable-free slice. `macos-host` already depends on `protocol` (for `TouchEvent`) and `log`. Adding nothing keeps the slice trivially CI-green and MSRV-1.80-safe. `[VERIFIED: in-repo Cargo.toml]` |

### Supporting (DEFERRED — the macOS injection adapter only; NOT this slice)
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| `core-graphics` | `0.24.0` | Safe-ish Rust bindings for `CGEvent*`, `CGDisplayBounds`, `CGWarpMouseCursorPosition` | Only in the deferred hands-on-Mac adapter, behind `#[cfg(target_os = "macos")]` + a `live-inject` feature. `[ASSUMED]` (registry name plausible, NOT slopcheck-verified; the project's existing macOS FFI is hand-rolled ObjC++/`extern "C"`, so the adapter may equally use a `.mm`/`extern "C"` shim like `cg-virtual-display` does — decide in the deferred wave). |
| `objc2` / `objc2-app-kit` | latest | Pure-Rust ObjC for `AXIsProcessTrusted` / onboarding | Deferred; also a P7 purity target. `[ASSUMED]` |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| `f64` rect/point | integer-pixel rect/point | CG uses `CGFloat` (= `f64` on arm64) and `CGPoint`/`CGRect` are `f64`; the warp/event APIs take `CGPoint`. Using `f64` end-to-end avoids a lossy cast at the boundary and matches the eventual adapter. Integers would force rounding decisions into the pure layer prematurely. Recommend `f64`. |
| Returning `PointerAction` from `step()` | Pushing into a `&mut dyn PointerSink` | Returning a value keeps the FSM purely functional and trivially testable (assert on the returned action); the sink is then driven by a thin loop in the adapter. Recommend **return** (`Option<PointerAction>` / small `Vec`), with `PointerSink` as the *separate* injection port the adapter implements. |
| A hand-rolled affine struct | A matrix/`glam` dep | Two multiplies + two adds need no linear-algebra crate. YAGNI. |
| Clamp out-of-range | Reject out-of-range | See Assumptions A1 — clamp finite values (a touch at the very edge rounds to nx≈1.0001 from sensor noise should still click the edge, not error); reject only NaN (genuinely meaningless). |

**Installation (this slice):** *No `Cargo.toml` change.* The module is pure `std` Rust added to the existing `macos-host` crate. (The deferred adapter's `core-graphics`/feature-gate is a later wave's concern.)

**Version verification performed:** toolchain `rustc 1.95.0` (project MSRV pinned to 1.80 in `macos-host/Cargo.toml`); workspace members confirmed (`protocol`, `macos-host`, `cg-virtual-display`, `android-client`). No new crate is added by this slice, so no registry/slopcheck gate is required for it (see Package Legitimacy Audit).

## Package Legitimacy Audit

> **Not applicable to this slice** — it adds **zero external packages**. The module is pure `std` Rust inside the existing `macos-host` crate (which already depends on the in-tree `protocol` and on `log 0.4`).

| Package | Registry | Age | Downloads | Source Repo | slopcheck | Disposition |
|---------|----------|-----|-----------|-------------|-----------|-------------|
| (none added by this slice) | — | — | — | — | n/a | No-op |
| `core-graphics` *(DEFERRED adapter only)* | crates.io | mature (servo-org lineage) | high | github.com/servo/core-foundation-rs | unavailable | **DEFERRED** — vet in the hands-on-Mac wave; `[ASSUMED]`. The project may instead extend its existing hand-rolled ObjC++/`extern "C"` shim pattern (`cg-virtual-display`), needing no crate at all. |

**Packages removed due to slopcheck [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** none
**Note:** slopcheck/registry verification for `core-graphics` (or the decision to hand-roll a shim) is deferred to the wave that actually writes the injection adapter — it is not on the critical path for the cable-free coordinate logic.

## Architecture Patterns

### System Architecture Diagram (the cable-free touch-mapping slice)

```
   (DEFERRED, Android)                  ┌──────────────── macos-host::touch (pure, CI-green) ────────────────┐
   AInputEvent ──► normalize  in-scope→ │                                                                    │
        │          to nx,ny [top-left]  │   protocol::messages::TouchEvent { pointer_id, phase, nx, ny }      │
        ▼                               │            │  (INPUT type — consumed, NOT redefined)                │
   Frame::Touch ──(transport, DEFERRED)─┼──────────► │                                                        │
                                        │            ▼                                                        │
                                        │   PointerStateMachine::step(&mut self, ev)                          │
                                        │      single active pointer (D4):                                    │
                                        │        Idle --Down--> Down(at)   ──► emit PointerAction::Down{at}    │
                                        │        Down --Move--> Dragging   ──► emit PointerAction::Move{at}    │
                                        │        Dragging --Move--> Dragging ─► emit PointerAction::Move{at}   │
                                        │        (Down|Dragging) --Up--> Idle ► emit PointerAction::Up{at}     │
                                        │            │                                                         │
                                        │            │ each action carries `at: CgPoint`, produced by:        │
                                        │            ▼                                                         │
                                        │   map_normalized_to_global(nx, ny, rect: DisplayRect) -> CgPoint    │
                                        │      reject NaN ▸ clamp finite to [0,1] ▸                            │
                                        │      gx = rect.x + nx*rect.w   (NO Y-flip — both top-left)           │
                                        │      gy = rect.y + ny*rect.h                                         │
                                        │            │                                                         │
                                        │            ▼  Vec<PointerAction> / Option<PointerAction>            │
                                        │   trait PointerSink { fn dispatch(&mut self, PointerAction); }       │
                                        └────────────┬───────────────────────────────────────────────────────┘
                                                     │  (port boundary)
                                  DEFERRED, macOS ──►│  impl PointerSink for CgEventInjector  (hands-on-Mac)
                                                     │     Down  -> CGEventCreateMouseEvent(LeftMouseDown) + post
                                                     │     Move  -> CGWarp / mouseDragged event + post
                                                     │     Up    -> LeftMouseUp + post
                                                     │     all gated on AXIsProcessTrusted()
                                                     ▼
                                  rect sourced at runtime from CGDisplayBounds(virtual_display_id)  [DEFERRED]
```
Trace the primary use case: a decoded `TouchEvent` (top-left normalized) is fed to `PointerStateMachine::step`; the FSM maps its `nx,ny` onto the supplied `DisplayRect` via `map_normalized_to_global` (no Y inversion) and emits one abstract `PointerAction` carrying the global `CgPoint`. The action crosses the `PointerSink` port; the deferred `CGEvent` adapter renders it as real mouse events on the virtual display. The `rect` itself comes (deferred) from `CGDisplayBounds(display_id)`, whose origin already places the virtual display within the same global space the cursor APIs use.

### Recommended file layout
```
crates/macos-host/src/
├── lib.rs        # add `pub mod touch;`  (capture, encode, transport, ... already present)
├── transport.rs  # PRECEDENT — pure echo logic + Transport seam; aoa.rs is the live adapter
├── encode_vt.rs  # PRECEDENT — pure avcc_to_annex_b + cfg-gated VideoToolbox adapter placeholder
└── touch.rs      # NEW — DisplayRect, CgPoint, map_normalized_to_global, MapError,
                  #       TouchPhase re-use, PointerState, PointerAction, PointerStateMachine,
                  #       PointerSink (port), + inline #[cfg(test)] tests.
                  #       The CGEvent adapter (CgEventInjector) is added LATER behind
                  #       #[cfg(all(target_os="macos", feature="live-inject"))] (see encode_vt.rs precedent).
```

### Pattern 1: The pure mapping function (no Y-flip) — THE correctness core
**What:** Affine map from a normalized top-left point onto a CG-global top-left rect. Because **both** spaces are top-left origin (y down), it is a direct scale+offset with **no** `(1.0 - ny)` inversion.
**When to use:** Always — this is the spine of the phase.
**Example:**
```rust
// Source: design recommendation. Coordinate-space facts CITED to Apple docs (see Sources):
//   CGDisplayBounds / CGWarpMouseCursorPosition / CGEventCreateMouseEvent all use the
//   Quartz GLOBAL DISPLAY coordinate space: origin = TOP-LEFT of the MAIN display, y DOWN.
//   Android/normalized touch is ALSO top-left (ny=0 at top) => NO Y inversion.

/// A display rectangle in the Core Graphics GLOBAL display coordinate space
/// (origin = top-left of the main display; y increases downward). For the virtual
/// display this comes (at runtime, in the deferred adapter) from
/// `CGDisplayBounds(display_id)`, whose `.origin` already offsets it within that space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayRect { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }

/// A point in the same CG global display coordinate space; feeds CGPoint at the FFI edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CgPoint { pub x: f64, pub y: f64 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError { NotFinite }   // nx or ny was NaN/±inf — meaningless, reject.

/// Map a normalized top-left touch point onto `rect` in CG global coordinates.
/// Finite out-of-range inputs are CLAMPED to [0,1] (edge touches / sensor noise still
/// land on the edge); non-finite inputs are REJECTED (the protocol does not validate them).
pub fn map_normalized_to_global(nx: f32, ny: f32, rect: DisplayRect) -> Result<CgPoint, MapError> {
    if !nx.is_finite() || !ny.is_finite() {
        return Err(MapError::NotFinite);
    }
    let cx = (nx as f64).clamp(0.0, 1.0);   // f64::clamp stable since 1.50 — MSRV-safe
    let cy = (ny as f64).clamp(0.0, 1.0);
    Ok(CgPoint {
        x: rect.x + cx * rect.w,             // NO flip on X
        y: rect.y + cy * rect.h,             // NO flip on Y — both spaces are top-left
    })
}
```

### Pattern 2: Single-pointer touch→mouse state machine (D4)
**What:** A 3-state FSM over one active pointer that turns `phase` transitions into abstract `PointerAction`s with drag semantics.
**When to use:** Every decoded `Frame::Touch` for the active pointer.
**Example:**
```rust
// Source: design recommendation (D4 single-pointer). Pure; no I/O; returns the action.
use protocol::messages::{TouchEvent, TouchPhase};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerAction {
    Down { at: CgPoint },   // -> LeftMouseDown (adapter)
    Move { at: CgPoint },   // -> mouseMoved / leftMouseDragged (adapter)
    Up   { at: CgPoint },   // -> LeftMouseUp (adapter)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PointerState { #[default] Idle, Down, Dragging }

#[derive(Debug, Default)]
pub struct PointerStateMachine {
    state: PointerState,
    active: Option<u32>,   // the one pointer_id we track (D4); others ignored mid-gesture
}

impl PointerStateMachine {
    /// Fold one TouchEvent into at most one PointerAction. Returns the mapped action,
    /// or None if the event was a no-op (e.g. a stray Move with no active pointer, or a
    /// second pointer during an active gesture — D4: single pointer only).
    pub fn step(&mut self, ev: &TouchEvent, rect: DisplayRect) -> Result<Option<PointerAction>, MapError> {
        // Ignore a different pointer while one gesture owns the cursor (D4).
        if let Some(id) = self.active {
            if ev.pointer_id != id && ev.phase != TouchPhase::Down {
                return Ok(None);
            }
        }
        let at = map_normalized_to_global(ev.nx, ev.ny, rect)?;
        let action = match (self.state, ev.phase) {
            (PointerState::Idle, TouchPhase::Down) => {
                self.active = Some(ev.pointer_id);
                self.state = PointerState::Down;
                Some(PointerAction::Down { at })
            }
            // Defensive: a Down arriving mid-gesture from the SAME pointer restarts it.
            (PointerState::Down | PointerState::Dragging, TouchPhase::Move) => {
                self.state = PointerState::Dragging;
                Some(PointerAction::Move { at })
            }
            (PointerState::Down | PointerState::Dragging, TouchPhase::Up) => {
                self.state = PointerState::Idle;
                self.active = None;
                Some(PointerAction::Up { at })
            }
            // Stray Move/Up with no active pointer, or Down while already active from a
            // SECOND pointer: no-op (single-pointer policy). Down-while-Idle handled above.
            _ => None,
        };
        Ok(action)
    }
}
```
A tap is `Down` then `Up` (no `Move`) → `MouseDown` + `MouseUp` at the same point = a click. A drag is `Down`, `Move`*, `Up` → click-and-drag. This satisfies success criteria #1 (tap → click at location) and #2 (drag) at the *logic* level.

### Pattern 3: The injection **port** (mirrors `Transport`/`Encoder`/`Capturer`)
**What:** A trait the deferred `CGEvent` adapter implements; the pure layer never names a macOS type.
```rust
// Source: design recommendation; mirrors macos_host::transport::Transport seam (D0).
/// The injection port (D0 swap point). The pure FSM produces PointerActions; an adapter
/// renders them. The macOS CGEvent adapter (DEFERRED, hands-on-Mac, AXIsProcessTrusted-gated)
/// is the first impl — exactly as `aoa.rs` is the first impl of the Transport seam.
pub trait PointerSink {
    fn dispatch(&mut self, action: PointerAction);
}
```
A trivial `RecordingSink` (a `Vec<PointerAction>`) makes the *end-to-end* fold testable in CI without any macOS API — the same way `LoopbackTransport` tests `echo_roundtrip`.

### Anti-Patterns to Avoid
- **Introducing a Y-flip `(1.0 - ny)`.** This is the single most likely bug. The flip is *only* needed when converting between Core Graphics (top-left) and **AppKit/`NSScreen`** (bottom-left). This phase targets CG global coordinates directly and the input is already top-left, so a flip would put the cursor at the *bottom* when the user touches the *top*. **No flip.** (Pin this with explicit corner tests — see Code Examples.)
- **Re-deriving `CGDisplayBounds` inside the pure function.** The rect is a parameter; sourcing it (FFI) is the adapter's job. Keeps the math pure/testable and avoids baking the main-display offset into the wrong layer.
- **Assuming the virtual display's origin is `(0,0)`.** In a multi-display arrangement the virtual display is offset; `CGDisplayBounds(virtual_id).origin` is generally non-zero. The mapping **must** add `rect.x/rect.y` (it does). Hard-coding origin 0 would land touches on the *main* display.
- **Redefining `TouchEvent`/`TouchPhase`.** Consume `protocol::messages::{TouchEvent, TouchPhase}`. Duplicating them desyncs the wire type.
- **Asserting equality on NaN / using `Eq` on f32 coords.** `TouchEvent` is `PartialEq` not `Eq`; test mapping of NaN via the `MapError::NotFinite` branch, not via float equality.
- **Panicking on hostile input.** `nx,ny` are attacker-influencable (unvalidated on decode). Return `Result`; never `unwrap`/index-panic.
- **Multiplexing multiple pointers onto the cursor.** D4 is single-pointer; a second simultaneous finger must not fight the cursor. Ignore it (A2).
- **Using post-1.80 stdlib APIs.** clippy is MSRV-aware; e.g. don't reach for `f64::midpoint` (1.85) to compute a center in tests — write `(a+b)*0.5`.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| The touch wire type | A new `TouchEvent`/`TouchPhase` in `touch.rs` | `protocol::messages::{TouchEvent, TouchPhase}` | Already shipped + serde-round-trip-tested in P5; redefining desyncs host/client. |
| Length/clamp/NaN helpers | A custom `is_nan`/range util | `f32::is_finite` + `f64::clamp` (std, ≤1.50) | Stdlib, MSRV-safe, correct for ±inf and NaN. |
| The injection-adapter abstraction | A bespoke callback shape | A `PointerSink` trait mirroring the existing `Transport`/`Encoder`/`Capturer` seams | Project convention (D0 ports & adapters); reviewers already understand it; the macOS adapter drops in like `aoa.rs`. |
| Obtaining the display rect | Re-implementing display enumeration in the pure layer | (DEFERRED adapter) `CGDisplayBounds(display_id)` from `core-graphics` or a `.mm` shim; pure layer takes the rect as a param | Keeps the testable math free of FFI; the bounds API already returns the rect in exactly the right global space. |

**Key insight:** This slice is almost entirely *pure arithmetic + a tiny FSM* composed behind an existing project pattern. The only genuinely tricky thing is a **coordinate-space fact**, not code — and that fact (no Y-flip) is now pinned to Apple's docs. Keep the code small; spend the rigor on the corner/clamp/NaN/drag tests.

## The Y-origin decision, pinned (research questions 1–3)

> This is the load-bearing correctness section the slice exists to settle. All three Apple APIs the future adapter will use share **one** coordinate space.

**1. What space does `CGWarpMouseCursorPosition` / `CGEventCreateMouseEvent` expect?**
The **Quartz global display coordinate space**, origin at the **top-left of the main display**, y increasing **downward**. `CGWarpMouseCursorPosition` "changes the location of the mouse cursor… to a point in global display coordinates… relative to the upper-left corner of the display." `CGEventCreateMouseEvent`'s `mouseCursorPosition` is documented as "the position of the mouse cursor in **global coordinates**," consistent with the rest of the CGEvent/`CGEventGetLocation` family which speaks of "global display coordinates." `[CITED: developer.apple.com — CGWarpMouseCursorPosition; "Controlling the Mouse Cursor" (QuartzDisplayServices); CoreGraphics CGEvent.h header comment]`

**2. How is a display's global rect obtained, and what origin?**
`CGRect CGDisplayBounds(CGDirectDisplayID display)` — "Returns the bounds of a display in the **global display coordinate space**… relative to the **upper-left corner of the main display**." So `CGDisplayBounds(virtual_display_id).origin` is exactly the virtual display's offset within the *same* space the cursor APIs consume. The project already obtains the `CGDirectDisplayID` via `cg_virtual_display::VirtualDisplay::display_id()` (verified in-repo). `[CITED: developer.apple.com — CGDisplayBounds(_:)]`

**3. Multi-display subtlety the mapping must account for.**
The main display's bounds origin is `(0,0)`; **other** displays (including our virtual one) have **non-zero** origins (and the main display is the *reference*, so a display arranged to the left/above the main can even have **negative** x/y). Because the mapping adds `rect.x/rect.y` from `CGDisplayBounds`, it lands on the virtual display regardless of arrangement — including negative offsets. The one trap to avoid is the **CG↔AppKit flip**: `NSScreen.frame`/AppKit use a **bottom-left** origin (y up) with the *main* screen's bottom-left as origin, which is a *different* space; converting an `NSScreen` rect would require `flippedY = mainHeight - (y + h)`. **We never use AppKit here**, so we never apply that flip. Use `CGDisplayBounds` (CG, top-left) end-to-end and the input touch (top-left) maps directly. `[CITED: Apple docs as above; the CG-vs-AppKit flip is the documented community gotcha — NSHipster CGGeometry / multiple Apple-archive references]`

**Net rule for the planner:** `gy = rect.y + ny*rect.h` (no inversion). Pin it with tests that assert ny=0 → top edge (`rect.y`) and ny=1 → bottom edge (`rect.y + rect.h`).

## Code Examples

### Corner/center mapping — proves NO Y-flip (the regression guard)
```rust
// Source: design recommendation. THE test that pins the Y-origin decision.
#[test]
fn maps_corners_without_y_flip() {
    // Virtual display offset to the right of (and below) the main display.
    let rect = DisplayRect { x: 1512.0, y: 0.0, w: 2400.0, h: 1080.0 };
    // Top-left touch -> top-left of the rect (NOT bottom-left).
    assert_eq!(map_normalized_to_global(0.0, 0.0, rect).unwrap(),
               CgPoint { x: 1512.0, y: 0.0 });
    // Bottom-right touch -> bottom-right of the rect.
    assert_eq!(map_normalized_to_global(1.0, 1.0, rect).unwrap(),
               CgPoint { x: 3912.0, y: 1080.0 });
    // ny=0 must be the TOP (y == rect.y), proving no (1.0 - ny) inversion.
    assert_eq!(map_normalized_to_global(0.5, 0.0, rect).unwrap().y, 0.0);
    // Center -> rect center (use *0.5, not f64::midpoint which is >1.80).
    assert_eq!(map_normalized_to_global(0.5, 0.5, rect).unwrap(),
               CgPoint { x: 1512.0 + 1200.0, y: 540.0 });
}

#[test]
fn maps_onto_negatively_offset_display() {
    // Virtual display arranged to the LEFT of the main display => negative origin x.
    let rect = DisplayRect { x: -2400.0, y: 0.0, w: 2400.0, h: 1080.0 };
    assert_eq!(map_normalized_to_global(0.0, 0.0, rect).unwrap().x, -2400.0);
    assert_eq!(map_normalized_to_global(1.0, 0.0, rect).unwrap().x, 0.0);
}
```

### Clamp + NaN policy
```rust
// Source: design recommendation (clamp finite out-of-range; reject NaN/inf).
#[test]
fn clamps_finite_out_of_range() {
    let rect = DisplayRect { x: 0.0, y: 0.0, w: 2400.0, h: 1080.0 };
    assert_eq!(map_normalized_to_global(-0.2, 1.3, rect).unwrap(),
               CgPoint { x: 0.0, y: 1080.0 }); // clamped to [0,1] each axis
}
#[test]
fn rejects_nan_and_inf() {
    let rect = DisplayRect { x: 0.0, y: 0.0, w: 2400.0, h: 1080.0 };
    assert_eq!(map_normalized_to_global(f32::NAN, 0.5, rect), Err(MapError::NotFinite));
    assert_eq!(map_normalized_to_global(0.5, f32::INFINITY, rect), Err(MapError::NotFinite));
}
```

### Drag sequence (success criterion #2) and tap=click (criterion #1)
```rust
// Source: design recommendation. Down -> Move* -> Up == click-and-drag.
use protocol::messages::{TouchEvent, TouchPhase};
fn ev(id: u32, phase: TouchPhase, nx: f32, ny: f32) -> TouchEvent {
    TouchEvent { pointer_id: id, phase, nx, ny }
}
#[test]
fn down_move_up_is_a_drag() {
    let rect = DisplayRect { x: 0.0, y: 0.0, w: 2400.0, h: 1080.0 };
    let mut sm = PointerStateMachine::default();
    let a = sm.step(&ev(1, TouchPhase::Down, 0.0, 0.0), rect).unwrap();
    let b = sm.step(&ev(1, TouchPhase::Move, 0.5, 0.5), rect).unwrap();
    let c = sm.step(&ev(1, TouchPhase::Up,   1.0, 1.0), rect).unwrap();
    assert_eq!(a, Some(PointerAction::Down { at: CgPoint { x: 0.0,    y: 0.0   } }));
    assert_eq!(b, Some(PointerAction::Move { at: CgPoint { x: 1200.0, y: 540.0 } }));
    assert_eq!(c, Some(PointerAction::Up   { at: CgPoint { x: 2400.0, y: 1080.0} }));
}
#[test]
fn down_then_up_is_a_click_at_one_point() {
    let rect = DisplayRect { x: 0.0, y: 0.0, w: 2400.0, h: 1080.0 };
    let mut sm = PointerStateMachine::default();
    let down = sm.step(&ev(1, TouchPhase::Down, 0.25, 0.75), rect).unwrap();
    let up   = sm.step(&ev(1, TouchPhase::Up,   0.25, 0.75), rect).unwrap();
    assert!(matches!(down, Some(PointerAction::Down { .. })));
    assert!(matches!(up,   Some(PointerAction::Up   { .. })));
}
#[test]
fn second_pointer_is_ignored_mid_gesture() {
    let rect = DisplayRect { x: 0.0, y: 0.0, w: 2400.0, h: 1080.0 };
    let mut sm = PointerStateMachine::default();
    sm.step(&ev(1, TouchPhase::Down, 0.1, 0.1), rect).unwrap();
    // A different finger's Move must not steal the cursor (D4 single-pointer).
    assert_eq!(sm.step(&ev(2, TouchPhase::Move, 0.9, 0.9), rect).unwrap(), None);
}
#[test]
fn stray_move_without_down_is_noop() {
    let rect = DisplayRect { x: 0.0, y: 0.0, w: 2400.0, h: 1080.0 };
    let mut sm = PointerStateMachine::default();
    assert_eq!(sm.step(&ev(1, TouchPhase::Move, 0.5, 0.5), rect).unwrap(), None);
}
```

## State of the Art

| Old Approach | Current Approach | Impact |
|--------------|------------------|--------|
| Mixing AppKit `NSScreen` (bottom-left) coords into cursor math, then "fixing" with ad-hoc flips | Stay entirely in the **Core Graphics global display space** (top-left) end-to-end; only flip when an `NSScreen` value genuinely enters | Eliminates the classic "cursor lands at the wrong vertical position / jumps between displays" bug. This phase never imports AppKit, so the flip never arises. |
| `CGPostMouseEvent` / `CGPostKeyboardEvent` (deprecated) | `CGEventCreateMouseEvent` + `CGEventPost` (the documented modern path) — *deferred* adapter | The adapter (later wave) should use the non-deprecated event-creation API. Noted now so the deferred plan doesn't reach for the old one. |

**Deprecated/outdated:** `CGPostMouseEvent` (use `CGEventCreateMouseEvent`+`CGEventPost` in the deferred adapter). Nothing in *this* pure slice is affected.

## Runtime State Inventory

> P6's cable-free slice is **greenfield pure logic** (a new module + tests), not a rename/refactor/migration. This section is included only to discharge it explicitly.

| Category | Items Found | Action Required |
|----------|-------------|------------------|
| Stored data | None — no datastore touched; the FSM is in-memory per-session. | none (verified: pure module, no persistence) |
| Live service config | None — no external service configured by this slice. | none |
| OS-registered state | None for the pure slice. (DEFERRED adapter will require the host to be granted **Accessibility** trust via `AXIsProcessTrusted()`/System Settings — an OS-registered grant, but that is the deferred wave.) | none now; note the Accessibility grant for the deferred adapter |
| Secrets/env vars | None. | none |
| Build artifacts | None new beyond the normal `target/` rebuild from adding a module. | none |

**Net:** Nothing to migrate for this slice — verified by the slice being a new pure module with no I/O or persistence.

## Common Pitfalls

### Pitfall 1: Applying a Y-flip that doesn't belong
**What goes wrong:** Developer "knows macOS is bottom-left" and writes `gy = rect.y + (1.0 - ny)*rect.h`. The cursor then appears at the bottom when the user touches the top.
**Why it happens:** Conflating CG global space (top-left) with AppKit/`NSScreen` (bottom-left). Both exist on macOS; only AppKit is flipped.
**How to avoid:** Target CG global coords exclusively; never import AppKit here. Lock it with the `maps_corners_without_y_flip` test (ny=0 must yield `rect.y`).
**Warning signs:** Any `1.0 - ny` / `mainHeight - y` in the pure layer; the corner test failing.

### Pitfall 2: Assuming the virtual display origin is (0,0)
**What goes wrong:** Cursor moves on the **main** display instead of the virtual one in a multi-monitor arrangement.
**Why it happens:** Forgetting `CGDisplayBounds.origin` is non-zero (and possibly negative) for non-main displays.
**How to avoid:** Always `rect.x + ...` / `rect.y + ...`; the rect is `CGDisplayBounds(virtual_id)`, not a size. Test with a non-zero (and a negative) offset rect.
**Warning signs:** Tests only ever use `x=0,y=0`; cursor "works" only when the virtual display happens to be the main one.

### Pitfall 3: Panicking / mis-handling unvalidated `nx,ny`
**What goes wrong:** NaN propagates into a `CGPoint`, or an `unwrap` panics on hostile input, taking down the host.
**Why it happens:** `protocol::TouchEvent` deliberately does **not** validate on decode (documented at `messages.rs` ~115–131).
**How to avoid:** `is_finite` reject for NaN/±inf; `clamp` for finite out-of-range; return `Result`, never `unwrap`. Test NaN and ±inf explicitly.
**Warning signs:** `nx as f64` used without a finite check; `.unwrap()` on the mapping in non-test code.

### Pitfall 4: Multi-pointer cursor fighting (violating D4)
**What goes wrong:** Two fingers each drive `CGWarp`, the cursor teleports between them.
**Why it happens:** Treating every `pointer_id` as the cursor.
**How to avoid:** Track one active pointer; ignore others until the active gesture ends (Pattern 2). Test the second-pointer no-op.
**Warning signs:** No `active: Option<u32>` in the FSM; tests never send two ids.

### Pitfall 5: MSRV regressions from newer stdlib float APIs
**What goes wrong:** CI clippy (MSRV-aware, 1.80) fails on `f64::midpoint` / `is_multiple_of` / `next_up`.
**Why it happens:** Reaching for convenient newer helpers in mapping or tests.
**How to avoid:** Use `clamp` (1.50), `is_finite`/`is_nan` (long-stable), arithmetic `(a+b)*0.5`. (`f32::clamp`/`f64::clamp` are fine.)
**Warning signs:** clippy `this method was stabilized after the MSRV`.

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | Policy = **clamp** finite out-of-range `nx,ny` to [0,1], **reject** only NaN/±inf | Pattern 1 / User Constraints | Low — both clamp and reject are defensible; clamp gives better edge-touch UX. Localized one-line change + test update. Confirm in discuss-phase. |
| A2 | A second `pointer_id` during an active gesture is **ignored** (not queued/swapped) | Pattern 2 | Low — D4 is single-pointer; "ignore" is the simplest correct policy. "Latest finger wins" is the alternative. Confirm. |
| A3 | Module lives in `crates/macos-host/src/touch.rs` (not `protocol::coords`) | Arch Responsibility Map | Low — both compile + test in CI; placement affects only crate boundaries/import paths. Recommended on co-location grounds. Confirm. |
| A4 | `DisplayRect`/`CgPoint` use `f64` (CGFloat on arm64), not integer pixels | Standard Stack | Low — `f64` matches the eventual `CGPoint` FFI and avoids premature rounding. Integer is viable but pushes rounding into the pure layer. |
| A5 | FSM `step()` **returns** `Option<PointerAction>` (sink driven externally) vs pushing into `PointerSink` | Pattern 2/3 | Low — return is more testable; the `PointerSink` port still exists for the adapter. Either is fine. |
| A6 | A tap (Down→Up, no Move) should emit Down then Up at the same point (= a click); no separate "Click" action | Code Examples | Low — matches how `CGEvent` clicks are formed (down+up). If a dedicated click action is wanted, add later. |
| A7 | The deferred adapter will use `CGEventCreateMouseEvent`+`CGEventPost` and/or `CGWarpMouseCursorPosition`, sourcing the rect from `CGDisplayBounds(display_id)` | State of the Art / Deferred | n/a to this slice (deferred); flagged so the next wave doesn't pick deprecated `CGPostMouseEvent`. |

## Open Questions

1. **Move vs Warp for the deferred adapter.** Should a `Move` render as a posted `leftMouseDragged`/`mouseMoved` event (so apps see motion) or as `CGWarpMouseCursorPosition` (teleport, no event)? — Recommendation: post `leftMouseDragged` while a button is down (real drag), `mouseMoved` otherwise; `CGWarp` only if a no-event reposition is ever needed. **Deferred** (adapter wave); does not affect the pure `PointerAction` shape.
2. **Does a bare tap need an explicit `Move` to the location first?** Some apps want the cursor at the point before the down. — Recommendation: the adapter posts the down *at* `at` (the event carries the position), so no pre-move is needed. **Deferred.**
3. **Coalescing rapid `Move`s.** High-rate touch could flood events. — Recommendation: leave the pure FSM 1:1 (one action per event); coalescing/rate-limiting is an adapter/transport concern. **Deferred / YAGNI** until measured.

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| Rust toolchain | building/testing `macos-host` | ✓ | rustc 1.95.0 (MSRV pinned 1.80) | — |
| `protocol` crate (`TouchEvent`) | the mapping INPUT type | ✓ (in-tree, shipped P5) | path dep | none needed |
| `cg-virtual-display` (`display_id()`) | (DEFERRED) sourcing the rect | ✓ (in-tree) | path dep | — |
| macOS + Accessibility permission | (DEFERRED) real `CGEvent` injection + `AXIsProcessTrusted` | ✗ for this slice | — | **none — out of scope for the cable-free slice** |
| Pixel 6a + live transport | (DEFERRED) `AInputEvent` capture → `Frame::Touch` | ✗ for this slice | — | **none — out of scope** |
| `core-graphics` crate / `.mm` shim | (DEFERRED) adapter FFI | not added | — | hand-rolled `extern "C"` shim (project precedent) |
| slopcheck | package legitimacy | n/a | — | **no packages added by this slice** |

**Missing with no fallback (blocking the deferred half only):** macOS + Accessibility grant, Pixel 6a, live transport, the `CGEvent`/`CGDisplayBounds` FFI. **None of these block the in-scope coordinate-mapping + FSM logic**, which is pure `std` Rust and runs on any CI runner.

## Validation Architecture

> `.planning/config.json` is absent → `nyquist_validation` treated as enabled.

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[test]` / `cargo test` (no external test dep) |
| Config file | none — workspace `Cargo.toml` + per-crate; inline `#[cfg(test)] mod tests` (matches `transport.rs`/`encode_vt.rs`) |
| Quick run command | `cargo test -p macos-host touch` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements → Test Map (in-scope slice)
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| TOUCH-01 | normalized→global mapping, NO Y-flip (corners/center) | unit | `cargo test -p macos-host -- touch::tests::maps_corners` | ❌ Wave 0 (`touch.rs`) |
| TOUCH-01 | negative/offset display rect mapping | unit | `cargo test -p macos-host -- touch::tests::maps_onto_neg` | ❌ Wave 0 |
| TOUCH-01 | clamp finite out-of-range; reject NaN/inf | unit | `cargo test -p macos-host -- touch::tests::clamps` `..rejects_nan` | ❌ Wave 0 |
| TOUCH-01 | Down→Move*→Up drag (criterion #2) | unit | `cargo test -p macos-host -- touch::tests::down_move_up` | ❌ Wave 0 |
| TOUCH-01 | tap = click at one point (criterion #1) | unit | `cargo test -p macos-host -- touch::tests::down_then_up` | ❌ Wave 0 |
| TOUCH-01 | single-pointer policy (2nd pointer ignored), stray Move no-op | unit | `cargo test -p macos-host -- touch::tests::second_pointer` | ❌ Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p macos-host touch` (+ `cargo clippy -p macos-host -- -D warnings`, `cargo fmt --check`)
- **Per wave merge:** `cargo test --workspace`
- **Phase gate (this slice):** `cargo test -p macos-host` green; `cargo clippy --workspace -- -D warnings` clean; `cargo build --workspace` green.

### Wave 0 Gaps
- [ ] `crates/macos-host/src/touch.rs` — new module: `DisplayRect`, `CgPoint`, `MapError`, `map_normalized_to_global`, `PointerAction`, `PointerState(Machine)`, `PointerSink`, + inline tests (covers the TOUCH-01 coordinate/FSM slice).
- [ ] `crates/macos-host/src/lib.rs` — add `pub mod touch;`.
- [ ] Framework install: none — `cargo test` is built in.
- *(No `Cargo.toml` change; no new dependency.)*

## Security Domain

> `security_enforcement` config absent → treated as enabled. The pure slice processes attacker-influencable `nx,ny` (unvalidated on decode) but performs no I/O, no FFI, no allocation-by-attacker-length.

### Applicable ASVS Categories
| ASVS Category | Applies | Standard Control |
|---------------|---------|------------------|
| V2 Authentication | no | Single physical USB peer; no auth in MVP (note for P7 if NCM/TCP transport chosen). |
| V3 Session Management | no | The pointer FSM is per-connection in-memory; no security session. |
| V4 Access Control | **yes (deferred adapter)** | The real injection is gated by macOS **Accessibility** trust (`AXIsProcessTrusted()`); the OS enforces it. In scope only as a noted seam. |
| V5 Input Validation | **yes** | `nx,ny` validated at the mapping boundary: NaN/±inf rejected, finite values clamped to [0,1]; no panic on hostile input; `pointer_id`/`phase` are typed enums/u32 (no parsing). |
| V6 Cryptography | no | No crypto on a local USB link in the MVP. |

### Known Threat Patterns for the touch-mapping slice
| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| NaN/±inf `nx,ny` → garbage `CGPoint` / cursor to undefined location | Tampering | `is_finite` reject → `MapError::NotFinite` (tested). |
| Out-of-range `nx,ny` → cursor escapes the virtual display onto another | Tampering | `clamp` to [0,1] before scaling (tested). |
| Hostile input → panic (DoS of the host) | Denial of Service | `Result`-returning mapping; no `unwrap`/unchecked index; FSM total over all `(state, phase)` pairs. |
| Multi-pointer flood / cursor hijack | Tampering / DoS | Single-pointer policy (D4) — second pointer ignored mid-gesture (tested). |
| Unauthorized cursor control once wired | Elevation of Privilege | (Deferred adapter) macOS Accessibility gate via `AXIsProcessTrusted()`; the OS denies injection without explicit user grant. |

## Sources

### Primary (HIGH confidence)
- In-repo (read in full): `crates/protocol/src/messages.rs` (the `TouchEvent`/`TouchPhase` INPUT type + the "not range-validated on decode" contract, lines ~104–131); `crates/macos-host/src/transport.rs` (the `Transport` seam + pure-logic/adapter precedent); `crates/macos-host/src/encode_vt.rs` (pure-fn + cfg-gated-adapter precedent); `crates/macos-host/src/lib.rs` (module/seam conventions); `crates/macos-host/Cargo.toml` (MSRV 1.80, `live-usb` feature pattern); `crates/cg-virtual-display/src/{lib.rs,shim.mm}` (`display_id()` / `CGGetActiveDisplayList`; the `extern "C"` shim pattern for macOS FFI).
- `developer.apple.com` — **CGDisplayBounds(_:)**: "bounds of a display in the global display coordinate space… relative to the **upper-left corner of the main display**." (origin convention — research Q2/Q3).
- `developer.apple.com` — **CGWarpMouseCursorPosition(_:)** + "Controlling the Mouse Cursor" (Quartz Display Services): point is in **global display coordinates**, **upper-left** origin (research Q1).
- CoreGraphics **`CGEvent.h`** header comment (Apple SDK): `mouseCursorPosition` is "the position of the mouse cursor in **global coordinates**" (research Q1) — consistent with the `CGEventGetLocation`/"global display coordinates" family.
- `.planning/ROADMAP.md` (P6 goal/criteria, "Single-pointer mouse emulation only (D4)"), `.planning/REQUIREMENTS.md` (TOUCH-01), `.planning/STATE.md` (D0/D4, MSRV, cable-free slicing precedent).
- `.planning/phases/P5-.../{RESEARCH.md,CONTEXT.md}` — the exact cable-free-slice RESEARCH.md structure/depth + deferred-portion documentation convention mirrored here.

### Secondary (MEDIUM confidence)
- NSHipster "CGGeometry" + Apple-archive Quartz docs — corroborate the **CG (top-left) vs AppKit/`NSScreen` (bottom-left)** flip distinction (research Q3, the gotcha to avoid).
- Local toolchain: `rustc 1.95.0`; MSRV pinned 1.80 (clippy-MSRV behavior).

### Tertiary (LOW confidence)
- `core-graphics` crate version (`0.24.0`) for the **deferred** adapter — registry name plausible but **not** slopcheck-verified; the project may hand-roll a `.mm` shim instead (precedent: `cg-virtual-display`). Flagged `[ASSUMED]`, decided in the deferred wave.

## Metadata

**Confidence breakdown:**
- Y-origin / coordinate-space decision (no Y-flip): **HIGH** — three independent Apple sources agree that `CGDisplayBounds`, `CGWarpMouseCursorPosition`, and `CGEventCreateMouseEvent` share the top-left global display space; input touch is also top-left.
- Architecture (pure module + injection port mirroring `Transport`/`Encoder`): **HIGH** — directly follows shipped in-repo precedent (D0).
- Mapping/FSM code + tests: **HIGH** — pure arithmetic + a small total FSM; every branch has a named test.
- Clamp/reject + single-pointer policy specifics: **MEDIUM** — sound defaults but `[ASSUMED]` (A1/A2); confirm in discuss-phase.
- Deferred adapter stack (`core-graphics` vs `.mm` shim, Move-vs-Warp): **LOW/MEDIUM** — out of scope for this slice, flagged for the hands-on-Mac wave.

**Research date:** 2026-06-03
**Valid until:** 2026-07-03 (stable — the Core Graphics coordinate-space contract is long-standing; the in-repo foundation is fixed). The deferred adapter's API choices (`core-graphics` version, deprecation status of CG event APIs) should be re-verified when that wave starts.
