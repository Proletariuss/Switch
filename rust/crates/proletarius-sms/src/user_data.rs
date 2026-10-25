//! Strict user-data decoding; carrier-specific mislabeled-text recovery is separate.
use crate::{alphabet, septets, udh};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coding {
    Gsm7,
    Binary,
    Utf16,
    Unsupported,
}

pub fn coding(dcs: u8) -> Coding {
    if dcs & 0x80 == 0 {
        if dcs & 0x20 != 0 {
            return Coding::Unsupported;
        }
        match (dcs >> 2) & 3 {
            0 => Coding::Gsm7,
            1 => Coding::Binary,
            2 => Coding::Utf16,
            _ => Coding::Unsupported,
        }
    } else if dcs & 0xe0 == 0xc0 {
        Coding::Gsm7
    } else if dcs & 0xf0 == 0xe0 {
        Coding::Utf16
    } else if dcs & 0xf0 == 0xf0 {
        if dcs & 4 != 0 {
            Coding::Binary
        } else {
            Coding::Gsm7
        }
    } else {
        Coding::Unsupported
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Binary(Vec<u8>),
}
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    UnsupportedCoding,
    UnsupportedLanguage,
    InvalidLength,
    InvalidText,
}

pub fn decode(dcs: u8, udl: u8, has_header: bool, data: &[u8]) -> Result<Content, DecodeError> {
    let coding = coding(dcs);
    if coding == Coding::Unsupported {
        return Err(DecodeError::UnsupportedCoding);
    }
    let length = usize::from(udl);
    let bytes = if coding == Coding::Gsm7 {
        (length * 7).div_ceil(8)
    } else {
        length
    };
    if bytes > 140 || bytes != data.len() {
        return Err(DecodeError::InvalidLength);
    }
    let header = if has_header {
        Some(udh::parse(data).map_err(|_| DecodeError::InvalidLength)?)
    } else {
        None
    };
    let skip = header.as_ref().map_or(0, |h| h.octets);
    match coding {
        Coding::Gsm7 => {
            if header
                .as_ref()
                .is_some_and(|h| h.elements.iter().any(|(id, _)| *id == 0x24 || *id == 0x25))
            {
                return Err(DecodeError::UnsupportedLanguage);
            }
            let header_units = (skip * 8).div_ceil(7);
            let count = length
                .checked_sub(header_units)
                .ok_or(DecodeError::InvalidLength)?;
            let values = septets::unpack(data, count, header_units * 7)
                .map_err(|_| DecodeError::InvalidLength)?;
            alphabet::decode(&values)
                .map(Content::Text)
                .map_err(|_| DecodeError::InvalidText)
        }
        Coding::Utf16 => {
            let payload = &data[skip..];
            if !payload.len().is_multiple_of(2) {
                return Err(DecodeError::InvalidLength);
            }
            let units: Vec<u16> = payload
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| u16::from_be_bytes([p[0], p[1]]))
                .collect();
            String::from_utf16(&units)
                .map(Content::Text)
                .map_err(|_| DecodeError::InvalidText)
        }
        Coding::Binary => Ok(Content::Binary(data[skip..].to_vec())),
        Coding::Unsupported => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dcs_and_declared_length_is_bounded() {
        let payload = [0_u8; 140];
        for dcs in 0..=255 {
            for udl in 0..=255 {
                let expected = if coding(dcs) == Coding::Gsm7 {
                    (usize::from(udl) * 7).div_ceil(8)
                } else {
                    usize::from(udl)
                };
                let available = expected.min(payload.len());
                let result = decode(dcs, udl, false, &payload[..available]);
                if coding(dcs) == Coding::Unsupported || expected > 140 {
                    assert!(result.is_err(), "accepted dcs={dcs} udl={udl}");
                }
                if available > 0 {
                    assert!(decode(dcs, udl, false, &payload[..available - 1]).is_err());
                }
            }
        }
    }

    #[test]
    fn submission_parts_decode_back_to_original_text() {
        for text in [
            "BAL".to_string(),
            "£€{}[]\\~|^".to_string(),
            "A".repeat(161),
            "中".repeat(71),
            "😀".repeat(36),
            format!("{}^{}", "A".repeat(152), "B".repeat(8)),
        ] {
            let parts = crate::submit::prepare("888", &text, 42).unwrap();
            let mut reconstructed = String::new();
            for part in parts {
                let dcs_offset = 5 + usize::from(part[2]).div_ceil(2);
                let content = decode(
                    part[dcs_offset],
                    part[dcs_offset + 1],
                    part[0] & 0x40 != 0,
                    &part[dcs_offset + 2..],
                )
                .unwrap();
                let Content::Text(value) = content else {
                    panic!("text became binary")
                };
                reconstructed.push_str(&value);
            }
            assert_eq!(reconstructed, text);
        }
    }

    #[test]
    fn invalid_header_fill_and_language_are_explicit() {
        assert_eq!(decode(0, 1, true, &[0]), Err(DecodeError::InvalidLength));
        assert_eq!(
            decode(0, 6, true, &[3, 0x24, 1, 1, 0, 0]),
            Err(DecodeError::UnsupportedLanguage)
        );
        assert_eq!(decode(8, 1, false, &[0]), Err(DecodeError::InvalidLength));
        assert_eq!(decode(0, 0, false, &[0]), Err(DecodeError::InvalidLength));
    }
    #[test]
    fn coding_groups_do_not_confuse_waiting_messages() {
        for code in 0xc0..=0xdf {
            assert_eq!(coding(code), Coding::Gsm7);
        }
        for code in 0xe0..=0xef {
            assert_eq!(coding(code), Coding::Utf16);
        }
        for code in 0x80..=0xbf {
            assert_eq!(coding(code), Coding::Unsupported);
        }
        assert_eq!(coding(0x20), Coding::Unsupported);
        assert_eq!(coding(0xf4), Coding::Binary);
    }
    #[test]
    fn decodes_text_and_preserves_binary() {
        assert_eq!(
            decode(0, 3, false, &[0xc2, 0x20, 0x13]),
            Ok(Content::Text("BAL".into()))
        );
        assert_eq!(
            decode(8, 2, false, &[0x4e, 0x2d]),
            Ok(Content::Text("中".into()))
        );
        assert_eq!(
            decode(4, 2, false, &[0xff, 0]),
            Ok(Content::Binary(vec![0xff, 0]))
        );
        assert!(decode(8, 2, false, &[0xd8, 0]).is_err());
        assert!(decode(0, 3, false, &[0xc2]).is_err());
        assert!(decode(0, 0, true, &[]).is_err());
    }
}
