//! Input-side frame pacing for the live decode path (latency root-cause fix, category A).
//!
//! The Pixel 6a's hardware H.264 decoder accumulates frames in its **input** queue when fed
//! faster than it drains; `arrive→decode` measures exactly that residence, so dropping frames
//! on the output side cannot reduce it (see
//! `docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md`). [`InputPacer`]
//! decides, per incoming frame, whether to feed it to the decoder or drop it — keeping the
//! in-flight depth bounded. Because H.264 deltas depend on prior frames, once we drop a delta
//! we must drop every frame until the next keyframe (drop-to-keyframe), where the stream
//! self-resyncs. Keyframes are always admitted so a backlog is always recoverable within one
//! GOP. This is the streaming analogue of the host's `coalesce_to_latest_keyframe`.

/// The pacer's decision for one inbound frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Feed this frame to the decoder.
    Feed,
    /// Drop this frame — the decoder input queue is at the cap, or we are mid drop-episode
    /// awaiting a keyframe to resync. No host action needed (the H.264 chain re-anchors on
    /// the next keyframe).
    Drop,
    /// Drop this frame AND ask the host for an out-of-band keyframe: this is the first drop
    /// of a drop-episode, so a [`protocol::messages::Control::RequestKeyframe`] should be sent
    /// to force an IDR and bound the resync to ~1 RTT instead of the periodic GOP (up to ~1 s).
    /// Emitted exactly once per episode (only on the transition into drop mode), so a burst of
    /// dropped deltas cannot flood the single-threaded receive loop.
    DropAndRequestKeyframe,
}

/// Bounds the decoder's in-flight input depth by dropping forward to the next keyframe.
pub struct InputPacer {
    /// Admit while in-flight depth is strictly below this. A small cap (1–2) keeps latency
    /// at a couple of frames; matches Moonlight's `OUTPUT_BUFFER_QUEUE_LIMIT = 2`.
    max_in_flight: usize,
    /// True after we dropped a non-keyframe: stay in drop mode until a keyframe resyncs.
    dropping: bool,
}

impl InputPacer {
    /// New pacer admitting while in-flight depth is `< max_in_flight`. A `max_in_flight` of 0
    /// is clamped to 1 (always allow at least the frame that is about to be decoded).
    pub fn new(max_in_flight: usize) -> Self {
        Self {
            max_in_flight: max_in_flight.max(1),
            dropping: false,
        }
    }

    /// Decide whether to feed the next frame to the decoder.
    ///
    /// - In drop mode: drop everything until a `keyframe`, which resyncs and clears the mode.
    /// - Otherwise: drop a non-keyframe once `in_flight >= max_in_flight` (and enter drop mode);
    ///   admit keyframes unconditionally (they are the resync point and bound the backlog).
    ///
    /// Returns [`Admission::Feed`] to feed, [`Admission::Drop`] to drop, or
    /// [`Admission::DropAndRequestKeyframe`] to drop and also ask the host for an IDR (the
    /// first drop of an episode).
    pub fn admit(&mut self, keyframe: bool, in_flight: usize) -> Admission {
        if self.dropping {
            if keyframe {
                self.dropping = false;
                Admission::Feed
            } else {
                Admission::Drop
            }
        } else if !keyframe && in_flight >= self.max_in_flight {
            self.dropping = true;
            Admission::DropAndRequestKeyframe
        } else {
            Admission::Feed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_everything_while_depth_is_under_cap() {
        let mut p = InputPacer::new(2);
        assert_eq!(p.admit(false, 0), Admission::Feed);
        assert_eq!(p.admit(false, 1), Admission::Feed);
        assert_eq!(p.admit(true, 1), Admission::Feed);
    }

    #[test]
    fn drops_delta_at_or_above_cap_and_enters_drop_mode() {
        let mut p = InputPacer::new(2);
        // depth == cap, non-keyframe → drop, and we are now in drop mode (first drop of the
        // episode also requests a keyframe).
        assert_eq!(p.admit(false, 2), Admission::DropAndRequestKeyframe);
        // Even if depth falls back under the cap, we keep dropping deltas until a keyframe.
        assert_eq!(p.admit(false, 0), Admission::Drop);
    }

    #[test]
    fn keyframe_always_admitted_and_clears_drop_mode() {
        let mut p = InputPacer::new(2);
        assert_eq!(p.admit(false, 5), Admission::DropAndRequestKeyframe); // enter drop mode
        assert_eq!(p.admit(true, 5), Admission::Feed); // keyframe admitted even over cap, resyncs
        assert_eq!(p.admit(false, 0), Admission::Feed); // back to normal admission after resync
    }

    #[test]
    fn keyframe_over_cap_when_not_dropping_is_still_admitted() {
        let mut p = InputPacer::new(2);
        // Not in drop mode, depth high, but it's a keyframe → admit (it's the resync point).
        assert_eq!(p.admit(true, 9), Admission::Feed);
        // Drop mode was never entered, so the next under-cap delta is admitted.
        assert_eq!(p.admit(false, 0), Admission::Feed);
    }

    #[test]
    fn new_with_zero_clamps_to_one() {
        let mut p = InputPacer::new(0);
        assert_eq!(p.admit(false, 0), Admission::Feed); // depth 0 < 1 → feed
        assert_eq!(p.admit(false, 1), Admission::DropAndRequestKeyframe); // depth 1 >= 1 → drop
    }

    #[test]
    fn dropped_then_keyframe_then_delta_sequence() {
        // A realistic burst: cap=1, depth spikes, deltas drop until the keyframe.
        let mut p = InputPacer::new(1);
        assert_eq!(p.admit(false, 0), Admission::Feed); // depth 0 < 1 → feed
        assert_eq!(p.admit(false, 1), Admission::DropAndRequestKeyframe); // 1 >= 1, delta → drop
        assert_eq!(p.admit(false, 1), Admission::Drop); // still dropping
        assert_eq!(p.admit(true, 1), Admission::Feed); // keyframe → resync
        assert_eq!(p.admit(false, 0), Admission::Feed); // depth 0 → feed
    }

    #[test]
    fn requests_keyframe_once_per_drop_episode() {
        // The first dropped delta of an episode must signal a keyframe request (so the host
        // can force an out-of-band IDR and bound the resync to ~1 RTT); later drops in the
        // SAME episode must not re-request (one outstanding request — no flooding the
        // single-threaded receive loop); a fresh episode after a resync requests again.
        let mut p = InputPacer::new(2);
        assert_eq!(p.admit(false, 0), Admission::Feed); // under cap → feed
        assert_eq!(p.admit(false, 2), Admission::DropAndRequestKeyframe); // episode start
        assert_eq!(p.admit(false, 2), Admission::Drop); // mid-episode → no re-request
        assert_eq!(p.admit(false, 0), Admission::Drop); // still dropping until a keyframe
        assert_eq!(p.admit(true, 5), Admission::Feed); // keyframe resyncs, ends episode
        assert_eq!(p.admit(false, 3), Admission::DropAndRequestKeyframe); // new episode
    }
}
