//! P3 capture probe (hands-on-Mac, Task B-probe). Compiled ONLY under `--features live-capture`.
//!
//! First hands-on milestone of P3 Wave B: answer the keystone unknown **before** building the
//! full encode pipeline — *can ScreenCaptureKit even SEE the P2 virtual display?* (Apple
//! FB17797423: a `CGVirtualDisplay` may be invisible to SCK, in which case we must fall back to
//! CGDisplayStream.) It brings up the phantom display, asks SCK for its shareable displays, and
//! reports whether our virtual display's `CGDirectDisplayID` is in the list.
//!
//! Build: `cargo build -p macos-host --features live-capture` → `target/debug/p3_probe`.
//! Requires the **Screen Recording** TCC grant (System Settings → Privacy & Security → Screen
//! Recording) or SCK returns an empty/black display list.

#[cfg(not(feature = "live-capture"))]
fn main() {
    eprintln!(
        "p3_probe requires --features live-capture (macOS, hands-on). Rebuild with that feature."
    );
}

#[cfg(feature = "live-capture")]
fn main() {
    use cg_virtual_display::VirtualDisplay;
    use objc2_screen_capture_kit::SCShareableContent;
    use std::sync::mpsc;
    use std::time::Duration;

    // 1. Bring up the P2 phantom display (geometry per D6: 2400×1080@60). Hold the handle for
    //    the whole probe — dropping it tears the display down.
    println!("p3_probe: creating virtual display 2400×1080@60…");
    let vdisplay = match VirtualDisplay::new(2400, 1080, 60.0) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("p3_probe: failed to create virtual display: {e}");
            std::process::exit(1);
        }
    };
    let target_id = vdisplay.display_id();
    println!("p3_probe: virtual display created — CGDirectDisplayID = {target_id}");
    // Give the window server a beat to register the new display before we enumerate.
    std::thread::sleep(Duration::from_millis(500));

    // 2. Ask ScreenCaptureKit for its shareable displays. The result arrives via an ObjC block
    //    invoked on an internal GCD queue; we extract the display ids INSIDE the block (so we
    //    never hold the ObjC objects past the callback) and hand them to the main thread.
    let (tx, rx) = mpsc::channel::<Result<Vec<u32>, String>>();
    let handler = block2::RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut objc2_foundation::NSError| {
            // SAFETY: SCK passes either a non-null content or a non-null error (never both null);
            // both pointers are valid for the duration of this callback.
            if let Some(content) = unsafe { content.as_ref() } {
                let mut ids = Vec::new();
                // SAFETY: `content` is a live SCShareableContent for the callback's duration;
                // `displays` returns its current display list with no extra invariants.
                let displays = unsafe { content.displays() };
                for display in displays.iter() {
                    // SAFETY: `display` is a live SCDisplay for the callback's duration.
                    ids.push(unsafe { display.displayID() });
                }
                let _ = tx.send(Ok(ids));
            } else if let Some(error) = unsafe { error.as_ref() } {
                let _ = tx.send(Err(error.localizedDescription().to_string()));
            } else {
                let _ = tx.send(Err("SCK returned neither content nor error".to_string()));
            }
        },
    );

    println!("p3_probe: querying SCShareableContent (needs Screen Recording permission)…");
    // SAFETY: `handler` outlives the call; SCK retains it until the callback fires.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };

    // 3. Block for the result (the callback fires from a GCD thread, so no run loop needed).
    let shareable = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(ids)) => ids,
        Ok(Err(e)) => {
            eprintln!("p3_probe: SCK error: {e}");
            eprintln!("  → most likely the Screen Recording TCC grant is missing.");
            std::process::exit(2);
        }
        Err(_) => {
            eprintln!("p3_probe: timed out waiting for SCShareableContent (10s).");
            eprintln!("  → SCK never called back; check Screen Recording permission / run loop.");
            std::process::exit(3);
        }
    };

    // 4. Verdict: is the virtual display in the shareable list?
    println!(
        "p3_probe: SCK reports {} shareable display(s): {:?}",
        shareable.len(),
        shareable
    );
    if shareable.contains(&target_id) {
        println!("p3_probe: ✅ VIRTUAL DISPLAY {target_id} IS visible to ScreenCaptureKit.");
        println!("  → P3 capture can use the SCK adapter. FB17797423 does NOT bite us.");
    } else {
        println!("p3_probe: ⚠️  virtual display {target_id} is NOT in SCK's shareable list.");
        println!("  → FB17797423 likely bites: SCK can't see the CGVirtualDisplay.");
        println!("  → Plan to capture via the CGDisplayStream fallback instead.");
    }
}
