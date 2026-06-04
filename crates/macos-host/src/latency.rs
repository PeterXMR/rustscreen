//! Pure per-sample latency accumulator (count / mean / min / max), unit-tested
//! off-hardware.
//!
//! Extracted from the `p3_encode` spike, which used it to report VideoToolbox
//! per-frame encode latency. It is deliberately a plain, dependency-free struct so
//! the **P5 pipeline** can reuse it for glass-to-glass latency-budget tracking (the
//! roadmap's < 50 ms criterion) without dragging in any macOS/objc2 types.
//!
//! Latencies are tracked in **microseconds** (`u128`, matching
//! [`std::time::Duration::as_micros`]); `avg_us` returns `f64` so the mean keeps
//! sub-microsecond resolution. The accumulator is generic over what is being timed —
//! it only records numbers.

/// Accumulates per-sample latency measurements (microseconds) and reports
/// count / mean / min / max.
///
/// The seeding is **count-based, not value-based**: the first [`record`](Self::record)
/// call seeds both `min` and `max` from that sample. This is the key correctness point
/// — a measurement of `0` µs (a legitimately sub-microsecond sample) is a real value,
/// not a sentinel for "unset". A value-initialised accumulator (`min` starting at `0`
/// and treating `0` as "no data yet") would wrongly discard a true `0` and could let a
/// later, larger sample masquerade as the minimum.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LatencyAccum {
    count: u64,
    sum_us: u128,
    /// Smallest sample seen. Meaningful only when `count > 0` (seeded by the first sample).
    min_us: u128,
    /// Largest sample seen. Meaningful only when `count > 0`.
    max_us: u128,
}

impl LatencyAccum {
    /// A fresh accumulator with no samples.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one latency sample, in microseconds.
    ///
    /// The first sample seeds both `min` and `max`; subsequent samples widen them.
    /// `us == 0` is a valid sample (sub-microsecond timing), not "unset".
    pub fn record(&mut self, us: u128) {
        if self.count == 0 {
            self.min_us = us;
            self.max_us = us;
        } else {
            if us < self.min_us {
                self.min_us = us;
            }
            if us > self.max_us {
                self.max_us = us;
            }
        }
        self.sum_us += us;
        self.count += 1;
    }

    /// Number of samples recorded.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Smallest sample (µs), or `None` when no samples have been recorded.
    ///
    /// Returns `Option` rather than `0`-on-empty deliberately: `0` is a *valid*
    /// (sub-microsecond) measurement, so a `0` sentinel for "no data" would be
    /// indistinguishable from a real reading — the very confusion this type avoids.
    pub fn min_us(&self) -> Option<u128> {
        (self.count > 0).then_some(self.min_us)
    }

    /// Largest sample (µs), or `None` when no samples have been recorded.
    pub fn max_us(&self) -> Option<u128> {
        (self.count > 0).then_some(self.max_us)
    }

    /// Arithmetic mean of recorded samples (µs); `0.0` when there are no samples.
    pub fn avg_us(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum_us as f64 / self.count as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_accum_has_no_samples() {
        let a = LatencyAccum::new();
        assert_eq!(a.count(), 0);
        assert_eq!(a.avg_us(), 0.0);
        // Empty → None, NOT 0 (0 is a valid measurement, never a sentinel).
        assert_eq!(a.min_us(), None);
        assert_eq!(a.max_us(), None);
    }

    #[test]
    fn first_sample_seeds_min_max_and_avg() {
        let mut a = LatencyAccum::new();
        a.record(500);
        assert_eq!(a.count(), 1);
        assert_eq!(a.min_us(), Some(500));
        assert_eq!(a.max_us(), Some(500));
        assert_eq!(a.avg_us(), 500.0);
    }

    #[test]
    fn min_tracks_smallest_max_tracks_largest() {
        let mut a = LatencyAccum::new();
        for us in [500u128, 200, 800, 350] {
            a.record(us);
        }
        assert_eq!(a.count(), 4);
        assert_eq!(a.min_us(), Some(200));
        assert_eq!(a.max_us(), Some(800));
    }

    #[test]
    fn avg_is_arithmetic_mean() {
        let mut a = LatencyAccum::new();
        a.record(100);
        a.record(300);
        assert_eq!(a.avg_us(), 200.0);
    }

    #[test]
    fn zero_us_first_sample_is_valid_not_unset() {
        // Regression: the previous inline accumulator used `if min == 0 { us }` seeding,
        // which treated a genuine 0 µs first sample as "unset" and let the SECOND sample
        // overwrite the minimum. Here a 0 µs first sample must stick as the real minimum.
        let mut a = LatencyAccum::new();
        a.record(0);
        a.record(10);
        assert_eq!(a.count(), 2);
        assert_eq!(
            a.min_us(),
            Some(0),
            "a true 0 µs sample is the minimum, not discarded"
        );
        assert_eq!(a.max_us(), Some(10));
        assert_eq!(a.avg_us(), 5.0);
    }

    #[test]
    fn zero_us_later_sample_lowers_min() {
        let mut a = LatencyAccum::new();
        a.record(7);
        a.record(0);
        assert_eq!(a.min_us(), Some(0));
        assert_eq!(a.max_us(), Some(7));
    }
}
