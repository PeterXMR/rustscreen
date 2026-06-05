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

use std::collections::VecDeque;

use crate::encode::LatencyStats;
use protocol::clock::ClockOffset;

/// Host-side per-frame stage timestamps (host clock, microseconds), held until the
/// matching phone [`protocol::messages::Frame::Stats`] arrives.
#[derive(Debug, Clone, Copy)]
struct HostStamps {
    pts_us: u64,
    capture_us: u64,
    encode_done_us: u64,
    send_done_us: u64,
}

/// A completed per-stage + glass-to-glass latency report (each field µs).
#[derive(Debug, Clone, Default)]
pub struct LatencyReport {
    pub capture_to_encode: LatencyStats,
    pub encode_to_send: LatencyStats,
    pub send_to_arrive: LatencyStats,
    pub arrive_to_decode: LatencyStats,
    pub decode_to_present: LatencyStats,
    pub glass_to_glass: LatencyStats,
    /// Count of samples with a negative computed interval (clock jitter), clamped to 0.
    pub anomalies: u64,
    /// Count of phone `Frame::Stats` that arrived with NO matching in-flight host record
    /// (its `pts_us` was never recorded, or was already evicted from the bounded FIFO).
    /// Surfaced so the report can't silently under-count: a high value means host records
    /// are being evicted before the phone's stats catch up (capacity too small or the phone
    /// lagging badly).
    pub unmatched_stats: u64,
}

/// Fuses host-side and phone-side per-frame timestamps (correlated by `pts_us`) into a
/// per-stage + glass-to-glass [`LatencyReport`]. Host timestamps wait in a bounded FIFO
/// keyed by `pts_us` until the phone's `Frame::Stats` arrives (or they are evicted).
#[derive(Debug)]
pub struct PipelineLatency {
    inflight: VecDeque<HostStamps>,
    capacity: usize,
    report: LatencyReport,
}

impl PipelineLatency {
    /// New accumulator retaining at most `capacity` un-matched host records.
    pub fn new(capacity: usize) -> Self {
        Self {
            inflight: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
            report: LatencyReport::default(),
        }
    }

    /// Record the host-side stage times for one frame (host clock, µs). Evicts the oldest
    /// un-matched record when over capacity.
    pub fn record_host(
        &mut self,
        pts_us: u64,
        capture_us: u64,
        encode_done_us: u64,
        send_done_us: u64,
    ) {
        if self.inflight.len() >= self.capacity {
            self.inflight.pop_front();
        }
        self.inflight.push_back(HostStamps {
            pts_us,
            capture_us,
            encode_done_us,
            send_done_us,
        });
    }

    /// Record the phone's `Frame::Stats` for one frame, converting its (phone-clock) times
    /// to host time via `offset`, then fusing with the matching host record. A report whose
    /// `pts_us` has no in-flight host record is dropped.
    pub fn record_stats(
        &mut self,
        pts_us: u64,
        arrive_us: u64,
        decode_us: u64,
        present_us: u64,
        offset: ClockOffset,
    ) {
        let Some(idx) = self.inflight.iter().position(|h| h.pts_us == pts_us) else {
            // No host record for this pts (never recorded, or already evicted). Count it so
            // the report surfaces silent eviction instead of just dropping the sample.
            self.report.unmatched_stats += 1;
            return;
        };
        let h = self
            .inflight
            .remove(idx)
            .expect("index from position is valid");

        // Phone clock → host clock: host = phone - offset.
        let to_host = |phone: u64| phone as i128 - offset.offset_us as i128;
        let arrive_h = to_host(arrive_us);
        let decode_h = to_host(decode_us);
        let present_h = to_host(present_us);

        let mut anomaly = false;
        let mut gap = |stats: &mut LatencyStats, from: i128, to: i128| {
            let d = to - from;
            if d < 0 {
                anomaly = true;
                stats.record(0);
            } else {
                stats.record(d as u64);
            }
        };

        gap(
            &mut self.report.capture_to_encode,
            h.capture_us as i128,
            h.encode_done_us as i128,
        );
        gap(
            &mut self.report.encode_to_send,
            h.encode_done_us as i128,
            h.send_done_us as i128,
        );
        gap(
            &mut self.report.send_to_arrive,
            h.send_done_us as i128,
            arrive_h,
        );
        gap(&mut self.report.arrive_to_decode, arrive_h, decode_h);
        gap(&mut self.report.decode_to_present, decode_h, present_h);
        gap(
            &mut self.report.glass_to_glass,
            h.capture_us as i128,
            present_h,
        );

        if anomaly {
            self.report.anomalies += 1;
        }
    }

    /// The accumulated report so far (cheap clone of the stage stats).
    pub fn report(&self) -> LatencyReport {
        self.report.clone()
    }
}

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

    fn offset(us: i64) -> protocol::clock::ClockOffset {
        protocol::clock::ClockOffset {
            offset_us: us,
            rtt_us: 0,
        }
    }

    #[test]
    fn fuses_host_and_phone_into_glass_to_glass() {
        let mut p = PipelineLatency::new(16);
        // Host stages for pts 1000 (host clock, µs): cap=0, enc=8, send=10.
        p.record_host(1000, 0, 8, 10);
        // Phone stages (phone clock): arrive=110, decode=120, present=130, offset=+100.
        // Converted to host clock: arrive=10, decode=20, present=30.
        p.record_stats(1000, 110, 120, 130, offset(100));
        let r = p.report();
        assert_eq!(r.glass_to_glass.count(), 1);
        // present_host(30) - capture(0) = 30 µs.
        assert_eq!(r.glass_to_glass.max(), Some(30));
        assert_eq!(r.capture_to_encode.max(), Some(8));
        assert_eq!(r.encode_to_send.max(), Some(2));
        assert_eq!(r.send_to_arrive.max(), Some(0));
        assert_eq!(r.arrive_to_decode.max(), Some(10));
        assert_eq!(r.decode_to_present.max(), Some(10));
    }

    #[test]
    fn stats_without_matching_host_record_is_dropped() {
        let mut p = PipelineLatency::new(16);
        p.record_stats(999, 100, 110, 120, offset(0)); // no host record for 999
        assert_eq!(p.report().glass_to_glass.count(), 0);
    }

    #[test]
    fn unmatched_stats_counter_increments_on_missing_host_record() {
        let mut p = PipelineLatency::new(16);
        // pts 999 was never recorded by record_host → unmatched.
        p.record_stats(999, 100, 110, 120, offset(0));
        assert_eq!(
            p.report().unmatched_stats,
            1,
            "missing host record is counted"
        );

        // A second unmatched stat increments again.
        p.record_stats(998, 100, 110, 120, offset(0));
        assert_eq!(p.report().unmatched_stats, 2);

        // A matched stat does NOT bump the unmatched counter.
        p.record_host(1000, 0, 8, 10);
        p.record_stats(1000, 110, 120, 130, offset(100));
        let r = p.report();
        assert_eq!(
            r.unmatched_stats, 2,
            "a matched stat leaves the counter untouched"
        );
        assert_eq!(r.glass_to_glass.count(), 1);
    }

    #[test]
    fn evicted_host_record_makes_stats_unmatched() {
        let mut p = PipelineLatency::new(2); // capacity 2 in-flight
        p.record_host(1, 0, 1, 2);
        p.record_host(2, 0, 1, 2);
        p.record_host(3, 0, 1, 2); // evicts pts 1
        p.record_stats(1, 10, 11, 12, offset(0)); // evicted → unmatched, not fused
        let r = p.report();
        assert_eq!(r.glass_to_glass.count(), 0);
        assert_eq!(
            r.unmatched_stats, 1,
            "an evicted host record makes its phone stat observably unmatched"
        );
    }

    #[test]
    fn bounded_map_evicts_oldest() {
        let mut p = PipelineLatency::new(2); // capacity 2 in-flight
        p.record_host(1, 0, 1, 2);
        p.record_host(2, 0, 1, 2);
        p.record_host(3, 0, 1, 2); // evicts pts 1
        p.record_stats(1, 10, 11, 12, offset(0)); // evicted → dropped
        p.record_stats(3, 10, 11, 12, offset(0)); // retained → fuses
        assert_eq!(p.report().glass_to_glass.count(), 1);
    }

    #[test]
    fn negative_interval_clamps_to_zero_and_counts_anomaly() {
        let mut p = PipelineLatency::new(16);
        p.record_host(1, 0, 1, 2);
        p.record_stats(1, 50, 50, 50, offset(100)); // present_host = 50-100 = -50
        let r = p.report();
        assert_eq!(r.glass_to_glass.max(), Some(0), "negative G2G clamps to 0");
        assert_eq!(r.anomalies, 1);
    }

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
