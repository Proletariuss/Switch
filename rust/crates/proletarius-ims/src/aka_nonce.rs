//! IMS AKA nonce decoding matching the deployed Go simauth compatibility.
//! RAND/AUTN are never formatted or logged; optional server data is not sent
//! to the SIM. The original nonce string must still be used for Digest math.
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
};
use zeroize::Zeroizing;

pub struct AkaNonce {
    pub rand: Zeroizing<[u8; 16]>,
    pub autn: Zeroizing<[u8; 16]>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum NonceError {
    Empty,
    TooLong,
    Malformed,
    TooShort,
}

const MAX_AKA_NONCE_LEN: usize = 8192;

pub fn decode_aka_nonce(value: &str) -> Result<AkaNonce, NonceError> {
    if value.len() > MAX_AKA_NONCE_LEN {
        return Err(NonceError::TooLong);
    }
    let value = value.trim();
    if value.is_empty() {
        return Err(NonceError::Empty);
    }
    // Preserve deployed hex-first precedence, including ambiguous strings.
    let raw = if value.len().is_multiple_of(2) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut bytes = Zeroizing::new(Vec::with_capacity(value.len() / 2));
        let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
        debug_assert!(remainder.is_empty());
        for pair in pairs {
            let nibble = |b: u8| {
                if b.is_ascii_digit() {
                    b - b'0'
                } else {
                    b.to_ascii_lowercase() - b'a' + 10
                }
            };
            bytes.push(nibble(pair[0]) * 16 + nibble(pair[1]));
        }
        bytes
    } else {
        let mut decoded = None;
        for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
            if let Ok(bytes) = engine.decode(value) {
                decoded = Some(Zeroizing::new(bytes));
                break;
            }
        }
        decoded.ok_or(NonceError::Malformed)?
    };
    if raw.len() < 32 {
        return Err(NonceError::TooShort);
    }
    Ok(AkaNonce {
        rand: Zeroizing::new(raw[..16].try_into().unwrap()),
        autn: Zeroizing::new(raw[16..32].try_into().unwrap()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deployed_encodings_preserve_rand_autn_and_ignore_server_suffix() {
        let bytes: Vec<u8> = (0..35).map(|i| if i < 16 { 0xfb } else { 0xff }).collect();
        let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        for encoded in [
            hex,
            STANDARD.encode(&bytes),
            STANDARD_NO_PAD.encode(&bytes),
            URL_SAFE.encode(&bytes),
            URL_SAFE_NO_PAD.encode(&bytes),
        ] {
            let decoded = decode_aka_nonce(&encoded).unwrap();
            assert_eq!(*decoded.rand, [0xfb; 16]);
            assert_eq!(*decoded.autn, [0xff; 16]);
        }
        for (value, error) in [
            ("".to_owned(), NonceError::Empty),
            ("!".to_owned(), NonceError::Malformed),
            ("01".repeat(16), NonceError::TooShort),
            ("x".repeat(MAX_AKA_NONCE_LEN + 1), NonceError::TooLong),
        ] {
            assert!(matches!(decode_aka_nonce(&value),Err(actual) if actual==error));
        }
    }
}
