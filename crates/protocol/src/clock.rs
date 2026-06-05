//! Pure SNTP-style clock-offset estimation between the host (Mac) and client (Pixel).
//!
//! The two devices run independent monotonic clocks. To express a phone-clock timestamp
//! in host-clock terms (needed for glass-to-glass latency), we estimate the offset with a
//! four-timestamp exchange, exactly like NTP:
//!
//! ```text
//! host sends ClockPing at t0 (host clock)
//! phone receives it at   t1 (phone clock)
//! phone sends ClockPong at t2 (phone clock)
//! host receives it at    t3 (host clock)
//! ```
//!
//! `offset = ((t1 - t0) + (t2 - t3)) / 2`  (phone_clock - host_clock)
//! `rtt    = (t3 - t0) - (t2 - t1)`
//!
//! To convert a phone timestamp `p` to host time: `p - offset`.

/// The result of one clock-offset estimation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockOffset {
    /// phone_clock − host_clock, in microseconds. Add to a host time to get phone time;
    /// subtract from a phone time to get host time. Signed: the phone may be behind.
    pub offset_us: i64,
    /// Round-trip time of the exchange, in microseconds. The offset error is bounded by
    /// `±rtt_us / 2`; callers log it as the precision caveat on any fused number.
    pub rtt_us: u64,
}

/// Estimate the clock offset from the four exchange timestamps (all microseconds).
///
/// Computed in `i128` so neither the subtraction nor the sum can overflow/underflow for
/// any `u64` inputs. `rtt` is clamped to `>= 0` (a non-monotonic capture would otherwise
/// produce a negative round-trip, which is meaningless).
pub fn estimate(t0: u64, t1: u64, t2: u64, t3: u64) -> ClockOffset {
    let (t0, t1, t2, t3) = (t0 as i128, t1 as i128, t2 as i128, t3 as i128);
    let offset = ((t1 - t0) + (t2 - t3)) / 2;
    let rtt = (t3 - t0) - (t2 - t1);
    ClockOffset {
        offset_us: offset as i64,
        rtt_us: rtt.max(0) as u64,
    }
}

/// Estimate the clock offset from the best of several four-timestamp exchanges.
///
/// Each tuple in `samples` is one `(t0, t1, t2, t3)` exchange, fed through [`estimate`].
/// The sample with the smallest `rtt_us` is the least contaminated by scheduling jitter,
/// so its offset is the most trustworthy — this is standard NTP "best of N" practice.
/// Returns `None` for an empty slice.
pub fn estimate_best_of(samples: &[(u64, u64, u64, u64)]) -> Option<ClockOffset> {
    samples
        .iter()
        .map(|&(t0, t1, t2, t3)| estimate(t0, t1, t2, t3))
        .min_by_key(|o| o.rtt_us)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_delay_zero_offset() {
        // Up delay == down delay, phone clock == host clock → offset 0, rtt = 2*delay.
        // t0=0, t1=100, t2=110 (10us server hold), t3=210.
        let o = estimate(0, 100, 110, 210);
        assert_eq!(o.offset_us, 0);
        assert_eq!(o.rtt_us, 200);
    }

    #[test]
    fn known_offset_symmetric_delay() {
        // Delay D=100 each way, phone ahead by O=50, zero server hold:
        // t0=0, t1=D+O=150, t2=150, t3=2D=200.
        let o = estimate(0, 150, 150, 200);
        assert_eq!(o.offset_us, 50);
        assert_eq!(o.rtt_us, 200);
    }

    #[test]
    fn phone_behind_negative_offset() {
        // Mirror of the above with phone behind by 50: O=-50.
        // t0=0, t1=D+O=50, t2=50, t3=200.
        let o = estimate(0, 50, 50, 200);
        assert_eq!(o.offset_us, -50);
        assert_eq!(o.rtt_us, 200);
    }

    #[test]
    fn rtt_excludes_server_hold() {
        // Server holds 40us (t2-t1=40); rtt must subtract it: (t3-t0)-(t2-t1).
        // t0=0, t1=100, t2=140, t3=200 → rtt = 200 - 40 = 160.
        let o = estimate(0, 100, 140, 200);
        assert_eq!(o.rtt_us, 160);
    }

    #[test]
    fn non_monotonic_clamps_rtt_to_zero() {
        // Degenerate inputs (hold longer than the whole round-trip) must not produce a
        // negative rtt; clamp to 0 rather than panic or wrap.
        let o = estimate(0, 100, 500, 200);
        assert_eq!(o.rtt_us, 0);
    }

    #[test]
    fn best_of_empty_is_none() {
        // No samples → nothing to estimate.
        assert_eq!(estimate_best_of(&[]), None);
    }

    #[test]
    fn best_of_single_matches_estimate() {
        // One sample must reduce to the plain single-sample estimator.
        let sample = (0u64, 150, 150, 200);
        assert_eq!(
            estimate_best_of(&[sample]),
            Some(estimate(sample.0, sample.1, sample.2, sample.3))
        );
    }

    #[test]
    fn best_of_picks_smallest_rtt() {
        // Three samples, all with true offset 0 (symmetric delay, no server hold), but
        // increasingly contaminated round-trips: rtt = 400, 200, 600. The middle one
        // (rtt 200) is the least contaminated and must win — NOT the first or the noisiest.
        let samples = [
            (0u64, 200, 200, 400), // rtt 400
            (0u64, 100, 100, 200), // rtt 200  ← best
            (0u64, 300, 300, 600), // rtt 600
        ];
        let best = estimate_best_of(&samples).unwrap();
        assert_eq!(best.rtt_us, 200);
        assert_eq!(best, estimate(0, 100, 100, 200));
    }

    #[test]
    fn best_of_ties_keep_first() {
        // Two samples with identical rtt (200) but different offsets. The tie-break is
        // deterministic: the FIRST minimum encountered wins (earliest sample), matching
        // `Iterator::min_by_key`. Here the first has offset +50, the second offset -50.
        let first = (0u64, 150, 150, 200); // offset +50, rtt 200
        let second = (0u64, 50, 50, 200); // offset -50, rtt 200
        let best = estimate_best_of(&[first, second]).unwrap();
        assert_eq!(best, estimate(0, 150, 150, 200));
        assert_eq!(best.offset_us, 50);
    }

    #[test]
    fn best_of_large_offset_overflow_safe() {
        // Realistic monotonic clocks: host near 5_000_000_000 us (~83 min uptime), phone
        // near 9_000_000_000 us — a ~4 s offset. A naive i64 (t1 - t0) would be fine, but
        // the sum/diff is computed in i128 by `estimate`; verify the best-of path inherits
        // that safety and still selects by smallest rtt.
        let host0 = 5_000_000_000u64;
        // phone ahead by 4 s.
        let phone_offset = 4_000_000_000i64;
        // Sample A: one-way delay 100us each, no hold, rtt 200 (best).
        let a = (
            host0,
            (host0 as i64 + 100 + phone_offset) as u64, // t1
            (host0 as i64 + 100 + phone_offset) as u64, // t2 (no hold)
            host0 + 200,                                // t3
        );
        // Sample B: same offset but one-way delay 500us, rtt 1000 (noisier, must lose).
        let b = (
            host0,
            (host0 as i64 + 500 + phone_offset) as u64,
            (host0 as i64 + 500 + phone_offset) as u64,
            host0 + 1000,
        );
        let best = estimate_best_of(&[b, a]).unwrap();
        assert_eq!(best.rtt_us, 200);
        assert_eq!(best.offset_us, phone_offset);
        assert_eq!(best, estimate(a.0, a.1, a.2, a.3));
    }
}
