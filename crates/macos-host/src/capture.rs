//! The `Capturer` port (seam) for pulling frames off the virtual display.
//!
//! This is the stable Rust-side contract (D0 / §0.1 of the roadmap, R3 mitigation).
//! The *simplest-now* adapter behind it is ScreenCaptureKit (`SCStream` filtered to
//! the P2 virtual display's `CGDirectDisplayID`), with `CGDisplayStream` as a
//! fallback; a later phase swaps the adapter for pure `objc2-screen-capture-kit`
//! **without changing this trait** or any pipeline code that consumes it.
//!
//! ## Handoff (needs a hands-on Mac run, not the phone)
//! The ScreenCaptureKit adapter is implemented separately because it requires running
//! against a live virtual display and visually verifying output — it cannot be unit
//! tested here. The pipeline that *drives* a `Capturer` (see [`crate::encode`]) is
//! fully testable today via a fake capturer, so adding the real adapter is a localized,
//! low-risk change behind this seam.

/// One frame captured from the virtual display — **metadata only** for now.
///
/// ⚠ Seam-stability caveat (honest status): this currently carries *only* pts/size, so
/// the [`Capturer`]→[`Encoder`] path cannot yet hand real pixels across. Wave B's
/// zero-copy `IOSurface`/`CVPixelBuffer` capture will require extending this struct (or
/// `Encoder::encode`) to carry a surface handle — a deliberate, **breaking** change to
/// this port. The metadata shape is what keeps Wave A unit-testable without hardware;
/// it is not the final port surface. (Capture+encode are fused inside the Wave B
/// adapter, so the surface never actually crosses the trait boundary at runtime — but
/// the type still has to grow to carry it.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFrame {
    /// Presentation timestamp in microseconds (monotonic from capture start).
    pub pts_us: u64,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
}

/// A source of captured frames. The simplest-now adapter wraps an `SCStream`; this
/// trait is what the encode pipeline depends on so the adapter is swappable (R3).
pub trait Capturer {
    /// Pull the next captured frame, or `None` once the capture stream ends.
    fn next_frame(&mut self) -> Option<CapturedFrame>;
}
