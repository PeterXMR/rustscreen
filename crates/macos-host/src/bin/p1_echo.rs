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
        let iface = dev.claim_interface(0).wait().map_err(|e| {
            // ME-02: actionable message — macOS may need sudo or the device is in use.
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "claim candidate interface 0 failed ({e}). On macOS the device may be in \
                     use by another process or require elevated privileges — try `sudo`."
                ),
            )
        })?;
        aoa::handshake(&iface)?;
        // Drop the pre-handshake handle; never reuse it across re-enumeration (Pitfall 2).
        drop(iface);
        drop(dev);
        let acc = aoa::reacquire(Duration::from_secs(5))?;
        Box::new(aoa::open_transport(&acc)?)
    };

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
