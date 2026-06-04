//! The `Transport` seam (D1 swap point) plus the pure, cable-free echo/verify/throughput
//! logic that proves XPORT-01 without any hardware.
//!
//! `Transport` is deliberately **just** `Read + Write + Send` via a blanket impl — no
//! `send`/`recv` methods. `protocol::framing::{write_frame, read_frame}` already operate
//! over `&mut dyn Read`/`&mut dyn Write`, so every concrete transport (`nusb` bulk
//! endpoints, the Android accessory `File`, a `TcpStream`) qualifies with zero ceremony
//! and the framing codec rides this seam unchanged (CONTEXT D2, RESEARCH Pattern 4).
//!
//! The live AOA/NCM adapters live in [`crate::aoa`] (cfg-gated behind the `live-usb`
//! feature); the spike binary drives them through [`echo_roundtrip`] here.

use std::io::{self, Read, Write};
use std::time::Duration;

/// The transport seam: anything byte-stream that is `Read + Write + Send`. This IS the
/// D1 swap point — `nusb` endpoints, the Android accessory `File`, and `TcpStream` all
/// satisfy it via the blanket impl below, and `protocol::framing` reuses it directly.
pub trait Transport: Read + Write + Send {}

impl<T: Read + Write + Send> Transport for T {}

/// Frame tag used by the P1 echo spike. Opaque to the peer — it simply echoes whatever tag
/// it reads — but defined once here so host and phone agree on a value for diagnostics.
pub const ECHO_TAG: u8 = 1;

/// Deterministic 1 MiB-style test pattern: byte `i` is `(i % 256) as u8`. A mismatch on
/// echo therefore points straight at the first corrupted offset (via [`first_mismatch`]).
pub fn make_pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 256) as u8).collect()
}

/// The first index at which `a` and `b` differ, or — if every shared byte matches but the
/// lengths differ — `Some(min_len)`. `None` only when the two slices are byte-for-byte equal.
pub fn first_mismatch(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .or_else(|| (a.len() != b.len()).then_some(a.len().min(b.len())))
}

/// Result of one echo round-trip: how many bytes were echoed and how long the transfer
/// span took (timing wraps only the transfer, not handshake/setup — RESEARCH Pitfall 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoStats {
    pub bytes: usize,
    pub elapsed: Duration,
}

/// Write `pattern` as one length-prefixed frame, read one complete frame back, and assert
/// the echoed payload matches `pattern` byte-for-byte.
///
/// **Why this is correct single-threaded** (BL-01/BL-02): the frame carries an explicit
/// `u32` length prefix, and the peer ([`android-client::transport::echo_loop`]) drains the
/// ENTIRE frame before it echoes a single byte back. The write phase and the read phase
/// therefore never overlap on the wire, so a real finite-buffer duplex pipe (USB bulk, a
/// socket) cannot deadlock — neither side is ever simultaneously blocked writing while the
/// peer is blocked writing. The old design streamed raw bytes and relied on the peer
/// echoing mid-stream (and ultimately on EOF), which deadlocks the instant both kernel
/// pipe buffers fill. The length prefix also removes the EOF-mid-stream dependency: the
/// reader knows exactly how many bytes to expect.
///
/// `read_frame` allocates to the WIRE-supplied length, but that length is bounded by
/// [`protocol::framing::MAX_FRAME_LEN`] (DoS guard T-P1-02). On any content/length
/// divergence returns [`io::ErrorKind::InvalidData`] naming the first differing offset.
/// Timing covers only the `write_frame` + `read_frame` span.
pub fn echo_roundtrip<T: Transport + ?Sized>(t: &mut T, pattern: &[u8]) -> io::Result<EchoStats> {
    use protocol::framing::{read_frame, write_frame};
    let start = std::time::Instant::now();
    // `framing::{write_frame, read_frame}` take `&mut dyn Write`/`&mut dyn Read`. A possibly
    // `?Sized` `&mut T` can't be unsize-coerced to a fresh trait object, so wrap it in a
    // thin `Sized` adapter that forwards `Read`/`Write` and hand THAT to the codec.
    let mut framed = ByRef(t);
    write_frame(&mut framed, ECHO_TAG, pattern)?;
    // Flush: nusb's EndpointWrite buffers, so a trailing partial (<16 KiB) chunk could sit
    // in the host's userspace buffer and the peer's read_frame would block forever waiting
    // for it (C-01). No-op for TcpStream/loopback; essential for the real AOA bulk path.
    framed.flush()?;
    let (_tag, got) = read_frame(&mut framed)?; // read_frame loops over partial reads (Pitfall 3)
    let elapsed = start.elapsed();
    if let Some(offset) = first_mismatch(pattern, &got) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("echo mismatch at byte offset {offset}"),
        ));
    }
    Ok(EchoStats {
        bytes: pattern.len(),
        elapsed,
    })
}

/// A `Sized` reborrow adapter so a `&mut T` (with `T: ?Sized`) can be passed to the
/// `&mut dyn Read`/`&mut dyn Write` framing codec. Forwards every call to the inner ref.
struct ByRef<'a, T: Read + Write + ?Sized>(&'a mut T);

impl<T: Read + Write + ?Sized> Read for ByRef<'_, T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl<T: Read + Write + ?Sized> Write for ByRef<'_, T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Throughput in megabits per second: `bytes * 8 / 1e6 / seconds`.
pub fn mbit_per_sec(bytes: usize, elapsed: Duration) -> f64 {
    (bytes as f64 * 8.0) / 1_000_000.0 / elapsed.as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// In-memory transport: bytes written are queued and returned on read. Models a perfect
    /// echo loop with no hardware.
    #[derive(Default)]
    struct LoopbackTransport {
        buf: VecDeque<u8>,
    }

    impl Write for LoopbackTransport {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf.extend(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for LoopbackTransport {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = out.len().min(self.buf.len());
            for slot in out.iter_mut().take(n) {
                *slot = self.buf.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    /// Like [`LoopbackTransport`] but `read` yields at most 16 KiB per call, forcing the
    /// partial-read reconciliation path (RESEARCH Pitfall 3 / the AOA 16 KiB buffer).
    #[derive(Default)]
    struct ChunkedLoopback {
        buf: VecDeque<u8>,
    }

    const CHUNK: usize = 16 * 1024;

    impl Write for ChunkedLoopback {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf.extend(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for ChunkedLoopback {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = out.len().min(self.buf.len()).min(CHUNK);
            for slot in out.iter_mut().take(n) {
                *slot = self.buf.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    /// A loopback that flips one byte of the echoed *payload*, to prove `echo_roundtrip`
    /// surfaces corruption. The flip targets a byte well past the 5-byte frame header so it
    /// lands in the payload (not the tag/length), exercising the payload-mismatch path.
    #[derive(Default)]
    struct CorruptingLoopback {
        buf: VecDeque<u8>,
        read_so_far: usize,
        flipped: bool,
    }

    impl Write for CorruptingLoopback {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf.extend(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for CorruptingLoopback {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = out.len().min(self.buf.len());
            for slot in out.iter_mut().take(n) {
                *slot = self.buf.pop_front().unwrap();
            }
            // Flip the first payload byte (frame layout: 1-byte tag + 4-byte BE len + payload,
            // so absolute offset 5). Only flip once, when this read spans that offset.
            const PAYLOAD_START: usize = 5;
            if !self.flipped && n > 0 {
                let span = self.read_so_far..self.read_so_far + n;
                if span.contains(&PAYLOAD_START) {
                    out[PAYLOAD_START - self.read_so_far] ^= 0xFF;
                    self.flipped = true;
                }
            }
            self.read_so_far += n;
            Ok(n)
        }
    }

    #[test]
    fn make_pattern_is_index_mod_256() {
        let p = make_pattern(1 << 20);
        assert_eq!(p.len(), 1 << 20);
        assert_eq!(p[0], 0);
        assert_eq!(p[255], 255);
        assert_eq!(p[256], 0);
        assert_eq!(p[257], 1);
    }

    #[test]
    fn first_mismatch_finds_offset() {
        assert_eq!(first_mismatch(b"abcd", b"abxd"), Some(2));
    }

    #[test]
    fn first_mismatch_reports_length_diff() {
        assert_eq!(first_mismatch(b"abc", b"abcd"), Some(3));
        assert_eq!(first_mismatch(b"abcd", b"abc"), Some(3));
    }

    #[test]
    fn first_mismatch_none_when_equal() {
        assert_eq!(first_mismatch(b"abcd", b"abcd"), None);
    }

    #[test]
    fn loopback_echo_roundtrips_1mib() {
        let mut t = LoopbackTransport::default();
        let p = make_pattern(1 << 20);
        let stats = echo_roundtrip(&mut t, &p).unwrap();
        assert_eq!(stats.bytes, 1 << 20);
    }

    #[test]
    fn echo_roundtrip_detects_corruption() {
        let mut t = CorruptingLoopback::default();
        let p = make_pattern(1 << 20);
        let err = echo_roundtrip(&mut t, &p).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn one_mib_in_40ms_is_about_209_mbit() {
        let v = mbit_per_sec(1 << 20, Duration::from_millis(40));
        assert!((v - 209.7).abs() < 1.0, "got {v}");
    }

    #[test]
    fn chunked_loopback_roundtrips_1mib() {
        let mut t = ChunkedLoopback::default();
        let p = make_pattern(1 << 20);
        let stats = echo_roundtrip(&mut t, &p).unwrap();
        assert_eq!(stats.bytes, 1 << 20);
        // The chunked reader never returns more than 16 KiB per call, yet read_exact
        // reconciles the full 1 MiB.
    }

    #[test]
    fn tcp_loopback_roundtrips_1mib_without_deadlock() {
        // The KEY regression guard for BL-01/BL-02. A real TCP socket has a FINITE kernel
        // send buffer, so the old raw-streaming echo (write the whole 1 MiB, then read it
        // back over the same duplex pipe with the peer echoing mid-stream) would DEADLOCK
        // here: both ends block on `write` once their send buffers fill, neither drains the
        // other. This test COMPLETING proves the length-framed design fixed it — the phone
        // peer (`echo_loop`) drains the entire frame before echoing, so the two directions
        // never overlap on the wire.
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");

        // Server thread: accept one connection, run the framed echo loop until the client
        // drops (clean EOF), then join. This mirrors android-client::transport::echo_loop.
        let server = std::thread::spawn(move || {
            let (mut sock, _peer) = listener.accept().expect("accept");
            use protocol::framing::{read_frame, write_frame};
            loop {
                match read_frame(&mut sock) {
                    Ok((tag, payload)) => {
                        write_frame(&mut sock, tag, &payload).expect("server write_frame");
                    }
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                    Err(e) => panic!("server read_frame: {e}"),
                }
            }
        });

        let mut client: TcpStream = TcpStream::connect(addr).expect("connect");
        let pattern = make_pattern(1 << 20); // 1 MiB — comfortably exceeds any pipe buffer.
        let stats = echo_roundtrip(&mut client, &pattern).expect("echo_roundtrip over TCP");
        assert_eq!(stats.bytes, 1 << 20);

        // Drop the client so the server sees EOF and the loop terminates cleanly.
        drop(client);
        server.join().expect("server thread join");
        // Reaching here at all is the proof: the old design would have hung this test.
    }

    #[test]
    fn framing_round_trips_over_transport() {
        // Regression: protocol::framing must stay framing-compatible over a Transport
        // (the seam P5 rides). Do NOT modify protocol::framing.
        use protocol::framing::{read_frame, write_frame};
        let mut t = LoopbackTransport::default();
        let payload = make_pattern(4096);
        write_frame(&mut t, 7, &payload).unwrap();
        let (tag, got) = read_frame(&mut t).unwrap();
        assert_eq!(tag, 7);
        assert_eq!(got, payload);
    }
}
