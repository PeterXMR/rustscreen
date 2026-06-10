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
//! - [`avcc_has_idr`] — keyframe detection straight from the AVCC layout (the NAL
//!   payload bytes are identical in both framings), so the encode hot path never has
//!   to convert an access unit just to ask whether it is a keyframe.
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
/// from the `avcC` configuration record, legally **1, 2, 3, or 4** (VideoToolbox commonly
/// emits 4, but it must not be assumed; the Wave B adapter reads it from the format
/// description and passes it here). This rewrites each unit as `00 00 00 01` + NAL data,
/// concatenated in order, yielding a buffer `protocol::nal` can parse.
///
/// Robustness: a prefix claiming more bytes than remain (truncated/malformed stream)
/// stops conversion without reading out of bounds (ASVS V5, threat T-P3-01); a stray
/// zero-length unit is skipped rather than aborting the remaining buffer; an empty
/// input yields an empty output.
///
/// `nal_length_size` MUST be in `1..=4` (an `avcC` `lengthSizeMinusOne` of 0..=3). Any
/// other value is a caller-contract violation and returns an empty `Vec` — critically,
/// `nal_length_size == 0` is REJECTED here rather than entering the loop, because with a
/// zero-width prefix `i += nal_length_size` never advances and the `len == 0` skip would
/// spin forever (a real release-build hang — a `debug_assert!` alone is compiled out).
pub fn avcc_to_annex_b(avcc: &[u8], nal_length_size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(avcc.len());
    append_avcc_as_annex_b(&mut out, avcc, nal_length_size);
    out
}

/// Walk the AVCC units in `avcc`, calling `f` with each NAL payload (no prefix). `f`
/// returns `false` to stop early. The single home of the bounds-checked AVCC walk —
/// the converter and the keyframe probe both ride it, so the truncation/zero-length/
/// prefix-width subtleties (documented on [`avcc_to_annex_b`]) exist exactly once.
fn for_each_avcc_nal(avcc: &[u8], nal_length_size: usize, mut f: impl FnMut(&[u8]) -> bool) {
    if !(1..=4).contains(&nal_length_size) {
        // Caller-contract violation. Walking nothing (rather than panicking across a
        // possible FFI boundary, or — for 0 — looping forever) is the safe library
        // behavior; the contract is documented on `avcc_to_annex_b`.
        return;
    }
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
        if !f(&avcc[i..i + len]) {
            return;
        }
        i += len;
    }
}

/// Append the Annex-B rewrite of `avcc` directly onto `out` — the allocation-free
/// (caller-owns-the-buffer) form of [`avcc_to_annex_b`], used by [`to_annex_b_frame`]
/// so the encode hot path builds each frame in ONE buffer instead of converting into
/// a temporary and copying it over.
fn append_avcc_as_annex_b(out: &mut Vec<u8>, avcc: &[u8], nal_length_size: usize) {
    for_each_avcc_nal(avcc, nal_length_size, |nal| {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
        true
    });
}

/// Whether the AVCC access unit contains an IDR slice — keyframe detection WITHOUT
/// converting to Annex-B first. The NAL payload bytes are identical in both framings
/// (only the per-unit prefix differs), so the type bits can be read straight from the
/// AVCC layout. The encode hot path needs the keyframe answer BEFORE conversion (it
/// decides SPS/PPS injection in [`to_annex_b_frame`]); probing here removes the old
/// convert-then-discard pass that cost a full frame-sized allocation+copy per frame.
/// Same walk contract as [`avcc_to_annex_b`]: an out-of-contract `nal_length_size` or
/// a truncated buffer yields `false` (no units walked ⇒ no IDR seen).
pub fn avcc_has_idr(avcc: &[u8], nal_length_size: usize) -> bool {
    let mut found = false;
    for_each_avcc_nal(avcc, nal_length_size, |nal| {
        found = protocol::nal::nal_unit_type(nal) == Some(protocol::nal::nal_type::IDR_SLICE);
        !found // keep walking until the first IDR
    });
    found
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
    // One allocation for the whole frame: the Annex-B body is the same size as the AVCC
    // input when the prefix width is 4 (VideoToolbox's usual) and within ±3 bytes/unit
    // otherwise, so reserving prefix + avcc.len() makes growth-reallocs rare, not load-bearing.
    let prefix = if is_keyframe {
        2 * START_CODE.len() + sps.len() + pps.len()
    } else {
        0
    };
    let mut out = Vec::with_capacity(prefix + avcc.len());
    if is_keyframe {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(sps);
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(pps);
    }
    append_avcc_as_annex_b(&mut out, avcc, nal_length_size);
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
    fn avcc_invalid_length_size_returns_empty_without_hanging() {
        // Regression: `nal_length_size == 0` previously only had a `debug_assert!` guard,
        // so a RELEASE build would spin forever (i never advances, len==0 skip loops). It
        // must now return empty for any size outside 1..=4 — and crucially terminate.
        // (If this regressed, the test would hang rather than fail — that's the symptom.)
        assert!(avcc_to_annex_b(&[1, 2, 3, 4, 5], 0).is_empty());
        assert!(avcc_to_annex_b(&[1, 2, 3, 4, 5], 5).is_empty());
        assert!(avcc_to_annex_b(&[1, 2, 3, 4, 5], usize::MAX).is_empty());
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
    fn avcc_one_byte_length_size_converts() {
        // lengthSizeMinusOne == 0 → a 1-byte length prefix. Exercises the BE accumulation loop
        // at its narrowest width (the kind of place a width-specific off-by-one hides).
        // [03][67 42 1F] -> [00 00 00 01][67 42 1F]
        let input = [0x03, 0x67, 0x42, 0x1F];
        let out = avcc_to_annex_b(&input, 1);
        assert_eq!(out, vec![0, 0, 0, 1, 0x67, 0x42, 0x1F]);
    }

    #[test]
    fn avcc_three_byte_length_size_converts() {
        // lengthSizeMinusOne == 2 → a 3-byte length prefix. [00 00 03][67 42 1F] -> start code.
        let input = [0x00, 0x00, 0x03, 0x67, 0x42, 0x1F];
        let out = avcc_to_annex_b(&input, 3);
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

    // --- avcc_has_idr (hot-path keyframe probe, no conversion) -----------------

    #[test]
    fn has_idr_true_when_idr_present_among_units() {
        let mut input = Vec::new();
        input.extend_from_slice(&avcc_unit(&[0x67, 0x42, 0x1F])); // SPS
        input.extend_from_slice(&avcc_unit(&[0x68, 0xCE])); // PPS
        input.extend_from_slice(&avcc_unit(&[0x65, 0x88, 0x99])); // IDR
        assert!(avcc_has_idr(&input, 4));
    }

    #[test]
    fn has_idr_false_for_delta_only_unit() {
        let input = avcc_unit(&[0x61, 0x88]); // non-IDR slice
        assert!(!avcc_has_idr(&input, 4));
    }

    #[test]
    fn has_idr_matches_is_keyframe_on_the_converted_stream() {
        // The probe replaced serve.rs's convert-then-`nal::is_keyframe` pass; the two
        // MUST agree on every shape the walk handles specially (multi-unit, zero-length
        // unit, truncated tail, narrow prefix), or the keyframe flag silently flips.
        let mut multi = Vec::new();
        multi.extend_from_slice(&avcc_unit(&[0x67, 0x42, 0x1F]));
        multi.extend_from_slice(&avcc_unit(&[0x65, 0x88]));
        let mut zero_len_then_idr = vec![0, 0, 0, 0];
        zero_len_then_idr.extend_from_slice(&avcc_unit(&[0x65, 0xAA]));
        let mut truncated = avcc_unit(&[0x61, 0x01]);
        truncated.extend_from_slice(&[0, 0, 0, 9, 0x65]); // IDR claimed but cut off
        let narrow = [0x02, 0x65, 0xAA]; // 1-byte prefix framing an IDR
        for (avcc, nal_len) in [
            (&multi[..], 4),
            (&zero_len_then_idr[..], 4),
            (&truncated[..], 4),
            (&narrow[..], 1),
            (&[][..], 4),
        ] {
            assert_eq!(
                avcc_has_idr(avcc, nal_len),
                nal::is_keyframe(&avcc_to_annex_b(avcc, nal_len)),
                "probe disagrees with convert-then-check for {avcc:02X?} (prefix {nal_len})"
            );
        }
    }

    #[test]
    fn has_idr_invalid_length_size_is_false_without_hanging() {
        // Same contract as the converter: 0 must terminate (not spin), 5+ walks nothing.
        assert!(!avcc_has_idr(&[1, 2, 3, 4, 5], 0));
        assert!(!avcc_has_idr(&[1, 2, 3, 4, 5], 5));
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
