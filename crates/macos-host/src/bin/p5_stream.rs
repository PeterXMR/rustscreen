//! P5 live-streaming host (item 1 / PR #21, hands-on-Mac+phone). Compiled ONLY under
//! `--features live-capture,live-usb`.
//!
//! Wires the proven spikes into the live pipeline: create the virtual display (P2) →
//! ScreenCaptureKit capture (P3) → VideoToolbox H.264 encode (P3) → **push** each encoded
//! access unit into a channel → [`run_stream_session`] drains it and streams `VideoConfig`
//! (before every keyframe) + `Video` frames over the live AOA transport (P1) to the phone,
//! which decodes-to-surface (P4).
//!
//! Push→pull bridge: SCK+VideoToolbox deliver frames asynchronously via a delegate (push),
//! so the delegate's VideoToolbox output handler pushes an [`EncodedFrame`] into the channel;
//! `run_stream_session` (cable-free, TDD'd) pulls and applies the wire contract. The loop runs
//! until the phone disconnects (transport error) — the live counterpart of P3's fixed 3 s clip.
//!
//! Build: `cargo build -p macos-host --features live-capture,live-usb` → `target/debug/p5_stream`.
//! Run from a terminal granted **Screen & System Audio Recording**, with the Pixel 6a plugged in
//! and the RustScreen app open. The Mac's extended desktop should appear live on the phone.

#[cfg(not(all(feature = "live-capture", feature = "live-usb")))]
fn main() {
    eprintln!(
        "p5_stream requires --features live-capture,live-usb (macOS, hands-on + phone). \
         Rebuild with both."
    );
}

#[cfg(all(feature = "live-capture", feature = "live-usb"))]
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
use std::sync::mpsc;
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
use std::sync::{Arc, Mutex};

#[cfg(all(feature = "live-capture", feature = "live-usb"))]
use macos_host::encode::EncodedFrame;
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
use objc2::runtime::NSObjectProtocol;
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
use objc2::DefinedClass;
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
use objc2_core_media::CMSampleBuffer;
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
use objc2_screen_capture_kit::{SCStream, SCStreamOutput, SCStreamOutputType};

/// Process-global monotonic origin. Initialised on first use so the capture delegate, the
/// stream loop's host-stage stamps, and clock-sync all share ONE clock origin — otherwise a
/// `capture_us` stamped against the delegate's clock could not be compared with an
/// `encode_done_us` stamped against a different `Instant`.
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Process-monotonic microseconds since [`START`] (initialised on first call).
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
fn now_us() -> u64 {
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros() as u64
}

/// Cached H.264 parameter sets (SPS, PPS) + AVCC NAL-length size, read once from the first
/// sample's format description and reused to inject params in-band ahead of every keyframe.
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
type ParamCache = Arc<Mutex<Option<(Vec<u8>, Vec<u8>, usize)>>>;

#[cfg(all(feature = "live-capture", feature = "live-usb"))]
struct FrameSinkIvars {
    session: objc2::rc::Retained<objc2_video_toolbox::VTCompressionSession>,
    /// Producer half of the push→pull bridge. Each encoded access unit is sent here; the main
    /// thread's `run_stream_session` is the consumer.
    tx: mpsc::SyncSender<EncodedFrame>,
    params: ParamCache,
    pts_idx: AtomicI64,
    /// VideoToolbox in-flight encode depth = frames submitted minus frames emitted. The capture
    /// delegate bounds this (latency TODO #2) so `capture→encode` cannot bufferbloat.
    in_flight: Arc<AtomicUsize>,
    /// Count of capture frames shed pre-encode because `in_flight` was at the cap (diagnostic).
    dropped: Arc<AtomicUsize>,
    /// Set by the encode handler when a fully-ENCODED frame is dropped post-encode (channel
    /// `Full`). The capture delegate consumes it to force the NEXT submitted frame to an IDR:
    /// a dropped P-frame leaves VideoToolbox's reference state pointing at an access unit the
    /// phone never received, so without a resync every later P-frame references the gap and the
    /// decoder shows artifacts until the next periodic keyframe (up to ~1 s at MaxKeyFrameInterval
    /// = 60). Unlike the pre-encode in-flight drop above, which keeps the stream valid with no
    /// resync because VideoToolbox never sees the shed frame.
    needs_keyframe: Arc<AtomicBool>,
}

#[cfg(all(feature = "live-capture", feature = "live-usb"))]
objc2::define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    #[name = "RSStreamSink"]
    #[ivars = FrameSinkIvars]
    struct FrameSink;

    unsafe impl NSObjectProtocol for FrameSink {}

    unsafe impl SCStreamOutput for FrameSink {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            output_type: SCStreamOutputType,
        ) {
            if output_type != SCStreamOutputType::Screen {
                return;
            }
            let Some(image) = (unsafe { sample_buffer.image_buffer() }) else {
                return;
            };
            // --- in-flight encode pacing (latency TODO #2) --------------------------------
            // Bound VideoToolbox's submitted-but-not-yet-emitted depth so `capture→encode`
            // cannot grow unbounded (the measured 55→116 ms p50 / 945 ms max bufferbloat).
            // When the encoder is already saturated, shed the freshest CAPTURE frame BEFORE
            // submitting it: the next frame we do submit is encoded as an ordinary P-frame
            // against the last ENCODED frame, so the H.264 stream stays valid with NO keyframe
            // resync (unlike the phone's post-encode InputPacer, which must drop-to-keyframe).
            const MAX_IN_FLIGHT_ENCODES: usize = 2;
            let in_flight = Arc::clone(&self.ivars().in_flight);
            if in_flight.load(Ordering::Relaxed) >= MAX_IN_FLIGHT_ENCODES {
                self.ivars().dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            in_flight.fetch_add(1, Ordering::Relaxed);
            let idx = self.ivars().pts_idx.fetch_add(1, Ordering::Relaxed);
            // Live presentation timestamps: a monotonic 60 Hz frame counter expressed in
            // microseconds. The decoder uses pts only for ordering; a 1-vsync grid is fine.
            let pts_us = (idx as u64).saturating_mul(16_666);
            let pts = objc2_core_media::CMTime::new(idx, 60);
            let dur = objc2_core_media::CMTime::new(1, 60);

            let tx = self.ivars().tx.clone();
            let params = Arc::clone(&self.ivars().params);
            // Stamp the REAL capture time (process-monotonic µs) the instant ScreenCaptureKit
            // delivered this frame, BEFORE it is submitted to VideoToolbox. Carrying this into
            // the EncodedFrame lets the stream loop measure capture→encode (and glass-to-glass)
            // against actual wall time — including mpsc-channel queueing — instead of deriving
            // it tautologically from encode_micros.
            let capture_us = now_us();
            // Stamp submit time into the handler; when VideoToolbox fires it, the elapsed time
            // is this one frame's encode latency.
            let submit_t = std::time::Instant::now();
            let in_flight_h = Arc::clone(&in_flight);
            let dropped_h = Arc::clone(&self.ivars().dropped);
            let needs_keyframe_h = Arc::clone(&self.ivars().needs_keyframe);
            let handler = block2::RcBlock::new(
                move |status: i32,
                      _flags: objc2_video_toolbox::VTEncodeInfoFlags,
                      sbuf: *mut CMSampleBuffer| {
                    // VideoToolbox fires this exactly once per accepted frame. Decrement FIRST,
                    // on every path (including the early returns below), so the in-flight counter
                    // can never leak and wedge the pacer permanently shut.
                    in_flight_h.fetch_sub(1, Ordering::Relaxed);
                    if status != 0 {
                        return;
                    }
                    let Some(sbuf) = (unsafe { sbuf.as_ref() }) else {
                        return;
                    };
                    // Cache SPS/PPS from the format description the first time we see one.
                    let mut guard = params.lock().unwrap();
                    if guard.is_none() {
                        if let Some(fmt) = unsafe { sbuf.format_description() } {
                            if let Some(p) = unsafe { h264_params(&fmt) } {
                                *guard = Some(p);
                            }
                        }
                    }
                    let Some((sps, pps, nal_len)) = guard.clone() else {
                        return; // no params yet → cannot build a self-describing AU
                    };
                    drop(guard);

                    let Some(avcc) = (unsafe { block_buffer_bytes(sbuf) }) else {
                        return;
                    };
                    // Keyframe detection from the encoded bytes (reuses tested protocol::nal),
                    // avoiding the CMSampleAttachments FFI. On keyframes, inject SPS/PPS in-band
                    // so the stream self-describes AND run_stream_session can cache the config.
                    let picture = macos_host::encode_vt::avcc_to_annex_b(&avcc, nal_len);
                    let keyframe = protocol::nal::is_keyframe(&picture);
                    let annex_b = macos_host::encode_vt::to_annex_b_frame(
                        &avcc, nal_len, keyframe, &sps, &pps,
                    );
                    let frame = EncodedFrame {
                        pts_us,
                        capture_us,
                        keyframe,
                        encode_micros: submit_t.elapsed().as_micros() as u64,
                        annex_b,
                    };
                    // Bounded hand-off (latency TODO #5): the consumer does a ~22 ms blocking USB
                    // write per frame (≈45 fps), but capture runs at 60 fps. With an UNBOUNDED
                    // channel the 15 fps surplus piled up and `capture→encode` (measured to the
                    // consumer's dequeue) grew without bound. A bounded `sync_channel` + `try_send`
                    // drops the frame when the consumer is behind (`Full`) instead of queuing it —
                    // pacing production to the consumer and keeping latency flat. `Disconnected`
                    // means the consumer (phone) is gone; the main thread handles teardown.
                    if let Err(mpsc::TrySendError::Full(_)) = tx.try_send(frame) {
                        dropped_h.fetch_add(1, Ordering::Relaxed);
                        // This frame was already ENCODED, so VideoToolbox advanced its reference
                        // state past an access unit the phone never received. Force the next
                        // submitted frame to an IDR so the decoder resyncs across the gap instead
                        // of decoding P-frames against a missing reference. (Re-set if that forced
                        // frame is itself dropped, so recovery retries on the following frame.)
                        needs_keyframe_h.store(true, Ordering::Relaxed);
                    }
                },
            );
            let mut info = objc2_video_toolbox::VTEncodeInfoFlags::empty();
            // If a fully-encoded frame was dropped post-encode (channel `Full`), force this frame
            // to an IDR so the phone's decoder resyncs across the reference-chain gap. `swap`
            // clears the flag atomically; if this forced frame is later dropped too, the handler
            // re-sets it. Built only on the rare forced frame, never on the steady-state path.
            let frame_props = self
                .ivars()
                .needs_keyframe
                .swap(false, Ordering::Relaxed)
                .then(|| {
                    objc2_core_foundation::CFDictionary::from_slices(
                        &[objc2_video_toolbox::kVTEncodeFrameOptionKey_ForceKeyFrame],
                        &[objc2_core_foundation::CFBoolean::new(true)],
                    )
                });
            // SAFETY: VideoToolbox `Block_copy`s the handler during this call (see p3_encode);
            // the heap `RcBlock` stays alive until VT releases it after firing. `frame_props` (if
            // present) is read synchronously by VideoToolbox during this call and outlives it;
            // `cast_unchecked` only erases the dictionary's phantom key/value types (identical
            // layout), which VideoToolbox reads dynamically by CFString key.
            let status = unsafe {
                self.ivars().session.encode_frame_with_output_handler(
                    &image,
                    pts,
                    dur,
                    frame_props.as_deref().map(|d| d.cast_unchecked()),
                    &mut info,
                    &*handler as *const _ as *mut _,
                )
            };
            if status != 0 {
                // Sync submit failure → the output handler will NOT fire, so compensate the
                // pre-submit increment to keep the in-flight count accurate.
                in_flight.fetch_sub(1, Ordering::Relaxed);
                eprintln!("p5_stream: EncodeFrame returned status {status}");
            }
        }
    }
);

#[cfg(all(feature = "live-capture", feature = "live-usb"))]
impl FrameSink {
    fn new(
        session: objc2::rc::Retained<objc2_video_toolbox::VTCompressionSession>,
        tx: mpsc::SyncSender<EncodedFrame>,
        params: ParamCache,
    ) -> objc2::rc::Retained<Self> {
        use objc2::AllocAnyThread;
        let this = Self::alloc().set_ivars(FrameSinkIvars {
            session,
            tx,
            params,
            pts_idx: AtomicI64::new(0),
            in_flight: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicUsize::new(0)),
            needs_keyframe: Arc::new(AtomicBool::new(false)),
        });
        unsafe { objc2::msg_send![super(this), init] }
    }
}

/// Extract (SPS, PPS, nal_length_size) from an H.264 `CMFormatDescription`.
///
/// # Safety
/// `fmt` must be a valid H.264 video format description.
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
unsafe fn h264_params(
    fmt: &objc2_core_media::CMFormatDescription,
) -> Option<(Vec<u8>, Vec<u8>, usize)> {
    use std::os::raw::c_int;
    let mut nal_len: c_int = 0;
    let get = |index: usize, nal_len: &mut c_int| -> Option<Vec<u8>> {
        let mut ptr: *const u8 = std::ptr::null();
        let mut size: usize = 0;
        let mut count: usize = 0;
        let status = unsafe {
            objc2_core_media::CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                fmt, index, &mut ptr, &mut size, &mut count, nal_len,
            )
        };
        if status != 0 || ptr.is_null() || size == 0 {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec())
    };
    let sps = get(0, &mut nal_len)?;
    let pps = get(1, &mut nal_len)?;
    Some((sps, pps, nal_len.max(1) as usize))
}

/// Copy the AVCC bytes out of a compressed sample's `CMBlockBuffer`.
///
/// Uses `CMBlockBufferCopyDataBytes`, which assembles the bytes regardless of how the
/// buffer is segmented — VideoToolbox H.264 output is usually one contiguous block, but a
/// segmented buffer must not be dropped (it could be a keyframe, freezing the stream until
/// the next contiguous one) nor read past its first segment.
///
/// # Safety
/// `sbuf` must be a valid compressed `CMSampleBuffer`.
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
unsafe fn block_buffer_bytes(sbuf: &CMSampleBuffer) -> Option<Vec<u8>> {
    use std::ffi::c_void;
    let bb = unsafe { sbuf.data_buffer() }?;
    let total_len = unsafe { bb.data_length() };
    if total_len == 0 {
        return None;
    }
    let mut buf = vec![0u8; total_len];
    let dst = std::ptr::NonNull::new(buf.as_mut_ptr() as *mut c_void)?;
    let status = unsafe { bb.copy_data_bytes(0, total_len, dst) };
    if status != 0 {
        eprintln!(
            "p5_stream: warn — CMBlockBufferCopyDataBytes failed (status {status}); dropping frame"
        );
        return None;
    }
    Some(buf)
}

#[cfg(all(feature = "live-capture", feature = "live-usb"))]
fn main() {
    use cg_virtual_display::{DisplayConfig, Side, VirtualDisplay};
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::AnyThread;
    use objc2_core_foundation::CFType;
    use objc2_foundation::{NSArray, NSError};
    use objc2_screen_capture_kit::{
        SCContentFilter, SCDisplay, SCShareableContent, SCStreamConfiguration, SCWindow,
    };
    use objc2_video_toolbox::VTCompressionSession;
    use std::time::Duration;

    const W: usize = 2400;
    const H: usize = 1080;

    // --- 1. Virtual display (held alive for the whole session) ----------------
    // Item 2: present as a named, HiDPI external display placed to the right of the main
    // display. Stable identity (DisplayConfig defaults) lets macOS remember any manual
    // rearrange; dropping `vdisplay` on disconnect tears it down so the desktop reflows.
    println!("p5_stream: creating virtual display {W}×{H}@60 (HiDPI, right of main)…");
    let cfg = DisplayConfig::new(W as u32, H as u32, 60.0)
        .with_hidpi(true)
        .arranged(Side::Right);
    let vdisplay = match VirtualDisplay::with_config(&cfg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("p5_stream: failed to create virtual display: {e}");
            std::process::exit(1);
        }
    };
    let target_id = vdisplay.display_id();
    std::thread::sleep(Duration::from_millis(500));

    // --- 2. Locate the SCDisplay for our virtual display ----------------------
    let (tx_disp, rx_disp) = mpsc::channel::<usize>();
    let disp_handler =
        block2::RcBlock::new(move |content: *mut SCShareableContent, _e: *mut NSError| {
            let mut found = 0usize;
            if let Some(content) = unsafe { content.as_ref() } {
                for d in unsafe { content.displays() }.iter() {
                    if unsafe { d.displayID() } == target_id {
                        found = Retained::into_raw(d) as usize;
                        break;
                    }
                }
            }
            let _ = tx_disp.send(found);
        });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&disp_handler) };
    let display: Retained<SCDisplay> = match rx_disp.recv_timeout(Duration::from_secs(10)) {
        Ok(p) if p != 0 => unsafe { Retained::from_raw(p as *mut SCDisplay) }.unwrap(),
        _ => {
            eprintln!("p5_stream: could not find SCDisplay {target_id} (Screen Recording grant?)");
            std::process::exit(2);
        }
    };

    // --- 3. SCStream config: 420v at display size, cursor shown (ladder item 3) -
    let no_windows = NSArray::<SCWindow>::new();
    let filter = unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &no_windows,
        )
    };
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(W);
        config.setHeight(H);
        config.setShowsCursor(true); // the Mac cursor composites into the stream (item 3)
        config.setPixelFormat(u32::from_be_bytes(*b"420v"));
        // Cap delivery at 60 fps (item 6 step 2). Without a minimum frame interval, ScreenCaptureKit
        // can deliver change-driven bursts above 60 fps, oversupplying the phone's 60 Hz present
        // path so frames pile up in the decoder (the measured arrive→decode growth). One frame per
        // 1/60 s bounds the source rate to what the phone can present.
        config.setMinimumFrameInterval(objc2_core_media::CMTime::new(1, 60));
    }

    // --- 4. VideoToolbox H.264 session (RealTime low-latency, no B-frames) ------
    // Low-latency encoder spec (latency TODO #1): EnableLowLatencyRateControl selects
    // VideoToolbox's dedicated low-delay hardware pipeline (one-in/one-out, no frame
    // reordering), so a submitted frame is emitted within ~one frame instead of being held.
    // This fixes the residual capture→encode HOLD TIME the in-flight cap alone could not remove
    // (measured on-device: cap bounded the max 945→382 ms but VT still held ~2 frames 40–62 ms
    // each under load). Must be passed at session creation — it picks the encoder.
    let encoder_spec = unsafe {
        objc2_core_foundation::CFDictionary::<CFType, CFType>::from_slices(
            &[
                objc2_video_toolbox::kVTVideoEncoderSpecification_EnableLowLatencyRateControl
                    .as_ref(),
            ],
            &[objc2_core_foundation::CFBoolean::new(true).as_ref()],
        )
    };
    let mut session_ptr: *mut VTCompressionSession = std::ptr::null_mut();
    let status = unsafe {
        VTCompressionSession::create(
            None,
            W as i32,
            H as i32,
            objc2_core_media::kCMVideoCodecType_H264,
            Some(encoder_spec.as_opaque()),
            None,
            None,
            None,
            std::ptr::null_mut(),
            std::ptr::NonNull::new(&mut session_ptr).unwrap(),
        )
    };
    if status != 0 || session_ptr.is_null() {
        eprintln!("p5_stream: VTCompressionSessionCreate failed: {status}");
        std::process::exit(3);
    }
    let session = unsafe { Retained::from_raw(session_ptr) }.unwrap();
    unsafe {
        let t: &CFType = (*objc2_core_foundation::kCFBooleanTrue.unwrap()).as_ref();
        let f: &CFType = (*objc2_core_foundation::kCFBooleanFalse.unwrap()).as_ref();
        objc2_video_toolbox::VTSessionSetProperty(
            &session,
            objc2_video_toolbox::kVTCompressionPropertyKey_RealTime,
            Some(t),
        );
        objc2_video_toolbox::VTSessionSetProperty(
            &session,
            objc2_video_toolbox::kVTCompressionPropertyKey_AllowFrameReordering,
            Some(f),
        );
    }

    // --- 4b. Rate control: cap bitrate + force periodic keyframes (item 6 step 2) ----------
    // Measure-before-tune found glass-to-glass ~500 ms dominated by *queue buildup*, not slow
    // stages: the encoder ran uncapped (default bitrate 0 = "encoder decides"), emitting large,
    // bursty access units that overflow the ~103 Mbit/s AOA link, so frames pile up in the host
    // encode channel and the phone's decoder input. Capping the average bitrate keeps frames
    // small enough that USB sustains 60 fps (the root throughput fix), and a 1-second keyframe
    // interval gives drop-to-keyframe (in run_stream_session_instrumented) frequent resync points
    // so any residual backlog is shed as a fresh IDR instead of growing unbounded latency.
    let fps_num = objc2_core_foundation::CFNumber::new_i32(60);
    let bitrate_num = objc2_core_foundation::CFNumber::new_i32(20_000_000); // ~20 Mbit/s
    let keyint_num = objc2_core_foundation::CFNumber::new_i32(60); // 60 frames @ 60 Hz = 1 s
    unsafe {
        let fps: &CFType = (*fps_num).as_ref();
        let br: &CFType = (*bitrate_num).as_ref();
        let ki: &CFType = (*keyint_num).as_ref();
        let s_fps = objc2_video_toolbox::VTSessionSetProperty(
            &session,
            objc2_video_toolbox::kVTCompressionPropertyKey_ExpectedFrameRate,
            Some(fps),
        );
        let s_br = objc2_video_toolbox::VTSessionSetProperty(
            &session,
            objc2_video_toolbox::kVTCompressionPropertyKey_AverageBitRate,
            Some(br),
        );
        let s_ki = objc2_video_toolbox::VTSessionSetProperty(
            &session,
            objc2_video_toolbox::kVTCompressionPropertyKey_MaxKeyFrameInterval,
            Some(ki),
        );
        if s_fps != 0 || s_br != 0 || s_ki != 0 {
            eprintln!(
                "p5_stream: warn — rate-control property status: \
                 ExpectedFrameRate={s_fps} AverageBitRate={s_br} MaxKeyFrameInterval={s_ki}"
            );
        }
    }
    println!("p5_stream: rate control set — 20 Mbit/s avg, keyframe every 1 s (60 fps).");

    // --- 5. Bring up the live AOA transport (P1 sequence) ---------------------
    // Done BEFORE starting capture so we don't buffer frames with no consumer. The connect
    // hello (read below) also guarantees the phone's reader is live before we write.
    let mut transport = match bring_up_aoa() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("p5_stream: AOA bring-up failed: {e}");
            std::process::exit(4);
        }
    };

    // --- 6. Wire the push→pull bridge and start capture -----------------------
    // Bounded hand-off so the producer (60 fps capture+encode) cannot outrun the consumer
    // (~45 fps, gated by the per-frame USB write). Depth 2 = one being-sent + one ready;
    // `try_send` in the encode handler drops the surplus instead of growing latency. (TODO #5)
    let (tx, rx) = mpsc::sync_channel::<EncodedFrame>(2);
    let params: ParamCache = Arc::new(Mutex::new(None));
    let sink = FrameSink::new(session.clone(), tx, Arc::clone(&params));
    let sink_proto: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*sink);
    let queue = dispatch2::DispatchQueue::new("com.rustscreen.stream", None);

    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None)
    };
    if let Err(e) = unsafe {
        stream.addStreamOutput_type_sampleHandlerQueue_error(
            sink_proto,
            SCStreamOutputType::Screen,
            Some(&queue),
        )
    } {
        eprintln!(
            "p5_stream: addStreamOutput failed: {}",
            e.localizedDescription()
        );
        std::process::exit(5);
    }

    let (start_tx, start_rx) = mpsc::channel::<Option<String>>();
    let start_handler = block2::RcBlock::new(move |error: *mut NSError| {
        let _ =
            start_tx.send(unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string()));
    });
    println!(
        "p5_stream: starting live capture → encode → stream (drag windows onto the new display!)…"
    );
    unsafe { stream.startCaptureWithCompletionHandler(Some(&start_handler)) };
    match start_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Some(err)) => {
            eprintln!("p5_stream: startCapture error: {err}");
            std::process::exit(6);
        }
        Ok(None) => {}
        Err(_) => {
            eprintln!("p5_stream: timed out waiting for startCapture.");
            std::process::exit(7);
        }
    }

    // --- 7. Handshake + clock-sync on the full-duplex transport ---------------
    // Latency instrumentation (Task 6): do the handshake explicitly (rather than letting
    // run_stream_session do it internally) so we can run an SNTP-style clock-sync BEFORE
    // streaming, then split the transport into independent read/write halves — a dedicated reader
    // thread pulls inbound `Frame::Stats` while the main thread streams video on the write half.
    use macos_host::session;
    use protocol::clock::{self, ClockOffset};
    use protocol::messages::Frame;
    use std::sync::atomic::AtomicBool;

    let offer = session::host_handshake(W as u32, H as u32, 60);
    // Fix #5: capture the negotiated AgreedConfig and thread it into the stream loop, rather than
    // discarding it and re-deriving the codec from the offer.
    let agreed = match session::perform_handshake(&mut transport, offer) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("p5_stream: handshake failed: {e}");
            drop(vdisplay);
            std::process::exit(8);
        }
    };
    println!(
        "p5_stream: handshake OK ({:?} {}×{}@{}) — running clock-sync…",
        agreed.codec, agreed.width, agreed.height, agreed.refresh_hz
    );

    // --- 7b. Split the transport; spawn the single inbound-frame reader -------
    // Fix #1/#3: split BEFORE clock-sync so ONE reader thread owns the read half and dispatches
    // every inbound frame — `ClockPong` (stamping t3 on receipt and forwarding it for the bounded
    // clock-sync wait) and `Frame::Stats` (forwarded to the stream loop). This makes the blocking
    // bulk-IN read live entirely on the reader thread, so:
    //   (#1) clock-sync no longer does its own blocking read — it waits on a channel with a
    //        timeout, so a phone that never answers degrades to offset=None within 2 s instead of
    //        hanging the host forever (the degrade path is finally reachable), and
    //   (#3) teardown does not depend on unblocking a pending IN read from the main thread: the
    //        reader polls a stop flag between whole frames (via nusb's read timeout, which does
    //        NOT corrupt framing because it only fires while no bytes of a frame are buffered),
    //        and as a hard backstop we join with a timeout + cancel any pending transfer.
    let (read_half, mut write_half) = transport.split();
    let stop = Arc::new(AtomicBool::new(false));
    let (pong_tx, pong_rx) = mpsc::channel::<(Frame, u64)>();
    let (stats_tx, stats_rx) = mpsc::channel::<Frame>();
    let reader_stop = Arc::clone(&stop);
    let reader = std::thread::spawn(move || {
        let mut read_half = read_half;
        // A bounded per-read timeout lets the reader notice the stop flag promptly while idle
        // between frames (the dominant teardown case) instead of blocking forever on a bulk IN
        // the phone will never satisfy once capture stops. nusb keeps the pending transfer alive
        // across a timeout, so re-issuing the read loses no data.
        read_half.set_read_timeout(Duration::from_millis(250));
        loop {
            if reader_stop.load(Ordering::Relaxed) {
                break;
            }
            match Frame::read_from(&mut read_half) {
                Ok(frame @ Frame::ClockPong { .. }) => {
                    // Stamp t3 on receipt (shared monotonic origin) and forward for the
                    // bounded clock-sync wait. If the main thread already moved on, ignore.
                    let _ = pong_tx.send((frame, now_us()));
                }
                Ok(frame @ Frame::Stats { .. }) => {
                    if stats_tx.send(frame).is_err() {
                        break; // main thread ended
                    }
                }
                Ok(_) => {} // ignore other inbound frames
                Err(e) => {
                    // A timeout just means "no frame this window" — loop and re-check the stop
                    // flag. Any other error means the peer is gone / framing broke → exit.
                    if matches!(&e, protocol::messages::MessageError::Io(io)
                        if io.kind() == std::io::ErrorKind::TimedOut)
                    {
                        continue;
                    }
                    break;
                }
            }
        }
    });

    // --- 7c. Clock-sync over the split halves (bounded, degrades gracefully) ---
    // Send the ping on the write half; the reader forwards the pong (with its t3). Wait at most
    // 2 s: if no pong arrives, degrade to offset=None (host-only stage timings, no glass-to-glass)
    // instead of hanging.
    let offset: Option<ClockOffset> = {
        use std::io::Write as _;
        let t0 = now_us();
        let ping = Frame::ClockPing { t0_us: t0 };
        let ping_sent = ping
            .write_to(&mut write_half)
            .and_then(|()| write_half.flush().map_err(Into::into));
        match ping_sent {
            Ok(()) => match pong_rx.recv_timeout(Duration::from_secs(2)) {
                Ok((Frame::ClockPong { t1_us, t2_us, .. }, t3)) => {
                    let off = clock::estimate(t0, t1_us, t2_us, t3);
                    println!(
                        "p5_stream: clock-sync OK — offset={} µs, rtt={} µs (±rtt/2 precision).",
                        off.offset_us, off.rtt_us
                    );
                    Some(off)
                }
                Ok(_) => None, // reader only forwards pongs here, but be defensive
                Err(_) => {
                    eprintln!(
                        "p5_stream: clock-sync timed out (no pong in 2 s); continuing with \
                         host-only stage timings (glass-to-glass unavailable)."
                    );
                    None
                }
            },
            Err(e) => {
                eprintln!(
                    "p5_stream: clock-sync ping failed ({e}); continuing with host-only stage \
                     timings (glass-to-glass unavailable)."
                );
                None
            }
        }
    };

    // --- 7d. Instrumented stream until the phone disconnects -------------------
    println!("p5_stream: streaming with live latency instrumentation…");
    let mut pipeline = macos_host::latency::PipelineLatency::new(256);
    let result = session::run_stream_session_instrumented(
        rx,
        &mut write_half,
        agreed,
        offset,
        &stats_rx,
        &mut pipeline,
        Duration::from_secs(1), // heartbeat / idle-disconnect probe
        Duration::from_secs(2), // print a latency report every ~2 s
        now_us,
        |report| print_report(report, offset),
        &std::sync::atomic::AtomicBool::new(false),
    );

    println!(
        "p5_stream: encoder pacing — shed {} frames total (in-flight cap + channel backpressure); \
         final in-flight depth {}.",
        sink.ivars().dropped.load(Ordering::Relaxed),
        sink.ivars().in_flight.load(Ordering::Relaxed),
    );

    // --- 8. Teardown ----------------------------------------------------------
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let stop_handler = block2::RcBlock::new(move |_e: *mut NSError| {
        let _ = stop_tx.send(());
    });
    unsafe { stream.stopCaptureWithCompletionHandler(Some(&stop_handler)) };
    let _ = stop_rx.recv_timeout(Duration::from_secs(5));
    drop(vdisplay); // remove the virtual display so the Mac desktop reflows
    drop(write_half); // closing the write half also signals the peer we are done
                      // Signal the reader to stop; it observes this between frames via its read timeout. We do NOT
                      // block indefinitely on join: in the pathological case where the reader is parked mid-frame
                      // inside a transfer, joining could hang teardown — so we give it a bounded grace period and
                      // otherwise let the detached thread die with the process. (nusb cannot cancel another
                      // thread's in-flight read from here, so a timeout-bounded join is the robust teardown.)
    stop.store(true, Ordering::Relaxed);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !reader.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if reader.is_finished() {
        let _ = reader.join();
    } else {
        eprintln!(
            "p5_stream: reader thread still parked on a bulk-IN read at teardown; detaching it \
             (it will die with the process) so exit does not hang."
        );
    }

    // Final latency report on exit.
    print_report(&pipeline.report(), offset);

    match result {
        Ok(summary) => println!(
            "p5_stream: session ended cleanly — {} frames, {} bytes sent; encode mean {:.2} ms.",
            summary.frames,
            summary.bytes_sent,
            summary.latency.mean().map_or(0.0, |m| m / 1000.0),
        ),
        Err(e) => eprintln!("p5_stream: session ended with error (likely disconnect): {e}"),
    }
}

/// Print a glass-to-glass latency report: each stage's avg/p50/p95/max in milliseconds, the
/// fused glass-to-glass line, the rtt/offset precision caveat, and the anomaly count.
///
/// When `offset` is `None` (clock-sync failed) only the host-side stages are meaningful — and
/// even those are populated only once a phone `Frame::Stats` fuses, which never happens without
/// an offset — so the host stages print "—" and we note glass-to-glass is unavailable.
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
fn print_report(
    r: &macos_host::latency::LatencyReport,
    offset: Option<protocol::clock::ClockOffset>,
) {
    use macos_host::encode::LatencyStats;

    // Format one stage as "avg/p50/p95/max ms" (µs → ms), or "—" when it has no samples.
    fn line(name: &str, s: &LatencyStats) {
        if s.count() == 0 {
            println!("  {name:<16} —");
            return;
        }
        let ms = |us: f64| us / 1000.0;
        let avg = s.mean().unwrap_or(0.0);
        let p50 = s.p50().unwrap_or(0) as f64;
        let p95 = s.p95().unwrap_or(0) as f64;
        let max = s.max().unwrap_or(0) as f64;
        println!(
            "  {name:<16} avg {:>6.2}  p50 {:>6.2}  p95 {:>6.2}  max {:>6.2}  ms  (n={})",
            ms(avg),
            ms(p50),
            ms(p95),
            ms(max),
            s.count(),
        );
    }

    println!("p5_stream: ─── latency report ───");
    match offset {
        Some(off) => {
            line("capture→encode", &r.capture_to_encode);
            line("encode→send", &r.encode_to_send);
            line("send→arrive", &r.send_to_arrive);
            line("arrive→decode", &r.arrive_to_decode);
            line("decode→present", &r.decode_to_present);
            line("GLASS→GLASS", &r.glass_to_glass);
            println!(
                "  offset={} µs  rtt={} µs  (±rtt/2 = ±{} µs precision on every fused number)",
                off.offset_us,
                off.rtt_us,
                off.rtt_us / 2,
            );
        }
        None => {
            line("capture→encode", &r.capture_to_encode);
            line("encode→send", &r.encode_to_send);
            println!("  glass-to-glass unavailable (clock-sync failed — no host↔phone offset).");
        }
    }
    println!("  anomalies (clock jitter, clamped to 0): {}", r.anomalies);
    println!(
        "  unmatched phone stats (no host record / evicted): {}",
        r.unmatched_stats
    );
    println!(
        "  dropped frames (drop-to-keyframe shed load): {}",
        r.dropped_frames
    );
}

/// Bring up the live AOA transport and read the device's connect-hello.
///
/// Mirrors `p1_echo`: enumerate → device-level AOA handshake (req 51/52/53) → re-enumerate in
/// accessory mode → claim the accessory interface → read the one-frame hello the phone sends the
/// instant it owns the accessory fd (so its reader is live before we write the handshake).
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
fn bring_up_aoa() -> std::io::Result<macos_host::aoa::AoaTransport> {
    use macos_host::aoa;
    use macos_host::transport::recv_frame;
    use nusb::MaybeFuture;
    use std::time::Duration;

    println!("p5_stream: finding the Pixel over USB…");
    let candidate = aoa::find_candidate()?;
    let dev = candidate
        .open()
        .wait()
        .map_err(|e| std::io::Error::other(format!("open candidate: {e}")))?;
    println!("p5_stream: sending AOA handshake (req 51/52/53)…");
    aoa::handshake(&dev)?;
    drop(dev);
    println!("p5_stream: waiting for accessory-mode re-enumeration…");
    let acc = aoa::reacquire(Duration::from_secs(5))?;
    let mut transport = aoa::open_transport(&acc)?;
    println!("p5_stream: accessory claimed — waiting for device hello (app opened the accessory)…");
    let (_tag, hello) = recv_frame(&mut transport)?;
    println!(
        "p5_stream: device hello received ({} bytes) — phone reader live.",
        hello.len()
    );
    Ok(transport)
}
