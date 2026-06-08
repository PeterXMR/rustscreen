//! Safe Rust API over macOS virtual-display creation.
//!
//! This is the stable *port* (D0 / §0.1 of the roadmap). The implementation is pure
//! [`objc2`] message-sends to the **private** CoreGraphics `CGVirtualDisplay` family
//! (`CGVirtualDisplay`, `CGVirtualDisplayDescriptor`, `CGVirtualDisplaySettings`,
//! `CGVirtualDisplayMode`). The Objective-C runtime resolves these real classes — which
//! ship inside CoreGraphics — at launch; there is no public header for them, so we look
//! them up by name and drive them with hand-written msg-sends.
//!
//! This replaces the earlier ObjC++ `shim.mm` (compiled via the `cc` crate); the public
//! `VirtualDisplay` API is byte-identical, so callers are unaffected by the swap (P7
//! Rust-purity upgrade — see §0.1 of the roadmap).
//!
//! The virtual display exists for as long as the [`VirtualDisplay`] value is alive; dropping
//! it tears the display down.

#![cfg(target_os = "macos")]

mod config;
pub use config::*;

use std::fmt;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSArray, NSString};

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

/// Looks up one of the private `CGVirtualDisplay*` classes by name. Returns `None` if the
/// class is not present in this macOS build (i.e. the private API is unavailable).
fn private_class(name: &str) -> Option<&'static AnyClass> {
    // `AnyClass::get` takes a NUL-terminated C string; build one on the stack.
    let mut buf = String::with_capacity(name.len() + 1);
    buf.push_str(name);
    buf.push('\0');
    let cstr = std::ffi::CStr::from_bytes_with_nul(buf.as_bytes()).ok()?;
    AnyClass::get(cstr)
}

/// A live macOS virtual display. The display is removed when this value is dropped.
///
/// We retain the `CGVirtualDisplay` object (`_display`) plus the descriptor (`_desc`) and its
/// dispatch queue (`_queue`). Rather than *assume* the private API takes ownership of the queue
/// and descriptor (an undocumented retain contract — if it is `assign`/`weak` instead of
/// `strong`, dropping our local handles after creation would free a queue CoreGraphics still
/// fires its termination handler onto, a use-after-free on teardown), we keep them alive for the
/// display's lifetime ourselves. Field order is the drop order: `_display` is released first (it
/// may reference the descriptor), then `_desc` (which references the queue), then `_queue`.
pub struct VirtualDisplay {
    _display: Retained<AnyObject>,
    _desc: Retained<AnyObject>,
    _queue: dispatch2::DispatchRetained<dispatch2::DispatchQueue>,
    display_id: u32,
}

impl VirtualDisplay {
    /// Create a virtual display of `width`x`height` pixels at `refresh` Hz, with the legacy
    /// defaults (stable identity, HiDPI off, no programmatic arrangement). Thin wrapper over
    /// [`with_config`](Self::with_config) so existing callers are unaffected.
    pub fn new(width: u32, height: u32, refresh: f64) -> Result<Self, VirtualDisplayError> {
        Self::with_config(&DisplayConfig::new(width, height, refresh))
    }

    /// Create a virtual display from a full [`DisplayConfig`] (item 2: name, identity, HiDPI
    /// scaled modes, and optional default-side arrangement).
    pub fn with_config(cfg: &DisplayConfig) -> Result<Self, VirtualDisplayError> {
        if cfg.width == 0 || cfg.height == 0 {
            return Err(VirtualDisplayError::CreationFailed);
        }
        // Reject a non-finite or non-positive refresh up front rather than handing 0/NaN to the
        // private `initWithWidth:height:refreshRate:` (a config error, not a runtime surprise).
        if !cfg.refresh.is_finite() || cfg.refresh <= 0.0 {
            return Err(VirtualDisplayError::CreationFailed);
        }
        // SAFETY: every msg-send below targets a private CoreGraphics class resolved by name;
        // the selectors and argument types match the reverse-engineered interfaces (the same
        // shapes the former ObjC++ shim re-declared). A missing class or a rejected
        // `applySettings:` is handled as `CreationFailed`, and we never touch a null object.
        unsafe { Self::new_inner(cfg) }
    }

    unsafe fn new_inner(cfg: &DisplayConfig) -> Result<Self, VirtualDisplayError> {
        let desc_cls = private_class("CGVirtualDisplayDescriptor");
        let mode_cls = private_class("CGVirtualDisplayMode");
        let set_cls = private_class("CGVirtualDisplaySettings");
        let disp_cls = private_class("CGVirtualDisplay");
        let (desc_cls, mode_cls, set_cls, disp_cls) = match (desc_cls, mode_cls, set_cls, disp_cls)
        {
            (Some(d), Some(m), Some(s), Some(v)) => (d, m, s, v),
            // One or more private classes are absent on this macOS build.
            _ => return Err(VirtualDisplayError::CreationFailed),
        };

        // --- Descriptor -----------------------------------------------------------------
        let desc: Retained<AnyObject> = {
            let allocated: *mut AnyObject = msg_send![desc_cls, alloc];
            let obj: *mut AnyObject = msg_send![allocated, init];
            Retained::from_raw(obj).ok_or(VirtualDisplayError::CreationFailed)?
        };

        let name = NSString::from_str(&cfg.name);
        let _: () = msg_send![&*desc, setName: &*name];
        // `maxPixelsWide`/`maxPixelsHigh` are `NSUInteger` (64-bit on arm64/x86_64). Pass `usize`,
        // not `u32`: a sub-word `u32` argument leaves the upper 32 bits of the register undefined
        // under the arm64 calling convention, which CoreGraphics could read as a garbage dimension.
        // Widening is safe either way — if the real selector is `uint32_t` the callee just reads
        // the correct low 32 bits.
        let _: () = msg_send![&*desc, setMaxPixelsWide: cfg.width as usize];
        let _: () = msg_send![&*desc, setMaxPixelsHigh: cfg.height as usize];
        // Physical size at ~110 ppi (1 px ≈ 0.231 mm). Only affects reported DPI, not creation.
        let size = CGSize::new(cfg.width as f64 * 0.231, cfg.height as f64 * 0.231);
        let _: () = msg_send![&*desc, setSizeInMillimeters: size];
        // Stable identity (vendor/product/serial) → macOS remembers the arrangement position.
        let _: () = msg_send![&*desc, setProductID: cfg.product_id];
        let _: () = msg_send![&*desc, setVendorID: cfg.vendor_id];
        let _: () = msg_send![&*desc, setSerialNum: cfg.serial];

        // The descriptor needs a serial dispatch queue (the private API fires its termination
        // handler on it). `dispatch2::DispatchQueue` is an objc-compatible dispatch object, so
        // we hand its pointer to `setQueue:`. We keep the queue alive for the display's lifetime.
        let queue = dispatch2::DispatchQueue::new("com.rustscreen.vdisplay", None);
        let queue_obj: *mut AnyObject = NonNull::from(&*queue).as_ptr().cast();
        let _: () = msg_send![&*desc, setQueue: queue_obj];

        // Termination handler: a no-op `void (^)(id, id)` block. CoreGraphics copies the block
        // when it is set, so the local `RcBlock` may drop after this call.
        let termination = RcBlock::new(|_a: *mut AnyObject, _b: *mut AnyObject| {});
        let _: () = msg_send![&*desc, setTerminationHandler: &*termination];

        // --- Display --------------------------------------------------------------------
        let disp: Retained<AnyObject> = {
            let allocated: *mut AnyObject = msg_send![disp_cls, alloc];
            let obj: *mut AnyObject = msg_send![allocated, initWithDescriptor: &*desc];
            Retained::from_raw(obj).ok_or(VirtualDisplayError::CreationFailed)?
        };

        // --- Mode + Settings ------------------------------------------------------------
        // Register the native pixel mode plus, when HiDPI is on, a scaled mode (D6) so macOS
        // exposes a "Larger Text … More Space" scaling slider. `hidpi_modes` builds the
        // ordered, de-duplicated spec list (pure-logic, unit-tested).
        let specs = hidpi_modes(cfg.width, cfg.height, cfg.refresh, cfg.hidpi);
        let mut mode_objs: Vec<Retained<AnyObject>> = Vec::with_capacity(specs.len());
        for spec in &specs {
            let allocated: *mut AnyObject = msg_send![mode_cls, alloc];
            let obj: *mut AnyObject = msg_send![
                allocated,
                initWithWidth: spec.width,
                height: spec.height,
                refreshRate: spec.refresh,
            ];
            mode_objs.push(Retained::from_raw(obj).ok_or(VirtualDisplayError::CreationFailed)?);
        }

        let settings: Retained<AnyObject> = {
            let allocated: *mut AnyObject = msg_send![set_cls, alloc];
            let obj: *mut AnyObject = msg_send![allocated, init];
            Retained::from_raw(obj).ok_or(VirtualDisplayError::CreationFailed)?
        };
        let modes = NSArray::from_retained_slice(&mode_objs);
        let _: () = msg_send![&*settings, setModes: &*modes];
        let _: () = msg_send![&*settings, setHiDPI: cfg.hidpi as u32];

        // --- Apply ----------------------------------------------------------------------
        let applied: bool = msg_send![&*disp, applySettings: &*settings];
        if !applied {
            return Err(VirtualDisplayError::CreationFailed);
        }

        let display_id: u32 = msg_send![&*disp, displayID];

        // Keep the descriptor and its dispatch queue alive for the display's lifetime (see the
        // `VirtualDisplay` struct doc) instead of relying on the private API's undocumented retain
        // semantics. The mode objects and `settings` are not referenced after `applySettings:`.
        let display = Self {
            _display: disp,
            _desc: desc,
            _queue: queue,
            display_id,
        };
        // Place on the requested side (non-fatal on CG error — see `arrange`).
        if let Arrangement::Side(side) = cfg.arrangement {
            display.arrange(side);
        }
        Ok(display)
    }

    /// The `CGDirectDisplayID` of the created display (non-zero on success), usable by
    /// downstream capture (P3).
    pub fn display_id(&self) -> u32 {
        self.display_id
    }

    /// Place this display on `side` of the main display via `CGConfigureDisplayOrigin`.
    /// Non-fatal: on any CoreGraphics error the display simply stays at its default macOS
    /// position (stable identity still lets macOS remember a later manual rearrange).
    pub fn arrange(&self, side: Side) {
        // SAFETY: a pure CG display-configuration transaction; `display_id` is this display's
        // own id and every begin is balanced by exactly one complete or cancel on every path.
        unsafe {
            let main_id = cg::CGMainDisplayID();
            let mb = cg::CGDisplayBounds(main_id);
            let main = DisplayBounds {
                x: mb.origin.x as i32,
                y: mb.origin.y as i32,
                width: mb.size.width as i32,
                height: mb.size.height as i32,
            };
            // The virtual display's own bounds give its (point) size for placement.
            let vb = cg::CGDisplayBounds(self.display_id);
            let vsize = (vb.size.width as i32, vb.size.height as i32);
            let (ox, oy) = arrangement_origin(main, vsize, side);

            let mut token: cg::CGDisplayConfigRef = core::ptr::null_mut();
            if cg::CGBeginDisplayConfiguration(&mut token) != 0 {
                eprintln!(
                    "cg-virtual-display: arrange: begin-config failed; leaving default position"
                );
                return;
            }
            if cg::CGConfigureDisplayOrigin(token, self.display_id, ox, oy) != 0 {
                eprintln!(
                    "cg-virtual-display: arrange: set-origin failed; leaving default position"
                );
                // Discard the open transaction so the rejected origin is not applied.
                // `CGCancelDisplayConfiguration` rolls back; `CGComplete*` would commit it.
                let _ = cg::CGCancelDisplayConfiguration(token);
                return;
            }
            // 0 = kCGConfigureForAppOnly: the change lives for as long as we hold the display.
            if cg::CGCompleteDisplayConfiguration(token, 0) != 0 {
                eprintln!("cg-virtual-display: arrange: complete-config failed");
            }
        }
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        // The retained `CGVirtualDisplay` is released right after this, tearing the display
        // down so the desktop reflows. Log it so disconnect teardown is observable.
        eprintln!(
            "cg-virtual-display: removing virtual display {} (desktop will reflow)",
            self.display_id
        );
    }
}

/// Minimal CoreGraphics display-configuration FFI (same inline-`extern "C"` style as
/// [`active_display_count`]). Used by [`VirtualDisplay::arrange`] to place the display.
mod cg {
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGSize {
        pub width: f64,
        pub height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }
    pub type CGDirectDisplayID = u32;
    pub type CGDisplayConfigRef = *mut core::ffi::c_void;

    extern "C" {
        pub fn CGMainDisplayID() -> CGDirectDisplayID;
        pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
        pub fn CGBeginDisplayConfiguration(config: *mut CGDisplayConfigRef) -> i32;
        pub fn CGConfigureDisplayOrigin(
            config: CGDisplayConfigRef,
            display: CGDirectDisplayID,
            x: i32,
            y: i32,
        ) -> i32;
        pub fn CGCompleteDisplayConfiguration(config: CGDisplayConfigRef, option: u32) -> i32;
        pub fn CGCancelDisplayConfiguration(config: CGDisplayConfigRef) -> i32;
    }
}

/// Number of active displays the window server currently reports. Used to observe that a
/// virtual display actually registered with the system.
pub fn active_display_count() -> u32 {
    extern "C" {
        fn CGGetActiveDisplayList(
            max_displays: u32,
            active_displays: *mut u32,
            display_count: *mut u32,
        ) -> i32;
    }
    let mut count: u32 = 0;
    // SAFETY: pure query — passing a null list with max 0 only fills `count`.
    let err = unsafe { CGGetActiveDisplayList(0, std::ptr::null_mut(), &mut count) };
    if err != 0 {
        return 0;
    }
    count
}
