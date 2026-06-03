//! Compiles the Objective-C++ shim that calls the private `CGVirtualDisplay` API.
//!
//! This shim is the "simplest-now" adapter (D0 / §0.1 of the roadmap). The plan is to
//! replace it with pure `objc2` bindings in a later phase (P7); the safe `VirtualDisplay`
//! API in `lib.rs` stays identical, so callers are unaffected by that swap.

fn main() {
    // Only build the shim on macOS — the private CoreGraphics class exists nowhere else.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    println!("cargo:rerun-if-changed=src/shim.mm");

    cc::Build::new()
        .file("src/shim.mm")
        .flag("-fobjc-arc") // let ARC manage the Objective-C object lifetimes inside the shim
        .compile("cgvd_shim");

    // The private CGVirtualDisplay* classes live in the CoreGraphics framework; Foundation
    // pulls in the Objective-C runtime + NSString/NSArray/dispatch used by the shim.
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=Foundation");
    // The shim is Objective-C++ (.mm), so the C++ runtime must be linked for the C++ ABI
    // symbols (e.g. __gxx_personality_v0); rustc links with -nodefaultlibs and won't add it.
    println!("cargo:rustc-link-lib=c++");
}
