//! RFC 4867 AMR-NB mode 7 payload framing used by O2/giffgaff incoming calls.
//!
//! The actual AMR encoder/decoder remains a native media dependency. This
//! module owns only the deterministic RTP payload conversion so it can move to
//! the Rust call core without taking ownership of the live audio device.

use std::fmt;

pub const MODE_7_SPEECH_BITS: usize = 244;
pub const OCTET_ALIGNED_MODE_7_BYTES: usize = 33;
pub const BANDWIDTH_EFFICIENT_MODE_7_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmrPayloadError {
    TruncatedOctetAligned,
    UnsupportedOctetAlignedFrame,
    TruncatedBandwidthEfficient,
    MultipleFramesUnsupported,
    UnsupportedFrameType(u8),
}

impl fmt::Display for AmrPayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedOctetAligned => {
                formatter.write_str("truncated octet-aligned AMR mode-7 payload")
            }
            Self::UnsupportedOctetAlignedFrame => {
                formatter.write_str("unsupported octet-aligned AMR frame")
            }
            Self::TruncatedBandwidthEfficient => {
                formatter.write_str("truncated bandwidth-efficient AMR mode-7 payload")
            }
            Self::MultipleFramesUnsupported => {
                formatter.write_str("multi-frame bandwidth-efficient AMR is unsupported")
            }
            Self::UnsupportedFrameType(value) => write!(
                formatter,
                "unsupported bandwidth-efficient AMR frame type {value}"
            ),
        }
    }
}

impl std::error::Error for AmrPayloadError {}

pub fn to_bandwidth_efficient(
    payload: &[u8],
) -> Result<[u8; BANDWIDTH_EFFICIENT_MODE_7_BYTES], AmrPayloadError> {
    if payload.len() < OCTET_ALIGNED_MODE_7_BYTES {
        return Err(AmrPayloadError::TruncatedOctetAligned);
    }
    let toc = payload[1];
    if toc & 0x80 != 0 || (toc >> 3) & 0x0f != 7 || toc & 0x04 == 0 {
        return Err(AmrPayloadError::UnsupportedOctetAlignedFrame);
    }
    let mut output = [0; BANDWIDTH_EFFICIENT_MODE_7_BYTES];
    write_bits(&mut output, 0, u64::from(payload[0] >> 4), 4);
    write_bits(&mut output, 4, 0, 1);
    write_bits(&mut output, 5, u64::from((toc >> 3) & 0x0f), 4);
    write_bits(&mut output, 9, u64::from((toc >> 2) & 1), 1);
    for bit in 0..MODE_7_SPEECH_BITS {
        write_bits(
            &mut output,
            10 + bit,
            u64::from(read_bit(&payload[2..], bit)),
            1,
        );
    }
    Ok(output)
}

pub fn to_octet_aligned(
    payload: &[u8],
) -> Result<[u8; OCTET_ALIGNED_MODE_7_BYTES], AmrPayloadError> {
    if payload.len() * 8 < 4 + 6 + MODE_7_SPEECH_BITS {
        return Err(AmrPayloadError::TruncatedBandwidthEfficient);
    }
    if read_bit(payload, 4) != 0 {
        return Err(AmrPayloadError::MultipleFramesUnsupported);
    }
    let frame_type = read_bits(payload, 5, 4) as u8;
    let quality = read_bits(payload, 9, 1) as u8;
    if frame_type != 7 || quality == 0 {
        return Err(AmrPayloadError::UnsupportedFrameType(frame_type));
    }
    let mut output = [0; OCTET_ALIGNED_MODE_7_BYTES];
    output[0] = (read_bits(payload, 0, 4) as u8) << 4;
    output[1] = frame_type << 3 | quality << 2;
    for bit in 0..MODE_7_SPEECH_BITS {
        write_bits(
            &mut output[2..],
            bit,
            u64::from(read_bit(payload, 10 + bit)),
            1,
        );
    }
    Ok(output)
}

fn read_bit(data: &[u8], bit: usize) -> u8 {
    (data[bit / 8] >> (7 - bit % 8)) & 1
}

fn read_bits(data: &[u8], offset: usize, count: usize) -> u64 {
    (0..count).fold(0, |value, index| {
        value << 1 | u64::from(read_bit(data, offset + index))
    })
}

fn write_bits(data: &mut [u8], offset: usize, value: u64, count: usize) {
    for index in 0..count {
        let bit = ((value >> (count - 1 - index)) & 1) as u8;
        let position = offset + index;
        let mask = 1 << (7 - position % 8);
        if bit != 0 {
            data[position / 8] |= mask;
        } else {
            data[position / 8] &= !mask;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> [u8; OCTET_ALIGNED_MODE_7_BYTES] {
        let mut payload = [0; OCTET_ALIGNED_MODE_7_BYTES];
        payload[0] = 0xf0;
        payload[1] = 7 << 3 | 1 << 2;
        for (index, octet) in payload.iter_mut().enumerate().skip(2) {
            *octet = (index as u8).wrapping_mul(17).wrapping_add(3);
        }
        payload[32] &= 0xf0;
        payload
    }

    #[test]
    fn mode_7_round_trip_matches_rfc4867_lengths() {
        let source = fixture();
        let packed = to_bandwidth_efficient(&source).unwrap();
        assert_eq!(packed.len(), 32);
        assert_eq!(to_octet_aligned(&packed).unwrap(), source);
    }

    #[test]
    fn rejects_truncated_and_multi_frame_payloads() {
        assert_eq!(
            to_bandwidth_efficient(&[0; 32]),
            Err(AmrPayloadError::TruncatedOctetAligned)
        );
        let mut packed = to_bandwidth_efficient(&fixture()).unwrap();
        packed[0] |= 0x08;
        assert_eq!(
            to_octet_aligned(&packed),
            Err(AmrPayloadError::MultipleFramesUnsupported)
        );
    }
}
