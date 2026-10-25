//! RFC 4106 AES-GCM-16 and RFC 3602 AES-CBC ESP, 32-bit sequence numbers,
//! tunnel mode only.
//! A fresh instance requires fresh IKE-derived keys; never restore a key with
//! its counter reset. Directional objects are intentionally not Clone.
use crate::sk::{SkPayloadError, decrypt_gcm, encrypt_gcm};
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit, block_padding::NoPadding};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use zeroize::Zeroizing;

const ESP_HEADER_LENGTH: usize = 8;
const GCM_IV_LENGTH: usize = 8;
const GCM_TAG_LENGTH: usize = 16;
const CBC_IV_LENGTH: usize = 16;
const CBC_BLOCK_LENGTH: usize = 16;
const MAX_ESP_PACKET_LENGTH: usize = 65_507;
const AUTH_HMAC_SHA2_256_128: u16 = 12;
const AUTH_HMAC_SHA2_512_256: u16 = 14;
const AUTH_HMAC_SHA1_96: u16 = 2;

#[derive(Debug, Eq, PartialEq)]
pub enum EspError {
    InvalidKey,
    InvalidSpi,
    InvalidPacket,
    UnsupportedIntegrity(u16),
    AuthenticationFailed,
    EntropyUnavailable,
    Replay,
    SequenceExhausted,
    Crypto(SkPayloadError),
}
impl std::fmt::Display for EspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKey => f.write_str("invalid ESP encryption or integrity key length"),
            Self::InvalidSpi => f.write_str("ESP SPI mismatch or zero SPI"),
            Self::InvalidPacket => f.write_str("malformed ESP tunnel packet"),
            Self::UnsupportedIntegrity(transform) => {
                write!(f, "unsupported ESP integrity transform {transform}")
            }
            Self::AuthenticationFailed => f.write_str("ESP authentication failed"),
            Self::EntropyUnavailable => f.write_str("system cryptographic entropy unavailable"),
            Self::Replay => f.write_str("ESP replay or packet outside receive window"),
            Self::SequenceExhausted => f.write_str("ESP sequence exhausted; rekey required"),
            Self::Crypto(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for EspError {}

enum EspSuite {
    Gcm {
        key: Zeroizing<Vec<u8>>,
    },
    Cbc {
        encryption_key: Zeroizing<Vec<u8>>,
        integrity_transform: u16,
        integrity_key: Zeroizing<Vec<u8>>,
    },
}

pub struct EspSender {
    spi: u32,
    suite: EspSuite,
    sequence: u32,
}
pub struct EspReceiver {
    spi: u32,
    suite: EspSuite,
    highest: u32,
    window: u64,
}

fn validate_spi(spi: u32) -> Result<(), EspError> {
    if spi == 0 {
        return Err(EspError::InvalidSpi);
    }
    Ok(())
}

fn validate_gcm_key(key: &[u8]) -> Result<(), EspError> {
    if !matches!(key.len(), 20 | 36) {
        return Err(EspError::InvalidKey);
    }
    Ok(())
}

fn validate_cbc_keys(
    encryption_key: &[u8],
    integrity_transform: u16,
    integrity_key: &[u8],
) -> Result<(), EspError> {
    if !matches!(encryption_key.len(), 16 | 32) {
        return Err(EspError::InvalidKey);
    }
    let expected_integrity_key_length = match integrity_transform {
        AUTH_HMAC_SHA1_96 => 20,
        AUTH_HMAC_SHA2_256_128 => 32,
        AUTH_HMAC_SHA2_512_256 => 64,
        other => return Err(EspError::UnsupportedIntegrity(other)),
    };
    if integrity_key.len() != expected_integrity_key_length {
        return Err(EspError::InvalidKey);
    }
    Ok(())
}

fn cbc_icv_length(integrity_transform: u16) -> Result<usize, EspError> {
    match integrity_transform {
        AUTH_HMAC_SHA1_96 => Ok(12),
        AUTH_HMAC_SHA2_256_128 => Ok(16),
        AUTH_HMAC_SHA2_512_256 => Ok(32),
        other => Err(EspError::UnsupportedIntegrity(other)),
    }
}

fn append_cbc_icv(
    packet: &mut Vec<u8>,
    integrity_transform: u16,
    integrity_key: &[u8],
) -> Result<(), EspError> {
    match integrity_transform {
        AUTH_HMAC_SHA1_96 => {
            let mut mac =
                Hmac::<Sha1>::new_from_slice(integrity_key).map_err(|_| EspError::InvalidKey)?;
            mac.update(packet);
            packet.extend_from_slice(&mac.finalize().into_bytes()[..12]);
        }
        AUTH_HMAC_SHA2_256_128 => {
            let mut mac =
                Hmac::<Sha256>::new_from_slice(integrity_key).map_err(|_| EspError::InvalidKey)?;
            mac.update(packet);
            packet.extend_from_slice(&mac.finalize().into_bytes()[..16]);
        }
        AUTH_HMAC_SHA2_512_256 => {
            let mut mac =
                Hmac::<Sha512>::new_from_slice(integrity_key).map_err(|_| EspError::InvalidKey)?;
            mac.update(packet);
            packet.extend_from_slice(&mac.finalize().into_bytes()[..32]);
        }
        other => return Err(EspError::UnsupportedIntegrity(other)),
    }
    Ok(())
}

fn verify_cbc_icv(
    authenticated: &[u8],
    received_icv: &[u8],
    integrity_transform: u16,
    integrity_key: &[u8],
) -> Result<(), EspError> {
    let result = match integrity_transform {
        AUTH_HMAC_SHA1_96 => {
            let mut mac =
                Hmac::<Sha1>::new_from_slice(integrity_key).map_err(|_| EspError::InvalidKey)?;
            mac.update(authenticated);
            mac.verify_truncated_left(received_icv)
        }
        AUTH_HMAC_SHA2_256_128 => {
            let mut mac =
                Hmac::<Sha256>::new_from_slice(integrity_key).map_err(|_| EspError::InvalidKey)?;
            mac.update(authenticated);
            mac.verify_truncated_left(received_icv)
        }
        AUTH_HMAC_SHA2_512_256 => {
            let mut mac =
                Hmac::<Sha512>::new_from_slice(integrity_key).map_err(|_| EspError::InvalidKey)?;
            mac.update(authenticated);
            mac.verify_truncated_left(received_icv)
        }
        other => return Err(EspError::UnsupportedIntegrity(other)),
    };
    result.map_err(|_| EspError::AuthenticationFailed)
}

fn next_header(packet: &[u8]) -> Result<u8, EspError> {
    if packet.len() > 65468 {
        return Err(EspError::InvalidPacket);
    }
    match packet.first().map(|b| b >> 4) {
        Some(4)
            if packet.len() >= 20
                && usize::from(packet[0] & 15) * 4 >= 20
                && usize::from(packet[0] & 15) * 4 <= packet.len()
                && usize::from(u16::from_be_bytes([packet[2], packet[3]])) == packet.len() =>
        {
            Ok(4)
        }
        Some(6)
            if packet.len() >= 40
                && usize::from(u16::from_be_bytes([packet[4], packet[5]])) + 40 == packet.len() =>
        {
            Ok(41)
        }
        _ => Err(EspError::InvalidPacket),
    }
}

impl EspSender {
    pub fn new(spi: u32, key: Zeroizing<Vec<u8>>) -> Result<Self, EspError> {
        validate_spi(spi)?;
        validate_gcm_key(&key)?;
        Ok(Self {
            spi,
            suite: EspSuite::Gcm { key },
            sequence: 0,
        })
    }

    /// Constructs a non-AEAD ESP sender for AES-CBC-128/256 with one of the
    /// SHA-2 integrity transforms implemented by the native data plane.
    pub fn new_cbc(
        spi: u32,
        encryption_key: Zeroizing<Vec<u8>>,
        integrity_transform: u16,
        integrity_key: Zeroizing<Vec<u8>>,
    ) -> Result<Self, EspError> {
        validate_spi(spi)?;
        validate_cbc_keys(&encryption_key, integrity_transform, &integrity_key)?;
        Ok(Self {
            spi,
            suite: EspSuite::Cbc {
                encryption_key,
                integrity_transform,
                integrity_key,
            },
            sequence: 0,
        })
    }

    pub fn seal(&mut self, packet: &[u8]) -> Result<Vec<u8>, EspError> {
        let next = next_header(packet)?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(EspError::SequenceExhausted)?;
        // Reserve before encryption: even a failure must not reuse this nonce.
        self.sequence = sequence;
        match &self.suite {
            EspSuite::Gcm { key } => Self::seal_gcm(self.spi, sequence, key, packet, next),
            suite @ EspSuite::Cbc { .. } => {
                let mut iv = [0_u8; CBC_IV_LENGTH];
                getrandom::fill(&mut iv).map_err(|_| EspError::EntropyUnavailable)?;
                Self::seal_cbc(self.spi, sequence, suite, packet, next, iv)
            }
        }
    }

    fn seal_gcm(
        spi: u32,
        sequence: u32,
        key: &[u8],
        packet: &[u8],
        next: u8,
    ) -> Result<Vec<u8>, EspError> {
        let mut header = Vec::with_capacity(16);
        header.extend_from_slice(&spi.to_be_bytes());
        header.extend_from_slice(&sequence.to_be_bytes());
        let iv = u64::from(sequence).to_be_bytes();
        let mut nonce = Zeroizing::new([0; 12]);
        nonce[..4].copy_from_slice(&key[key.len() - 4..]);
        nonce[4..].copy_from_slice(&iv);
        let padding = (4 - (packet.len() + 2) % 4) % 4;
        let mut plain = Zeroizing::new(Vec::with_capacity(packet.len() + padding + 2));
        plain.extend_from_slice(packet);
        for i in 1..=padding {
            plain.push(i as u8);
        }
        plain.extend_from_slice(&[padding as u8, next]);
        let encrypted = encrypt_gcm(key, &nonce, &header, &plain).map_err(EspError::Crypto)?;
        header.extend_from_slice(&iv);
        header.extend_from_slice(&encrypted);
        if header.len() > MAX_ESP_PACKET_LENGTH {
            return Err(EspError::InvalidPacket);
        }
        Ok(header)
    }

    fn seal_cbc(
        spi: u32,
        sequence: u32,
        suite: &EspSuite,
        packet: &[u8],
        next: u8,
        iv: [u8; CBC_IV_LENGTH],
    ) -> Result<Vec<u8>, EspError> {
        let EspSuite::Cbc {
            encryption_key,
            integrity_transform,
            integrity_key,
        } = suite
        else {
            return Err(EspError::InvalidKey);
        };
        let padding = (CBC_BLOCK_LENGTH - (packet.len() + 2) % CBC_BLOCK_LENGTH) % CBC_BLOCK_LENGTH;
        let encrypted_length = packet
            .len()
            .checked_add(padding + 2)
            .ok_or(EspError::InvalidPacket)?;
        let icv_length = cbc_icv_length(*integrity_transform)?;
        let capacity = ESP_HEADER_LENGTH
            .checked_add(CBC_IV_LENGTH)
            .and_then(|length| length.checked_add(encrypted_length))
            .and_then(|length| length.checked_add(icv_length))
            .filter(|length| *length <= MAX_ESP_PACKET_LENGTH)
            .ok_or(EspError::InvalidPacket)?;
        let mut plain = Zeroizing::new(Vec::with_capacity(encrypted_length));
        plain.extend_from_slice(packet);
        for byte in 1..=padding {
            plain.push(byte as u8);
        }
        plain.extend_from_slice(&[padding as u8, next]);
        match encryption_key.len() {
            16 => {
                cbc::Encryptor::<aes::Aes128>::new_from_slices(encryption_key, &iv)
                    .map_err(|_| EspError::InvalidKey)?
                    .encrypt_padded::<NoPadding>(&mut plain, encrypted_length)
                    .map_err(|_| EspError::InvalidPacket)?;
            }
            32 => {
                cbc::Encryptor::<aes::Aes256>::new_from_slices(encryption_key, &iv)
                    .map_err(|_| EspError::InvalidKey)?
                    .encrypt_padded::<NoPadding>(&mut plain, encrypted_length)
                    .map_err(|_| EspError::InvalidPacket)?;
            }
            _ => return Err(EspError::InvalidKey),
        }
        let mut output = Vec::with_capacity(capacity);
        output.extend_from_slice(&spi.to_be_bytes());
        output.extend_from_slice(&sequence.to_be_bytes());
        output.extend_from_slice(&iv);
        output.extend_from_slice(&plain);
        append_cbc_icv(&mut output, *integrity_transform, integrity_key)?;
        debug_assert_eq!(output.len(), capacity);
        Ok(output)
    }

    #[cfg(test)]
    fn seal_cbc_with_iv(
        &mut self,
        packet: &[u8],
        iv: [u8; CBC_IV_LENGTH],
    ) -> Result<Vec<u8>, EspError> {
        let next = next_header(packet)?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(EspError::SequenceExhausted)?;
        self.sequence = sequence;
        match &self.suite {
            suite @ EspSuite::Cbc { .. } => {
                Self::seal_cbc(self.spi, sequence, suite, packet, next, iv)
            }
            EspSuite::Gcm { .. } => Err(EspError::InvalidKey),
        }
    }
}

impl EspReceiver {
    pub fn new(spi: u32, key: Zeroizing<Vec<u8>>) -> Result<Self, EspError> {
        validate_spi(spi)?;
        validate_gcm_key(&key)?;
        Ok(Self {
            spi,
            suite: EspSuite::Gcm { key },
            highest: 0,
            window: 0,
        })
    }

    /// Constructs a non-AEAD ESP receiver matching [`EspSender::new_cbc`].
    pub fn new_cbc(
        spi: u32,
        encryption_key: Zeroizing<Vec<u8>>,
        integrity_transform: u16,
        integrity_key: Zeroizing<Vec<u8>>,
    ) -> Result<Self, EspError> {
        validate_spi(spi)?;
        validate_cbc_keys(&encryption_key, integrity_transform, &integrity_key)?;
        Ok(Self {
            spi,
            suite: EspSuite::Cbc {
                encryption_key,
                integrity_transform,
                integrity_key,
            },
            highest: 0,
            window: 0,
        })
    }

    pub fn open(&mut self, packet: &[u8]) -> Result<Zeroizing<Vec<u8>>, EspError> {
        if !(ESP_HEADER_LENGTH..=MAX_ESP_PACKET_LENGTH).contains(&packet.len()) {
            return Err(EspError::InvalidPacket);
        }
        if u32::from_be_bytes(packet[..4].try_into().unwrap()) != self.spi {
            return Err(EspError::InvalidSpi);
        }
        let sequence = u32::from_be_bytes(packet[4..8].try_into().unwrap());
        if sequence == 0 {
            return Err(EspError::Replay);
        }
        if sequence <= self.highest {
            let delta = self.highest - sequence;
            if delta >= 64 || self.window & (1_u64 << delta) != 0 {
                return Err(EspError::Replay);
            }
        }
        let (mut plain, alignment) = match &self.suite {
            EspSuite::Gcm { key } => (Self::open_gcm(packet, key)?, 4),
            EspSuite::Cbc {
                encryption_key,
                integrity_transform,
                integrity_key,
            } => (
                Self::open_cbc(packet, encryption_key, *integrity_transform, integrity_key)?,
                CBC_BLOCK_LENGTH,
            ),
        };
        Self::validate_plaintext(&mut plain, alignment)?;
        // Commit only after authentication and structural validation.
        if sequence > self.highest {
            let delta = sequence - self.highest;
            self.window = if delta >= 64 {
                1
            } else {
                (self.window << delta) | 1
            };
            self.highest = sequence;
        } else {
            self.window |= 1_u64 << (self.highest - sequence);
        }
        Ok(plain)
    }

    fn open_gcm(packet: &[u8], key: &[u8]) -> Result<Zeroizing<Vec<u8>>, EspError> {
        if packet.len() < ESP_HEADER_LENGTH + GCM_IV_LENGTH + GCM_TAG_LENGTH {
            return Err(EspError::InvalidPacket);
        }
        let mut nonce = Zeroizing::new([0; 12]);
        nonce[..4].copy_from_slice(&key[key.len() - 4..]);
        nonce[4..].copy_from_slice(&packet[8..16]);
        Ok(Zeroizing::new(
            decrypt_gcm(key, &nonce, &packet[..8], &packet[16..]).map_err(EspError::Crypto)?,
        ))
    }

    fn open_cbc(
        packet: &[u8],
        encryption_key: &[u8],
        integrity_transform: u16,
        integrity_key: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, EspError> {
        let icv_length = cbc_icv_length(integrity_transform)?;
        let authenticated_length = packet
            .len()
            .checked_sub(icv_length)
            .filter(|length| *length >= ESP_HEADER_LENGTH + CBC_IV_LENGTH + CBC_BLOCK_LENGTH)
            .ok_or(EspError::InvalidPacket)?;
        let (authenticated, received_icv) = packet.split_at(authenticated_length);
        // Authenticate the complete ESP header, explicit IV, and ciphertext
        // before any CBC decryption or replay-window state change.
        verify_cbc_icv(
            authenticated,
            received_icv,
            integrity_transform,
            integrity_key,
        )?;
        let ciphertext = &packet[ESP_HEADER_LENGTH + CBC_IV_LENGTH..authenticated_length];
        if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(CBC_BLOCK_LENGTH) {
            return Err(EspError::InvalidPacket);
        }
        let mut plain = Zeroizing::new(ciphertext.to_vec());
        match encryption_key.len() {
            16 => {
                cbc::Decryptor::<aes::Aes128>::new_from_slices(
                    encryption_key,
                    &packet[ESP_HEADER_LENGTH..ESP_HEADER_LENGTH + CBC_IV_LENGTH],
                )
                .map_err(|_| EspError::InvalidKey)?
                .decrypt_padded::<NoPadding>(&mut plain)
                .map_err(|_| EspError::InvalidPacket)?;
            }
            32 => {
                cbc::Decryptor::<aes::Aes256>::new_from_slices(
                    encryption_key,
                    &packet[ESP_HEADER_LENGTH..ESP_HEADER_LENGTH + CBC_IV_LENGTH],
                )
                .map_err(|_| EspError::InvalidKey)?
                .decrypt_padded::<NoPadding>(&mut plain)
                .map_err(|_| EspError::InvalidPacket)?;
            }
            _ => return Err(EspError::InvalidKey),
        }
        Ok(plain)
    }

    fn validate_plaintext(
        plain: &mut Zeroizing<Vec<u8>>,
        alignment: usize,
    ) -> Result<(), EspError> {
        if plain.len() < 2 || !plain.len().is_multiple_of(alignment) {
            return Err(EspError::InvalidPacket);
        }
        let pad = usize::from(plain[plain.len() - 2]);
        let next = plain[plain.len() - 1];
        let end = plain
            .len()
            .checked_sub(pad + 2)
            .ok_or(EspError::InvalidPacket)?;
        if plain[end..end + pad]
            .iter()
            .enumerate()
            .any(|(i, b)| usize::from(*b) != i + 1)
        {
            return Err(EspError::InvalidPacket);
        }
        plain.truncate(end);
        if next_header(plain)? != next {
            return Err(EspError::InvalidPacket);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_go_esp_wire_vector_and_accepts_ipv6() {
        let mut tx = EspSender::new(42, Zeroizing::new(vec![0x11; 20])).unwrap();
        let packet = tx.seal(&ip()).unwrap();
        let hex: String = packet.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "0000002a0000000100000000000000018f69e264314ff72800a725369ba61872c13efce98c258e052e64125bcb9730f7e0050daf285b4ee5"
        );
        let mut rx = EspReceiver::new(42, Zeroizing::new(vec![0x11; 20])).unwrap();
        assert_eq!(&*rx.open(&packet).unwrap(), &ip());
        let mut v6 = vec![0; 40];
        v6[0] = 0x60;
        assert_eq!(&*rx.open(&tx.seal(&v6).unwrap()).unwrap(), &v6);
        assert!(tx.seal(&[]).is_err());
    }

    #[test]
    fn gcm_esp_matches_independent_openssl_wire_vectors() {
        // Fixed vectors were generated with Node's OpenSSL AES-GCM API from
        // RFC 4106's key|salt, SPI|sequence AAD, explicit IV and ESP trailer.
        // Distinct key/salt bytes catch a misplaced salt that the older
        // repeated-byte Go compatibility vector cannot distinguish.
        let vectors = [
            (
                16,
                [0xa1, 0xb2, 0xc3, 0xd4],
                0x1234_5678,
                "4500001400010000400600000a0000010a000002",
                "123456780000000100000000000000018f1ef979daf39264e3889f693955f029ac14aa86b71360cb9b9d13888935bb19cb86c945338412b3",
            ),
            (
                32,
                [0xd0, 0xe1, 0xf2, 0xa3],
                0x90ab_cdef,
                "600000000000064020010db800000000000000000000000120010db8000000000000000000000002",
                "90abcdef0000000100000000000000012714e62a1e90e1364f8a70998ae5173e8625624856c08e30e6dfbdff33fc601ad9aa9fcbde94c9f1095faa3aa1fa731c59efcb30810177710a028c06",
            ),
        ];
        for (key_len, salt, spi, ip_hex, expected_hex) in vectors {
            let mut key: Vec<u8> = (0..key_len).map(|byte| byte as u8).collect();
            key.extend_from_slice(&salt);
            let ip: Vec<u8> = ip_hex
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            let mut sender = EspSender::new(spi, Zeroizing::new(key.clone())).unwrap();
            let packet = sender.seal(&ip).unwrap();
            let actual_hex: String = packet.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(actual_hex, expected_hex);

            let independent_packet: Vec<u8> = expected_hex
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            let mut receiver = EspReceiver::new(spi, Zeroizing::new(key)).unwrap();
            assert_eq!(&*receiver.open(&independent_packet).unwrap(), &ip);
        }
    }
    fn ip() -> Vec<u8> {
        ip_with_length(20)
    }

    fn ip_with_length(length: usize) -> Vec<u8> {
        assert!((20..=u16::MAX as usize).contains(&length));
        let mut p = vec![0; length];
        p[0] = 0x45;
        p[2..4].copy_from_slice(&(length as u16).to_be_bytes());
        p
    }

    fn cbc_sender(
        encryption_key_length: usize,
        integrity_transform: u16,
        integrity_key_length: usize,
    ) -> EspSender {
        EspSender::new_cbc(
            42,
            Zeroizing::new(vec![0x11; encryption_key_length]),
            integrity_transform,
            Zeroizing::new(vec![0x22; integrity_key_length]),
        )
        .unwrap()
    }

    fn cbc_receiver(
        encryption_key_length: usize,
        integrity_transform: u16,
        integrity_key_length: usize,
    ) -> EspReceiver {
        EspReceiver::new_cbc(
            42,
            Zeroizing::new(vec![0x11; encryption_key_length]),
            integrity_transform,
            Zeroizing::new(vec![0x22; integrity_key_length]),
        )
        .unwrap()
    }

    #[test]
    fn cbc_sha2_matches_independent_wire_vectors_and_accepts_ipv6() {
        let suites = [
            (
                16,
                AUTH_HMAC_SHA2_256_128,
                32,
                "0000002a000000014444444444444444444444444444444420f15531391f5588cc0ecc85514d4b2d9c6931b2016713502c29996e7430c20afc61ea2ac44e28cae6438e4a39828760",
            ),
            (
                32,
                AUTH_HMAC_SHA2_512_256,
                64,
                "0000002a000000014444444444444444444444444444444441aa825a81b5de961f5e7ad978a1c694e331dbf91b1ec46fdb1a5335b8164040e0b6e594d435d0f3347f495bdb99b0ec4ef8de3c381d8a36612688a11364c427",
            ),
        ];
        for (encryption_length, integrity, integrity_length, expected) in suites {
            let mut sender = cbc_sender(encryption_length, integrity, integrity_length);
            let packet = sender.seal_cbc_with_iv(&ip(), [0x44; 16]).unwrap();
            let hex: String = packet.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(hex, expected);
            let mut receiver = cbc_receiver(encryption_length, integrity, integrity_length);
            assert_eq!(&*receiver.open(&packet).unwrap(), &ip());

            let mut ipv6 = vec![0; 40];
            ipv6[0] = 0x60;
            let packet = sender.seal_cbc_with_iv(&ipv6, [0x55; 16]).unwrap();
            assert_eq!(&*receiver.open(&packet).unwrap(), &ipv6);
        }
    }

    #[test]
    fn aes256_cbc_sha256_matches_independent_openssl_wire_vector() {
        // This is the AES-256-CBC/HMAC-SHA256-128 ESP combination requested by
        // the 3HK template. Node/OpenSSL generated the fixed wire bytes using
        // distinct encryption, integrity and IV bytes.
        let encryption_key: Vec<u8> = (0..32).collect();
        let integrity_key: Vec<u8> = (32..64).collect();
        let iv: [u8; 16] = std::array::from_fn(|index| index as u8 + 64);
        let ip = [
            0x45, 0, 0, 20, 0, 1, 0, 0, 64, 6, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2,
        ];
        let expected = "1234567800000001404142434445464748494a4b4c4d4e4fef62edf58751d888c31b3df7d1b4ee5f2b9443003c81a8153ca5d10aef56d38fbf6504e8f85a82bdf3ce5708aebd1931";
        let mut sender = EspSender::new_cbc(
            0x1234_5678,
            Zeroizing::new(encryption_key.clone()),
            AUTH_HMAC_SHA2_256_128,
            Zeroizing::new(integrity_key.clone()),
        )
        .unwrap();
        let actual = sender.seal_cbc_with_iv(&ip, iv).unwrap();
        assert_eq!(
            actual
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            expected
        );

        let independent_packet: Vec<u8> = expected
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let mut receiver = EspReceiver::new_cbc(
            0x1234_5678,
            Zeroizing::new(encryption_key),
            AUTH_HMAC_SHA2_256_128,
            Zeroizing::new(integrity_key),
        )
        .unwrap();
        assert_eq!(&*receiver.open(&independent_packet).unwrap(), &ip);
    }

    #[test]
    fn cbc_sha1_96_esp_round_trip_authenticates_before_replay_commit() {
        let mut sender = cbc_sender(16, AUTH_HMAC_SHA1_96, 20);
        let packet = sender.seal_cbc_with_iv(&ip(), [0x44; 16]).unwrap();
        assert_eq!(packet.len(), 8 + 16 + 32 + 12);
        let mut receiver = cbc_receiver(16, AUTH_HMAC_SHA1_96, 20);
        let mut forged = packet.clone();
        *forged.last_mut().unwrap() ^= 1;
        assert_eq!(receiver.open(&forged), Err(EspError::AuthenticationFailed));
        assert_eq!(receiver.highest, 0);
        assert_eq!(&*receiver.open(&packet).unwrap(), &ip());
    }

    #[test]
    fn cbc_authenticates_before_commit_and_bounds_replay_and_exhaustion() {
        for (encryption_length, integrity, integrity_length) in [
            (16, AUTH_HMAC_SHA2_256_128, 32),
            (32, AUTH_HMAC_SHA2_512_256, 64),
        ] {
            let mut sender = cbc_sender(encryption_length, integrity, integrity_length);
            let first = sender.seal_cbc_with_iv(&ip(), [0x44; 16]).unwrap();
            let second = sender.seal_cbc_with_iv(&ip(), [0x55; 16]).unwrap();
            let mut receiver = cbc_receiver(encryption_length, integrity, integrity_length);

            for index in [7, 8, 24, second.len() - 1] {
                let mut forged = second.clone();
                forged[index] ^= 1;
                assert_eq!(receiver.open(&forged), Err(EspError::AuthenticationFailed));
                assert_eq!((receiver.highest, receiver.window), (0, 0));
            }
            assert_eq!(&*receiver.open(&second).unwrap(), &ip());
            assert_eq!(&*receiver.open(&first).unwrap(), &ip());
            assert_eq!(receiver.open(&first), Err(EspError::Replay));

            sender.sequence = 100;
            let far = sender.seal_cbc_with_iv(&ip(), [0x66; 16]).unwrap();
            assert_eq!(&*receiver.open(&far).unwrap(), &ip());
            assert_eq!(receiver.open(&second), Err(EspError::Replay));
            sender.sequence = u32::MAX;
            assert_eq!(sender.seal(&ip()), Err(EspError::SequenceExhausted));
        }
    }

    #[test]
    fn cbc_rejects_authenticated_bad_padding_without_consuming_sequence() {
        let mut sender = cbc_sender(16, AUTH_HMAC_SHA2_256_128, 32);
        let valid = sender.seal_cbc_with_iv(&ip(), [0x44; 16]).unwrap();
        let mut forged = valid[..valid.len() - 16].to_vec();
        // Change the first padding byte in the final plaintext block via CBC's
        // preceding ciphertext block, then authenticate the malformed packet.
        forged[28] ^= 1;
        append_cbc_icv(&mut forged, AUTH_HMAC_SHA2_256_128, &[0x22; 32]).unwrap();
        let mut receiver = cbc_receiver(16, AUTH_HMAC_SHA2_256_128, 32);
        assert_eq!(receiver.open(&forged), Err(EspError::InvalidPacket));
        assert_eq!((receiver.highest, receiver.window), (0, 0));
        assert_eq!(&*receiver.open(&valid).unwrap(), &ip());
    }

    #[test]
    fn cbc_key_and_wire_length_boundaries_are_explicit() {
        for (encryption_length, integrity, integrity_length, maximum_ip) in [
            (16, AUTH_HMAC_SHA2_256_128, 32, 65_454),
            (32, AUTH_HMAC_SHA2_512_256, 64, 65_438),
        ] {
            let mut sender = cbc_sender(encryption_length, integrity, integrity_length);
            let packet = sender
                .seal_cbc_with_iv(&ip_with_length(maximum_ip), [0x44; 16])
                .unwrap();
            assert!(packet.len() <= MAX_ESP_PACKET_LENGTH);
            let mut receiver = cbc_receiver(encryption_length, integrity, integrity_length);
            assert_eq!(
                receiver.open(&packet).unwrap().len(),
                maximum_ip,
                "maximum accepted inner packet must round trip"
            );
            assert_eq!(
                sender.seal_cbc_with_iv(&ip_with_length(maximum_ip + 1), [0x55; 16]),
                Err(EspError::InvalidPacket)
            );
        }

        for (encryption_length, integrity, integrity_length, expected) in [
            (24, AUTH_HMAC_SHA2_256_128, 32, EspError::InvalidKey),
            (16, AUTH_HMAC_SHA2_256_128, 31, EspError::InvalidKey),
            (16, AUTH_HMAC_SHA2_512_256, 32, EspError::InvalidKey),
            (16, 13, 48, EspError::UnsupportedIntegrity(13)),
        ] {
            assert!(matches!(
                EspSender::new_cbc(
                    42,
                    Zeroizing::new(vec![0; encryption_length]),
                    integrity,
                    Zeroizing::new(vec![0; integrity_length]),
                ),
                Err(error) if error == expected
            ));
        }
    }

    #[test]
    fn tampering_reordering_duplicates_and_wrap_are_bounded() {
        for size in [20, 36] {
            let key = vec![0x11; size];
            let mut tx = EspSender::new(42, Zeroizing::new(key.clone())).unwrap();
            let mut rx = EspReceiver::new(42, Zeroizing::new(key)).unwrap();
            let first = tx.seal(&ip()).unwrap();
            let second = tx.seal(&ip()).unwrap();
            let mut forged = second.clone();
            forged[7] = 200;
            assert!(rx.open(&forged).is_err());
            assert_eq!(rx.highest, 0);
            assert_eq!(&*rx.open(&second).unwrap(), &ip());
            assert!(rx.open(&first).is_ok());
            assert!(matches!(rx.open(&first), Err(EspError::Replay)));
            tx.sequence = 100;
            let far = tx.seal(&ip()).unwrap();
            assert!(rx.open(&far).is_ok());
            assert!(matches!(rx.open(&second), Err(EspError::Replay)));
            tx.sequence = u32::MAX;
            assert!(matches!(tx.seal(&ip()), Err(EspError::SequenceExhausted)));
        }
    }
}
