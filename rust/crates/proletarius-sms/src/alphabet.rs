//! Default GSM alphabet and extension table, matching the existing TPDU codec.
const DEFAULT: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞ\u{1b}ÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";
const EXTENSION: &[(u8, char)] = &[
    (10, '\u{c}'),
    (20, '^'),
    (40, '{'),
    (41, '}'),
    (47, '\\'),
    (60, '['),
    (61, '~'),
    (62, ']'),
    (64, '|'),
    (101, '€'),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphabetError {
    UnsupportedCharacter,
    InvalidCode,
    TrailingEscape,
    UnknownExtension,
}

pub fn encode(text: &str) -> Result<Vec<u8>, AlphabetError> {
    let mut result = Vec::new();
    for ch in text.chars() {
        if let Some(index) = DEFAULT
            .chars()
            .position(|value| value == ch && ch != '\u{1b}')
        {
            result.push(index as u8);
        } else if let Some(&(code, _)) = EXTENSION.iter().find(|(_, value)| *value == ch) {
            result.extend_from_slice(&[27, code]);
        } else {
            return Err(AlphabetError::UnsupportedCharacter);
        }
    }
    Ok(result)
}

pub fn decode(values: &[u8]) -> Result<String, AlphabetError> {
    let table: Vec<char> = DEFAULT.chars().collect();
    let mut result = String::new();
    let mut values = values.iter();
    while let Some(&code) = values.next() {
        let ch = if code == 27 {
            let extension = values.next().ok_or(AlphabetError::TrailingEscape)?;
            EXTENSION
                .iter()
                .find(|(value, _)| value == extension)
                .map(|(_, ch)| *ch)
                .ok_or(AlphabetError::UnknownExtension)?
        } else {
            *table
                .get(usize::from(code))
                .ok_or(AlphabetError::InvalidCode)?
        };
        result.push(ch);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_default_and_extension_character_roundtrips() {
        assert_eq!(DEFAULT.chars().count(), 128);
        for (index, ch) in DEFAULT.chars().enumerate() {
            if index == 27 {
                continue;
            }
            assert_eq!(encode(&ch.to_string()).unwrap(), [index as u8]);
            assert_eq!(decode(&[index as u8]).unwrap(), ch.to_string());
        }
        for &(code, ch) in EXTENSION {
            assert_eq!(encode(&ch.to_string()).unwrap(), [27, code]);
            assert_eq!(decode(&[27, code]).unwrap(), ch.to_string());
        }
    }
    #[test]
    fn special_characters_are_not_silently_replaced() {
        assert_eq!(encode("BAL").unwrap(), b"BAL");
        assert_eq!(encode("中文"), Err(AlphabetError::UnsupportedCharacter));
        assert_eq!(encode("\u{1b}"), Err(AlphabetError::UnsupportedCharacter));
        assert_eq!(decode(&[27]), Err(AlphabetError::TrailingEscape));
        assert_eq!(decode(&[27, 0]), Err(AlphabetError::UnknownExtension));
        assert_eq!(decode(&[128]), Err(AlphabetError::InvalidCode));
    }
}
