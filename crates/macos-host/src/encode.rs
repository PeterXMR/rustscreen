//! The `Encoder` port (seam), the encode-session pipeline, and per-frame latency
//! stats — all platform-agnostic and unit-tested.
//!
//! The *simplest-now* adapter behind [`Encoder`] is the `videotoolbox` crate's H.264
//! encoder (D2: realtime, low-latency, no B-frames, zero-copy IOSurface); it is
//! already Rust source, so the P7 "pure-Rust" target is a no-op here. Wrapping it
//! behind this trait is the R3 mitigation: the experimental crate can be swapped or
//! given a `CGDisplayStream` fallback without touching pipeline logic.
//!
//! ## Handoff (needs a hands-on Mac run, not the phone)
//! The VideoToolbox adapter that implements [`Encoder`] requires a live capture +
//! `ffplay out.h264` visual check, so it lands separately. Everything in this module
//! — the pipeline that drives an `Encoder`, codec-config extraction, latency logging
//! — is tested here with a fake encoder, so the adapter drops in behind the seam.

use std::io::{self, Write};

use protocol::nal::{self, CodecConfig};

use crate::capture::{CapturedFrame, Capturer};

/// One access unit of encoder output: Annex-B NAL bytes plus metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    /// Presentation timestamp in microseconds (carried through from capture).
    pub pts_us: u64,
    /// Whether this access unit is a keyframe (IDR; carries SPS/PPS).
    pub keyframe: bool,
    /// Wall-clock time the encoder took to produce this frame, in microseconds.
    /// The adapter measures its own hardware-call latency so the pipeline can log it
    /// deterministically.
    pub encode_micros: u64,
    /// The encoded access unit as an Annex-B byte stream.
    pub annex_b: Vec<u8>,
}

/// The macOS encode seam (R3). The simplest-now adapter is VideoToolbox H.264.
pub trait Encoder {
    /// Encode one captured frame into an Annex-B access unit.
    fn encode(&mut self, frame: &CapturedFrame) -> EncodedFrame;
}

/// Accumulating per-frame encode-latency statistics (microseconds).
///
/// Tracks count/sum/min/max for cheap aggregates and retains the individual samples so
/// tail-latency percentiles can be reported. For RustScreen's sub-50 ms glass-to-glass
/// budget the **tail** (p95/p99) is what matters — a healthy mean can still hide a
/// stall that ruins the experience — so [`percentile`](Self::percentile) is the headline
/// metric, not [`mean`](Self::mean).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LatencyStats {
    count: u64,
    sum: u64,
    min: Option<u64>,
    max: Option<u64>,
    samples: Vec<u64>,
}

impl LatencyStats {
    /// A fresh, empty accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one frame's encode latency in microseconds.
    pub fn record(&mut self, micros: u64) {
        self.count += 1;
        self.sum += micros;
        self.min = Some(self.min.map_or(micros, |m| m.min(micros)));
        self.max = Some(self.max.map_or(micros, |m| m.max(micros)));
        self.samples.push(micros);
    }

    /// Number of frames recorded.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Smallest recorded latency, or `None` if no frames were recorded.
    pub fn min(&self) -> Option<u64> {
        self.min
    }

    /// Largest recorded latency, or `None` if no frames were recorded.
    pub fn max(&self) -> Option<u64> {
        self.max
    }

    /// Mean latency in microseconds, or `None` if no frames were recorded.
    pub fn mean(&self) -> Option<f64> {
        if self.count == 0 {
            None
        } else {
            Some(self.sum as f64 / self.count as f64)
        }
    }

    /// The `p`-th percentile latency (microseconds) using the nearest-rank method,
    /// or `None` if no frames were recorded. `p` is clamped to `0.0..=100.0`.
    ///
    /// Nearest-rank: rank = ceil(p/100 * n), 1-indexed into the sorted samples (p == 0
    /// maps to the smallest sample). Useful for tail latency, e.g. `percentile(95.0)`.
    pub fn percentile(&self, p: f64) -> Option<u64> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let n = sorted.len();
        let p = p.clamp(0.0, 100.0);
        // Nearest-rank, 1-indexed: rank = ceil(p/100 * n), with p == 0 -> rank 1.
        let rank = (p / 100.0 * n as f64).ceil() as usize;
        let idx = rank.saturating_sub(1).min(n - 1);
        Some(sorted[idx])
    }

    /// 50th-percentile (median) latency. Convenience for [`percentile`](Self::percentile).
    pub fn p50(&self) -> Option<u64> {
        self.percentile(50.0)
    }

    /// 95th-percentile latency — the headline tail metric for the latency budget.
    pub fn p95(&self) -> Option<u64> {
        self.percentile(95.0)
    }

    /// 99th-percentile latency.
    pub fn p99(&self) -> Option<u64> {
        self.percentile(99.0)
    }
}

/// Summary of a completed encode session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    /// Number of frames captured and encoded.
    pub frames: u64,
    /// Total Annex-B bytes written to the sink.
    pub bytes_written: usize,
    /// Codec config (SPS/PPS) extracted from the stream, once available. `None` if
    /// no keyframe carrying parameter sets was seen.
    pub codec_config: Option<CodecConfig>,
    /// Per-frame encode-latency stats.
    pub latency: LatencyStats,
}

/// Drive a capture→encode→sink session for up to `max_frames` frames.
///
/// Pulls frames from `capturer`, encodes each via `encoder`, writes the Annex-B
/// output to `sink` in order, records encode latency, and captures the codec config
/// (SPS/PPS) from the first **keyframe** (parameter sets live on keyframes; a P-slice
/// is never scanned). This is the platform-agnostic core that the
/// VideoToolbox/ScreenCaptureKit adapters plug into (R3 seam).
pub fn run_session(
    capturer: &mut dyn Capturer,
    encoder: &mut dyn Encoder,
    sink: &mut dyn Write,
    max_frames: u64,
) -> io::Result<SessionSummary> {
    let mut summary = SessionSummary {
        frames: 0,
        bytes_written: 0,
        codec_config: None,
        latency: LatencyStats::new(),
    };

    while summary.frames < max_frames {
        let Some(frame) = capturer.next_frame() else {
            break;
        };
        let encoded = encoder.encode(&frame);

        sink.write_all(&encoded.annex_b)?;
        summary.bytes_written += encoded.annex_b.len();
        summary.latency.record(encoded.encode_micros);
        summary.frames += 1;

        // SPS/PPS ride on keyframes; only scan keyframe output (cheaper and avoids
        // misreading a P-slice that happens to contain parameter-set-like bytes).
        if summary.codec_config.is_none() && encoded.keyframe {
            summary.codec_config = nal::extract_codec_config(&encoded.annex_b);
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- LatencyStats ---------------------------------------------------------

    #[test]
    fn latency_empty_has_no_stats() {
        let s = LatencyStats::new();
        assert_eq!(s.count(), 0);
        assert_eq!(s.min(), None);
        assert_eq!(s.max(), None);
        assert_eq!(s.mean(), None);
    }

    #[test]
    fn latency_single_sample() {
        let mut s = LatencyStats::new();
        s.record(40);
        assert_eq!(s.count(), 1);
        assert_eq!(s.min(), Some(40));
        assert_eq!(s.max(), Some(40));
        assert_eq!(s.mean(), Some(40.0));
    }

    #[test]
    fn latency_tracks_min_max_mean() {
        let mut s = LatencyStats::new();
        for v in [30u64, 10, 50, 10] {
            s.record(v);
        }
        assert_eq!(s.count(), 4);
        assert_eq!(s.min(), Some(10));
        assert_eq!(s.max(), Some(50));
        assert_eq!(s.mean(), Some(25.0)); // (30+10+50+10)/4
    }

    #[test]
    fn latency_empty_has_no_percentiles() {
        let s = LatencyStats::new();
        assert_eq!(s.percentile(50.0), None);
        assert_eq!(s.p95(), None);
    }

    #[test]
    fn latency_reports_tail_percentiles() {
        // Samples 10..=100 (n=10). Nearest-rank: rank = ceil(p/100 * n), 1-indexed
        // into the ascending samples. Recorded out of order to prove sorting.
        let mut s = LatencyStats::new();
        for v in [50u64, 10, 100, 30, 20, 80, 40, 90, 60, 70] {
            s.record(v);
        }
        assert_eq!(s.p50(), Some(50)); // ceil(0.50*10)=5 -> 5th = 50
        assert_eq!(s.p95(), Some(100)); // ceil(0.95*10)=10 -> 10th = 100
        assert_eq!(s.p99(), Some(100)); // ceil(0.99*10)=10 -> 10th = 100
        assert_eq!(s.percentile(0.0), Some(10)); // smallest
        assert_eq!(s.percentile(100.0), Some(100)); // largest
                                                    // The tail diverges from the mean — the whole point of tracking it.
        assert_eq!(s.mean(), Some(55.0));
    }

    // --- run_session ----------------------------------------------------------

    /// Yields `n` frames at 60 fps spacing, then stops.
    struct FakeCapturer {
        remaining: u64,
        pts: u64,
    }
    impl Capturer for FakeCapturer {
        fn next_frame(&mut self) -> Option<CapturedFrame> {
            if self.remaining == 0 {
                return None;
            }
            self.remaining -= 1;
            let f = CapturedFrame {
                pts_us: self.pts,
                width: 2400,
                height: 1080,
            };
            self.pts += 16_666;
            Some(f)
        }
    }

    /// Emits a keyframe (SPS+PPS+IDR) on the first frame, plain slices after, with a
    /// fixed per-frame latency. Lets the pipeline be tested deterministically.
    struct FakeEncoder {
        frame_index: u64,
    }
    impl Encoder for FakeEncoder {
        fn encode(&mut self, frame: &CapturedFrame) -> EncodedFrame {
            let keyframe = self.frame_index == 0;
            let mut annex_b = Vec::new();
            if keyframe {
                annex_b.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x1F]); // SPS
                annex_b.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE]); // PPS
                annex_b.extend_from_slice(&[0, 0, 0, 1, 0x65, 0xAA]); // IDR
            } else {
                annex_b.extend_from_slice(&[0, 0, 0, 1, 0x61, 0xBB]); // non-IDR
            }
            self.frame_index += 1;
            EncodedFrame {
                pts_us: frame.pts_us,
                keyframe,
                encode_micros: 5,
                annex_b,
            }
        }
    }

    #[test]
    fn session_encodes_all_frames_and_writes_stream() {
        let mut cap = FakeCapturer {
            remaining: 3,
            pts: 0,
        };
        let mut enc = FakeEncoder { frame_index: 0 };
        let mut sink: Vec<u8> = Vec::new();

        let summary = run_session(&mut cap, &mut enc, &mut sink, 10).unwrap();

        assert_eq!(summary.frames, 3);
        assert_eq!(summary.bytes_written, sink.len());
        // The written stream should split back into 5 NAL units: SPS, PPS, IDR, then
        // two non-IDR slices.
        let units: Vec<&[u8]> = nal::iter_nal_units(&sink).collect();
        assert_eq!(units.len(), 5);
    }

    #[test]
    fn session_extracts_codec_config_from_keyframe() {
        let mut cap = FakeCapturer {
            remaining: 2,
            pts: 0,
        };
        let mut enc = FakeEncoder { frame_index: 0 };
        let mut sink: Vec<u8> = Vec::new();

        let summary = run_session(&mut cap, &mut enc, &mut sink, 10).unwrap();

        let cfg = summary.codec_config.expect("config extracted");
        assert_eq!(cfg.sps, vec![0x67, 0x42, 0x1F]);
        assert_eq!(cfg.pps, vec![0x68, 0xCE]);
    }

    /// Frame 0 is a NON-keyframe whose bytes nonetheless contain SPS+PPS; frame 1 is a
    /// real keyframe. Used to prove the pipeline keys codec-config extraction off the
    /// `keyframe` flag, not off whatever parameter sets happen to appear in a slice.
    struct NonKeyframeCarriesParamsEncoder {
        frame_index: u64,
    }
    impl Encoder for NonKeyframeCarriesParamsEncoder {
        fn encode(&mut self, frame: &CapturedFrame) -> EncodedFrame {
            let keyframe = self.frame_index >= 1;
            let mut annex_b = Vec::new();
            annex_b.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x01]); // SPS
            annex_b.extend_from_slice(&[0, 0, 0, 1, 0x68, 0x02]); // PPS
            annex_b.extend_from_slice(&[0, 0, 0, 1, if keyframe { 0x65 } else { 0x61 }, 0x00]);
            self.frame_index += 1;
            EncodedFrame {
                pts_us: frame.pts_us,
                keyframe,
                encode_micros: 1,
                annex_b,
            }
        }
    }

    #[test]
    fn session_extracts_config_only_from_keyframe_flag() {
        // Run just the first (non-keyframe) frame: even though its bytes carry SPS/PPS,
        // codec_config must stay None because the frame is not flagged as a keyframe.
        let mut cap = FakeCapturer {
            remaining: 1,
            pts: 0,
        };
        let mut enc = NonKeyframeCarriesParamsEncoder { frame_index: 0 };
        let mut sink: Vec<u8> = Vec::new();
        let summary = run_session(&mut cap, &mut enc, &mut sink, 1).unwrap();
        assert_eq!(summary.codec_config, None);

        // Run through the keyframe (frame 1): now config is captured.
        let mut cap = FakeCapturer {
            remaining: 2,
            pts: 0,
        };
        let mut enc = NonKeyframeCarriesParamsEncoder { frame_index: 0 };
        let mut sink: Vec<u8> = Vec::new();
        let summary = run_session(&mut cap, &mut enc, &mut sink, 2).unwrap();
        let cfg = summary.codec_config.expect("config from keyframe");
        assert_eq!(cfg.sps, vec![0x67, 0x01]);
        assert_eq!(cfg.pps, vec![0x68, 0x02]);
    }

    #[test]
    fn session_records_latency_per_frame() {
        let mut cap = FakeCapturer {
            remaining: 4,
            pts: 0,
        };
        let mut enc = FakeEncoder { frame_index: 0 };
        let mut sink: Vec<u8> = Vec::new();

        let summary = run_session(&mut cap, &mut enc, &mut sink, 10).unwrap();

        assert_eq!(summary.latency.count(), 4);
        assert_eq!(summary.latency.mean(), Some(5.0));
    }

    #[test]
    fn session_stops_at_max_frames() {
        let mut cap = FakeCapturer {
            remaining: 100,
            pts: 0,
        };
        let mut enc = FakeEncoder { frame_index: 0 };
        let mut sink: Vec<u8> = Vec::new();

        let summary = run_session(&mut cap, &mut enc, &mut sink, 5).unwrap();

        assert_eq!(summary.frames, 5);
    }

    #[test]
    fn session_handles_capturer_ending_before_max() {
        let mut cap = FakeCapturer {
            remaining: 2,
            pts: 0,
        };
        let mut enc = FakeEncoder { frame_index: 0 };
        let mut sink: Vec<u8> = Vec::new();

        let summary = run_session(&mut cap, &mut enc, &mut sink, 50).unwrap();

        assert_eq!(summary.frames, 2);
    }
}
