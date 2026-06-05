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
    /// Returns `true` to feed, `false` to drop.
    pub fn admit(&mut self, keyframe: bool, in_flight: usize) -> bool {
        if self.dropping {
            if keyframe {
                self.dropping = false;
                true
            } else {
                false
            }
        } else if !keyframe && in_flight >= self.max_in_flight {
            self.dropping = true;
            false
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_everything_while_depth_is_under_cap() {
        let mut p = InputPacer::new(2);
        assert!(p.admit(false, 0));
        assert!(p.admit(false, 1));
        assert!(p.admit(true, 1));
    }

    #[test]
    fn drops_delta_at_or_above_cap_and_enters_drop_mode() {
        let mut p = InputPacer::new(2);
        // depth == cap, non-keyframe → drop, and we are now in drop mode.
        assert!(!p.admit(false, 2));
        // Even if depth falls back under the cap, we keep dropping deltas until a keyframe.
        assert!(!p.admit(false, 0));
    }

    #[test]
    fn keyframe_always_admitted_and_clears_drop_mode() {
        let mut p = InputPacer::new(2);
        assert!(!p.admit(false, 5)); // enter drop mode
        assert!(p.admit(true, 5)); // keyframe admitted even over cap, resyncs
        assert!(p.admit(false, 0)); // back to normal admission after resync
    }

    #[test]
    fn keyframe_over_cap_when_not_dropping_is_still_admitted() {
        let mut p = InputPacer::new(2);
        // Not in drop mode, depth high, but it's a keyframe → admit (it's the resync point).
        assert!(p.admit(true, 9));
        // Drop mode was never entered, so the next under-cap delta is admitted.
        assert!(p.admit(false, 0));
    }

    #[test]
    fn new_with_zero_clamps_to_one() {
        let mut p = InputPacer::new(0);
        assert!(p.admit(false, 0)); // depth 0 < 1 → feed
        assert!(!p.admit(false, 1)); // depth 1 >= 1 → drop
    }

    #[test]
    fn dropped_then_keyframe_then_delta_sequence() {
        // A realistic burst: cap=1, depth spikes, deltas drop until the keyframe.
        let mut p = InputPacer::new(1);
        assert!(p.admit(false, 0)); // depth 0 < 1 → feed
        assert!(!p.admit(false, 1)); // depth 1 >= 1, delta → drop
        assert!(!p.admit(false, 1)); // still dropping
        assert!(p.admit(true, 1)); // keyframe → resync
        assert!(p.admit(false, 0)); // depth 0 → feed
    }
}
