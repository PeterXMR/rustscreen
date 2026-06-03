//! Safe Rust API over macOS virtual-display creation.
//!
//! This is the stable *port* (D0 / §0.1 of the roadmap). The current implementation is
//! backed by a small Objective-C++ shim (`src/shim.mm`) calling the private CoreGraphics
//! `CGVirtualDisplay` API. A later phase (P7) replaces the shim with pure `objc2` bindings
//! without changing this public API.
//!
//! The virtual display exists for as long as the [`VirtualDisplay`] value is alive; dropping
//! it tears the display down.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::fmt;

extern "C" {
    fn rs_vdisplay_create(
        width: u32,
        height: u32,
        refresh: f64,
        out_display_id: *mut u32,
    ) -> *mut c_void;
    fn rs_vdisplay_destroy(handle: *mut c_void);
    fn rs_active_display_count() -> u32;
}

/// Error creating a virtual display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtualDisplayError {
    /// The private `CGVirtualDisplay` API rejected the request or is unavailable on this OS.
    CreationFailed,
}

impl fmt::Display for VirtualDisplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VirtualDisplayError::CreationFailed => write!(
                f,
                "CGVirtualDisplay creation failed (private API unavailable or settings rejected)"
            ),
        }
    }
}

impl std::error::Error for VirtualDisplayError {}

/// A live macOS virtual display. The display is removed when this value is dropped.
pub struct VirtualDisplay {
    handle: *mut c_void,
    display_id: u32,
}

impl VirtualDisplay {
    /// Create a virtual display of `width`x`height` pixels at `refresh` Hz.
    pub fn new(width: u32, height: u32, refresh: f64) -> Result<Self, VirtualDisplayError> {
        let mut display_id: u32 = 0;
        // SAFETY: FFI to the shim; `out_display_id` points to a valid local; a null return
        // means failure and we never dereference the handle in that case.
        let handle = unsafe { rs_vdisplay_create(width, height, refresh, &mut display_id) };
        if handle.is_null() {
            return Err(VirtualDisplayError::CreationFailed);
        }
        Ok(Self { handle, display_id })
    }

    /// The `CGDirectDisplayID` of the created display (non-zero on success), usable by
    /// downstream capture (P3).
    pub fn display_id(&self) -> u32 {
        self.display_id
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        // SAFETY: `handle` was produced by `rs_vdisplay_create` and is released exactly once.
        unsafe { rs_vdisplay_destroy(self.handle) };
    }
}

/// Number of active displays the window server currently reports. Used to observe that a
/// virtual display actually registered with the system.
pub fn active_display_count() -> u32 {
    // SAFETY: FFI to a pure query with no arguments.
    unsafe { rs_active_display_count() }
}
