//! Bark's optional provider-side AES-CBC envelope.
//!
//! This is separate from the SWF1 queue envelope: Bark requires an ASCII IV,
//! PKCS#7 padding, and standard Base64 fields. It must never be reused for
//! local queue encryption.

use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::NoPadding};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use zeroize::Zeroizing;

const AES_BLOCK_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BarkEncryptionAlgorithm {
    Aes128Cbc,
    Aes256Cbc,
}

impl BarkEncryptionAlgorithm {
    pub fn from_name(value: &str) -> Result<Self, BarkEncryptionError> {
        match value {
            "AES-128-CBC" => Ok(Self::Aes128Cbc),
            "AES-256-CBC" => Ok(Self::Aes256Cbc),
            _ => Err(BarkEncryptionError::UnsupportedAlgorithm),
        }
    }

    const fn key_bytes(self) -> usize {
        match self {
            Self::Aes128Cbc => 16,
            Self::Aes256Cbc => 32,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BarkEncryptionError {
    UnsupportedAlgorithm,
    InvalidKeyLength,
    InvalidPayloadLength,
    EntropyUnavailable,
    EncryptionFailed,
}

impl std::fmt::Display for BarkEncryptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedAlgorithm => "unsupported Bark encryption algorithm",
            Self::InvalidKeyLength => "Bark encryption key length does not match the algorithm",
            Self::InvalidPayloadLength => "Bark encryption payload is too large",
            Self::EntropyUnavailable => "system cryptographic entropy unavailable",
            Self::EncryptionFailed => "Bark encryption failed",
        })
    }
}

impl std::error::Error for BarkEncryptionError {}

/// Encrypts a Bark JSON payload and returns standard Base64 ciphertext plus
/// Bark's printable ASCII IV. The plaintext and expanded padded buffer are
/// zeroized when released.
pub fn encrypt_bark_payload(
    plaintext: &[u8],
    algorithm: BarkEncryptionAlgorithm,
    key: &[u8],
) -> Result<(String, String), BarkEncryptionError> {
    encrypt_bark_payload_with_entropy(plaintext, algorithm, key, |buffer| {
        getrandom::fill(buffer).map_err(|_| BarkEncryptionError::EntropyUnavailable)
    })
}

fn encrypt_bark_payload_with_entropy<F>(
    plaintext: &[u8],
    algorithm: BarkEncryptionAlgorithm,
    key: &[u8],
    mut fill_entropy: F,
) -> Result<(String, String), BarkEncryptionError>
where
    F: FnMut(&mut [u8]) -> Result<(), BarkEncryptionError>,
{
    if key.len() != algorithm.key_bytes() {
        return Err(BarkEncryptionError::InvalidKeyLength);
    }
    let padding_bytes = AES_BLOCK_BYTES - plaintext.len() % AES_BLOCK_BYTES;
    let padded_len = plaintext
        .len()
        .checked_add(padding_bytes)
        .ok_or(BarkEncryptionError::InvalidPayloadLength)?;
    let mut padded = Zeroizing::new(Vec::new());
    padded
        .try_reserve_exact(padded_len)
        .map_err(|_| BarkEncryptionError::InvalidPayloadLength)?;
    padded.extend_from_slice(plaintext);
    padded.resize(padded_len, padding_bytes as u8);

    let mut random_iv = Zeroizing::new([0; AES_BLOCK_BYTES]);
    fill_entropy(random_iv.as_mut())?;
    let mut ascii_iv = Zeroizing::new([0; AES_BLOCK_BYTES]);
    for (target, random) in ascii_iv.iter_mut().zip(random_iv.iter()) {
        // Go's implementation maps each entropy byte into printable ASCII
        // '!' through '~' before passing the IV to Bark.
        *target = b'!' + random % 94;
    }

    let ciphertext = match algorithm {
        BarkEncryptionAlgorithm::Aes128Cbc => {
            cbc::Encryptor::<aes::Aes128>::new_from_slices(key, ascii_iv.as_ref())
                .map_err(|_| BarkEncryptionError::InvalidKeyLength)?
                .encrypt_padded::<NoPadding>(&mut padded, padded_len)
                .map_err(|_| BarkEncryptionError::EncryptionFailed)?
        }
        BarkEncryptionAlgorithm::Aes256Cbc => {
            cbc::Encryptor::<aes::Aes256>::new_from_slices(key, ascii_iv.as_ref())
                .map_err(|_| BarkEncryptionError::InvalidKeyLength)?
                .encrypt_padded::<NoPadding>(&mut padded, padded_len)
                .map_err(|_| BarkEncryptionError::EncryptionFailed)?
        }
    };

    Ok((
        STANDARD.encode(ciphertext),
        String::from_utf8(ascii_iv.to_vec()).expect("Bark IV is printable ASCII"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{BlockModeDecrypt, block_padding::NoPadding};

    fn deterministic_entropy(target: &mut [u8]) -> Result<(), BarkEncryptionError> {
        for (index, byte) in target.iter_mut().enumerate() {
            *byte = index as u8;
        }
        Ok(())
    }

    fn round_trip(algorithm: BarkEncryptionAlgorithm, key: &[u8]) {
        let plaintext = r#"{"title":"Switch 短信","body":"PAC fixture","icon":"https://example.com/switch.png","group":"Switch-SMS"}"#;
        let (ciphertext, iv) = encrypt_bark_payload_with_entropy(
            plaintext.as_bytes(),
            algorithm,
            key,
            deterministic_entropy,
        )
        .unwrap();
        assert_eq!(iv.len(), AES_BLOCK_BYTES);
        assert!(iv.bytes().all(|byte| (b'!'..=b'~').contains(&byte)));
        let mut ciphertext = Zeroizing::new(STANDARD.decode(ciphertext).unwrap());
        let decrypted = match algorithm {
            BarkEncryptionAlgorithm::Aes128Cbc => {
                cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv.as_bytes())
                    .unwrap()
                    .decrypt_padded::<NoPadding>(&mut ciphertext)
                    .unwrap()
            }
            BarkEncryptionAlgorithm::Aes256Cbc => {
                cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv.as_bytes())
                    .unwrap()
                    .decrypt_padded::<NoPadding>(&mut ciphertext)
                    .unwrap()
            }
        };
        let padding_bytes = *decrypted.last().unwrap() as usize;
        assert!((1..=AES_BLOCK_BYTES).contains(&padding_bytes));
        assert!(
            decrypted[decrypted.len() - padding_bytes..]
                .iter()
                .all(|byte| *byte as usize == padding_bytes)
        );
        assert_eq!(
            &decrypted[..decrypted.len() - padding_bytes],
            plaintext.as_bytes()
        );
    }

    #[test]
    fn bark_aes_cbc_round_trips_for_both_key_sizes() {
        round_trip(BarkEncryptionAlgorithm::Aes128Cbc, b"0123456789abcdef");
        round_trip(
            BarkEncryptionAlgorithm::Aes256Cbc,
            b"0123456789abcdef0123456789abcdef",
        );
    }

    #[test]
    fn bark_key_length_and_algorithm_fail_closed() {
        assert_eq!(
            encrypt_bark_payload_with_entropy(
                b"payload",
                BarkEncryptionAlgorithm::Aes128Cbc,
                b"short",
                deterministic_entropy,
            ),
            Err(BarkEncryptionError::InvalidKeyLength)
        );
        assert_eq!(
            BarkEncryptionAlgorithm::from_name("AES-192-CBC"),
            Err(BarkEncryptionError::UnsupportedAlgorithm)
        );
    }

    #[test]
    fn bark_encryptions_use_fresh_iv_and_ciphertext() {
        let plaintext = br#"{"body":"same payload"}"#;
        let key = b"0123456789abcdef";
        let first =
            encrypt_bark_payload(plaintext, BarkEncryptionAlgorithm::Aes128Cbc, key).unwrap();
        let second =
            encrypt_bark_payload(plaintext, BarkEncryptionAlgorithm::Aes128Cbc, key).unwrap();
        assert_ne!(first.0, second.0);
        assert_ne!(first.1, second.1);
    }
}
