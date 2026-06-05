//! Length-prefixed framing for the Mac↔Pixel link.
//!
//! Every message on the wire is a self-delimiting frame so the reader can recover
//! message boundaries from a byte stream (USB bulk / socket). Wire format (roadmap
//! §301):
//!
//! ```text
//! ┌──────┬───────────────┬───────────────────┐
//! │ tag  │ len (u32 BE)  │ payload[len]      │
//! │ 1 B  │ 4 B           │ len bytes         │
//! └──────┴───────────────┴───────────────────┘
//! ```
//!
//! The `tag` identifies the message kind (Handshake / VideoConfig / Video / Touch /
//! Control — defined later in `messages.rs`); this module only moves opaque tagged
//! byte payloads, keeping framing independent of payload encoding (`postcard` for
//! control messages, raw bytes for video — no serde over big buffers).

use std::io::{self, Read, Write};

/// Largest payload accepted in a single frame. Guards [`read_frame`] against a
/// corrupt or hostile length prefix triggering an unbounded allocation. 16 MiB
/// comfortably exceeds a 1080p H.264 keyframe.
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;

/// Upper bound on the buffer [`read_frame`] fills in a single `read` call when
/// pulling in a payload. The payload is read incrementally in chunks of at most this
/// size so a corrupt or hostile length prefix can only force an up-front allocation
/// bounded by this constant — never the full declared length (up to
/// [`MAX_FRAME_LEN`] = 16 MiB). The growable buffer still grows to the *actual*
/// payload length as bytes genuinely arrive.
pub const READ_CHUNK_LEN: usize = 64 * 1024;

/// Write one framed message: `tag`, then the payload length as a big-endian `u32`,
/// then `payload`. Returns [`io::ErrorKind::InvalidInput`] if `payload` is longer
/// than [`MAX_FRAME_LEN`].
pub fn write_frame(w: &mut dyn Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() as u64 > MAX_FRAME_LEN as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame payload exceeds MAX_FRAME_LEN",
        ));
    }
    // Assemble the 5-byte header (tag · u32 BE len) and write it in ONE `write_all`,
    // mirroring `read_frame`'s single 5-byte header read. Three separate writes risked an
    // orphaned partial header on a mid-write transport error (e.g. a full USB bulk
    // endpoint): the reader would then resync onto garbage and the whole session desyncs
    // with no way to recover short of a reconnect. A single header write makes the header
    // atomic w.r.t. that failure mode.
    let len = payload.len() as u32; // safe: guarded by the MAX_FRAME_LEN check above
    let mut header = [0u8; 5];
    header[0] = tag;
    header[1..5].copy_from_slice(&len.to_be_bytes());
    w.write_all(&header)?;
    w.write_all(payload)?;
    Ok(())
}

/// Read one framed message, returning `(tag, payload)`. Returns an error on EOF
/// (including a truncated header or payload) or if the declared length exceeds
/// [`MAX_FRAME_LEN`].
pub fn read_frame(r: &mut dyn Read) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    r.read_exact(&mut header)?;
    let tag = header[0];
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame length exceeds MAX_FRAME_LEN",
        ));
    }
    // Read the payload INCREMENTALLY in chunks of at most READ_CHUNK_LEN rather than
    // allocating `len` bytes up front. This bounds the largest single allocation/read
    // by the chunk size, so a hostile peer declaring (a valid-but-large) `len` cannot
    // force a multi-MiB allocation before a single byte of payload has been verified to
    // exist. The buffer still grows to the true payload size as bytes actually arrive.
    let len = len as usize;
    let mut payload = Vec::new();
    let mut remaining = len;
    while remaining > 0 {
        let want = remaining.min(READ_CHUNK_LEN);
        let start = payload.len();
        payload.resize(start + want, 0);
        // Fill exactly `want` bytes; a short stream (declared `len` but fewer bytes
        // actually delivered) surfaces here as UnexpectedEof — same clean error the
        // previous single `read_exact` produced for a truncated payload.
        r.read_exact(&mut payload[start..start + want])?;
        remaining -= want;
    }
    Ok((tag, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trips_a_frame() {
        let mut buf = Vec::new();
        write_frame(&mut buf, 7, &[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();

        let mut cur = Cursor::new(buf);
        let (tag, payload) = read_frame(&mut cur).unwrap();
        assert_eq!(tag, 7);
        assert_eq!(payload, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn round_trips_empty_payload() {
        let mut buf = Vec::new();
        write_frame(&mut buf, 3, &[]).unwrap();

        let mut cur = Cursor::new(buf);
        let (tag, payload) = read_frame(&mut cur).unwrap();
        assert_eq!(tag, 3);
        assert!(payload.is_empty());
    }

    #[test]
    fn reads_multiple_frames_in_order() {
        let mut buf = Vec::new();
        write_frame(&mut buf, 1, b"first").unwrap();
        write_frame(&mut buf, 2, b"second").unwrap();

        let mut cur = Cursor::new(buf);
        let a = read_frame(&mut cur).unwrap();
        let b = read_frame(&mut cur).unwrap();
        assert_eq!(a, (1, b"first".to_vec()));
        assert_eq!(b, (2, b"second".to_vec()));
    }

    #[test]
    fn wire_layout_is_tag_then_be_len_then_payload() {
        let mut buf = Vec::new();
        write_frame(&mut buf, 0x09, &[0xAA, 0xBB]).unwrap();
        // tag, then u32 BE length = 2, then payload.
        assert_eq!(buf, vec![0x09, 0x00, 0x00, 0x00, 0x02, 0xAA, 0xBB]);
    }

    #[test]
    fn read_truncated_header_errors() {
        // Only 3 bytes — not even the 5-byte header is complete.
        let mut cur = Cursor::new(vec![0x01, 0x00, 0x00]);
        let err = read_frame(&mut cur).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_truncated_payload_errors() {
        // Header claims 4 payload bytes but only 2 follow.
        let mut cur = Cursor::new(vec![0x01, 0x00, 0x00, 0x00, 0x04, 0xAA, 0xBB]);
        let err = read_frame(&mut cur).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_rejects_length_over_max() {
        // Declared length = MAX_FRAME_LEN + 1, with no payload bytes present. Must
        // error on the length check, not attempt to read/allocate it.
        let over = MAX_FRAME_LEN + 1;
        let mut header = vec![0x01];
        header.extend_from_slice(&over.to_be_bytes());
        let mut cur = Cursor::new(header);
        let err = read_frame(&mut cur).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn write_rejects_payload_over_max() {
        let oversized = vec![0u8; MAX_FRAME_LEN as usize + 1];
        let mut buf = Vec::new();
        let err = write_frame(&mut buf, 1, &oversized).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(buf.is_empty(), "nothing should be written on rejection");
    }

    #[test]
    fn read_reads_payload_in_capped_chunks() {
        // The key DoS guarantee: a frame declaring a large payload must NOT cause a
        // single up-front allocation/read of the full declared length. We wrap the
        // payload bytes in a Read that records the largest single buffer it is asked
        // to fill, and assert that no individual read request exceeds READ_CHUNK_LEN.
        //
        // Declared len is comfortably larger than one chunk so a naive
        // `read_exact(&mut vec![0; len])` would ask for `len` bytes in one call and
        // trip the assertion.
        struct MaxBufRecorder<R> {
            inner: R,
            max_buf: usize,
        }
        impl<R: Read> Read for MaxBufRecorder<R> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.max_buf = self.max_buf.max(buf.len());
                // Serve at most one chunk per call so read_exact must loop — this also
                // exercises the short-read handling.
                let cap = buf.len().min(READ_CHUNK_LEN);
                self.inner.read(&mut buf[..cap])
            }
        }

        let payload_len = READ_CHUNK_LEN * 3 + 123;
        let payload: Vec<u8> = (0..payload_len).map(|i| i as u8).collect();
        let mut wire = Vec::new();
        write_frame(&mut wire, 0x42, &payload).unwrap();

        let mut recorder = MaxBufRecorder {
            inner: Cursor::new(wire),
            max_buf: 0,
        };
        let (tag, got) = read_frame(&mut recorder).unwrap();
        assert_eq!(tag, 0x42);
        assert_eq!(got, payload, "payload must round-trip intact");
        assert!(
            recorder.max_buf <= READ_CHUNK_LEN,
            "read_frame asked for {} bytes in one call; must never exceed the {}-byte chunk cap",
            recorder.max_buf,
            READ_CHUNK_LEN
        );
    }

    #[test]
    fn read_accepts_length_exactly_max() {
        // Companion to read_rejects_length_over_max (MAX+1): a frame declaring exactly
        // MAX_FRAME_LEN must be ACCEPTED. Pins the boundary as inclusive — the check is
        // `len > MAX_FRAME_LEN`, so a future `>=` slip would wrongly reject the largest valid
        // frame (a 1080p keyframe near the cap). Round-trips a real MAX-length payload.
        let payload = vec![0xABu8; MAX_FRAME_LEN as usize];
        let mut buf = Vec::new();
        write_frame(&mut buf, 5, &payload).unwrap();
        let mut cur = Cursor::new(buf);
        let (tag, got) = read_frame(&mut cur).unwrap();
        assert_eq!(tag, 5);
        assert_eq!(got.len(), MAX_FRAME_LEN as usize);
        // Assert CONTENTS too, not just length — a wrong-but-right-sized buffer must not slip.
        assert!(
            got.iter().all(|&b| b == 0xAB),
            "payload bytes must round-trip intact"
        );
    }
}
