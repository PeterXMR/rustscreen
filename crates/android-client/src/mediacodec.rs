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

use std::cell::Cell;
use std::ffi::CStr;
use std::ptr::NonNull;

use ndk_sys as sys;
use protocol::messages::VideoCodec;
use protocol::nal::CodecConfig;

use crate::decode::{DecodeError, DecoderInput, PresentedFrame, VideoDecoder};

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
    /// Access units handed to `queueInputBuffer` so far (latency item A depth accounting).
    queued: Cell<u64>,
    /// Output buffers pulled from `dequeueOutputBuffer` so far (rendered or dropped).
    dequeued: Cell<u64>,
}

impl MediaCodecDecoder {
    /// Build an adapter that will decode onto `window`. The codec itself is created on the
    /// first [`VideoDecoder::configure`] call, when SPS/PPS are known.
    pub fn new(window: NativeWindow) -> Self {
        MediaCodecDecoder {
            window: Some(window),
            codec: None,
            queued: Cell::new(0),
            dequeued: Cell::new(0),
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
                // Low-latency decode (item 6 step 2). Tell the HW decoder to emit each frame as
                // soon as it is decoded instead of buffering a reorder/lookahead window. On the
                // Pixel 6a the default behaviour holds ~15+ frames internally (~290 ms measured),
                // which dominated glass-to-glass; with this set, arrive→decode collapses toward the
                // raw per-frame decode time. The key exists from API 31; the Pixel 6a (API 33+)
                // honours it. (libmediandk resolves the symbol at load — fine on the target device;
                // a future minSdk<31 build would gate this behind a runtime API check.)
                sys::AMediaFormat_setInt32(format, sys::AMEDIAFORMAT_KEY_LOW_LATENCY, 1);
                // The Pixel 6a's decoder is `c2.exynos.h264.decoder` (Google Tensor), which
                // IGNORES the generic KEY_LOW_LATENCY above — the measured ~290+ ms input-queue
                // residence is the symptom. The Exynos C2 component honours this *vendor* key
                // instead (Moonlight sets per-SoC vendor keys for exactly this reason). Setting
                // an unknown vendor key on another decoder is silently ignored, so this is safe
                // to set unconditionally. Verify on-device via logcat (CCodec component name).
                const VENDOR_LOW_LATENCY_KEY: &CStr = c"vendor.rtc-ext-dec-low-latency.enable";
                sys::AMediaFormat_setInt32(format, VENDOR_LOW_LATENCY_KEY.as_ptr(), 1);
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

    fn decode(&mut self, input: &DecoderInput) -> Result<Vec<PresentedFrame>, DecodeError> {
        let codec = self
            .codec
            .as_ref()
            .ok_or_else(|| DecodeError::Adapter("decode called before configure".into()))?
            .ptr
            .as_ptr();

        // Accumulates every frame released-with-render during this call (both while
        // draining to free an input slot below and in the final drain), reported up so the
        // session layer can emit per-frame latency `Stats` (Task 7).
        let mut presented: Vec<PresentedFrame> = Vec::new();

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
                self.drain_output(codec, &mut presented)?;
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
            self.queued.set(self.queued.get() + 1);
        }

        // Drain whatever output is ready and render it onto the surface. We do not block
        // for output beyond a short timeout: at steady state one input yields ~one output,
        // and rendering is the point (decode-to-surface), so we render every ready frame.
        self.drain_output(codec, &mut presented)?;
        Ok(presented)
    }

    fn in_flight(&self) -> usize {
        // Saturating: dequeued can briefly equal queued; never underflow.
        self.queued.get().saturating_sub(self.dequeued.get()) as usize
    }

    fn pump(&mut self) -> Result<Vec<PresentedFrame>, DecodeError> {
        // Drain ready output without submitting input — used when the session's pacer drops an
        // incoming frame but the codec should keep draining so the surface stays current.
        let codec = self
            .codec
            .as_ref()
            .ok_or_else(|| DecodeError::Adapter("pump called before configure".into()))?
            .ptr
            .as_ptr();
        let mut presented = Vec::new();
        self.drain_output(codec, &mut presented)?;
        Ok(presented)
    }
}

impl MediaCodecDecoder {
    /// Pull ready decoded frames and render them onto the surface (`render = true`),
    /// appending one [`PresentedFrame`] per rendered buffer to `presented`.
    ///
    /// Loops until the decoder reports `TRY_AGAIN_LATER` (no more output ready), so a
    /// burst of buffered output is flushed in one call without unbounded blocking. Each
    /// rendered buffer is stamped with the process-global monotonic phone clock
    /// ([`crate::now_us`]): `decode_us` when the output buffer is dequeued (decode
    /// complete), `present_us` right after `releaseOutputBuffer(render = true)` (the
    /// present), keyed by the buffer's `presentationTimeUs` (the originating
    /// `Frame::Video.pts_us`). Using the global clock ensures these timestamps share the
    /// same epoch as `arrive_us` and the ClockPong t1/t2 values stamped in `run_session`.
    fn drain_output(
        &self,
        codec: *mut sys::AMediaCodec,
        presented: &mut Vec<PresentedFrame>,
    ) -> Result<(), DecodeError> {
        // First collect every output buffer that is ready RIGHT NOW (without blocking past the
        // first not-ready dequeue), each with its decode-complete stamp. We then present only the
        // NEWEST and drop the rest (item 6 step 2): for a live mirror the freshest decodable frame
        // wins — rendering stale frames just to "show every frame" re-introduces exactly the
        // present-queue latency we are removing. With LOW_LATENCY the decoder rarely buffers ahead,
        // so in steady state this collects a single buffer and drops nothing.
        let mut ready: Vec<(usize, u64, u64)> = Vec::new(); // (out_index, pts_us, decode_us)
        loop {
            let mut info = sys::AMediaCodecBufferInfo {
                offset: 0,
                size: 0,
                presentationTimeUs: 0,
                flags: 0,
            };
            // Block (up to DEQUEUE_TIMEOUT_US) ONLY while waiting for the first buffer of this
            // drain; once we have at least one, poll the rest non-blocking (timeout 0). The old
            // code used the full 10 ms timeout on EVERY iteration, so after collecting the newest
            // buffer the trailing "is there another?" poll always blocked the full ~10 ms before
            // returning TRY_AGAIN_LATER. That wasted ~10 ms per frame on the single decode thread:
            // it inflated `decode→present` to ~one timeout AND, worse, kept the thread from reading
            // the next access unit off USB — surfacing upstream as `send→arrive`. With LOW_LATENCY
            // the decoder emits ~one buffer per input, so in steady state the previous frame is
            // already decoded and the first dequeue returns immediately; the timeout now only bites
            // on a genuine stall/startup. (Lowest-latency: drain what's ready, never block for more.)
            let timeout = if ready.is_empty() {
                DEQUEUE_TIMEOUT_US
            } else {
                0
            };
            // SAFETY: `codec` is a started decoder; `info` is a valid out-param.
            let out_index =
                unsafe { sys::AMediaCodec_dequeueOutputBuffer(codec, &mut info, timeout) };

            if out_index >= 0 {
                // Decoded buffer ready: stamp decode-complete now (the dequeue IS the decode).
                // Defer the present decision until we know whether a fresher buffer is also ready.
                let decode_us = crate::now_us();
                // `presentationTimeUs` is the pts we queued with this access unit; the cast is
                // safe because we only ever queue non-negative `u64` pts values.
                ready.push((
                    out_index as usize,
                    info.presentationTimeUs as u64,
                    decode_us,
                ));
                // Keep draining: more frames may already be decoded and waiting.
                continue;
            }

            match out_index {
                INFO_TRY_AGAIN_LATER => break,
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

        // Depth accounting (latency item A): every dequeued output — whether we render it or
        // drop it as stale — leaves the codec's pipeline, so it counts against in-flight.
        self.dequeued.set(self.dequeued.get() + ready.len() as u64);

        let Some((&(newest_idx, newest_pts, newest_decode_us), stale)) = ready.split_last() else {
            return Ok(()); // nothing decoded this round
        };

        // Drop every buffer older than the newest: release WITHOUT render (frees the buffer, no
        // present). This is what keeps the surface current under oversupply.
        for &(idx, ..) in stale {
            // SAFETY: `idx` is a valid output buffer index just dequeued and not yet released.
            let status = unsafe { sys::AMediaCodec_releaseOutputBuffer(codec, idx, false) };
            check(status, "releaseOutputBuffer(render=false drop)")?;
        }
        if !stale.is_empty() {
            log::debug!(
                "MediaCodecDecoder: dropped {} late frame(s) to stay current",
                stale.len()
            );
        }

        // Present the newest decoded buffer onto the ANativeWindow (D3 — no CPU copy back to us)
        // using the TIMESTAMPED release so the decode thread does NOT block on the next vsync
        // (latency PR 4 / research category D). The boolean form `releaseOutputBuffer(idx, true)`
        // couples this thread to SurfaceFlinger's vsync — it blocks ~one refresh (the measured
        // ~13 ms `decode→present`) — and while it blocks we are NOT draining USB, which is the
        // dominant upstream phone-pull cost (the host's transfer can't complete until we read the
        // next bytes, so it surfaces as `encode→send`/`send→arrive`). `releaseOutputBufferAtTime`
        // hands the buffer to the compositor with a desired present time and returns immediately;
        // SurfaceFlinger scans it out at the next vsync (the unavoidable panel latency) while we
        // get straight back to reading the next access unit. A timestamp of "now" = present ASAP.
        // `present_us` now stamps the HANDOFF (actual scanout is ~one vsync later, as before — but
        // it no longer stalls this thread); the global clock keeps it in the same epoch as
        // arrive_us / ClockPong t1,t2.
        // SAFETY: `newest_idx` is a valid output buffer index just dequeued and not yet released.
        let status = unsafe {
            sys::AMediaCodec_releaseOutputBufferAtTime(codec, newest_idx, monotonic_now_ns())
        };
        check(status, "releaseOutputBufferAtTime(render now)")?;
        let present_us = crate::now_us();
        presented.push(PresentedFrame {
            pts_us: newest_pts,
            decode_us: newest_decode_us,
            present_us,
        });
        Ok(())
    }
}

/// Current `CLOCK_MONOTONIC` time in nanoseconds — the timebase
/// [`sys::AMediaCodec_releaseOutputBufferAtTime`] expects for the desired present time (the same
/// domain as `System.nanoTime()` and the Choreographer vsync clock). Returning "now" presents the
/// frame as soon as possible **without blocking the decode thread on vsync** (PR 4).
///
/// Declared directly against libc (`clock_gettime`) rather than pulling the `libc` crate for one
/// call — the symbol is always linked on Android. `c_long` matches bionic's `time_t`/`tv_nsec`
/// (both `long`) on every Android ABI (64-bit on lp64 targets, 32-bit on ilp32), so the
/// `timespec` layout is correct without per-ABI gating.
// `i64::from(c_long)` is a real widening on 32-bit ABIs (`c_long == i32`) but a no-op on lp64
// (`c_long == i64`), where clippy flags it as a useless conversion. We target arm64 (lp64) in
// practice, so allow it here to keep the source correct for both without a per-ABI `cfg`.
#[allow(clippy::useless_conversion)]
fn monotonic_now_ns() -> i64 {
    use core::ffi::{c_int, c_long};
    #[repr(C)]
    struct Timespec {
        tv_sec: c_long,
        tv_nsec: c_long,
    }
    extern "C" {
        fn clock_gettime(clk_id: c_int, tp: *mut Timespec) -> c_int;
    }
    const CLOCK_MONOTONIC: c_int = 1;
    let mut ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-param for the duration of the call and CLOCK_MONOTONIC is always
    // available. On the (practically impossible) failure path we return 0 — a timestamp in the far
    // past, which the codec also treats as "present ASAP" — so the present still happens.
    let rc = unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
    if rc != 0 {
        return 0;
    }
    // `i64::from` (not `as i64`) widens correctly whether `c_long` is 32-bit (ilp32 ABIs) or
    // 64-bit (lp64) — and is a no-op the cast lint won't flag on lp64 where `c_long == i64`.
    i64::from(ts.tv_sec)
        .saturating_mul(1_000_000_000)
        .saturating_add(i64::from(ts.tv_nsec))
}
