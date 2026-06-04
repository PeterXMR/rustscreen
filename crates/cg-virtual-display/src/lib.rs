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
/// We hold only the retained `CGVirtualDisplay` object (`_display`); dropping it releases
/// the object, which tears the on-screen display down. The dispatch queue and termination
/// handler are *not* stored here: the private API retains them internally when they are set
/// on the descriptor (the `queue` property is `strong`, the `terminationHandler` is `copy`),
/// so our local handles in `new_inner` can be dropped after creation without a
/// use-after-free.
pub struct VirtualDisplay {
    _display: Retained<AnyObject>,
    display_id: u32,
}

impl VirtualDisplay {
    /// Create a virtual display of `width`x`height` pixels at `refresh` Hz.
    pub fn new(width: u32, height: u32, refresh: f64) -> Result<Self, VirtualDisplayError> {
        // SAFETY: every msg-send below targets a private CoreGraphics class resolved by name;
        // the selectors and argument types match the reverse-engineered interfaces (the same
        // shapes the former ObjC++ shim re-declared). A missing class or a rejected
        // `applySettings:` is handled as `CreationFailed`, and we never touch a null object.
        unsafe { Self::new_inner(width, height, refresh) }
    }

    unsafe fn new_inner(
        width: u32,
        height: u32,
        refresh: f64,
    ) -> Result<Self, VirtualDisplayError> {
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

        let name = NSString::from_str("RustScreen");
        let _: () = msg_send![&*desc, setName: &*name];
        let _: () = msg_send![&*desc, setMaxPixelsWide: width];
        let _: () = msg_send![&*desc, setMaxPixelsHigh: height];
        // Physical size at ~110 ppi (1 px ≈ 0.231 mm). Only affects reported DPI, not creation.
        let size = CGSize::new(width as f64 * 0.231, height as f64 * 0.231);
        let _: () = msg_send![&*desc, setSizeInMillimeters: size];
        let _: () = msg_send![&*desc, setProductID: 0x0001u32];
        // 'rm' — arbitrary, identifies RustScreen displays.
        let _: () = msg_send![&*desc, setVendorID: 0x726Du32];
        let _: () = msg_send![&*desc, setSerialNum: 0x0001u32];

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
        let mode: Retained<AnyObject> = {
            let allocated: *mut AnyObject = msg_send![mode_cls, alloc];
            let obj: *mut AnyObject = msg_send![
                allocated,
                initWithWidth: width,
                height: height,
                refreshRate: refresh,
            ];
            Retained::from_raw(obj).ok_or(VirtualDisplayError::CreationFailed)?
        };

        let settings: Retained<AnyObject> = {
            let allocated: *mut AnyObject = msg_send![set_cls, alloc];
            let obj: *mut AnyObject = msg_send![allocated, init];
            Retained::from_raw(obj).ok_or(VirtualDisplayError::CreationFailed)?
        };
        let modes = NSArray::from_retained_slice(std::slice::from_ref(&mode));
        let _: () = msg_send![&*settings, setModes: &*modes];
        let _: () = msg_send![&*settings, setHiDPI: 0u32];

        // --- Apply ----------------------------------------------------------------------
        let applied: bool = msg_send![&*disp, applySettings: &*settings];
        if !applied {
            return Err(VirtualDisplayError::CreationFailed);
        }

        let display_id: u32 = msg_send![&*disp, displayID];

        // The dispatch queue is retained by the descriptor/display internally, so we may drop
        // our local handle; `desc`, `mode`, `settings` are likewise retained as needed. Only the
        // display itself must outlive this function to keep the on-screen display alive.
        drop(queue);

        Ok(Self {
            _display: disp,
            display_id,
        })
    }

    /// The `CGDirectDisplayID` of the created display (non-zero on success), usable by
    /// downstream capture (P3).
    pub fn display_id(&self) -> u32 {
        self.display_id
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
