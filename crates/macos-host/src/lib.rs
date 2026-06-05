//! RustScreen macOS host library: the platform-agnostic, unit-tested core that the
//! binary (`main.rs`) and the macOS hardware adapters build on.
//!
//! P3 introduces two seams here (R3 mitigation — wrap experimental macOS APIs behind
//! our own traits so they stay swappable):
//! - [`capture::Capturer`] — pull frames off the virtual display (ScreenCaptureKit).
//! - [`encode::Encoder`] — hardware-encode to H.264 (VideoToolbox).
//!
//! The pipeline that drives them ([`encode::run_session`]), codec-config extraction,
//! and latency logging are all here and tested without hardware; the SCK/VideoToolbox
//! adapters drop in behind the traits.

pub mod capture;
pub mod capture_select;
pub mod daemon;
pub mod encode;
pub mod encode_vt;
pub mod latency;
pub mod session;
pub mod touch;
pub mod transport;

/// Live USB (AOA) host adapter — compiled only under `--features live-usb` (Wave B,
/// hands-on-Mac). Excluded from default CI so Wave A stays cross-platform.
#[cfg(feature = "live-usb")]
pub mod aoa;

/// The live host pipeline (`serve::run_host`) — virtual display → capture → encode → AOA → phone.
/// Extracted from the `p5_stream` spike; both `p5_stream` and the `rustscreen` daemon drive it.
/// Needs both the capture/encode stack and the USB host path, so it is gated on both features.
#[cfg(all(feature = "live-capture", feature = "live-usb"))]
pub mod serve;
