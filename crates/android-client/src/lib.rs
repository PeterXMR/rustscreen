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
            // transferring sole ownership to this call; we wrap it exactly once.
            let mut transport =
                unsafe { crate::transport::AccessoryFdTransport::from_raw_fd(fd as RawFd) };
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
}
