//! VideoToolbox encode adapter (hands-on-Mac, Wave B) **plus** its pure, cable-free
//! support logic: AVCC→Annex-B conversion and on-keyframe SPS/PPS in-band injection.
//!
//! VideoToolbox emits H.264 in AVCC framing (each NAL unit prefixed by a big-endian
//! length whose width — 1/2/4 bytes — comes from the `avcC` `lengthSizeMinusOne`) and
//! keeps SPS/PPS out-of-band in the format description. The
//! rest of RustScreen (`protocol::nal`, the on-disk `.h264`, and the P5 wire format)
//! is Annex-B start-code framed with SPS/PPS carried in-band on keyframes. The two
//! pure functions here bridge that gap (D3) and are unit-tested without any hardware:
//!
//! - [`avcc_to_annex_b`] — rewrite length-prefixed AVCC NAL units as start-code
//!   (`00 00 00 01`) prefixed Annex-B NAL units.
//! - [`to_annex_b_frame`] — convert one AVCC picture access unit and, on keyframes,
//!   prepend the out-of-band SPS and PPS so the resulting stream is self-describing
//!   (`protocol::nal::extract_codec_config` can recover them).
//!
//! Implements RESEARCH Pattern 3; every length prefix is bounds-checked (ASVS V5,
//! threat T-P3-01) so a malformed/truncated buffer can never overrun or panic.
//!
//! The VideoToolbox `Encoder` adapter (`VtEncoder`) is added in Wave B (Task B2) and
//! will live in this same file behind
//! `#[cfg(all(target_os = "macos", feature = "live-capture"))]`. See the placeholder
//! section at the bottom of this file. No macOS / objc2 / videotoolbox crate is
//! imported by the compiled (non-cfg-gated) code, so Wave A CI stays clean and
//! cross-platform.

/// The 4-byte Annex-B start code RustScreen emits in front of every NAL unit.
const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Convert a length-prefixed AVCC byte buffer into Annex-B start-code framing.
///
/// AVCC frames each NAL unit as a big-endian length prefix followed by that many bytes
/// of NAL data. The prefix width is `nal_length_size` bytes — `lengthSizeMinusOne + 1`
/// from the `avcC` configuration record, legally **1, 2, or 4** (VideoToolbox commonly
/// emits 4, but it must not be assumed; the Wave B adapter reads it from the format
/// description and passes it here). This rewrites each unit as `00 00 00 01` + NAL data,
/// concatenated in order, yielding a buffer `protocol::nal` can parse.
///
/// Robustness: a prefix claiming more bytes than remain (truncated/malformed stream)
/// stops conversion without reading out of bounds (ASVS V5, threat T-P3-01); a stray
/// zero-length unit is skipped rather than aborting the remaining buffer; an empty
/// input yields an empty output.
///
/// `nal_length_size` must be in `1..=4`; other values are a caller bug (debug-asserted).
pub fn avcc_to_annex_b(avcc: &[u8], nal_length_size: usize) -> Vec<u8> {
    debug_assert!(
        (1..=4).contains(&nal_length_size),
        "AVCC nal_length_size must be 1..=4 (from avcC lengthSizeMinusOne), got {nal_length_size}"
    );
    let mut out = Vec::with_capacity(avcc.len());
    let mut i = 0usize;
    while i + nal_length_size <= avcc.len() {
        // Read the big-endian length prefix of `nal_length_size` bytes.
        let mut len = 0usize;
        for k in 0..nal_length_size {
            len = (len << 8) | avcc[i + k] as usize;
        }
        i += nal_length_size;
        if len == 0 {
            continue; // stray zero-length unit — skip it, don't abort the buffer
        }
        // Bounds guard: a length claiming more than remains means a malformed or
        // truncated buffer — stop without overrunning (T-P3-01 / ASVS V5).
        if i + len > avcc.len() {
            break;
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&avcc[i..i + len]);
        i += len;
    }
    out
}

/// Convert one AVCC picture access unit to Annex-B and, on keyframes, inject the
/// out-of-band SPS and PPS in-band ahead of the picture NAL units.
///
/// **Contract:** the caller passes the raw AVCC picture payload (which carries no
/// parameter sets in-band — VideoToolbox keeps SPS/PPS in the format description) plus
/// the SPS and PPS NAL bytes (without start codes) obtained out-of-band. When
/// `is_keyframe` is true this prepends `00 00 00 01` + SPS, then `00 00 00 01` + PPS,
/// then the converted picture; when false it returns just the converted picture. The
/// function always prepends on keyframes (it does not attempt to detect parameter
/// sets already present in the AVCC payload) — keeping the picture stream
/// parameter-set-free upstream is the caller's responsibility.
///
/// **Precondition (enforced in Wave B):** VideoToolbox must NOT be configured to emit
/// parameter sets in-band; otherwise keyframes would carry duplicate SPS/PPS. The Wave
/// B adapter takes SPS/PPS from the out-of-band format description and feeds only the
/// picture payload here.
pub fn to_annex_b_frame(
    avcc: &[u8],
    nal_length_size: usize,
    is_keyframe: bool,
    sps: &[u8],
    pps: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    if is_keyframe {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(sps);
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(pps);
    }
    out.extend_from_slice(&avcc_to_annex_b(avcc, nal_length_size));
    out
}

// =============================================================================
// Wave B (Task B2) placeholder — VideoToolbox `Encoder` adapter.
//
// The `VtEncoder` struct implementing `crate::encode::Encoder` lands here behind
// `#[cfg(all(target_os = "macos", feature = "live-capture"))]`, alongside the pure
// converter above (which it calls to turn VideoToolbox's AVCC output into Annex-B and
// to inject SPS/PPS from the format description on keyframes). NO macOS / videotoolbox
// imports are added until that gated task — this keeps Wave A CI cross-platform and
// dependency-free.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::nal;

    /// Build an AVCC unit: 4-byte big-endian length prefix followed by `nal`.
    fn avcc_unit(nal: &[u8]) -> Vec<u8> {
        let mut v = (nal.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(nal);
        v
    }

    // --- avcc_to_annex_b ------------------------------------------------------

    #[test]
    fn avcc_empty_input_yields_empty_output() {
        assert!(avcc_to_annex_b(&[], 4).is_empty());
    }

    #[test]
    fn avcc_single_nal_gets_four_byte_start_code() {
        // [00 00 00 05][67 42 1F 00 01] -> [00 00 00 01][67 42 1F 00 01]
        let input = [0, 0, 0, 5, 0x67, 0x42, 0x1F, 0x00, 0x01];
        let out = avcc_to_annex_b(&input, 4);
        assert_eq!(out, vec![0, 0, 0, 1, 0x67, 0x42, 0x1F, 0x00, 0x01]);
    }

    #[test]
    fn avcc_multi_nal_converts_in_order() {
        let mut input = Vec::new();
        input.extend_from_slice(&avcc_unit(&[0x67, 0x42, 0x1F])); // SPS
        input.extend_from_slice(&avcc_unit(&[0x68, 0xCE])); // PPS

        let out = avcc_to_annex_b(&input, 4);

        let mut expected = Vec::new();
        expected.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x1F]);
        expected.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE]);
        assert_eq!(out, expected);

        // And the order is preserved when split back out.
        let units: Vec<&[u8]> = nal::iter_nal_units(&out).collect();
        assert_eq!(units, vec![&[0x67, 0x42, 0x1F][..], &[0x68, 0xCE][..]]);
    }

    #[test]
    fn avcc_length_overrun_is_bounded_no_panic() {
        // Length prefix claims 10 bytes but only 3 follow — must stop, not overrun.
        let input = [0, 0, 0, 10, 0xAA, 0xBB, 0xCC];
        let out = avcc_to_annex_b(&input, 4);
        assert!(out.is_empty(), "overrunning prefix must yield no output");
    }

    #[test]
    fn avcc_partial_trailing_prefix_is_ignored() {
        // One valid NAL followed by a stray 2 bytes that can't form a 4-byte prefix.
        let mut input = avcc_unit(&[0x67, 0x01]);
        input.extend_from_slice(&[0x00, 0x00]);
        let out = avcc_to_annex_b(&input, 4);
        assert_eq!(out, vec![0, 0, 0, 1, 0x67, 0x01]);
    }

    #[test]
    fn avcc_round_trips_through_extract_codec_config() {
        // AVCC buffer carrying SPS + PPS + IDR, converted, then parsed back.
        let mut input = Vec::new();
        input.extend_from_slice(&avcc_unit(&[0x67, 0x42, 0x1F])); // SPS
        input.extend_from_slice(&avcc_unit(&[0x68, 0xCE])); // PPS
        input.extend_from_slice(&avcc_unit(&[0x65, 0x88, 0x99])); // IDR

        let out = avcc_to_annex_b(&input, 4);

        let cfg = nal::extract_codec_config(&out).expect("config recovered from Annex-B");
        assert_eq!(cfg.sps, vec![0x67, 0x42, 0x1F]);
        assert_eq!(cfg.pps, vec![0x68, 0xCE]);
        assert!(nal::is_keyframe(&out));
    }

    #[test]
    fn avcc_two_byte_length_size_converts() {
        // Real AVCC uses lengthSizeMinusOne from avcC: the prefix can be 1/2/4 bytes.
        // Here a 2-byte big-endian length (0x0003) frames a 3-byte NAL.
        let input = [0x00, 0x03, 0x67, 0x42, 0x1F];
        let out = avcc_to_annex_b(&input, 2);
        assert_eq!(out, vec![0, 0, 0, 1, 0x67, 0x42, 0x1F]);
    }

    #[test]
    fn avcc_zero_length_unit_is_skipped_not_fatal() {
        // A stray zero-length unit must be skipped, NOT abort the rest of the buffer.
        // [00 00 00 00] (len 0) then [00 00 00 03][AA BB CC] -> the real NAL survives.
        let mut input = vec![0, 0, 0, 0];
        input.extend_from_slice(&avcc_unit(&[0xAA, 0xBB, 0xCC]));
        let out = avcc_to_annex_b(&input, 4);
        assert_eq!(out, vec![0, 0, 0, 1, 0xAA, 0xBB, 0xCC]);
    }

    // --- to_annex_b_frame (in-band SPS/PPS injection) -------------------------

    #[test]
    fn annex_b_keyframe_injects_sps_pps() {
        let sps = [0x67, 0x42, 0x1F];
        let pps = [0x68, 0xCE];
        // Picture AVCC payload carries only the IDR slice, no parameter sets in-band.
        let picture = avcc_unit(&[0x65, 0xAA, 0xBB]);

        let out = to_annex_b_frame(&picture, 4, true, &sps, &pps);

        // extract_codec_config recovers the injected SPS/PPS even though the AVCC
        // picture payload had none in-band.
        let cfg = nal::extract_codec_config(&out).expect("injected config present");
        assert_eq!(cfg.sps, sps.to_vec());
        assert_eq!(cfg.pps, pps.to_vec());

        // Output order: SPS, PPS, then the picture NAL.
        let units: Vec<&[u8]> = nal::iter_nal_units(&out).collect();
        assert_eq!(units.len(), 3);
        assert_eq!(nal::nal_unit_type(units[0]), Some(nal::nal_type::SPS));
        assert_eq!(nal::nal_unit_type(units[1]), Some(nal::nal_type::PPS));
        assert_eq!(nal::nal_unit_type(units[2]), Some(nal::nal_type::IDR_SLICE));
    }

    #[test]
    fn annex_b_non_keyframe_does_not_inject() {
        let sps = [0x67, 0x42, 0x1F];
        let pps = [0x68, 0xCE];
        let picture = avcc_unit(&[0x61, 0xCC]); // non-IDR slice

        let out = to_annex_b_frame(&picture, 4, false, &sps, &pps);

        // Just the converted picture NAL — no SPS/PPS prepended.
        assert_eq!(out, vec![0, 0, 0, 1, 0x61, 0xCC]);
        assert_eq!(nal::extract_codec_config(&out), None);
        let units: Vec<&[u8]> = nal::iter_nal_units(&out).collect();
        assert_eq!(units.len(), 1);
        assert_eq!(
            nal::nal_unit_type(units[0]),
            Some(nal::nal_type::NON_IDR_SLICE)
        );
    }

    #[test]
    fn annex_b_keyframe_with_multi_nal_picture_preserves_order() {
        let sps = [0x67, 0x01];
        let pps = [0x68, 0x01];
        // Two picture NALs in the AVCC payload.
        let mut picture = avcc_unit(&[0x65, 0x10]);
        picture.extend_from_slice(&avcc_unit(&[0x65, 0x20]));

        let out = to_annex_b_frame(&picture, 4, true, &sps, &pps);

        let units: Vec<&[u8]> = nal::iter_nal_units(&out).collect();
        assert_eq!(units.len(), 4); // SPS, PPS, slice, slice
        assert_eq!(units[0], &sps[..]);
        assert_eq!(units[1], &pps[..]);
        assert_eq!(units[2], &[0x65, 0x10][..]);
        assert_eq!(units[3], &[0x65, 0x20][..]);
    }
}
