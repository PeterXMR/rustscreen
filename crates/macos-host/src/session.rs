//! The host-side send-session orchestrator (P5, PIPE-01 criterion #3 send-side).
//!
//! This module wires the EXISTING port seams — [`Capturer`], [`Encoder`], and
//! [`Transport`] — into a complete live pipeline: handshake negotiation, then a
//! capture→encode→send loop that transmits [`Frame::VideoConfig`] on connect and on
//! every subsequent keyframe (so a late-joining or recovering decoder can always
//! reconfigure), followed by [`Frame::Video`] for every access unit.
//!
//! # Design summary
//! - Handshake exchange: host sends `Frame::Handshake` (its offer), client replies with
//!   its own `Frame::Handshake` where `width`/`height` = max panel resolution,
//!   `refresh_hz` = max refresh rate, and `codecs` = supported codecs.  The host maps
//!   the reply into a synthetic [`ClientCaps`] and calls `negotiate()`.  This is the
//!   minimal sound exchange that fits the existing `Frame`/`negotiate` API with no new
//!   wire types.
//!
//! # Platform-agnostic
//! No `#[cfg]` feature gates here — the entire module runs in CI under `cargo test`.
//! The live VideoToolbox/nusb adapters that supply the concrete types behind the traits
//! are separately compiled under `--features live-usb` / `encode_vt`.

use std::io::{self, Read, Write};

use protocol::{
    messages::{
        AgreedConfig, ClientCaps, Frame, Handshake, MessageError, NegotiationError, VideoCodec,
    },
    nal::{self, CodecConfig},
    protocol_version,
};

use crate::{
    capture::Capturer,
    encode::{Encoder, LatencyStats},
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during session setup or streaming.
#[derive(Debug)]
pub enum SessionError {
    /// An I/O error on the underlying transport.
    Io(io::Error),
    /// A protocol framing or message codec error.
    Message(MessageError),
    /// Handshake negotiation failed (version mismatch, no common codec, or resolution
    /// not supported by the client's panel).
    Negotiation(NegotiationError),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Io(e) => write!(f, "session I/O error: {e}"),
            SessionError::Message(e) => write!(f, "session message error: {e}"),
            SessionError::Negotiation(e) => write!(f, "session negotiation error: {e:?}"),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::Io(e) => Some(e),
            SessionError::Message(e) => Some(e),
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
        SessionError::Message(e)
    }
}

impl From<NegotiationError> for SessionError {
    fn from(e: NegotiationError) -> Self {
        SessionError::Negotiation(e)
    }
}

// ---------------------------------------------------------------------------
// Session summary
// ---------------------------------------------------------------------------

/// Summary of a completed host send-session.
#[derive(Debug, Clone, PartialEq)]
pub struct SendSessionSummary {
    /// Number of frames captured, encoded, and sent.
    pub frames: u64,
    /// Total protocol bytes written to the transport (sum of all `Frame::write_to` bytes).
    pub bytes_sent: usize,
    /// Codec config (SPS/PPS) extracted from the stream, once available.  `None` if no
    /// keyframe carrying parameter sets was seen before the session ended.
    pub codec_config: Option<CodecConfig>,
    /// Per-frame encode-latency stats.
    pub latency: LatencyStats,
    /// The negotiated streaming configuration agreed during the handshake.
    pub agreed_config: AgreedConfig,
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

/// Perform the host-side handshake over `transport`.
///
/// 1. Write a `Frame::Handshake` advertising the host's offer.
/// 2. Read the peer's reply (expected to be a `Frame::Handshake` carrying the
///    client's capabilities in the matching fields).
/// 3. Construct a [`ClientCaps`] from the peer's reply and call [`negotiate`].
///
/// The exchange uses a single message in each direction.  The client is expected to
/// reply with a `Frame::Handshake` whose fields encode its constraints:
/// - `protocol_version` — the client's version (must match the host's).
/// - `width` / `height` — the client's **maximum** panel resolution.
/// - `refresh_hz` — the client's **maximum** refresh rate.
/// - `codecs` — the codecs the client can decode.
///
/// This is the minimal sound exchange: both peers speak the same `Handshake` wire
/// type, the host interprets the reply as caps, and `negotiate()` does the rest.
pub fn perform_handshake(
    transport: &mut (impl Read + Write),
    offer: Handshake,
) -> Result<AgreedConfig, SessionError> {
    // 1. Send our offer. Flush before blocking on the reply: nusb's `EndpointWrite`
    //    buffers, so an unflushed partial (<16 KiB) frame would sit in the host's
    //    userspace buffer and the peer's read would block forever (C-01). No-op for
    //    TcpStream/loopback; essential for the real AOA bulk path.
    Frame::Handshake(offer.clone()).write_to(transport)?;
    transport.flush()?;

    // 2. Read the peer's reply.
    let reply = Frame::read_from(transport)?;
    let client_hs = match reply {
        Frame::Handshake(h) => h,
        other => {
            return Err(SessionError::Message(MessageError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected Handshake reply, got {other:?}"),
            ))));
        }
    };

    // 3. Map the peer's reply into ClientCaps.
    let caps = ClientCaps {
        protocol_version: client_hs.protocol_version,
        max_width: client_hs.width,
        max_height: client_hs.height,
        max_refresh_hz: client_hs.refresh_hz,
        codecs: client_hs.codecs,
    };

    // 4. Negotiate.
    let agreed = protocol::messages::negotiate(&offer, &caps)?;
    Ok(agreed)
}

// ---------------------------------------------------------------------------
// Counting Write adapter (for bytes_sent tracking)
// ---------------------------------------------------------------------------

/// A `Write` wrapper that counts the total bytes written.
struct CountingWrite<'a> {
    inner: &'a mut dyn Write,
    total: usize,
}

impl<'a> CountingWrite<'a> {
    fn new(inner: &'a mut dyn Write) -> Self {
        Self { inner, total: 0 }
    }
}

impl Write for CountingWrite<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.total += n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// Session loop
// ---------------------------------------------------------------------------

/// Drive a host send-session over `transport` for up to `max_frames` frames.
///
/// Step 1 — Handshake: sends a `Frame::Handshake` (built from `offer`) and reads the
/// peer's reply, calling `negotiate()` to produce the [`AgreedConfig`].
///
/// Step 2 — Stream loop: pulls frames from `capturer`, encodes via `encoder`, and
/// for each encoded frame:
///   - Extracts SPS/PPS from the **first** keyframe encountered and caches it.
///   - Sends `Frame::VideoConfig` **before** every keyframe (on connect and on each
///     subsequent IDR — a decoder that joins late can always reconfigure).
///   - Sends `Frame::Video` for every access unit.
///   - Records `encode_micros` in [`LatencyStats`].
///
/// Returns a [`SendSessionSummary`] on success, or a [`SessionError`] on any I/O,
/// message, or negotiation failure.
pub fn run_send_session(
    capturer: &mut dyn Capturer,
    encoder: &mut dyn Encoder,
    transport: &mut (impl Read + Write),
    offer: Handshake,
    max_frames: u64,
) -> Result<SendSessionSummary, SessionError> {
    // --- Handshake -----------------------------------------------------------
    let agreed = perform_handshake(transport, offer)?;

    // --- Streaming loop ------------------------------------------------------
    let mut counting = CountingWrite::new(transport);
    let mut summary = SendSessionSummary {
        frames: 0,
        bytes_sent: 0,
        codec_config: None,
        latency: LatencyStats::new(),
        agreed_config: agreed.clone(),
    };

    while summary.frames < max_frames {
        let Some(captured) = capturer.next_frame() else {
            break;
        };
        let encoded = encoder.encode(&captured);

        // Extract and cache codec config from the first keyframe.
        if summary.codec_config.is_none() && encoded.keyframe {
            summary.codec_config = nal::extract_codec_config(&encoded.annex_b);
        }

        // Send VideoConfig on every keyframe (connect AND subsequent IDR recovery).
        if encoded.keyframe {
            if let Some(ref cfg) = summary.codec_config {
                // Build the sps_pps Annex-B stream from the cached CodecConfig.
                let sps_pps = codec_config_to_annex_b(cfg);
                Frame::VideoConfig {
                    codec: agreed.codec,
                    sps_pps,
                }
                .write_to(&mut counting)?;
            }
        }

        // Send the access unit.
        Frame::Video {
            pts_us: encoded.pts_us,
            keyframe: encoded.keyframe,
            nal: encoded.annex_b,
        }
        .write_to(&mut counting)?;

        // Flush each frame so it reaches the peer instead of stalling in nusb's
        // userspace write buffer (C-01). `CountingWrite` forwards flush to the
        // transport; no-op for TcpStream/loopback, essential for the AOA bulk path.
        counting.flush()?;

        summary.latency.record(encoded.encode_micros);
        summary.frames += 1;
    }

    summary.bytes_sent = counting.total;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Helper: CodecConfig → Annex-B sps_pps byte stream
// ---------------------------------------------------------------------------

/// Serialise a [`CodecConfig`] back into an Annex-B byte stream (`00 00 00 01 <SPS>
/// 00 00 00 01 <PPS>`) suitable for a [`Frame::VideoConfig`] payload.
fn codec_config_to_annex_b(cfg: &CodecConfig) -> Vec<u8> {
    const SC4: [u8; 4] = [0, 0, 0, 1];
    let mut out = Vec::with_capacity(SC4.len() * 2 + cfg.sps.len() + cfg.pps.len());
    out.extend_from_slice(&SC4);
    out.extend_from_slice(&cfg.sps);
    out.extend_from_slice(&SC4);
    out.extend_from_slice(&cfg.pps);
    out
}

// ---------------------------------------------------------------------------
// Convenience: build the default host Handshake offer
// ---------------------------------------------------------------------------

/// Construct the standard host offer for the given display configuration.
pub fn host_handshake(width: u32, height: u32, refresh_hz: u32) -> Handshake {
    Handshake {
        protocol_version: protocol_version(),
        width,
        height,
        refresh_hz,
        codecs: vec![VideoCodec::H264],
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::CapturedFrame;
    use crate::encode::EncodedFrame;
    use std::collections::VecDeque;
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // Fake Capturer (same pattern as encode.rs)
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Fake Encoder — keyframe on frame 0, then every N frames
    // -----------------------------------------------------------------------

    struct FakeEncoder {
        frame_index: u64,
        /// Emit a keyframe every `keyframe_interval` frames (0 = only frame 0).
        keyframe_interval: u64,
    }

    impl FakeEncoder {
        fn new_default() -> Self {
            Self {
                frame_index: 0,
                keyframe_interval: 0,
            }
        }

        fn new_with_interval(keyframe_interval: u64) -> Self {
            Self {
                frame_index: 0,
                keyframe_interval,
            }
        }
    }

    impl Encoder for FakeEncoder {
        fn encode(&mut self, frame: &CapturedFrame) -> EncodedFrame {
            let keyframe = self.frame_index == 0
                || (self.keyframe_interval > 0 && self.frame_index % self.keyframe_interval == 0);
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

    // -----------------------------------------------------------------------
    // Duplex in-memory transport
    //
    // The session loop does: write (handshake offer) → read (peer reply) → write (frames).
    // We pre-load the read side with a scripted peer reply and capture all writes on the
    // write side.
    // -----------------------------------------------------------------------

    /// A split-cursor transport:
    /// - `read_src`: bytes the "peer" will send back (read side, pre-loaded).
    /// - `write_sink`: bytes the host has written (write side, inspectable after the call).
    struct SplitTransport {
        read_src: Cursor<Vec<u8>>,
        write_sink: Vec<u8>,
    }

    impl SplitTransport {
        fn new(peer_reply: Vec<u8>) -> Self {
            Self {
                read_src: Cursor::new(peer_reply),
                write_sink: Vec::new(),
            }
        }

        /// Decode all frames from the write_sink in order.
        fn decode_sent_frames(&mut self) -> Vec<Frame> {
            let mut frames = Vec::new();
            let mut cur = Cursor::new(self.write_sink.clone());
            loop {
                match Frame::read_from(&mut cur) {
                    Ok(f) => frames.push(f),
                    Err(MessageError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        break
                    }
                    Err(e) => panic!("decode_sent_frames error: {e}"),
                }
            }
            frames
        }
    }

    impl Read for SplitTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.read_src.read(buf)
        }
    }

    impl Write for SplitTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_sink.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // -----------------------------------------------------------------------
    // Flush-audit transport — enforces the C-01 invariant
    //
    // On the live AOA bulk path, nusb's `EndpointWrite` buffers in userspace: a
    // frame that is written but not flushed never reaches the peer, so a blocking
    // read deadlocks. A no-op `flush()` (like `SplitTransport`'s) hides this. This
    // transport models the buffer: it tracks bytes written since the last flush and
    // panics if a read is attempted — or the session returns — with bytes still
    // unflushed.
    // -----------------------------------------------------------------------

    struct FlushAuditTransport {
        read_src: Cursor<Vec<u8>>,
        /// Bytes written since the last `flush()`. C-01: must be 0 before any
        /// blocking read and when the session returns.
        unflushed: usize,
    }

    impl FlushAuditTransport {
        fn new(peer_reply: Vec<u8>) -> Self {
            Self {
                read_src: Cursor::new(peer_reply),
                unflushed: 0,
            }
        }
    }

    impl Read for FlushAuditTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            assert_eq!(
                self.unflushed, 0,
                "C-01 violation: blocking read with {} unflushed byte(s) in the write \
                 buffer — would deadlock the AOA bulk path",
                self.unflushed
            );
            self.read_src.read(buf)
        }
    }

    impl Write for FlushAuditTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.unflushed += buf.len();
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.unflushed = 0;
            Ok(())
        }
    }

    // -----------------------------------------------------------------------
    // Helpers for building scripted peer replies
    // -----------------------------------------------------------------------

    fn make_client_reply(
        version: u32,
        max_width: u32,
        max_height: u32,
        max_refresh_hz: u32,
        codecs: Vec<VideoCodec>,
    ) -> Vec<u8> {
        let reply_hs = Handshake {
            protocol_version: version,
            width: max_width,
            height: max_height,
            refresh_hz: max_refresh_hz,
            codecs,
        };
        let mut buf = Vec::new();
        Frame::Handshake(reply_hs).write_to(&mut buf).unwrap();
        buf
    }

    fn default_offer() -> Handshake {
        host_handshake(2400, 1080, 60)
    }

    fn default_client_reply() -> Vec<u8> {
        make_client_reply(protocol_version(), 2400, 1080, 60, vec![VideoCodec::H264])
    }

    // -----------------------------------------------------------------------
    // Handshake tests
    // -----------------------------------------------------------------------

    #[test]
    fn handshake_success_produces_agreed_config() {
        let reply_bytes = default_client_reply();
        let mut t = SplitTransport::new(reply_bytes);
        let agreed = perform_handshake(&mut t, default_offer()).unwrap();

        assert_eq!(agreed.width, 2400);
        assert_eq!(agreed.height, 1080);
        assert_eq!(agreed.refresh_hz, 60);
        assert_eq!(agreed.codec, VideoCodec::H264);
    }

    #[test]
    fn handshake_clamps_refresh_to_client_max() {
        let reply_bytes =
            make_client_reply(protocol_version(), 2400, 1080, 30, vec![VideoCodec::H264]);
        let mut t = SplitTransport::new(reply_bytes);
        let agreed = perform_handshake(&mut t, default_offer()).unwrap();
        assert_eq!(agreed.refresh_hz, 30);
    }

    #[test]
    fn handshake_version_mismatch_yields_negotiation_error() {
        // Client replies with a different protocol version — negotiate() must fail.
        let reply_bytes = make_client_reply(99, 2400, 1080, 60, vec![VideoCodec::H264]);
        let mut t = SplitTransport::new(reply_bytes);
        let err = perform_handshake(&mut t, default_offer()).unwrap_err();
        assert!(
            matches!(
                err,
                SessionError::Negotiation(NegotiationError::VersionMismatch { .. })
            ),
            "expected VersionMismatch, got {err:?}"
        );
    }

    #[test]
    fn handshake_no_common_codec_yields_negotiation_error() {
        let reply_bytes =
            make_client_reply(protocol_version(), 2400, 1080, 60, vec![VideoCodec::Hevc]);
        let mut t = SplitTransport::new(reply_bytes);
        let err = perform_handshake(&mut t, default_offer()).unwrap_err();
        assert!(
            matches!(
                err,
                SessionError::Negotiation(NegotiationError::NoCommonCodec)
            ),
            "expected NoCommonCodec, got {err:?}"
        );
    }

    #[test]
    fn handshake_resolution_too_big_yields_negotiation_error() {
        // Client can only handle 1080×720 but host offers 2400×1080.
        let reply_bytes =
            make_client_reply(protocol_version(), 1080, 720, 60, vec![VideoCodec::H264]);
        let mut t = SplitTransport::new(reply_bytes);
        let err = perform_handshake(&mut t, default_offer()).unwrap_err();
        assert!(
            matches!(
                err,
                SessionError::Negotiation(NegotiationError::ResolutionUnsupported { .. })
            ),
            "expected ResolutionUnsupported, got {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // VideoConfig-on-keyframe tests
    // -----------------------------------------------------------------------

    /// 3 frames, only frame 0 is a keyframe → expect exactly 1 VideoConfig + 3 Video.
    #[test]
    fn video_config_sent_once_for_single_keyframe() {
        let mut t = SplitTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 3,
            pts: 0,
        };
        let mut enc = FakeEncoder::new_default();
        let summary = run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 10).unwrap();

        assert_eq!(summary.frames, 3);

        // Skip the initial Handshake frame that the host sent, then count the rest.
        let all_frames = t.decode_sent_frames();
        // First frame should be the Handshake offer.
        assert!(
            matches!(&all_frames[0], Frame::Handshake(_)),
            "first frame must be Handshake"
        );
        let stream_frames = &all_frames[1..];

        let video_config_count = stream_frames
            .iter()
            .filter(|f| matches!(f, Frame::VideoConfig { .. }))
            .count();
        let video_count = stream_frames
            .iter()
            .filter(|f| matches!(f, Frame::Video { .. }))
            .count();

        assert_eq!(
            video_config_count, 1,
            "one VideoConfig (on the first/only keyframe)"
        );
        assert_eq!(video_count, 3, "one Video per captured frame");
    }

    /// Encoder produces keyframes at frames 0, 2, 4 (interval=2) over 5 frames.
    /// Expect 3 VideoConfig frames (one per keyframe) + 5 Video frames.
    #[test]
    fn video_config_sent_on_every_keyframe() {
        let mut t = SplitTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 5,
            pts: 0,
        };
        let mut enc = FakeEncoder::new_with_interval(2); // keyframe at 0, 2, 4
        let summary = run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 10).unwrap();

        assert_eq!(summary.frames, 5);

        let all_frames = t.decode_sent_frames();
        let stream_frames = &all_frames[1..]; // skip the Handshake offer

        let video_config_count = stream_frames
            .iter()
            .filter(|f| matches!(f, Frame::VideoConfig { .. }))
            .count();
        let video_count = stream_frames
            .iter()
            .filter(|f| matches!(f, Frame::Video { .. }))
            .count();

        assert_eq!(video_config_count, 3, "VideoConfig on frames 0, 2, 4");
        assert_eq!(video_count, 5, "Video for every frame");
    }

    /// Verify that VideoConfig precedes Video on each keyframe.
    #[test]
    fn video_config_precedes_video_on_keyframe() {
        let mut t = SplitTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 2,
            pts: 0,
        };
        // frame 0 = keyframe, frame 1 = delta
        let mut enc = FakeEncoder::new_default();
        run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 10).unwrap();

        let all_frames = t.decode_sent_frames();
        // layout: Handshake | VideoConfig | Video(kf) | Video(delta)
        assert!(matches!(&all_frames[0], Frame::Handshake(_)));
        assert!(
            matches!(&all_frames[1], Frame::VideoConfig { .. }),
            "VideoConfig must precede the first Video"
        );
        assert!(matches!(
            &all_frames[2],
            Frame::Video { keyframe: true, .. }
        ));
        assert!(matches!(
            &all_frames[3],
            Frame::Video {
                keyframe: false,
                ..
            }
        ));
    }

    /// C-01: every write must be flushed before the session blocks on a read and
    /// before it returns, otherwise frames stall in nusb's userspace buffer and the
    /// peer deadlocks. `FlushAuditTransport` panics on any unflushed read/return.
    #[test]
    fn session_flushes_writes_before_reads_and_at_end() {
        let mut t = FlushAuditTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 3,
            pts: 0,
        };
        let mut enc = FakeEncoder::new_with_interval(2); // keyframes at 0, 2
        let summary = run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 10).unwrap();

        assert_eq!(summary.frames, 3);
        assert_eq!(
            t.unflushed, 0,
            "session returned with unflushed bytes — frames would stall in the write buffer"
        );
    }

    // -----------------------------------------------------------------------
    // Video frame content tests
    // -----------------------------------------------------------------------

    #[test]
    fn video_frames_carry_correct_pts_keyframe_nal() {
        let mut t = SplitTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 2,
            pts: 1000,
        };
        let mut enc = FakeEncoder::new_default();
        run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 10).unwrap();

        let all_frames = t.decode_sent_frames();
        // Find the Video frames in order.
        let videos: Vec<_> = all_frames
            .iter()
            .filter_map(|f| {
                if let Frame::Video {
                    pts_us,
                    keyframe,
                    nal,
                } = f
                {
                    Some((*pts_us, *keyframe, nal.clone()))
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(videos.len(), 2);
        let (pts0, kf0, _nal0) = &videos[0];
        assert_eq!(*pts0, 1000);
        assert!(*kf0);

        let (pts1, kf1, _nal1) = &videos[1];
        assert_eq!(*pts1, 1000 + 16_666);
        assert!(!*kf1);
    }

    // -----------------------------------------------------------------------
    // VideoConfig content test
    // -----------------------------------------------------------------------

    #[test]
    fn video_config_carries_correct_codec_and_sps_pps() {
        let mut t = SplitTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 1,
            pts: 0,
        };
        let mut enc = FakeEncoder::new_default();
        let summary = run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 10).unwrap();

        // Codec config must be populated from the keyframe.
        let cfg = summary.codec_config.expect("codec_config populated");
        assert_eq!(cfg.sps, vec![0x67, 0x42, 0x1F]);
        assert_eq!(cfg.pps, vec![0x68, 0xCE]);

        // Find the VideoConfig frame and verify its contents.
        let all_frames = t.decode_sent_frames();
        let vc = all_frames.iter().find_map(|f| {
            if let Frame::VideoConfig { codec, sps_pps } = f {
                Some((*codec, sps_pps.clone()))
            } else {
                None
            }
        });
        let (codec, sps_pps) = vc.expect("VideoConfig frame present");
        assert_eq!(codec, VideoCodec::H264);

        // The sps_pps must parse back to the same SPS/PPS.
        let parsed = nal::extract_codec_config(&sps_pps).expect("sps_pps parses");
        assert_eq!(parsed.sps, vec![0x67, 0x42, 0x1F]);
        assert_eq!(parsed.pps, vec![0x68, 0xCE]);
    }

    // -----------------------------------------------------------------------
    // Latency and control-flow tests
    // -----------------------------------------------------------------------

    #[test]
    fn latency_recorded_per_frame() {
        let mut t = SplitTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 4,
            pts: 0,
        };
        let mut enc = FakeEncoder::new_default();
        let summary = run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 10).unwrap();

        assert_eq!(summary.latency.count(), 4);
        assert_eq!(summary.latency.mean(), Some(5.0)); // FakeEncoder always returns 5 µs
    }

    #[test]
    fn session_stops_at_max_frames() {
        let mut t = SplitTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 100,
            pts: 0,
        };
        let mut enc = FakeEncoder::new_default();
        let summary = run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 5).unwrap();

        assert_eq!(summary.frames, 5);
    }

    #[test]
    fn session_ends_when_capturer_stops_early() {
        let mut t = SplitTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 2,
            pts: 0,
        };
        let mut enc = FakeEncoder::new_default();
        let summary = run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 50).unwrap();

        assert_eq!(summary.frames, 2);
    }

    // -----------------------------------------------------------------------
    // Agreed config is propagated through the summary
    // -----------------------------------------------------------------------

    #[test]
    fn summary_carries_agreed_config() {
        let mut t = SplitTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 1,
            pts: 0,
        };
        let mut enc = FakeEncoder::new_default();
        let summary = run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 10).unwrap();

        assert_eq!(summary.agreed_config.codec, VideoCodec::H264);
        assert_eq!(summary.agreed_config.width, 2400);
        assert_eq!(summary.agreed_config.height, 1080);
    }

    // -----------------------------------------------------------------------
    // bytes_sent is non-zero when frames were sent
    // -----------------------------------------------------------------------

    #[test]
    fn bytes_sent_matches_actual_write_size() {
        let mut t = SplitTransport::new(default_client_reply());
        let mut cap = FakeCapturer {
            remaining: 2,
            pts: 0,
        };
        let mut enc = FakeEncoder::new_default();
        let summary = run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 10).unwrap();

        // bytes_sent accounts for the VideoConfig + Video frames (not the handshake offer,
        // which is written before the CountingWrite is created).
        // We only check it's non-zero and consistent with the write_sink length.
        // The handshake offer is the difference.
        let total_in_sink = t.write_sink.len();
        // Handshake is always the first frame written; the rest is bytes_sent.
        let all_frames_before_sink: Vec<_> = {
            let mut cur = Cursor::new(t.write_sink.clone());
            let mut v = Vec::new();
            while let Ok(f) = Frame::read_from(&mut cur) {
                v.push(f);
            }
            v
        };
        let handshake_frame = Frame::Handshake(default_offer());
        let mut hs_bytes = Vec::new();
        handshake_frame.write_to(&mut hs_bytes).unwrap();
        let handshake_size = hs_bytes.len();

        assert_eq!(
            summary.bytes_sent,
            total_in_sink - handshake_size,
            "bytes_sent should cover VideoConfig + Video frames"
        );
        // Sanity: at least one VideoConfig and two Video frames were sent.
        let stream_frame_count = all_frames_before_sink.len() - 1; // subtract the handshake
        assert!(stream_frame_count >= 3, "VideoConfig + 2 Video frames");
    }

    // -----------------------------------------------------------------------
    // VecDeque transport (verifies the Transport blanket impl)
    // -----------------------------------------------------------------------

    /// A duplex in-memory transport backed by two VecDeques: one pre-filled with the peer
    /// reply (read side), one accumulating the host's writes (write side).
    struct DuplexTransport {
        rx: VecDeque<u8>,
        tx: Vec<u8>,
    }

    impl DuplexTransport {
        fn new(peer_reply: Vec<u8>) -> Self {
            Self {
                rx: peer_reply.into(),
                tx: Vec::new(),
            }
        }
    }

    impl Read for DuplexTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.rx.len());
            for slot in buf.iter_mut().take(n) {
                *slot = self.rx.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    impl Write for DuplexTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.tx.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn session_works_over_duplex_transport() {
        let reply = default_client_reply();
        let mut t = DuplexTransport::new(reply);
        let mut cap = FakeCapturer {
            remaining: 3,
            pts: 0,
        };
        let mut enc = FakeEncoder::new_default();
        let summary = run_send_session(&mut cap, &mut enc, &mut t, default_offer(), 10).unwrap();
        assert_eq!(summary.frames, 3);
        assert!(summary.codec_config.is_some());
    }
}
