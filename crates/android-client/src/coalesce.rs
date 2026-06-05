//! Pure touch MOVE-event coalescing (P6 back-channel). High-frequency `MotionEvent.MOVE`
//! samples (the phone reports them far faster than the video cadence) would flood the USB
//! link and pile up in the host's injection queue — the same queue-residence failure mode
//! that hurt video latency. This module batches MOVEs down to the caller's send tick while
//! keeping gesture transitions instant and correctly ordered.
//!
//! # Contract
//! - **MOVE — keep-latest, coalesced.** [`MoveCoalescer::on_event`] buffers a MOVE and
//!   returns nothing; a later MOVE *replaces* the buffered one (only the most recent cursor
//!   position matters — older intermediate positions are dropped). The buffered MOVE is
//!   emitted by [`MoveCoalescer::flush`], which the caller drives on its send tick
//!   (~60–90 Hz, matching the video cadence).
//! - **DOWN / UP — immediate, NEVER coalesced.** `on_event` returns a DOWN or UP right away
//!   (tap responsiveness depends on the DOWN landing promptly; an UP must release the host
//!   pointer without delay).
//! - **Flush-before-transition ordering.** When a DOWN or UP arrives while a MOVE is
//!   buffered, `on_event` returns `[buffered_move, transition]` — the pending move is
//!   flushed FIRST so the host never sees a press/release out of order with the motion that
//!   preceded it.
//! - **Single-cursor model.** There is ONE pending-move slot, not one per `pointer_id`. The
//!   host drives a single mouse cursor and its single-pointer FSM ([`crate::touch`]) already
//!   ignores any secondary pointer, so a later MOVE replaces the pending one regardless of
//!   which finger produced it. Keeping that policy here would duplicate it; this module stays
//!   pointer-agnostic and lets the host FSM own the single-pointer decision.
//!
//! Pure: no time, no threading, no I/O. The caller owns the clock and drives `flush` on its
//! tick — so this is host-testable in CI, the same discipline as [`crate::touch`]. The
//! JNI/Kotlin capture shim and the live send of the emitted events over the transport are
//! device-side glue, deferred to the hardware phase.

use protocol::messages::{TouchEvent, TouchPhase};

/// Coalesces touch MOVE events down to the caller's send tick while passing DOWN/UP through
/// immediately and in order. See the [module docs](self) for the full contract.
#[derive(Debug, Default)]
pub struct MoveCoalescer {
    /// The most recent buffered MOVE awaiting the next [`MoveCoalescer::flush`], if any.
    pending: Option<TouchEvent>,
}

impl MoveCoalescer {
    /// Create an empty coalescer with no pending move.
    pub fn new() -> Self {
        MoveCoalescer { pending: None }
    }

    /// Feed one touch event. Returns the events to send *now*, in order:
    /// - MOVE: buffered (replacing any prior pending move); returns `[]`.
    /// - DOWN/UP: returns the pending move first (if any) then this event, never buffering it.
    pub fn on_event(&mut self, ev: TouchEvent) -> Vec<TouchEvent> {
        match ev.phase {
            TouchPhase::Move => {
                self.pending = Some(ev);
                vec![]
            }
            TouchPhase::Down | TouchPhase::Up => match self.pending.take() {
                Some(pending) => vec![pending, ev],
                None => vec![ev],
            },
        }
    }

    /// Emit the buffered MOVE (the latest cursor position) and clear it, or `None` if no MOVE
    /// is pending. Call this on each send tick.
    pub fn flush(&mut self) -> Option<TouchEvent> {
        self.pending.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(phase: TouchPhase, nx: f32) -> TouchEvent {
        TouchEvent {
            pointer_id: 0,
            phase,
            nx,
            ny: 0.0,
        }
    }

    #[test]
    fn single_move_is_buffered_then_flushed() {
        let mut c = MoveCoalescer::new();
        assert_eq!(c.on_event(ev(TouchPhase::Move, 0.5)), vec![]);
        assert_eq!(c.flush(), Some(ev(TouchPhase::Move, 0.5)));
    }

    #[test]
    fn two_moves_keep_only_latest() {
        let mut c = MoveCoalescer::new();
        assert_eq!(c.on_event(ev(TouchPhase::Move, 0.1)), vec![]);
        assert_eq!(c.on_event(ev(TouchPhase::Move, 0.9)), vec![]);
        // Older move (0.1) dropped; only the latest (0.9) survives.
        assert_eq!(c.flush(), Some(ev(TouchPhase::Move, 0.9)));
    }

    #[test]
    fn down_is_returned_immediately_and_never_buffered() {
        let mut c = MoveCoalescer::new();
        assert_eq!(
            c.on_event(ev(TouchPhase::Down, 0.3)),
            vec![ev(TouchPhase::Down, 0.3)]
        );
        // The Down was emitted, not buffered, so there is nothing left to flush.
        assert_eq!(c.flush(), None);
    }

    #[test]
    fn pending_move_flushed_before_a_following_down() {
        let mut c = MoveCoalescer::new();
        assert_eq!(c.on_event(ev(TouchPhase::Move, 0.4)), vec![]);
        // The buffered move must come out FIRST, then the Down — order preserved.
        assert_eq!(
            c.on_event(ev(TouchPhase::Down, 0.4)),
            vec![ev(TouchPhase::Move, 0.4), ev(TouchPhase::Down, 0.4)]
        );
        // Buffered move was emitted alongside the Down, so nothing remains.
        assert_eq!(c.flush(), None);
    }

    #[test]
    fn pending_move_flushed_before_a_following_up() {
        let mut c = MoveCoalescer::new();
        assert_eq!(c.on_event(ev(TouchPhase::Move, 0.7)), vec![]);
        // Same ordering guarantee as Down: pending move first, then the Up.
        assert_eq!(
            c.on_event(ev(TouchPhase::Up, 0.7)),
            vec![ev(TouchPhase::Move, 0.7), ev(TouchPhase::Up, 0.7)]
        );
        assert_eq!(c.flush(), None);
    }

    #[test]
    fn flush_is_none_when_empty_and_clears_after_emitting() {
        let mut c = MoveCoalescer::new();
        // No pending move yet.
        assert_eq!(c.flush(), None);
        // Buffer one, flush it, then a second flush must be empty (cleared).
        let _ = c.on_event(ev(TouchPhase::Move, 0.2));
        assert_eq!(c.flush(), Some(ev(TouchPhase::Move, 0.2)));
        assert_eq!(c.flush(), None);
    }

    #[test]
    fn moves_coalesce_across_pointer_ids_single_cursor_model() {
        // Single-cursor model (matches the host's single-pointer FSM, which ignores any
        // secondary pointer): there is ONE pending-move slot, NOT one per pointer_id. A
        // later Move replaces the prior pending Move regardless of which finger produced it.
        let mut c = MoveCoalescer::new();
        let p1 = TouchEvent {
            pointer_id: 1,
            phase: TouchPhase::Move,
            nx: 0.1,
            ny: 0.0,
        };
        let p2 = TouchEvent {
            pointer_id: 2,
            phase: TouchPhase::Move,
            nx: 0.8,
            ny: 0.0,
        };
        assert_eq!(c.on_event(p1), vec![]);
        assert_eq!(c.on_event(p2.clone()), vec![]);
        // Only the latest move (pointer 2) survives — pointer 1's is dropped.
        assert_eq!(c.flush(), Some(p2));
    }
}
