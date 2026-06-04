//! RustScreen Android client (cdylib). JNI entry points called by the thin Kotlin shell (D7).
//! All app logic lives here in Rust; the Kotlin shell is glue only and will be replaced by
//! NativeActivity in a later phase.

/// Transport core: the platform-agnostic `echo_loop` (host-testable) plus the
/// android-only `AccessoryFdTransport`. `echo_loop` is NOT cfg-gated so CI exercises it.
pub mod transport;

/// Touch input core (P6): pure conversion of a raw Android `MotionEvent` sample into a
/// normalized `protocol::messages::TouchEvent` — the inverse of the host's coordinate
/// mapping. NOT cfg-gated, so CI exercises it (the JNI/Kotlin capture shim is glue).
pub mod touch;

/// Decode core (P4 Wave A): the platform-agnostic `VideoDecoder` port + `DecodeSession`
/// orchestrator that turns protocol `Frame`s into configure/decode calls. NOT cfg-gated,
/// so CI exercises it against a fake. The `AMediaCodec` decode-to-surface adapter, JNI
/// surface plumbing, and Kotlin `SurfaceView` (P4 Wave B) are hardware-blocked (the Pixel)
/// and drop in behind the port — see the module docs.
pub mod decode;

/// Client receive-session orchestrator (P5 PIPE-01 criterion #3): platform-agnostic
/// receive loop that performs the client handshake and routes protocol `Frame`s into the
/// `DecodeSession` / `VideoDecoder` port. NOT cfg-gated — CI exercises it in full.
pub mod session;

/// P4 Wave B (DEC-01): the concrete `ndk-sys` `AMediaCodec` decode-to-surface adapter
/// implementing the [`decode::VideoDecoder`] port. Hardware-blocked (needs the Pixel 6a),
/// so it is `#[cfg(target_os = "android")]` AND behind the `live-decode` feature — the
/// default `cargo build --workspace` pulls no `ndk`/`ndk-sys` dependency, exactly like
/// `macos-host`'s `live-usb`/`live-inject` gating. The Wave-A host tests in [`decode`]
/// never touch it.
#[cfg(all(target_os = "android", feature = "live-decode"))]
pub mod mediacodec;

#[cfg(target_os = "android")]
mod android {
    use jni::objects::JClass;
    use jni::JNIEnv;

    /// Called once from Kotlin `MainActivity` at startup. P0: just proves the JNI bridge works.
    #[no_mangle]
    pub extern "system" fn Java_com_rustscreen_client_MainActivity_nativeInit(
        _env: JNIEnv,
        _class: JClass,
    ) {
        android_logger::init_once(
            android_logger::Config::default().with_max_level(log::LevelFilter::Info),
        );
        log::info!("hello from Rust");
    }

    /// Called from Kotlin after `UsbManager.openAccessory()` → `ParcelFileDescriptor` →
    /// `detachFd()`. Rust takes sole ownership of the detached accessory fd (T-P1-03 —
    /// single `from_raw_fd`, no double-close) and runs the Wave-A-tested [`echo_loop`]
    /// (echo each length-framed message back, until EOF). This entry is only the fd→Rust seam;
    /// the echo logic lives in the platform-agnostic core (RESEARCH Pattern 3 / D0).
    #[no_mangle]
    pub extern "system" fn Java_com_rustscreen_client_MainActivity_nativeOnUsbFd(
        _env: JNIEnv,
        _class: JClass,
        fd: jni::sys::jint,
    ) {
        use std::os::fd::RawFd;
        // Defensively reject a negative fd before the unsafe wrap below: `File::from_raw_fd`
        // is undefined behavior on an invalid fd (the `FromRawFd` contract requires a valid,
        // open fd) and its `Drop` would `close()` it. This makes the wrap sound regardless of
        // what the JNI caller passes — a future caller, a test, or a platform-specific
        // ParcelFileDescriptor that yields a bad fd. (In the normal flow the Kotlin side has
        // already screened it; detachFd() on a live descriptor returns a valid fd and throws
        // IllegalStateException if already closed — it does not return -1.) (BL-04)
        if fd < 0 {
            log::error!("nativeOnUsbFd: refusing invalid accessory fd {fd} (detachFd failed?)");
            return;
        }
        log::info!("nativeOnUsbFd: received accessory fd {fd}, starting echo loop");
        // BL-03: a Rust `panic!` unwinding across the `extern "system"` FFI boundary is
        // undefined behavior. Catch any panic here (and any echo error), log it, and return
        // cleanly so the unwind never crosses back into the JVM. `AssertUnwindSafe` is sound:
        // on a caught panic we do not observe `transport` again — `catch_unwind` drops it,
        // closing the fd exactly once.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: `fd` was detached on the Kotlin side via ParcelFileDescriptor.detachFd(),
            // transferring sole ownership to this call; we wrap it exactly once. `fd >= 0` was
            // checked above, satisfying the FromRawFd "valid, open fd" contract.
            let mut transport =
                unsafe { crate::transport::AccessoryFdTransport::from_raw_fd(fd as RawFd) };
            // Connect handshake: announce readiness the instant we own the accessory fd by
            // sending a one-frame "hello" (`protocol::HELLO_TAG`, empty payload). The host
            // blocks reading this BEFORE it writes, so its first bulk-OUT write can't land
            // before our reader is live and be dropped by the gadget (startup-ordering deadlock
            // seen on the Pixel 6a). Device→host bulk IN buffers reliably, so sending first is
            // safe even if the host reads a moment later.
            use std::io::Write as _;
            if let Err(e) = protocol::framing::write_frame(&mut transport, protocol::HELLO_TAG, &[])
                .and_then(|()| transport.flush())
            {
                log::error!("nativeOnUsbFd: failed to send connect hello: {e}");
                return Ok(0); // matches echo_loop's Ok(total) shape; nothing echoed
            }
            log::info!("nativeOnUsbFd: sent connect hello, entering echo loop");
            crate::transport::echo_loop(&mut transport)
        }));
        match result {
            Ok(Ok(total)) => log::info!("nativeOnUsbFd: echo loop ended, {total} bytes echoed"),
            Ok(Err(e)) => log::error!("nativeOnUsbFd: echo loop error: {e}"),
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic payload".to_string());
                log::error!("nativeOnUsbFd: caught panic, not unwinding across FFI: {msg}");
            }
        }
    }

    /// Set when the surface is destroyed so the decode loop ends instead of rendering into
    /// a dead window. The decode loop checks it between access units (cooperative stop).
    #[cfg(feature = "live-decode")]
    static SURFACE_GONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Handle to the running decode thread. A surface re-create (rapid surfaceDestroyed ->
    /// surfaceCreated from a rotation or app resume) stops and JOINs the previous decode
    /// loop before starting a new one, so two decoders never render into the same
    /// SurfaceView and no stale loop renders into a destroyed window.
    #[cfg(feature = "live-decode")]
    static DECODE_THREAD: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> =
        std::sync::Mutex::new(None);

    /// P4 Wave B (DEC-01): called from Kotlin `SurfaceHolder.Callback.surfaceCreated` with
    /// the `SurfaceView`'s `Surface`. We turn it into an `ANativeWindow`
    /// (`ANativeWindow_fromSurface`) and hand it to the [`crate::mediacodec`] decode-to-
    /// surface adapter (D3 — frames render onto this surface with no CPU copy).
    ///
    /// For the Wave-B acceptance spike the decode *source* is the P3 `out.h264` pushed to
    /// the device (`adb push out.h264 /data/local/tmp/out.h264`); it self-configures via
    /// in-band SPS/PPS, so no `VideoConfig` frame is needed. (When P5/C2 lands the live RX
    /// loop, `nativeOnUsbFd` feeds the same [`crate::decode::DecodeSession`] from the wire
    /// instead — the adapter and session are unchanged.)
    ///
    /// FFI discipline mirrors `nativeOnUsbFd`: the `ANativeWindow` is acquired here on the
    /// JNI thread (the `JNIEnv`/`Surface` ref are thread-local and must not escape), then
    /// the blocking decode loop runs on a dedicated thread so the UI thread never stalls.
    /// Any panic is caught so it never unwinds across the `extern "system"` boundary.
    #[cfg(feature = "live-decode")]
    #[no_mangle]
    pub extern "system" fn Java_com_rustscreen_client_MainActivity_nativeOnSurface(
        env: JNIEnv,
        _class: JClass,
        surface: jni::sys::jobject,
    ) {
        use crate::decode::DecodeSession;
        use crate::mediacodec::{MediaCodecDecoder, NativeWindow};

        // A surface is (re)created. If a previous decode thread is still running (a rapid
        // surfaceDestroyed -> surfaceCreated from a rotation or app resume), stop and JOIN it
        // before clearing the flag, so we never have two decode loops on the same SurfaceView
        // nor a stale loop rendering into the now-destroyed window.
        SURFACE_GONE.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut guard) = DECODE_THREAD.lock() {
            if let Some(prev) = guard.take() {
                let _ = prev.join();
            }
        }
        // The old loop has exited; allow the new one to run.
        SURFACE_GONE.store(false, std::sync::atomic::Ordering::SeqCst);

        // Acquire the native window on the JNI thread. `surface` is a local ref valid for
        // the duration of this call; `ANativeWindow_fromSurface` takes its own reference,
        // so the resulting `NativeWindow` outlives the local ref and is safe to move to the
        // decode thread.
        // SAFETY: `env` is the live JNI env for this thread; `surface` is the non-null
        // Surface JNI passed us (Kotlin only calls this with a valid created surface).
        let window = match unsafe { NativeWindow::from_surface(env.get_raw(), surface) } {
            Some(w) => w,
            None => {
                log::error!("nativeOnSurface: ANativeWindow_fromSurface returned null");
                return;
            }
        };
        log::info!("nativeOnSurface: acquired ANativeWindow, spawning decode thread");

        // The decode loop blocks (file read + per-frame submit + render), so run it off the
        // UI thread. The window's sole ownership moves into the thread.
        let spawn = std::thread::Builder::new()
            .name("decode-surface".into())
            .spawn(move || {
                // BL-03 discipline: never let a panic unwind across the FFI boundary (this
                // closure is the thread root, but a panic here would also abort cleanly —
                // catch it, log it, return).
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut decoder = MediaCodecDecoder::new(window);
                    let mut session = DecodeSession::new();
                    run_test_h264(&mut session, &mut decoder)
                }));
                match result {
                    Ok(Ok(n)) => {
                        log::info!("nativeOnSurface: decode loop ended, {n} access units rendered")
                    }
                    Ok(Err(e)) => log::error!("nativeOnSurface: decode error: {e}"),
                    Err(panic) => {
                        let msg = panic
                            .downcast_ref::<&str>()
                            .map(|s| s.to_string())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "unknown panic payload".to_string());
                        log::error!(
                            "nativeOnSurface: caught panic, not unwinding across FFI: {msg}"
                        );
                    }
                }
            });
        match spawn {
            Ok(handle) => {
                if let Ok(mut guard) = DECODE_THREAD.lock() {
                    *guard = Some(handle);
                }
            }
            Err(e) => log::error!("nativeOnSurface: failed to spawn decode thread: {e}"),
        }
    }

    /// P4 Wave B: called from `SurfaceHolder.Callback.surfaceDestroyed`. Signals the decode
    /// loop to stop (cooperatively, between access units) so it stops handing frames to a
    /// dead window; the decode thread then drops its `MediaCodecDecoder`, which stops +
    /// deletes the codec and releases the `ANativeWindow` reference.
    #[cfg(feature = "live-decode")]
    #[no_mangle]
    pub extern "system" fn Java_com_rustscreen_client_MainActivity_nativeOnSurfaceDestroyed(
        _env: JNIEnv,
        _class: JClass,
    ) {
        log::info!("nativeOnSurfaceDestroyed: signalling decode loop to stop");
        SURFACE_GONE.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Read the P3 acceptance clip `out.h264`, split it into per-picture access units (via
    /// [`protocol::nal::split_access_units`]), and drive the [`crate::decode::DecodeSession`]
    /// (which configures the decoder from the in-band SPS/PPS and submits each AU for
    /// decode-to-surface).
    ///
    /// This is the Wave-B spike feeder; the live wire feeder (C2) replaces it. We split the
    /// elementary stream at each picture boundary so each `Frame::Video` is one access unit
    /// the session can timestamp and submit.
    #[cfg(feature = "live-decode")]
    fn run_test_h264(
        session: &mut crate::decode::DecodeSession,
        decoder: &mut dyn crate::decode::VideoDecoder,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        use protocol::messages::Frame;
        use protocol::nal;

        // adb push out.h264 /data/local/tmp/out.h264
        const TEST_CLIP: &str = "/data/local/tmp/out.h264";
        let bytes = std::fs::read(TEST_CLIP)
            .map_err(|e| format!("reading {TEST_CLIP}: {e} (adb push out.h264 there first)"))?;
        log::info!(
            "nativeOnSurface: read {} bytes of H.264 from {TEST_CLIP}",
            bytes.len()
        );

        // ~60 fps cadence for the spike (presentation timestamps in microseconds). The
        // surface renders as fast as releaseOutputBuffer(render=true) presents; the pts is
        // carried through to the codec for ordering.
        const FRAME_INTERVAL_US: u64 = 16_666;

        let mut count: u64 = 0;
        let mut pts_us: u64 = 0;
        for au in nal::split_access_units(&bytes) {
            // Cooperative stop: bail if the surface was destroyed mid-clip.
            if SURFACE_GONE.load(std::sync::atomic::Ordering::SeqCst) {
                log::info!("nativeOnSurface: surface gone, stopping decode loop");
                break;
            }
            let keyframe = nal::is_keyframe(au);
            let frame = Frame::Video {
                pts_us,
                keyframe,
                nal: au.to_vec(),
            };
            session.feed(&frame, decoder)?;
            count += 1;
            pts_us += FRAME_INTERVAL_US;
        }
        Ok(count)
    }
}
