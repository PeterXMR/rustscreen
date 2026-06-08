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

use std::collections::{HashMap, VecDeque};

use crate::encode::LatencyStats;
use protocol::clock::ClockOffset;

/// Host-side per-frame stage timestamps (host clock, microseconds), held until the
/// matching phone [`protocol::messages::Frame::Stats`] arrives. Keyed by `pts_us` in the
/// in-flight map, so that field lives in the key, not here.
#[derive(Debug, Clone, Copy)]
struct HostStamps {
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
    /// Count of encoded frames the send loop deliberately dropped via drop-to-keyframe (item 6
    /// step 2) because a fresh IDR made the queued P-frames stale. Surfaced so the latency win
    /// is attributable to shed load: a non-zero value with bounded latency is the intended
    /// tradeoff; a runaway value means the pipeline is badly under-provisioned for the bitrate.
    pub dropped_frames: u64,
}

impl LatencyReport {
    /// Compact, grep-friendly one-line summary for the live log: glass-to-glass p50/p95 plus each
    /// stage's p50 (all ms), and the shed/anomaly counters. Lets you eyeball a run at a glance and
    /// `grep SUMMARY` a long log. The standing-queue tell to watch (PR 1 lesson): if `snd→arr`
    /// climbs and plateaus while `drop=0`, frames are bufferbloating between host and phone.
    pub fn summary_line(&self) -> String {
        let p50 = |s: &LatencyStats| s.p50().map_or(f64::NAN, |v| v as f64 / 1000.0);
        let p95 = |s: &LatencyStats| s.p95().map_or(f64::NAN, |v| v as f64 / 1000.0);
        format!(
            "G2G p50={:.1} p95={:.1} | cap→enc {:.1} | enc→snd {:.1} | snd→arr {:.1} | \
             arr→dec {:.1} | dec→pres {:.1} | drop={} unmatched={} (n={})",
            p50(&self.glass_to_glass),
            p95(&self.glass_to_glass),
            p50(&self.capture_to_encode),
            p50(&self.encode_to_send),
            p50(&self.send_to_arrive),
            p50(&self.arrive_to_decode),
            p50(&self.decode_to_present),
            self.dropped_frames,
            self.unmatched_stats,
            self.glass_to_glass.count(),
        )
    }

    /// CSV header matching [`Self::csv_row`]; written once when a fresh benchmark log is created.
    pub fn csv_header() -> &'static str {
        "elapsed_s,n,g2g_p50,g2g_p95,g2g_max,cap_enc_p50,cap_enc_p95,enc_snd_p50,enc_snd_p95,\
         snd_arr_p50,snd_arr_p95,arr_dec_p50,arr_dec_p95,dec_pres_p50,dec_pres_p95,\
         dropped,unmatched,anomalies"
    }

    /// One CSV row (all stage times in **ms**) for accumulating results across runs and proposed
    /// improvements. `elapsed_s` is wall-seconds since the session started, so rows stay ordered.
    pub fn csv_row(&self, elapsed_s: f64) -> String {
        let p50 = |s: &LatencyStats| s.p50().map_or(0.0, |v| v as f64 / 1000.0);
        let p95 = |s: &LatencyStats| s.p95().map_or(0.0, |v| v as f64 / 1000.0);
        let mx = |s: &LatencyStats| s.max().map_or(0.0, |v| v as f64 / 1000.0);
        format!(
            "{:.1},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},\
             {:.2},{:.2},{},{},{}",
            elapsed_s,
            self.glass_to_glass.count(),
            p50(&self.glass_to_glass),
            p95(&self.glass_to_glass),
            mx(&self.glass_to_glass),
            p50(&self.capture_to_encode),
            p95(&self.capture_to_encode),
            p50(&self.encode_to_send),
            p95(&self.encode_to_send),
            p50(&self.send_to_arrive),
            p95(&self.send_to_arrive),
            p50(&self.arrive_to_decode),
            p95(&self.arrive_to_decode),
            p50(&self.decode_to_present),
            p95(&self.decode_to_present),
            self.dropped_frames,
            self.unmatched_stats,
            self.anomalies,
        )
    }

    /// Append one [`Self::csv_row`] to the benchmark log at `path`, writing [`Self::csv_header`]
    /// first if the file is new/empty. Lets a live run accumulate a CSV you can diff across
    /// proposed improvements (`RUSTSCREEN_LATENCY_CSV=/path` in `serve`). Best-effort: the caller
    /// logs and continues on error rather than disturbing the stream.
    pub fn append_csv(&self, path: &std::path::Path, elapsed_s: f64) -> std::io::Result<()> {
        use std::io::Write as _;
        // Create the parent directory if the user pointed RUSTSCREEN_LATENCY_CSV at a path whose
        // folder doesn't exist yet (e.g. ~/rustscreen-bench/run.csv) — otherwise the open below
        // fails with "No such file or directory" on every report.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let fresh = std::fs::metadata(path)
            .map(|m| m.len() == 0)
            .unwrap_or(true);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        if fresh {
            writeln!(f, "{}", Self::csv_header())?;
        }
        writeln!(f, "{}", self.csv_row(elapsed_s))
    }
}

/// Fuses host-side and phone-side per-frame timestamps (correlated by `pts_us`) into a
/// per-stage + glass-to-glass [`LatencyReport`]. Host timestamps wait in a bounded FIFO
/// keyed by `pts_us` until the phone's `Frame::Stats` arrives (or they are evicted).
///
/// Storage is O(1): `inflight` maps `pts_us → HostStamps` for lookup/removal, and
/// `order` is a FIFO of keys in insertion order so the oldest un-matched record can be
/// evicted when over capacity (the old design scanned/`remove`d a `VecDeque`, O(n)).
#[derive(Debug)]
pub struct PipelineLatency {
    inflight: HashMap<u64, HostStamps>,
    /// `pts_us` keys in insertion order, for bounded oldest-first eviction.
    order: VecDeque<u64>,
    capacity: usize,
    report: LatencyReport,
}

impl PipelineLatency {
    /// New accumulator retaining at most `capacity` un-matched host records.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            inflight: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
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
        // Evict oldest-by-insertion until there is room for one more. `order` may hold
        // keys already removed by record_stats, so skip any that are no longer present.
        while self.inflight.len() >= self.capacity {
            match self.order.pop_front() {
                Some(old) => {
                    self.inflight.remove(&old);
                }
                None => break,
            }
        }
        // Only track ordering for a newly inserted key. A duplicate `pts_us` (genuine repeat or
        // the ~584-year u64-µs wrap) overwrites the value in place and is already in `order`, so it
        // must NOT be pushed again — `insert` returning `Some` signals that.
        if self
            .inflight
            .insert(
                pts_us,
                HostStamps {
                    capture_us,
                    encode_done_us,
                    send_done_us,
                },
            )
            .is_none()
        {
            self.order.push_back(pts_us);
        }
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
        let Some(h) = self.inflight.remove(&pts_us) else {
            // No host record for this pts (never recorded, or already evicted). Count it so
            // the report surfaces silent eviction instead of just dropping the sample.
            self.report.unmatched_stats += 1;
            return;
        };
        // Note: the key may linger in `order` until it reaches the front during eviction,
        // where the stale-key skip in record_host drops it. This keeps removal O(1).

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

    /// Record that the send loop dropped `n` stale frames (drop-to-keyframe). Folded into the
    /// report so the periodic print surfaces the shed-load count alongside the latency stages.
    pub fn record_dropped(&mut self, n: u64) {
        self.report.dropped_frames += n;
    }

    /// Borrow the accumulated report so far. Returns a reference rather than cloning: a
    /// [`LatencyReport`] holds six bounded sample rings (6 × up to `MAX_RETAINED_SAMPLES` u64s),
    /// and the periodic ~2 s report tick runs on the streaming thread — cloning ~0.8 MB there
    /// would steal hot-path time. Every caller only reads.
    pub fn report(&self) -> &LatencyReport {
        &self.report
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
    fn interleaved_out_of_order_pts_still_fuse() {
        // Characterization: record three host stamps, then fuse their phone stats in a
        // DIFFERENT order than recorded. HashMap lookup must find each by pts_us regardless
        // of insertion order (this already held under the old position-scan VecDeque, so it
        // pins behavior across the storage change rather than failing first).
        let mut p = PipelineLatency::new(16);
        p.record_host(10, 0, 1, 2);
        p.record_host(20, 0, 2, 4);
        p.record_host(30, 0, 3, 6);

        // Fuse middle, then last, then first — all out of insertion order.
        p.record_stats(20, 100, 100, 100, offset(100)); // host present = 0
        p.record_stats(30, 100, 100, 100, offset(100));
        p.record_stats(10, 100, 100, 100, offset(100));

        let r = p.report();
        assert_eq!(
            r.glass_to_glass.count(),
            3,
            "all three fuse regardless of order"
        );
        assert_eq!(r.unmatched_stats, 0, "every pts found a host record");
        // capture(0) → present_host(0) for each → max gap 0.
        assert_eq!(r.glass_to_glass.max(), Some(0));
        // capture_to_encode max across {1,2,3} = 3.
        assert_eq!(r.capture_to_encode.max(), Some(3));
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
    fn record_dropped_accumulates_into_report() {
        let mut p = PipelineLatency::new(16);
        assert_eq!(p.report().dropped_frames, 0);
        p.record_dropped(3);
        p.record_dropped(2);
        assert_eq!(
            p.report().dropped_frames,
            5,
            "drop-to-keyframe counts accumulate so the report surfaces total shed load"
        );
    }

    #[test]
    fn summary_line_reports_glass_to_glass_p50() {
        let mut p = PipelineLatency::new(16);
        p.record_host(1000, 0, 8, 10);
        p.record_stats(1000, 110, 120, 130, offset(100)); // g2g = 30 µs = 0.0 ms rounded
        let line = p.report().summary_line();
        assert!(
            line.contains("G2G p50="),
            "summary must lead with glass-to-glass: {line}"
        );
        assert!(
            line.contains("(n=1)"),
            "summary must show the sample count: {line}"
        );
    }

    #[test]
    fn csv_row_column_count_matches_header() {
        // The CSV is only useful if every row lines up with the header — guard that invariant so
        // a future column add/remove can't silently desync the benchmark log.
        let header_cols = LatencyReport::csv_header().split(',').count();
        let row_cols = LatencyReport::default().csv_row(12.5).split(',').count();
        assert_eq!(
            header_cols, row_cols,
            "csv_row must have one field per header column"
        );
    }

    #[test]
    fn append_csv_writes_header_once_then_rows() {
        // Two appends to a fresh file: header written exactly once, then one row per call — so a
        // run accumulates a diffable benchmark log (RUSTSCREEN_LATENCY_CSV).
        // Use a NON-existent subdirectory so the test also pins parent-dir creation (the
        // RUSTSCREEN_LATENCY_CSV → "No such file or directory" bug fix).
        let dir = std::env::temp_dir().join(format!("rustscreen-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("run.csv");
        let r = LatencyReport::default();
        r.append_csv(&path, 2.0).unwrap();
        r.append_csv(&path, 4.0).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3, "1 header + 2 rows: {body}");
        assert_eq!(lines[0], LatencyReport::csv_header());
        assert!(lines[1].starts_with("2.0,"));
        assert!(lines[2].starts_with("4.0,"));
        let _ = std::fs::remove_dir_all(&dir);
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
