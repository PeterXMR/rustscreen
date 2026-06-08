//! RustScreen Android client (cdylib). JNI entry points called by the thin Kotlin shell (D7).
//! All app logic lives here in Rust; the Kotlin shell is glue only and will be replaced by
//! NativeActivity in a later phase.

use std::sync::OnceLock;
use std::time::Instant;

static CLOCK_START: OnceLock<Instant> = OnceLock::new();

/// Process-global monotonic phone clock in microseconds. All latency timestamps
/// (arrive/decode/present + clock-sync t1/t2) MUST use this single epoch so they are
/// comparable and consistent with the host's clock-sync offset.
pub fn now_us() -> u64 {
    CLOCK_START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// Transport core: the platform-agnostic `echo_loop` (host-testable) plus the
/// android-only `AccessoryFdTransport`. `echo_loop` is NOT cfg-gated so CI exercises it.
pub mod transport;

/// Touch input core (P6): pure conversion of a raw Android `MotionEvent` sample into a
/// normalized `protocol::messages::TouchEvent` — the inverse of the host's coordinate
/// mapping. NOT cfg-gated, so CI exercises it (the JNI/Kotlin capture shim is glue).
pub mod touch;

/// Touch move-event coalescing core (pure): batch high-frequency MOVE events down to the
/// send tick while DOWN/UP pass through immediately and in order. NOT cfg-gated, so CI
/// exercises it (the JNI/Kotlin/transport wiring is the device-side glue, deferred).
pub mod coalesce;

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

/// Input-side frame pacing (latency item A): a pure state machine that admits a frame only
/// while the decoder's in-flight depth is under a small cap, else drops forward to the next
/// keyframe. Platform-agnostic and CI-tested; the receive loop in `session` drives it.
pub mod pacing;

/// Surface↔USB rendezvous (P5 live pipeline): the one-way `WindowSlot` handoff that lets the
/// USB decode-session thread wait for the render surface, which arrives on a separate Android
/// callback in either order. Generic + NOT cfg-gated, so CI host-tests the handoff logic.
pub mod rendezvous;

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

    /// Run a JNI entry-point body under `catch_unwind` so a Rust `panic!` can never unwind across
    /// the `extern "system"` FFI boundary (BL-03) — that is undefined behavior and aborts the host
    /// Android process. A caught panic is logged and swallowed; the JNI call returns normally.
    /// `AssertUnwindSafe` is sound here: on a caught panic we do not observe the closure's captured
    /// state again. (`nativeOnUsbFd` inlines its own variant because it threads a return value.)
    fn jni_guard(name: &str, body: impl FnOnce()) {
        if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic payload".to_string());
            log::error!("{name}: caught panic, not unwinding across FFI: {msg}");
        }
    }

    /// Called once from Kotlin `MainActivity` at startup. P0: just proves the JNI bridge works.
    #[no_mangle]
    pub extern "system" fn Java_com_rustscreen_client_MainActivity_nativeInit(
        _env: JNIEnv,
        _class: JClass,
    ) {
        jni_guard("nativeInit", || {
            android_logger::init_once(
                android_logger::Config::default().with_max_level(log::LevelFilter::Info),
            );
            log::info!("hello from Rust");
        });
    }

    /// Called from Kotlin after `UsbManager.openAccessory()` → `ParcelFileDescriptor` →
    /// `detachFd()`. Rust takes sole ownership of the detached accessory fd (T-P1-03 —
    /// single `from_raw_fd`, no double-close). With `--features live-decode` (the shipped
    /// app) this runs the **live decode session**: send the connect-hello, rendezvous with
    /// the render surface, then drive [`crate::session::run_session`], which decodes each
    /// received `Frame::Video` onto that surface until the host disconnects. Without
    /// `live-decode` it falls back to the P1 [`crate::transport::echo_loop`] so a bare build
    /// still links. This entry is only the fd→Rust seam; the session/echo logic lives in the
    /// platform-agnostic core.
    ///
    /// The call BLOCKS for the whole session (Kotlin runs it on a dedicated thread and
    /// releases its "session active" latch when this returns), so the session must run inline
    /// here rather than on a spawned thread.
    #[no_mangle]
    pub extern "system" fn Java_com_rustscreen_client_MainActivity_nativeOnUsbFd(
        _env: JNIEnv,
        _class: JClass,
        fd: jni::sys::jint,
    ) -> jni::sys::jboolean {
        use std::os::fd::RawFd;
        // Defensively reject a negative fd before the unsafe wrap below: `File::from_raw_fd`
        // is undefined behavior on an invalid fd (the `FromRawFd` contract requires a valid,
        // open fd) and its `Drop` would `close()` it. (In the normal flow the Kotlin side has
        // already screened it; detachFd() on a live descriptor returns a valid fd.) (BL-04)
        if fd < 0 {
            log::error!("nativeOnUsbFd: refusing invalid accessory fd {fd} (detachFd failed?)");
            return jni::sys::JNI_FALSE;
        }
        // BL-03: never let a Rust `panic!` unwind across the `extern "system"` FFI boundary.
        // Catch it (and any session/echo error), log it, and return cleanly. `AssertUnwindSafe`
        // is sound: on a caught panic we do not observe the transport again — `catch_unwind`
        // drops it, closing the fd exactly once.
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_usb_fd(fd as RawFd)));
        // Returns `true` to Kotlin ONLY on a clean host stop (`Control::Bye` → `rustscreen stop`),
        // so the app closes itself. EOF/disconnect (replug), session errors, and panics return
        // `false` → the app stays alive and keeps polling to reconnect.
        match result {
            Ok(Ok(ended_by_host_bye)) => {
                log::info!("nativeOnUsbFd: session ended cleanly (host_stop={ended_by_host_bye})");
                if ended_by_host_bye {
                    jni::sys::JNI_TRUE
                } else {
                    jni::sys::JNI_FALSE
                }
            }
            Ok(Err(e)) => {
                log::error!("nativeOnUsbFd: session error: {e}");
                jni::sys::JNI_FALSE
            }
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic payload".to_string());
                log::error!("nativeOnUsbFd: caught panic, not unwinding across FFI: {msg}");
                jni::sys::JNI_FALSE
            }
        }
    }

    /// Wrap the detached accessory `fd` and send the connect-hello — shared by both
    /// [`run_usb_fd`] variants so the handshake-ordering invariant lives in exactly one place.
    ///
    /// The host blocks reading this one-frame hello (`protocol::HELLO_TAG`, empty payload)
    /// BEFORE it writes its handshake offer, so its first bulk-OUT write can't be dropped by the
    /// gadget before our reader is live (the startup-ordering deadlock seen on the Pixel 6a).
    /// Sending first is safe because device→host bulk-IN buffers reliably.
    fn open_with_hello(
        fd: std::os::fd::RawFd,
    ) -> Result<crate::transport::AccessoryFdTransport, Box<dyn std::error::Error>> {
        use std::io::Write as _;
        // SAFETY: `fd` was detached on the Kotlin side (sole ownership) and is >= 0 (checked in
        // nativeOnUsbFd before this is reached); wrapped exactly once.
        let mut transport = unsafe { crate::transport::AccessoryFdTransport::from_raw_fd(fd) };
        protocol::framing::write_frame(&mut transport, protocol::HELLO_TAG, &[])?;
        transport.flush()?;
        Ok(transport)
    }

    /// Live decode session (shipped app): connect-hello → rendezvous for the render surface →
    /// [`crate::session::run_session`] decoding to that surface until the host disconnects
    /// (EOF) or errors.
    #[cfg(feature = "live-decode")]
    fn run_usb_fd(fd: std::os::fd::RawFd) -> Result<bool, Box<dyn std::error::Error>> {
        use crate::decode::DecodeSession;
        use crate::mediacodec::MediaCodecDecoder;
        use crate::session::run_session;
        use protocol::messages::{ClientCaps, VideoCodec};
        use std::time::Duration;

        log::info!("nativeOnUsbFd: received accessory fd {fd}, starting live decode session");
        let mut transport = open_with_hello(fd)?;
        log::info!("nativeOnUsbFd: sent connect hello; waiting for the render surface…");

        // Rendezvous: the surface arrives on a separate callback, possibly after the fd.
        let window = SURFACE_SLOT
            .take_blocking(Duration::from_secs(10))
            .ok_or("no render surface within 10s (SurfaceView never created?)")?;
        log::info!("nativeOnUsbFd: surface acquired; decoding to surface");

        let mut decoder = MediaCodecDecoder::new(window);
        let mut session = DecodeSession::new();
        // Advertise the device's H.264 decode ceiling rather than a single hardcoded mode.
        // negotiate() only requires the client max to be >= the host's offered resolution, and
        // the decoder sizes itself from the stream's in-band SPS regardless of what we advertise
        // here — so a generous 4K ceiling (well within the Pixel 6a's hardware AVC decoder)
        // accepts any realistic host offer instead of failing negotiation on anything but
        // 2400×1080. Refresh stays at the panel's real 60 Hz. (Per-surface negotiation — reading
        // the actual ANativeWindow dimensions — is deferred to the hotplug ladder item.)
        let caps = ClientCaps {
            protocol_version: protocol::protocol_version(),
            max_width: 3840,
            max_height: 2160,
            max_refresh_hz: 60,
            codecs: vec![VideoCodec::H264],
        };
        let summary = run_session(&mut transport, &caps, &mut session, &mut decoder)?;
        log::info!(
            "nativeOnUsbFd: {} frames received, {} decoded, {} keyframes, {} input-dropped",
            summary.frames_received,
            summary.decoded_count,
            summary.keyframe_count,
            summary.input_frames_dropped
        );
        // `true` only when the host sent Control::Bye (`rustscreen stop`) → Kotlin closes the app.
        Ok(summary.ended_by_host_bye)
    }

    /// P1 fallback (no `live-decode`): echo each framed message back until EOF, so a bare
    /// `cargo build -p android-client` (without the decode stack) still links this JNI entry.
    #[cfg(not(feature = "live-decode"))]
    fn run_usb_fd(fd: std::os::fd::RawFd) -> Result<bool, Box<dyn std::error::Error>> {
        log::info!(
            "nativeOnUsbFd: received accessory fd {fd}, starting echo loop (no live-decode)"
        );
        let mut transport = open_with_hello(fd)?;
        log::info!("nativeOnUsbFd: sent connect hello, entering echo loop");
        let bytes = crate::transport::echo_loop(&mut transport)?;
        log::info!("nativeOnUsbFd: echo loop ended ({bytes} bytes)");
        // The echo fallback has no Bye-vs-disconnect distinction (it ends only on EOF), so never
        // signal a host stop — the live-decode build is the one that closes the app.
        Ok(false)
    }

    /// The render surface, deposited by `nativeOnSurface` and consumed by the USB decode
    /// session thread ([`run_usb_fd`]). A one-way handoff so the session starts regardless of
    /// whether the surface or the USB fd arrived first (when the app is launched by plugging
    /// in, the fd can arrive before the `SurfaceView`'s surface is created).
    #[cfg(feature = "live-decode")]
    static SURFACE_SLOT: crate::rendezvous::WindowSlot<crate::mediacodec::NativeWindow> =
        crate::rendezvous::WindowSlot::new();

    /// Called from Kotlin `SurfaceHolder.Callback.surfaceCreated` with the `SurfaceView`'s
    /// `Surface`. Turns it into an `ANativeWindow` (`ANativeWindow_fromSurface`) and deposits
    /// it in [`SURFACE_SLOT`] for the USB decode session to claim. Returns immediately so the
    /// UI thread never blocks — the blocking decode runs on the USB thread in [`run_usb_fd`].
    ///
    /// FFI discipline: the `ANativeWindow` is acquired here on the JNI thread (the
    /// `JNIEnv`/`Surface` ref are thread-local and must not escape); `NativeWindow` then owns
    /// its own `ANativeWindow` reference and is `Send`, so the slot can hand it to the session
    /// thread.
    #[cfg(feature = "live-decode")]
    #[no_mangle]
    pub extern "system" fn Java_com_rustscreen_client_MainActivity_nativeOnSurface(
        env: JNIEnv,
        _class: JClass,
        surface: jni::sys::jobject,
    ) {
        use crate::mediacodec::NativeWindow;

        jni_guard("nativeOnSurface", || {
            // SAFETY: `env` is the live JNI env for this thread; `surface` is the non-null Surface
            // JNI passed us (Kotlin only calls this with a valid created surface).
            let window = match unsafe { NativeWindow::from_surface(env.get_raw(), surface) } {
                Some(w) => w,
                None => {
                    log::error!("nativeOnSurface: ANativeWindow_fromSurface returned null");
                    return;
                }
            };
            log::info!("nativeOnSurface: surface ready — depositing for the USB decode session");
            SURFACE_SLOT.put(window);
        });
    }

    /// Called from `SurfaceHolder.Callback.surfaceDestroyed`. Retracts any window still sitting
    /// in [`SURFACE_SLOT`] so a session that has not yet claimed it cannot configure the decoder
    /// onto a now-dead surface (the destroy-before-take race on the launch-by-plug path), and so
    /// a surface deposited but never consumed does not leak its `ANativeWindow` reference.
    ///
    /// For the MVP single-connect pipeline a surface lost *after* a session already claimed it
    /// still ends only when the host disconnects (USB EOF) — robust mid-session surface-recreate
    /// is deferred to the hotplug ladder item.
    #[cfg(feature = "live-decode")]
    #[no_mangle]
    pub extern "system" fn Java_com_rustscreen_client_MainActivity_nativeOnSurfaceDestroyed(
        _env: JNIEnv,
        _class: JClass,
    ) {
        jni_guard("nativeOnSurfaceDestroyed", || {
            log::info!("nativeOnSurfaceDestroyed: surface gone — retracting any unclaimed window");
            SURFACE_SLOT.clear();
        });
    }
}
