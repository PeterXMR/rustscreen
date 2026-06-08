//! One-way handoff of the render surface from the SurfaceView callback to the USB
//! decode-session thread, re-deposited once per decode session (the slot is consume-once,
//! so each new session re-`put`s the current surface — see `WindowSlot::put`).
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

use std::sync::atomic::{AtomicBool, Ordering};
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

/// A pending render-surface change for a *running* decode session, polled once per receive-loop
/// iteration so the session can re-point the decoder onto a recreated surface (app foregrounded)
/// or stop rendering onto a destroyed one (app backgrounded) WITHOUT tearing the session down.
///
/// This is the mid-session counterpart to [`WindowSlot`]: `WindowSlot` is the consume-once
/// rendezvous that hands a *fresh* session its initial surface; [`SurfaceMailbox`] is a live
/// channel the *same* session keeps reading for the rest of its life. A `SurfaceView`'s surface
/// is destroyed and recreated on every background/foreground transition, so a decoder bound once
/// at startup would otherwise keep rendering into the destroyed surface's abandoned `BufferQueue`
/// (error spam → input-queue stall → a costly full reconnect). See `mediacodec::SURFACE_MAILBOX`.
pub enum SurfaceChange<T> {
    /// A new render surface arrived (foreground / surface re-create): swap the decoder onto it.
    Swap(T),
    /// The render surface was destroyed (backgrounded): stop rendering until one returns.
    Lost,
    /// Nothing changed since the last poll.
    None,
}

/// Single-producer (UI/JNI thread) / single-consumer (decode-session thread) mailbox for the
/// live surface lifecycle. Generic over the window type so it is host-testable with a stand-in;
/// the Android layer instantiates it as `SurfaceMailbox<NativeWindow>`. No FFI here.
pub struct SurfaceMailbox<T> {
    /// Cheap hot-path gate the decode loop checks every iteration. `Relaxed` per the project's
    /// hot-path flag convention — it is a standalone "something changed" hint with no companion
    /// memory of its own; the `Mutex` below publishes the actual `inner` data with proper
    /// acquire/release, so a momentarily-stale `dirty` only ever costs one extra (no-op) poll and
    /// never strands a change (the next `deposit`/`mark_lost` re-raises it).
    dirty: AtomicBool,
    inner: Mutex<MailboxInner<T>>,
}

struct MailboxInner<T> {
    /// Newest deposited surface not yet taken by the session loop.
    pending: Option<T>,
    /// The surface was destroyed with no replacement yet — render must pause.
    lost: bool,
}

impl<T> SurfaceMailbox<T> {
    /// An empty mailbox. `const` so it can back a `static`.
    pub const fn new() -> Self {
        Self {
            dirty: AtomicBool::new(false),
            inner: Mutex::new(MailboxInner {
                pending: None,
                lost: false,
            }),
        }
    }

    /// `surfaceCreated`: a new render surface is available. Supersedes any prior un-taken surface
    /// (freshest wins) and clears `lost` — the new surface replaces the destroyed one.
    pub fn deposit(&self, window: T) {
        {
            let mut g = self.inner.lock().unwrap();
            g.pending = Some(window);
            g.lost = false;
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// `surfaceDestroyed`: the current render surface is gone. Drops any not-yet-taken pending
    /// surface too (it is the very surface being destroyed) and records the lost edge so the
    /// next poll pauses rendering.
    pub fn mark_lost(&self) {
        {
            let mut g = self.inner.lock().unwrap();
            g.pending = None;
            g.lost = true;
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Cheap per-iteration gate — `true` if a change is waiting. Lets the steady-state loop skip
    /// the mutex entirely on the overwhelmingly common "nothing changed" path.
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    /// Consume the pending change and clear the gate. A pending surface takes precedence over a
    /// prior loss (if a surface arrived after the loss we want to swap onto it, not report the
    /// now-stale loss). Returns [`SurfaceChange::None`] when nothing is pending.
    pub fn take(&self) -> SurfaceChange<T> {
        let mut g = self.inner.lock().unwrap();
        // Clear under the lock so a concurrent producer that re-raises `dirty` after us is not
        // lost: its write happens-after this store, leaving `dirty` true for the next poll.
        self.dirty.store(false, Ordering::Relaxed);
        if let Some(window) = g.pending.take() {
            g.lost = false;
            SurfaceChange::Swap(window)
        } else if std::mem::replace(&mut g.lost, false) {
            SurfaceChange::Lost
        } else {
            SurfaceChange::None
        }
    }
}

impl<T> Default for SurfaceMailbox<T> {
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

    // --- SurfaceMailbox (mid-session background/foreground swap channel) ------------------

    /// Match helper: `SurfaceChange` has no `PartialEq` (its `T` need not), so assert by arm.
    fn swapped<T>(c: SurfaceChange<T>) -> Option<T> {
        match c {
            SurfaceChange::Swap(w) => Some(w),
            _ => None,
        }
    }
    fn is_lost<T>(c: &SurfaceChange<T>) -> bool {
        matches!(c, SurfaceChange::Lost)
    }
    fn is_none<T>(c: &SurfaceChange<T>) -> bool {
        matches!(c, SurfaceChange::None)
    }

    #[test]
    fn mailbox_starts_clean() {
        let mb: SurfaceMailbox<&str> = SurfaceMailbox::new();
        assert!(!mb.is_dirty(), "a fresh mailbox has nothing to apply");
        assert!(is_none(&mb.take()));
    }

    #[test]
    fn deposit_yields_one_swap_then_goes_clean() {
        // App foregrounded: a new surface arrives and the loop swaps onto it exactly once.
        let mb: SurfaceMailbox<&str> = SurfaceMailbox::new();
        mb.deposit("surface-2");
        assert!(mb.is_dirty(), "a deposit must raise the poll gate");
        assert_eq!(swapped(mb.take()), Some("surface-2"));
        assert!(!mb.is_dirty(), "take clears the gate");
        assert!(is_none(&mb.take()), "a second poll sees no change");
    }

    #[test]
    fn mark_lost_yields_one_lost_then_goes_clean() {
        // App backgrounded: the surface is destroyed; the loop pauses rendering exactly once.
        let mb: SurfaceMailbox<&str> = SurfaceMailbox::new();
        mb.mark_lost();
        assert!(mb.is_dirty());
        assert!(is_lost(&mb.take()));
        assert!(!mb.is_dirty());
        assert!(is_none(&mb.take()));
    }

    #[test]
    fn deposit_then_lost_reports_lost_and_drops_the_destroyed_surface() {
        // A surface arrived and was destroyed before the loop polled: the loss supersedes it and
        // the (now-dead) surface is dropped, never handed to the decoder.
        let mb: SurfaceMailbox<&str> = SurfaceMailbox::new();
        mb.deposit("doomed-surface");
        mb.mark_lost();
        assert!(
            is_lost(&mb.take()),
            "destroy after deposit must win → pause render"
        );
        assert!(is_none(&mb.take()));
    }

    #[test]
    fn lost_then_deposit_reports_swap_freshest_wins() {
        // Background then foreground before the loop polled: the newest surface wins over the loss.
        let mb: SurfaceMailbox<&str> = SurfaceMailbox::new();
        mb.mark_lost();
        mb.deposit("surface-new");
        assert_eq!(swapped(mb.take()), Some("surface-new"));
        assert!(
            is_none(&mb.take()),
            "the loss was superseded, not also reported"
        );
    }

    #[test]
    fn newest_deposit_supersedes_an_untaken_one() {
        let mb: SurfaceMailbox<i32> = SurfaceMailbox::new();
        mb.deposit(1);
        mb.deposit(2);
        assert_eq!(
            swapped(mb.take()),
            Some(2),
            "only the freshest surface is handed out"
        );
        assert!(is_none(&mb.take()));
    }
}
