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

/// Largest single read issued to the accessory fd. Sourced from the shared
/// [`protocol::BULK_TRANSFER_SIZE`] — the SAME constant the host sizes its bulk transfers to —
/// so the device read can never end up smaller than a host packet (which would truncate it).
/// See the read impl and `protocol::BULK_TRANSFER_SIZE` for why this is load-bearing.
#[cfg(target_os = "android")]
const ACCESSORY_READ_CHUNK: usize = protocol::BULK_TRANSFER_SIZE;

/// The buffered-read algorithm behind [`AccessoryFdTransport::read`], factored out so it can be
/// unit-tested on any platform (the `cfg(target_os = "android")` transport that uses it is never
/// compiled on CI, but the bug-prone buffering logic IS — see the tests). When `rbuf` is
/// drained it pulls up to `chunk` bytes from `inner` in ONE read, then serves `buf` from `rbuf`.
///
/// `chunk` MUST be ≥ the peer's max single transfer: on the Android `f_accessory` gadget the
/// size passed to the underlying `read()` bounds the USB OUT request, so a small codec read
/// issued directly on the fd would truncate the host's larger packet. Buffering decouples the
/// codec's small `read_exact`s from the wire's transfer granularity (see the type doc).
///
/// Cfg-gated to `android` (the real user) plus `test` (the unit tests) so a default non-android
/// build doesn't flag it as dead code.
#[cfg(any(target_os = "android", test))]
fn chunked_read<R: Read>(
    inner: &mut R,
    rbuf: &mut Vec<u8>,
    rpos: &mut usize,
    chunk: usize,
    buf: &mut [u8],
) -> io::Result<usize> {
    // Per the `Read` contract, a zero-length target returns immediately — and crucially WITHOUT
    // triggering a refill, so an empty read can never be mistaken for (or cause) EOF.
    if buf.is_empty() {
        return Ok(0);
    }
    // Refill with ONE large read when our buffer is drained, so the peer's transfer fits whole.
    if *rpos >= rbuf.len() {
        rbuf.resize(chunk, 0);
        let n = inner.read(rbuf)?;
        rbuf.truncate(n);
        *rpos = 0;
        if n == 0 {
            return Ok(0); // EOF — peer closed the connection
        }
    }
    let avail = &rbuf[*rpos..];
    let k = avail.len().min(buf.len());
    buf[..k].copy_from_slice(&avail[..k]);
    *rpos += k;
    Ok(k)
}

/// Wraps the bidirectional accessory file descriptor as a `Read + Write` transport.
///
/// The fd was `detachFd()`-ed on the Kotlin side, relinquishing its ownership; this type
/// takes sole ownership via the single `unsafe { File::from_raw_fd }` in
/// [`AccessoryFdTransport::from_raw_fd`] and closes it on drop — no double-close
/// (single-ownership, threat T-P1-03). Reading receives the host's bulk-OUT, writing sends
/// to the host's bulk-IN, so one `File` is both reader and writer.
///
/// **Why reads are internally buffered (load-bearing for AOA):** on the Android
/// `f_accessory` gadget, the size passed to `read()` bounds the USB OUT request, so the
/// host's bulk packet must fit in ONE read or the excess is dropped on overflow. The
/// framing codec does small `read_exact`s (a 5-byte header, then the payload), so reading
/// the fd directly would queue a tiny OUT request and silently truncate every host packet —
/// the host's transfer still ACKs, but the device loses all but the first few bytes,
/// deadlocking the round-trip. So we always read a full [`ACCESSORY_READ_CHUNK`] from the fd
/// into `rbuf` and serve the codec's small reads from there.
#[cfg(target_os = "android")]
pub struct AccessoryFdTransport {
    file: std::fs::File,
    rbuf: Vec<u8>,
    rpos: usize,
}

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
        AccessoryFdTransport {
            file: std::fs::File::from_raw_fd(fd),
            rbuf: Vec::new(),
            rpos: 0,
        }
    }
}

#[cfg(target_os = "android")]
impl Read for AccessoryFdTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // All the buffering logic lives in the platform-independent `chunked_read` (tested on
        // CI); here we just bind it to the accessory `File` and the AOA transfer size.
        chunked_read(
            &mut self.file,
            &mut self.rbuf,
            &mut self.rpos,
            ACCESSORY_READ_CHUNK,
            buf,
        )
    }
}

#[cfg(target_os = "android")]
impl Write for AccessoryFdTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
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

    // ---- chunked_read: the platform-independent buffered-read algorithm behind
    // AccessoryFdTransport (which itself is cfg(android) and never built on CI). These tests
    // exercise the exact refill/partial-read/EOF logic that fixed the P1 host→device truncation
    // deadlock — previously untested, since the only caller was Android-only. ----

    /// A `Read + Write` adapter that routes reads through `chunked_read` over an inner
    /// `DuplexFake`, mirroring how `AccessoryFdTransport` wraps the accessory `File`.
    struct ChunkedFake {
        inner: DuplexFake,
        rbuf: Vec<u8>,
        rpos: usize,
        chunk: usize,
    }

    impl Read for ChunkedFake {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            chunked_read(
                &mut self.inner,
                &mut self.rbuf,
                &mut self.rpos,
                self.chunk,
                buf,
            )
        }
    }
    impl Write for ChunkedFake {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.inner.write(data)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    #[test]
    fn chunked_read_reassembles_frames_across_refills() {
        // The whole point of the buffer: the codec's small read_exact(5)+payload reads must be
        // served from large refills even when the underlying transport hands back tiny reads.
        // inner.max_read = 3 forces a 3-byte-at-a-time fd, while chunk = 16 KiB; echo_loop must
        // still reassemble and echo two whole frames byte-for-byte.
        let mut framed = Vec::new();
        write_frame(&mut framed, 1, &make_pattern(5000)).unwrap();
        write_frame(&mut framed, 2, &make_pattern(37)).unwrap();
        let mut fake = ChunkedFake {
            inner: DuplexFake {
                read_queue: framed.iter().copied().collect(),
                write_sink: Vec::new(),
                max_read: 3, // pathological: underlying reads return only 3 bytes each
            },
            rbuf: Vec::new(),
            rpos: 0,
            chunk: 16 * 1024,
        };
        let total = echo_loop(&mut fake).unwrap();
        assert_eq!(total, 5000 + 37);
        assert_eq!(
            fake.inner.write_sink, framed,
            "frames must echo byte-for-byte"
        );
    }

    #[test]
    fn chunked_read_eof_is_idempotent() {
        // A drained inner yields Ok(0); a second call must ALSO yield Ok(0) (not refill garbage
        // or loop) — the clean repeated-EOF behavior echo_loop relies on at a frame boundary.
        let mut inner = DuplexFake {
            read_queue: VecDeque::new(),
            write_sink: Vec::new(),
            max_read: 16,
        };
        let (mut rbuf, mut rpos) = (Vec::new(), 0usize);
        let mut buf = [0u8; 8];
        assert_eq!(
            chunked_read(&mut inner, &mut rbuf, &mut rpos, 64, &mut buf).unwrap(),
            0
        );
        assert_eq!(
            chunked_read(&mut inner, &mut rbuf, &mut rpos, 64, &mut buf).unwrap(),
            0
        );
    }

    #[test]
    fn chunked_read_empty_buf_returns_zero_without_refilling() {
        // A zero-length target must return Ok(0) immediately WITHOUT pulling from the inner
        // reader — otherwise an empty read could spuriously consume/buffer data or look like EOF.
        let mut inner = DuplexFake {
            read_queue: (0..10).collect(),
            write_sink: Vec::new(),
            max_read: 16,
        };
        let (mut rbuf, mut rpos) = (Vec::new(), 0usize);
        assert_eq!(
            chunked_read(&mut inner, &mut rbuf, &mut rpos, 64, &mut []).unwrap(),
            0
        );
        assert!(rbuf.is_empty(), "no refill should have happened");
        assert_eq!(inner.read_queue.len(), 10, "inner reader must be untouched");
    }

    #[test]
    fn echo_loop_propagates_non_eof_read_error() {
        // A hard read error (not a clean frame-boundary EOF) must surface to the caller, not be
        // swallowed as a clean shutdown.
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "boom"))
            }
        }
        impl Write for Failing {
            fn write(&mut self, d: &[u8]) -> io::Result<usize> {
                Ok(d.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let err = echo_loop(&mut Failing).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
}
