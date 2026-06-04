//! Links the CoreGraphics framework on macOS.
//!
//! The crate drives the private `CGVirtualDisplay` family with pure `objc2` msg-sends (P7
//! Rust-purity upgrade — there is no longer an ObjC++ `.mm` shim nor a `cc` build dep). The
//! private classes are resolved at runtime via the Objective-C runtime, so they need no
//! link-time symbols; we only link CoreGraphics for the `CGGetActiveDisplayList` C function
//! used by `active_display_count`.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
}
