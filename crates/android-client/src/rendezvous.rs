//! Handoff of the render surface from the SurfaceView callback to the USB decode-session
//! thread — and, for the life of the session, an ongoing channel for surface lock/unlock.
//!
//! The two things a live decode session needs — the render **surface** and the USB
//! **transport fd** — arrive on independent Android callbacks in an order that is not fixed:
//! when the app is launched by plugging in (the `USB_ACCESSORY_ATTACHED` intent), the USB fd
//! can arrive *before* the `SurfaceView`'s surface is created; when the app is already open,
//! the surface exists first. The session thread owns the fd (it is the JNI argument), so it
//! only needs to *receive* the surface — hence a one-way slot rather than a symmetric join.
//!
//! Beyond the initial rendezvous ([`take_blocking`](WindowSlot::take_blocking)), the same slot
//! carries **mid-session surface changes**: when the phone is locked the `SurfaceView`'s surface
//! is destroyed ([`mark_gone`](WindowSlot::mark_gone)) and on unlock a *new* one is created
//! ([`put`](WindowSlot::put)). The running decode loop polls [`take_pending`](WindowSlot::take_pending)
//! each frame to swap the new surface into the live decoder (`AMediaCodec_setOutputSurface`) and
//! [`is_gone`](WindowSlot::is_gone) to skip rendering into a dead window during the locked gap —
//! so lock/unlock resumes on the warm decoder instead of going black until app restart.
//!
//! [`WindowSlot`] is generic over the window type so it is host-testable with a stand-in;
//! the Android JNI layer instantiates it as `WindowSlot<NativeWindow>`. No FFI here.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Mutable state behind the slot's lock: the (optional) deposited window plus a `gone` flag
/// recording that the surface was destroyed and no replacement has arrived yet.
struct SlotState<T> {
    window: Option<T>,
    gone: bool,
}

/// A single-slot, cross-thread handoff for the render window — **reusable across a session**.
///
/// The surface callback [`put`](Self::put)s the window; the session thread first
/// [`take_blocking`](Self::take_blocking)s it for the initial rendezvous, then polls
/// [`take_pending`](Self::take_pending) / [`is_gone`](Self::is_gone) each frame so a surface
/// destroyed+recreated mid-session (phone lock/unlock) can be swapped into the running decoder
/// instead of leaving it rendering to a dead window.
pub struct WindowSlot<T> {
    state: Mutex<SlotState<T>>,
    ready: Condvar,
}

impl<T> WindowSlot<T> {
    /// An empty slot. `const` so it can back a `static`.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(SlotState {
                window: None,
                gone: false,
            }),
            ready: Condvar::new(),
        }
    }

    /// Deposit the window (from `surfaceCreated`) and wake a waiting session thread. Clears the
    /// `gone` flag (a live surface exists again). Replaces any previously-deposited-but-unconsumed
    /// window (e.g. a surface re-create before the session claimed the old one) — freshest wins.
    pub fn put(&self, window: T) {
        let mut guard = self.state.lock().unwrap();
        guard.window = Some(window);
        guard.gone = false;
        self.ready.notify_one();
    }

    /// Record that the surface was destroyed (`surfaceDestroyed`): drop any deposited-but-unconsumed
    /// window so a later take cannot hand out a dead surface, set `gone` so the running decode loop
    /// skips rendering until a new surface arrives, and wake any blocked taker so it re-evaluates
    /// against its deadline instead of waiting out a full timeout.
    pub fn mark_gone(&self) {
        let mut guard = self.state.lock().unwrap();
        guard.window = None;
        guard.gone = true;
        self.ready.notify_one();
    }

    /// Non-blocking: take a freshly-deposited window if one is waiting, else `None`. Used by the
    /// running decode loop each frame to pick up a surface re-created after a lock/unlock and swap
    /// it into the live decoder. Does not block and does not touch `gone` (cleared by `put`).
    pub fn take_pending(&self) -> Option<T> {
        self.state.lock().unwrap().window.take()
    }

    /// Whether the surface is currently destroyed with no replacement yet (locked-gap). The decode
    /// loop reads this to release output buffers without rendering into a dead window.
    pub fn is_gone(&self) -> bool {
        self.state.lock().unwrap().gone
    }

    /// Block until a window is available or `timeout` elapses, then take it. Returns `None`
    /// on timeout with no window (e.g. the app never showed its `SurfaceView`). Takes the
    /// window out, so a second caller does not receive a stale handoff.
    pub fn take_blocking(&self, timeout: Duration) -> Option<T> {
        // Track an absolute deadline so spurious or `mark_gone` wakeups don't re-arm the full
        // timeout — `timeout` is an upper bound on the total wait, not a per-wakeup budget.
        let deadline = Instant::now() + timeout;
        let mut guard = self.state.lock().unwrap();
        loop {
            if let Some(window) = guard.window.take() {
                return Some(window);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                // Deadline reached (possibly via several short wakeups): give up.
                return guard.window.take();
            };
            let (next, res) = self.ready.wait_timeout(guard, remaining).unwrap();
            guard = next;
            if res.timed_out() {
                // One last look in case a `put` raced the timeout, then give up.
                return guard.window.take();
            }
        }
    }
}

impl<T> Default for WindowSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn put_then_take_returns_window() {
        // Surface-first: the window is already deposited when the session thread arrives.
        let slot: WindowSlot<&str> = WindowSlot::new();
        slot.put("surface");
        assert_eq!(slot.take_blocking(Duration::from_secs(1)), Some("surface"));
    }

    #[test]
    fn take_consumes_so_second_take_times_out() {
        let slot: WindowSlot<&str> = WindowSlot::new();
        slot.put("surface");
        assert_eq!(slot.take_blocking(Duration::from_secs(1)), Some("surface"));
        // Already consumed → a second taker gets nothing (no stale re-handoff).
        assert_eq!(slot.take_blocking(Duration::from_millis(20)), None);
    }

    #[test]
    fn take_blocks_until_put_arrives_later() {
        // Launch-by-plug: the USB session thread waits; the surface is deposited afterwards.
        let slot: Arc<WindowSlot<i32>> = Arc::new(WindowSlot::new());
        let producer = {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                slot.put(42);
            })
        };
        assert_eq!(slot.take_blocking(Duration::from_secs(2)), Some(42));
        producer.join().unwrap();
    }

    #[test]
    fn take_times_out_when_no_window() {
        let slot: WindowSlot<i32> = WindowSlot::new();
        assert_eq!(slot.take_blocking(Duration::from_millis(20)), None);
    }

    #[test]
    fn mark_gone_retracts_window_so_take_does_not_hand_out_a_stale_one() {
        // Surface deposited, then destroyed before the USB thread claimed it: `mark_gone` must
        // retract the (now-dead) window so the next taker does not configure the decoder
        // onto a destroyed surface — it sees an empty slot and times out instead.
        let slot: WindowSlot<&str> = WindowSlot::new();
        slot.put("stale-surface");
        slot.mark_gone();
        assert_eq!(slot.take_blocking(Duration::from_millis(20)), None);
    }

    #[test]
    fn mark_gone_on_empty_slot_is_a_noop_for_takers() {
        let slot: WindowSlot<i32> = WindowSlot::new();
        slot.mark_gone();
        assert_eq!(slot.take_blocking(Duration::from_millis(10)), None);
    }

    #[test]
    fn put_clears_gone_and_take_pending_picks_up_the_new_surface() {
        // Lock/unlock mid-session: the surface is destroyed (gone), then a new one is created.
        // The running decode loop polls take_pending + is_gone — it must see the new surface and
        // no longer be `gone`.
        let slot: WindowSlot<&str> = WindowSlot::new();
        // Initial rendezvous consumes the first surface (as the session start does).
        slot.put("surface-1");
        assert_eq!(slot.take_blocking(Duration::from_millis(20)), Some("surface-1"));

        // Lock: surface destroyed.
        slot.mark_gone();
        assert!(slot.is_gone(), "gone while the screen is locked");
        assert_eq!(slot.take_pending(), None, "no surface available during the gap");

        // Unlock: a new surface is created.
        slot.put("surface-2");
        assert!(!slot.is_gone(), "put clears gone — a live surface exists again");
        assert_eq!(
            slot.take_pending(),
            Some("surface-2"),
            "the running loop picks up the recreated surface for setOutputSurface"
        );
        // Consumed once, so a second poll yields nothing (no stale re-swap).
        assert_eq!(slot.take_pending(), None);
    }

    #[test]
    fn take_pending_is_none_when_nothing_deposited() {
        let slot: WindowSlot<i32> = WindowSlot::new();
        assert_eq!(slot.take_pending(), None);
        assert!(!slot.is_gone(), "a fresh slot is not 'gone'");
    }

    #[test]
    fn take_blocking_respects_its_deadline_under_a_wakeup_storm() {
        // A taker waiting with a 100ms timeout must give up at ~100ms even while it is being
        // woken repeatedly without a window arriving. If the timeout is re-armed on each
        // wakeup (the bug), a steady stream of wakeups keeps it blocked far past 100ms.
        use std::time::Instant;
        let slot: Arc<WindowSlot<i32>> = Arc::new(WindowSlot::new());
        let storm = {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || {
                // ~500ms of wakeups, one every 10ms — never deposits a window.
                for _ in 0..50 {
                    slot.mark_gone();
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
        };
        let start = Instant::now();
        let result = slot.take_blocking(Duration::from_millis(100));
        let elapsed = start.elapsed();
        storm.join().unwrap();

        assert_eq!(result, None);
        assert!(
            elapsed < Duration::from_millis(400),
            "take_blocking ignored its deadline under a wakeup storm (waited {elapsed:?})"
        );
    }
}
