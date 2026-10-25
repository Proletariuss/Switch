//! SMS-SUBMIT framing. The caller supplies the concatenation reference.
use crate::{
    SmsError,
    segments::{self, TextSegments},
    septets,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingPolicy {
    Auto,
    Ucs2,
    Modem,
}

pub fn prepare(recipient: &str, text: &str, reference: u8) -> Result<Vec<Vec<u8>>, SmsError> {
    prepare_with_policy(recipient, text, reference, EncodingPolicy::Auto, true)
}

pub fn prepare_with_policy(
    recipient: &str,
    text: &str,
    reference: u8,
    encoding: EncodingPolicy,
    request_status_report: bool,
) -> Result<Vec<Vec<u8>>, SmsError> {
    let mut normalized = String::with_capacity(21);
    for ch in recipient.trim().chars() {
        match ch {
            '0'..='9' => normalized.push(ch),
            '+' if normalized.is_empty() => normalized.push(ch),
            ' ' | '-' | '(' | ')' => {}
            _ => return Err(SmsError::InvalidAddress),
        }
        if normalized.len() > 21 {
            return Err(SmsError::InvalidAddress);
        }
    }
    let digits = normalized
        .strip_prefix('+')
        .unwrap_or(&normalized)
        .as_bytes();
    if digits.is_empty() || digits.len() > 20 {
        return Err(SmsError::InvalidAddress);
    }
    let mut prefix = vec![
        if request_status_report { 0x21 } else { 0x01 },
        0,
        digits.len() as u8,
        if normalized.starts_with('+') {
            0x91
        } else {
            0x81
        },
    ];
    for pair in digits.chunks(2) {
        prefix.push((pair.get(1).map_or(15, |b| b - b'0') << 4) | (pair[0] - b'0'));
    }
    prefix.push(0);
    let parts = match encoding {
        EncodingPolicy::Auto => segments::split(text)?,
        EncodingPolicy::Ucs2 => segments::split_ucs2(text)?,
        EncodingPolicy::Modem if !text.is_ascii() => segments::split_ucs2(text)?,
        EncodingPolicy::Modem => segments::split(text)?,
    };
    let total = match &parts {
        TextSegments::Gsm7(p) => p.len(),
        TextSegments::Utf16(p) => p.len(),
    };
    let mut result = Vec::with_capacity(total);
    for index in 0..total {
        let header = if total > 1 {
            vec![5, 0, 3, reference, total as u8, (index + 1) as u8]
        } else {
            vec![]
        };
        let mut tpdu = prefix.clone();
        if !request_status_report {
            // Preserve the modem backend encoder's per-segment TP-MR sequence.
            tpdu[1] = (index + 1) as u8;
        }
        if total > 1 {
            tpdu[0] |= 0x40;
        }
        match &parts {
            TextSegments::Gsm7(parts) => {
                let header_septets = (header.len() * 8).div_ceil(7);
                let mut data = septets::pack(&parts[index], header_septets * 7)?;
                data[..header.len()].copy_from_slice(&header);
                tpdu.extend_from_slice(&[0, (header_septets + parts[index].len()) as u8]);
                tpdu.extend(data);
            }
            TextSegments::Utf16(parts) => {
                tpdu.extend_from_slice(&[8, (header.len() + parts[index].len() * 2) as u8]);
                tpdu.extend(header);
                for unit in &parts[index] {
                    tpdu.extend_from_slice(&unit.to_be_bytes());
                }
            }
        }
        result.push(tpdu);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn balance_and_unicode_wire_vectors() {
        assert_eq!(
            prepare("888", "BAL", 42).unwrap(),
            vec![vec![
                0x21, 0, 3, 0x81, 0x88, 0xf8, 0, 0, 3, 0xc2, 0x20, 0x13
            ]]
        );
        assert_eq!(
            prepare("888", "中", 42).unwrap(),
            vec![vec![0x21, 0, 3, 0x81, 0x88, 0xf8, 0, 8, 2, 0x4e, 0x2d]]
        );
    }
    #[test]
    fn concatenation_header_and_payload_limits() {
        for text in ["A".repeat(161), "中".repeat(71), "😀".repeat(36)] {
            let parts = prepare("+44 (7700) 900123", &text, 42).unwrap();
            assert_eq!(parts.len(), 2);
            for (index, part) in parts.iter().enumerate() {
                assert_eq!(part[0], 0x61);
                assert_eq!(&part[13..19], &[5, 0, 3, 42, 2, (index + 1) as u8]);
                assert!(part.len() <= 153);
            }
        }
        assert!(prepare("+", "BAL", 0).is_err());
        assert!(prepare("8x88", "BAL", 0).is_err());
        assert!(prepare(&"8".repeat(100_000), "BAL", 0).is_err());
        assert!(prepare("123456789012345678901", "BAL", 0).is_err());
        assert!(prepare("+12345678901234567890", "BAL", 0).is_ok());
    }

    #[test]
    fn modem_policy_matches_legacy_unicode_selection_without_status_report() {
        let ascii = prepare_with_policy("888", "BAL", 42, EncodingPolicy::Modem, false).unwrap();
        assert_eq!(
            ascii[0],
            vec![0x01, 1, 3, 0x81, 0x88, 0xf8, 0, 0, 3, 0xc2, 0x20, 0x13]
        );

        let modem_default =
            prepare_with_policy("10086", "€", 42, EncodingPolicy::Modem, false).unwrap();
        let forced_ucs2 =
            prepare_with_policy("10086", "€", 42, EncodingPolicy::Ucs2, false).unwrap();
        assert_eq!(modem_default, forced_ucs2);
        assert_eq!(modem_default[0][0], 0x01);
        assert_eq!(modem_default[0][1], 1);
        assert_eq!(modem_default[0][8], 0x08);
    }
}
