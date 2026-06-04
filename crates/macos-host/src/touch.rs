//! The pure, cable-free, CI-green touch-mapping + single-pointer pointer-FSM core for
//! P6's touch back-channel.
//!
//! THE load-bearing correctness fact: Core Graphics' GLOBAL display coordinate space
//! and normalized Android touch are **both** top-left origin (y increases downward),
//! so mapping a normalized touch point onto a display rectangle is a straight affine
//! scale + offset with **NO Y-flip** — `gx = rect.x + nx*rect.w`, `gy = rect.y +
//! ny*rect.h`. A `ny`-inversion belongs *only* to AppKit/`NSScreen` (bottom-left),
//! which this phase never touches; introducing it here would put the cursor at the
//! bottom when the user touches the top. This module deliberately contains no such
//! inversion (and a grep gate enforces that).
//!
//! Layering: this module CONSUMES [`protocol::messages::TouchEvent`] /
//! [`protocol::messages::TouchPhase`] (the shipped P5 wire INPUT type) and never
//! redefines them. It owns input sanitation because `protocol::TouchEvent` is
//! deliberately NOT range-validated on decode (documented at `messages.rs` ~115–131):
//! finite out-of-range `nx,ny` are clamped to `[0,1]`, NaN/±inf are rejected with
//! [`MapError::NotFinite`]; nothing here ever panics on hostile input.
//!
//! The pure code names NO macOS / Core Graphics FFI type and is **not**
//! `cfg`-gated, so it builds and tests on any CI runner. The macOS injection adapter —
//! a real `CGEvent`/`CGEventCreateMouseEvent`/`CGEventPost` implementation of the
//! [`PointerSink`] port, gated on `AXIsProcessTrusted()` — lives in this same file behind
//! `#[cfg(all(target_os = "macos", feature = "live-inject"))]` (see the `cg_inject`
//! module / `CgEventSink` below), exactly as [`crate::encode_vt`] co-locates its
//! cfg-gated VideoToolbox adapter with the pure `avcc_to_annex_b` logic. The display
//! rectangle is sourced at runtime by that adapter from `CGDisplayBounds(display_id)`
//! and passed in as a [`DisplayRect`].

/// A display rectangle in the Core Graphics GLOBAL display coordinate space (origin =
/// top-left of the main display; y increases downward). It is sourced at runtime from
/// `CGDisplayBounds(display_id)`, whose `.origin` already offsets it within that same
/// space the cursor APIs consume (possibly NEGATIVELY when the display is arranged left
/// of / above the main display). The `p6_inject` bin fills it from
/// `CGDisplay::main().bounds()` today; the live pipeline will pass the VIRTUAL display's
/// id (deferred). This pure layer takes it as a parameter and never assumes origin
/// `(0, 0)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayRect {
    /// Left edge in CG global coordinates (may be negative).
    pub x: f64,
    /// Top edge in CG global coordinates (may be negative).
    pub y: f64,
    /// Width in pixels.
    pub w: f64,
    /// Height in pixels.
    pub h: f64,
}

/// A point in the same CG global display coordinate space; feeds `CGPoint` at the
/// DEFERRED FFI edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CgPoint {
    /// X in CG global coordinates.
    pub x: f64,
    /// Y in CG global coordinates.
    pub y: f64,
}

/// Why a normalized touch point could not be mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// `nx` or `ny` was `NaN`/±inf — meaningless, rejected before any arithmetic.
    NotFinite,
}

/// Map a normalized top-left touch point onto `rect` in CG global coordinates.
///
/// Finite out-of-range inputs are CLAMPED to `[0,1]` (an edge touch / sensor noise
/// still lands on the edge); non-finite inputs are REJECTED with
/// [`MapError::NotFinite`] (the protocol does not validate them). There is NO Y-flip:
/// `ny == 0.0` maps to `rect.y` (the top edge), `ny == 1.0` to `rect.y + rect.h` (the
/// bottom edge). Never panics on any input.
pub fn map_normalized_to_global(nx: f32, ny: f32, rect: DisplayRect) -> Result<CgPoint, MapError> {
    if !nx.is_finite() || !ny.is_finite() {
        return Err(MapError::NotFinite);
    }
    // f64::clamp is stable since 1.50 — MSRV-1.80 safe.
    let cx = (nx as f64).clamp(0.0, 1.0);
    let cy = (ny as f64).clamp(0.0, 1.0);
    Ok(CgPoint {
        x: rect.x + cx * rect.w, // NO flip on X
        y: rect.y + cy * rect.h, // NO flip on Y — both spaces are top-left
    })
}

/// One abstract pointer action carrying a mapped [`CgPoint`]. The DEFERRED macOS
/// adapter renders each as a real mouse event (`Down` -> `LeftMouseDown`, `Move` ->
/// `mouseMoved`/`leftMouseDragged`, `Up` -> `LeftMouseUp`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerAction {
    /// Press at `at`.
    Down {
        /// The mapped global coordinate.
        at: CgPoint,
    },
    /// Move/drag to `at`.
    Move {
        /// The mapped global coordinate.
        at: CgPoint,
    },
    /// Release at `at`.
    Up {
        /// The mapped global coordinate.
        at: CgPoint,
    },
}

/// The single-pointer gesture state (D4). Private — callers observe behavior via the
/// [`PointerAction`]s [`PointerStateMachine::step`] emits, not the raw state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PointerState {
    /// No gesture in progress.
    #[default]
    Idle,
    /// A pointer is down but has not yet moved.
    Down,
    /// A pointer is down and has moved (dragging).
    Dragging,
}

/// A single-pointer touch→mouse state machine (D4). Folds a [`TouchEvent`] sequence
/// for ONE active pointer into [`PointerAction`]s; a second `pointer_id` arriving
/// mid-gesture is ignored (single-pointer policy). Construct with `Default`.
///
/// [`TouchEvent`]: protocol::messages::TouchEvent
#[derive(Debug, Default)]
pub struct PointerStateMachine {
    state: PointerState,
    /// The one `pointer_id` this machine tracks while a gesture is active (D4).
    active: Option<u32>,
}

impl PointerStateMachine {
    /// Fold one `TouchEvent` into at most one [`PointerAction`].
    ///
    /// Returns `Ok(Some(action))` for a recognized transition, `Ok(None)` for a no-op
    /// (a stray `Move`/`Up` with no active pointer, or a SECOND pointer during an active
    /// gesture — D4), or `Err(MapError)` if the event's `nx,ny` are non-finite (state is
    /// NOT advanced on a rejected event). The `(state, phase)` match is TOTAL over every
    /// pair — hostile / out-of-order input is a no-op, never a panic.
    pub fn step(
        &mut self,
        ev: &protocol::messages::TouchEvent,
        rect: DisplayRect,
    ) -> Result<Option<PointerAction>, MapError> {
        use protocol::messages::TouchPhase;

        // While one gesture owns the cursor, ignore a DIFFERENT pointer outright (D4) —
        // any phase, without mapping. This guard (not the match below) is what handles a
        // foreign pointer_id; the `(Down|Dragging, Down)` arm below fires only for the
        // ACTIVE id sending a redundant Down.
        if let Some(id) = self.active {
            if ev.pointer_id != id {
                return Ok(None);
            }
        }

        // Map first; `?` propagates MapError and leaves state unchanged.
        let at = map_normalized_to_global(ev.nx, ev.ny, rect)?;

        let action = match (self.state, ev.phase) {
            (PointerState::Idle, TouchPhase::Down) => {
                self.active = Some(ev.pointer_id);
                self.state = PointerState::Down;
                Some(PointerAction::Down { at })
            }
            (PointerState::Down | PointerState::Dragging, TouchPhase::Move) => {
                self.state = PointerState::Dragging;
                Some(PointerAction::Move { at })
            }
            (PointerState::Down | PointerState::Dragging, TouchPhase::Up) => {
                self.state = PointerState::Idle;
                self.active = None;
                Some(PointerAction::Up { at })
            }
            // Stray Move/Up with no active pointer (Idle), or the active pointer sending a
            // redundant Down mid-gesture: no-op (single-pointer policy).
            (PointerState::Idle, TouchPhase::Move)
            | (PointerState::Idle, TouchPhase::Up)
            | (PointerState::Down, TouchPhase::Down)
            | (PointerState::Dragging, TouchPhase::Down) => None,
        };
        Ok(action)
    }
}

/// The injection PORT (D0 swap point). The pure [`PointerStateMachine`] produces
/// [`PointerAction`]s; an adapter renders them. The macOS `CGEvent` injector
/// (DEFERRED, hands-on-Mac, `AXIsProcessTrusted()`-gated) is the first impl — exactly
/// as `crate::aoa` is the first impl of the `transport::Transport` seam. A test-only
/// `RecordingSink` drives the end-to-end fold in CI with no macOS API.
pub trait PointerSink {
    /// Render one pointer action (the deferred macOS impl posts a real `CGEvent`).
    fn dispatch(&mut self, action: PointerAction);
}

// ───────────────────────────────────────────────────────────────────────────────────
// Live macOS cursor-injection adapter (P6 Wave B) — the first concrete `PointerSink`.
// Compiled ONLY under `--features live-inject` on macOS, exactly as `aoa`/`p1_echo` sit
// behind `live-usb`. It turns each pure `PointerAction` into a real Quartz `CGEvent`
// posted to the HID stream, so the system cursor moves/clicks/drags. This is the D0 swap
// point's hardware half: the pure `PointerStateMachine` decides WHAT action to take
// (CI-tested above); this adapter decides HOW to effect it on macOS (verified by hand via
// the `p6_inject` bin — never in CI, which has no display/accessibility grant).
// ───────────────────────────────────────────────────────────────────────────────────

#[cfg(all(target_os = "macos", feature = "live-inject"))]
pub use cg_inject::{accessibility_trusted, CgEventSink, InjectError};

#[cfg(all(target_os = "macos", feature = "live-inject"))]
mod cg_inject {
    use super::{CgPoint, PointerAction, PointerSink};
    use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint as CgEventPoint;

    // AXIsProcessTrusted lives in the ApplicationServices umbrella framework. A tiny direct
    // FFI binding (D0 "simplest-now, replaceable-later") avoids pulling an extra crate for
    // one nullary call; a later purity pass can swap in `accessibility-sys`. The C return
    // type is `Boolean` (a `u8`); we treat any non-zero as trusted.
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }

    /// Whether this process holds macOS Accessibility permission. Without it the OS
    /// silently DROPS injected events, so the adapter refuses to construct (fail-fast with
    /// an actionable message at the call site) rather than no-op invisibly.
    pub fn accessibility_trusted() -> bool {
        // SAFETY: nullary C function, no arguments, plain integer return — always safe.
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// Why the injector could not be created.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum InjectError {
        /// Accessibility permission not granted — see System Settings ▸ Privacy &
        /// Security ▸ Accessibility. (TOUCH-01 onboarding gate.)
        NotTrusted,
        /// Quartz refused to create a `CGEventSource` (should not normally happen).
        EventSource,
    }

    /// A [`PointerSink`] that posts real Quartz mouse events. Tracks button state so a
    /// `Move` while pressed becomes a DRAG (`LeftMouseDragged`) and a `Move` while up
    /// becomes a plain `MouseMoved` — matching the FSM's tap/drag semantics.
    ///
    /// **Threading (WR-01):** call [`dispatch`](PointerSink::dispatch) from the main
    /// thread (or a thread running the main run loop). Quartz may silently DROP or
    /// reorder events posted from an arbitrary background thread — the same silent-drop
    /// failure mode the [`accessibility_trusted`] gate exists to prevent. `p6_inject`
    /// satisfies this by dispatching on `main`; the future live pipeline must marshal
    /// touch frames onto the main thread before dispatching.
    pub struct CgEventSink {
        source: CGEventSource,
        pressed: bool,
    }

    impl CgEventSink {
        /// Create the injector, failing fast if Accessibility permission is absent
        /// ([`InjectError::NotTrusted`]) — the gate TOUCH-01 requires.
        pub fn new() -> Result<Self, InjectError> {
            if !accessibility_trusted() {
                return Err(InjectError::NotTrusted);
            }
            let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
                .map_err(|()| InjectError::EventSource)?;
            Ok(Self {
                source,
                pressed: false,
            })
        }

        fn emit(&self, kind: CGEventType, at: CgPoint) {
            let point = CgEventPoint::new(at.x, at.y);
            // `new_mouse_event` consumes the source; clone the retained CF handle per event.
            match CGEvent::new_mouse_event(self.source.clone(), kind, point, CGMouseButton::Left) {
                Ok(ev) => ev.post(CGEventTapLocation::HID),
                Err(()) => log::warn!("CgEventSink: Quartz refused to create a mouse event"),
            }
        }
    }

    impl PointerSink for CgEventSink {
        fn dispatch(&mut self, action: PointerAction) {
            match action {
                PointerAction::Down { at } => {
                    self.pressed = true;
                    self.emit(CGEventType::LeftMouseDown, at);
                }
                PointerAction::Move { at } => {
                    let kind = if self.pressed {
                        CGEventType::LeftMouseDragged
                    } else {
                        CGEventType::MouseMoved
                    };
                    self.emit(kind, at);
                }
                PointerAction::Up { at } => {
                    self.emit(CGEventType::LeftMouseUp, at);
                    self.pressed = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use exactly-representable f64 values so `assert_eq!` on `CgPoint` is exact.

    #[test]
    fn map_corners_no_y_flip() {
        // Virtual display offset to the right of the main display.
        let rect = DisplayRect {
            x: 1512.0,
            y: 0.0,
            w: 2400.0,
            h: 1080.0,
        };
        // Top-left touch -> top-left of the rect (NOT bottom-left).
        assert_eq!(
            map_normalized_to_global(0.0, 0.0, rect).unwrap(),
            CgPoint { x: 1512.0, y: 0.0 }
        );
        // Bottom-right touch -> bottom-right of the rect.
        assert_eq!(
            map_normalized_to_global(1.0, 1.0, rect).unwrap(),
            CgPoint {
                x: 3912.0,
                y: 1080.0
            }
        );
        // ny=0 MUST be the TOP edge (y == rect.y) — proves there is no ny-inversion.
        assert_eq!(map_normalized_to_global(0.5, 0.0, rect).unwrap().y, 0.0);
    }

    #[test]
    fn map_center() {
        let rect = DisplayRect {
            x: 1512.0,
            y: 0.0,
            w: 2400.0,
            h: 1080.0,
        };
        // Center computed with *0.5 (never f64::midpoint — post-1.80).
        assert_eq!(
            map_normalized_to_global(0.5, 0.5, rect).unwrap(),
            CgPoint {
                x: 1512.0 + 1200.0,
                y: 540.0
            }
        );
    }

    #[test]
    fn map_negative_origin_rect() {
        // Virtual display arranged to the LEFT of the main display => negative origin x.
        let rect = DisplayRect {
            x: -2400.0,
            y: 0.0,
            w: 2400.0,
            h: 1080.0,
        };
        assert_eq!(map_normalized_to_global(0.0, 0.0, rect).unwrap().x, -2400.0);
        assert_eq!(map_normalized_to_global(1.0, 0.0, rect).unwrap().x, 0.0);
    }

    #[test]
    fn map_non_unit_scale_rect() {
        // w != h proves independent per-axis scaling.
        let rect = DisplayRect {
            x: 0.0,
            y: 0.0,
            w: 2400.0,
            h: 1080.0,
        };
        assert_eq!(
            map_normalized_to_global(0.25, 0.75, rect).unwrap(),
            CgPoint { x: 600.0, y: 810.0 }
        );
    }

    #[test]
    fn map_clamps_finite_out_of_range() {
        let rect = DisplayRect {
            x: 0.0,
            y: 0.0,
            w: 2400.0,
            h: 1080.0,
        };
        // Each axis clamped into [0,1] BEFORE scaling: -0.2 -> 0.0, 1.3 -> 1.0.
        assert_eq!(
            map_normalized_to_global(-0.2, 1.3, rect).unwrap(),
            CgPoint { x: 0.0, y: 1080.0 }
        );
    }

    #[test]
    fn map_rejects_nan() {
        let rect = DisplayRect {
            x: 0.0,
            y: 0.0,
            w: 2400.0,
            h: 1080.0,
        };
        assert_eq!(
            map_normalized_to_global(f32::NAN, 0.5, rect),
            Err(MapError::NotFinite)
        );
        assert_eq!(
            map_normalized_to_global(0.5, f32::NAN, rect),
            Err(MapError::NotFinite)
        );
    }

    #[test]
    fn map_rejects_inf() {
        let rect = DisplayRect {
            x: 0.0,
            y: 0.0,
            w: 2400.0,
            h: 1080.0,
        };
        assert_eq!(
            map_normalized_to_global(f32::INFINITY, 0.5, rect),
            Err(MapError::NotFinite)
        );
        assert_eq!(
            map_normalized_to_global(0.5, f32::NEG_INFINITY, rect),
            Err(MapError::NotFinite)
        );
    }
}

#[cfg(test)]
mod fsm_tests {
    use super::*;
    use protocol::messages::{TouchEvent, TouchPhase};

    const RECT: DisplayRect = DisplayRect {
        x: 0.0,
        y: 0.0,
        w: 2400.0,
        h: 1080.0,
    };

    /// Terse TouchEvent constructor for the FSM cases.
    fn ev(id: u32, phase: TouchPhase, nx: f32, ny: f32) -> TouchEvent {
        TouchEvent {
            pointer_id: id,
            phase,
            nx,
            ny,
        }
    }

    #[test]
    fn fsm_tap_is_down_then_up() {
        // Down then Up at the same point == a click at one location (criterion #1).
        let mut sm = PointerStateMachine::default();
        let at = CgPoint { x: 600.0, y: 810.0 };
        let down = sm.step(&ev(1, TouchPhase::Down, 0.25, 0.75), RECT).unwrap();
        let up = sm.step(&ev(1, TouchPhase::Up, 0.25, 0.75), RECT).unwrap();
        assert_eq!(down, Some(PointerAction::Down { at }));
        assert_eq!(up, Some(PointerAction::Up { at }));
    }

    #[test]
    fn fsm_drag_emits_down_moves_up() {
        // Down, Move*, Up == click-and-drag (criterion #2).
        let mut sm = PointerStateMachine::default();
        let a = sm.step(&ev(1, TouchPhase::Down, 0.0, 0.0), RECT).unwrap();
        let b = sm.step(&ev(1, TouchPhase::Move, 0.5, 0.5), RECT).unwrap();
        let c = sm.step(&ev(1, TouchPhase::Move, 1.0, 1.0), RECT).unwrap();
        let d = sm.step(&ev(1, TouchPhase::Up, 1.0, 1.0), RECT).unwrap();
        assert_eq!(
            a,
            Some(PointerAction::Down {
                at: CgPoint { x: 0.0, y: 0.0 }
            })
        );
        assert_eq!(
            b,
            Some(PointerAction::Move {
                at: CgPoint {
                    x: 1200.0,
                    y: 540.0
                }
            })
        );
        assert_eq!(
            c,
            Some(PointerAction::Move {
                at: CgPoint {
                    x: 2400.0,
                    y: 1080.0
                }
            })
        );
        assert_eq!(
            d,
            Some(PointerAction::Up {
                at: CgPoint {
                    x: 2400.0,
                    y: 1080.0
                }
            })
        );
    }

    #[test]
    fn fsm_second_pointer_ignored_mid_gesture() {
        // A different finger's Move must NOT steal the cursor (D4 single-pointer).
        let mut sm = PointerStateMachine::default();
        sm.step(&ev(1, TouchPhase::Down, 0.1, 0.1), RECT).unwrap();
        assert_eq!(
            sm.step(&ev(2, TouchPhase::Move, 0.9, 0.9), RECT).unwrap(),
            None
        );
    }

    #[test]
    fn fsm_second_pointer_down_ignored_mid_gesture() {
        // A second pointer's Down mid-gesture is ignored AND the active pointer stays
        // id=1 — a subsequent Move(id=1) still emits.
        let mut sm = PointerStateMachine::default();
        sm.step(&ev(1, TouchPhase::Down, 0.1, 0.1), RECT).unwrap();
        assert_eq!(
            sm.step(&ev(2, TouchPhase::Down, 0.9, 0.9), RECT).unwrap(),
            None
        );
        // id=1 is still the active pointer.
        assert_eq!(
            sm.step(&ev(1, TouchPhase::Move, 0.5, 0.5), RECT).unwrap(),
            Some(PointerAction::Move {
                at: CgPoint {
                    x: 1200.0,
                    y: 540.0
                }
            })
        );
    }

    #[test]
    fn fsm_stray_move_without_down_is_noop() {
        let mut sm = PointerStateMachine::default();
        assert_eq!(
            sm.step(&ev(1, TouchPhase::Move, 0.5, 0.5), RECT).unwrap(),
            None
        );
    }

    #[test]
    fn fsm_up_without_down_is_noop() {
        let mut sm = PointerStateMachine::default();
        assert_eq!(
            sm.step(&ev(1, TouchPhase::Up, 0.5, 0.5), RECT).unwrap(),
            None
        );
    }

    #[test]
    fn fsm_propagates_map_error() {
        // A Down whose nx is NaN surfaces MapError (does not panic, does not swallow);
        // state is not advanced on a rejected event.
        let mut sm = PointerStateMachine::default();
        assert_eq!(
            sm.step(&ev(1, TouchPhase::Down, f32::NAN, 0.5), RECT),
            Err(MapError::NotFinite)
        );
        // State unchanged: a following valid Down still opens the gesture.
        assert_eq!(
            sm.step(&ev(1, TouchPhase::Down, 0.0, 0.0), RECT).unwrap(),
            Some(PointerAction::Down {
                at: CgPoint { x: 0.0, y: 0.0 }
            })
        );
    }

    /// Test-only injection sink that records every dispatched action — mirrors
    /// `transport::LoopbackTransport`, proving the end-to-end fold in CI with no macOS API.
    #[derive(Default)]
    struct RecordingSink {
        actions: Vec<PointerAction>,
    }

    impl PointerSink for RecordingSink {
        fn dispatch(&mut self, action: PointerAction) {
            self.actions.push(action);
        }
    }

    #[test]
    fn fsm_recording_sink_drives_end_to_end() {
        let mut sm = PointerStateMachine::default();
        let mut sink = RecordingSink::default();
        let seq = [
            ev(1, TouchPhase::Down, 0.0, 0.0),
            ev(1, TouchPhase::Move, 0.5, 0.5),
            ev(1, TouchPhase::Up, 1.0, 1.0),
        ];
        for e in &seq {
            if let Some(action) = sm.step(e, RECT).unwrap() {
                sink.dispatch(action);
            }
        }
        assert_eq!(
            sink.actions,
            vec![
                PointerAction::Down {
                    at: CgPoint { x: 0.0, y: 0.0 }
                },
                PointerAction::Move {
                    at: CgPoint {
                        x: 1200.0,
                        y: 540.0
                    }
                },
                PointerAction::Up {
                    at: CgPoint {
                        x: 2400.0,
                        y: 1080.0
                    }
                },
            ]
        );
    }
}
