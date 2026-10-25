//! Text segmentation prior to TPDU framing. No device or transport operations.
use crate::{SmsError, alphabet};

#[derive(Debug, PartialEq, Eq)]
pub enum TextSegments {
    Gsm7(Vec<Vec<u8>>),
    Utf16(Vec<Vec<u16>>),
}

pub fn split(text: &str) -> Result<TextSegments, SmsError> {
    split_with_ucs2_policy(text, false)
}

pub fn split_ucs2(text: &str) -> Result<TextSegments, SmsError> {
    split_with_ucs2_policy(text, true)
}

fn split_with_ucs2_policy(text: &str, force_ucs2: bool) -> Result<TextSegments, SmsError> {
    // Bound work before allocating intermediate character buffers.
    if text.is_empty() || text.contains('\0') {
        return Err(SmsError::InvalidUserDataLength);
    }
    if text.len() > 255 * 153 * 4 {
        return Err(SmsError::MessageTooLarge);
    }
    if !force_ucs2 && let Ok(values) = alphabet::encode(text) {
        if values.len() <= 160 {
            return Ok(TextSegments::Gsm7(vec![values]));
        }
        let mut remaining = values.as_slice();
        let mut parts = Vec::new();
        while !remaining.is_empty() {
            let mut count = remaining.len().min(153);
            if count < remaining.len() && remaining[count - 1] == 27 {
                count -= 1;
            }
            parts.push(remaining[..count].to_vec());
            if parts.len() > 255 {
                return Err(SmsError::MessageTooLarge);
            }
            remaining = &remaining[count..];
        }
        return Ok(TextSegments::Gsm7(parts));
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= 70 {
        return Ok(TextSegments::Utf16(vec![units]));
    }
    let mut remaining = units.as_slice();
    let mut parts = Vec::new();
    while !remaining.is_empty() {
        let mut count = remaining.len().min(67);
        if count < remaining.len() && (0xd800..=0xdbff).contains(&remaining[count - 1]) {
            count -= 1;
        }
        parts.push(remaining[..count].to_vec());
        if parts.len() > 255 {
            return Err(SmsError::MessageTooLarge);
        }
        remaining = &remaining[count..];
    }
    Ok(TextSegments::Utf16(parts))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gsm_extension_stays_with_its_escape() {
        let text = format!("{}^{}", "A".repeat(152), "B".repeat(8));
        let TextSegments::Gsm7(parts) = split(&text).unwrap() else {
            panic!("wrong alphabet")
        };
        assert_eq!(parts.iter().map(Vec::len).collect::<Vec<_>>(), [152, 10]);
        assert_eq!(alphabet::decode(&parts.concat()).unwrap(), text);
        for part in parts {
            assert!(alphabet::decode(&part).is_ok());
        }
    }
    #[test]
    fn surrogate_pairs_stay_together() {
        let text = format!("{}😀{}", "中".repeat(66), "文".repeat(4));
        let TextSegments::Utf16(parts) = split(&text).unwrap() else {
            panic!("wrong alphabet")
        };
        assert_eq!(parts.iter().map(Vec::len).collect::<Vec<_>>(), [66, 6]);
        for part in &parts {
            assert!(String::from_utf16(part).is_ok());
        }
        assert_eq!(String::from_utf16(&parts.concat()).unwrap(), text);
    }
    #[test]
    fn single_part_boundaries_and_capacity() {
        assert!(matches!(split(&"A".repeat(160)).unwrap(),TextSegments::Gsm7(p) if p.len()==1));
        assert!(matches!(split(&"中".repeat(70)).unwrap(),TextSegments::Utf16(p) if p.len()==1));
        assert!(split("").is_err());
        assert!(split("a\0b").is_err());
        assert!(split(&"A".repeat(255 * 153 + 1)).is_err());
    }
}
