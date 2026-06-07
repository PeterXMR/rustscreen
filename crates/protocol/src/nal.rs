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
    /// Supplemental Enhancement Information.
    pub const SEI: u8 = 6;
    /// Sequence Parameter Set.
    pub const SPS: u8 = 7;
    /// Picture Parameter Set.
    pub const PPS: u8 = 8;
    /// Access Unit Delimiter.
    pub const AUD: u8 = 9;
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

/// Split an Annex-B elementary stream into access units, one per coded picture.
///
/// Each returned slice is one access unit **with** its start codes, ready to hand to a
/// decoder. A picture's leading non-VCL NALs (SPS/PPS/SEI/AUD) are grouped with the VCL
/// slice that follows them, so a keyframe's in-band SPS/PPS stay attached to *its own* IDR
/// — including the second and later keyframes in a stream. A new access unit begins at the
/// first AU-starting NAL (a VCL slice, or a leading SPS/PPS/SEI/AUD) that follows the VCL
/// slice of the previous picture. Single-slice pictures are assumed (each VCL slice starts a
/// new picture); the MVP encoder emits one slice per frame.
pub fn split_access_units(stream: &[u8]) -> Vec<&[u8]> {
    let starts = start_code_offsets(stream);
    if starts.is_empty() {
        return Vec::new();
    }

    let mut units = Vec::new();
    let mut au_start = starts[0];
    let mut seen_vcl = false;
    for (i, &off) in starts.iter().enumerate() {
        let nal_end = starts.get(i + 1).copied().unwrap_or(stream.len());
        let ty = nal_unit_type(strip_start_code(&stream[off..nal_end]));
        let is_vcl = matches!(
            ty,
            Some(nal_type::IDR_SLICE) | Some(nal_type::NON_IDR_SLICE)
        );
        // A NAL that can begin an access unit: a VCL slice, or a leading parameter/marker NAL.
        let starts_au = is_vcl
            || matches!(
                ty,
                Some(nal_type::SPS)
                    | Some(nal_type::PPS)
                    | Some(nal_type::SEI)
                    | Some(nal_type::AUD)
            );
        // Once this picture's slice has been seen, the next AU-starting NAL (the next
        // picture's leading SPS/PPS/SEI or its slice) opens a new access unit.
        if seen_vcl && starts_au {
            units.push(&stream[au_start..off]);
            au_start = off;
            seen_vcl = false;
        }
        if is_vcl {
            seen_vcl = true;
        }
    }
    units.push(&stream[au_start..]);
    units
}

/// Byte offsets of each Annex-B start code in `stream` (offset at the start code's first
/// `00 00 01` byte). A 4-byte start code is reported at its `00 00 01`, leaving the leading
/// extra `00` in the previous NAL's slice — harmless for access-unit slicing.
fn start_code_offsets(stream: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            offsets.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    offsets
}

/// Strip a leading 3- or 4-byte Annex-B start code from a NAL slice, returning the body
/// (which begins at the NAL header byte).
fn strip_start_code(nal: &[u8]) -> &[u8] {
    nal.strip_prefix(&[0, 0, 0, 1])
        .or_else(|| nal.strip_prefix(&[0, 0, 1]))
        .unwrap_or(nal)
}

/// Decode the coded picture `(width, height)` in luma samples from an H.264 SPS NAL.
///
/// `sps` is a single SPS NAL unit **without** its start code (it begins at the NAL
/// header byte), exactly as stored in [`CodecConfig::sps`]. Returns `None` if the slice
/// is not an SPS or is truncated/malformed. The Pixel's `AMediaCodec` requires explicit
/// `KEY_WIDTH`/`KEY_HEIGHT` at configure time, so the decode-to-surface adapter reads
/// them from here rather than relying on the decoder to derive them from `csd-0`.
///
/// Parses just enough of the SPS (through `frame_cropping`) per ITU-T H.264 §7.3.2.1.1,
/// applying the frame-crop offsets so e.g. a coded 2400x1088 reports the displayed
/// 2400x1080. Crop-unit scaling follows the SPS `chroma_format_idc` (correct for
/// monochrome, 4:2:0, 4:2:2, and 4:4:4); the MVP encodes 4:2:0 (D2).
pub fn sps_dimensions(sps: &[u8]) -> Option<(u32, u32)> {
    if nal_unit_type(sps)? != nal_type::SPS {
        return None;
    }
    // RBSP = SPS payload (after the 1-byte NAL header) with emulation-prevention bytes
    // removed: any `00 00 03` sequence drops the `03`.
    let payload = sps.get(1..)?;
    let mut rbsp = Vec::with_capacity(payload.len());
    let mut zeros = 0usize;
    for &b in payload {
        if zeros >= 2 && b == 0x03 {
            zeros = 0;
            continue; // skip emulation-prevention byte
        }
        rbsp.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }

    let mut r = BitReader::new(&rbsp);
    let profile_idc = r.u(8)?;
    let _constraint_flags = r.u(8)?;
    let _level_idc = r.u(8)?;
    let _seq_parameter_set_id = r.ue()?;

    // High-profile (and friends) carry chroma/bit-depth fields + an optional scaling matrix.
    let mut chroma_format_idc = 1; // default 4:2:0 for profiles without the field
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format_idc = r.ue()?;
        if chroma_format_idc == 3 {
            let _separate_colour_plane_flag = r.u(1)?;
        }
        let _bit_depth_luma_minus8 = r.ue()?;
        let _bit_depth_chroma_minus8 = r.ue()?;
        let _qpprime_y_zero_transform_bypass_flag = r.u(1)?;
        let seq_scaling_matrix_present_flag = r.u(1)?;
        if seq_scaling_matrix_present_flag == 1 {
            let lists = if chroma_format_idc != 3 { 8 } else { 12 };
            for i in 0..lists {
                if r.u(1)? == 1 {
                    // Present scaling list: walk it with the spec's delta loop so the bit
                    // cursor lands correctly. Size is 16 (4x4, i<6) or 64 (8x8).
                    let size = if i < 6 { 16 } else { 64 };
                    let mut last_scale = 8i32;
                    let mut next_scale = 8i32;
                    for _ in 0..size {
                        if next_scale != 0 {
                            let delta = r.se()?;
                            next_scale = (last_scale + delta + 256) % 256;
                        }
                        if next_scale != 0 {
                            last_scale = next_scale;
                        }
                    }
                }
            }
        }
    }

    let _log2_max_frame_num_minus4 = r.ue()?;
    let pic_order_cnt_type = r.ue()?;
    if pic_order_cnt_type == 0 {
        let _log2_max_pic_order_cnt_lsb_minus4 = r.ue()?;
    } else if pic_order_cnt_type == 1 {
        let _delta_pic_order_always_zero_flag = r.u(1)?;
        let _offset_for_non_ref_pic = r.se()?;
        let _offset_for_top_to_bottom_field = r.se()?;
        let num_ref_frames_in_pic_order_cnt_cycle = r.ue()?;
        for _ in 0..num_ref_frames_in_pic_order_cnt_cycle {
            let _offset_for_ref_frame = r.se()?;
        }
    }

    let _max_num_ref_frames = r.ue()?;
    let _gaps_in_frame_num_value_allowed_flag = r.u(1)?;
    let pic_width_in_mbs_minus1 = r.ue()?;
    let pic_height_in_map_units_minus1 = r.ue()?;
    let frame_mbs_only_flag = r.u(1)?;
    if frame_mbs_only_flag == 0 {
        let _mb_adaptive_frame_field_flag = r.u(1)?;
    }
    let _direct_8x8_inference_flag = r.u(1)?;

    // Checked arithmetic: `pic_width/height_*_minus1` come straight from `ue()` and can be
    // hostile (up to ~u32::MAX) on a malformed/adversarial SPS reachable from the wire. An
    // unchecked `+1`/`*16` would panic in debug and silently wrap a garbage resolution into
    // `AMediaCodec.configure` in release. Returning `None` instead keeps the module's documented
    // "a malformed SPS degrades to None rather than panicking" contract.
    let width = pic_width_in_mbs_minus1.checked_add(1)?.checked_mul(16)?;
    let height = (2 - frame_mbs_only_flag)
        .checked_mul(pic_height_in_map_units_minus1.checked_add(1)?)?
        .checked_mul(16)?;

    // Frame cropping trims the coded macroblock grid down to the displayed picture
    // (e.g. 1088 -> 1080).
    let (mut crop_l, mut crop_r, mut crop_t, mut crop_b) = (0u32, 0u32, 0u32, 0u32);
    if r.u(1)? == 1 {
        crop_l = r.ue()?;
        crop_r = r.ue()?;
        crop_t = r.ue()?;
        crop_b = r.ue()?;
    }
    // Crop units are SubWidthC / SubHeightC * (2 - frame_mbs_only) per ITU-T H.264
    // Table 6-1; monochrome (chroma_format_idc 0) has no chroma arrays so the unit is 1.
    let (sub_width_c, sub_height_c) = match chroma_format_idc {
        1 => (2, 2), // 4:2:0
        2 => (2, 1), // 4:2:2
        _ => (1, 1), // 4:4:4 (3); monochrome (0) handled below
    };
    let (crop_unit_x, crop_unit_y) = if chroma_format_idc == 0 {
        (1, 2 - frame_mbs_only_flag)
    } else {
        (sub_width_c, sub_height_c * (2 - frame_mbs_only_flag))
    };

    let crop_x = crop_l.checked_add(crop_r)?.checked_mul(crop_unit_x)?;
    let crop_y = crop_t.checked_add(crop_b)?.checked_mul(crop_unit_y)?;
    let width = width.checked_sub(crop_x)?;
    let height = height.checked_sub(crop_y)?;
    Some((width, height))
}

/// Minimal MSB-first bit reader over an RBSP byte slice, with the H.264 Exp-Golomb
/// codings (`ue`/`se`). Every read returns `None` on exhaustion so a truncated SPS
/// degrades to `None` instead of panicking.
struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, bit_pos: 0 }
    }

    /// Read `n` bits (n <= 32) MSB-first as an unsigned value.
    fn u(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.data.get(self.bit_pos / 8)?;
            let bit = (byte >> (7 - (self.bit_pos % 8))) & 1;
            v = (v << 1) | bit as u32;
            self.bit_pos += 1;
        }
        Some(v)
    }

    /// Unsigned Exp-Golomb `ue(v)`.
    fn ue(&mut self) -> Option<u32> {
        let mut leading_zeros = 0u32;
        while self.u(1)? == 0 {
            leading_zeros += 1;
            if leading_zeros > 31 {
                return None; // malformed / runaway
            }
        }
        if leading_zeros == 0 {
            return Some(0);
        }
        let rest = self.u(leading_zeros)?;
        Some((1u32 << leading_zeros) - 1 + rest)
    }

    /// Signed Exp-Golomb `se(v)`.
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        let val = k.div_ceil(2) as i32;
        Some(if k % 2 == 0 { -val } else { val })
    }
}

/// Whether `stream` contains an IDR slice (a keyframe / random-access point).
pub fn is_keyframe(stream: &[u8]) -> bool {
    iter_nal_units(stream).any(|nal| nal_unit_type(nal) == Some(nal_type::IDR_SLICE))
}

/// Whether `stream` already carries an in-band SPS **and** PPS — i.e. it is
/// self-describing and a decoder fed only this access unit could configure itself.
///
/// Used by [`to_annex_b_access_unit`] to decide whether to inject the out-of-band
/// parameter sets ahead of a keyframe (avoiding duplicate SPS/PPS when the encoder
/// already wrote them in-band, as RustScreen's P3 encoder does before the first IDR).
///
/// Presence only — order is NOT checked. H.264 requires SPS before PPS (before the
/// picture); RustScreen relies on the encoder emitting that order (P3's VideoToolbox
/// adapter does). A pathological PPS-before-SPS stream would still report `true` here.
pub fn has_in_band_config(stream: &[u8]) -> bool {
    let mut has_sps = false;
    let mut has_pps = false;
    for nal in iter_nal_units(stream) {
        match nal_unit_type(nal) {
            Some(nal_type::SPS) => has_sps = true,
            Some(nal_type::PPS) => has_pps = true,
            _ => {}
        }
        if has_sps && has_pps {
            return true;
        }
    }
    false
}

/// The 4-byte Annex-B start code RustScreen emits in front of every NAL unit.
const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Why a [`Frame::Video`](crate::messages::Frame::Video) payload could not be turned
/// into a decoder-ready access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessUnitError {
    /// The video payload contained no NAL bytes at all — there is nothing to decode.
    EmptyPayload,
}

impl core::fmt::Display for AccessUnitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AccessUnitError::EmptyPayload => write!(f, "video payload contained no NAL bytes"),
        }
    }
}

impl std::error::Error for AccessUnitError {}

/// Build a decoder-ready Annex-B access unit from a `Frame::Video` NAL payload.
///
/// RustScreen's wire `nal` payload is already Annex-B start-code framed (the host writes
/// the encoder's Annex-B output verbatim — see `macos_host::encode`). This helper makes
/// the access unit **self-describing** so a freshly-configured `AMediaCodec` can decode
/// it: on a **keyframe** whose payload does not already carry in-band SPS/PPS, it prepends
/// the out-of-band [`CodecConfig`] (`00 00 00 01` + SPS, then `00 00 00 01` + PPS) ahead
/// of the picture. When the payload already has in-band parameter sets (P3's encoder
/// injects them before the first IDR), or when `config` is `None`, the payload is passed
/// through unchanged — never duplicating SPS/PPS. Non-keyframes are always passed through.
///
/// An empty payload is rejected ([`AccessUnitError::EmptyPayload`]) rather than yielding a
/// zero-byte buffer the decoder would choke on.
///
/// This is the pure, host-tested feed logic behind the Wave-B `AMediaCodec` adapter; it
/// owns no FFI and no allocation policy beyond the obvious copy.
pub fn to_annex_b_access_unit(
    nal: &[u8],
    keyframe: bool,
    config: Option<&CodecConfig>,
) -> Result<Vec<u8>, AccessUnitError> {
    if nal.is_empty() {
        return Err(AccessUnitError::EmptyPayload);
    }
    // Only inject on a keyframe, only when we have a config, and only when the payload
    // isn't already self-describing (else we'd send duplicate SPS/PPS).
    let inject = match config {
        Some(_) if keyframe => !has_in_band_config(nal),
        _ => false,
    };
    if !inject {
        return Ok(nal.to_vec());
    }
    let config = config.expect("inject implies config.is_some()");
    let mut out =
        Vec::with_capacity(START_CODE.len() * 2 + config.sps.len() + config.pps.len() + nal.len());
    out.extend_from_slice(&START_CODE);
    out.extend_from_slice(&config.sps);
    out.extend_from_slice(&START_CODE);
    out.extend_from_slice(&config.pps);
    out.extend_from_slice(nal);
    Ok(out)
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

    // --- has_in_band_config ---------------------------------------------------

    #[test]
    fn in_band_config_true_with_sps_and_pps() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x67, 0x42]); // SPS
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x68, 0xCE]); // PPS
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x65, 0x00]); // IDR
        assert!(has_in_band_config(&s));
    }

    #[test]
    fn in_band_config_false_with_only_sps() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x67, 0x42]); // SPS only
        assert!(!has_in_band_config(&s));
    }

    #[test]
    fn in_band_config_false_for_plain_slice() {
        let mut s = Vec::new();
        s.extend_from_slice(&SC4);
        s.extend_from_slice(&[0x65, 0x00]); // IDR with no params in-band
        assert!(!has_in_band_config(&s));
    }

    // --- to_annex_b_access_unit -----------------------------------------------

    fn config() -> CodecConfig {
        CodecConfig {
            sps: vec![0x67, 0x42, 0x1F],
            pps: vec![0x68, 0xCE],
        }
    }

    #[test]
    fn au_empty_payload_is_rejected() {
        assert_eq!(
            to_annex_b_access_unit(&[], true, Some(&config())),
            Err(AccessUnitError::EmptyPayload)
        );
        // Empty is rejected regardless of keyframe / config presence.
        assert_eq!(
            to_annex_b_access_unit(&[], false, None),
            Err(AccessUnitError::EmptyPayload)
        );
    }

    #[test]
    fn au_keyframe_without_in_band_config_injects_sps_pps() {
        // Bare IDR picture (no params in-band) on a keyframe → config is prepended so the
        // decoder can configure itself; extract_codec_config must then recover it.
        let mut picture = Vec::new();
        picture.extend_from_slice(&SC4);
        picture.extend_from_slice(&[0x65, 0xAA, 0xBB]); // IDR

        let au = to_annex_b_access_unit(&picture, true, Some(&config())).unwrap();

        let cfg = extract_codec_config(&au).expect("config recovered from AU");
        assert_eq!(cfg.sps, vec![0x67, 0x42, 0x1F]);
        assert_eq!(cfg.pps, vec![0x68, 0xCE]);
        let units: Vec<&[u8]> = iter_nal_units(&au).collect();
        assert_eq!(units.len(), 3); // SPS, PPS, IDR
        assert_eq!(nal_unit_type(units[0]), Some(nal_type::SPS));
        assert_eq!(nal_unit_type(units[1]), Some(nal_type::PPS));
        assert_eq!(nal_unit_type(units[2]), Some(nal_type::IDR_SLICE));
    }

    #[test]
    fn au_keyframe_with_in_band_config_is_passed_through_no_dup() {
        // Payload already self-describing (P3's encoder injects SPS/PPS before the first
        // IDR): the helper must NOT prepend a second copy.
        let mut picture = Vec::new();
        picture.extend_from_slice(&SC4);
        picture.extend_from_slice(&[0x67, 0x01]); // in-band SPS
        picture.extend_from_slice(&SC4);
        picture.extend_from_slice(&[0x68, 0x01]); // in-band PPS
        picture.extend_from_slice(&SC4);
        picture.extend_from_slice(&[0x65, 0x00]); // IDR

        let au = to_annex_b_access_unit(&picture, true, Some(&config())).unwrap();

        assert_eq!(au, picture, "already-self-describing AU must pass through");
        let sps_count = iter_nal_units(&au)
            .filter(|n| nal_unit_type(n) == Some(nal_type::SPS))
            .count();
        assert_eq!(sps_count, 1, "no duplicate SPS injected");
    }

    #[test]
    fn au_keyframe_without_config_passes_through() {
        // No out-of-band config available → nothing to inject; pass the keyframe through.
        let mut picture = Vec::new();
        picture.extend_from_slice(&SC4);
        picture.extend_from_slice(&[0x65, 0xAA]); // bare IDR
        let au = to_annex_b_access_unit(&picture, true, None).unwrap();
        assert_eq!(au, picture);
    }

    #[test]
    fn au_non_keyframe_is_passed_through() {
        // A delta frame is never prefixed with parameter sets, even with a config present.
        let mut picture = Vec::new();
        picture.extend_from_slice(&SC4);
        picture.extend_from_slice(&[0x61, 0xCC]); // non-IDR slice
        let au = to_annex_b_access_unit(&picture, false, Some(&config())).unwrap();
        assert_eq!(au, picture);
        assert_eq!(extract_codec_config(&au), None);
    }

    #[test]
    fn sps_dimensions_high_profile_with_crop() {
        // The real SPS from the P3 `out.h264` acceptance clip: High profile (100),
        // level 5.0, coded 2400x1088 with a 4:2:0 bottom crop down to 2400x1080.
        let sps = [
            0x27, 0x64, 0x00, 0x32, 0xac, 0x56, 0x80, 0x25, 0x80, 0x89, 0xf9, 0x50,
        ];
        assert_eq!(sps_dimensions(&sps), Some((2400, 1080)));
    }

    #[test]
    fn sps_dimensions_rejects_non_sps_nal() {
        // A PPS NAL (type 8), not an SPS — no dimensions to read.
        assert_eq!(sps_dimensions(&[0x28, 0xee, 0x3c, 0xb0]), None);
    }

    #[test]
    fn sps_dimensions_rejects_truncated() {
        // Truncated SPS (header + profile only) must not panic; returns None.
        assert_eq!(sps_dimensions(&[0x67, 0x64]), None);
    }

    // Map each access unit to the sequence of NAL types it contains (for AU-split asserts).
    fn au_nal_types(au: &[u8]) -> Vec<u8> {
        iter_nal_units(au).filter_map(nal_unit_type).collect()
    }

    #[test]
    fn split_access_units_keeps_each_keyframes_sps_pps_with_its_idr() {
        // [SPS1 PPS1 IDR1][P][SPS2 PPS2 IDR2] — every keyframe's parameter sets must stay
        // attached to ITS IDR, not the preceding picture. Header bytes: 0x67=SPS, 0x68=PPS,
        // 0x65=IDR, 0x41=non-IDR slice.
        let mut s = Vec::new();
        for nal in [
            &[0x67, 0x11][..], // SPS1
            &[0x68, 0x22][..], // PPS1
            &[0x65, 0x33][..], // IDR1
            &[0x41, 0x44][..], // P
            &[0x67, 0x55][..], // SPS2
            &[0x68, 0x66][..], // PPS2
            &[0x65, 0x77][..], // IDR2
        ] {
            s.extend_from_slice(&SC4);
            s.extend_from_slice(nal);
        }

        let units = split_access_units(&s);
        let types: Vec<Vec<u8>> = units.iter().map(|u| au_nal_types(u)).collect();
        assert_eq!(
            types,
            vec![
                vec![nal_type::SPS, nal_type::PPS, nal_type::IDR_SLICE],
                vec![nal_type::NON_IDR_SLICE],
                vec![nal_type::SPS, nal_type::PPS, nal_type::IDR_SLICE],
            ]
        );
    }

    #[test]
    fn split_access_units_groups_leading_sei_with_following_idr() {
        // The P3 encoder emits SPS+PPS+SEI before each IDR; the SEI must group with the IDR.
        let mut s = Vec::new();
        for nal in [
            &[0x67, 0x11][..], // SPS
            &[0x68, 0x22][..], // PPS
            &[0x06, 0x05][..], // SEI
            &[0x65, 0x33][..], // IDR
        ] {
            s.extend_from_slice(&SC4);
            s.extend_from_slice(nal);
        }
        let units = split_access_units(&s);
        assert_eq!(units.len(), 1);
        assert_eq!(
            au_nal_types(units[0]),
            vec![
                nal_type::SPS,
                nal_type::PPS,
                nal_type::SEI,
                nal_type::IDR_SLICE
            ]
        );
    }

    #[test]
    fn split_access_units_empty_stream_is_empty() {
        assert!(split_access_units(&[]).is_empty());
    }
}
