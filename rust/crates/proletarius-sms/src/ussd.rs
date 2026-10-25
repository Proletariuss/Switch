//! Carrier compatibility decoding for text returned by AT+CUSD.
use crate::{alphabet, septets};

/// Decodes a modem-provided USSD text field while preserving its original value
/// when the field is already textual or cannot be decoded safely.
pub fn decode_text(raw: &str, dcs: i32) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let Some(bytes) = decode_hex(raw) else {
        return raw.to_owned();
    };

    if is_ucs2(dcs) {
        if !bytes.len().is_multiple_of(2) {
            return raw.to_owned();
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        return char::decode_utf16(units)
            .map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }

    let septet_count = bytes.len() + bytes.len() / 7;
    let Ok(mut values) = septets::unpack(&bytes, septet_count, 0) else {
        return raw.to_owned();
    };
    if bytes.len() > 1
        && (bytes.last().is_some_and(|last| last >> 1 == 0x0d)
            || (bytes.last() == Some(&0x0d)
                && bytes
                    .get(bytes.len() - 2)
                    .is_some_and(|value| value >> 1 == 0x0d)))
    {
        values.pop();
    }
    alphabet::decode(&values).unwrap_or_else(|_| raw.to_owned())
}

fn is_ucs2(dcs: i32) -> bool {
    let coding_group = (dcs >> 4) & 0x0f;
    let alphabet = (dcs >> 2) & 0x03;
    if coding_group == 0 || coding_group == 1 {
        alphabet == 2
    } else if dcs == 72 {
        true
    } else {
        (0x40..=0x7f).contains(&dcs) && alphabet == 2
    }
}

fn decode_hex(raw: &str) -> Option<Vec<u8>> {
    let value = raw.as_bytes();
    if value.len() % 2 != 0 {
        return None;
    }
    value
        .chunks_exact(2)
        .map(|pair| Some((hex_value(pair[0])? << 4) | hex_value(pair[1])?))
        .collect()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_modem_decoded_text_and_handles_quoted_commas() {
        assert_eq!(decode_text("Balance, £5", 15), "Balance, £5");
    }

    #[test]
    fn decodes_packed_gsm7_and_extension_table_values() {
        assert_eq!(decode_text("c22013", 15), "BAL");
        assert_eq!(decode_text("c22013", 0), "BAL");
    }

    #[test]
    fn decodes_ucs2_and_replaces_unpaired_surrogates_like_go() {
        assert_eq!(decode_text("00480069", 72), "Hi");
        assert_eq!(decode_text("D800", 72), "\u{fffd}");
        assert_eq!(decode_text("D83DDE00", 72), "😀");
    }

    #[test]
    fn malformed_hex_or_gsm_payload_falls_back_to_original_text() {
        assert_eq!(decode_text("004", 72), "004");
        assert_eq!(decode_text("not-hex", 15), "not-hex");
        assert_eq!(decode_text("1B00", 15), "1B00");
    }
}
