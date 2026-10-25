use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt};
use aes::{Aes128, Aes256, Block};
use aes_gcm::aead::{Aead, Nonce, Payload};
use aes_gcm::{Aes128Gcm, Aes256Gcm, KeyInit};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::{
    IkeHeader, IkeProtocolError, PayloadChainError, PayloadFrame, decode_payload_chain,
    encode_payload_chain,
};

const SK_PAYLOAD: u8 = 46;
const AES_CBC: u16 = 12;
const GCM_16: u16 = 20;
const HMAC_SHA2_256_128: u16 = 12;
const HMAC_SHA2_512_256: u16 = 14;
const HMAC_SHA1_96: u16 = 2;
const CBC_BLOCK_LENGTH: usize = 16;
const CBC_IV_LENGTH: usize = 16;
const GCM_IV_LENGTH: usize = 8;
const GCM_SALT_LENGTH: usize = 4;
const GCM_TAG_LENGTH: usize = 16;

#[derive(Debug, Eq, PartialEq)]
pub enum SkPayloadError {
    Ike(IkeProtocolError),
    Payload(PayloadChainError),
    UnsupportedEncryption(u16),
    UnsupportedIntegrity(u16),
    InvalidKeyLength,
    InvalidPacket,
    AuthenticationFailed,
    EntropyUnavailable,
}

impl std::fmt::Display for SkPayloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ike(error) => error.fmt(formatter),
            Self::Payload(error) => error.fmt(formatter),
            Self::UnsupportedEncryption(id) => {
                write!(formatter, "unsupported IKE SK encryption transform {id}")
            }
            Self::UnsupportedIntegrity(id) => {
                write!(formatter, "unsupported IKE SK integrity transform {id}")
            }
            Self::InvalidKeyLength => {
                formatter.write_str("invalid IKE SK encryption or integrity key length")
            }
            Self::InvalidPacket => formatter.write_str("malformed IKE SK payload"),
            Self::AuthenticationFailed => formatter.write_str("IKE SK authentication failed"),
            Self::EntropyUnavailable => {
                formatter.write_str("system cryptographic entropy unavailable")
            }
        }
    }
}

/// Encrypts an IKE encrypted payload (SK) with AES-CBC and an explicit
/// HMAC-SHA1-96 or HMAC-SHA2 integrity transform. The ICV authenticates the complete IKE
/// message through the ciphertext and excludes only the ICV itself.
pub fn seal_ike_sk_cbc(
    header: IkeHeader,
    encryption_transform: u16,
    integrity_transform: u16,
    encryption_key: &[u8],
    integrity_key: &[u8],
    frames: &[PayloadFrame],
) -> Result<Vec<u8>, SkPayloadError> {
    let mut iv = [0_u8; CBC_IV_LENGTH];
    getrandom::fill(&mut iv).map_err(|_| SkPayloadError::EntropyUnavailable)?;
    seal_ike_sk_cbc_with_iv(
        header,
        encryption_transform,
        integrity_transform,
        encryption_key,
        integrity_key,
        frames,
        iv,
    )
}

#[allow(clippy::too_many_arguments)]
fn seal_ike_sk_cbc_with_iv(
    header: IkeHeader,
    encryption_transform: u16,
    integrity_transform: u16,
    encryption_key: &[u8],
    integrity_key: &[u8],
    frames: &[PayloadFrame],
    iv: [u8; CBC_IV_LENGTH],
) -> Result<Vec<u8>, SkPayloadError> {
    let icv_length = validate_cbc(
        encryption_transform,
        encryption_key,
        integrity_transform,
        integrity_key,
    )?;
    let (inner_first_payload, plaintext) = encode_payload_chain(frames)?;
    let mut plaintext = Zeroizing::new(plaintext);
    let length_with_pad_octet = plaintext
        .len()
        .checked_add(1)
        .ok_or(SkPayloadError::InvalidPacket)?;
    let padding_length =
        (CBC_BLOCK_LENGTH - length_with_pad_octet % CBC_BLOCK_LENGTH) % CBC_BLOCK_LENGTH;
    let padded_length = plaintext
        .len()
        .checked_add(padding_length)
        .ok_or(SkPayloadError::InvalidPacket)?;
    plaintext.resize(padded_length, 0);
    plaintext.push(u8::try_from(padding_length).expect("AES block padding is at most 15 bytes"));
    let ciphertext = encrypt_cbc(encryption_key, &iv, &plaintext)?;
    let sk_length = 4_usize
        .checked_add(CBC_IV_LENGTH)
        .and_then(|length| length.checked_add(ciphertext.len()))
        .and_then(|length| length.checked_add(icv_length))
        .filter(|length| *length <= u16::MAX as usize)
        .ok_or(SkPayloadError::InvalidPacket)?;

    let mut packet = header.encode(&[])?;
    packet[16] = SK_PAYLOAD;
    packet[24..28].copy_from_slice(
        &(28_u32 + u32::try_from(sk_length).map_err(|_| SkPayloadError::InvalidPacket)?)
            .to_be_bytes(),
    );
    packet.extend_from_slice(&[inner_first_payload, 0]);
    packet.extend_from_slice(&(sk_length as u16).to_be_bytes());
    packet.extend_from_slice(&iv);
    packet.extend_from_slice(&ciphertext);
    let icv = compute_integrity(integrity_transform, integrity_key, &packet)?;
    packet.extend_from_slice(&icv[..icv_length]);
    Ok(packet)
}

/// Authenticates and decrypts an AES-CBC/HMAC IKE SK packet, then
/// parses its inner payload chain. The ICV is compared in constant time and
/// is always verified before any CBC decryption or padding processing.
pub fn open_ike_sk_cbc(
    packet: &[u8],
    encryption_transform: u16,
    integrity_transform: u16,
    encryption_key: &[u8],
    integrity_key: &[u8],
) -> Result<(IkeHeader, Vec<PayloadFrame>), SkPayloadError> {
    let icv_length = validate_cbc(
        encryption_transform,
        encryption_key,
        integrity_transform,
        integrity_key,
    )?;
    let (header, body) = IkeHeader::decode(packet)?;
    let minimum_length = 4 + CBC_IV_LENGTH + CBC_BLOCK_LENGTH + icv_length;
    if header.next_payload != SK_PAYLOAD || body.len() < minimum_length {
        return Err(SkPayloadError::InvalidPacket);
    }
    let payload_length = usize::from(u16::from_be_bytes([body[2], body[3]]));
    if payload_length != body.len() || body[1] & 0x7f != 0 {
        return Err(SkPayloadError::InvalidPacket);
    }
    let ciphertext_length = body
        .len()
        .checked_sub(4 + CBC_IV_LENGTH + icv_length)
        .ok_or(SkPayloadError::InvalidPacket)?;
    if ciphertext_length == 0 || !ciphertext_length.is_multiple_of(CBC_BLOCK_LENGTH) {
        return Err(SkPayloadError::InvalidPacket);
    }
    let authenticated_length = packet
        .len()
        .checked_sub(icv_length)
        .ok_or(SkPayloadError::InvalidPacket)?;
    let received_icv = &packet[authenticated_length..];
    verify_integrity(
        integrity_transform,
        integrity_key,
        &packet[..authenticated_length],
        received_icv,
    )?;

    let inner_first_payload = body[0];
    let iv_start = 4;
    let ciphertext_start = iv_start + CBC_IV_LENGTH;
    let iv: &[u8; CBC_IV_LENGTH] = body[iv_start..ciphertext_start]
        .try_into()
        .map_err(|_| SkPayloadError::InvalidPacket)?;
    let ciphertext = &body[ciphertext_start..ciphertext_start + ciphertext_length];
    let plaintext = decrypt_cbc(encryption_key, iv, ciphertext)?;
    let mut plaintext = Zeroizing::new(plaintext);
    let padding_length = usize::from(*plaintext.last().ok_or(SkPayloadError::InvalidPacket)?);
    let inner_length = plaintext
        .len()
        .checked_sub(padding_length + 1)
        .ok_or(SkPayloadError::InvalidPacket)?;
    plaintext.truncate(inner_length);
    let frames = decode_payload_chain(inner_first_payload, &plaintext)?;
    Ok((header, frames))
}

impl std::error::Error for SkPayloadError {}

impl From<IkeProtocolError> for SkPayloadError {
    fn from(value: IkeProtocolError) -> Self {
        Self::Ike(value)
    }
}

impl From<PayloadChainError> for SkPayloadError {
    fn from(value: PayloadChainError) -> Self {
        Self::Payload(value)
    }
}

/// Encrypts an IKE encrypted payload (SK) for the negotiated AES-GCM-16
/// transform. The 4-byte salt is the tail of SK_ei; the per-packet 8-byte IV
/// is generated from the OS CSPRNG. The outer IKE header is authenticated as
/// AAD together with the SK generic header, per RFC 5282 section 5.1.
pub fn seal_ike_sk_gcm(
    header: IkeHeader,
    encryption_transform: u16,
    encryption_key: &[u8],
    frames: &[PayloadFrame],
) -> Result<Vec<u8>, SkPayloadError> {
    let mut iv = [0_u8; GCM_IV_LENGTH];
    getrandom::fill(&mut iv).map_err(|_| SkPayloadError::EntropyUnavailable)?;
    seal_ike_sk_gcm_with_iv(header, encryption_transform, encryption_key, frames, iv)
}

fn seal_ike_sk_gcm_with_iv(
    header: IkeHeader,
    encryption_transform: u16,
    encryption_key: &[u8],
    frames: &[PayloadFrame],
    iv: [u8; GCM_IV_LENGTH],
) -> Result<Vec<u8>, SkPayloadError> {
    validate_gcm(encryption_transform, encryption_key)?;
    let (inner_first_payload, plaintext) = encode_payload_chain(frames)?;
    let mut plaintext = Zeroizing::new(plaintext);
    // GCM needs no alignment padding, but Pad Length is always present.
    plaintext.push(0);
    let ciphertext_length = plaintext
        .len()
        .checked_add(GCM_TAG_LENGTH)
        .ok_or(SkPayloadError::InvalidPacket)?;
    let sk_length = 4_usize
        .checked_add(GCM_IV_LENGTH)
        .and_then(|length| length.checked_add(ciphertext_length))
        .filter(|length| *length <= u16::MAX as usize)
        .ok_or(SkPayloadError::InvalidPacket)?;

    let mut sk_body = Vec::with_capacity(sk_length);
    sk_body.extend_from_slice(&[inner_first_payload, 0]);
    sk_body.extend_from_slice(&(sk_length as u16).to_be_bytes());
    sk_body.extend_from_slice(&iv);
    sk_body.resize(sk_length, 0);

    let mut authenticated_header = header.encode(&[])?;
    authenticated_header[16] = SK_PAYLOAD;
    authenticated_header[24..28].copy_from_slice(
        &(28_u32 + u32::try_from(sk_length).map_err(|_| SkPayloadError::InvalidPacket)?)
            .to_be_bytes(),
    );
    let mut aad = authenticated_header.clone();
    aad.extend_from_slice(&sk_body[..4]);
    let mut nonce = [0_u8; 12];
    nonce[..GCM_SALT_LENGTH]
        .copy_from_slice(&encryption_key[encryption_key.len() - GCM_SALT_LENGTH..]);
    nonce[GCM_SALT_LENGTH..].copy_from_slice(&iv);
    let ciphertext = encrypt_gcm(encryption_key, &nonce, &aad, &plaintext)?;
    nonce.fill(0);
    if ciphertext.len() != ciphertext_length {
        return Err(SkPayloadError::InvalidPacket);
    }
    sk_body.truncate(4 + GCM_IV_LENGTH);
    sk_body.extend_from_slice(&ciphertext);
    let mut packet = authenticated_header;
    packet.extend_from_slice(&sk_body);
    Ok(packet)
}

/// Authenticates and decrypts an AES-GCM-16 IKE SK packet, then parses its
/// inner generic payload chain. Authentication is verified before parsing.
pub fn open_ike_sk_gcm(
    packet: &[u8],
    encryption_transform: u16,
    encryption_key: &[u8],
) -> Result<(IkeHeader, Vec<PayloadFrame>), SkPayloadError> {
    validate_gcm(encryption_transform, encryption_key)?;
    let (header, body) = IkeHeader::decode(packet)?;
    if header.next_payload != SK_PAYLOAD || body.len() < 4 + GCM_IV_LENGTH + GCM_TAG_LENGTH {
        return Err(SkPayloadError::InvalidPacket);
    }
    let payload_length = usize::from(u16::from_be_bytes([body[2], body[3]]));
    if payload_length != body.len() || body[1] & 0x7f != 0 {
        return Err(SkPayloadError::InvalidPacket);
    }
    let inner_first_payload = body[0];
    let iv = &body[4..4 + GCM_IV_LENGTH];
    let ciphertext = &body[4 + GCM_IV_LENGTH..];
    let mut nonce = [0_u8; 12];
    nonce[..GCM_SALT_LENGTH]
        .copy_from_slice(&encryption_key[encryption_key.len() - GCM_SALT_LENGTH..]);
    nonce[GCM_SALT_LENGTH..].copy_from_slice(iv);
    let plaintext = decrypt_gcm(encryption_key, &nonce, &packet[..32], ciphertext)?;
    nonce.fill(0);
    let mut plaintext = Zeroizing::new(plaintext);
    let padding = usize::from(*plaintext.last().ok_or(SkPayloadError::InvalidPacket)?);
    let inner_len = plaintext
        .len()
        .checked_sub(padding + 1)
        .ok_or(SkPayloadError::InvalidPacket)?;
    plaintext.truncate(inner_len);
    let frames = decode_payload_chain(inner_first_payload, &plaintext)?;
    Ok((header, frames))
}

fn validate_gcm(transform: u16, key: &[u8]) -> Result<(), SkPayloadError> {
    if transform != GCM_16 {
        return Err(SkPayloadError::UnsupportedEncryption(transform));
    }
    if key.len() != 16 + GCM_SALT_LENGTH && key.len() != 32 + GCM_SALT_LENGTH {
        return Err(SkPayloadError::InvalidKeyLength);
    }
    Ok(())
}

fn validate_cbc(
    encryption_transform: u16,
    encryption_key: &[u8],
    integrity_transform: u16,
    integrity_key: &[u8],
) -> Result<usize, SkPayloadError> {
    if encryption_transform != AES_CBC {
        return Err(SkPayloadError::UnsupportedEncryption(encryption_transform));
    }
    if encryption_key.len() != 16 && encryption_key.len() != 32 {
        return Err(SkPayloadError::InvalidKeyLength);
    }
    let (key_length, icv_length) = match integrity_transform {
        HMAC_SHA1_96 => (20, 12),
        HMAC_SHA2_256_128 => (32, 16),
        HMAC_SHA2_512_256 => (64, 32),
        _ => {
            return Err(SkPayloadError::UnsupportedIntegrity(integrity_transform));
        }
    };
    if integrity_key.len() != key_length {
        return Err(SkPayloadError::InvalidKeyLength);
    }
    Ok(icv_length)
}

fn compute_integrity(
    integrity_transform: u16,
    integrity_key: &[u8],
    authenticated: &[u8],
) -> Result<Zeroizing<Vec<u8>>, SkPayloadError> {
    let digest = match integrity_transform {
        HMAC_SHA1_96 => {
            let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(integrity_key)
                .map_err(|_| SkPayloadError::InvalidKeyLength)?;
            mac.update(authenticated);
            mac.finalize().into_bytes().to_vec()
        }
        HMAC_SHA2_256_128 => {
            let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(integrity_key)
                .map_err(|_| SkPayloadError::InvalidKeyLength)?;
            mac.update(authenticated);
            mac.finalize().into_bytes().to_vec()
        }
        HMAC_SHA2_512_256 => {
            let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(integrity_key)
                .map_err(|_| SkPayloadError::InvalidKeyLength)?;
            mac.update(authenticated);
            mac.finalize().into_bytes().to_vec()
        }
        _ => {
            return Err(SkPayloadError::UnsupportedIntegrity(integrity_transform));
        }
    };
    Ok(Zeroizing::new(digest))
}

fn verify_integrity(
    integrity_transform: u16,
    integrity_key: &[u8],
    authenticated: &[u8],
    received: &[u8],
) -> Result<(), SkPayloadError> {
    let expected_length = match integrity_transform {
        HMAC_SHA1_96 => 12,
        HMAC_SHA2_256_128 => 16,
        HMAC_SHA2_512_256 => 32,
        _ => {
            return Err(SkPayloadError::UnsupportedIntegrity(integrity_transform));
        }
    };
    if received.len() != expected_length {
        return Err(SkPayloadError::InvalidPacket);
    }
    let computed = compute_integrity(integrity_transform, integrity_key, authenticated)?;
    if bool::from(computed[..expected_length].ct_eq(received)) {
        Ok(())
    } else {
        Err(SkPayloadError::AuthenticationFailed)
    }
}

fn encrypt_cbc(
    key: &[u8],
    iv: &[u8; CBC_IV_LENGTH],
    plaintext: &[u8],
) -> Result<Vec<u8>, SkPayloadError> {
    if plaintext.is_empty() || !plaintext.len().is_multiple_of(CBC_BLOCK_LENGTH) {
        return Err(SkPayloadError::InvalidPacket);
    }
    macro_rules! encrypt_blocks {
        ($cipher:expr) => {{
            let cipher = $cipher;
            let mut previous = *iv;
            let mut ciphertext = Vec::with_capacity(plaintext.len());
            for chunk in plaintext.chunks_exact(CBC_BLOCK_LENGTH) {
                let mut block = Block::default();
                block.copy_from_slice(chunk);
                for index in 0..CBC_BLOCK_LENGTH {
                    block[index] ^= previous[index];
                }
                cipher.encrypt_block(&mut block);
                ciphertext.extend_from_slice(&block);
                previous.copy_from_slice(&block);
            }
            Ok(ciphertext)
        }};
    }
    match key.len() {
        16 => encrypt_blocks!(
            Aes128::new_from_slice(key).map_err(|_| SkPayloadError::InvalidKeyLength)?
        ),
        32 => encrypt_blocks!(
            Aes256::new_from_slice(key).map_err(|_| SkPayloadError::InvalidKeyLength)?
        ),
        _ => Err(SkPayloadError::InvalidKeyLength),
    }
}

fn decrypt_cbc(
    key: &[u8],
    iv: &[u8; CBC_IV_LENGTH],
    ciphertext: &[u8],
) -> Result<Vec<u8>, SkPayloadError> {
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(CBC_BLOCK_LENGTH) {
        return Err(SkPayloadError::InvalidPacket);
    }
    macro_rules! decrypt_blocks {
        ($cipher:expr) => {{
            let cipher = $cipher;
            let mut previous = *iv;
            let mut plaintext = Vec::with_capacity(ciphertext.len());
            for chunk in ciphertext.chunks_exact(CBC_BLOCK_LENGTH) {
                let mut encrypted = [0_u8; CBC_BLOCK_LENGTH];
                encrypted.copy_from_slice(chunk);
                let mut block = Block::default();
                block.copy_from_slice(chunk);
                cipher.decrypt_block(&mut block);
                for index in 0..CBC_BLOCK_LENGTH {
                    block[index] ^= previous[index];
                }
                plaintext.extend_from_slice(&block);
                previous = encrypted;
            }
            Ok(plaintext)
        }};
    }
    match key.len() {
        16 => decrypt_blocks!(
            Aes128::new_from_slice(key).map_err(|_| SkPayloadError::InvalidKeyLength)?
        ),
        32 => decrypt_blocks!(
            Aes256::new_from_slice(key).map_err(|_| SkPayloadError::InvalidKeyLength)?
        ),
        _ => Err(SkPayloadError::InvalidKeyLength),
    }
}

pub(crate) fn encrypt_gcm(
    key: &[u8],
    nonce: &[u8; 12],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, SkPayloadError> {
    let nonce = Nonce::<Aes128Gcm>::try_from(nonce.as_slice()).expect("fixed 12-byte GCM nonce");
    let payload = Payload {
        msg: plaintext,
        aad,
    };
    match key.len() {
        20 => Aes128Gcm::new_from_slice(&key[..16])
            .expect("validated AES-128 key length")
            .encrypt(&nonce, payload)
            .map_err(|_| SkPayloadError::InvalidPacket),
        36 => Aes256Gcm::new_from_slice(&key[..32])
            .expect("validated AES-256 key length")
            .encrypt(&nonce, payload)
            .map_err(|_| SkPayloadError::InvalidPacket),
        _ => Err(SkPayloadError::InvalidKeyLength),
    }
}

pub(crate) fn decrypt_gcm(
    key: &[u8],
    nonce: &[u8; 12],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, SkPayloadError> {
    let nonce = Nonce::<Aes128Gcm>::try_from(nonce.as_slice()).expect("fixed 12-byte GCM nonce");
    let payload = Payload {
        msg: ciphertext,
        aad,
    };
    match key.len() {
        20 => Aes128Gcm::new_from_slice(&key[..16])
            .expect("validated AES-128 key length")
            .decrypt(&nonce, payload)
            .map_err(|_| SkPayloadError::AuthenticationFailed),
        36 => Aes256Gcm::new_from_slice(&key[..32])
            .expect("validated AES-256 key length")
            .decrypt(&nonce, payload)
            .map_err(|_| SkPayloadError::AuthenticationFailed),
        _ => Err(SkPayloadError::InvalidKeyLength),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExchangeType;
    use sha2::{Digest, Sha256};

    fn header() -> IkeHeader {
        IkeHeader {
            initiator_spi: 0x0102_0304_0506_0708,
            responder_spi: 0x1112_1314_1516_1718,
            next_payload: 0,
            version: 0x20,
            exchange: ExchangeType::IkeAuth,
            flags: IkeHeader::INITIATOR_FLAG,
            message_id: 1,
        }
    }

    #[test]
    fn gcm_sk_round_trip_preserves_ike_auth_frames() {
        let frames = [
            PayloadFrame {
                kind: 48,
                critical: false,
                body: vec![2, 17, 0, 5, 1],
            },
            PayloadFrame {
                kind: 41,
                critical: false,
                body: vec![0, 0, 0, 14],
            },
        ];
        let key = [0x55; 20];
        let packet = seal_ike_sk_gcm_with_iv(header(), GCM_16, &key, &frames, [0x22; 8]).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&packet)),
            "5a12914fe8e827f85fee298e78bc0b832589c0e247367544502ec2e8e519de39"
        );
        let (decoded_header, decoded_frames) = open_ike_sk_gcm(&packet, GCM_16, &key).unwrap();
        assert_eq!(decoded_header.exchange, ExchangeType::IkeAuth);
        assert_eq!(decoded_header.message_id, 1);
        assert_eq!(decoded_frames, frames);
    }

    #[test]
    fn cbc_sha2_sk_round_trip_covers_128_and_256_bit_aes() {
        let frames = [
            PayloadFrame {
                kind: 48,
                critical: false,
                body: vec![2, 17, 0, 5, 1],
            },
            PayloadFrame {
                kind: 41,
                critical: false,
                body: vec![0, 0, 0, 14],
            },
        ];
        for (encryption_key, integrity_transform, integrity_key) in [
            (vec![0x55; 16], HMAC_SHA2_256_128, vec![0x33; 32]),
            (vec![0x66; 32], HMAC_SHA2_512_256, vec![0x44; 64]),
        ] {
            let packet = seal_ike_sk_cbc_with_iv(
                header(),
                AES_CBC,
                integrity_transform,
                &encryption_key,
                &integrity_key,
                &frames,
                [0x22; CBC_IV_LENGTH],
            )
            .unwrap();
            let (decoded_header, decoded_frames) = open_ike_sk_cbc(
                &packet,
                AES_CBC,
                integrity_transform,
                &encryption_key,
                &integrity_key,
            )
            .unwrap();
            assert_eq!(decoded_header.exchange, ExchangeType::IkeAuth);
            assert_eq!(decoded_header.message_id, 1);
            assert_eq!(decoded_frames, frames);
        }
    }

    #[test]
    fn cbc_sha1_96_sk_round_trip_uses_explicit_20_byte_key_and_12_byte_icv() {
        let frames = [PayloadFrame {
            kind: 41,
            critical: false,
            body: vec![0, 0, 0, 14],
        }];
        let packet = seal_ike_sk_cbc_with_iv(
            header(),
            AES_CBC,
            HMAC_SHA1_96,
            &[0x55; 16],
            &[0x33; 20],
            &frames,
            [0x22; CBC_IV_LENGTH],
        )
        .unwrap();
        assert_eq!(
            packet.len() - u16::from_be_bytes([packet[26], packet[27]]) as usize,
            0
        );
        assert_eq!(
            open_ike_sk_cbc(&packet, AES_CBC, HMAC_SHA1_96, &[0x55; 16], &[0x33; 20])
                .unwrap()
                .1,
            frames
        );
        let mut altered = packet;
        *altered.last_mut().unwrap() ^= 1;
        assert_eq!(
            open_ike_sk_cbc(&altered, AES_CBC, HMAC_SHA1_96, &[0x55; 16], &[0x33; 20]),
            Err(SkPayloadError::AuthenticationFailed)
        );
    }

    #[test]
    fn cbc_sha2_sk_has_stable_go_compatible_wire_digests() {
        let frames = [
            PayloadFrame {
                kind: 48,
                critical: false,
                body: vec![2, 17, 0, 5, 1],
            },
            PayloadFrame {
                kind: 41,
                critical: false,
                body: vec![0, 0, 0, 14],
            },
        ];
        let packet = seal_ike_sk_cbc_with_iv(
            header(),
            AES_CBC,
            HMAC_SHA2_256_128,
            &[0x55; 16],
            &[0x33; 32],
            &frames,
            [0x22; CBC_IV_LENGTH],
        )
        .unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&packet)),
            "ac9d593d2fcba9efce0ae6462ccda9943c8dd94c6c0804d795b1892020f043d4"
        );
        let packet = seal_ike_sk_cbc_with_iv(
            header(),
            AES_CBC,
            HMAC_SHA2_512_256,
            &[0x66; 32],
            &[0x44; 64],
            &frames,
            [0x22; CBC_IV_LENGTH],
        )
        .unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&packet)),
            "e0bfa7b7b01cc80fd1fa5db8c0b306cb18ce8aa291aefabdd9b857097839ef18"
        );
    }

    #[test]
    fn cbc_sk_authenticates_before_decrypt_and_rejects_bad_lengths() {
        let frames = [PayloadFrame {
            kind: 48,
            critical: false,
            body: vec![1, 2, 3],
        }];
        let encryption_key = [0x55; 16];
        let integrity_key = [0x33; 32];
        let packet = seal_ike_sk_cbc_with_iv(
            header(),
            AES_CBC,
            HMAC_SHA2_256_128,
            &encryption_key,
            &integrity_key,
            &frames,
            [0x22; CBC_IV_LENGTH],
        )
        .unwrap();

        let mut tampered_iv = packet.clone();
        tampered_iv[32] ^= 1;
        assert_eq!(
            open_ike_sk_cbc(
                &tampered_iv,
                AES_CBC,
                HMAC_SHA2_256_128,
                &encryption_key,
                &integrity_key,
            ),
            Err(SkPayloadError::AuthenticationFailed)
        );
        let mut tampered_icv = packet.clone();
        *tampered_icv.last_mut().unwrap() ^= 1;
        assert_eq!(
            open_ike_sk_cbc(
                &tampered_icv,
                AES_CBC,
                HMAC_SHA2_256_128,
                &encryption_key,
                &integrity_key,
            ),
            Err(SkPayloadError::AuthenticationFailed)
        );
        assert_eq!(
            open_ike_sk_cbc(
                &packet,
                AES_CBC,
                HMAC_SHA2_256_128,
                &encryption_key,
                &[0x44; 32],
            ),
            Err(SkPayloadError::AuthenticationFailed)
        );
        assert!(
            open_ike_sk_cbc(
                &packet[..packet.len() - 1],
                AES_CBC,
                HMAC_SHA2_256_128,
                &encryption_key,
                &integrity_key,
            )
            .is_err()
        );
        assert_eq!(
            seal_ike_sk_cbc_with_iv(
                header(),
                20,
                HMAC_SHA2_256_128,
                &encryption_key,
                &integrity_key,
                &frames,
                [0x22; CBC_IV_LENGTH],
            ),
            Err(SkPayloadError::UnsupportedEncryption(20))
        );
        assert_eq!(
            seal_ike_sk_cbc_with_iv(
                header(),
                AES_CBC,
                13,
                &encryption_key,
                &integrity_key,
                &frames,
                [0x22; CBC_IV_LENGTH],
            ),
            Err(SkPayloadError::UnsupportedIntegrity(13))
        );
        assert_eq!(
            seal_ike_sk_cbc_with_iv(
                header(),
                AES_CBC,
                HMAC_SHA2_256_128,
                &[0x55; 24],
                &integrity_key,
                &frames,
                [0x22; CBC_IV_LENGTH],
            ),
            Err(SkPayloadError::InvalidKeyLength)
        );
        assert_eq!(
            seal_ike_sk_cbc_with_iv(
                header(),
                AES_CBC,
                HMAC_SHA2_256_128,
                &encryption_key,
                &[0x33; 31],
                &frames,
                [0x22; CBC_IV_LENGTH],
            ),
            Err(SkPayloadError::InvalidKeyLength)
        );
    }

    fn authenticated_cbc_packet(
        plaintext: &[u8],
        inner_first_payload: u8,
        encryption_key: &[u8],
        integrity_key: &[u8],
    ) -> Vec<u8> {
        let iv = [0x22; CBC_IV_LENGTH];
        let ciphertext = encrypt_cbc(encryption_key, &iv, plaintext).unwrap();
        let sk_length = 4 + CBC_IV_LENGTH + ciphertext.len() + 16;
        let mut packet = header().encode(&[]).unwrap();
        packet[16] = SK_PAYLOAD;
        packet[24..28].copy_from_slice(&((28 + sk_length) as u32).to_be_bytes());
        packet.extend_from_slice(&[inner_first_payload, 0]);
        packet.extend_from_slice(&(sk_length as u16).to_be_bytes());
        packet.extend_from_slice(&iv);
        packet.extend_from_slice(&ciphertext);
        let icv = compute_integrity(HMAC_SHA2_256_128, integrity_key, &packet).unwrap();
        packet.extend_from_slice(&icv[..16]);
        packet
    }

    #[test]
    fn cbc_sk_accepts_opaque_padding_and_rejects_authenticated_bad_padding() {
        let encryption_key = [0x55; 16];
        let integrity_key = [0x33; 32];
        let mut valid_plaintext = vec![0xa5; 15];
        valid_plaintext.push(15);
        let valid = authenticated_cbc_packet(&valid_plaintext, 0, &encryption_key, &integrity_key);
        assert!(
            open_ike_sk_cbc(
                &valid,
                AES_CBC,
                HMAC_SHA2_256_128,
                &encryption_key,
                &integrity_key,
            )
            .unwrap()
            .1
            .is_empty()
        );

        let mut invalid_plaintext = vec![0; 16];
        *invalid_plaintext.last_mut().unwrap() = 16;
        let invalid =
            authenticated_cbc_packet(&invalid_plaintext, 0, &encryption_key, &integrity_key);
        assert_eq!(
            open_ike_sk_cbc(
                &invalid,
                AES_CBC,
                HMAC_SHA2_256_128,
                &encryption_key,
                &integrity_key,
            ),
            Err(SkPayloadError::InvalidPacket)
        );
    }

    #[test]
    fn gcm_sk_rejects_tampering_wrong_key_and_unsupported_suite() {
        let key = [0x55; 20];
        let frames = [PayloadFrame {
            kind: 48,
            critical: false,
            body: vec![1, 2, 3],
        }];
        let packet = seal_ike_sk_gcm_with_iv(header(), GCM_16, &key, &frames, [0x22; 8]).unwrap();
        let mut tampered = packet.clone();
        tampered[28] ^= 1;
        assert_eq!(
            open_ike_sk_gcm(&tampered, GCM_16, &key),
            Err(SkPayloadError::AuthenticationFailed)
        );
        let mut tampered = packet.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(
            open_ike_sk_gcm(&tampered, GCM_16, &key),
            Err(SkPayloadError::AuthenticationFailed)
        );
        assert_eq!(
            open_ike_sk_gcm(&packet, GCM_16, &[0x44; 20]),
            Err(SkPayloadError::AuthenticationFailed)
        );
        assert_eq!(
            seal_ike_sk_gcm_with_iv(header(), 19, &key, &frames, [0x22; 8]),
            Err(SkPayloadError::UnsupportedEncryption(19))
        );
        assert_eq!(
            seal_ike_sk_gcm_with_iv(header(), GCM_16, &[0x55; 28], &frames, [0x22; 8]),
            Err(SkPayloadError::InvalidKeyLength)
        );
    }

    #[test]
    fn accepts_rfc5282_arbitrary_padding_and_rejects_missing_length() {
        for pad in [0_usize, 255, 256] {
            let plaintext = if pad == 256 {
                vec![]
            } else {
                let mut p = vec![0x99; pad];
                p.push(pad as u8);
                p
            };
            let sk_len = 4 + 8 + plaintext.len() + 16;
            let mut aad = header().encode(&[]).unwrap();
            aad[16] = 46;
            aad[24..28].copy_from_slice(&((28 + sk_len) as u32).to_be_bytes());
            aad.extend_from_slice(&[0, 0, (sk_len >> 8) as u8, sk_len as u8]);
            let mut nonce = [0x22; 12];
            nonce[..4].fill(0x55);
            let encrypted = encrypt_gcm(&[0x55; 20], &nonce, &aad, &plaintext).unwrap();
            let mut packet = aad;
            packet.extend_from_slice(&[0x22; 8]);
            packet.extend(encrypted);
            let result = open_ike_sk_gcm(&packet, 20, &[0x55; 20]);
            if pad == 256 {
                assert_eq!(result, Err(SkPayloadError::InvalidPacket));
            } else {
                assert!(result.unwrap().1.is_empty());
            }
        }
    }
}
