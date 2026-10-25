//! Exact selected IMS transforms and 3GPP CK_IM/IK_IM mapping.
//! Supporting a transform does not advertise it or alter negotiation policy.
use super::EspError;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encryption {
    AesCbc,
    TripleDesCbc,
    Null,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integrity {
    Sha1,
    Md5,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Suite {
    pub encryption: Encryption,
    pub integrity: Integrity,
}
impl Suite {
    pub const AES_SHA1: Self = Self {
        encryption: Encryption::AesCbc,
        integrity: Integrity::Sha1,
    };
    pub fn selected(encryption: &str, integrity: &str) -> Result<Self, EspError> {
        let encryption = match encryption {
            "aes-cbc" => Encryption::AesCbc,
            "des-ede3-cbc" => Encryption::TripleDesCbc,
            "null" => Encryption::Null,
            _ => return Err(EspError::UnsupportedTransform),
        };
        let integrity = match integrity {
            "hmac-sha-1-96" => Integrity::Sha1,
            "hmac-md5-96" => Integrity::Md5,
            _ => return Err(EspError::UnsupportedTransform),
        };
        Ok(Self {
            encryption,
            integrity,
        })
    }
    pub fn iv_len(self) -> usize {
        match self.encryption {
            Encryption::AesCbc => 16,
            Encryption::TripleDesCbc => 8,
            Encryption::Null => 0,
        }
    }
    pub fn block_len(self) -> usize {
        match self.encryption {
            Encryption::AesCbc => 16,
            Encryption::TripleDesCbc => 8,
            Encryption::Null => 1,
        }
    }
    pub fn encryption_key_len(self) -> usize {
        match self.encryption {
            Encryption::AesCbc => 16,
            Encryption::TripleDesCbc => 24,
            Encryption::Null => 0,
        }
    }
    pub fn integrity_key_len(self) -> usize {
        match self.integrity {
            Integrity::Sha1 => 20,
            Integrity::Md5 => 16,
        }
    }
    pub(crate) fn derive(
        self,
        ck: &[u8; 16],
        ik: &[u8; 16],
    ) -> (Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>) {
        let encryption = match self.encryption {
            Encryption::AesCbc => Zeroizing::new(ck.to_vec()),
            Encryption::TripleDesCbc => {
                // Preserve the existing IMS CK_IM expansion: K1/K2/K1,
                // with odd parity in the least significant bit of each byte.
                let mut key = Zeroizing::new(Vec::with_capacity(24));
                key.extend_from_slice(ck);
                key.extend_from_slice(&ck[..8]);
                for byte in &mut *key {
                    *byte &= 0xfe;
                    *byte |= u8::from(byte.count_ones().is_multiple_of(2));
                }
                key
            }
            Encryption::Null => Zeroizing::new(vec![]),
        };
        let mut integrity = Zeroizing::new(ik.to_vec());
        if self.integrity == Integrity::Sha1 {
            integrity.extend_from_slice(&[0; 4]);
        }
        (encryption, integrity)
    }
    pub(crate) fn validate_keys(self, encryption: &[u8], integrity: &[u8]) -> Result<(), EspError> {
        if encryption.len() != self.encryption_key_len()
            || integrity.len() != self.integrity_key_len()
        {
            return Err(EspError::InvalidKey);
        }
        Ok(())
    }
}
