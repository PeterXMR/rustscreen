//! Android-side transport: the platform-agnostic [`echo_loop`] core (read one complete
//! length-framed message, write it straight back, until EOF) plus the
//! `cfg(target_os = "android")` [`AccessoryFdTransport`] that wraps the raw accessory fd
//! handed down from Kotlin.
//!
//! [`echo_loop`] is deliberately NOT cfg-gated so it is exercised on host CI against an
//! in-memory fake — it is the Wave-A-tested core (Architectural Responsibility Map: echo
//! belongs in the Rust core, behind the `Transport` seam). The `nativeOnUsbFd` JNI entry
//! (in `lib.rs`) is just the fd→Rust seam that drives it on the phone.

use std::io::{self, Read, Write};

use protocol::framing::{read_frame, write_frame};

/// Read one complete length-framed message and write it straight back, repeating until the
/// host drops the connection (a clean `UnexpectedEof` at a frame boundary). Returns the
/// total number of payload bytes echoed.
///
/// **Why this is deadlock-free** (BL-01/BL-02): each iteration `read_frame` drains the
/// ENTIRE frame (it knows the exact length from the `u32` prefix) BEFORE `write_frame`
/// echoes anything back. The read phase and write phase never overlap, so the host's
/// [`echo_roundtrip`] (write whole frame, then read whole frame) interlocks cleanly over a
/// finite-buffer duplex pipe — there is no point at which both ends are blocked writing.
/// The old design read a 16 KiB chunk and echoed it mid-stream, relying on a 0-byte EOF
/// read to stop; over a real socket that deadlocks once both pipe buffers fill.
///
/// `read_frame`/`write_frame` are reused verbatim from `protocol::framing` — the same codec
/// the host uses and the seam P5 rides — so host and phone are guaranteed wire-compatible.
pub fn echo_loop<T: Read + Write>(t: &mut T) -> io::Result<u64> {
    let mut total: u64 = 0;
    let mut n: u32 = 0;
    loop {
        log::info!("echo_loop: waiting for frame #{n} (blocking read)…");
        match read_frame(t) {
            Ok((tag, payload)) => {
                log::info!(
                    "echo_loop: READ frame #{n} tag={tag} {} bytes — echoing back",
                    payload.len()
                );
                write_frame(t, tag, &payload)?;
                log::info!("echo_loop: WROTE frame #{n} back ({} bytes)", payload.len());
                total += payload.len() as u64;
                n += 1;
            }
            // Host closed the connection at a frame boundary — clean shutdown.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                log::info!("echo_loop: EOF at frame boundary — {total} bytes echoed total");
                return Ok(total);
            }
            Err(e) => {
                log::warn!("echo_loop: read error: {e}");
                return Err(e);
            }
        }
    }
}

/// Wraps the bidirectional accessory file descriptor as a `Read + Write` transport.
///
/// The fd was `detachFd()`-ed on the Kotlin side, relinquishing its ownership; this type
/// takes sole ownership via the single `unsafe { File::from_raw_fd }` in
/// [`AccessoryFdTransport::from_raw_fd`] and closes it on drop — no double-close
/// (single-ownership, threat T-P1-03). Reading receives the host's bulk-OUT, writing sends
/// to the host's bulk-IN, so one `File` is both reader and writer.
#[cfg(target_os = "android")]
pub struct AccessoryFdTransport(pub std::fs::File);

#[cfg(target_os = "android")]
impl AccessoryFdTransport {
    /// Take ownership of an already-detached raw fd. The single `unsafe` is localized here;
    /// callers must pass an fd they own exactly once (Kotlin `detachFd()` guarantees this).
    ///
    /// # Safety
    /// `fd` must be a valid, open file descriptor whose ownership has been transferred to this
    /// call (e.g. via `ParcelFileDescriptor.detachFd()`); it must not be closed elsewhere.
    pub unsafe fn from_raw_fd(fd: std::os::fd::RawFd) -> Self {
        use std::os::fd::FromRawFd;
        AccessoryFdTransport(std::fs::File::from_raw_fd(fd))
    }
}

#[cfg(target_os = "android")]
impl Read for AccessoryFdTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

#[cfg(target_os = "android")]
impl Write for AccessoryFdTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::net::{TcpListener, TcpStream};

    /// In-memory duplex fake: a pre-loaded read queue (the host's bulk-OUT, holding framed
    /// bytes) and a separate write sink (the frames we echo back). EOF is a 0-byte read once
    /// the queue drains, which surfaces as `UnexpectedEof` at a frame boundary inside
    /// `read_frame` — the clean-shutdown signal `echo_loop` waits for.
    struct DuplexFake {
        read_queue: VecDeque<u8>,
        write_sink: Vec<u8>,
        max_read: usize,
    }

    impl Read for DuplexFake {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = out.len().min(self.read_queue.len()).min(self.max_read);
            for slot in out.iter_mut().take(n) {
                *slot = self.read_queue.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    impl Write for DuplexFake {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.write_sink.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn make_pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 256) as u8).collect()
    }

    #[test]
    fn echo_loop_echoes_framed_messages_until_eof() {
        // The read side is pre-loaded with two complete frames delivered in ≤16 KiB chunks,
        // then EOF. echo_loop must echo BOTH frames byte-for-byte (header + payload) onto
        // the write side, then return the summed payload length.
        let p1 = make_pattern(1 << 20); // 1 MiB
        let p2 = make_pattern(4096);
        let mut framed = Vec::new();
        write_frame(&mut framed, 1, &p1).unwrap();
        write_frame(&mut framed, 2, &p2).unwrap();

        let mut fake = DuplexFake {
            read_queue: framed.iter().copied().collect(),
            write_sink: Vec::new(),
            max_read: 16 * 1024,
        };
        let total = echo_loop(&mut fake).unwrap();
        assert_eq!(total, (1 << 20) + 4096);
        // The echoed bytes must be exactly the frames we fed in (perfect echo).
        assert_eq!(fake.write_sink, framed);
    }

    #[test]
    fn echo_loop_returns_zero_on_immediate_eof() {
        let mut fake = DuplexFake {
            read_queue: VecDeque::new(),
            write_sink: Vec::new(),
            max_read: 16 * 1024,
        };
        assert_eq!(echo_loop(&mut fake).unwrap(), 0);
        assert!(fake.write_sink.is_empty());
    }

    #[test]
    fn echo_loop_over_real_tcp_socket_no_deadlock() {
        // Companion to the host-side TCP regression: prove the phone's framed echo_loop
        // interlocks with a real finite-buffer socket without deadlocking. A peer thread
        // connects, writes one 1 MiB frame, reads the echoed frame back, asserts equality,
        // then drops the socket so echo_loop sees EOF and returns.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let peer = std::thread::spawn(move || {
            let mut sock = TcpStream::connect(addr).expect("connect");
            let payload = make_pattern(1 << 20);
            write_frame(&mut sock, 7, &payload).expect("peer write_frame");
            let (tag, echoed) = read_frame(&mut sock).expect("peer read_frame");
            assert_eq!(tag, 7);
            assert_eq!(echoed, payload);
            drop(sock); // signal EOF to echo_loop
        });

        let (mut server, _peer_addr) = listener.accept().expect("accept");
        let total = echo_loop(&mut server).expect("echo_loop over TCP");
        assert_eq!(total, (1 << 20) as u64);
        peer.join().expect("peer join");
    }
}
