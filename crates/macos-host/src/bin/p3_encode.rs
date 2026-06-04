//! P3 encode spike (hands-on-Mac, Task B2/B3). Compiled ONLY under `--features live-capture`.
//!
//! Final P3 Wave B milestone: capture the virtual display (proven by `p3_capture`) AND
//! hardware-encode it to H.264 with VideoToolbox, writing a playable `out.h264` — the
//! ffplay visual gate is P3 success criterion #1. Per-frame pipeline:
//!
//!   SCStream CVPixelBuffer → VTCompressionSession (H.264, RealTime, no B-frames)
//!     → output handler: AVCC CMBlockBuffer + SPS/PPS (format desc)
//!     → `to_annex_b_frame` (Wave A) → append to out.h264
//!
//! Build: `cargo build -p macos-host --features live-capture` → `target/debug/p3_encode`.
//! Run from a terminal granted **Screen & System Audio Recording**. Then:
//!   `ffplay -autoexit out.h264`  (or `ffprobe out.h264`) to confirm it decodes.

#[cfg(not(feature = "live-capture"))]
fn main() {
    eprintln!("p3_encode requires --features live-capture (macOS, hands-on). Rebuild with it.");
}

#[cfg(feature = "live-capture")]
use std::sync::atomic::{AtomicI64, Ordering};
#[cfg(feature = "live-capture")]
use std::sync::{Arc, Mutex};

// In scope by bare name for the `define_class!` delegate below.
#[cfg(feature = "live-capture")]
use objc2::runtime::NSObjectProtocol;
#[cfg(feature = "live-capture")]
use objc2::DefinedClass;
#[cfg(feature = "live-capture")]
use objc2_core_media::CMSampleBuffer;
#[cfg(feature = "live-capture")]
use objc2_screen_capture_kit::{SCStream, SCStreamOutput, SCStreamOutputType};

/// Accumulated encoder output, shared between the capture (GCD) thread that submits frames and
/// the VideoToolbox thread that runs the output handler.
#[cfg(feature = "live-capture")]
#[derive(Default)]
struct EncoderOut {
    /// The Annex-B byte stream destined for out.h264.
    annexb: Vec<u8>,
    /// Set once we've written the out-of-band SPS/PPS (from the first sample's format desc).
    wrote_params: bool,
    /// AVCC NAL length-prefix width from the format description (usually 4).
    nal_len: usize,
    /// SPS / PPS byte lengths — logged for criterion #2 (codec config extracted for downstream decode).
    sps_len: usize,
    pps_len: usize,
    /// Count of encoded output samples seen.
    frames_out: u64,
    /// Per-frame VideoToolbox encode latency (submit → output-handler fired).
    /// Criterion #3: per-frame encode latency, distinct from the frame-arrival wall cadence.
    latency: macos_host::latency::LatencyAccum,
}

#[cfg(feature = "live-capture")]
struct FrameSinkIvars {
    session: objc2::rc::Retained<objc2_video_toolbox::VTCompressionSession>,
    out: Arc<Mutex<EncoderOut>>,
    pts_idx: AtomicI64,
}

#[cfg(feature = "live-capture")]
objc2::define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    #[name = "RSEncodeSink"]
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
            // The CVImageBuffer (pixel buffer) is the encoder input.
            let Some(image) = (unsafe { sample_buffer.image_buffer() }) else {
                return;
            };
            // PTS is a monotonic frame counter on a 60 Hz timebase (idx/60 s). For a raw
            // .h264 elementary stream (no container timestamps) only monotonicity matters;
            // ffplay renders at its assumed frame rate. A live muxer/RTP path (P5) would
            // instead stamp wall-clock-derived PTS so playback timing matches real time.
            let idx = self.ivars().pts_idx.fetch_add(1, Ordering::Relaxed);
            let pts = objc2_core_media::CMTime::new(idx, 60);
            let dur = objc2_core_media::CMTime::new(1, 60);

            let out = Arc::clone(&self.ivars().out);
            // Stamp submit time INTO this frame's handler closure — when VideoToolbox fires it,
            // `submit_t.elapsed()` is exactly this one frame's encode latency (criterion #3).
            let submit_t = std::time::Instant::now();
            // Output handler: VideoToolbox calls this (possibly on its own thread) with the
            // compressed sample. We pull the AVCC bytes + SPS/PPS and append Annex-B.
            let handler = block2::RcBlock::new(
                move |status: i32,
                      _flags: objc2_video_toolbox::VTEncodeInfoFlags,
                      sbuf: *mut CMSampleBuffer| {
                    if status != 0 {
                        return;
                    }
                    let Some(sbuf) = (unsafe { sbuf.as_ref() }) else {
                        return;
                    };
                    let mut st = out.lock().unwrap();
                    // On the first sample, emit the out-of-band SPS/PPS in-band (Annex-B) so the
                    // stream is self-describing before the first IDR.
                    //
                    // SPIKE LIMITATION: this injects parameter sets ONCE (first frame only). That
                    // is correct for a fixed-config session whose only IDR is at the start. For P5
                    // live streaming, where a client may attach mid-stream, SPS/PPS must be
                    // RE-INJECTED ahead of EVERY keyframe — which needs per-sample keyframe
                    // detection (kCMSampleAttachmentKey_NotSync). Deferred to P5.
                    if !st.wrote_params {
                        if let Some(fmt) = unsafe { sbuf.format_description() } {
                            if let Some((sps, pps, nal_len)) = unsafe { h264_params(&fmt) } {
                                st.nal_len = nal_len;
                                st.sps_len = sps.len();
                                st.pps_len = pps.len();
                                println!(
                                    "p3_encode: codec config — SPS {} B, PPS {} B, nal_len={} (extracted for downstream decode)",
                                    sps.len(),
                                    pps.len(),
                                    nal_len
                                );
                                // Reuse the tested Wave-A helper for the in-band param prepend:
                                // with an EMPTY picture payload, `to_annex_b_frame` emits exactly
                                // `start-code + SPS + start-code + PPS` (the picture NALs are
                                // appended below). Keeps the start-code framing in one tested place.
                                let params = macos_host::encode_vt::to_annex_b_frame(
                                    &[],
                                    nal_len,
                                    true,
                                    &sps,
                                    &pps,
                                );
                                st.annexb.extend_from_slice(&params);
                                st.wrote_params = true;
                            }
                        }
                    }
                    if let Some(avcc) = unsafe { block_buffer_bytes(sbuf) } {
                        let nal_len = if st.nal_len == 0 { 4 } else { st.nal_len };
                        let annexb = macos_host::encode_vt::avcc_to_annex_b(&avcc, nal_len);
                        st.annexb.extend_from_slice(&annexb);
                        st.frames_out += 1;
                        st.latency.record(submit_t.elapsed().as_micros());
                    }
                },
            );
            let mut info = objc2_video_toolbox::VTEncodeInfoFlags::empty();
            // SAFETY / lifetime: the handler block may be invoked asynchronously on a VT
            // thread AFTER this call returns (per the VideoToolbox docs), i.e. after the
            // local `handler` RcBlock is dropped. This is sound because
            // VTCompressionSessionEncodeFrameWithOutputHandler `Block_copy`s the handler
            // during the call, and `block2::RcBlock` is heap-allocated — so the copy is a
            // refcount bump on the same block, keeping it alive until VT releases it after
            // firing. (Empirically confirmed: 57 frames produced a clean, ffplay-decodable
            // stream.) `&*handler` is a valid block pointer for the duration of this call.
            let status = unsafe {
                self.ivars().session.encode_frame_with_output_handler(
                    &image,
                    pts,
                    dur,
                    None,
                    &mut info,
                    &*handler as *const _ as *mut _,
                )
            };
            if status != 0 {
                eprintln!("p3_encode: EncodeFrame returned status {status}");
            }
        }
    }
);

#[cfg(feature = "live-capture")]
impl FrameSink {
    fn new(
        session: objc2::rc::Retained<objc2_video_toolbox::VTCompressionSession>,
        out: Arc<Mutex<EncoderOut>>,
    ) -> objc2::rc::Retained<Self> {
        use objc2::AllocAnyThread;
        let this = Self::alloc().set_ivars(FrameSinkIvars {
            session,
            out,
            pts_idx: AtomicI64::new(0),
        });
        unsafe { objc2::msg_send![super(this), init] }
    }
}

/// Extract (SPS, PPS, nal_length_size) from an H.264 `CMFormatDescription`.
///
/// # Safety
/// `fmt` must be a valid H.264 video format description.
#[cfg(feature = "live-capture")]
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

/// Copy the contiguous AVCC bytes out of a compressed sample's `CMBlockBuffer`.
///
/// # Safety
/// `sbuf` must be a valid compressed `CMSampleBuffer`.
#[cfg(feature = "live-capture")]
unsafe fn block_buffer_bytes(sbuf: &CMSampleBuffer) -> Option<Vec<u8>> {
    use std::os::raw::c_char;
    let bb = unsafe { sbuf.data_buffer() }?;
    let mut len_at_offset: usize = 0;
    let mut total_len: usize = 0;
    let mut data: *mut c_char = std::ptr::null_mut();
    let status = unsafe { bb.data_pointer(0, &mut len_at_offset, &mut total_len, &mut data) };
    if status != 0 || data.is_null() || total_len == 0 {
        return None;
    }
    // `len_at_offset` is the contiguous run starting at offset 0; `total_len` is the
    // buffer's full logical length. VideoToolbox H.264 output is a single contiguous block
    // (len_at_offset == total_len), so we copy `total_len` bytes from `data`. GUARD against
    // a (theoretical) non-contiguous buffer: if `len_at_offset < total_len`, `data` points
    // only at the first segment and reading `total_len` bytes would over-read past it.
    // Bail rather than risk an OOB read. (Production fix for non-contiguous buffers:
    // CMBlockBufferCopyDataBytes into a `total_len`-sized Vec — deferred; VT never produces
    // them here.)
    if len_at_offset != total_len {
        eprintln!(
            "p3_encode: warn — non-contiguous CMBlockBuffer ({len_at_offset} of {total_len} B contiguous); dropping frame"
        );
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(data as *const u8, total_len) }.to_vec())
}

#[cfg(feature = "live-capture")]
fn main() {
    use cg_virtual_display::VirtualDisplay;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::AnyThread;
    use objc2_core_foundation::CFType;
    use objc2_foundation::{NSArray, NSError};
    use objc2_screen_capture_kit::{
        SCContentFilter, SCDisplay, SCShareableContent, SCStreamConfiguration, SCWindow,
    };
    use objc2_video_toolbox::VTCompressionSession;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const W: usize = 2400;
    const H: usize = 1080;

    println!("p3_encode: creating virtual display {W}×{H}@60…");
    let vdisplay = match VirtualDisplay::new(W as u32, H as u32, 60.0) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("p3_encode: failed to create virtual display: {e}");
            std::process::exit(1);
        }
    };
    let target_id = vdisplay.display_id();
    std::thread::sleep(Duration::from_millis(500));

    // Locate the SCDisplay (transfer it main-ward as a raw pointer; see p3_capture).
    let (tx, rx) = mpsc::channel::<usize>();
    let handler =
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
            let _ = tx.send(found);
        });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    let display: Retained<SCDisplay> = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(p) if p != 0 => unsafe { Retained::from_raw(p as *mut SCDisplay) }.unwrap(),
        _ => {
            eprintln!("p3_encode: could not find SCDisplay {target_id} (Screen Recording grant?)");
            std::process::exit(2);
        }
    };

    // SCStream config: deliver 420v (NV12) frames at the display size — the native H.264 input.
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
        config.setShowsCursor(true);
        // '420v' = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange.
        config.setPixelFormat(u32::from_be_bytes(*b"420v"));
    }

    // Build the VideoToolbox H.264 compression session (no C callback — block handler path).
    let mut session_ptr: *mut VTCompressionSession = std::ptr::null_mut();
    let status = unsafe {
        VTCompressionSession::create(
            None,
            W as i32,
            H as i32,
            objc2_core_media::kCMVideoCodecType_H264,
            None,
            None,
            None,
            None, // output callback: NULL → use EncodeFrameWithOutputHandler
            std::ptr::null_mut(),
            std::ptr::NonNull::new(&mut session_ptr).unwrap(),
        )
    };
    if status != 0 || session_ptr.is_null() {
        eprintln!("p3_encode: VTCompressionSessionCreate failed: {status}");
        std::process::exit(3);
    }
    let session = unsafe { Retained::from_raw(session_ptr) }.unwrap();
    // Low-latency: real-time + no frame reordering (no B-frames → output order = input order).
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

    let out = Arc::new(Mutex::new(EncoderOut::default()));
    let sink = FrameSink::new(session.clone(), Arc::clone(&out));
    let sink_proto: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*sink);
    let queue = dispatch2::DispatchQueue::new("com.rustscreen.encode", None);

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
            "p3_encode: addStreamOutput failed: {}",
            e.localizedDescription()
        );
        std::process::exit(4);
    }

    let (start_tx, start_rx) = mpsc::channel::<Option<String>>();
    let start_handler = block2::RcBlock::new(move |error: *mut NSError| {
        let _ =
            start_tx.send(unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string()));
    });
    println!("p3_encode: starting capture+encode for 3s (move a window onto the new display!)…");
    let t0 = Instant::now();
    unsafe { stream.startCaptureWithCompletionHandler(Some(&start_handler)) };
    match start_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Some(err)) => {
            eprintln!("p3_encode: startCapture error: {err}");
            std::process::exit(5);
        }
        Ok(None) => {}
        Err(_) => {
            eprintln!("p3_encode: timed out waiting for startCapture.");
            std::process::exit(6);
        }
    }

    std::thread::sleep(Duration::from_secs(3));

    // Stop capture, flush the encoder, and write the file.
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let stop_handler = block2::RcBlock::new(move |_e: *mut NSError| {
        let _ = stop_tx.send(());
    });
    unsafe { stream.stopCaptureWithCompletionHandler(Some(&stop_handler)) };
    let _ = stop_rx.recv_timeout(Duration::from_secs(5));
    // BLOCKS until every pending frame (PTS ≤ i64::MAX/60 — i.e. all of them) has been
    // emitted: "frames with presentation timestamps up to and including this timestamp
    // will be emitted before the function returns". So when this returns, all output
    // handlers have fired and the `out` lock below sees the complete stream (no drain race).
    unsafe {
        session.complete_frames(objc2_core_media::CMTime::new(i64::MAX, 60));
    }
    let elapsed = t0.elapsed();

    let st = out.lock().unwrap();
    if st.frames_out == 0 {
        eprintln!("p3_encode: ⚠️ zero frames encoded — nothing to write.");
        std::process::exit(7);
    }
    let path = "out.h264";
    if let Err(e) = std::fs::write(path, &st.annexb) {
        eprintln!("p3_encode: failed to write {path}: {e}");
        std::process::exit(8);
    }
    let ms_per_frame = elapsed.as_secs_f64() * 1000.0 / st.frames_out as f64;
    println!(
        "p3_encode: ✅ wrote {path}: {} frames, {} bytes, nal_len={}, ~{:.1} ms/frame wall cadence.",
        st.frames_out,
        st.annexb.len(),
        st.nal_len,
        ms_per_frame
    );
    println!(
        "p3_encode: codec config: SPS {} B, PPS {} B (in-band before first IDR).",
        st.sps_len, st.pps_len
    );
    println!(
        "p3_encode: per-frame ENCODE latency — avg {:.2} ms, min {:.2} ms, max {:.2} ms (RealTime, no B-frames).",
        st.latency.avg_us() / 1000.0,
        st.latency.min_us().map_or(0.0, |v| v as f64 / 1000.0),
        st.latency.max_us().map_or(0.0, |v| v as f64 / 1000.0),
    );
    println!("  → verify: ffplay -autoexit {path}   (or: ffprobe {path})");
}
