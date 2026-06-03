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
    w.write_all(&[tag])?;
    w.write_all(&(payload.len() as u32).to_be_bytes())?;
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
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
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
}
