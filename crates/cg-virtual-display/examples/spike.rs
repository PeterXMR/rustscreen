//! P2 keystone spike: create a virtual display from Rust and observe that the macOS window
//! server registers it (active display count goes up, a non-zero CGDirectDisplayID is
//! returned), then tear it down.
//!
//! Run with:  cargo run -p cg-virtual-display --example spike

use std::thread::sleep;
use std::time::Duration;

use cg_virtual_display::{active_display_count, VirtualDisplay};

fn main() {
    let before = active_display_count();
    println!("active displays before: {before}");

    let display = match VirtualDisplay::new(2400, 1080, 60.0) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("SPIKE FAILED: {e}");
            std::process::exit(1);
        }
    };

    let id = display.display_id();
    // Give the window server a moment to register the new display.
    sleep(Duration::from_millis(750));
    let during = active_display_count();

    println!("created virtual display: CGDirectDisplayID = {id}");
    println!("active displays during:  {during}");

    let ok = id != 0 && during > before;
    println!("holding the display for 3s (check System Settings ▸ Displays)…");
    sleep(Duration::from_secs(3));

    drop(display);
    sleep(Duration::from_millis(750));
    let after = active_display_count();
    println!("active displays after drop: {after}");

    if ok {
        println!(
            "SPIKE PASSED: virtual display created from Rust, id={id}, count {before}->{during}"
        );
    } else {
        eprintln!(
            "SPIKE INCONCLUSIVE: id={id}, count {before}->{during}->{after} (expected id!=0 and count increase)"
        );
        std::process::exit(2);
    }
}
