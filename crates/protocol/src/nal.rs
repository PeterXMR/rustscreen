//! H.264 Annex-B NAL-unit parsing — pure Rust, no platform dependencies.
//!
//! Both halves of RustScreen need to reason about the NAL units in an H.264 stream:
//! the macOS host (P3) must pull the codec config (SPS/PPS) out of the encoder's
//! output to log it and to forward it as `VideoConfig`; the Pixel client (P4) must
//! split the stream into NAL units and feed the SPS/PPS (`csd-0`) to `AMediaCodec`
//! before the picture slices. That logic is platform-independent, so it lives here in
//! the shared `protocol` crate.
//!
//! Annex-B framing: each NAL unit is preceded by a start-code prefix — either the
//! 3-byte `00 00 01` or the 4-byte `00 00 00 01`. The first byte of a NAL unit (its
//! header) carries `nal_unit_type` in its low 5 bits.

/// H.264 `nal_unit_type` values RustScreen cares about (low 5 bits of the header byte).
pub mod nal_type {
    /// Coded slice of a non-IDR picture.
    pub const NON_IDR_SLICE: u8 = 1;
    /// Coded slice of an IDR picture (a keyframe the decoder can start from).
    pub const IDR_SLICE: u8 = 5;
    /// Sequence Parameter Set.
    pub const SPS: u8 = 7;
    /// Picture Parameter Set.
    pub const PPS: u8 = 8;
}

/// Codec configuration extracted from an H.264 stream.
///
/// `sps` and `pps` are the raw NAL units **without** their start codes (each begins
/// at its NAL header byte). Downstream this becomes the decoder's `csd-0` (P4) and
/// the `VideoConfig` payload sent on connect and on each keyframe (P5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecConfig {
    /// The Sequence Parameter Set NAL unit (no start code).
    pub sps: Vec<u8>,
    /// The Picture Parameter Set NAL unit (no start code).
    pub pps: Vec<u8>,
}

/// Iterator over the NAL units in an Annex-B byte stream.
///
/// Each yielded slice is one NAL unit **without** its start code (it starts at the
/// NAL header byte). Both 3- and 4-byte start codes are handled; bytes before the
/// first start code are ignored; empty units (back-to-back start codes) are skipped.
///
/// Precondition: a NAL unit's bytes must not *end* in `0x00` immediately before the
/// next start code — per Annex B such trailing zeros are `trailing_zero_8bits` /
/// `leading_zero_8bits` of the start code, so they are stripped from the unit (pinned
/// by the `nal_excludes_trailing_zero_bytes...` and `single_zero_byte_nal...` tests).
/// This never affects real H.264 SPS/PPS/slice NALs (which do not end in `0x00`), but
/// callers must not rely on this iterator for byte-exact round-tripping of arbitrary
/// payloads.
pub struct NalUnits<'a> {
    stream: &'a [u8],
    /// Start index of the current NAL's data (just past a start code), if known.
    data_start: Option<usize>,
    /// Index to resume scanning for the next start code.
    search_from: usize,
}

/// Iterate the NAL units in an Annex-B byte `stream`. See [`NalUnits`].
pub fn iter_nal_units(stream: &[u8]) -> NalUnits<'_> {
    NalUnits {
        stream,
        data_start: None,
        search_from: 0,
    }
}

/// The `nal_unit_type` (low 5 bits of the header byte) of a NAL unit slice, or
/// `None` if the slice is empty.
pub fn nal_unit_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| b & 0x1f)
}

/// Extract the [`CodecConfig`] (first SPS + first PPS) from an Annex-B `stream`.
/// Returns `None` if either parameter set is absent.
pub fn extract_codec_config(stream: &[u8]) -> Option<CodecConfig> {
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;
    for nal in iter_nal_units(stream) {
        match nal_unit_type(nal) {
            Some(nal_type::SPS) if sps.is_none() => sps = Some(nal.to_vec()),
            Some(nal_type::PPS) if pps.is_none() => pps = Some(nal.to_vec()),
            _ => {}
        }
        if sps.is_some() && pps.is_some() {
            break;
        }
    }
    Some(CodecConfig {
        sps: sps?,
        pps: pps?,
    })
}

/// Whether `stream` contains an IDR slice (a keyframe / random-access point).
pub fn is_keyframe(stream: &[u8]) -> bool {
    iter_nal_units(stream).any(|nal| nal_unit_type(nal) == Some(nal_type::IDR_SLICE))
}

/// Find the next Annex-B start code at or after `from`. Returns
/// `(start_code_index, data_index)` where `start_code_index` is the first byte of
/// the start code (extended left over any extra leading zero bytes of a 4+-byte
/// start code, but never before `from`) and `data_index` is the first byte after
/// the `00 00 01` marker.
fn find_start_code(stream: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            let mut sc_start = i;
            while sc_start > from && stream[sc_start - 1] == 0 {
                sc_start -= 1;
            }
            return Some((sc_start, i + 3));
        }
        i += 1;
    }
    None
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        loop {
            // Establish where the current NAL's data begins (just past a start code).
            let data_start = match self.data_start {
                Some(ds) => ds,
                None => {
                    let (_, ds) = find_start_code(self.stream, self.search_from)?;
                    self.data_start = Some(ds);
                    self.search_from = ds;
                    ds
                }
            };

            // The current NAL ends at the next start code, or at end of stream.
            match find_start_code(self.stream, self.search_from) {
                Some((sc_start, next_ds)) => {
                    let nal = &self.stream[data_start..sc_start];
                    self.data_start = Some(next_ds);
                    self.search_from = next_ds;
                    if nal.is_empty() {
                        continue; // back-to-back start codes
                    }
                    return Some(nal);
                }
                None => {
                    let nal = &self.stream[data_start..];
                    self.data_start = None;
                    self.search_from = self.stream.len();
                    return if nal.is_empty() { None } else { Some(nal) };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 3-byte start code helper sequences for readability.
    const SC3: [u8; 3] = [0, 0, 1];
    const SC4: [u8; 4] = [0, 0, 0, 1];

    #[test]
    fn splits_units_with_four_byte_start_codes() {
        // [SC4][67 AA][SC4][68 BB][SC4][65 CC]
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x67, 0xAA]);
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x68, 0xBB]);
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x65, 0xCC]);

        let units: Vec<&[u8]> = iter_nal_units(&s).collect();
        assert_eq!(
            units,
            vec![&[0x67, 0xAA][..], &[0x68, 0xBB][..], &[0x65, 0xCC][..]]
        );
    }

    #[test]
    fn splits_units_with_three_byte_start_codes() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC3);
        s.extend_from_slice(&[0x67, 0x11]);
        s.extend_from_slice(&SC3);
        s.extend_from_slice(&[0x68, 0x22]);

        let units: Vec<&[u8]> = iter_nal_units(&s).collect();
        assert_eq!(units, vec![&[0x67, 0x11][..], &[0x68, 0x22][..]]);
    }

    #[test]
    fn handles_mixed_start_code_lengths() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x67]);
        s.extend_from_slice(&SC3);
        s.extend_from_slice(&[0x68]);

        let units: Vec<&[u8]> = iter_nal_units(&s).collect();
        assert_eq!(units, vec![&[0x67][..], &[0x68][..]]);
    }

    #[test]
    fn ignores_leading_bytes_before_first_start_code() {
        let mut s = vec![0xDE, 0xAD];
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x67, 0x01]);

        let units: Vec<&[u8]> = iter_nal_units(&s).collect();
        assert_eq!(units, vec![&[0x67, 0x01][..]]);
    }

    #[test]
    fn skips_empty_units_between_back_to_back_start_codes() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&SC4); // empty unit between the two
        s.extend_from_slice(&[0x67, 0x09]);

        let units: Vec<&[u8]> = iter_nal_units(&s).collect();
        assert_eq!(units, vec![&[0x67, 0x09][..]]);
    }

    #[test]
    fn empty_stream_yields_nothing() {
        let units: Vec<&[u8]> = iter_nal_units(&[]).collect();
        assert!(units.is_empty());
    }

    #[test]
    fn stream_without_start_code_yields_nothing() {
        let units: Vec<&[u8]> = iter_nal_units(&[0x01, 0x02, 0x03]).collect();
        assert!(units.is_empty());
    }

    #[test]
    fn nal_unit_type_reads_low_five_bits() {
        assert_eq!(nal_unit_type(&[0x67]), Some(nal_type::SPS)); // 0b0110_0111 -> 7
        assert_eq!(nal_unit_type(&[0x68]), Some(nal_type::PPS)); // 0b0110_1000 -> 8
        assert_eq!(nal_unit_type(&[0x65]), Some(nal_type::IDR_SLICE)); // -> 5
        assert_eq!(nal_unit_type(&[0x61]), Some(nal_type::NON_IDR_SLICE)); // -> 1
    }

    #[test]
    fn nal_unit_type_of_empty_is_none() {
        assert_eq!(nal_unit_type(&[]), None);
    }

    #[test]
    fn extracts_sps_and_pps() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x67, 0x42, 0x1F]); // SPS
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x68, 0xCE]); // PPS
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x65, 0x88]); // IDR slice

        let cfg = extract_codec_config(&s).expect("config present");
        assert_eq!(cfg.sps, vec![0x67, 0x42, 0x1F]);
        assert_eq!(cfg.pps, vec![0x68, 0xCE]);
    }

    #[test]
    fn nal_excludes_trailing_zero_bytes_belonging_to_next_start_code() {
        // Per Annex B, zero bytes immediately before a start-code prefix are
        // leading_zero_8bits of that start code — not part of the preceding NAL.
        // Here the SPS bytes are `67 42 00`, but the trailing `00` abuts a 4-byte
        // start code, so it is consumed by the start code and stripped from the NAL.
        let mut s = Vec::new();
        s.extend_from_slice(&SC3);
        s.extend_from_slice(&[0x67, 0x42, 0x00]);
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x68, 0xCE]);

        let units: Vec<&[u8]> = iter_nal_units(&s).collect();
        assert_eq!(units, vec![&[0x67, 0x42][..], &[0x68, 0xCE][..]]);
    }

    #[test]
    fn single_zero_byte_nal_is_dropped_documented_edge() {
        // A NAL whose only data byte is 0x00, sandwiched between start codes, is
        // consumed entirely: the 0x00 is a leading_zero_8bits of the next start code,
        // leaving an empty unit which is skipped. Pins the documented precondition
        // (F6). Harmless for real H.264 (0x00 is nal_unit_type 0 = unspecified).
        let mut s = Vec::new();
        s.extend_from_slice(&SC3);
        s.extend_from_slice(&[0x00]); // sole data byte — gets absorbed
        s.extend_from_slice(&SC3);
        s.extend_from_slice(&[0x68, 0xCE]);

        let units: Vec<&[u8]> = iter_nal_units(&s).collect();
        assert_eq!(units, vec![&[0x68, 0xCE][..]]);
    }

    #[test]
    fn extract_uses_first_sps_and_pps_when_repeated() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x67, 0x01]); // first SPS
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x68, 0x01]); // first PPS
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x67, 0x02]); // second SPS (ignored)

        let cfg = extract_codec_config(&s).expect("config present");
        assert_eq!(cfg.sps, vec![0x67, 0x01]);
        assert_eq!(cfg.pps, vec![0x68, 0x01]);
    }

    #[test]
    fn extract_returns_none_when_sps_missing() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x68, 0x01]); // only PPS
        assert_eq!(extract_codec_config(&s), None);
    }

    #[test]
    fn extract_returns_none_when_pps_missing() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x67, 0x01]); // only SPS
        assert_eq!(extract_codec_config(&s), None);
    }

    #[test]
    fn is_keyframe_true_when_idr_present() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x67, 0x01]); // SPS
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x65, 0x88]); // IDR
        assert!(is_keyframe(&s));
    }

    #[test]
    fn is_keyframe_false_without_idr() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x61, 0x88]); // non-IDR slice only
        assert!(!is_keyframe(&s));
    }
}
