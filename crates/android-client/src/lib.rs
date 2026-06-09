//! RustScreen Android client (cdylib). The render surface + activity lifecycle run here as a
//! NativeActivity event loop (`android_main`, android-activity); the remaining thin Kotlin shell
//! (a `NativeActivity` subclass) does only the USB-accessory permission flow — which has no NDK
//! equivalent — and hands the accessory fd to `nativeOnUsbFd`. All pipeline logic is Rust.

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
    // jni 0.22 split the old `JNIEnv` alias into `Env` (full API, not FFI-safe) and
    // `EnvUnowned` (the FFI-safe type for capturing the raw `*JNIEnv` in native methods).
    // These two entry points never touch the env (it's `_env`), so we take the FFI-safe
    // `EnvUnowned` exactly as jni 0.22 prescribes for `extern "system"` native methods.
    use jni::EnvUnowned;

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
        _env: EnvUnowned,
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
    /// # Threading Requirement (Critical)
    ///
    /// **This function BLOCKS for the entire session duration** (potentially hours). The caller
    /// **MUST** invoke it from a dedicated background thread — NEVER from the Android UI/main
    /// thread. Calling this on the main thread will cause an ANR (Application Not Responding)
    /// and the system will kill the process.
    ///
    /// The Kotlin side (`MainActivity`) is responsible for spawning a dedicated thread (e.g. via
    /// `Thread` or `ExecutorService`) before calling this JNI method, and for releasing its
    /// "session active" latch only when this function returns. This design keeps the Rust session
    /// logic synchronous and simple while pushing the threading policy to the Kotlin caller,
    /// which has access to Android threading primitives.
    ///
    /// If you need async/non-blocking behavior, the architecture would need to change:
    /// the session would need to run on a spawned Rust thread and this JNI entry would return
    /// immediately with a handle/callback for teardown. That is a future API change.
    #[no_mangle]
    pub extern "system" fn Java_com_rustscreen_client_MainActivity_nativeOnUsbFd(
        _env: EnvUnowned,
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

        // Re-seed the consume-once slot from the retained live window so a reconnect (a new
        // session with the surface already created and the slot long since drained) binds
        // immediately — mirrors the old Kotlin shell's per-session re-deposit. A cold
        // launch-by-plug (fd before the first InitWindow) finds CURRENT_WINDOW empty here and
        // instead waits below for android_main to put() the window once the OS creates it.
        //
        // Hold CURRENT_WINDOW across the put so a concurrent TerminateWindow/Destroy on the
        // android_main thread (retract_window, which clears the slot under THIS SAME lock) cannot
        // wedge between our read of CURRENT_WINDOW and the put. Without that, a retract could clear
        // the slot and null CURRENT_WINDOW after we cloned but before we put, re-depositing a
        // just-destroyed window into the freshly-cleared slot for take_blocking to hand the decoder
        // — configuring MediaCodec onto an abandoned BufferQueue. CURRENT_WINDOW is always the
        // outer lock (the slot/mailbox code never reaches back for it), so this nesting cannot
        // deadlock; the FFI release of any window the put replaces is a bare refcount decrement
        // that takes no Rust lock, so running it under the guard is safe. The lock is released
        // before the blocking take below; a poisoned guard is recovered rather than panicked on
        // (the android_main thread shares this lock).
        {
            let cur = CURRENT_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(w) = cur.as_ref() {
                SURFACE_SLOT.put(w.clone_acquire());
            }
        }
        // Rendezvous: the window arrives from android_main's InitWindow, possibly after the fd.
        let window = SURFACE_SLOT
            .take_blocking(Duration::from_secs(10))
            .ok_or("no render surface within 10s (NativeActivity window never created?)")?;
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

    /// The render surface, deposited by `android_main`'s `InitWindow` and consumed by the USB
    /// decode session thread ([`run_usb_fd`]). A one-way handoff so the session starts regardless
    /// of whether the surface or the USB fd arrived first (when the app is launched by plugging
    /// in, the fd can arrive before the NativeActivity window is created).
    #[cfg(feature = "live-decode")]
    static SURFACE_SLOT: crate::rendezvous::WindowSlot<crate::mediacodec::NativeWindow> =
        crate::rendezvous::WindowSlot::new();

    /// The latest live render window, retained by [`android_main`] across its lifetime (set on
    /// `InitWindow`, cleared on `TerminateWindow`). `SURFACE_SLOT` is consume-once, so a *new*
    /// session that starts while the window already exists (a USB reconnect — replug/EOF — with
    /// the process and surface still alive) would find the slot drained and time out. `run_usb_fd`
    /// re-seeds the slot from this before blocking, exactly mirroring the old Kotlin shell's
    /// per-session `currentSurface` re-deposit. A cold launch-by-plug (fd before the first
    /// `InitWindow`) finds this empty and instead waits for `InitWindow` to `put()`.
    #[cfg(feature = "live-decode")]
    static CURRENT_WINDOW: std::sync::Mutex<Option<crate::mediacodec::NativeWindow>> =
        std::sync::Mutex::new(None);

    /// NativeActivity entry point (android-activity, native-activity backend). Replaces the
    /// Kotlin `SurfaceView` + the `nativeOnSurface`/`nativeOnSurfaceDestroyed` JNI calls: the OS
    /// surface lifecycle now arrives here as `InitWindow`/`TerminateWindow` events, and we feed
    /// the SAME two channels the decode session already consumes — so every downstream behavior
    /// (cold-launch rendezvous via `SURFACE_SLOT`, and the live background→foreground swap via
    /// `SURFACE_MAILBOX`, PR #36) is preserved unchanged. The USB fd handoff stays in Kotlin
    /// (`nativeOnUsbFd`) because `UsbManager` has no NDK equivalent.
    ///
    /// Runs on android-activity's dedicated main thread for the activity's lifetime; the blocking
    /// decode runs on the separate USB thread the Kotlin shell spawns.
    #[cfg(feature = "live-decode")]
    #[no_mangle]
    fn android_main(app: android_activity::AndroidApp) {
        use std::time::Duration;

        // Idempotent: the logger is also init'd by nativeInit from Kotlin onCreate; init here too
        // so events logged from this thread are captured regardless of thread start ordering.
        android_logger::init_once(
            android_logger::Config::default().with_max_level(log::LevelFilter::Info),
        );
        log::info!("android_main: NativeActivity event loop started");

        let mut running = true;
        while running {
            app.poll_events(Some(Duration::from_millis(250)), |event| {
                // BL-03 discipline: `android_main` is an FFI entry (android-activity's C glue calls
                // it), so a panic must NEVER unwind across that boundary — catch it here exactly
                // like `nativeOnUsbFd`. A caught panic is logged and the loop continues. The window
                // statics are shared with the panic-capable decode thread, so each lock below also
                // recovers a poisoned guard via `into_inner` rather than `unwrap`-panicking, so one
                // thread's panic can't brick the window channel for the rest of the process.
                let exit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle_main_event(&app, event)
                }));
                match exit {
                    Ok(should_exit) => running = !should_exit && running,
                    Err(_) => {
                        log::error!("android_main: caught panic in event handler, not unwinding");
                    }
                }
            });
        }
    }

    /// Handle one android-activity event, returning `true` iff the loop should exit (`Destroy`).
    /// Split out of [`android_main`] so the whole body runs under one `catch_unwind` (the FFI
    /// boundary) without nesting the match. Feeds the same surface channels the old surface JNI did.
    #[cfg(feature = "live-decode")]
    fn handle_main_event(
        app: &android_activity::AndroidApp,
        event: android_activity::PollEvent<'_>,
    ) -> bool {
        use crate::mediacodec::{NativeWindow, SURFACE_MAILBOX};
        use android_activity::{MainEvent, PollEvent};

        let PollEvent::Main(main) = event else {
            return false;
        };
        match main {
            // Surface available (app start, or foreground after a real destroy).
            MainEvent::InitWindow { .. } => {
                let Some(ndk_win) = app.native_window() else {
                    log::error!("android_main: InitWindow but native_window() is None");
                    return false;
                };
                // SAFETY: the pointer is a live ANativeWindow for this InitWindow..TerminateWindow
                // span; from_ptr_acquire adds our own ref so the handle stays valid once retained.
                let Some(window) =
                    (unsafe { NativeWindow::from_ptr_acquire(ndk_win.ptr().as_ptr()) })
                else {
                    log::error!("android_main: native window pointer was null");
                    return false;
                };
                log::info!("android_main: InitWindow — depositing render window");
                // Mirror the old nativeOnSurface: feed BOTH the live swap mailbox (a running
                // session re-points onto it) and the consume-once slot (a fresh session's initial
                // bind), and retain it for the reconnect re-seed. The slot put is held under
                // CURRENT_WINDOW so the retained window and the consume-once slot stay consistent
                // against a concurrent run_usb_fd re-seed (which puts under the same lock); this
                // arm runs on the android_main thread, the same thread as retract_window, so the
                // two of them never race each other. The mailbox is an independent channel, fed
                // outside the lock.
                SURFACE_MAILBOX.deposit(window.clone_acquire());
                {
                    let mut cur = CURRENT_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
                    *cur = Some(window.clone_acquire());
                    SURFACE_SLOT.put(window);
                }
                false
            }
            // Surface destroyed (app backgrounded). Mirror nativeOnSurfaceDestroyed.
            MainEvent::TerminateWindow { .. } => {
                log::info!("android_main: TerminateWindow — retracting render window");
                retract_window();
                false
            }
            // Input (touch) is available. NativeActivity routes input to native code, and the OS
            // input dispatcher ANRs the app ("Waited 5001ms for MotionEvent") if buffered events
            // are not consumed — and android-activity delivers only ONE `InputAvailable` between
            // drains, so ignoring it strands ALL later input, not just one event. This app is a
            // read-only monitor (touch is not wired), so we just drain the queue, marking every
            // event `Unhandled` (which still finishes it so the queue can't back up; the system
            // applies its normal fallback, e.g. back/volume keys keep working).
            MainEvent::InputAvailable => {
                drain_input(app);
                false
            }
            // Activity going away: retract the window (in case Destroy arrives without a preceding
            // TerminateWindow, so no window is left leaked in the slot / stale in the mailbox) and
            // exit the loop.
            MainEvent::Destroy => {
                log::info!("android_main: Destroy — exiting event loop");
                retract_window();
                true
            }
            _ => false,
        }
    }

    /// Drain all buffered input events, consuming each as `Unhandled` (touch is not wired). This
    /// MUST run on the `android_main` thread (it does — `handle_main_event` is called from the
    /// `poll_events` callback). Not draining stalls the OS input dispatcher → ANR; see the
    /// `MainEvent::InputAvailable` arm.
    #[cfg(feature = "live-decode")]
    fn drain_input(app: &android_activity::AndroidApp) {
        use android_activity::InputStatus;
        match app.input_events_iter() {
            // `next` returns false once the buffer is empty; loop until then so every event is
            // finished in this single response to `InputAvailable`.
            Ok(mut iter) => while iter.next(|_event| InputStatus::Unhandled) {},
            Err(err) => log::error!("android_main: input_events_iter failed: {err:?}"),
        }
    }

    /// Retract the current render window from all three holders — the consume-once slot, the live
    /// mailbox, and the retained `CURRENT_WINDOW` — so no stale/dead surface is handed to a session
    /// and no `ANativeWindow` reference is leaked. Shared by `TerminateWindow` and `Destroy`.
    #[cfg(feature = "live-decode")]
    fn retract_window() {
        // Clear the slot under CURRENT_WINDOW so it is mutually exclusive with run_usb_fd's
        // re-seed put (which holds the same lock): otherwise a clear here could land between that
        // re-seed's read of CURRENT_WINDOW and its put, leaving a dead window in the slot. Holding
        // the guard across the clear + null makes a racing re-seed see either the live window
        // (before) or None (after), never a torn state. The mailbox uses its own lock, so the
        // mark_lost ordering relative to the slot does not matter.
        let mut cur = CURRENT_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        SURFACE_SLOT.clear();
        crate::mediacodec::SURFACE_MAILBOX.mark_lost();
        *cur = None;
    }
}
