//! P3 capture spike (hands-on-Mac, Task B1). Compiled ONLY under `--features live-capture`.
//!
//! Second hands-on milestone of P3 Wave B (after `p3_probe` proved SCK can *see* the virtual
//! display): prove SCK actually *delivers frames* from it. Brings up the P2 virtual display,
//! builds an `SCStream` filtered to that display, installs an `SCStreamOutput` delegate, runs
//! capture for ~2 s, and reports how many frames arrived. This is the test of the one capture
//! footgun the research flagged — the `SCStreamOutput` delegate not firing.
//!
//! Build: `cargo build -p macos-host --features live-capture` → `target/debug/p3_capture`.
//! Requires the **Screen & System Audio Recording** TCC grant (run from a granted terminal).

#[cfg(not(feature = "live-capture"))]
fn main() {
    eprintln!("p3_capture requires --features live-capture (macOS, hands-on). Rebuild with it.");
}

#[cfg(feature = "live-capture")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "live-capture")]
use std::sync::Arc;

// `define_class!` matches the implemented protocol as a bare identifier, so these traits/types
// must be in scope by name (not referenced via a path) inside the macro below.
#[cfg(feature = "live-capture")]
use objc2::runtime::NSObjectProtocol;
#[cfg(feature = "live-capture")]
use objc2::DefinedClass;
#[cfg(feature = "live-capture")]
use objc2_core_media::CMSampleBuffer;
#[cfg(feature = "live-capture")]
use objc2_screen_capture_kit::{SCStream, SCStreamOutput, SCStreamOutputType};

// The frame-sink delegate: a custom NSObject subclass implementing the SCStreamOutput protocol.
// SCK invokes `stream:didOutputSampleBuffer:ofType:` on our sample-handler dispatch queue for
// every captured frame; we just count screen frames into a shared atomic.
#[cfg(feature = "live-capture")]
struct FrameSinkIvars {
    screen_frames: Arc<AtomicU64>,
}

#[cfg(feature = "live-capture")]
objc2::define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    #[name = "RSFrameSink"]
    #[ivars = FrameSinkIvars]
    struct FrameSink;

    unsafe impl NSObjectProtocol for FrameSink {}

    unsafe impl SCStreamOutput for FrameSink {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output(
            &self,
            _stream: &SCStream,
            _sample_buffer: &CMSampleBuffer,
            output_type: SCStreamOutputType,
        ) {
            if output_type == SCStreamOutputType::Screen {
                self.ivars().screen_frames.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
);

#[cfg(feature = "live-capture")]
impl FrameSink {
    fn new(screen_frames: Arc<AtomicU64>) -> objc2::rc::Retained<Self> {
        use objc2::AllocAnyThread;
        let this = Self::alloc().set_ivars(FrameSinkIvars { screen_frames });
        unsafe { objc2::msg_send![super(this), init] }
    }
}

#[cfg(feature = "live-capture")]
fn main() {
    use cg_virtual_display::VirtualDisplay;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::AnyThread;
    use objc2_foundation::{NSArray, NSError};
    // SCStream / SCStreamOutput / SCStreamOutputType are imported at module scope (above).
    use objc2_screen_capture_kit::{
        SCContentFilter, SCDisplay, SCShareableContent, SCStreamConfiguration, SCWindow,
    };
    use std::sync::mpsc;
    use std::time::Duration;

    // 1. Bring up the phantom display and hold it for the whole run.
    println!("p3_capture: creating virtual display 2400×1080@60…");
    let vdisplay = match VirtualDisplay::new(2400, 1080, 60.0) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("p3_capture: failed to create virtual display: {e}");
            std::process::exit(1);
        }
    };
    let target_id = vdisplay.display_id();
    println!("p3_capture: virtual display CGDirectDisplayID = {target_id}");
    std::thread::sleep(Duration::from_millis(500));

    // 2. Find the matching SCDisplay via the async shareable-content query. SCDisplay isn't
    //    Send, so we transfer ownership across the GCD callback → main thread as a raw pointer
    //    (sound: SCDisplay is an immutable descriptor, safe to use from another thread).
    let (tx, rx) = mpsc::channel::<usize>();
    let handler = block2::RcBlock::new(
        move |content: *mut SCShareableContent, _error: *mut NSError| {
            let mut found: usize = 0;
            if let Some(content) = unsafe { content.as_ref() } {
                let displays = unsafe { content.displays() };
                for display in displays.iter() {
                    if unsafe { display.displayID() } == target_id {
                        // `display` is an owned Retained<SCDisplay> from the iterator; leak it
                        // as a raw pointer to hand ownership to the main thread (reclaimed once).
                        found = Retained::into_raw(display) as usize;
                        break;
                    }
                }
            }
            let _ = tx.send(found);
        },
    );
    println!("p3_capture: locating SCDisplay {target_id} via SCShareableContent…");
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    let display_ptr = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(0) | Err(_) => {
            eprintln!("p3_capture: could not find SCDisplay {target_id} (TCC grant missing?).");
            std::process::exit(2);
        }
        Ok(p) => p,
    };
    // SAFETY: `display_ptr` is the leaked Retained<SCDisplay> from the block; reclaim it once.
    let display: Retained<SCDisplay> =
        unsafe { Retained::from_raw(display_ptr as *mut SCDisplay) }.unwrap();

    // 3. Build the content filter (just this display, no excluded windows) and a basic config.
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
        config.setWidth(2400);
        config.setHeight(1080);
        config.setShowsCursor(true);
    }

    // 4. Create the stream (no stream-level delegate), attach our frame sink on a dedicated
    //    dispatch queue, and start capture.
    let screen_frames = Arc::new(AtomicU64::new(0));
    let sink = FrameSink::new(Arc::clone(&screen_frames));
    let sink_proto: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*sink);
    let queue = dispatch2::DispatchQueue::new("com.rustscreen.capture", None);

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
            "p3_capture: addStreamOutput failed: {}",
            e.localizedDescription()
        );
        std::process::exit(3);
    }

    let (start_tx, start_rx) = mpsc::channel::<Option<String>>();
    let start_handler = block2::RcBlock::new(move |error: *mut NSError| {
        let msg = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = start_tx.send(msg);
    });
    println!("p3_capture: starting capture…");
    unsafe { stream.startCaptureWithCompletionHandler(Some(&start_handler)) };
    match start_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Some(err)) => {
            eprintln!("p3_capture: startCapture error: {err}");
            std::process::exit(4);
        }
        Ok(None) => println!("p3_capture: capture started — collecting frames for 2s…"),
        Err(_) => {
            eprintln!("p3_capture: timed out waiting for startCapture callback.");
            std::process::exit(5);
        }
    }

    // 5. Let frames accumulate on the GCD queue, then stop and report.
    std::thread::sleep(Duration::from_secs(2));
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let stop_handler = block2::RcBlock::new(move |_error: *mut NSError| {
        let _ = stop_tx.send(());
    });
    unsafe { stream.stopCaptureWithCompletionHandler(Some(&stop_handler)) };
    let _ = stop_rx.recv_timeout(Duration::from_secs(5));

    let n = screen_frames.load(Ordering::Relaxed);
    println!("p3_capture: captured {n} screen frame(s) in ~2s.");
    if n > 0 {
        println!(
            "p3_capture: ✅ SCStreamOutput delegate FIRES — frames flow from display {target_id}."
        );
        println!("  → capture path proven. Next: feed CVPixelBuffers to a VideoToolbox encoder.");
        println!(
            "  (An idle/empty virtual desktop emits few frames by design — SCFrameStatus.idle;"
        );
        println!("   move a window onto the extended display to see the rate climb.)");
    } else {
        println!("p3_capture: ⚠️ zero frames — delegate never fired (run-loop / queue footgun).");
        std::process::exit(6);
    }
}
