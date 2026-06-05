//! Client receive-session orchestrator (P5 PIPE-01 criterion #3 — receive-side wiring).
//!
//! This module implements the **client side** of the live pipeline: a platform-agnostic
//! receive loop that reads protocol [`Frame`]s off any `Read + Write` transport, performs
//! the client half of the handshake, and routes frames into the existing
//! [`DecodeSession`] / [`VideoDecoder`] port.
//!
//! ## Handshake
//! On connection the host sends a [`Frame::Handshake`] offer; the client reads it,
//! validates it via [`negotiate`], and replies with a [`Frame::Handshake`] carrying the
//! client's [`ClientCaps`] (wrapped as a `Handshake` — the client-to-host direction reuses
//! the same frame variant so no new tag is needed). The host calls [`negotiate`] on its
//! side; both ends arrive at the same [`AgreedConfig`] independently (pure function, same
//! inputs). A negotiation failure aborts the session with [`SessionError::Negotiation`].
//!
//! ## Receive loop
//! After the handshake the loop reads frames until the peer closes the connection (clean
//! EOF) or a [`Control::Bye`] / [`Control::Pause`] / [`Control::Resume`] is received:
//! - `VideoConfig` / `Video` → forwarded to [`DecodeSession::feed`].
//! - `Control::Bye` → clean shutdown; loop exits.
//! - Other `Control` variants → logged / acknowledged; loop continues (the transport-only
//!   client has no way to honour `RequestKeyframe` itself — the session layer above would
//!   write one back; here we simply continue).
//! - `Handshake` / `Touch` / `ClockPing` / `ClockPong` / `Stats` → not part of the inbound
//!   decode path; silently ignored here (the live decode loop answers `ClockPing` and emits
//!   `Stats` via [`StatsTracker`] / [`pong_for_ping`]).
//! - Clean EOF (peer closed at a frame boundary) → loop exits; NOT an error.
//!
//! ## Returns
//! [`run_session`] returns a [`SessionSummary`] on success.
//!
//! ## No hardware / no new dependencies
//! Everything here is pure Rust (`std::io`, `protocol`, the existing `decode` port).
//! There is no `ndk`, no JNI, no `cfg(target_os = "android")` gate — host CI exercises
//! this in full.

use std::collections::VecDeque;
use std::io::{self, Read, Write};

use protocol::messages::{
    AgreedConfig, ClientCaps, Control, Frame, Handshake, MessageError, NegotiationError,
};

use crate::decode::{DecodeError, DecodeSession, VideoDecoder};
use crate::pacing::InputPacer;

// Re-export negotiate so callers that need it don't have to reach into protocol directly.
use protocol::messages::negotiate;

/// Phone-side per-frame timing, keyed by `pts_us`, accumulated as a frame moves through
/// arrive → decode → present. On `present` it produces the `Frame::Stats` to send to the
/// host. Bounded FIFO so a frame that never presents (dropped by the decoder) cannot leak.
pub struct StatsTracker {
    inflight: VecDeque<(u64, u64, Option<u64>)>, // (pts_us, arrive_us, decode_us)
    capacity: usize,
}

impl StatsTracker {
    /// New tracker retaining at most `capacity` in-flight frames.
    pub fn new(capacity: usize) -> Self {
        Self {
            inflight: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    /// Record arrival of the full access unit for `pts_us` (phone clock, µs).
    pub fn on_arrive(&mut self, pts_us: u64, arrive_us: u64) {
        if self.inflight.len() >= self.capacity {
            self.inflight.pop_front();
        }
        self.inflight.push_back((pts_us, arrive_us, None));
    }

    /// Record decode completion for `pts_us`.
    pub fn on_decode(&mut self, pts_us: u64, decode_us: u64) {
        if let Some(e) = self.inflight.iter_mut().find(|(p, ..)| *p == pts_us) {
            e.2 = Some(decode_us);
        }
    }

    /// Record present for `pts_us` and, if arrive+decode were seen, produce the `Frame::Stats`.
    pub fn on_present(&mut self, pts_us: u64, present_us: u64) -> Option<Frame> {
        let idx = self.inflight.iter().position(|(p, ..)| *p == pts_us)?;
        let (_, arrive_us, decode) = self.inflight.remove(idx)?;
        let decode_us = decode?;
        Some(Frame::Stats {
            pts_us,
            arrive_us,
            decode_us,
            present_us,
        })
    }
}

/// If `frame` is a `ClockPing`, build the matching `ClockPong` from the client's receive
/// time `t1_us` and send time `t2_us` (client clock, µs). Otherwise `None`.
pub fn pong_for_ping(frame: Frame, t1_us: u64, t2_us: u64) -> Option<Frame> {
    match frame {
        Frame::ClockPing { t0_us } => Some(Frame::ClockPong {
            t0_us,
            t1_us,
            t2_us,
        }),
        _ => None,
    }
}

/// Why the client session failed.
#[derive(Debug)]
pub enum SessionError {
    /// An I/O error on the underlying transport (not a clean frame-boundary EOF).
    Io(io::Error),
    /// A protocol framing or message decoding error.
    Message(MessageError),
    /// The handshake offer could not be reconciled with the client's capabilities.
    Negotiation(NegotiationError),
    /// The decoder returned an error while processing a frame.
    Decode(DecodeError),
}

impl core::fmt::Display for SessionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SessionError::Io(e) => write!(f, "transport I/O error: {e}"),
            SessionError::Message(e) => write!(f, "protocol message error: {e}"),
            SessionError::Negotiation(e) => write!(f, "handshake negotiation failed: {e:?}"),
            SessionError::Decode(e) => write!(f, "decode error: {e}"),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::Io(e) => Some(e),
            SessionError::Message(e) => Some(e),
            SessionError::Decode(e) => Some(e),
            SessionError::Negotiation(_) => None,
        }
    }
}

impl From<io::Error> for SessionError {
    fn from(e: io::Error) -> Self {
        SessionError::Io(e)
    }
}

impl From<MessageError> for SessionError {
    fn from(e: MessageError) -> Self {
        // A clean EOF at a frame boundary from Frame::read_from surfaces as an
        // MessageError::Io wrapping an UnexpectedEof. The receive loop handles the
        // frame-boundary EOF case itself (see `is_clean_eof`); this From is for
        // unexpected mid-stream I/O errors in other contexts.
        SessionError::Message(e)
    }
}

impl From<NegotiationError> for SessionError {
    fn from(e: NegotiationError) -> Self {
        SessionError::Negotiation(e)
    }
}

impl From<DecodeError> for SessionError {
    fn from(e: DecodeError) -> Self {
        SessionError::Decode(e)
    }
}

/// Summary returned by a successfully completed session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// Total number of frames received from the transport (all variants).
    pub frames_received: u64,
    /// Number of access units successfully submitted to the decoder.
    pub decoded_count: u64,
    /// Number of keyframes submitted.
    pub keyframe_count: u64,
    /// Whether the decoder was configured at least once.
    pub configured: bool,
    /// Number of Video frames dropped on the input side by the pacer (drop-to-keyframe under
    /// in-flight depth pressure) — the latency-item-A shed-load counter.
    pub input_frames_dropped: u64,
    /// The negotiated streaming configuration (present when the handshake succeeded).
    pub agreed: Option<AgreedConfig>,
}

/// Returns `true` when `e` is an end-of-stream signalled by [`io::ErrorKind::UnexpectedEof`].
///
/// [`protocol::framing::read_frame`] calls `read_exact` on the length header and then on the
/// payload; either hitting EOF surfaces as [`io::ErrorKind::UnexpectedEof`]. We treat that as
/// a clean shutdown (the peer closed the connection), matching `echo_loop`'s convention.
///
/// Note: at this layer EOF on the header (a true frame boundary) is **indistinguishable** from
/// EOF mid-payload (a truncated final frame) — both are `UnexpectedEof`. A truncated trailing
/// frame is therefore treated as a clean close rather than a stream error. This matches the
/// `echo_loop` tradeoff and is acceptable for the MVP; a length-then-CRC or explicit `Bye` (the
/// graceful path) distinguishes the two when it matters.
fn is_clean_eof(e: &MessageError) -> bool {
    match e {
        MessageError::Io(io_err) => io_err.kind() == io::ErrorKind::UnexpectedEof,
        _ => false,
    }
}

/// Bound on in-flight frames the [`StatsTracker`] retains. The host pipeline keeps only a
/// handful of frames between arrive and present; this generous cap means a frame the decoder
/// drops (never presents) cannot leak, while no realistic depth is ever evicted early.
const STATS_TRACKER_CAPACITY: usize = 64;

/// Max decoder in-flight input depth before the pacer drops forward to the next keyframe.
/// Small (Moonlight uses 2) so latency stays at a couple of frames. See
/// `docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md`.
const MAX_IN_FLIGHT: usize = 2;

/// Perform the client half of the protocol handshake and enter the receive loop, using a
/// monotonic phone clock for latency stamping.
///
/// 1. Reads the host's [`Frame::Handshake`] offer from `transport`.
/// 2. Calls [`negotiate`] to reconcile it against `client_caps`.
/// 3. Writes back the client's capabilities as a [`Frame::Handshake`] (reusing the
///    `Handshake` variant; the host ignores unknown-direction frames in current protocol).
/// 4. Enters the receive loop, routing frames to `decode_session` / `decoder`, answering the
///    host's [`Frame::ClockPing`] with a [`Frame::ClockPong`], and sending a per-frame
///    [`Frame::Stats`] back as each decoded frame is presented (Task 7), until the peer
///    closes the connection or sends `Control::Bye`.
///
/// Returns [`SessionSummary`] on clean exit; [`SessionError`] on any unrecoverable error.
///
/// The clock is the process-global [`crate::now_us`] (a single `OnceLock<Instant>` epoch,
/// monotonic on Android, backed by `CLOCK_MONOTONIC`). This ensures that `arrive_us`,
/// `ClockPong` t1/t2, and the adapter's `decode_us`/`present_us` all share one epoch so
/// the host's clock-sync offset converts them correctly.
/// [`run_session_with_clock`] takes an injectable clock so the latency orchestration is
/// deterministically testable without a device.
pub fn run_session<T: Read + Write>(
    transport: &mut T,
    client_caps: &ClientCaps,
    decode_session: &mut DecodeSession,
    decoder: &mut dyn VideoDecoder,
) -> Result<SessionSummary, SessionError> {
    run_session_with_clock(
        transport,
        client_caps,
        decode_session,
        decoder,
        crate::now_us,
    )
}

/// [`run_session`] with an injectable monotonic clock (`now_us`), for deterministic tests.
///
/// `now_us` must be monotonic non-decreasing — the receive loop calls it to stamp frame
/// arrival and to fill the `ClockPong` receive/send times. Production uses an
/// [`Instant`]-based closure; tests inject a fake counter so emitted `Stats`/`ClockPong`
/// timestamps are exact.
pub fn run_session_with_clock<T: Read + Write>(
    transport: &mut T,
    client_caps: &ClientCaps,
    decode_session: &mut DecodeSession,
    decoder: &mut dyn VideoDecoder,
    mut now_us: impl FnMut() -> u64,
) -> Result<SessionSummary, SessionError> {
    // --- Step 1: read host handshake offer -----------------------------------
    let host_handshake = match Frame::read_from(transport) {
        Ok(Frame::Handshake(h)) => h,
        Ok(other) => {
            // Peer sent something that is not a Handshake first — treat it as a
            // protocol error by surfacing an IoError.
            return Err(SessionError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected Handshake as first frame, got {:?}", other),
            )));
        }
        Err(e) => return Err(SessionError::from(e)),
    };

    // --- Step 2: negotiate ---------------------------------------------------
    let agreed = negotiate(&host_handshake, client_caps).map_err(SessionError::Negotiation)?;

    // --- Step 3: reply with our caps -----------------------------------------
    // The client sends its capabilities back as a Handshake frame. The host reads
    // this and calls negotiate() on its side to reach the same AgreedConfig.
    let reply = Frame::Handshake(Handshake {
        protocol_version: client_caps.protocol_version,
        width: client_caps.max_width,
        height: client_caps.max_height,
        refresh_hz: client_caps.max_refresh_hz,
        codecs: client_caps.codecs.clone(),
    });
    reply.write_to(transport).map_err(SessionError::from)?;
    // Flush so the caps reply reaches the host before we block reading the stream below.
    // `run_session` is generic over any `Read + Write`; a buffered transport (e.g. a
    // `BufWriter`-wrapped NCM/TCP fallback) would otherwise hold the reply in its buffer and
    // deadlock the host — which blocks reading our caps before it starts streaming. The live
    // `AccessoryFdTransport` is unbuffered (a raw `File`) so this is a no-op there, but it
    // honours the generic contract and mirrors the P1 connect-hello flush.
    transport.flush().map_err(SessionError::from)?;

    // --- Step 4: receive loop ------------------------------------------------
    let mut frames_received: u64 = 0;
    // Per-frame latency tracker: arrive (here) + decode/present (reported up from the
    // decoder) build the `Frame::Stats` sent back to the host (Task 7).
    let mut stats = StatsTracker::new(STATS_TRACKER_CAPACITY);
    let mut pacer = InputPacer::new(MAX_IN_FLIGHT);
    let mut input_frames_dropped: u64 = 0;

    loop {
        let frame = match Frame::read_from(transport) {
            Ok(f) => f,
            Err(e) if is_clean_eof(&e) => {
                // Peer closed at a frame boundary — clean shutdown.
                break;
            }
            Err(e) => return Err(SessionError::from(e)),
        };

        frames_received += 1;

        match &frame {
            Frame::Video {
                pts_us, keyframe, ..
            } => {
                // Input-side pacing (latency item A): admit only while the decoder's in-flight
                // depth is under the cap; otherwise drop forward to the next keyframe but still
                // pump the decoder so its output/surface stay current.
                let presented = if pacer.admit(*keyframe, decoder.in_flight()) {
                    // Stamp arrival ONLY for admitted frames: a dropped frame never decodes or
                    // presents, so it must not enter the bounded StatsTracker FIFO (where its
                    // orphan record could evict an admitted frame's record under burst drops).
                    stats.on_arrive(*pts_us, now_us());
                    decode_session
                        .feed(&frame, decoder)
                        .map_err(SessionError::Decode)?
                } else {
                    input_frames_dropped += 1;
                    decoder.pump().map_err(SessionError::Decode)?
                };
                // Each presented output frame completes a per-frame record; emit its Stats.
                let mut wrote_stats = false;
                for pf in presented {
                    stats.on_decode(pf.pts_us, pf.decode_us);
                    if let Some(stats_frame) = stats.on_present(pf.pts_us, pf.present_us) {
                        stats_frame
                            .write_to(transport)
                            .map_err(SessionError::from)?;
                        wrote_stats = true;
                    }
                }
                // Flush so the host's stats-reader thread sees these promptly even behind a
                // buffered transport (the live `AccessoryFdTransport` is an unbuffered File,
                // so this is a no-op there).
                if wrote_stats {
                    transport.flush().map_err(SessionError::from)?;
                }
            }
            Frame::VideoConfig { .. } => {
                // Config drives configure() but never presents a frame; ignore the (empty)
                // presented list.
                decode_session
                    .feed(&frame, decoder)
                    .map_err(SessionError::Decode)?;
            }
            Frame::ClockPing { .. } => {
                // Answer promptly: the host blocks ~2s waiting for the pong. Stamp receive
                // (t1) and send (t2) on the same phone clock.
                let t1 = now_us();
                if let Some(pong) = pong_for_ping(frame.clone(), t1, now_us()) {
                    pong.write_to(transport).map_err(SessionError::from)?;
                    // Flush immediately: the host blocks ~2s on this pong, and a buffered
                    // transport would otherwise hold it past the deadline.
                    transport.flush().map_err(SessionError::from)?;
                }
            }
            Frame::Control(Control::Bye) => {
                // Graceful disconnect requested; exit the loop cleanly.
                break;
            }
            Frame::Control(_) => {
                // RequestKeyframe / Pause / Resume — the session-layer above handles
                // these; here we just continue reading (no write-back at this layer).
            }
            // Handshake mid-stream (unexpected), Touch, an echoed ClockPong, inbound
            // Stats, and Scroll (a client→host input frame, never received here) are not
            // part of the client read path; silently ignore here.
            Frame::Handshake(_)
            | Frame::Touch(_)
            | Frame::ClockPong { .. }
            | Frame::Stats { .. }
            | Frame::Scroll { .. } => {}
        }
    }

    Ok(SessionSummary {
        frames_received,
        decoded_count: decode_session.decoded_count(),
        keyframe_count: decode_session.keyframe_count(),
        configured: decode_session.configured(),
        input_frames_dropped,
        agreed: Some(agreed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::messages::{Handshake, VideoCodec};
    use std::collections::VecDeque;
    use std::io;

    // -------------------------------------------------------------------------
    // Test fakes
    // -------------------------------------------------------------------------

    /// Recorded call on the fake decoder.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum DecoderCall {
        Configure(VideoCodec, protocol::nal::CodecConfig),
        Decode(crate::decode::DecoderInput),
        Pump,
    }

    /// Fake [`VideoDecoder`] that records every call and returns scripted present timing so
    /// the latency orchestration (arrive→decode→present → `Frame::Stats`) is provable with
    /// no device.
    #[derive(Debug, Default)]
    struct RecordingDecoder {
        calls: std::cell::RefCell<Vec<DecoderCall>>,
        /// If `Some(idx)`, the `idx`-th `decode` call returns an Adapter error.
        fail_on_decode_index: Option<u64>,
        decode_index: u64,
        /// Scripted `PresentedFrame`s each `decode` call returns, popped front-to-back.
        present_script: VecDeque<Vec<crate::decode::PresentedFrame>>,
        /// Depth returned by successive `in_flight()` calls, popped front-to-back; 0 when empty.
        in_flight_script: std::cell::RefCell<VecDeque<usize>>,
    }

    impl VideoDecoder for RecordingDecoder {
        fn configure(
            &mut self,
            codec: VideoCodec,
            config: &protocol::nal::CodecConfig,
        ) -> Result<(), DecodeError> {
            self.calls
                .borrow_mut()
                .push(DecoderCall::Configure(codec, config.clone()));
            Ok(())
        }

        fn decode(
            &mut self,
            input: &crate::decode::DecoderInput,
        ) -> Result<Vec<crate::decode::PresentedFrame>, DecodeError> {
            let idx = self.decode_index;
            self.decode_index += 1;
            if self.fail_on_decode_index == Some(idx) {
                return Err(DecodeError::Adapter("simulated codec failure".into()));
            }
            self.calls
                .borrow_mut()
                .push(DecoderCall::Decode(input.clone()));
            Ok(self.present_script.pop_front().unwrap_or_default())
        }

        fn in_flight(&self) -> usize {
            self.in_flight_script.borrow_mut().pop_front().unwrap_or(0)
        }

        fn pump(&mut self) -> Result<Vec<crate::decode::PresentedFrame>, DecodeError> {
            self.calls.borrow_mut().push(DecoderCall::Pump);
            Ok(Vec::new())
        }
    }

    /// In-memory duplex transport: pre-loaded read queue + write sink.
    struct MemTransport {
        read_queue: VecDeque<u8>,
        pub write_sink: Vec<u8>,
    }

    impl MemTransport {
        fn new(data: Vec<u8>) -> Self {
            Self {
                read_queue: data.into_iter().collect(),
                write_sink: Vec::new(),
            }
        }
    }

    impl io::Read for MemTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.read_queue.len());
            for slot in buf.iter_mut().take(n) {
                *slot = self.read_queue.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    impl io::Write for MemTransport {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.write_sink.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // -------------------------------------------------------------------------
    // NAL helpers (mirroring decode.rs test helpers)
    // -------------------------------------------------------------------------

    const SC4: [u8; 4] = [0, 0, 0, 1];

    fn sps_pps_bytes() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&SC4);
        v.extend_from_slice(&[0x67, 0x42, 0x1F]); // SPS
        v.extend_from_slice(&SC4);
        v.extend_from_slice(&[0x68, 0xCE]); // PPS
        v
    }

    fn bare_keyframe_nal() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&SC4);
        v.extend_from_slice(&[0x65, 0xAA, 0xBB]); // IDR
        v
    }

    fn delta_nal() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&SC4);
        v.extend_from_slice(&[0x61, 0xCC]); // non-IDR slice
        v
    }

    // -------------------------------------------------------------------------
    // Handshake helpers
    // -------------------------------------------------------------------------

    fn default_host_handshake() -> Handshake {
        Handshake {
            protocol_version: protocol::protocol_version(),
            width: 2400,
            height: 1080,
            refresh_hz: 60,
            codecs: vec![VideoCodec::H264],
        }
    }

    fn default_client_caps() -> ClientCaps {
        ClientCaps {
            protocol_version: protocol::protocol_version(),
            max_width: 2400,
            max_height: 1080,
            max_refresh_hz: 60,
            codecs: vec![VideoCodec::H264],
        }
    }

    /// Build a pre-encoded `MemTransport` starting with a host `Handshake` offer followed
    /// by `extra_frames`. Callers supply additional frames to test different scenarios.
    fn transport_with_frames(extra_frames: &[Frame]) -> MemTransport {
        let mut buf = Vec::new();
        Frame::Handshake(default_host_handshake())
            .write_to(&mut buf)
            .unwrap();
        for f in extra_frames {
            f.write_to(&mut buf).unwrap();
        }
        MemTransport::new(buf)
    }

    // -------------------------------------------------------------------------
    // Tests
    // -------------------------------------------------------------------------

    #[test]
    fn handshake_only_clean_eof_returns_empty_summary() {
        // Host sends handshake, then closes the connection. Session should complete
        // cleanly with zero frames decoded.
        let mut t = transport_with_frames(&[]);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        assert_eq!(summary.frames_received, 0);
        assert_eq!(summary.decoded_count, 0);
        assert_eq!(summary.keyframe_count, 0);
        assert!(!summary.configured);
        assert!(summary.agreed.is_some());
        // Client must have written back its caps as a Handshake frame.
        assert!(!t.write_sink.is_empty());
    }

    #[test]
    fn video_config_then_video_frames_drive_decoder() {
        // VideoConfig + keyframe + two deltas → configure called once, decode called 3×.
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Video {
                pts_us: 0,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
            Frame::Video {
                pts_us: 16_666,
                keyframe: false,
                nal: delta_nal(),
            },
            Frame::Video {
                pts_us: 33_333,
                keyframe: false,
                nal: delta_nal(),
            },
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        assert_eq!(summary.frames_received, 4);
        assert_eq!(summary.decoded_count, 3);
        assert_eq!(summary.keyframe_count, 1);
        assert!(summary.configured);

        let configure_count = dec
            .calls
            .borrow()
            .iter()
            .filter(|c| matches!(c, DecoderCall::Configure(..)))
            .count();
        assert_eq!(configure_count, 1, "configure called exactly once");
    }

    #[test]
    fn in_band_keyframe_without_video_config_still_decodes() {
        // A self-describing keyframe (SPS+PPS+IDR in-band) configures the decoder even
        // when no VideoConfig was sent first.
        let mut nal = Vec::new();
        nal.extend_from_slice(&SC4);
        nal.extend_from_slice(&[0x67, 0x01]); // SPS
        nal.extend_from_slice(&SC4);
        nal.extend_from_slice(&[0x68, 0x01]); // PPS
        nal.extend_from_slice(&SC4);
        nal.extend_from_slice(&[0x65, 0x00]); // IDR

        let frames = [Frame::Video {
            pts_us: 42,
            keyframe: true,
            nal,
        }];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        assert!(summary.configured);
        assert_eq!(summary.decoded_count, 1);
        assert_eq!(summary.keyframe_count, 1);
    }

    #[test]
    fn video_before_config_surfaces_no_config_error() {
        // A delta frame arriving before any config must surface as
        // SessionError::Decode(DecodeError::NoConfig).
        let frames = [Frame::Video {
            pts_us: 0,
            keyframe: false,
            nal: delta_nal(),
        }];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        match run_session(&mut t, &caps, &mut ds, &mut dec) {
            Err(SessionError::Decode(DecodeError::NoConfig)) => {}
            other => panic!("expected SessionError::Decode(NoConfig), got {other:?}"),
        }
    }

    #[test]
    fn decoder_adapter_error_surfaces_as_session_decode_error() {
        // A mid-stream decoder failure (the Wave-B AMediaCodec adapter returning a status
        // code) must propagate as SessionError::Decode(DecodeError::Adapter(..)) — not be
        // swallowed. Configure succeeds (via VideoConfig), then the first decode() fails.
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Video {
                pts_us: 0,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        // Fail the very first decode() call.
        let mut dec = RecordingDecoder {
            fail_on_decode_index: Some(0),
            ..RecordingDecoder::default()
        };
        let caps = default_client_caps();

        match run_session(&mut t, &caps, &mut ds, &mut dec) {
            Err(SessionError::Decode(DecodeError::Adapter(msg))) => {
                assert!(
                    msg.contains("simulated codec failure"),
                    "unexpected adapter message: {msg}"
                );
            }
            other => panic!("expected SessionError::Decode(Adapter), got {other:?}"),
        }
    }

    #[test]
    fn control_bye_ends_loop_cleanly() {
        // VideoConfig + keyframe + Bye → loop exits after Bye, NOT an error.
        // Bye itself is not counted in frames_received (counted before the break).
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Video {
                pts_us: 0,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
            Frame::Control(Control::Bye),
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        // 3 frames received (VideoConfig + Video + Control::Bye all counted before break).
        assert_eq!(summary.frames_received, 3);
        assert_eq!(summary.decoded_count, 1);
        assert_eq!(summary.keyframe_count, 1);
        assert!(summary.configured);
    }

    #[test]
    fn clean_eof_ends_loop_with_correct_summary() {
        // Full sequence: config, keyframe, 2 deltas, then EOF. Summary must match exactly.
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Video {
                pts_us: 0,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
            Frame::Video {
                pts_us: 16_666,
                keyframe: false,
                nal: delta_nal(),
            },
            Frame::Video {
                pts_us: 33_333,
                keyframe: false,
                nal: delta_nal(),
            },
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        assert_eq!(summary.frames_received, 4);
        assert_eq!(summary.decoded_count, 3);
        assert_eq!(summary.keyframe_count, 1);
        assert!(summary.configured);
        assert_eq!(summary.input_frames_dropped, 0);
        // Agreed config must reflect the host offer.
        let agreed = summary.agreed.unwrap();
        assert_eq!(agreed.width, 2400);
        assert_eq!(agreed.height, 1080);
        assert_eq!(agreed.codec, VideoCodec::H264);
    }

    #[test]
    fn negotiation_failure_aborts_session() {
        // Host offers a protocol version the client doesn't match — negotiation fails.
        let mut buf = Vec::new();
        Frame::Handshake(Handshake {
            protocol_version: 999, // incompatible
            width: 2400,
            height: 1080,
            refresh_hz: 60,
            codecs: vec![VideoCodec::H264],
        })
        .write_to(&mut buf)
        .unwrap();
        let mut t = MemTransport::new(buf);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        match run_session(&mut t, &caps, &mut ds, &mut dec) {
            Err(SessionError::Negotiation(_)) => {}
            other => panic!("expected Negotiation error, got {other:?}"),
        }
    }

    #[test]
    fn non_bye_control_frames_do_not_stop_loop() {
        // RequestKeyframe / Pause / Resume must not terminate the loop.
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Control(Control::RequestKeyframe),
            Frame::Control(Control::Pause),
            Frame::Control(Control::Resume),
            Frame::Video {
                pts_us: 0,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        // 5 frames (VideoConfig + 3 Control + 1 Video), 1 decoded.
        assert_eq!(summary.frames_received, 5);
        assert_eq!(summary.decoded_count, 1);
    }

    #[test]
    fn keyframe_and_decoded_counts_match_decode_session() {
        // Five keyframes; counts in SessionSummary must equal decode_session's getters.
        let mut frames: Vec<Frame> = vec![Frame::VideoConfig {
            codec: VideoCodec::H264,
            sps_pps: sps_pps_bytes(),
        }];
        for i in 0..5u64 {
            frames.push(Frame::Video {
                pts_us: i * 16_666,
                keyframe: true,
                nal: bare_keyframe_nal(),
            });
        }

        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        assert_eq!(summary.decoded_count, ds.decoded_count());
        assert_eq!(summary.keyframe_count, ds.keyframe_count());
        assert_eq!(summary.configured, ds.configured());
        assert_eq!(summary.keyframe_count, 5);
        assert_eq!(summary.decoded_count, 5);
    }

    #[test]
    fn client_caps_reply_written_to_transport() {
        // After the handshake the client must have written a Frame::Handshake back.
        // Decode it from write_sink and verify the protocol_version matches.
        let mut t = transport_with_frames(&[]);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        let mut cursor = std::io::Cursor::new(&t.write_sink);
        let frame = Frame::read_from(&mut cursor).expect("client reply should be a valid frame");
        match frame {
            Frame::Handshake(h) => {
                assert_eq!(h.protocol_version, caps.protocol_version);
                assert_eq!(h.width, caps.max_width);
                assert_eq!(h.height, caps.max_height);
            }
            other => panic!("expected Handshake reply, got {other:?}"),
        }
    }

    #[test]
    fn stats_tracker_builds_stats_frame_for_pts() {
        let mut t = StatsTracker::new(8);
        t.on_arrive(1000, 50);
        t.on_decode(1000, 60);
        let frame = t
            .on_present(1000, 75)
            .expect("complete record yields a Stats frame");
        assert_eq!(
            frame,
            Frame::Stats {
                pts_us: 1000,
                arrive_us: 50,
                decode_us: 60,
                present_us: 75
            }
        );
    }

    #[test]
    fn present_without_arrive_yields_none() {
        let mut t = StatsTracker::new(8);
        assert!(t.on_present(2000, 75).is_none());
    }

    #[test]
    fn present_with_arrive_but_no_decode_yields_none() {
        // The other half of the "only emit when complete" rule: arrive seen, decode never
        // recorded → present must NOT produce a Stats frame (a partial record is dropped).
        let mut t = StatsTracker::new(8);
        t.on_arrive(3000, 50);
        assert!(t.on_present(3000, 75).is_none());
    }

    #[test]
    fn pong_for_ping_carries_receive_and_send_times() {
        let pong = pong_for_ping(Frame::ClockPing { t0_us: 7 }, 100, 105);
        assert_eq!(
            pong,
            Some(Frame::ClockPong {
                t0_us: 7,
                t1_us: 100,
                t2_us: 105
            })
        );
        assert!(pong_for_ping(Frame::Control(protocol::messages::Control::Bye), 1, 2).is_none());
    }

    // -------------------------------------------------------------------------
    // Task 7: live latency orchestration (ClockPing→Pong, per-frame Stats)
    // -------------------------------------------------------------------------

    /// A monotonic fake clock: each call returns the next value from a scripted sequence,
    /// repeating the last value once exhausted. Lets tests assert exact stamp values.
    fn scripted_clock(values: Vec<u64>) -> impl FnMut() -> u64 {
        let mut it = values.into_iter().peekable();
        let mut last = 0u64;
        move || {
            if let Some(v) = it.next() {
                last = v;
            }
            last
        }
    }

    fn read_back_frames(sink: &[u8]) -> Vec<Frame> {
        let mut cursor = std::io::Cursor::new(sink);
        let mut frames = Vec::new();
        while (cursor.position() as usize) < sink.len() {
            match Frame::read_from(&mut cursor) {
                Ok(f) => frames.push(f),
                Err(_) => break,
            }
        }
        frames
    }

    #[test]
    fn clock_ping_produces_clock_pong_with_phone_times() {
        // Host sends a ClockPing with t0; the client must reply with a ClockPong echoing t0
        // and carrying the phone's receive (t1) and send (t2) times from the injected clock.
        let frames = [Frame::ClockPing { t0_us: 12_345 }];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        // First clock call is t1 (receipt), second is t2 (send).
        let clock = scripted_clock(vec![1000, 1005]);
        run_session_with_clock(&mut t, &caps, &mut ds, &mut dec, clock).unwrap();

        // write_sink = client Handshake reply, then the ClockPong.
        let written = read_back_frames(&t.write_sink);
        let pong = written
            .iter()
            .find(|f| matches!(f, Frame::ClockPong { .. }))
            .expect("a ClockPong must be written back");
        assert_eq!(
            *pong,
            Frame::ClockPong {
                t0_us: 12_345,
                t1_us: 1000,
                t2_us: 1005,
            }
        );
    }

    #[test]
    fn presented_video_frame_emits_stats_with_correct_ordering() {
        // VideoConfig + keyframe whose decode yields a PresentedFrame → a Frame::Stats is
        // written back with the right pts and arrive ≤ decode ≤ present ordering.
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Video {
                pts_us: 9000,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder {
            present_script: VecDeque::from(vec![vec![crate::decode::PresentedFrame {
                pts_us: 9000,
                decode_us: 220,
                present_us: 240,
            }]]),
            ..RecordingDecoder::default()
        };
        let caps = default_client_caps();

        // Clock: only on_arrive consults it (one call, for the single Video frame).
        let clock = scripted_clock(vec![200]);
        run_session_with_clock(&mut t, &caps, &mut ds, &mut dec, clock).unwrap();

        let written = read_back_frames(&t.write_sink);
        let stats = written
            .iter()
            .find(|f| matches!(f, Frame::Stats { .. }))
            .expect("a Stats frame must be written back");
        match stats {
            Frame::Stats {
                pts_us,
                arrive_us,
                decode_us,
                present_us,
            } => {
                assert_eq!(*pts_us, 9000);
                assert_eq!(*arrive_us, 200);
                assert_eq!(*decode_us, 220);
                assert_eq!(*present_us, 240);
                assert!(arrive_us <= decode_us && decode_us <= present_us);
            }
            other => panic!("expected Stats, got {other:?}"),
        }
    }

    #[test]
    fn video_with_no_presented_frame_writes_no_stats() {
        // A Video whose decode yields no presented frame (decoder still buffering) must
        // write no Stats yet — only the Handshake reply is in the sink.
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Video {
                pts_us: 4242,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        // Empty present_script → decode returns an empty Vec (no frame presented).
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        let clock = scripted_clock(vec![200]);
        run_session_with_clock(&mut t, &caps, &mut ds, &mut dec, clock).unwrap();

        let written = read_back_frames(&t.write_sink);
        assert!(
            !written.iter().any(|f| matches!(f, Frame::Stats { .. })),
            "no Stats must be written when nothing presented"
        );
        // The decode still happened (frame was fed to the decoder).
        assert_eq!(ds.decoded_count(), 1);
    }

    #[test]
    fn missing_handshake_first_frame_is_error() {
        // If the host sends a Video frame before a Handshake the session must error, not panic.
        let mut buf = Vec::new();
        Frame::Video {
            pts_us: 0,
            keyframe: false,
            nal: delta_nal(),
        }
        .write_to(&mut buf)
        .unwrap();
        let mut t = MemTransport::new(buf);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder::default();
        let caps = default_client_caps();

        match run_session(&mut t, &caps, &mut ds, &mut dec) {
            Err(SessionError::Io(_)) => {}
            other => panic!("expected Io error for non-Handshake first frame, got {other:?}"),
        }
    }

    #[test]
    fn high_in_flight_delta_is_dropped_and_pumps_instead_of_decoding() {
        // VideoConfig + keyframe (admitted, depth 0) + delta reported at high depth (dropped).
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Video {
                pts_us: 0,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
            Frame::Video {
                pts_us: 16_666,
                keyframe: false,
                nal: delta_nal(),
            },
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        // in_flight() is consulted once per Video frame, before feeding. Keyframe sees depth 0
        // (admitted); the delta sees depth 99 (>= the cap) → dropped → pump() instead.
        let mut dec = RecordingDecoder {
            in_flight_script: std::cell::RefCell::new(VecDeque::from(vec![0usize, 99usize])),
            ..RecordingDecoder::default()
        };
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        assert_eq!(
            summary.input_frames_dropped, 1,
            "the high-depth delta was dropped"
        );
        // The keyframe was decoded; the delta was NOT decoded but pump() ran.
        let calls = dec.calls.borrow();
        let decodes = calls
            .iter()
            .filter(|c| matches!(c, DecoderCall::Decode(_)))
            .count();
        let pumps = calls
            .iter()
            .filter(|c| matches!(c, DecoderCall::Pump))
            .count();
        assert_eq!(decodes, 1, "only the keyframe is decoded");
        assert_eq!(pumps, 1, "the dropped delta still pumps the decoder");
        assert_eq!(ds.decoded_count(), 1);
    }

    #[test]
    fn keyframe_is_admitted_even_at_high_depth() {
        // A keyframe must always feed (it's the resync point), even when in_flight is huge.
        let frames = [
            Frame::VideoConfig {
                codec: VideoCodec::H264,
                sps_pps: sps_pps_bytes(),
            },
            Frame::Video {
                pts_us: 0,
                keyframe: true,
                nal: bare_keyframe_nal(),
            },
        ];
        let mut t = transport_with_frames(&frames);
        let mut ds = DecodeSession::new();
        let mut dec = RecordingDecoder {
            in_flight_script: std::cell::RefCell::new(VecDeque::from(vec![999usize])),
            ..RecordingDecoder::default()
        };
        let caps = default_client_caps();

        let summary = run_session(&mut t, &caps, &mut ds, &mut dec).unwrap();

        assert_eq!(summary.input_frames_dropped, 0);
        assert_eq!(ds.decoded_count(), 1, "keyframe decoded despite high depth");
    }
}
