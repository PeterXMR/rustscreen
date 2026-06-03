//! Application-level message types and the `Frame` codec.
//!
//! This module sits **on top of** [`crate::framing`]: every [`Frame`] is encoded as
//! exactly one length-prefixed framing frame — `tag` byte = message kind, payload =
//! the variant's bytes. We do **not** reinvent length-prefixing; `framing` owns that
//! (including the [`crate::framing::MAX_FRAME_LEN`] guard that protects decode-time
//! allocations).
//!
//! Payload encoding (roadmap §301, CONTEXT locked decisions 1–3):
//! - Structured variants (`Handshake`, `VideoConfig`, `Touch`, `Control`) use
//!   [`postcard`] (`to_allocvec` / `from_bytes`).
//! - The `Video` variant is written **raw**: `[pts_us u64 BE][keyframe u8][nal …]`
//!   with no serde framing over the (potentially large) NAL buffer.
//!
//! `std` note: `serde`/`postcard` are pulled in `alloc`-only (`default-features =
//! false`, no `use-std`) so a future `no_std` split stays *possible* — but this module
//! is **not** `no_std` today: it uses `std::io::{Read, Write}` (like [`crate::framing`])
//! and `std::error::Error`. The alloc-only deps keep that door open, nothing more.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

use crate::framing;

/// Per-variant `u8` tag constants (RESEARCH Pattern 1: explicit, append-only, never
/// renumber). These are the `tag` byte of the underlying framing frame.
pub mod tag {
    /// [`Frame::Handshake`]
    pub const HANDSHAKE: u8 = 1;
    /// [`Frame::VideoConfig`]
    pub const VIDEO_CONFIG: u8 = 2;
    /// [`Frame::Video`]
    pub const VIDEO: u8 = 3;
    /// [`Frame::Touch`]
    pub const TOUCH: u8 = 4;
    /// [`Frame::Control`]
    pub const CONTROL: u8 = 5;
}

/// Video codec identifier. `Hevc` is reserved; only `H264` is exercised now (D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC (reserved — not exercised in the MVP).
    Hevc,
}

/// On-wire payload of [`Frame::VideoConfig`]. A named struct (rather than an ad-hoc
/// tuple) so the postcard wire shape lives in exactly one place: postcard is
/// non-self-describing and positional, so a tuple and a struct encode identically —
/// pinning it as a type means a future field addition can't silently desync the
/// encode and decode sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct VideoConfigPayload {
    codec: VideoCodec,
    sps_pps: Vec<u8>,
}

/// The host's connection offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    /// Protocol version — checked against [`crate::protocol_version`].
    pub protocol_version: u32,
    /// Offered desktop width in pixels.
    pub width: u32,
    /// Offered desktop height in pixels.
    pub height: u32,
    /// Offered refresh rate in Hz.
    pub refresh_hz: u32,
    /// Codecs the host can produce, in host preference order.
    pub codecs: Vec<VideoCodec>,
}

/// The client's (Pixel) capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientCaps {
    /// Protocol version — checked against [`crate::protocol_version`].
    pub protocol_version: u32,
    /// Maximum panel width in pixels.
    pub max_width: u32,
    /// Maximum panel height in pixels.
    pub max_height: u32,
    /// Maximum panel refresh rate in Hz.
    pub max_refresh_hz: u32,
    /// Codecs the client can decode. Order is **not** significant — [`negotiate`]
    /// resolves codecs in the *host's* preference order, not the client's.
    pub codecs: Vec<VideoCodec>,
}

/// Out-of-band control messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    /// Ask the host to emit a keyframe (recovery / late join).
    RequestKeyframe,
    /// Pause streaming.
    Pause,
    /// Resume streaming.
    Resume,
    /// Graceful disconnect.
    Bye,
}

/// The phase of a touch pointer's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchPhase {
    /// Pointer pressed.
    Down,
    /// Pointer moved while down.
    Move,
    /// Pointer released.
    Up,
}

/// A single touch event with normalized (`0.0..=1.0`) coordinates.
///
/// `nx`/`ny` are **not** range-validated on decode — a peer can send out-of-range or
/// `NaN` values. The consumer (P6 touch injection / coordinate mapping) must clamp or
/// reject. Because of the `f32` fields this type is `PartialEq` but not `Eq` (and `NaN`
/// breaks equality), so tests compare exactly-representable values only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TouchEvent {
    /// Stable id distinguishing simultaneous pointers.
    pub pointer_id: u32,
    /// Lifecycle phase.
    pub phase: TouchPhase,
    /// Normalized x in `0.0..=1.0`.
    pub nx: f32,
    /// Normalized y in `0.0..=1.0`.
    pub ny: f32,
}

/// One application message. Encodes to exactly one [`framing`] frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// Host → client connection offer.
    Handshake(Handshake),
    /// Codec configuration (SPS/PPS) sent on connect and on each keyframe.
    VideoConfig {
        /// Negotiated codec.
        codec: VideoCodec,
        /// Concatenated SPS/PPS bytes (see [`crate::nal::CodecConfig`]).
        sps_pps: Vec<u8>,
    },
    /// One encoded access unit. Payload is raw (no serde) — see module docs.
    Video {
        /// Presentation timestamp in microseconds.
        pts_us: u64,
        /// Whether this access unit is a keyframe (IDR).
        keyframe: bool,
        /// The NAL bytes, verbatim.
        nal: Vec<u8>,
    },
    /// Client → host touch input.
    Touch(TouchEvent),
    /// Either-direction control message.
    Control(Control),
}

/// Errors produced when encoding or decoding a [`Frame`].
#[derive(Debug)]
pub enum MessageError {
    /// A framing-frame tag outside the 1–5 registry.
    UnknownTag(u8),
    /// A `Video` payload shorter than its 9-byte fixed header.
    ShortVideoHeader,
    /// A structured payload failed to **serialize** (postcard, encode side). Carries
    /// the underlying error so an encode bug isn't mistaken for corrupt input.
    Encode(postcard::Error),
    /// A structured payload failed to **deserialize** (postcard, decode side). Carries
    /// the underlying error to keep wire-debugging tractable.
    Decode(postcard::Error),
    /// A structured payload deserialized but had **extra bytes** after the value.
    /// Decode is canonical — every byte of the framing payload must be consumed — so
    /// trailing data is rejected rather than silently ignored.
    TrailingBytes,
    /// An I/O error from the underlying [`framing`] layer.
    Io(std::io::Error),
}

impl std::fmt::Display for MessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageError::UnknownTag(t) => write!(f, "unknown frame tag: {t}"),
            MessageError::ShortVideoHeader => write!(f, "video payload shorter than 9-byte header"),
            MessageError::Encode(e) => write!(f, "failed to serialize structured payload: {e}"),
            MessageError::Decode(e) => write!(f, "failed to deserialize structured payload: {e}"),
            MessageError::TrailingBytes => {
                write!(f, "structured payload had trailing bytes after the value")
            }
            MessageError::Io(e) => write!(f, "framing I/O error: {e}"),
        }
    }
}

impl std::error::Error for MessageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MessageError::Io(e) => Some(e),
            // postcard::Error's std::error::Error impl is feature-gated; we surface its
            // detail via Display/Debug rather than chain it here.
            _ => None,
        }
    }
}

impl From<std::io::Error> for MessageError {
    fn from(e: std::io::Error) -> Self {
        MessageError::Io(e)
    }
}

/// Fixed size of the raw `Video` payload header: `pts_us` (u64 BE) + `keyframe` (u8).
const VIDEO_HEADER_LEN: usize = 9;

/// Deserialize a postcard payload and require it to be **fully consumed**. Trailing
/// bytes after the value are rejected ([`MessageError::TrailingBytes`]) so decode is
/// canonical — distinct framing payloads can never map to the same [`Frame`].
fn decode_canonical<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T, MessageError> {
    let (value, rest) = postcard::take_from_bytes::<T>(payload).map_err(MessageError::Decode)?;
    if rest.is_empty() {
        Ok(value)
    } else {
        Err(MessageError::TrailingBytes)
    }
}

impl Frame {
    /// Encode to `(tag, payload)` — **pure**, no I/O. Structured variants use
    /// `postcard::to_allocvec`; the `Video` variant builds its raw header + NAL.
    pub fn to_tag_payload(&self) -> Result<(u8, Vec<u8>), MessageError> {
        match self {
            Frame::Handshake(h) => {
                let payload = postcard::to_allocvec(h).map_err(MessageError::Encode)?;
                Ok((tag::HANDSHAKE, payload))
            }
            Frame::VideoConfig { codec, sps_pps } => {
                let payload = postcard::to_allocvec(&VideoConfigPayload {
                    codec: *codec,
                    sps_pps: sps_pps.clone(),
                })
                .map_err(MessageError::Encode)?;
                Ok((tag::VIDEO_CONFIG, payload))
            }
            Frame::Video {
                pts_us,
                keyframe,
                nal,
            } => {
                let mut payload = Vec::with_capacity(VIDEO_HEADER_LEN + nal.len());
                payload.extend_from_slice(&pts_us.to_be_bytes());
                payload.push(*keyframe as u8);
                payload.extend_from_slice(nal);
                Ok((tag::VIDEO, payload))
            }
            Frame::Touch(t) => {
                let payload = postcard::to_allocvec(t).map_err(MessageError::Encode)?;
                Ok((tag::TOUCH, payload))
            }
            Frame::Control(c) => {
                let payload = postcard::to_allocvec(c).map_err(MessageError::Encode)?;
                Ok((tag::CONTROL, payload))
            }
        }
    }

    /// Decode a `(tag, payload)` back into a [`Frame`] — **pure**, no I/O. Returns a
    /// typed error on an unknown tag, a short `Video` header, a malformed payload, or
    /// trailing bytes after a structured payload (decode is canonical).
    pub fn decode(tag: u8, payload: &[u8]) -> Result<Frame, MessageError> {
        match tag {
            tag::HANDSHAKE => Ok(Frame::Handshake(decode_canonical(payload)?)),
            tag::VIDEO_CONFIG => {
                let p: VideoConfigPayload = decode_canonical(payload)?;
                Ok(Frame::VideoConfig {
                    codec: p.codec,
                    sps_pps: p.sps_pps,
                })
            }
            tag::VIDEO => {
                // Split off the fixed 9-byte header; a slice shorter than that is a
                // ShortVideoHeader, so the u64 conversion below can never fail/panic.
                let Some((header, nal)) = payload.split_at_checked(VIDEO_HEADER_LEN) else {
                    return Err(MessageError::ShortVideoHeader);
                };
                let pts_us = u64::from_be_bytes(
                    header[0..8]
                        .try_into()
                        .map_err(|_| MessageError::ShortVideoHeader)?,
                );
                let keyframe = header[8] != 0;
                Ok(Frame::Video {
                    pts_us,
                    keyframe,
                    nal: nal.to_vec(),
                })
            }
            tag::TOUCH => Ok(Frame::Touch(decode_canonical(payload)?)),
            tag::CONTROL => Ok(Frame::Control(decode_canonical(payload)?)),
            other => Err(MessageError::UnknownTag(other)),
        }
    }

    /// Encode and write one frame to `w` via [`framing::write_frame`].
    pub fn write_to(&self, w: &mut dyn Write) -> Result<(), MessageError> {
        let (tag, payload) = self.to_tag_payload()?;
        framing::write_frame(w, tag, &payload)?;
        Ok(())
    }

    /// Read and decode one frame from `r` via [`framing::read_frame`].
    pub fn read_from(r: &mut dyn Read) -> Result<Frame, MessageError> {
        let (tag, payload) = framing::read_frame(r)?;
        Frame::decode(tag, &payload)
    }
}

/// The mutually-agreed streaming configuration produced by [`negotiate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgreedConfig {
    /// Agreed width in pixels (the host's offered width, which fits the client).
    pub width: u32,
    /// Agreed height in pixels.
    pub height: u32,
    /// Agreed refresh rate — the host offer clamped to the client's maximum.
    pub refresh_hz: u32,
    /// Agreed codec — the first host-preferred codec the client also supports.
    pub codec: VideoCodec,
}

/// Why a handshake could not be reconciled into an [`AgreedConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NegotiationError {
    /// The two peers run incompatible protocol versions.
    VersionMismatch {
        /// The host's `protocol_version`.
        host: u32,
        /// The client's `protocol_version`.
        client: u32,
    },
    /// The host and client codec sets do not intersect.
    NoCommonCodec,
    /// The host's offered resolution exceeds the client's maximum panel size.
    ResolutionUnsupported {
        /// The host-offered `(width, height)`.
        offered: (u32, u32),
        /// The client's `(max_width, max_height)`.
        client_max: (u32, u32),
    },
}

/// Reconcile a host [`Handshake`] offer against the client's [`ClientCaps`].
///
/// Pure function (no I/O, no global state). This reconciles the two **peers** against
/// each other; it does not itself consult [`crate::protocol_version`]. The host is
/// responsible for populating `host.protocol_version` from `protocol_version()` (it is
/// the encoder running this build) — so peer-equality is the meaningful check here.
///
/// Resolution order:
/// 1. Protocol versions must be equal (else [`NegotiationError::VersionMismatch`]).
/// 2. Pick the first **host-preferred** codec the client also supports (else
///    [`NegotiationError::NoCommonCodec`]).
/// 3. The offered resolution must fit the client panel (else
///    [`NegotiationError::ResolutionUnsupported`]); equal dimensions fit.
/// 4. The refresh rate is the host offer clamped down to the client's maximum.
pub fn negotiate(host: &Handshake, client: &ClientCaps) -> Result<AgreedConfig, NegotiationError> {
    if host.protocol_version != client.protocol_version {
        return Err(NegotiationError::VersionMismatch {
            host: host.protocol_version,
            client: client.protocol_version,
        });
    }

    let codec = host
        .codecs
        .iter()
        .copied()
        .find(|c| client.codecs.contains(c))
        .ok_or(NegotiationError::NoCommonCodec)?;

    if host.width > client.max_width || host.height > client.max_height {
        return Err(NegotiationError::ResolutionUnsupported {
            offered: (host.width, host.height),
            client_max: (client.max_width, client.max_height),
        });
    }

    Ok(AgreedConfig {
        width: host.width,
        height: host.height,
        refresh_hz: host.refresh_hz.min(client.max_refresh_hz),
        codec,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{nal, protocol_version};
    use std::io::Cursor;

    fn roundtrip(frame: &Frame) -> Frame {
        let mut buf = Vec::new();
        frame.write_to(&mut buf).expect("write_to");
        let mut cur = Cursor::new(buf);
        Frame::read_from(&mut cur).expect("read_from")
    }

    #[test]
    fn roundtrip_handshake() {
        let frame = Frame::Handshake(Handshake {
            protocol_version: protocol_version(),
            width: 2400,
            height: 1080,
            refresh_hz: 60,
            codecs: vec![VideoCodec::H264],
        });
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn roundtrip_video_config() {
        // SPS/PPS carried faithfully from a real nal::extract_codec_config output.
        let mut stream = Vec::new();
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(&[0x67, 0x42, 0x1F]); // SPS
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(&[0x68, 0xCE]); // PPS
        let cfg = nal::extract_codec_config(&stream).expect("config present");

        let mut sps_pps = cfg.sps.clone();
        sps_pps.extend_from_slice(&cfg.pps);
        let frame = Frame::VideoConfig {
            codec: VideoCodec::H264,
            sps_pps,
        };
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn roundtrip_video_normal() {
        let frame = Frame::Video {
            pts_us: 123,
            keyframe: true,
            nal: vec![0, 1, 2, 3],
        };
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn roundtrip_video_empty_nal() {
        let frame = Frame::Video {
            pts_us: 7,
            keyframe: false,
            nal: vec![],
        };
        // Header-only payload is exactly 9 bytes.
        let (_, payload) = frame.to_tag_payload().unwrap();
        assert_eq!(payload.len(), 9);
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn roundtrip_video_large_nal() {
        let frame = Frame::Video {
            pts_us: 999_999,
            keyframe: true,
            nal: (0..100_000u32).map(|i| i as u8).collect(),
        };
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn roundtrip_touch() {
        // Exactly-representable f32 values so PartialEq is exact.
        let frame = Frame::Touch(TouchEvent {
            pointer_id: 1,
            phase: TouchPhase::Move,
            nx: 0.5,
            ny: 0.25,
        });
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn roundtrip_control_variants() {
        for c in [
            Control::RequestKeyframe,
            Control::Pause,
            Control::Resume,
            Control::Bye,
        ] {
            let frame = Frame::Control(c.clone());
            assert_eq!(roundtrip(&frame), frame);
        }
    }

    #[test]
    fn roundtrip_multi_frame_stream() {
        // Three different frames into ONE buffer, decoded back in order — proves the
        // self-delimiting framing is reused, not reinvented.
        let frames = vec![
            Frame::Control(Control::RequestKeyframe),
            Frame::Video {
                pts_us: 42,
                keyframe: false,
                nal: vec![0xAA, 0xBB, 0xCC],
            },
            Frame::Touch(TouchEvent {
                pointer_id: 9,
                phase: TouchPhase::Down,
                nx: 0.0,
                ny: 1.0,
            }),
        ];
        let mut buf = Vec::new();
        for f in &frames {
            f.write_to(&mut buf).unwrap();
        }
        let mut cur = Cursor::new(buf);
        for expected in &frames {
            assert_eq!(&Frame::read_from(&mut cur).unwrap(), expected);
        }
    }

    #[test]
    fn video_payload_raw_layout() {
        let frame = Frame::Video {
            pts_us: 0x0102_0304_0506_0708,
            keyframe: true,
            nal: vec![0xAA, 0xBB],
        };
        let (tag, payload) = frame.to_tag_payload().unwrap();
        assert_eq!(tag, tag::VIDEO);
        // [pts u64 BE][keyframe = 1][nal verbatim] — no serde/postcard prefix anywhere.
        assert_eq!(
            payload,
            vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x01, 0xAA, 0xBB]
        );
    }

    #[test]
    fn video_payload_keyframe_false_byte() {
        let frame = Frame::Video {
            pts_us: 0,
            keyframe: false,
            nal: vec![],
        };
        let (_, payload) = frame.to_tag_payload().unwrap();
        // 9th byte (index 8) is the keyframe flag = 0.
        assert_eq!(payload[8], 0x00);
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        // A valid Handshake payload with one extra byte appended must be rejected,
        // not silently accepted — decode is canonical (consumes the whole payload).
        let frame = Frame::Handshake(Handshake {
            protocol_version: protocol_version(),
            width: 2400,
            height: 1080,
            refresh_hz: 60,
            codecs: vec![VideoCodec::H264],
        });
        let (tag, mut payload) = frame.to_tag_payload().unwrap();
        payload.push(0xAA); // trailing garbage
        match Frame::decode(tag, &payload) {
            Err(MessageError::TrailingBytes) => {}
            other => panic!("expected TrailingBytes, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_unknown_tag() {
        match Frame::decode(99, &[]) {
            Err(MessageError::UnknownTag(99)) => {}
            other => panic!("expected UnknownTag(99), got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_short_video_header() {
        match Frame::decode(tag::VIDEO, &[0, 0, 0]) {
            Err(MessageError::ShortVideoHeader) => {}
            other => panic!("expected ShortVideoHeader, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_malformed_postcard() {
        // Garbage bytes for the Handshake struct must not panic; surfaces as a
        // Decode error that carries the underlying postcard error.
        match Frame::decode(tag::HANDSHAKE, &[0xFF; 3]) {
            Err(MessageError::Decode(_)) => {}
            other => panic!("expected Decode(_), got {other:?}"),
        }
    }

    #[test]
    fn video_config_payload_wire_is_stable_struct_encoding() {
        // VideoConfig encodes via the named VideoConfigPayload struct. Pin the exact
        // postcard bytes so a future field addition can't silently change the wire.
        let frame = Frame::VideoConfig {
            codec: VideoCodec::H264,
            sps_pps: vec![0x67, 0x42],
        };
        let (tag, payload) = frame.to_tag_payload().unwrap();
        assert_eq!(tag, tag::VIDEO_CONFIG);
        // postcard: enum variant index 0 (H264) = 0x00; Vec len 2 = 0x02; then bytes.
        assert_eq!(payload, vec![0x00, 0x02, 0x67, 0x42]);
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn write_oversized_video_surfaces_framing_error() {
        // No new guard in messages: oversized payloads are rejected by framing's
        // MAX_FRAME_LEN before any wire write. Surfaces as MessageError::Io.
        let frame = Frame::Video {
            pts_us: 0,
            keyframe: true,
            nal: vec![0u8; framing::MAX_FRAME_LEN as usize + 1],
        };
        let mut buf = Vec::new();
        match frame.write_to(&mut buf) {
            Err(MessageError::Io(e)) => {
                assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
            }
            other => panic!("expected Io(InvalidInput), got {other:?}"),
        }
    }

    // ---- Task 2: negotiate() matrix -------------------------------------------------

    fn host(width: u32, height: u32, refresh_hz: u32, codecs: Vec<VideoCodec>) -> Handshake {
        Handshake {
            protocol_version: 1,
            width,
            height,
            refresh_hz,
            codecs,
        }
    }

    fn client(
        max_width: u32,
        max_height: u32,
        max_refresh_hz: u32,
        codecs: Vec<VideoCodec>,
    ) -> ClientCaps {
        ClientCaps {
            protocol_version: 1,
            max_width,
            max_height,
            max_refresh_hz,
            codecs,
        }
    }

    #[test]
    fn negotiate_happy_path() {
        let h = host(2400, 1080, 60, vec![VideoCodec::H264]);
        let c = client(2400, 1080, 60, vec![VideoCodec::H264]);
        assert_eq!(
            negotiate(&h, &c),
            Ok(AgreedConfig {
                width: 2400,
                height: 1080,
                refresh_hz: 60,
                codec: VideoCodec::H264,
            })
        );
    }

    #[test]
    fn negotiate_accepts_peers_at_current_protocol_version() {
        // Both sides derive their version from protocol_version() (the host's source of
        // truth) — not a literal 1 — and negotiation succeeds. negotiate() checks peer
        // equality; anchoring the host's version to protocol_version() is the caller's
        // job (see fn docs), exercised here.
        let mut h = host(2400, 1080, 60, vec![VideoCodec::H264]);
        h.protocol_version = protocol_version();
        let mut c = client(2400, 1080, 60, vec![VideoCodec::H264]);
        c.protocol_version = protocol_version();
        assert!(negotiate(&h, &c).is_ok());
    }

    #[test]
    fn negotiate_rejects_version_mismatch() {
        let mut h = host(2400, 1080, 60, vec![VideoCodec::H264]);
        h.protocol_version = 2;
        let c = client(2400, 1080, 60, vec![VideoCodec::H264]);
        assert_eq!(
            negotiate(&h, &c),
            Err(NegotiationError::VersionMismatch { host: 2, client: 1 })
        );
    }

    #[test]
    fn negotiate_rejects_no_common_codec() {
        let h = host(2400, 1080, 60, vec![VideoCodec::Hevc]);
        let c = client(2400, 1080, 60, vec![VideoCodec::H264]);
        assert_eq!(negotiate(&h, &c), Err(NegotiationError::NoCommonCodec));
    }

    #[test]
    fn negotiate_host_preferred_codec_wins() {
        // Intersection is host-preference-ordered, NOT client-ordered.
        let h = host(2400, 1080, 60, vec![VideoCodec::Hevc, VideoCodec::H264]);
        let c = client(2400, 1080, 60, vec![VideoCodec::H264, VideoCodec::Hevc]);
        assert_eq!(negotiate(&h, &c).unwrap().codec, VideoCodec::Hevc);
    }

    #[test]
    fn negotiate_rejects_resolution_too_big() {
        let h = host(3840, 2160, 60, vec![VideoCodec::H264]);
        let c = client(2400, 1080, 60, vec![VideoCodec::H264]);
        assert_eq!(
            negotiate(&h, &c),
            Err(NegotiationError::ResolutionUnsupported {
                offered: (3840, 2160),
                client_max: (2400, 1080),
            })
        );
    }

    #[test]
    fn negotiate_clamps_refresh_down() {
        let h = host(2400, 1080, 60, vec![VideoCodec::H264]);
        let c = client(2400, 1080, 30, vec![VideoCodec::H264]);
        assert_eq!(negotiate(&h, &c).unwrap().refresh_hz, 30);
    }

    #[test]
    fn negotiate_clamp_up_does_not_exceed_host() {
        let h = host(2400, 1080, 60, vec![VideoCodec::H264]);
        let c = client(2400, 1080, 90, vec![VideoCodec::H264]);
        assert_eq!(negotiate(&h, &c).unwrap().refresh_hz, 60);
    }

    #[test]
    fn negotiate_boundary_fit() {
        // Exactly equal dimensions fit (the check is `>`, not `>=`).
        let h = host(2400, 1080, 60, vec![VideoCodec::H264]);
        let c = client(2400, 1080, 60, vec![VideoCodec::H264]);
        assert!(negotiate(&h, &c).is_ok());
    }
}
