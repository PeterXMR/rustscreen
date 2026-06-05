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
use std::sync::atomic::{AtomicI64, Ordering};
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

/// Cached H.264 parameter sets (SPS, PPS) + AVCC NAL-length size, read once from the first
/// sample's format description and reused to inject params in-band ahead of every keyframe.
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
type ParamCache = Arc<Mutex<Option<(Vec<u8>, Vec<u8>, usize)>>>;

#[cfg(all(feature = "live-capture", feature = "live-usb"))]
struct FrameSinkIvars {
    session: objc2::rc::Retained<objc2_video_toolbox::VTCompressionSession>,
    /// Producer half of the push→pull bridge. Each encoded access unit is sent here; the main
    /// thread's `run_stream_session` is the consumer.
    tx: mpsc::Sender<EncodedFrame>,
    params: ParamCache,
    pts_idx: AtomicI64,
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
            let idx = self.ivars().pts_idx.fetch_add(1, Ordering::Relaxed);
            // Live presentation timestamps: a monotonic 60 Hz frame counter expressed in
            // microseconds. The decoder uses pts only for ordering; a 1-vsync grid is fine.
            let pts_us = (idx as u64).saturating_mul(16_666);
            let pts = objc2_core_media::CMTime::new(idx, 60);
            let dur = objc2_core_media::CMTime::new(1, 60);

            let tx = self.ivars().tx.clone();
            let params = Arc::clone(&self.ivars().params);
            // Stamp submit time into the handler; when VideoToolbox fires it, the elapsed time
            // is this one frame's encode latency.
            let submit_t = std::time::Instant::now();
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
                        keyframe,
                        encode_micros: submit_t.elapsed().as_micros() as u64,
                        annex_b,
                    };
                    // A send error means the consumer (run_stream_session) has ended — the phone
                    // disconnected. Stop submitting; the main thread tears down.
                    let _ = tx.send(frame);
                },
            );
            let mut info = objc2_video_toolbox::VTEncodeInfoFlags::empty();
            // SAFETY: VideoToolbox `Block_copy`s the handler during this call (see p3_encode);
            // the heap `RcBlock` stays alive until VT releases it after firing.
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
                eprintln!("p5_stream: EncodeFrame returned status {status}");
            }
        }
    }
);

#[cfg(all(feature = "live-capture", feature = "live-usb"))]
impl FrameSink {
    fn new(
        session: objc2::rc::Retained<objc2_video_toolbox::VTCompressionSession>,
        tx: mpsc::Sender<EncodedFrame>,
        params: ParamCache,
    ) -> objc2::rc::Retained<Self> {
        use objc2::AllocAnyThread;
        let this = Self::alloc().set_ivars(FrameSinkIvars {
            session,
            tx,
            params,
            pts_idx: AtomicI64::new(0),
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
    }

    // --- 4. VideoToolbox H.264 session (RealTime, no B-frames) -----------------
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
    let (tx, rx) = mpsc::channel::<EncodedFrame>();
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

    // --- 7. Stream until the phone disconnects --------------------------------
    // run_stream_session does the handshake then drains the channel, sending VideoConfig
    // before every keyframe + Video per access unit, until the transport errors (disconnect).
    let offer = macos_host::session::host_handshake(W as u32, H as u32, 60);
    let result = macos_host::session::run_stream_session(rx, &mut transport, offer);

    // --- 8. Teardown ----------------------------------------------------------
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let stop_handler = block2::RcBlock::new(move |_e: *mut NSError| {
        let _ = stop_tx.send(());
    });
    unsafe { stream.stopCaptureWithCompletionHandler(Some(&stop_handler)) };
    let _ = stop_rx.recv_timeout(Duration::from_secs(5));
    drop(vdisplay); // remove the virtual display so the Mac desktop reflows

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
