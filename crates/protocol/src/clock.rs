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
}
