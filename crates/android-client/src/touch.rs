//! The Android half of P6's touch back-channel: convert a raw Android `MotionEvent`
//! sample into a normalized [`protocol::messages::TouchEvent`] for the macOS host's
//! coordinate-mapping → single-pointer-FSM → `CGEvent`-injection layer
//! ([`macos_host::touch`]) to consume.
//!
//! This is the exact INVERSE of the host's `map_normalized_to_global`: the host turns
//! normalized `(nx,ny) ∈ [0,1]` back into global Core Graphics pixels, so this side must
//! produce normalized coords in the SAME convention — **top-left origin, y-down**, which
//! is what Android `MotionEvent.getX()/getY()` already use. Because both ends agree on
//! top-left/y-down there is NO axis flip anywhere in the touch path (the host doc calls
//! this out as its load-bearing correctness fact).
//!
//! Pure & host-testable: this module names NO JNI/NDK type and is not `cfg`-gated, so CI
//! exercises it on any runner (the same discipline as `transport::echo_loop`). The Kotlin
//! `onTouchEvent` → JNI shim that feeds it raw samples, and the live send of the produced
//! `Frame::Touch` over the transport, are the device-side glue — deferred until the live
//! session (P5) exists, exactly as the host's real `CgEventSink` injection sits behind a
//! seam awaiting the live pipeline.
//!
//! [`macos_host::touch`]: ../../macos_host/touch/index.html

use protocol::messages::{TouchEvent, TouchPhase};

/// Android `MotionEvent` MASKED action codes (`android.view.MotionEvent.ACTION_*`). The
/// Kotlin shim passes `event.actionMasked` (already stripped of the pointer-index bits),
/// so these are compared directly.
pub mod action {
    /// `ACTION_DOWN` — first pointer touches down.
    pub const DOWN: i32 = 0;
    /// `ACTION_UP` — last pointer lifts.
    pub const UP: i32 = 1;
    /// `ACTION_MOVE` — a pointer moved.
    pub const MOVE: i32 = 2;
    /// `ACTION_CANCEL` — gesture aborted (treated as an Up so the host releases).
    pub const CANCEL: i32 = 3;
    /// `ACTION_POINTER_DOWN` — a secondary pointer touched down.
    pub const POINTER_DOWN: i32 = 5;
    /// `ACTION_POINTER_UP` — a secondary pointer lifted.
    pub const POINTER_UP: i32 = 6;
}

/// Map a masked `MotionEvent` action to a [`TouchPhase`], or `None` for actions that are
/// not a single-pointer gesture transition (hover, scroll, button events, …) — the caller
/// simply skips those samples.
///
/// `CANCEL` maps to [`TouchPhase::Up`] so an aborted gesture still releases the host
/// pointer (never leaves a button stuck down). `POINTER_DOWN`/`POINTER_UP` (secondary
/// fingers) are mapped too; the host's single-pointer FSM (D4) is what actually ignores a
/// second simultaneous pointer, keeping that policy in ONE place.
pub fn phase_for_action(masked_action: i32) -> Option<TouchPhase> {
    match masked_action {
        action::DOWN | action::POINTER_DOWN => Some(TouchPhase::Down),
        action::MOVE => Some(TouchPhase::Move),
        action::UP | action::POINTER_UP | action::CANCEL => Some(TouchPhase::Up),
        _ => None,
    }
}

/// Normalize a pixel coordinate within a view of size `extent` to `[0,1]` (top-left
/// origin). Out-of-range inputs are CLAMPED (an edge/slop touch still lands on the edge);
/// a non-positive or non-finite `extent`, or a non-finite `coord`, collapses to `0.0` so
/// the result is ALWAYS a finite value in `[0,1]` (never `NaN`/±inf, never a div-by-zero).
pub fn normalize(coord: f32, extent: f32) -> f32 {
    if !extent.is_finite() || extent <= 0.0 || !coord.is_finite() {
        return 0.0;
    }
    (coord / extent).clamp(0.0, 1.0)
}

/// Build a normalized [`TouchEvent`] from one raw Android touch sample, or `None` when the
/// action has no single-pointer mapping (see [`phase_for_action`]).
///
/// `x`/`y` are the pointer's pixel position from `MotionEvent.getX()/getY()`; `view_w`/
/// `view_h` are the touched `SurfaceView`'s pixel size. The resulting `nx,ny` are always
/// finite and clamped to `[0,1]` ([`normalize`]). `pointer_id` is the stable
/// `getPointerId(index)` so the host FSM can track one finger across a gesture.
pub fn to_touch_event(
    masked_action: i32,
    pointer_id: u32,
    x: f32,
    y: f32,
    view_w: f32,
    view_h: f32,
) -> Option<TouchEvent> {
    let phase = phase_for_action(masked_action)?;
    Some(TouchEvent {
        pointer_id,
        phase,
        nx: normalize(x, view_w),
        ny: normalize(y, view_h),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use exactly-representable values so f32 asserts are exact (TouchEvent is PartialEq,
    // not Eq — NaN would break equality, which is exactly why `normalize` never emits it).

    #[test]
    fn normalize_center_and_corners() {
        assert_eq!(normalize(0.0, 1080.0), 0.0); // top/left edge
        assert_eq!(normalize(1080.0, 1080.0), 1.0); // bottom/right edge
        assert_eq!(normalize(540.0, 1080.0), 0.5); // center
        assert_eq!(normalize(600.0, 2400.0), 0.25);
    }

    #[test]
    fn normalize_clamps_out_of_range() {
        // Negative (touch slop past the edge) and beyond-extent both clamp into [0,1].
        assert_eq!(normalize(-50.0, 1080.0), 0.0);
        assert_eq!(normalize(5000.0, 2400.0), 1.0);
    }

    #[test]
    fn normalize_degenerate_extent_is_zero() {
        // Zero / negative / non-finite extent must not divide-by-zero or yield NaN.
        assert_eq!(normalize(100.0, 0.0), 0.0);
        assert_eq!(normalize(100.0, -10.0), 0.0);
        assert_eq!(normalize(100.0, f32::NAN), 0.0);
        assert_eq!(normalize(100.0, f32::INFINITY), 0.0);
    }

    #[test]
    fn normalize_non_finite_coord_is_zero() {
        assert_eq!(normalize(f32::NAN, 1080.0), 0.0);
        assert_eq!(normalize(f32::INFINITY, 1080.0), 0.0);
        assert_eq!(normalize(f32::NEG_INFINITY, 1080.0), 0.0);
    }

    #[test]
    fn phase_mapping_covers_all_single_pointer_actions() {
        assert_eq!(phase_for_action(action::DOWN), Some(TouchPhase::Down));
        assert_eq!(
            phase_for_action(action::POINTER_DOWN),
            Some(TouchPhase::Down)
        );
        assert_eq!(phase_for_action(action::MOVE), Some(TouchPhase::Move));
        assert_eq!(phase_for_action(action::UP), Some(TouchPhase::Up));
        assert_eq!(phase_for_action(action::POINTER_UP), Some(TouchPhase::Up));
        // CANCEL releases (never a stuck button).
        assert_eq!(phase_for_action(action::CANCEL), Some(TouchPhase::Up));
    }

    #[test]
    fn phase_mapping_ignores_unhandled_actions() {
        // e.g. ACTION_HOVER_MOVE (7), ACTION_SCROLL (8), or any future/unknown code.
        assert_eq!(phase_for_action(7), None);
        assert_eq!(phase_for_action(8), None);
        assert_eq!(phase_for_action(-1), None);
    }

    #[test]
    fn to_touch_event_normalizes_a_down_sample() {
        // A DOWN at (600,810) in a 2400×1080 view → normalized (0.25, 0.75).
        let ev = to_touch_event(action::DOWN, 1, 600.0, 810.0, 2400.0, 1080.0)
            .expect("DOWN maps to a TouchEvent");
        assert_eq!(
            ev,
            TouchEvent {
                pointer_id: 1,
                phase: TouchPhase::Down,
                nx: 0.25,
                ny: 0.75,
            }
        );
    }

    #[test]
    fn to_touch_event_returns_none_for_unhandled_action() {
        assert!(to_touch_event(7, 0, 10.0, 10.0, 100.0, 100.0).is_none());
    }

    #[test]
    fn to_touch_event_clamps_and_stays_finite_on_hostile_input() {
        // Out-of-range + degenerate height: nx clamps to 1.0, ny collapses to 0.0 — the
        // host's map_normalized_to_global will then accept it (finite, in range) rather
        // than reject it as NotFinite.
        let ev = to_touch_event(action::MOVE, 2, 9999.0, f32::NAN, 1000.0, 0.0)
            .expect("MOVE maps to a TouchEvent");
        assert_eq!(ev.nx, 1.0);
        assert_eq!(ev.ny, 0.0);
        assert_eq!(ev.phase, TouchPhase::Move);
        assert_eq!(ev.pointer_id, 2);
    }
}
