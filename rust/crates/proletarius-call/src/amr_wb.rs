//! Single-frame AMR-WB RTP payload framing used by the IMS call path.
//!
//! This matches the live Go `amrWBPacketize` / `amrWBDepacketize` wire format.
//! Audio encoding and decoding remain separate from this protocol-only module.

use std::fmt;

const FRAME_BITS: [Option<usize>; 16] = [
    Some(132),
    Some(177),
    Some(253),
    Some(285),
    Some(317),
    Some(365),
    Some(397),
    Some(461),
    Some(477),
    Some(35), // SID
    None,
    None,
    None,
    None,
    Some(0), // speech lost
    Some(0), // no data
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmrWbPayloadError {
    UnsupportedFrameType(u8),
    InvalidFrameBitsLength { expected: usize, actual: usize },
    ShortPayload,
    UnsupportedOctetAlignedHeader,
    MultipleFramesUnsupported,
    InvalidFrameSize { expected: usize, actual: usize },
}

impl fmt::Display for AmrWbPayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFrameType(mode) => {
                write!(formatter, "unsupported AMR-WB frame type {mode}")
            }
            Self::InvalidFrameBitsLength { expected, actual } => {
                write!(
                    formatter,
                    "AMR-WB frame needs {expected} bytes, got {actual}"
                )
            }
            Self::ShortPayload => formatter.write_str("short AMR-WB RTP payload"),
            Self::UnsupportedOctetAlignedHeader => {
                formatter.write_str("unsupported octet-aligned AMR-WB header")
            }
            Self::MultipleFramesUnsupported => {
                formatter.write_str("multi-frame AMR-WB RTP payload is unsupported")
            }
            Self::InvalidFrameSize { expected, actual } => {
                write!(
                    formatter,
                    "AMR-WB RTP payload needs {expected} bytes, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for AmrWbPayloadError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmrWbFrame {
    pub bits: Vec<u8>,
    pub mode: u8,
    pub good: bool,
    /// Raw four-bit codec mode request. Modes 0..=8 request an AMR-WB
    /// encoder mode, 15 cancels a previous request, and 9..=14 are reserved.
    /// The caller applies negotiated mode-set and switching policy.
    pub raw_cmr: u8,
}

pub fn frame_bits(mode: u8) -> Option<usize> {
    FRAME_BITS.get(usize::from(mode)).copied().flatten()
}

/// Wrap one encoded frame in RFC 4867 octet-aligned or bandwidth-efficient RTP.
/// The outgoing quality bit is set, as in the live Go call path.
pub fn packetize(bits: &[u8], mode: u8, octet_aligned: bool) -> Result<Vec<u8>, AmrWbPayloadError> {
    let bit_count = frame_bits(mode).ok_or(AmrWbPayloadError::UnsupportedFrameType(mode))?;
    let expected = bit_count.div_ceil(8);
    if bits.len() != expected {
        return Err(AmrWbPayloadError::InvalidFrameBitsLength {
            expected,
            actual: bits.len(),
        });
    }
    if octet_aligned {
        let mut payload = Vec::with_capacity(2 + bits.len());
        payload.extend_from_slice(&[0xf0, (mode << 3) | 0x04]);
        payload.extend_from_slice(bits);
        return Ok(payload);
    }

    let mut payload = vec![0; (10 + bit_count).div_ceil(8)];
    payload[0] = 0xf0 | (mode >> 1);
    payload[1] = ((mode & 1) << 7) | 0x40;
    for bit in 0..bit_count {
        if bits[bit / 8] & (1 << (7 - bit % 8)) != 0 {
            let position = 10 + bit;
            payload[position / 8] |= 1 << (7 - position % 8);
        }
    }
    Ok(payload)
}

/// Parse one RTP payload. Bandwidth-efficient padding bits are discarded;
/// octet-aligned bytes are preserved, matching the live Go parser.
pub fn depacketize(payload: &[u8], octet_aligned: bool) -> Result<AmrWbFrame, AmrWbPayloadError> {
    if payload.len() < 2 {
        return Err(AmrWbPayloadError::ShortPayload);
    }
    let raw_cmr = payload[0] >> 4;
    let (mode, good, offset) = if octet_aligned {
        if payload[0] & 0x0f != 0 || payload[1] & 0x83 != 0 {
            return Err(AmrWbPayloadError::UnsupportedOctetAlignedHeader);
        }
        ((payload[1] >> 3) & 0x0f, payload[1] & 0x04 != 0, 16)
    } else {
        if payload[0] & 0x08 != 0 {
            return Err(AmrWbPayloadError::MultipleFramesUnsupported);
        }
        (
            ((payload[0] & 0x07) << 1) | (payload[1] >> 7),
            payload[1] & 0x40 != 0,
            10,
        )
    };
    let bit_count = frame_bits(mode).ok_or(AmrWbPayloadError::UnsupportedFrameType(mode))?;
    let expected = (offset + bit_count).div_ceil(8);
    if payload.len() != expected {
        return Err(AmrWbPayloadError::InvalidFrameSize {
            expected,
            actual: payload.len(),
        });
    }
    if octet_aligned {
        return Ok(AmrWbFrame {
            bits: payload[2..].to_vec(),
            mode,
            good,
            raw_cmr,
        });
    }

    let mut bits = vec![0; bit_count.div_ceil(8)];
    for bit in 0..bit_count {
        let position = 10 + bit;
        if payload[position / 8] & (1 << (7 - position % 8)) != 0 {
            bits[bit / 8] |= 1 << (7 - bit % 8);
        }
    }
    Ok(AmrWbFrame {
        bits,
        mode,
        good,
        raw_cmr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_supported_modes_round_trip_in_both_formats() {
        for mode in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 14, 15] {
            let bit_count = frame_bits(mode).unwrap();
            let mut bits = (0..bit_count.div_ceil(8))
                .map(|index| (index * 37 + usize::from(mode) * 11) as u8)
                .collect::<Vec<_>>();
            if bit_count % 8 != 0 {
                *bits.last_mut().unwrap() &= 0xff << (8 - bit_count % 8);
            }
            for octet_aligned in [false, true] {
                let payload = packetize(&bits, mode, octet_aligned).unwrap();
                assert_eq!(
                    payload.len(),
                    (if octet_aligned { 16 } else { 10 } + bit_count).div_ceil(8),
                    "mode {mode}, octet_aligned {octet_aligned}"
                );
                assert_eq!(
                    depacketize(&payload, octet_aligned).unwrap(),
                    AmrWbFrame {
                        bits: bits.clone(),
                        mode,
                        good: true,
                        raw_cmr: 15,
                    },
                    "mode {mode}, octet_aligned {octet_aligned}"
                );
            }
        }
    }

    #[test]
    fn sid_and_no_data_match_go_wire_bytes() {
        let mut speech = vec![0; 32];
        speech[..7].copy_from_slice(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde]);
        let speech_be = packetize(&speech, 2, false).unwrap();
        assert_eq!(
            &speech_be[..9],
            &[0xf1, 0x44, 0x8d, 0x15, 0x9e, 0x26, 0xaf, 0x37, 0x80]
        );
        assert_eq!(speech_be.len(), 33);
        let sid = [0x12, 0x34, 0x56, 0x78, 0xa0];
        assert_eq!(
            packetize(&sid, 9, true).unwrap(),
            [0xf0, 0x4c, 0x12, 0x34, 0x56, 0x78, 0xa0]
        );
        assert_eq!(
            packetize(&sid, 9, false).unwrap(),
            [0xf4, 0xc4, 0x8d, 0x15, 0x9e, 0x28]
        );
        assert_eq!(packetize(&[], 14, true).unwrap(), [0xf0, 0x74]);
        assert_eq!(packetize(&[], 14, false).unwrap(), [0xf7, 0x40]);
        assert_eq!(packetize(&[], 15, true).unwrap(), [0xf0, 0x7c]);
        assert_eq!(packetize(&[], 15, false).unwrap(), [0xf7, 0xc0]);
        let lost = depacketize(&[0xf0, 0x70], true).unwrap();
        assert_eq!(
            lost,
            AmrWbFrame {
                bits: vec![],
                mode: 14,
                good: false,
                raw_cmr: 15,
            }
        );
        let bad_sid = depacketize(&[0xf4, 0x84, 0x8d, 0x15, 0x9e, 0x28], false).unwrap();
        assert_eq!(bad_sid.mode, 9);
        assert!(!bad_sid.good);
        assert_eq!(bad_sid.bits, sid);
    }

    #[test]
    fn both_formats_preserve_every_raw_cmr_without_changing_the_speech_frame() {
        let mut bits = vec![0x5a; frame_bits(8).unwrap().div_ceil(8)];
        *bits.last_mut().unwrap() &= 0xf8;
        for octet_aligned in [false, true] {
            for raw_cmr in 0..=15 {
                let mut payload = packetize(&bits, 8, octet_aligned).unwrap();
                payload[0] = (raw_cmr << 4) | (payload[0] & 0x0f);
                assert_eq!(
                    depacketize(&payload, octet_aligned).unwrap(),
                    AmrWbFrame {
                        bits: bits.clone(),
                        mode: 8,
                        good: true,
                        raw_cmr,
                    },
                    "CMR {raw_cmr}, octet_aligned {octet_aligned}"
                );
            }
        }
    }

    #[test]
    fn malformed_headers_and_lengths_are_rejected() {
        assert_eq!(
            depacketize(&[0xf0], true),
            Err(AmrWbPayloadError::ShortPayload)
        );
        assert_eq!(
            depacketize(&[0xf1, 0x7c], true),
            Err(AmrWbPayloadError::UnsupportedOctetAlignedHeader)
        );
        assert_eq!(
            depacketize(&[0xf0, 0xfc], true),
            Err(AmrWbPayloadError::UnsupportedOctetAlignedHeader)
        );
        assert_eq!(
            depacketize(&[0xf8, 0x40], false),
            Err(AmrWbPayloadError::MultipleFramesUnsupported)
        );
        assert_eq!(
            depacketize(&[0xf5, 0x00], false),
            Err(AmrWbPayloadError::UnsupportedFrameType(10))
        );
        assert_eq!(
            depacketize(&[0xf0, 0x4c], true),
            Err(AmrWbPayloadError::InvalidFrameSize {
                expected: 7,
                actual: 2
            })
        );
        assert_eq!(
            depacketize(&[0xf4, 0xc0], false),
            Err(AmrWbPayloadError::InvalidFrameSize {
                expected: 6,
                actual: 2
            })
        );
        assert_eq!(
            packetize(&[0; 4], 9, false),
            Err(AmrWbPayloadError::InvalidFrameBitsLength {
                expected: 5,
                actual: 4
            })
        );
        assert_eq!(
            packetize(&[], 10, true),
            Err(AmrWbPayloadError::UnsupportedFrameType(10))
        );
    }
}
