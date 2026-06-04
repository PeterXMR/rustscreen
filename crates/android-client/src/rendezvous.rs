//! One-shot handoff of the render surface from the SurfaceView callback to the USB
//! decode-session thread.
//!
//! The two things a live decode session needs — the render **surface** and the USB
//! **transport fd** — arrive on independent Android callbacks in an order that is not fixed:
//! when the app is launched by plugging in (the `USB_ACCESSORY_ATTACHED` intent), the USB fd
//! can arrive *before* the `SurfaceView`'s surface is created; when the app is already open,
//! the surface exists first. The session thread owns the fd (it is the JNI argument), so it
//! only needs to *receive* the surface — hence a one-way slot rather than a symmetric join.
//!
//! [`WindowSlot`] is generic over the window type so it is host-testable with a stand-in;
//! the Android JNI layer instantiates it as `WindowSlot<NativeWindow>`. No FFI here.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// A single-slot, cross-thread handoff for the render window.
///
/// The surface callback [`put`](Self::put)s the window; the session thread
/// [`take_blocking`](Self::take_blocking)s it, waiting if the surface has not arrived yet.
pub struct WindowSlot<T> {
    slot: Mutex<Option<T>>,
    ready: Condvar,
}

impl<T> WindowSlot<T> {
    /// An empty slot. `const` so it can back a `static`.
    pub const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    /// Deposit the window (from the surface callback) and wake a waiting session thread.
    /// Replaces any previously-deposited-but-unconsumed window (e.g. a surface re-create
    /// before a session claimed the old one) — the freshest surface wins.
    pub fn put(&self, window: T) {
        let mut guard = self.slot.lock().unwrap();
        *guard = Some(window);
        self.ready.notify_one();
    }

    /// Retract any deposited-but-unconsumed window (e.g. the surface was destroyed before
    /// the session thread claimed it). Drops the stored window so a later `take_blocking`
    /// cannot hand out a dead surface, and wakes any blocked taker so it re-evaluates
    /// against its deadline instead of waiting out a full timeout. A no-op when empty.
    pub fn clear(&self) {
        let mut guard = self.slot.lock().unwrap();
        *guard = None;
        self.ready.notify_one();
    }

    /// Block until a window is available or `timeout` elapses, then take it. Returns `None`
    /// on timeout with no window (e.g. the app never showed its `SurfaceView`). Takes the
    /// window out, so a second caller does not receive a stale handoff.
    pub fn take_blocking(&self, timeout: Duration) -> Option<T> {
        // Track an absolute deadline so spurious or `clear` wakeups don't re-arm the full
        // timeout — `timeout` is an upper bound on the total wait, not a per-wakeup budget.
        let deadline = Instant::now() + timeout;
        let mut guard = self.slot.lock().unwrap();
        loop {
            if let Some(window) = guard.take() {
                return Some(window);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                // Deadline reached (possibly via several short wakeups): give up.
                return guard.take();
            };
            let (next, res) = self.ready.wait_timeout(guard, remaining).unwrap();
            guard = next;
            if res.timed_out() {
                // One last look in case a `put` raced the timeout, then give up.
                return guard.take();
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
    fn clear_retracts_window_so_take_does_not_hand_out_a_stale_one() {
        // Surface deposited, then destroyed before the USB thread claimed it: `clear` must
        // retract the (now-dead) window so the next taker does not configure the decoder
        // onto a destroyed surface — it sees an empty slot and times out instead.
        let slot: WindowSlot<&str> = WindowSlot::new();
        slot.put("stale-surface");
        slot.clear();
        assert_eq!(slot.take_blocking(Duration::from_millis(20)), None);
    }

    #[test]
    fn clear_on_empty_slot_is_a_noop() {
        let slot: WindowSlot<i32> = WindowSlot::new();
        slot.clear();
        assert_eq!(slot.take_blocking(Duration::from_millis(10)), None);
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
                    slot.clear();
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
