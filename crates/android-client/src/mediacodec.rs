//! P4 Wave B (DEC-01): the concrete `AMediaCodec` decode-to-surface adapter that
//! satisfies the platform-agnostic [`VideoDecoder`](crate::decode::VideoDecoder) port.
//!
//! This is the *simplest-now* Pixel decode seam: a thin `ndk-sys` wrapper over
//! `libmediandk` that drives a hardware H.264 decoder straight onto an `ANativeWindow`
//! (decode-to-surface, D3 — no CPU copy of the decoded picture). It is
//! `#[cfg(target_os = "android")]` and gated behind the `live-decode` Cargo feature, so
//! the default `cargo build --workspace` pulls no `ndk`/`ndk-sys` dependency and the
//! Wave-A host tests in [`crate::decode`] stay device-free.
//!
//! ## Lifecycle
//! 1. The Kotlin `SurfaceView`'s `SurfaceHolder.Callback` hands a `Surface` to
//!    `nativeOnSurface` (see [`crate::lib`]), which turns it into an `ANativeWindow` via
//!    `ANativeWindow_fromSurface` and constructs a [`MediaCodecDecoder`] around it.
//! 2. `DecodeSession` calls [`VideoDecoder::configure`] with the codec + SPS/PPS once the
//!    first `VideoConfig` (or in-band keyframe) is seen — we build an `AMediaFormat`
//!    (`mime = video/avc`, `csd-0` = SPS+PPS Annex-B), `AMediaCodec_configure` it onto the
//!    window, and `AMediaCodec_start`.
//! 3. Each access unit is submitted via [`VideoDecoder::decode`]:
//!    `dequeueInputBuffer` → copy the Annex-B bytes in → `queueInputBuffer`, then drain any
//!    ready output with `dequeueOutputBuffer` + `releaseOutputBuffer(render = true)` so the
//!    frame is composited onto the surface.
//!
//! ## Why hand-rolled `ndk-sys` (R4)
//! The `rust_mediacodec` crate is stale (v0.1.2, 2022) and the `ndk` crate's `media`
//! module is gated/partial across versions, so we own the FFI directly against the
//! bindgen symbols in `ndk-sys` — exactly the same discipline as the host's encoder.
//!
//! ## Safety / FFI discipline
//! All `unsafe` calls are local and checked: pointers are null-guarded, `media_status_t`
//! is mapped to [`DecodeError::Adapter`] rather than panicking, and the codec / format /
//! window handles are freed in `Drop`. This module never panics across the JNI boundary —
//! `nativeOnSurface` wraps it in a `catch_unwind` (mirroring `nativeOnUsbFd`).

use std::ffi::CStr;
use std::ptr::NonNull;

use ndk_sys as sys;
use protocol::messages::VideoCodec;
use protocol::nal::CodecConfig;

use crate::decode::{DecodeError, DecoderInput, VideoDecoder};

// Link the NDK media library. `ndk-sys` only declares the FFI signatures and links
// `libandroid` (for `ANativeWindow_*`), NOT `libmediandk` — so the `AMediaCodec_*` /
// `AMediaFormat_*` / `AMEDIAFORMAT_KEY_*` symbols this module uses are left undefined.
// A cdylib build tolerates that, but `dlopen` of the `.so` on-device fails to resolve
// `AMEDIAFORMAT_KEY_MIME` (a data symbol, eagerly bound) → `UnsatisfiedLinkError` at
// `System.loadLibrary`. This empty `#[link]` block forces a DT_NEEDED on `libmediandk`
// without redeclaring any symbol. It is only compiled with this module (android +
// `live-decode`), so the host build is unaffected.
#[link(name = "mediandk")]
extern "C" {}

/// H.264 MIME type for `AMediaCodec_createDecoderByType` / `AMEDIAFORMAT_KEY_MIME`.
const MIME_AVC: &CStr = c"video/avc";

/// `AMediaCodec_dequeueOutputBuffer` returns these negative sentinels (in addition to a
/// real buffer index `>= 0`) instead of a separate status enum.
const INFO_TRY_AGAIN_LATER: isize = -1;
const INFO_OUTPUT_FORMAT_CHANGED: isize = -2;
const INFO_OUTPUT_BUFFERS_CHANGED: isize = -3;

/// How long to block waiting for an input buffer / output buffer, in microseconds.
/// Small but non-zero: the decoder usually has an input slot immediately; a short wait
/// avoids a busy spin without adding meaningful latency to the live path.
const DEQUEUE_TIMEOUT_US: i64 = 10_000;

/// Max `dequeueInputBuffer` retries before declaring the decoder stalled. Each retry waits
/// up to `DEQUEUE_TIMEOUT_US` and drains output in between, so this is a generous (~1s)
/// budget — input slots normally free up within a frame or two of draining output.
const MAX_INPUT_DEQUEUE_ATTEMPTS: u32 = 100;

/// Owns the native window handed in from the Kotlin `Surface` (decode-to-surface target).
///
/// Released with `ANativeWindow_release` on `Drop`. Constructed on the JNI thread in
/// `nativeOnSurface`; moved into the [`MediaCodecDecoder`] on `configure`.
pub struct NativeWindow {
    ptr: NonNull<sys::ANativeWindow>,
}

impl NativeWindow {
    /// Acquire an `ANativeWindow` from a Java `Surface`.
    ///
    /// # Safety
    /// `env` must be a valid JNI environment pointer for the current thread and `surface`
    /// a valid local/global reference to a non-recycled `android.view.Surface` (both true
    /// for the args JNI passes to `nativeOnSurface`). `ANativeWindow_fromSurface` adds a
    /// reference we own and release in `Drop`.
    pub unsafe fn from_surface(
        env: *mut jni::sys::JNIEnv,
        surface: jni::sys::jobject,
    ) -> Option<Self> {
        // ndk-sys and the jni crate both ultimately use `jni_sys`, so these pointer types
        // are layout-identical; cast to the exact types `ANativeWindow_fromSurface` wants.
        let raw = sys::ANativeWindow_fromSurface(env.cast(), surface.cast());
        NonNull::new(raw).map(|ptr| NativeWindow { ptr })
    }

    fn as_ptr(&self) -> *mut sys::ANativeWindow {
        self.ptr.as_ptr()
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `ANativeWindow_fromSurface` (which added our reference)
        // and is released exactly once here.
        unsafe { sys::ANativeWindow_release(self.ptr.as_ptr()) }
    }
}

// `ANativeWindow` is internally reference-counted and the NDK permits using it across
// threads; the decode loop runs on a dedicated thread distinct from the JNI thread that
// created it, so we move ownership across that boundary.
unsafe impl Send for NativeWindow {}

/// The `AMediaCodec` decode-to-surface adapter (DEC-01).
///
/// Holds the native window until `configure` consumes it into the codec, then owns the
/// running codec for the rest of the session. All handles are freed in `Drop`.
pub struct MediaCodecDecoder {
    /// The decode-to-surface target, taken by `configure`. `Some` until the first
    /// `configure`; `None` afterwards (the window is bound into the codec).
    window: Option<NativeWindow>,
    /// The running decoder, created lazily on the first `configure`.
    codec: Option<Codec>,
}

impl MediaCodecDecoder {
    /// Build an adapter that will decode onto `window`. The codec itself is created on the
    /// first [`VideoDecoder::configure`] call, when SPS/PPS are known.
    pub fn new(window: NativeWindow) -> Self {
        MediaCodecDecoder {
            window: Some(window),
            codec: None,
        }
    }
}

/// RAII wrapper over a started `AMediaCodec` (stopped + deleted on `Drop`).
struct Codec {
    ptr: NonNull<sys::AMediaCodec>,
    /// Kept alive for the codec's lifetime: the codec renders into this window.
    _window: NativeWindow,
}

impl Drop for Codec {
    fn drop(&mut self) {
        // SAFETY: `ptr` is a valid, started codec; stop then delete, exactly once.
        unsafe {
            sys::AMediaCodec_stop(self.ptr.as_ptr());
            sys::AMediaCodec_delete(self.ptr.as_ptr());
        }
    }
}

/// Map a non-OK `media_status_t` to a debuggable [`DecodeError::Adapter`].
fn check(status: sys::media_status_t, context: &str) -> Result<(), DecodeError> {
    if status == sys::media_status_t::AMEDIA_OK {
        Ok(())
    } else {
        Err(DecodeError::Adapter(format!(
            "{context} failed: media_status_t({})",
            status.0
        )))
    }
}

impl VideoDecoder for MediaCodecDecoder {
    fn configure(&mut self, codec: VideoCodec, config: &CodecConfig) -> Result<(), DecodeError> {
        // D2: H.264 is the only MVP codec. HEVC is reserved in the protocol but this
        // adapter only wires `video/avc` (track H adds the HEVC arm later).
        if codec != VideoCodec::H264 {
            return Err(DecodeError::Adapter(format!(
                "unsupported codec {codec:?}; this adapter only decodes H.264"
            )));
        }

        // Reconfigure: drop any prior codec first so the window is free to bind again.
        // The window lives inside the old `Codec`; on a mid-stream reconfig we recover it
        // by recreating from scratch is not possible (the Surface ref is gone), so a
        // reconfigure replaces only the codec and reuses the same window handle. To keep
        // ownership simple we forbid reconfigure for now and surface it as an adapter
        // error — the live MVP sends one config; resolution changes are a later concern.
        if self.codec.is_some() {
            return Err(DecodeError::Adapter(
                "mid-stream reconfigure is not supported by the Wave-B adapter".into(),
            ));
        }

        let window = self
            .window
            .take()
            .ok_or_else(|| DecodeError::Adapter("configure called with no surface bound".into()))?;

        // The Pixel's C2 decoder rejects `configure` if the format has no explicit
        // width/height — it falls back to a 320x240 default and returns BAD_VALUE rather
        // than deriving the size from csd-0. Read the coded picture size from the SPS and
        // set it on the format below.
        let (width, height) = protocol::nal::sps_dimensions(&config.sps)
            .ok_or_else(|| DecodeError::Adapter("could not parse width/height from SPS".into()))?;

        // Build `csd-0`: the decoder wants the SPS and PPS as Annex-B NAL units
        // (start-code prefixed), concatenated. `CodecConfig` stores them without start
        // codes, so prepend a 4-byte start code to each.
        let mut csd0 = Vec::with_capacity(config.sps.len() + config.pps.len() + 8);
        csd0.extend_from_slice(&[0, 0, 0, 1]);
        csd0.extend_from_slice(&config.sps);
        csd0.extend_from_slice(&[0, 0, 0, 1]);
        csd0.extend_from_slice(&config.pps);

        // SAFETY: all pointers below are checked for null before use; the `AMediaFormat`
        // is deleted before we return on every path; the codec is created onto `window`,
        // whose ownership moves into the returned `Codec` on success.
        unsafe {
            let format = sys::AMediaFormat_new();
            if format.is_null() {
                return Err(DecodeError::Adapter(
                    "AMediaFormat_new returned null".into(),
                ));
            }
            // Free the format on every exit path from here on.
            let result = (|| {
                // mime = "video/avc". `AMEDIAFORMAT_KEY_MIME` is a `static mut *const
                // c_char` the NDK populates at load; read it (unsafe) and pass our MIME.
                sys::AMediaFormat_setString(format, sys::AMEDIAFORMAT_KEY_MIME, MIME_AVC.as_ptr());
                // width/height (from the SPS): required by the C2 decoder at configure time.
                sys::AMediaFormat_setInt32(format, sys::AMEDIAFORMAT_KEY_WIDTH, width as i32);
                sys::AMediaFormat_setInt32(format, sys::AMEDIAFORMAT_KEY_HEIGHT, height as i32);
                // csd-0 = SPS+PPS. The decoder copies the buffer, so our `csd0` can drop
                // after this call.
                sys::AMediaFormat_setBuffer(
                    format,
                    sys::AMEDIAFORMAT_KEY_CSD_0,
                    csd0.as_ptr().cast(),
                    csd0.len(),
                );

                let codec_ptr = sys::AMediaCodec_createDecoderByType(MIME_AVC.as_ptr());
                let codec_ptr = NonNull::new(codec_ptr).ok_or_else(|| {
                    DecodeError::Adapter(
                        "AMediaCodec_createDecoderByType(video/avc) returned null".into(),
                    )
                })?;

                // Configure decode-to-surface: pass the window, no crypto, no flags
                // (flags = 0 means decoder, not encoder). width/height + csd-0 were set on
                // the format above.
                let status = sys::AMediaCodec_configure(
                    codec_ptr.as_ptr(),
                    format,
                    window.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                );
                if let Err(e) = check(status, "AMediaCodec_configure") {
                    sys::AMediaCodec_delete(codec_ptr.as_ptr());
                    return Err(e);
                }

                let status = sys::AMediaCodec_start(codec_ptr.as_ptr());
                if let Err(e) = check(status, "AMediaCodec_start") {
                    sys::AMediaCodec_delete(codec_ptr.as_ptr());
                    return Err(e);
                }

                Ok(Codec {
                    ptr: codec_ptr,
                    _window: window,
                })
            })();

            sys::AMediaFormat_delete(format);
            self.codec = Some(result?);
        }

        log::info!("MediaCodecDecoder: configured video/avc decode-to-surface");
        Ok(())
    }

    fn decode(&mut self, input: &DecoderInput) -> Result<(), DecodeError> {
        let codec = self
            .codec
            .as_ref()
            .ok_or_else(|| DecodeError::Adapter("decode called before configure".into()))?
            .ptr
            .as_ptr();

        // Acquire an input slot. `dequeueInputBuffer` returning < 0 is INFO_TRY_AGAIN_LATER:
        // no slot is free *yet* because the codec is still working through queued frames —
        // normal, not an error. Drain ready output (which renders frames and releases input
        // slots) and retry, until a slot frees up or we exhaust the budget.
        // SAFETY: `codec` is a started decoder.
        let in_index = {
            let mut attempts = 0u32;
            loop {
                let idx = unsafe { sys::AMediaCodec_dequeueInputBuffer(codec, DEQUEUE_TIMEOUT_US) };
                if idx >= 0 {
                    break idx as usize;
                }
                // No input slot yet: drain ready output (renders frames + frees input slots),
                // then retry — so the stall error below only fires after a drain attempt.
                self.drain_output(codec)?;
                attempts += 1;
                if attempts >= MAX_INPUT_DEQUEUE_ATTEMPTS {
                    return Err(DecodeError::Adapter(
                        "no input buffer after draining output; decoder stalled".into(),
                    ));
                }
            }
        };

        // SAFETY: `codec` is a started decoder and `in_index` is a valid input slot just
        // dequeued. We copy the access unit into the codec-owned buffer and queue it;
        // buffer pointer / size are validated before the copy.
        unsafe {
            let mut capacity: usize = 0;
            let buf = sys::AMediaCodec_getInputBuffer(codec, in_index, &mut capacity);
            if buf.is_null() {
                // The NDK contract requires every dequeued input slot to be queued or it is
                // lost. Queue it empty to return it to the codec, then surface the error.
                sys::AMediaCodec_queueInputBuffer(codec, in_index, 0, 0, 0, 0);
                return Err(DecodeError::Adapter("getInputBuffer returned null".into()));
            }
            let au = &input.annex_b;
            if au.len() > capacity {
                // Give the dequeued slot back (queue empty) before erroring, so a single
                // oversized AU does not permanently leak an input buffer.
                sys::AMediaCodec_queueInputBuffer(codec, in_index, 0, 0, 0, 0);
                return Err(DecodeError::Adapter(format!(
                    "access unit ({} B) exceeds input buffer capacity ({} B)",
                    au.len(),
                    capacity
                )));
            }
            std::ptr::copy_nonoverlapping(au.as_ptr(), buf, au.len());

            // flags = 0 (no BUFFER_FLAG_CODEC_CONFIG: csd-0 was supplied in the format;
            // a self-describing keyframe's SPS/PPS in-band is also fine as plain data).
            let status =
                sys::AMediaCodec_queueInputBuffer(codec, in_index, 0, au.len(), input.pts_us, 0);
            check(status, "queueInputBuffer")?;
        }

        // Drain whatever output is ready and render it onto the surface. We do not block
        // for output beyond a short timeout: at steady state one input yields ~one output,
        // and rendering is the point (decode-to-surface), so we render every ready frame.
        self.drain_output(codec)
    }
}

impl MediaCodecDecoder {
    /// Pull ready decoded frames and render them onto the surface (`render = true`).
    ///
    /// Loops until the decoder reports `TRY_AGAIN_LATER` (no more output ready), so a
    /// burst of buffered output is flushed in one call without unbounded blocking.
    fn drain_output(&self, codec: *mut sys::AMediaCodec) -> Result<(), DecodeError> {
        loop {
            let mut info = sys::AMediaCodecBufferInfo {
                offset: 0,
                size: 0,
                presentationTimeUs: 0,
                flags: 0,
            };
            // SAFETY: `codec` is a started decoder; `info` is a valid out-param.
            let out_index = unsafe {
                sys::AMediaCodec_dequeueOutputBuffer(codec, &mut info, DEQUEUE_TIMEOUT_US)
            };

            if out_index >= 0 {
                // A decoded frame: release it WITH render so it composites onto the
                // ANativeWindow (D3 — no CPU copy back to us).
                // SAFETY: `out_index` is a valid output buffer index just dequeued.
                let status = unsafe {
                    sys::AMediaCodec_releaseOutputBuffer(codec, out_index as usize, true)
                };
                check(status, "releaseOutputBuffer(render=true)")?;
                // Keep draining: more frames may be queued.
                continue;
            }

            match out_index {
                INFO_TRY_AGAIN_LATER => return Ok(()),
                // Format / buffer changes are informational for decode-to-surface; the
                // surface adapts automatically. Keep draining.
                INFO_OUTPUT_FORMAT_CHANGED | INFO_OUTPUT_BUFFERS_CHANGED => continue,
                other => {
                    return Err(DecodeError::Adapter(format!(
                        "dequeueOutputBuffer returned unexpected status {other}"
                    )))
                }
            }
        }
    }
}
