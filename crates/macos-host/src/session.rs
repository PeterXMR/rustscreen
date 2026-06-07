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
        AgreedConfig, ClientCaps, Control, Frame, Handshake, MessageError, NegotiationError,
        VideoCodec,
    },
    nal::{self, CodecConfig},
    protocol_version,
};

use crate::{
    capture::Capturer,
    encode::{EncodedFrame, Encoder, LatencyStats},
    latency::PipelineLatency,
};
use protocol::clock::ClockOffset;

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
// Clock sync
// ---------------------------------------------------------------------------

/// Why a clock-sync exchange failed.
#[derive(Debug)]
pub enum ClockSyncError {
    /// The reply frame was not a `ClockPong`.
    UnexpectedReply,
    /// A framing/codec error reading or writing the exchange.
    Message(protocol::messages::MessageError),
}

impl std::fmt::Display for ClockSyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClockSyncError::UnexpectedReply => write!(f, "clock-sync reply was not a ClockPong"),
            ClockSyncError::Message(e) => write!(f, "clock-sync I/O: {e}"),
        }
    }
}
impl std::error::Error for ClockSyncError {}
impl From<protocol::messages::MessageError> for ClockSyncError {
    fn from(e: protocol::messages::MessageError) -> Self {
        ClockSyncError::Message(e)
    }
}

/// Run one SNTP-style clock-sync exchange over `transport`, returning the estimated
/// [`protocol::clock::ClockOffset`]. `now_us` supplies host-clock microseconds (injected
/// for testability): called once for `t0` (before the ping) and once for `t3` (after the
/// pong).
pub fn perform_clock_sync(
    transport: &mut (impl Read + Write),
    mut now_us: impl FnMut() -> u64,
) -> Result<protocol::clock::ClockOffset, ClockSyncError> {
    let t0 = now_us();
    Frame::ClockPing { t0_us: t0 }.write_to(transport)?;
    transport
        .flush()
        .map_err(|e| ClockSyncError::Message(e.into()))?;
    let reply = Frame::read_from(transport)?;
    let t3 = now_us();
    let Frame::ClockPong {
        t0_us: _echoed_t0,
        t1_us,
        t2_us,
    } = reply
    else {
        return Err(ClockSyncError::UnexpectedReply);
    };
    // Use the LOCAL `t0` captured before the ping, not the pong's echoed `t0_us`. A stale
    // or wrong pong (e.g. an echo from an earlier ping) would otherwise corrupt the offset;
    // anchoring on our own send timestamp keeps the estimate sound regardless of what the
    // peer echoes back.
    Ok(protocol::clock::estimate(t0, t1_us, t2_us, t3))
}

// ---------------------------------------------------------------------------
// Counting Write adapter (for bytes_sent tracking)
// ---------------------------------------------------------------------------

/// A `Write` wrapper that counts the total bytes written.
///
/// Generic over the concrete writer `W` (rather than `&mut dyn Write`) so LLVM can
/// monomorphize and inline through it on the per-frame send hot path — no vtable
/// dispatch per `write`/`flush`.
struct CountingWrite<'a, W: Write> {
    inner: &'a mut W,
    total: usize,
}

impl<'a, W: Write> CountingWrite<'a, W> {
    fn new(inner: &'a mut W) -> Self {
        Self { inner, total: 0 }
    }
}

impl<W: Write> Write for CountingWrite<'_, W> {
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
        let encode_micros = encoded.encode_micros;
        // Shared wire contract (config-on-keyframe + Video + flush) lives in send_encoded_frame.
        send_encoded_frame(
            encoded,
            &mut counting,
            &mut summary.codec_config,
            agreed.codec,
        )?;
        summary.latency.record(encode_micros);
        summary.frames += 1;
    }

    summary.bytes_sent = counting.total;
    Ok(summary)
}

/// Drive a host send-session from a channel of **pre-encoded** frames.
///
/// This is the live-pipeline counterpart to [`run_send_session`]. ScreenCaptureKit +
/// VideoToolbox deliver encoded frames asynchronously via a delegate callback (push), which
/// does not fit the pull-based `Capturer`/`Encoder` split. So the live bin's `SCStreamOutput`
/// delegate pushes each [`EncodedFrame`] into `frames`, and this function pulls them off the
/// channel and applies the **identical wire contract** as [`run_send_session`]: handshake,
/// then `Frame::VideoConfig` before every keyframe (connect + each IDR), `Frame::Video` per
/// access unit, flushing each frame (C-01). The loop ends when the channel closes (every
/// sender dropped, i.e. capture stopped) or when the transport errors (peer disconnect).
///
/// While idle it sends a `Control::Heartbeat` every [`STREAM_HEARTBEAT_INTERVAL`]: a static
/// screen produces no ScreenCaptureKit frames, so without this probe a peer disconnect would
/// go undetected (no write is attempted) and the loop would block forever. The heartbeat
/// write surfaces the disconnect as an I/O error; the phone ignores non-`Bye` control frames,
/// so it is decoder-neutral.
pub fn run_stream_session(
    frames: std::sync::mpsc::Receiver<EncodedFrame>,
    transport: &mut (impl Read + Write),
    offer: Handshake,
) -> Result<SendSessionSummary, SessionError> {
    run_stream_session_inner(frames, transport, offer, STREAM_HEARTBEAT_INTERVAL)
}

/// Interval between `Control::Heartbeat` liveness probes on an idle stream.
const STREAM_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Implementation of [`run_stream_session`], parameterised by the heartbeat interval so tests
/// can drive the idle/disconnect path without waiting a real second.
fn run_stream_session_inner(
    frames: std::sync::mpsc::Receiver<EncodedFrame>,
    transport: &mut (impl Read + Write),
    offer: Handshake,
    heartbeat: std::time::Duration,
) -> Result<SendSessionSummary, SessionError> {
    use std::sync::mpsc::RecvTimeoutError;

    let agreed = perform_handshake(transport, offer)?;

    let mut counting = CountingWrite::new(transport);
    let mut summary = SendSessionSummary {
        frames: 0,
        bytes_sent: 0,
        codec_config: None,
        latency: LatencyStats::new(),
        agreed_config: agreed.clone(),
    };

    loop {
        match frames.recv_timeout(heartbeat) {
            Ok(encoded) => {
                let encode_micros = encoded.encode_micros;
                send_encoded_frame(
                    encoded,
                    &mut counting,
                    &mut summary.codec_config,
                    agreed.codec,
                )?;
                summary.latency.record(encode_micros);
                summary.frames += 1;
            }
            // No frame within the heartbeat window — the screen may be static. Probe the
            // transport so a peer disconnect is detected promptly instead of blocking
            // forever; a write/flush error here means the peer is gone (handled by `?`).
            Err(RecvTimeoutError::Timeout) => {
                Frame::Control(Control::Heartbeat).write_to(&mut counting)?;
                counting.flush()?;
            }
            // Every sender dropped → capture stopped: end the stream cleanly.
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    summary.bytes_sent = counting.total;
    Ok(summary)
}

/// Drop-to-keyframe coalescing for the live send path (item 6 step 2). Given a backlog of
/// encoded access units drained from the encode channel (oldest first, as they would be sent),
/// return only the frames worth sending so host-side queue latency stays bounded:
///
/// * If the backlog contains a keyframe, drop **everything before the last keyframe** and return
///   the suffix from that IDR onward. The dropped P-frames are stale and their decode targets are
///   about to be replaced wholesale by the fresh IDR, so skipping them loses only intermediate
///   motion — the freshest *decodable* frame wins. Host queue residence can then never exceed one
///   keyframe interval (hence the periodic `MaxKeyFrameInterval` on the encoder).
/// * If there is **no** keyframe, every frame is returned in order: H.264 P-frames form a
///   dependency chain, so dropping one would corrupt the stream until the next keyframe — there is
///   no safe resync point to jump to.
///
/// Pure and allocation-light (a single `split_off`), so it is unit-tested off-hardware.
fn coalesce_to_latest_keyframe(mut backlog: Vec<EncodedFrame>) -> Vec<EncodedFrame> {
    match backlog.iter().rposition(|f| f.keyframe) {
        Some(last_kf) => backlog.split_off(last_kf),
        None => backlog,
    }
}

/// Drive an **instrumented** post-handshake stream loop over a write-only half, fusing live
/// glass-to-glass latency.
///
/// Unlike [`run_stream_session`], this function does **not** perform the handshake — the caller
/// (`p5_stream`) does the handshake and clock-sync on the full-duplex transport first, then
/// [`crate::aoa::AoaTransport::split`]s it so a dedicated reader thread can pull inbound
/// `Frame::Stats` (forwarded here via `stats_rx`) while this loop owns the write half.
///
/// For each encoded frame it captures monotonic host timestamps and calls
/// [`PipelineLatency::record_host`]:
/// - `capture_us` = the real capture timestamp the delegate stamped on the frame
///   ([`EncodedFrame::capture_us`]) — NOT derived from `encode_micros`, so capture→encode and the
///   glass-to-glass total reflect actual wall time including any mpsc-channel queueing,
/// - `encode_done_us` = the instant just before the send,
/// - `send_done_us` = the instant just after the write+flush returns.
///
/// After each send it non-blockingly drains `stats_rx`, fusing any phone `Frame::Stats` (via
/// `offset`, if clock-sync succeeded) into `pipeline`. The wire contract itself is unchanged:
/// it reuses [`send_encoded_frame`] and the same idle-heartbeat liveness probe as
/// [`run_stream_session_inner`].
///
/// `now_us` supplies monotonic host-clock microseconds (injected for testability). Every
/// `report_each` of wall time the loop invokes `on_report` with a snapshot of the accumulated
/// [`crate::latency::LatencyReport`] so the caller can print a periodic latency report without
/// this (platform-agnostic) module taking on any I/O; the caller is expected to print one final
/// report after this function returns. The loop ends when the frame channel closes (capture
/// stopped) or the transport errors (peer disconnect).
#[allow(clippy::too_many_arguments)]
pub fn run_stream_session_instrumented(
    frames: &std::sync::mpsc::Receiver<EncodedFrame>,
    write_half: &mut impl Write,
    agreed: AgreedConfig,
    offset: Option<ClockOffset>,
    stats_rx: &std::sync::mpsc::Receiver<Frame>,
    pipeline: &mut PipelineLatency,
    heartbeat: std::time::Duration,
    report_each: std::time::Duration,
    mut now_us: impl FnMut() -> u64,
    mut on_report: impl FnMut(&crate::latency::LatencyReport),
    stop: &std::sync::atomic::AtomicBool,
) -> Result<SendSessionSummary, SessionError> {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Instant;

    let mut last_report = Instant::now();
    // Per-connection gate: the phone's MediaCodec decoder is brand new on every (re)connect and
    // can only start from a keyframe. Drop any P-frames that arrive before this connection's first
    // IDR — they are undecodable by a fresh decoder. `seen_keyframe` resets to `false` on every
    // call, so the guard is automatically re-armed after every reconnect (run_host calls this fn
    // fresh per connection).
    let mut seen_keyframe = false;

    // The handshake already negotiated this `agreed` config on the full-duplex transport before
    // the split; it is threaded in directly so the wire contract (codec) matches exactly what the
    // peer agreed to, with no re-derivation from the host's own offer.
    let mut counting = CountingWrite::new(write_half);
    let mut summary = SendSessionSummary {
        frames: 0,
        bytes_sent: 0,
        codec_config: None,
        latency: LatencyStats::new(),
        agreed_config: agreed.clone(),
    };

    loop {
        // External stop (e.g. `rustscreen stop` → SIGTERM) breaks the live stream so the
        // normal teardown below runs (final stats drain + byte tally). Checked between whole
        // frames, never mid-write, so the wire framing stays intact.
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match frames.recv_timeout(heartbeat) {
            Ok(first) => {
                // Drain everything that piled up in the channel while the previous (slow) USB
                // write was in flight, then drop-to-keyframe (item 6 step 2): under load we send
                // only [latest keyframe .. now], shedding stale P-frames so host-side queue
                // residence stays bounded to one keyframe interval. When the consumer keeps up,
                // the backlog is just `[first]` and nothing is dropped.
                let mut backlog = vec![first];
                while let Ok(f) = frames.try_recv() {
                    backlog.push(f);
                }
                let drained = backlog.len();
                let to_send = coalesce_to_latest_keyframe(backlog);
                let dropped = (drained - to_send.len()) as u64;
                if dropped > 0 {
                    pipeline.record_dropped(dropped);
                }

                for encoded in to_send {
                    // A (re)connected decoder can only start at a keyframe. Drop any P-frames that
                    // precede this connection's first keyframe — a fresh MediaCodec has no reference
                    // for them, so sending them would show one undecodable frame before the IDR. This
                    // closes the post-reconnect producer race (a lone P-frame can land before the
                    // forced IDR) with zero hot-path cost — it only sheds frames the decoder can't use.
                    // `seen_keyframe` is per-call, so it resets on every reconnect (run_host calls this
                    // fresh each connection).
                    if !seen_keyframe {
                        if !encoded.keyframe {
                            pipeline.record_dropped(1);
                            continue;
                        }
                        seen_keyframe = true;
                    }

                    let pts_us = encoded.pts_us;
                    let encode_micros = encoded.encode_micros;
                    // Real capture time stamped by the delegate (shared monotonic origin), NOT
                    // `encode_done_us - encode_micros`: that would make capture→encode
                    // tautologically equal to encode_micros and hide mpsc-channel queue latency.
                    let capture_us = encoded.capture_us;
                    let encode_done_us = now_us();

                    send_encoded_frame(
                        encoded,
                        &mut counting,
                        &mut summary.codec_config,
                        agreed.codec,
                    )?;

                    let send_done_us = now_us();
                    pipeline.record_host(pts_us, capture_us, encode_done_us, send_done_us);
                    summary.latency.record(encode_micros);
                    summary.frames += 1;
                }

                // Non-blockingly drain whatever phone Stats the reader thread has forwarded.
                drain_stats(stats_rx, offset, pipeline);
            }
            // No frame within the heartbeat window — probe the transport so a disconnect is
            // detected promptly instead of blocking forever. Also drain any pending stats.
            Err(RecvTimeoutError::Timeout) => {
                Frame::Control(Control::Heartbeat).write_to(&mut counting)?;
                counting.flush()?;
                drain_stats(stats_rx, offset, pipeline);
            }
            // Every sender dropped → capture stopped: end the stream cleanly.
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Periodic report so a long-running live session surfaces latency continuously.
        if last_report.elapsed() >= report_each {
            on_report(pipeline.report());
            last_report = Instant::now();
        }
    }

    // Final drain: fuse any phone Stats that arrived after the last frame was sent (the phone's
    // report for the tail frames lags the host's send by the network + decode + present latency).
    drain_stats(stats_rx, offset, pipeline);

    summary.bytes_sent = counting.total;
    Ok(summary)
}

/// Non-blockingly drain forwarded phone `Frame::Stats` and fuse them into `pipeline` (only when
/// clock-sync produced an `offset`). Other frame kinds and a closed/empty channel are ignored.
fn drain_stats(
    stats_rx: &std::sync::mpsc::Receiver<Frame>,
    offset: Option<ClockOffset>,
    pipeline: &mut PipelineLatency,
) {
    while let Ok(frame) = stats_rx.try_recv() {
        if let Frame::Stats {
            pts_us,
            arrive_us,
            decode_us,
            present_us,
        } = frame
        {
            if let Some(off) = offset {
                pipeline.record_stats(pts_us, arrive_us, decode_us, present_us, off);
            }
        }
    }
}

/// Send one encoded access unit on the wire, applying the shared keyframe/config contract:
/// cache the codec config from the first keyframe seen, emit `Frame::VideoConfig` before
/// every keyframe, then the `Frame::Video`, then flush (C-01). Shared by [`run_send_session`]
/// and [`run_stream_session`] so the wire contract lives in exactly one place.
fn send_encoded_frame<W: Write>(
    encoded: EncodedFrame,
    out: &mut CountingWrite<'_, W>,
    codec_config: &mut Option<CodecConfig>,
    codec: VideoCodec,
) -> Result<(), SessionError> {
    if codec_config.is_none() && encoded.keyframe {
        *codec_config = nal::extract_codec_config(&encoded.annex_b);
    }
    if encoded.keyframe {
        if let Some(cfg) = codec_config.as_ref() {
            let sps_pps = codec_config_to_annex_b(cfg);
            Frame::VideoConfig { codec, sps_pps }.write_to(out)?;
        }
    }
    Frame::Video {
        pts_us: encoded.pts_us,
        keyframe: encoded.keyframe,
        nal: encoded.annex_b,
    }
    .write_to(out)?;
    // Flush each frame so it reaches the peer instead of stalling in nusb's userspace write
    // buffer (C-01). No-op for TcpStream/loopback; essential for the AOA bulk path.
    out.flush()?;
    Ok(())
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
                capture_us: 0,
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
    // Disconnect-after-handshake transport — models a peer that dies once the
    // session is live. Reads serve the scripted handshake reply; the first write
    // attempted AFTER that reply has been consumed fails, as a real AOA/TCP write
    // would once the cable is pulled. Used to prove the idle heartbeat surfaces a
    // disconnect instead of blocking forever.
    // -----------------------------------------------------------------------

    struct DisconnectAfterHandshake {
        read_src: Cursor<Vec<u8>>,
        handshake_read: bool,
    }

    impl DisconnectAfterHandshake {
        fn new(peer_reply: Vec<u8>) -> Self {
            Self {
                read_src: Cursor::new(peer_reply),
                handshake_read: false,
            }
        }
    }

    impl Read for DisconnectAfterHandshake {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.read_src.read(buf)?;
            // Once the client reply has been fully consumed, the handshake is done and
            // any later write is a post-handshake (e.g. heartbeat) write.
            if self.read_src.position() as usize >= self.read_src.get_ref().len() {
                self.handshake_read = true;
            }
            Ok(n)
        }
    }

    impl Write for DisconnectAfterHandshake {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.handshake_read {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "peer disconnected",
                ));
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.handshake_read {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "peer disconnected",
                ));
            }
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

    /// The AgreedConfig the default handshake negotiates — threaded directly into
    /// `run_stream_session_instrumented` (the caller does the handshake before the split).
    fn default_agreed() -> AgreedConfig {
        AgreedConfig {
            width: 2400,
            height: 1080,
            refresh_hz: 60,
            codec: VideoCodec::H264,
        }
    }

    fn default_client_reply() -> Vec<u8> {
        make_client_reply(protocol_version(), 2400, 1080, 60, vec![VideoCodec::H264])
    }

    // -----------------------------------------------------------------------
    // run_stream_session — push→pull bridge for the live SCK+VT pipeline.
    // The live bin's SCStreamOutput delegate is the producer (push); this
    // consumes pre-encoded frames from an mpsc channel and drives the same
    // wire contract as run_send_session. Cable-free, host-tested.
    // -----------------------------------------------------------------------

    fn keyframe_encoded(pts_us: u64) -> EncodedFrame {
        let mut annex_b = Vec::new();
        annex_b.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x1F]); // SPS
        annex_b.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE]); // PPS
        annex_b.extend_from_slice(&[0, 0, 0, 1, 0x65, 0xAA]); // IDR
        EncodedFrame {
            pts_us,
            capture_us: 0,
            keyframe: true,
            encode_micros: 7,
            annex_b,
        }
    }

    fn delta_encoded(pts_us: u64) -> EncodedFrame {
        EncodedFrame {
            pts_us,
            capture_us: 0,
            keyframe: false,
            encode_micros: 3,
            annex_b: vec![0, 0, 0, 1, 0x61, 0xBB],
        }
    }

    // --- drop-to-keyframe coalescing (item 6 step 2) -----------------------

    fn ptss(frames: &[EncodedFrame]) -> Vec<u64> {
        frames.iter().map(|f| f.pts_us).collect()
    }

    #[test]
    fn coalesce_empty_backlog_is_empty() {
        assert!(coalesce_to_latest_keyframe(Vec::new()).is_empty());
    }

    #[test]
    fn coalesce_without_keyframe_keeps_all_in_order() {
        // No IDR in the backlog: every P-frame must be kept (dropping one corrupts the
        // stream until the next keyframe — there is nothing to resync to).
        let backlog = vec![delta_encoded(1), delta_encoded(2), delta_encoded(3)];
        let kept = coalesce_to_latest_keyframe(backlog);
        assert_eq!(ptss(&kept), vec![1, 2, 3]);
    }

    #[test]
    fn coalesce_drops_everything_before_the_last_keyframe() {
        // Backlog built up during a slow USB write: stale P-frames precede a fresh IDR.
        // Drop them and resume at the keyframe — the freshest decodable frame wins, and
        // host-side queue residence can never exceed one keyframe interval.
        let backlog = vec![
            delta_encoded(1),
            delta_encoded(2),
            keyframe_encoded(3),
            delta_encoded(4),
        ];
        let kept = coalesce_to_latest_keyframe(backlog);
        assert_eq!(
            ptss(&kept),
            vec![3, 4],
            "drop pre-keyframe staleness, keep IDR→now"
        );
        assert!(kept[0].keyframe, "the survivor head is the keyframe");
    }

    #[test]
    fn coalesce_picks_the_latest_keyframe_when_several_present() {
        // Two IDRs in one backlog (deep backlog spanning >1 keyframe interval): jump to the
        // LAST one so we shed the most staleness.
        let backlog = vec![
            keyframe_encoded(1),
            delta_encoded(2),
            keyframe_encoded(3),
            delta_encoded(4),
        ];
        let kept = coalesce_to_latest_keyframe(backlog);
        assert_eq!(ptss(&kept), vec![3, 4]);
    }

    #[test]
    fn coalesce_single_frame_is_untouched() {
        assert_eq!(
            ptss(&coalesce_to_latest_keyframe(vec![delta_encoded(7)])),
            vec![7]
        );
        assert_eq!(
            ptss(&coalesce_to_latest_keyframe(vec![keyframe_encoded(7)])),
            vec![7]
        );
    }

    #[test]
    fn instrumented_loop_drops_to_latest_keyframe_under_backlog() {
        // Pre-load a deep backlog spanning two keyframes — the channel state the loop would see
        // after a slow USB write let five frames pile up. The drain+coalesce step must send only
        // [last keyframe .. now] = 2 frames, drop the 3 stale leading frames, and count them.
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(keyframe_encoded(0)).unwrap();
        tx.send(delta_encoded(16_666)).unwrap();
        tx.send(delta_encoded(33_332)).unwrap();
        tx.send(keyframe_encoded(49_998)).unwrap();
        tx.send(delta_encoded(66_664)).unwrap();
        drop(tx); // all frames already queued → first recv + try_recv drain sees the whole backlog

        let (_stats_tx, stats_rx) = std::sync::mpsc::channel();
        let mut t = 0u64;
        let now = move || {
            t += 10;
            t
        };
        let mut pipeline = PipelineLatency::new(16);
        let mut sink: Vec<u8> = Vec::new();

        let summary = run_stream_session_instrumented(
            &rx,
            &mut sink,
            default_agreed(),
            None, // no clock offset needed: this test asserts shed-load, not fusion
            &stats_rx,
            &mut pipeline,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(3600), // no periodic report during the test
            now,
            |_r| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();

        assert_eq!(summary.frames, 2, "only [last keyframe .. now] is sent");
        assert_eq!(
            pipeline.report().dropped_frames,
            3,
            "the 3 stale leading frames are dropped and counted"
        );

        // Wire contract intact on the survivors: VideoConfig precedes the surviving keyframe and
        // exactly two Video frames go out.
        let mut cur = Cursor::new(sink);
        let mut sent = Vec::new();
        while let Ok(f) = Frame::read_from(&mut cur) {
            sent.push(f);
        }
        let videos = sent
            .iter()
            .filter(|f| matches!(f, Frame::Video { .. }))
            .count();
        assert_eq!(videos, 2, "two Video frames survive the coalesce");
        assert!(
            sent.iter().any(|f| matches!(f, Frame::VideoConfig { .. })),
            "VideoConfig still precedes the surviving keyframe"
        );
    }

    #[test]
    fn instrumented_session_returns_when_stop_flag_set() {
        use std::sync::atomic::AtomicBool;
        // A deep backlog is queued, but the stop flag is already set: the loop must break on
        // its first iteration (before draining any frame) and return cleanly — proving an
        // external `rustscreen stop` (SIGTERM) tears down an active stream promptly.
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(keyframe_encoded(0)).unwrap();
        tx.send(delta_encoded(16_666)).unwrap();
        tx.send(keyframe_encoded(33_332)).unwrap();
        // Keep the sender alive: a closed channel would also end the loop, masking the stop path.
        let _keep = tx;

        let (_stats_tx, stats_rx) = std::sync::mpsc::channel();
        let mut t = 0u64;
        let now = move || {
            t += 10;
            t
        };
        let mut pipeline = PipelineLatency::new(16);
        let mut sink: Vec<u8> = Vec::new();
        let stop = AtomicBool::new(true); // stop already requested

        let summary = run_stream_session_instrumented(
            &rx,
            &mut sink,
            default_agreed(),
            None,
            &stats_rx,
            &mut pipeline,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(3600),
            now,
            |_r| {},
            &stop,
        )
        .unwrap();

        assert_eq!(summary.frames, 0, "stop breaks before any frame is sent");
        assert!(sink.is_empty(), "no bytes written once stop is observed");
    }

    #[test]
    fn stream_session_sends_videoconfig_before_each_keyframe() {
        // The live pipeline pushes pre-encoded frames into a channel; run_stream_session
        // handshakes, then sends VideoConfig before every keyframe and a Video frame for
        // every access unit — the same wire contract as run_send_session, but driven by a
        // channel rather than a pull-based Capturer (the SCK+VT pipeline is push-based).
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(keyframe_encoded(0)).unwrap();
        tx.send(delta_encoded(16_666)).unwrap();
        tx.send(keyframe_encoded(33_332)).unwrap();
        drop(tx); // close the channel → stream loop ends cleanly (capture stopped)

        let mut t = SplitTransport::new(default_client_reply());
        let summary = run_stream_session(rx, &mut t, default_offer()).unwrap();

        assert_eq!(summary.frames, 3);
        assert_eq!(summary.agreed_config.codec, VideoCodec::H264);
        assert!(summary.codec_config.is_some());

        let sent = t.decode_sent_frames();
        // Handshake offer, then: VideoConfig+Video (kf), Video (delta), VideoConfig+Video (kf).
        assert_eq!(sent.len(), 6, "got: {sent:?}");
        assert!(matches!(sent[0], Frame::Handshake(_)));
        assert!(matches!(sent[1], Frame::VideoConfig { .. }));
        assert!(matches!(sent[2], Frame::Video { keyframe: true, .. }));
        assert!(matches!(
            sent[3],
            Frame::Video {
                keyframe: false,
                ..
            }
        ));
        assert!(matches!(sent[4], Frame::VideoConfig { .. }));
        assert!(matches!(sent[5], Frame::Video { keyframe: true, .. }));
    }

    /// C-01 on the live path: `run_stream_session` must flush every write before it blocks
    /// on a read and before it returns, exactly like `run_send_session`. Pins the invariant
    /// directly to the streaming function so a future divergence from the shared helper is
    /// caught here, not only via `run_send_session`.
    #[test]
    fn stream_session_flushes_writes_before_reads_and_at_end() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(keyframe_encoded(0)).unwrap();
        tx.send(delta_encoded(16_666)).unwrap();
        drop(tx); // close channel → loop ends cleanly without any heartbeat write

        let mut t = FlushAuditTransport::new(default_client_reply());
        let summary = run_stream_session(rx, &mut t, default_offer()).unwrap();

        assert_eq!(summary.frames, 2);
        assert_eq!(
            t.unflushed, 0,
            "stream session returned with unflushed bytes — frames would stall in the write buffer"
        );
    }

    /// Liveness: when no frames arrive (static screen) the session must not block forever.
    /// The heartbeat probe writes to the transport, which surfaces a peer disconnect as an
    /// error and ends the session instead of hanging.
    #[test]
    fn stream_session_heartbeat_detects_idle_disconnect() {
        // Keep the sender alive so the channel stays open (Timeout, not Disconnected) but
        // never send a frame — the only writes are heartbeats.
        let (_tx, rx) = std::sync::mpsc::channel::<EncodedFrame>();
        let mut t = DisconnectAfterHandshake::new(default_client_reply());

        let result = run_stream_session_inner(
            rx,
            &mut t,
            default_offer(),
            std::time::Duration::from_millis(5),
        );

        assert!(
            result.is_err(),
            "an idle session over a dead transport must detect the disconnect via heartbeat, not hang"
        );
    }

    // -----------------------------------------------------------------------
    // run_stream_session_instrumented — post-handshake loop with live stats fusion.
    // The caller does the handshake + clock-sync + split first, then a reader thread
    // forwards phone Frame::Stats into stats_rx while this loop streams on the write
    // half and records host stage timestamps. Cable-free, host-tested.
    // -----------------------------------------------------------------------

    #[test]
    fn instrumented_stream_fuses_host_and_phone_stats() {
        use crate::latency::PipelineLatency;
        use protocol::clock::ClockOffset;

        // Inject one keyframe and a delta. The phone's report for a given pts only arrives after
        // the host has sent (and recorded) that frame, so we model that lag with a background
        // thread that forwards each Stats frame for a pts *after* a host record for it can exist.
        // To keep the test deterministic, we gate the loop on the frame timing: each frame is sent
        // with a small delay, and the stats thread forwards each report keyed to a frame that the
        // loop will already have recorded by the time the per-frame/heartbeat drain runs.
        let (tx, rx) = std::sync::mpsc::channel();
        let (stats_tx, stats_rx) = std::sync::mpsc::channel::<Frame>();

        // Frame producer: send kf(1000), then after the stat for 1000 is delivered, send
        // delta(2000), then the stat for 2000, then close. The ordering guarantees each Stats is
        // drained only once its host record exists, so both fuse.
        let producer = std::thread::spawn(move || {
            tx.send(keyframe_encoded(1000)).unwrap();
            // Give the loop time to record host stamps for pts 1000.
            std::thread::sleep(std::time::Duration::from_millis(15));
            stats_tx
                .send(Frame::Stats {
                    pts_us: 1000,
                    arrive_us: 100,
                    decode_us: 110,
                    present_us: 120,
                })
                .unwrap();
            tx.send(delta_encoded(2000)).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(15));
            stats_tx
                .send(Frame::Stats {
                    pts_us: 2000,
                    arrive_us: 200,
                    decode_us: 210,
                    present_us: 220,
                })
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(15));
            // dropping tx + stats_tx closes both channels → loop ends, final drain fuses the tail.
        });

        // Monotonic host clock: deterministic increasing stamps (10 µs apart).
        let mut t = 0u64;
        let now = move || {
            t += 10;
            t
        };

        let offset = Some(ClockOffset {
            offset_us: 0,
            rtt_us: 4,
        });
        let mut pipeline = PipelineLatency::new(16);
        let mut sink: Vec<u8> = Vec::new();

        let summary = run_stream_session_instrumented(
            &rx,
            &mut sink,
            default_agreed(),
            offset,
            &stats_rx,
            &mut pipeline,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(3600), // no periodic report during the test
            now,
            |_r| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        producer.join().unwrap();

        assert_eq!(summary.frames, 2);
        assert_eq!(summary.agreed_config.codec, VideoCodec::H264);

        // Both phone Stats matched an in-flight host record → two fused glass-to-glass samples.
        let report = pipeline.report();
        assert_eq!(
            report.glass_to_glass.count(),
            2,
            "both frames' phone stats should fuse with their host records"
        );
        // Host stages were recorded for both frames too.
        assert_eq!(report.capture_to_encode.count(), 2);
        assert_eq!(report.encode_to_send.count(), 2);

        // The write half carries the same wire contract: VideoConfig+Video(kf), Video(delta).
        // (Plus any idle Heartbeats interleaved while waiting on the producer's delays.)
        let mut cur = Cursor::new(sink);
        let mut sent = Vec::new();
        while let Ok(f) = Frame::read_from(&mut cur) {
            sent.push(f);
        }
        let videos: Vec<_> = sent
            .iter()
            .filter(|f| matches!(f, Frame::Video { .. }))
            .collect();
        assert_eq!(
            videos.len(),
            2,
            "two Video frames, regardless of heartbeats"
        );
        assert!(
            sent.iter().any(|f| matches!(f, Frame::VideoConfig { .. })),
            "VideoConfig precedes the keyframe"
        );
        // No Handshake frame here (handshake happened on the duplex transport before the split).
        assert!(!sent.iter().any(|f| matches!(f, Frame::Handshake(_))));
    }

    #[test]
    fn instrumented_stream_without_offset_records_host_only() {
        use crate::latency::PipelineLatency;

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(keyframe_encoded(1000)).unwrap();
        drop(tx);

        // A phone Stats arrives, but with no clock offset it must NOT be fused (no glass-to-glass).
        let (stats_tx, stats_rx) = std::sync::mpsc::channel::<Frame>();
        stats_tx
            .send(Frame::Stats {
                pts_us: 1000,
                arrive_us: 100,
                decode_us: 110,
                present_us: 120,
            })
            .unwrap();
        drop(stats_tx);

        let mut t = 0u64;
        let now = move || {
            t += 10;
            t
        };
        let mut pipeline = PipelineLatency::new(16);
        let mut sink: Vec<u8> = Vec::new();

        let summary = run_stream_session_instrumented(
            &rx,
            &mut sink,
            default_agreed(),
            None, // clock-sync failed → degrade to host-only stages
            &stats_rx,
            &mut pipeline,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(3600),
            now,
            |_r| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();

        assert_eq!(summary.frames, 1);
        let report = pipeline.report();
        // Without a clock offset the host never fuses phone Stats, so no stage in the report has
        // any samples — glass-to-glass (and every cross-clock stage) is unavailable. The host
        // stamps are still captured into the in-flight FIFO; they simply never get fused. The
        // summary's encode-latency stat is the host-only signal that remains usable.
        assert_eq!(
            report.glass_to_glass.count(),
            0,
            "no offset → phone stats are not fused; glass-to-glass unavailable"
        );
        assert_eq!(report.capture_to_encode.count(), 0);
        assert_eq!(
            summary.latency.count(),
            1,
            "host-only encode latency is still recorded"
        );
    }

    /// Fix #5: the negotiated AgreedConfig is threaded in directly (not re-derived from the
    /// offer), so the codec the loop writes is exactly what was passed — even when it differs
    /// from the host's own H.264 default.
    #[test]
    fn instrumented_stream_uses_threaded_agreed_config() {
        use crate::latency::PipelineLatency;

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(keyframe_encoded(1000)).unwrap();
        drop(tx);
        let (_stats_tx, stats_rx) = std::sync::mpsc::channel::<Frame>();

        // A non-default agreed config (HEVC) proves the loop honours the threaded value rather
        // than re-deriving H.264 from the offer's codec list.
        let agreed = AgreedConfig {
            width: 1920,
            height: 1080,
            refresh_hz: 30,
            codec: VideoCodec::Hevc,
        };

        let mut t = 0u64;
        let now = move || {
            t += 10;
            t
        };
        let mut pipeline = PipelineLatency::new(16);
        let mut sink: Vec<u8> = Vec::new();

        let summary = run_stream_session_instrumented(
            &rx,
            &mut sink,
            agreed.clone(),
            None,
            &stats_rx,
            &mut pipeline,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(3600),
            now,
            |_r| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();

        assert_eq!(summary.agreed_config, agreed);
        // The VideoConfig frame on the wire carries the threaded codec.
        let mut cur = Cursor::new(sink);
        let mut codec_seen = None;
        while let Ok(f) = Frame::read_from(&mut cur) {
            if let Frame::VideoConfig { codec, .. } = f {
                codec_seen = Some(codec);
            }
        }
        assert_eq!(
            codec_seen,
            Some(VideoCodec::Hevc),
            "the wire VideoConfig uses the threaded agreed codec, not a re-derived default"
        );
    }

    /// Fix #2: capture→encode is measured from the frame's real `capture_us` (stamped by the
    /// delegate), NOT derived from `encode_micros`. A frame whose capture_us is well before the
    /// loop's encode_done stamp must produce a capture→encode interval reflecting that real gap.
    #[test]
    fn instrumented_stream_uses_real_capture_timestamp() {
        use crate::latency::PipelineLatency;
        use protocol::clock::ClockOffset;

        // Build a frame whose capture_us = 5 (stamped "early"), so when the loop stamps
        // encode_done from the injected clock the capture→encode gap is the real difference,
        // independent of encode_micros.
        let mut kf = keyframe_encoded(1000);
        kf.capture_us = 5;
        kf.encode_micros = 999; // deliberately unrelated to the capture→encode gap

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(kf).unwrap();
        drop(tx);

        // Phone stat for pts 1000 so the host record fuses and the capture→encode stage records.
        let (stats_tx, stats_rx) = std::sync::mpsc::channel::<Frame>();
        stats_tx
            .send(Frame::Stats {
                pts_us: 1000,
                arrive_us: 1000,
                decode_us: 1000,
                present_us: 1000,
            })
            .unwrap();
        drop(stats_tx);

        // Injected clock: first call (encode_done) returns 30, next (send_done) 40, etc.
        let mut t = 20u64;
        let now = move || {
            t += 10;
            t
        };
        let offset = Some(ClockOffset {
            offset_us: 0,
            rtt_us: 0,
        });
        let mut pipeline = PipelineLatency::new(16);
        let mut sink: Vec<u8> = Vec::new();

        run_stream_session_instrumented(
            &rx,
            &mut sink,
            default_agreed(),
            offset,
            &stats_rx,
            &mut pipeline,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(3600),
            now,
            |_r| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();

        let report = pipeline.report();
        assert_eq!(report.capture_to_encode.count(), 1);
        // encode_done_us = 30 (first now() call), capture_us = 5 → gap = 25, NOT encode_micros(999)
        // and NOT 0 (which a `encode_done - encode_micros` derivation would have produced).
        assert_eq!(
            report.capture_to_encode.max(),
            Some(25),
            "capture→encode uses the real capture_us, not encode_micros"
        );
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
    // Clock sync tests
    // -----------------------------------------------------------------------

    use protocol::clock::ClockOffset;

    #[test]
    fn clock_sync_round_trips_and_estimates_offset() {
        // The "phone" pre-writes the ClockPong it will reply with; the host reads it.
        let mut pong = Vec::new();
        Frame::ClockPong {
            t0_us: 0,
            t1_us: 150,
            t2_us: 150,
        }
        .write_to(&mut pong)
        .unwrap();

        let mut transport = DuplexFake::new(pong);

        // Host clock returns t0=0 on the ping, t3=200 on receipt.
        let mut times = [0u64, 200].into_iter();
        let offset = perform_clock_sync(&mut transport, || times.next().unwrap()).unwrap();

        assert_eq!(
            offset,
            ClockOffset {
                offset_us: 50,
                rtt_us: 200
            }
        );
        // The host must have written exactly one ClockPing carrying t0=0.
        let mut cur = std::io::Cursor::new(transport.written());
        assert_eq!(
            Frame::read_from(&mut cur).unwrap(),
            Frame::ClockPing { t0_us: 0 }
        );
    }

    #[test]
    fn clock_sync_rejects_unexpected_reply() {
        let mut not_a_pong = Vec::new();
        Frame::Control(Control::Heartbeat)
            .write_to(&mut not_a_pong)
            .unwrap();
        let mut transport = DuplexFake::new(not_a_pong);
        let mut times = [0u64, 1].into_iter();
        let err = perform_clock_sync(&mut transport, || times.next().unwrap());
        assert!(matches!(err, Err(ClockSyncError::UnexpectedReply)));
    }

    /// A `Read + Write` that serves canned bytes on read and captures writes.
    struct DuplexFake {
        to_read: std::io::Cursor<Vec<u8>>,
        written: Vec<u8>,
    }
    impl DuplexFake {
        fn new(to_read: Vec<u8>) -> Self {
            Self {
                to_read: std::io::Cursor::new(to_read),
                written: Vec::new(),
            }
        }
        fn written(&self) -> &[u8] {
            &self.written
        }
    }
    impl std::io::Read for DuplexFake {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.to_read.read(buf)
        }
    }
    impl std::io::Write for DuplexFake {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
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

    // -----------------------------------------------------------------------
    // CountingWrite byte-counting guard (protects the generic refactor)
    // -----------------------------------------------------------------------

    #[test]
    fn counting_write_increments_total_by_bytes_written() {
        let mut sink: Vec<u8> = Vec::new();
        let mut counting = CountingWrite::new(&mut sink);

        // Each write of N bytes must bump `total` by exactly N.
        let n1 = counting.write(b"hello").unwrap();
        assert_eq!(n1, 5);
        assert_eq!(counting.total, 5);

        let n2 = counting.write(b", world").unwrap();
        assert_eq!(n2, 7);
        assert_eq!(counting.total, 12);

        // flush must not change the counter and must reach the inner writer.
        counting.flush().unwrap();
        assert_eq!(counting.total, 12);

        assert_eq!(sink, b"hello, world");
    }

    #[test]
    fn instrumented_borrows_receiver_so_it_is_reusable_across_connections() {
        // Drop the sender up front: with no live sender, each call sees `Disconnected`
        // immediately and returns a zero-frame summary. The point of the test is that
        // `rx` is still usable for the SECOND call — i.e. the param is borrowed, not moved.
        let (tx, rx) = std::sync::mpsc::sync_channel::<EncodedFrame>(2);
        drop(tx);
        let (_stats_tx, stats_rx) = std::sync::mpsc::channel();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let now = || 0u64;

        let mut pipeline = PipelineLatency::new(16);
        let mut sink: Vec<u8> = Vec::new();
        let s1 = run_stream_session_instrumented(
            &rx,
            &mut sink,
            default_agreed(),
            None,
            &stats_rx,
            &mut pipeline,
            std::time::Duration::from_millis(1),
            std::time::Duration::from_secs(3600),
            now,
            |_r| {},
            &stop,
        )
        .unwrap();

        let mut pipeline2 = PipelineLatency::new(16);
        let mut sink2: Vec<u8> = Vec::new();
        let s2 = run_stream_session_instrumented(
            &rx,
            &mut sink2,
            default_agreed(),
            None,
            &stats_rx,
            &mut pipeline2,
            std::time::Duration::from_millis(1),
            std::time::Duration::from_secs(3600),
            now,
            |_r| {},
            &stop,
        )
        .unwrap();

        assert_eq!(s1.frames, 0, "no sender → zero frames");
        assert_eq!(s2.frames, 0, "receiver reusable for a second connection");
    }

    #[test]
    fn a_pframe_before_the_first_keyframe_is_not_sent() {
        // A freshly (re)connected decoder must receive a keyframe first. A lone P-frame that
        // arrives before any keyframe (the post-reconnect producer race) must be DROPPED, not sent.
        let (tx, rx) = std::sync::mpsc::sync_channel::<EncodedFrame>(4);
        tx.send(delta_encoded(1000)).unwrap();
        drop(tx); // capture then ends → loop sees Disconnected and exits
        let (_stats_tx, stats_rx) = std::sync::mpsc::channel();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let now = || 0u64;
        let mut pipeline = PipelineLatency::new(16);
        let mut sink: Vec<u8> = Vec::new();
        let summary = run_stream_session_instrumented(
            &rx,
            &mut sink,
            default_agreed(),
            None,
            &stats_rx,
            &mut pipeline,
            std::time::Duration::from_millis(1),
            std::time::Duration::from_secs(3600),
            now,
            |_r| {},
            &stop,
        )
        .unwrap();
        assert_eq!(
            summary.frames, 0,
            "a P-frame before the first keyframe must not be sent"
        );
    }
}
