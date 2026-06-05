//! The `VideoDecoder` port (seam) and the cable-free [`DecodeSession`] orchestrator that
//! drives it from protocol [`Frame`]s — all platform-agnostic and host-tested.
//!
//! This mirrors the macOS host's `encode` module (the `Encoder` port + `run_session`
//! pipeline + a test fake): the *simplest-now* adapter behind [`VideoDecoder`] is the
//! Android `AMediaCodec` decode-to-surface path, but that is hardware (needs the Pixel),
//! so it lands in Wave B. Everything here — turning a stream of `Frame`s into
//! `configure` + `decode` calls, tracking codec config / keyframes, and refusing a
//! `Video` that arrives before any config — is provable in CI against a fake decoder with
//! NO device.
//!
//! ## P4 Wave B (hardware, deferred — needs the Pixel 6a)
//! The concrete `AMediaCodec` adapter implementing [`VideoDecoder`] (a thin `ndk-sys`
//! wrapper that configures the codec with SPS/PPS as `csd-0` and queues each access unit
//! for decode-to-surface onto an `ANativeWindow`, D3 — no copy), the JNI surface
//! plumbing, and the Kotlin `SurfaceView` shell are all device-blocked. They drop in
//! behind this port (`#[cfg(target_os = "android")]`, the same discipline as
//! `transport::AccessoryFdTransport`) without touching the orchestration tested here. NO
//! `ndk`/`ndk-sys`/JNI dependency is added until that gated task — the default
//! `cargo build --workspace` pulls nothing new.

use protocol::messages::{Frame, VideoCodec};
use protocol::nal::{self, AccessUnitError, CodecConfig};

/// One access unit handed to the decoder: a decoder-ready Annex-B byte buffer plus the
/// metadata `AMediaCodec` needs to queue it (`presentationTimeUs`, keyframe flag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoderInput {
    /// Presentation timestamp in microseconds (carried through from `Frame::Video`),
    /// passed straight to `AMediaCodec.queueInputBuffer`.
    pub pts_us: u64,
    /// Whether this access unit is a keyframe (IDR / random-access point).
    pub keyframe: bool,
    /// The decoder-ready Annex-B access unit (see [`nal::to_annex_b_access_unit`]).
    pub annex_b: Vec<u8>,
}

/// The Pixel decode seam (R4 — owning the MediaCodec wrapper rather than a stale crate).
///
/// The simplest-now adapter is `AMediaCodec` decode-to-surface. Both methods return a
/// `Result` so the Wave-B adapter can surface codec errors (`AMediaCodec` status codes,
/// a dequeue timeout, a configure failure) without panicking across the FFI boundary.
pub trait VideoDecoder {
    /// Configure the decoder with the codec and its parameter sets (SPS/PPS → `csd-0`).
    /// Called once before the first access unit, and again if the config changes.
    fn configure(&mut self, codec: VideoCodec, config: &CodecConfig) -> Result<(), DecodeError>;

    /// Submit one decoder-ready access unit for decode (decode-to-surface in Wave B).
    fn decode(&mut self, input: &DecoderInput) -> Result<(), DecodeError>;
}

/// Why driving the decoder failed.
#[derive(Debug)]
pub enum DecodeError {
    /// A `Frame::Video` arrived before any `Frame::VideoConfig` (or before an in-band
    /// keyframe established the codec config) — the decoder cannot be configured, so the
    /// access unit cannot be decoded.
    NoConfig,
    /// The video payload could not be turned into a decoder-ready access unit (e.g. an
    /// empty NAL buffer). Wraps the underlying [`AccessUnitError`].
    AccessUnit(AccessUnitError),
    /// The underlying decoder adapter (Wave B `AMediaCodec`) reported an error. Carries a
    /// human-readable detail so a device-side failure stays debuggable.
    Adapter(String),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::NoConfig => {
                write!(f, "received a Video frame before any VideoConfig")
            }
            DecodeError::AccessUnit(e) => write!(f, "could not build access unit: {e}"),
            DecodeError::Adapter(msg) => write!(f, "decoder adapter error: {msg}"),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DecodeError::AccessUnit(e) => Some(e),
            _ => None,
        }
    }
}

impl From<AccessUnitError> for DecodeError {
    fn from(e: AccessUnitError) -> Self {
        DecodeError::AccessUnit(e)
    }
}

/// Drives a [`VideoDecoder`] from a stream of protocol [`Frame`]s — the platform-agnostic
/// core the `AMediaCodec` adapter plugs into (R4 seam).
///
/// Feed it frames one at a time with [`feed`](Self::feed):
/// - [`Frame::VideoConfig`] → split the concatenated SPS/PPS into a [`CodecConfig`] and
///   `configure` the decoder (the config is remembered for later access-unit injection).
/// - [`Frame::Video`] → build a decoder-ready Annex-B access unit (injecting the remembered
///   SPS/PPS ahead of a bare keyframe — see [`nal::to_annex_b_access_unit`]) and `decode`
///   it. A `Video` before any usable config is rejected with [`DecodeError::NoConfig`].
/// - other variants (`Handshake` / `Touch` / `Control`) are not part of the decode path
///   and are ignored here (the session layer handles them).
///
/// The session also derives the config from an **in-band keyframe** when no `VideoConfig`
/// was sent first: a self-describing keyframe (SPS+PPS+IDR in its NAL bytes) lets the
/// decoder configure itself, so it is accepted rather than rejected.
#[derive(Debug)]
pub struct DecodeSession {
    /// The established codec config, used to inject SPS/PPS ahead of a bare keyframe and
    /// to gate the first `Video`. A `VideoConfig` frame reconfigures the decoder only when
    /// its SPS/PPS differ from the current config — the host re-sends an identical config
    /// before every keyframe, and re-applying it would force an unsupported mid-stream
    /// reconfigure. (Codec type is not compared; a codec-only change can't occur for the
    /// H.264-only MVP, where SPS/PPS bytes already encode the codec.)
    /// An in-band keyframe only establishes it when none is set yet — it does not override
    /// a prior config.
    config: Option<CodecConfig>,
    /// The negotiated codec (from `VideoConfig`); defaults to H.264 when config is first
    /// learned from an in-band keyframe (D2: H.264 is the only MVP codec).
    codec: VideoCodec,
    /// Number of access units submitted to the decoder.
    decoded: u64,
    /// Number of keyframes submitted.
    keyframes: u64,
    /// Whether the decoder has been configured at least once.
    configured: bool,
}

impl Default for DecodeSession {
    fn default() -> Self {
        Self::new()
    }
}

impl DecodeSession {
    /// A fresh session with no config seen yet.
    pub fn new() -> Self {
        Self {
            config: None,
            codec: VideoCodec::H264,
            decoded: 0,
            keyframes: 0,
            configured: false,
        }
    }

    /// Whether codec config (SPS/PPS) has been established (via `VideoConfig` or an
    /// in-band keyframe) and the decoder configured.
    pub fn configured(&self) -> bool {
        self.configured
    }

    /// Number of access units submitted to the decoder so far.
    pub fn decoded_count(&self) -> u64 {
        self.decoded
    }

    /// Number of keyframes submitted so far.
    pub fn keyframe_count(&self) -> u64 {
        self.keyframes
    }

    /// Feed one protocol [`Frame`] to the session, driving `decoder` as needed.
    ///
    /// Returns `Ok(())` for handled and ignored frames alike; returns a [`DecodeError`]
    /// when a `Video` cannot be decoded (no config, empty payload, or an adapter error).
    pub fn feed(
        &mut self,
        frame: &Frame,
        decoder: &mut dyn VideoDecoder,
    ) -> Result<(), DecodeError> {
        match frame {
            Frame::VideoConfig { codec, sps_pps } => {
                if let Some(config) = nal::extract_codec_config(sps_pps) {
                    // The host re-sends VideoConfig before EVERY keyframe so a late-joining
                    // client can start decoding. Only (re)configure when the config actually
                    // changes: re-applying an identical config would trigger a mid-stream
                    // reconfigure that the Wave-B AMediaCodec adapter rejects, killing the
                    // session (the live black-screen-after-~0.5s bug). A genuinely different
                    // config (e.g. a resolution change) still reaches configure() — and is
                    // still rejected by the MVP adapter, unchanged from before.
                    if self.config.as_ref() != Some(&config) {
                        self.codec = *codec;
                        decoder.configure(*codec, &config)?;
                        self.config = Some(config);
                        self.configured = true;
                    }
                }
                // A VideoConfig whose bytes carry no usable SPS+PPS is ignored: the next
                // in-band keyframe can still establish the config. (Defensive — the host
                // always sends a valid pair.)
                Ok(())
            }
            Frame::Video {
                pts_us,
                keyframe,
                nal,
            } => self.feed_video(*pts_us, *keyframe, nal, decoder),
            // Not part of the decode path; the session layer routes these elsewhere.
            Frame::Handshake(_) | Frame::Touch(_) | Frame::Control(_) => Ok(()),
        }
    }

    fn feed_video(
        &mut self,
        pts_us: u64,
        keyframe: bool,
        nal: &[u8],
        decoder: &mut dyn VideoDecoder,
    ) -> Result<(), DecodeError> {
        // The `keyframe` flag is trusted from the protocol (the host is authoritative);
        // it is not cross-checked against nal::is_keyframe(nal).
        // A keyframe carrying its own in-band SPS/PPS can configure the decoder even if no
        // VideoConfig was sent first — recover the config from it.
        if self.config.is_none() && keyframe {
            if let Some(config) = nal::extract_codec_config(nal) {
                decoder.configure(self.codec, &config)?;
                self.config = Some(config);
                self.configured = true;
            }
        }

        if !self.configured {
            return Err(DecodeError::NoConfig);
        }

        let annex_b = nal::to_annex_b_access_unit(nal, keyframe, self.config.as_ref())?;
        let input = DecoderInput {
            pts_us,
            keyframe,
            annex_b,
        };
        decoder.decode(&input)?;
        self.decoded += 1;
        if keyframe {
            self.keyframes += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One recorded interaction with the decoder, so the orchestration is provable in CI
    /// with no device — the decode-side analogue of the host's `FakeEncoder`/`RecordingSink`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Configure(VideoCodec, CodecConfig),
        Decode(DecoderInput),
    }

    /// Test-only [`VideoDecoder`] that records every `configure`/`decode` call. An optional
    /// `fail_on_decode_index` makes a chosen `decode` return an adapter error, so error
    /// propagation is testable too.
    #[derive(Debug, Default)]
    struct RecordingDecoder {
        calls: Vec<Call>,
        fail_on_decode_index: Option<u64>,
        decode_index: u64,
    }

    impl VideoDecoder for RecordingDecoder {
        fn configure(
            &mut self,
            codec: VideoCodec,
            config: &CodecConfig,
        ) -> Result<(), DecodeError> {
            self.calls.push(Call::Configure(codec, config.clone()));
            Ok(())
        }

        fn decode(&mut self, input: &DecoderInput) -> Result<(), DecodeError> {
            let idx = self.decode_index;
            self.decode_index += 1;
            if self.fail_on_decode_index == Some(idx) {
                return Err(DecodeError::Adapter("simulated codec failure".into()));
            }
            self.calls.push(Call::Decode(input.clone()));
            Ok(())
        }
    }

    const SC4: [u8; 4] = [0, 0, 0, 1];

    /// SPS/PPS concatenated the way `VideoConfig.sps_pps` carries them: Annex-B framed.
    fn sps_pps_bytes() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&SC4);
        v.extend_from_slice(&[0x67, 0x42, 0x1F]); // SPS
        v.extend_from_slice(&SC4);
        v.extend_from_slice(&[0x68, 0xCE]); // PPS
        v
    }

    fn expected_config() -> CodecConfig {
        CodecConfig {
            sps: vec![0x67, 0x42, 0x1F],
            pps: vec![0x68, 0xCE],
        }
    }

    fn video_config_frame() -> Frame {
        Frame::VideoConfig {
            codec: VideoCodec::H264,
            sps_pps: sps_pps_bytes(),
        }
    }

    /// A bare IDR keyframe access unit (no in-band SPS/PPS).
    fn bare_keyframe(pts_us: u64) -> Frame {
        let mut nal = Vec::new();
        nal.extend_from_slice(&SC4);
        nal.extend_from_slice(&[0x65, 0xAA, 0xBB]); // IDR
        Frame::Video {
            pts_us,
            keyframe: true,
            nal,
        }
    }

    /// A non-keyframe (delta) access unit.
    fn delta_frame(pts_us: u64) -> Frame {
        let mut nal = Vec::new();
        nal.extend_from_slice(&SC4);
        nal.extend_from_slice(&[0x61, 0xCC]); // non-IDR slice
        Frame::Video {
            pts_us,
            keyframe: false,
            nal,
        }
    }

    #[test]
    fn config_frame_configures_decoder() {
        let mut dec = RecordingDecoder::default();
        let mut session = DecodeSession::new();
        assert!(!session.configured());

        session.feed(&video_config_frame(), &mut dec).unwrap();

        assert!(session.configured());
        assert_eq!(
            dec.calls,
            vec![Call::Configure(VideoCodec::H264, expected_config())]
        );
    }

    #[test]
    fn video_before_config_is_rejected() {
        let mut dec = RecordingDecoder::default();
        let mut session = DecodeSession::new();

        // A delta frame with no prior config and no in-band SPS/PPS cannot be decoded.
        match session.feed(&delta_frame(0), &mut dec) {
            Err(DecodeError::NoConfig) => {}
            other => panic!("expected NoConfig, got {other:?}"),
        }
        assert!(dec.calls.is_empty(), "decoder must not be touched");
        assert_eq!(session.decoded_count(), 0);
    }

    #[test]
    fn config_then_keyframe_injects_sps_pps_then_decodes() {
        let mut dec = RecordingDecoder::default();
        let mut session = DecodeSession::new();

        session.feed(&video_config_frame(), &mut dec).unwrap();
        session.feed(&bare_keyframe(1000), &mut dec).unwrap();

        assert_eq!(session.decoded_count(), 1);
        assert_eq!(session.keyframe_count(), 1);

        // The decoded access unit must be self-describing: SPS/PPS injected ahead of the
        // bare IDR, recoverable by extract_codec_config.
        let decode_input = match &dec.calls[1] {
            Call::Decode(input) => input,
            other => panic!("expected a Decode call, got {other:?}"),
        };
        assert_eq!(decode_input.pts_us, 1000);
        assert!(decode_input.keyframe);
        let cfg = protocol::nal::extract_codec_config(&decode_input.annex_b)
            .expect("injected config present in AU");
        assert_eq!(cfg, expected_config());
    }

    #[test]
    fn in_band_keyframe_configures_without_video_config() {
        // No VideoConfig first: a self-describing keyframe (SPS+PPS+IDR in-band) must
        // configure the decoder from its own bytes and then decode.
        let mut nal = Vec::new();
        nal.extend_from_slice(&SC4);
        nal.extend_from_slice(&[0x67, 0x01]); // SPS
        nal.extend_from_slice(&SC4);
        nal.extend_from_slice(&[0x68, 0x01]); // PPS
        nal.extend_from_slice(&SC4);
        nal.extend_from_slice(&[0x65, 0x00]); // IDR
        let frame = Frame::Video {
            pts_us: 42,
            keyframe: true,
            nal: nal.clone(),
        };

        let mut dec = RecordingDecoder::default();
        let mut session = DecodeSession::new();
        session.feed(&frame, &mut dec).unwrap();

        assert!(session.configured());
        assert_eq!(session.decoded_count(), 1);
        // configure (from in-band params) then decode (the AU passed through unchanged —
        // already self-describing, so no duplicate SPS/PPS).
        assert_eq!(
            dec.calls,
            vec![
                Call::Configure(
                    VideoCodec::H264,
                    CodecConfig {
                        sps: vec![0x67, 0x01],
                        pps: vec![0x68, 0x01],
                    }
                ),
                Call::Decode(DecoderInput {
                    pts_us: 42,
                    keyframe: true,
                    annex_b: nal,
                }),
            ]
        );
    }

    #[test]
    fn delta_after_config_decodes_without_reconfigure() {
        let mut dec = RecordingDecoder::default();
        let mut session = DecodeSession::new();

        session.feed(&video_config_frame(), &mut dec).unwrap();
        session.feed(&bare_keyframe(0), &mut dec).unwrap();
        session.feed(&delta_frame(16_666), &mut dec).unwrap();

        assert_eq!(session.decoded_count(), 2);
        assert_eq!(session.keyframe_count(), 1);
        // Exactly one configure (from VideoConfig); the delta is passed through unchanged.
        let configures = dec
            .calls
            .iter()
            .filter(|c| matches!(c, Call::Configure(..)))
            .count();
        assert_eq!(configures, 1);
        match dec.calls.last().unwrap() {
            Call::Decode(input) => {
                assert!(!input.keyframe);
                // A delta carries no SPS/PPS — none injected.
                assert_eq!(protocol::nal::extract_codec_config(&input.annex_b), None);
            }
            other => panic!("expected a Decode call, got {other:?}"),
        }
    }

    #[test]
    fn empty_video_payload_surfaces_access_unit_error() {
        let mut dec = RecordingDecoder::default();
        let mut session = DecodeSession::new();
        session.feed(&video_config_frame(), &mut dec).unwrap();

        let empty = Frame::Video {
            pts_us: 0,
            keyframe: false,
            nal: vec![],
        };
        match session.feed(&empty, &mut dec) {
            Err(DecodeError::AccessUnit(AccessUnitError::EmptyPayload)) => {}
            other => panic!("expected AccessUnit(EmptyPayload), got {other:?}"),
        }
        assert_eq!(session.decoded_count(), 0);
    }

    #[test]
    fn adapter_decode_error_propagates() {
        let mut dec = RecordingDecoder {
            fail_on_decode_index: Some(0),
            ..Default::default()
        };
        let mut session = DecodeSession::new();
        session.feed(&video_config_frame(), &mut dec).unwrap();

        match session.feed(&bare_keyframe(0), &mut dec) {
            Err(DecodeError::Adapter(_)) => {}
            other => panic!("expected Adapter error, got {other:?}"),
        }
        // A failed decode must not be counted.
        assert_eq!(session.decoded_count(), 0);
    }

    #[test]
    fn non_video_frames_are_ignored() {
        use protocol::messages::Control;
        let mut dec = RecordingDecoder::default();
        let mut session = DecodeSession::new();

        session
            .feed(&Frame::Control(Control::RequestKeyframe), &mut dec)
            .unwrap();
        assert!(dec.calls.is_empty());
        assert!(!session.configured());
        assert_eq!(session.decoded_count(), 0);
    }

    #[test]
    fn second_config_reconfigures_decoder() {
        // A mid-stream resolution change re-sends VideoConfig; the session reconfigures.
        let mut dec = RecordingDecoder::default();
        let mut session = DecodeSession::new();
        session.feed(&video_config_frame(), &mut dec).unwrap();

        let mut other = Vec::new();
        other.extend_from_slice(&SC4);
        other.extend_from_slice(&[0x67, 0x99]); // different SPS
        other.extend_from_slice(&SC4);
        other.extend_from_slice(&[0x68, 0x99]); // different PPS
        session
            .feed(
                &Frame::VideoConfig {
                    codec: VideoCodec::H264,
                    sps_pps: other,
                },
                &mut dec,
            )
            .unwrap();

        let configures = dec
            .calls
            .iter()
            .filter(|c| matches!(c, Call::Configure(..)))
            .count();
        assert_eq!(configures, 2);
    }

    #[test]
    fn identical_second_config_does_not_reconfigure() {
        // The live host re-sends the SAME VideoConfig before every keyframe. The session must
        // configure exactly once — re-applying an identical config would force an unsupported
        // mid-stream reconfigure in the Wave-B adapter and kill the session.
        let mut dec = RecordingDecoder::default();
        let mut session = DecodeSession::new();
        session.feed(&video_config_frame(), &mut dec).unwrap();
        session.feed(&video_config_frame(), &mut dec).unwrap();

        let configures = dec
            .calls
            .iter()
            .filter(|c| matches!(c, Call::Configure(..)))
            .count();
        assert_eq!(configures, 1);
    }

    #[test]
    fn drives_a_realistic_frame_sequence() {
        // VideoConfig, keyframe, two deltas, another keyframe — the live cadence.
        let mut dec = RecordingDecoder::default();
        let mut session = DecodeSession::new();
        for frame in [
            video_config_frame(),
            bare_keyframe(0),
            delta_frame(16_666),
            delta_frame(33_333),
            bare_keyframe(50_000),
        ] {
            session.feed(&frame, &mut dec).unwrap();
        }
        assert_eq!(session.decoded_count(), 4);
        assert_eq!(session.keyframe_count(), 2);
        assert!(session.configured());
    }
}
