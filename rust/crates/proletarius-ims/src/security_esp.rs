//! Negotiated IMS transport ESP. Verify the complete ICV before decrypting.
//! NULL remains authenticated ESP; selected plaintext TCP is never admitted.
mod suite;
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit, block_padding::NoPadding};
use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
pub use suite::{Encryption, Integrity, Suite};
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq)]
pub enum EspError {
    InvalidPacket,
    InvalidSpi,
    InvalidKey,
    UnsupportedTransform,
    Authentication,
    Replay,
    Exhausted,
    Entropy,
}
const MAX_TCP_BYTES: usize = 65_000;
const ICV_BYTES: usize = 12;
pub const ESP_HEADER_IV_ICV_BYTES: usize = 36;

/// Compatibility helper for the existing AES-CBC packet budget.
pub fn sealed_len(tcp_len: usize) -> Option<usize> {
    sealed_len_for_suite(Suite::AES_SHA1, tcp_len)
}
pub fn sealed_len_for_suite(suite: Suite, tcp_len: usize) -> Option<usize> {
    let block = suite.block_len();
    let padded = tcp_len.checked_add(2)?.checked_add(block - 1)? / block * block;
    (8 + suite.iv_len() + ICV_BYTES).checked_add(padded)
}
pub struct Sender {
    spi: u32,
    sequence: u32,
    suite: Suite,
    encryption: Zeroizing<Vec<u8>>,
    integrity: Zeroizing<Vec<u8>>,
}
pub struct Receiver {
    spi: u32,
    highest: u32,
    window: u64,
    suite: Suite,
    encryption: Zeroizing<Vec<u8>>,
    integrity: Zeroizing<Vec<u8>>,
}
impl Sender {
    /// Fixed AES/SHA1 compatibility constructor; it does not negotiate.
    pub fn new(
        spi: u32,
        encryption: Zeroizing<[u8; 16]>,
        integrity: Zeroizing<[u8; 20]>,
    ) -> Result<Self, EspError> {
        Self::with_suite(
            spi,
            Suite::AES_SHA1,
            Zeroizing::new(encryption.to_vec()),
            Zeroizing::new(integrity.to_vec()),
        )
    }
    pub fn with_suite(
        spi: u32,
        suite: Suite,
        encryption: Zeroizing<Vec<u8>>,
        integrity: Zeroizing<Vec<u8>>,
    ) -> Result<Self, EspError> {
        if spi == 0 {
            return Err(EspError::InvalidSpi);
        }
        suite.validate_keys(&encryption, &integrity)?;
        Ok(Self {
            spi,
            sequence: 0,
            suite,
            encryption,
            integrity,
        })
    }
    pub fn seal(&mut self, tcp: &[u8]) -> Result<Vec<u8>, EspError> {
        let mut iv = [0; 16];
        if self.suite.iv_len() > 0 {
            getrandom::fill(&mut iv[..self.suite.iv_len()]).map_err(|_| EspError::Entropy)?;
        }
        self.seal_with_iv(tcp, &iv[..self.suite.iv_len()])
    }
    fn seal_with_iv(&mut self, tcp: &[u8], iv: &[u8]) -> Result<Vec<u8>, EspError> {
        if !(20..=MAX_TCP_BYTES).contains(&tcp.len()) || iv.len() != self.suite.iv_len() {
            return Err(EspError::InvalidPacket);
        }
        self.sequence = self.sequence.checked_add(1).ok_or(EspError::Exhausted)?;
        let block = self.suite.block_len();
        let pad = (block - (tcp.len() + 2) % block) % block;
        let mut plain = Zeroizing::new(tcp.to_vec());
        for byte in 1..=pad {
            plain.push(byte as u8);
        }
        plain.extend_from_slice(&[pad as u8, 6]);
        encrypt(self.suite, &self.encryption, iv, &mut plain)?;
        let mut packet = Vec::with_capacity(8 + iv.len() + plain.len() + ICV_BYTES);
        packet.extend_from_slice(&self.spi.to_be_bytes());
        packet.extend_from_slice(&self.sequence.to_be_bytes());
        packet.extend_from_slice(iv);
        packet.extend_from_slice(&plain);
        append_icv(self.suite.integrity, &self.integrity, &mut packet)?;
        Ok(packet)
    }
}
impl Receiver {
    pub fn new(
        spi: u32,
        encryption: Zeroizing<[u8; 16]>,
        integrity: Zeroizing<[u8; 20]>,
    ) -> Result<Self, EspError> {
        Self::with_suite(
            spi,
            Suite::AES_SHA1,
            Zeroizing::new(encryption.to_vec()),
            Zeroizing::new(integrity.to_vec()),
        )
    }
    pub fn with_suite(
        spi: u32,
        suite: Suite,
        encryption: Zeroizing<Vec<u8>>,
        integrity: Zeroizing<Vec<u8>>,
    ) -> Result<Self, EspError> {
        if spi == 0 {
            return Err(EspError::InvalidSpi);
        }
        suite.validate_keys(&encryption, &integrity)?;
        Ok(Self {
            spi,
            highest: 0,
            window: 0,
            suite,
            encryption,
            integrity,
        })
    }
    pub fn open(&mut self, packet: &[u8]) -> Result<Zeroizing<Vec<u8>>, EspError> {
        let overhead = 8 + self.suite.iv_len() + ICV_BYTES;
        // NULL from the deployed Go peer uses block-size 1, while other peers
        // may add authenticated ESP alignment padding. Validate the trailer,
        // not a guessed NULL cipher alignment.
        if packet.len() < overhead + 22
            || packet.len() > overhead + MAX_TCP_BYTES + 257
            || !(packet.len() - overhead).is_multiple_of(self.suite.block_len())
        {
            return Err(EspError::InvalidPacket);
        }
        if u32::from_be_bytes(packet[..4].try_into().unwrap()) != self.spi {
            return Err(EspError::InvalidSpi);
        }
        let seq = u32::from_be_bytes(packet[4..8].try_into().unwrap());
        if seq == 0
            || (seq <= self.highest
                && (self.highest - seq >= 64 || self.window & (1u64 << (self.highest - seq)) != 0))
        {
            return Err(EspError::Replay);
        }
        let authenticated = packet.len() - ICV_BYTES;
        verify_icv(
            self.suite.integrity,
            &self.integrity,
            &packet[..authenticated],
            &packet[authenticated..],
        )?;
        let offset = 8 + self.suite.iv_len();
        let mut plain = Zeroizing::new(packet[offset..authenticated].to_vec());
        decrypt(self.suite, &self.encryption, &packet[8..offset], &mut plain)?;
        let pad = usize::from(plain[plain.len() - 2]);
        let end = plain
            .len()
            .checked_sub(pad + 2)
            .ok_or(EspError::InvalidPacket)?;
        if !(20..=MAX_TCP_BYTES).contains(&end)
            || plain[plain.len() - 1] != 6
            || plain[end..end + pad]
                .iter()
                .enumerate()
                .any(|(index, byte)| usize::from(*byte) != index + 1)
        {
            return Err(EspError::InvalidPacket);
        }
        plain.truncate(end);
        // Neither a bad ICV, a bad cipher key nor an invalid trailer consumes
        // sequence space. Commit only after the authenticated frame is valid.
        if seq > self.highest {
            let shift = seq - self.highest;
            self.window = if shift >= 64 {
                1
            } else {
                (self.window << shift) | 1
            };
            self.highest = seq;
        } else {
            self.window |= 1u64 << (self.highest - seq);
        }
        Ok(plain)
    }
}
fn encrypt(suite: Suite, key: &[u8], iv: &[u8], plain: &mut [u8]) -> Result<(), EspError> {
    let len = plain.len();
    match suite.encryption {
        Encryption::AesCbc => {
            cbc::Encryptor::<aes::Aes128>::new_from_slices(key, iv)
                .map_err(|_| EspError::InvalidKey)?
                .encrypt_padded::<NoPadding>(plain, len)
                .map_err(|_| EspError::InvalidPacket)?;
        }
        Encryption::TripleDesCbc => {
            cbc::Encryptor::<des::TdesEde3>::new_from_slices(key, iv)
                .map_err(|_| EspError::InvalidKey)?
                .encrypt_padded::<NoPadding>(plain, len)
                .map_err(|_| EspError::InvalidPacket)?;
        }
        Encryption::Null => {}
    }
    Ok(())
}
fn decrypt(suite: Suite, key: &[u8], iv: &[u8], plain: &mut [u8]) -> Result<(), EspError> {
    match suite.encryption {
        Encryption::AesCbc => {
            cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
                .map_err(|_| EspError::InvalidKey)?
                .decrypt_padded::<NoPadding>(plain)
                .map_err(|_| EspError::InvalidPacket)?;
        }
        Encryption::TripleDesCbc => {
            cbc::Decryptor::<des::TdesEde3>::new_from_slices(key, iv)
                .map_err(|_| EspError::InvalidKey)?
                .decrypt_padded::<NoPadding>(plain)
                .map_err(|_| EspError::InvalidPacket)?;
        }
        Encryption::Null => {}
    }
    Ok(())
}
fn append_icv(integrity: Integrity, key: &[u8], packet: &mut Vec<u8>) -> Result<(), EspError> {
    match integrity {
        Integrity::Sha1 => {
            let mut mac = Hmac::<Sha1>::new_from_slice(key).map_err(|_| EspError::InvalidKey)?;
            mac.update(packet);
            packet.extend_from_slice(&mac.finalize().into_bytes()[..ICV_BYTES]);
        }
        Integrity::Md5 => {
            let mut mac = Hmac::<Md5>::new_from_slice(key).map_err(|_| EspError::InvalidKey)?;
            mac.update(packet);
            packet.extend_from_slice(&mac.finalize().into_bytes()[..ICV_BYTES]);
        }
    }
    Ok(())
}
fn verify_icv(integrity: Integrity, key: &[u8], data: &[u8], icv: &[u8]) -> Result<(), EspError> {
    match integrity {
        Integrity::Sha1 => {
            let mut mac = Hmac::<Sha1>::new_from_slice(key).map_err(|_| EspError::InvalidKey)?;
            mac.update(data);
            mac.verify_truncated_left(icv)
                .map_err(|_| EspError::Authentication)
        }
        Integrity::Md5 => {
            let mut mac = Hmac::<Md5>::new_from_slice(key).map_err(|_| EspError::InvalidKey)?;
            mac.update(data);
            mac.verify_truncated_left(icv)
                .map_err(|_| EspError::Authentication)
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    #[test]
    fn tamper_replay_reordering_and_exhaustion() {
        let mut sender =
            Sender::new(42, Zeroizing::new([0x11; 16]), Zeroizing::new([0x22; 20])).unwrap();
        let mut receiver =
            Receiver::new(42, Zeroizing::new([0x11; 16]), Zeroizing::new([0x22; 20])).unwrap();
        let payload = [0x33; 20];
        let first = sender.seal_with_iv(&payload, &[0x44; 16]).unwrap();
        let hex: String = first.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "0000002a0000000144444444444444444444444444444444dc44b424bcb85288ce3be42430864e8b29687bec19b041a7bd700c5353ba8ac3c8dece0d2b98e9920ce61645"
        );
        let second = sender.seal(&payload).unwrap();
        let mut bad = second.clone();
        bad[7] = 200;
        assert_eq!(receiver.open(&bad), Err(EspError::Authentication));
        assert_eq!(receiver.highest, 0);
        assert_eq!(&*receiver.open(&second).unwrap(), &payload);
        assert_eq!(&*receiver.open(&first).unwrap(), &payload);
        assert_eq!(receiver.open(&first), Err(EspError::Replay));
        sender.sequence = u32::MAX;
        assert_eq!(sender.seal(&payload), Err(EspError::Exhausted));
    }

    #[test]
    fn wire_length_boundaries_are_explicit() {
        for (tcp, wire) in [
            (20, 68),
            (1214, 1252),
            (1215, 1268),
            (1198, 1236),
            (1199, 1252),
        ] {
            assert_eq!(sealed_len(tcp), Some(wire));
        }
        assert_eq!(sealed_len(usize::MAX), None);
    }
}

#[cfg(test)]
#[path = "security_esp/tests.rs"]
mod suite_tests;
