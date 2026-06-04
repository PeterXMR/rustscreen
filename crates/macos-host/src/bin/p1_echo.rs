//! P1 transport spike (hands-on-Mac, Task B3). Compiled ONLY under `--features live-usb`.
//!
//! Default path = AOA: enumerate → handshake (req 51/52/53) → re-enumerate → claim → echo a
//! 1 MiB pattern over the bulk endpoints, repeated to amortize per-transfer overhead, and
//! print sustained throughput (Mbit/s). `--ncm <host:port>` = the NCM/TCP fallback over a
//! `TcpStream`. Either way the SAME `echo_roundtrip` drives the transport — the D1 swap point.
//!
//! Build: `cargo build -p macos-host --features live-usb` → `target/debug/p1_echo`.
//! This binary is NOT run in CI; its live behavior is verified in the Task B3 checkpoint.

#[cfg(not(feature = "live-usb"))]
fn main() {
    eprintln!("p1_echo requires --features live-usb (macOS, hands-on). Rebuild with that feature.");
}

#[cfg(feature = "live-usb")]
fn main() -> std::io::Result<()> {
    use macos_host::aoa;
    use macos_host::transport::{echo_roundtrip, make_pattern, mbit_per_sec, Transport};
    use nusb::MaybeFuture;
    use std::time::Duration;

    let args: Vec<String> = std::env::args().collect();

    // Number of 1 MiB echoes; ≥ several MB amortizes per-transfer overhead (Pitfall 5).
    const ITERS: usize = 16;
    let pattern = make_pattern(1 << 20);

    let mut transport: Box<dyn Transport> = if args.get(1).map(String::as_str) == Some("--ncm") {
        let addr = args
            .get(2)
            .expect("usage: p1_echo --ncm <host:port>")
            .clone();
        // LO-01: this spike initializes no logger, so log::* macros are silent. Use
        // println!/eprintln! for visible diagnostics (matches the project spike convention).
        println!("NCM/TCP fallback: connecting to {addr}");
        Box::new(aoa::NcmTransport::connect(&addr)?)
    } else {
        println!("AOA path: finding candidate device");
        let candidate = aoa::find_candidate()?;
        let dev = candidate
            .open()
            .wait()
            .map_err(|e| std::io::Error::other(format!("open candidate: {e}")))?;
        // Handshake via DEVICE-LEVEL control transfers — do NOT claim interface 0 here.
        // macOS binds a class driver to the phone's interface 0 and rejects the claim with
        // kIOReturnExclusiveAccess; the AOA requests only need the default control endpoint.
        println!("sending AOA handshake (req 51/52/53) via control endpoint…");
        aoa::handshake(&dev)?;
        // Drop the pre-handshake handle; never reuse it across re-enumeration (Pitfall 2).
        drop(dev);
        println!("waiting for the device to re-enumerate in accessory mode…");
        let acc = aoa::reacquire(Duration::from_secs(5))?;
        Box::new(aoa::open_transport(&acc)?)
    };

    eprintln!("transport ready (accessory interface claimed — no sudo needed)");

    // Connect handshake: the DEVICE sends a one-frame hello the instant it opens the
    // accessory; block until we read it so we KNOW the app's reader is live before we write.
    // Without this, our first bulk-OUT write can land before the app has opened
    // /dev/usb_accessory and the gadget drops it — a startup-ordering deadlock observed live
    // on the Pixel 6a (host wrote ~2-4 s before the app opened; both sides then hung).
    // (NCM/TCP has no such race, but the hello is harmless there — the app sends it anyway.)
    eprintln!("waiting for device hello (app opened the accessory)…");
    let (_tag, hello) = macos_host::transport::recv_frame(transport.as_mut())?;
    eprintln!(
        "device hello received ({} bytes) — app reader is live",
        hello.len()
    );

    // Warm-up: prove a small frame round-trips both directions before the 1 MiB stress test.
    // A failure here localizes to the transport handshake rather than the bulk loop.
    use macos_host::transport::{recv_frame, send_frame, ECHO_TAG};
    let warm = make_pattern(32);
    send_frame(transport.as_mut(), ECHO_TAG, &warm)?;
    let (_t, got) = recv_frame(transport.as_mut())?;
    if got != warm {
        return Err(std::io::Error::other("warm-up echo mismatch"));
    }
    println!("warm-up OK: 32B frame echoed byte-for-byte");

    let mut total_bytes = 0usize;
    let mut total_elapsed = Duration::ZERO;
    for i in 0..ITERS {
        let stats = echo_roundtrip(transport.as_mut(), &pattern).inspect_err(|e| {
            eprintln!("echo {i} FAILED: {e}");
        })?;
        total_bytes += stats.bytes;
        total_elapsed += stats.elapsed;
        println!("echo {i}: {} bytes ok", stats.bytes);
    }

    let mbit = mbit_per_sec(total_bytes, total_elapsed);
    println!(
        "P1 echo OK: {} MiB byte-for-byte over {} iters; throughput {:.1} Mbit/s",
        total_bytes / (1 << 20),
        ITERS,
        mbit
    );
    Ok(())
}
