//! P5 live-streaming host spike (item 1 / PR #21, hands-on-Mac+phone). Compiled ONLY under
//! `--features live-capture,live-usb`.
//!
//! Thin wrapper over [`macos_host::serve::run_host`], which holds the live pipeline (virtual
//! display → ScreenCaptureKit capture → VideoToolbox H.264 → AOA transport → phone). The
//! pipeline was extracted from this spike so it and the `rustscreen` daemon drive the identical,
//! on-device-verified code path; this entry point keeps working unchanged (the `run-on-device`
//! skill still invokes `p5_stream`). It runs in the foreground with a stop flag that is never
//! set here — Ctrl-C kills the foreground process as before.
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
fn main() {
    let stop = std::sync::atomic::AtomicBool::new(false);
    if let Err(e) = macos_host::serve::run_host(&macos_host::serve::HostOpts::default(), &stop) {
        eprintln!("p5_stream: {e}");
        std::process::exit(1);
    }
}
