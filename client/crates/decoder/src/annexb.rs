//! Annex-B elementary stream helpers.
//!
//! The host sends H.264/HEVC in Annex-B framing (start codes). VideoToolbox
//! wants a format description plus length-prefixed (AVCC) access units, so the
//! decoder needs to split on start codes, pull out the parameter sets, and
//! re-frame the rest. All of that is pure data manipulation and lives here so it
//! can be tested on any platform.

use macss_protocol::Codec;

/// Length of a start code at `index`, or 0 if there is none.
fn start_code_len(data: &[u8], index: usize) -> usize {
    let rest = &data[index..];
    if rest.len() >= 4 && rest[0] == 0 && rest[1] == 0 && rest[2] == 0 && rest[3] == 1 {
        4
    } else if rest.len() >= 3 && rest[0] == 0 && rest[1] == 0 && rest[2] == 1 {
        3
    } else {
        0
    }
}

/// Trailing zero bytes between NAL units are `trailing_zero_8bits` and are not
/// part of the preceding NAL.
fn trim_trailing_zeros(data: &[u8], start: usize, end: usize) -> usize {
    let mut trimmed = end;
    while trimmed > start && data[trimmed - 1] == 0 {
        trimmed -= 1;
    }
    trimmed
}

/// Split an Annex-B buffer into NAL units, without their start codes.
pub fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut index = 0;
    let mut current: Option<usize> = None;

    while index < data.len() {
        let sc = start_code_len(data, index);
        if sc > 0 {
            if let Some(start) = current {
                let end = trim_trailing_zeros(data, start, index);
                if end > start {
                    nals.push(&data[start..end]);
                }
            }
            index += sc;
            current = Some(index);
        } else {
            index += 1;
        }
    }

    if let Some(start) = current {
        let end = trim_trailing_zeros(data, start, data.len());
        if end > start {
            nals.push(&data[start..end]);
        }
    }

    nals
}

/// H.264 NAL unit type (`nal_unit_type`), from the low 5 bits.
pub fn h264_nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|byte| byte & 0x1F)
}

/// HEVC NAL unit type, from bits 1..7 of the first byte.
pub fn hevc_nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|byte| (byte >> 1) & 0x3F)
}

/// True when the NAL carries a decoder configuration parameter set.
pub fn is_parameter_set(codec: Codec, nal: &[u8]) -> bool {
    match codec {
        Codec::H264 => matches!(h264_nal_type(nal), Some(7 | 8)), // SPS | PPS
        Codec::Hevc => matches!(hevc_nal_type(nal), Some(32 | 33 | 34)), // VPS | SPS | PPS
    }
}

/// Split NALs into (parameter sets, coded slices) for the given codec.
pub fn split_parameter_sets<'a>(
    codec: Codec,
    nals: &[&'a [u8]],
) -> (Vec<&'a [u8]>, Vec<&'a [u8]>) {
    let mut sets = Vec::new();
    let mut slices = Vec::new();
    for nal in nals {
        if is_parameter_set(codec, nal) {
            sets.push(*nal);
        } else {
            slices.push(*nal);
        }
    }
    (sets, slices)
}

/// Re-frame NALs as a length-prefixed (AVCC) access unit with 4-byte lengths.
pub fn to_avcc(nals: &[&[u8]]) -> Vec<u8> {
    let total: usize = nals.iter().map(|nal| nal.len() + 4).sum();
    let mut out = Vec::with_capacity(total);
    for nal in nals {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stream() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1F]); // SPS
        data.extend_from_slice(&[0, 0, 1, 0x68, 0xCE, 0x38, 0x80]); // PPS (3-byte start code)
        data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21]); // IDR slice
        data
    }

    #[test]
    fn splits_mixed_start_codes() {
        let stream = sample_stream();
        let nals = split_nals(&stream);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], [0x67, 0x42, 0x00, 0x1F]);
        assert_eq!(nals[1], [0x68, 0xCE, 0x38, 0x80]);
        assert_eq!(nals[2], [0x65, 0x88, 0x84, 0x21]);
    }

    #[test]
    fn trims_trailing_zero_bytes() {
        let stream = [0, 0, 0, 1, 0x65, 0x11, 0x00, 0x00, 0x00, 0x00];
        let nals = split_nals(&stream);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], [0x65, 0x11]);
    }

    #[test]
    fn handles_input_without_start_codes() {
        assert!(split_nals(&[]).is_empty());
        assert!(split_nals(&[1, 2, 3, 4]).is_empty());
    }

    #[test]
    fn classifies_h264_parameter_sets() {
        let stream = sample_stream();
        let nals = split_nals(&stream);
        let (sets, slices) = split_parameter_sets(Codec::H264, &nals);
        assert_eq!(sets.len(), 2);
        assert_eq!(slices.len(), 1);
        assert_eq!(h264_nal_type(sets[0]), Some(7));
        assert_eq!(h264_nal_type(sets[1]), Some(8));
        assert_eq!(h264_nal_type(slices[0]), Some(5));
    }

    #[test]
    fn classifies_hevc_parameter_sets() {
        // VPS (32) | SPS (33) | PPS (34) all have (byte >> 1) & 0x3F equal to type.
        let vps = [0x40u8, 0x01];
        let sps = [0x42u8, 0x01];
        let pps = [0x44u8, 0x01];
        let slice = [0x26u8, 0x01]; // IDR_W_RADL (19)
        assert_eq!(hevc_nal_type(&vps), Some(32));
        assert_eq!(hevc_nal_type(&sps), Some(33));
        assert_eq!(hevc_nal_type(&pps), Some(34));
        assert_eq!(hevc_nal_type(&slice), Some(19));

        let nals: Vec<&[u8]> = vec![&vps, &sps, &pps, &slice];
        let (sets, slices) = split_parameter_sets(Codec::Hevc, &nals);
        assert_eq!(sets.len(), 3);
        assert_eq!(slices.len(), 1);
    }

    #[test]
    fn writes_avcc_length_prefixes() {
        let a = [0x65u8, 0x11];
        let b = [0x41u8, 0x22, 0x33];
        let avcc = to_avcc(&[&a, &b]);
        assert_eq!(
            avcc,
            vec![
                0, 0, 0, 2, 0x65, 0x11, //
                0, 0, 0, 3, 0x41, 0x22, 0x33
            ]
        );
        assert!(to_avcc(&[]).is_empty());
    }

    #[test]
    fn avcc_round_trips_through_a_length_parser() {
        let stream = sample_stream();
        let nals = split_nals(&stream);
        let (_, slices) = split_parameter_sets(Codec::H264, &nals);
        let avcc = to_avcc(&slices);
        let len = u32::from_be_bytes([avcc[0], avcc[1], avcc[2], avcc[3]]) as usize;
        assert_eq!(len, slices[0].len());
        assert_eq!(&avcc[4..4 + len], slices[0]);
    }
}
