//! P6 cursor-injection smoke bin (hands-on-Mac). Compiled ONLY under `--features
//! live-inject` (see `[[bin]] required-features` in Cargo.toml), so the default
//! `cargo build --workspace` never pulls `core-graphics`.
//!
//! NO phone, NO cable: it drives a scripted tap + drag through the SAME
//! `PointerStateMachine` that CI unit-tests, into the live `CgEventSink`, so you can
//! WATCH the macOS cursor jump and drag on the main display. This is the manual proof of
//! TOUCH-01 criteria #1/#2 (tap → click, drag → click-and-drag) and the
//! `AXIsProcessTrusted()` gate — the half that cannot run in CI.
//!
//! Build: `cargo build -p macos-host --features live-inject` → `target/debug/p6_inject`.
//! First run will fail with `NotTrusted` until you grant Accessibility permission to the
//! terminal/binary (System Settings ▸ Privacy & Security ▸ Accessibility), then re-run.
//!
//! The target rect comes from `CGDisplay::main().bounds()` — i.e. real `CGDisplayBounds`,
//! exactly the source the production adapter will use (here: the MAIN display; once the
//! live pipeline exists it becomes the virtual display's id).

use std::thread::sleep;
use std::time::Duration;

use core_graphics::display::CGDisplay;
use macos_host::touch::{CgEventSink, DisplayRect, PointerSink, PointerStateMachine};
use protocol::messages::{TouchEvent, TouchPhase};

fn main() {
    // Real CGDisplayBounds for the main display → the global-coordinate rect we map into.
    let bounds = CGDisplay::main().bounds();
    let rect = DisplayRect {
        x: bounds.origin.x,
        y: bounds.origin.y,
        w: bounds.size.width,
        h: bounds.size.height,
    };
    println!(
        "p6_inject: main display rect = {{ x: {}, y: {}, w: {}, h: {} }}",
        rect.x, rect.y, rect.w, rect.h
    );

    let mut sink = match CgEventSink::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "p6_inject: cannot inject events ({e:?}).\n\
                 Grant Accessibility permission, then re-run:\n  \
                 System Settings ▸ Privacy & Security ▸ Accessibility ▸ enable your \
                 terminal (or the p6_inject binary)."
            );
            std::process::exit(1);
        }
    };

    // A scripted single-pointer gesture stream (normalized, top-left origin):
    //   1) a TAP at the center (Down→Up at one point) → a click,
    //   2) a DRAG from the upper-left quadrant to the lower-right (Down→Move*→Up).
    let script = [
        (1u32, TouchPhase::Down, 0.5, 0.5),
        (1, TouchPhase::Up, 0.5, 0.5),
        (1, TouchPhase::Down, 0.25, 0.25),
        (1, TouchPhase::Move, 0.40, 0.40),
        (1, TouchPhase::Move, 0.55, 0.55),
        (1, TouchPhase::Move, 0.75, 0.75),
        (1, TouchPhase::Up, 0.75, 0.75),
    ];

    let mut sm = PointerStateMachine::default();
    for (pointer_id, phase, nx, ny) in script {
        let ev = TouchEvent {
            pointer_id,
            phase,
            nx,
            ny,
        };
        match sm.step(&ev, rect) {
            Ok(Some(action)) => {
                println!("{action:?}");
                sink.dispatch(action);
            }
            Ok(None) => {}
            Err(e) => eprintln!("p6_inject: map error: {e:?}"),
        }
        // Pause so the motion is visible to the eye (and the OS coalesces less).
        sleep(Duration::from_millis(400));
    }

    println!("p6_inject: done — the cursor should have clicked at center, then dragged ↘.");
}
