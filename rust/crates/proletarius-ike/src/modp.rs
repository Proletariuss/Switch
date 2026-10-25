use crypto_bigint::{U1024, U2048, const_monty_params, modular::ConstMontyForm};
use zeroize::{Zeroize, Zeroizing};

const MODP2048_PRIME: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA051015728E5A8AACAA68FFFFFFFFFFFFFFFF";

// RFC 2409 section 6.2 (IKE MODP group 2). This 1024-bit group is weak by
// modern standards and is only selected when the matched carrier profile
// explicitly requires group 2; it is never a fallback for a stronger offer.
const MODP1024_PRIME: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE65381FFFFFFFFFFFFFFFF";

const_monty_params!(Modp1024, U1024, MODP1024_PRIME);
type Modp1024Form = ConstMontyForm<Modp1024, { U1024::LIMBS }>;
const MODP1024_P: U1024 = U1024::from_be_hex(MODP1024_PRIME);

const_monty_params!(Modp2048, U2048, MODP2048_PRIME);
type Modp2048Form = ConstMontyForm<Modp2048, { U2048::LIMBS }>;
const MODP2048_P: U2048 = U2048::from_be_hex(MODP2048_PRIME);

const GROUP14_BYTES: usize = 256;
const GROUP2_BYTES: usize = 128;

#[derive(Debug, Eq, PartialEq)]
pub enum DhError {
    EntropyUnavailable,
    InvalidPeerKeyLength,
    InvalidPeerPublicKey,
    InvalidGroup2PeerKeyLength,
    InvalidGroup2PeerPublicKey,
}

impl std::fmt::Display for DhError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EntropyUnavailable => {
                formatter.write_str("system cryptographic entropy unavailable")
            }
            Self::InvalidPeerKeyLength => {
                formatter.write_str("MODP-2048 peer key must be 256 bytes")
            }
            Self::InvalidPeerPublicKey => {
                formatter.write_str("MODP-2048 peer public key is outside the valid range")
            }
            Self::InvalidGroup2PeerKeyLength => {
                formatter.write_str("MODP-1024 peer key must be 128 bytes")
            }
            Self::InvalidGroup2PeerPublicKey => {
                formatter.write_str("MODP-1024 peer public key is outside the valid range")
            }
        }
    }
}

impl std::error::Error for DhError {}

/// One-shot IKEv2 MODP-2048 key pair (DH group 14).
/// The private exponent is kept in zeroizing memory and its debug output is redacted.
pub struct Modp2048KeyPair {
    private_exponent: Zeroizing<U2048>,
    public_key: Vec<u8>,
}

impl std::fmt::Debug for Modp2048KeyPair {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Modp2048KeyPair([PRIVATE KEY REDACTED])")
    }
}

impl Modp2048KeyPair {
    pub fn generate() -> Result<Self, DhError> {
        loop {
            let mut bytes = Zeroizing::new([0u8; GROUP14_BYTES]);
            getrandom::fill(&mut bytes[..]).map_err(|_| DhError::EntropyUnavailable)?;
            let candidate = Zeroizing::new(U2048::from_be_slice(&bytes[..]));
            let upper_bound = MODP2048_P.wrapping_sub(&U2048::ONE);
            if *candidate > U2048::ONE && *candidate < upper_bound {
                return Ok(Self::from_private_exponent(*candidate));
            }
        }
    }

    pub fn public_key(&self) -> &[u8] {
        &self.public_key
    }

    pub fn shared_secret(&self, peer_public_key: &[u8]) -> Result<Zeroizing<Vec<u8>>, DhError> {
        if peer_public_key.len() != GROUP14_BYTES {
            return Err(DhError::InvalidPeerKeyLength);
        }
        let peer = U2048::from_be_slice(peer_public_key);
        let upper_bound = MODP2048_P.wrapping_sub(&U2048::ONE);
        if peer <= U2048::ONE || peer >= upper_bound {
            return Err(DhError::InvalidPeerPublicKey);
        }
        let subgroup_order = upper_bound.shr_vartime(1);
        if Modp2048Form::new(&peer).pow(&subgroup_order).retrieve() != U2048::ONE {
            return Err(DhError::InvalidPeerPublicKey);
        }
        let mut shared = Modp2048Form::new(&peer)
            .pow(&self.private_exponent)
            .retrieve();
        let encoded = shared.to_be_bytes();
        let result = Zeroizing::new(encoded.as_ref().to_vec());
        shared.zeroize();
        Ok(result)
    }

    fn from_private_exponent(private_exponent: U2048) -> Self {
        let private_exponent = Zeroizing::new(private_exponent);
        let generator = U2048::from(2u8);
        let public = Modp2048Form::new(&generator)
            .pow(&private_exponent)
            .retrieve();
        let public_key = public.to_be_bytes().as_ref().to_vec();
        Self {
            private_exponent,
            public_key,
        }
    }
}

impl Drop for Modp2048KeyPair {
    fn drop(&mut self) {
        self.public_key.zeroize();
    }
}

/// IKEv2 MODP group 2 key pair. Use only for an exact, explicitly configured
/// carrier proposal; callers must not retry with this group after a stronger
/// proposal fails.
pub struct Modp1024KeyPair {
    private_exponent: Zeroizing<U1024>,
    public_key: Vec<u8>,
}

impl std::fmt::Debug for Modp1024KeyPair {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Modp1024KeyPair([PRIVATE KEY REDACTED])")
    }
}

impl Modp1024KeyPair {
    pub fn generate() -> Result<Self, DhError> {
        loop {
            let mut bytes = Zeroizing::new([0u8; GROUP2_BYTES]);
            getrandom::fill(&mut bytes[..]).map_err(|_| DhError::EntropyUnavailable)?;
            let candidate = Zeroizing::new(U1024::from_be_slice(&bytes[..]));
            let upper_bound = MODP1024_P.wrapping_sub(&U1024::ONE);
            if *candidate > U1024::ONE && *candidate < upper_bound {
                return Ok(Self::from_private_exponent(*candidate));
            }
        }
    }

    pub fn public_key(&self) -> &[u8] {
        &self.public_key
    }

    pub fn shared_secret(&self, peer_public_key: &[u8]) -> Result<Zeroizing<Vec<u8>>, DhError> {
        if peer_public_key.len() != GROUP2_BYTES {
            return Err(DhError::InvalidGroup2PeerKeyLength);
        }
        let peer = U1024::from_be_slice(peer_public_key);
        let upper_bound = MODP1024_P.wrapping_sub(&U1024::ONE);
        if peer <= U1024::ONE || peer >= upper_bound {
            return Err(DhError::InvalidGroup2PeerPublicKey);
        }
        // The RFC 2409 group-2 prime is a safe prime and 2 has order q.
        // Validate membership before using the peer value in a private pow.
        let subgroup_order = upper_bound.shr_vartime(1);
        if Modp1024Form::new(&peer).pow(&subgroup_order).retrieve() != U1024::ONE {
            return Err(DhError::InvalidGroup2PeerPublicKey);
        }
        let mut shared = Modp1024Form::new(&peer)
            .pow(&self.private_exponent)
            .retrieve();
        let encoded = shared.to_be_bytes();
        let result = Zeroizing::new(encoded.as_ref().to_vec());
        shared.zeroize();
        Ok(result)
    }

    fn from_private_exponent(private_exponent: U1024) -> Self {
        let private_exponent = Zeroizing::new(private_exponent);
        let generator = U1024::from(2u8);
        let public = Modp1024Form::new(&generator)
            .pow(&private_exponent)
            .retrieve();
        Self {
            private_exponent,
            public_key: public.to_be_bytes().as_ref().to_vec(),
        }
    }
}

impl Drop for Modp1024KeyPair {
    fn drop(&mut self) {
        self.public_key.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn generated_keys_interoperate_and_have_fixed_width_encoding() {
        let alice = Modp2048KeyPair::generate().unwrap();
        let bob = Modp2048KeyPair::generate().unwrap();
        assert_eq!(alice.public_key().len(), GROUP14_BYTES);
        assert_eq!(bob.public_key().len(), GROUP14_BYTES);
        let alice_secret = alice.shared_secret(bob.public_key()).unwrap();
        let bob_secret = bob.shared_secret(alice.public_key()).unwrap();
        assert_eq!(*alice_secret, *bob_secret);
        assert_eq!(alice_secret.len(), GROUP14_BYTES);
    }

    #[test]
    fn group2_keys_interoperate_and_validate_fixed_width_peer_values() {
        let alice = Modp1024KeyPair::generate().unwrap();
        let bob = Modp1024KeyPair::generate().unwrap();
        assert_eq!(alice.public_key().len(), GROUP2_BYTES);
        assert_eq!(bob.public_key().len(), GROUP2_BYTES);
        let alice_secret = alice.shared_secret(bob.public_key()).unwrap();
        let bob_secret = bob.shared_secret(alice.public_key()).unwrap();
        assert_eq!(*alice_secret, *bob_secret);
        assert_eq!(alice_secret.len(), GROUP2_BYTES);
        assert_eq!(
            alice.shared_secret(&[1, 2]),
            Err(DhError::InvalidGroup2PeerKeyLength)
        );
        let mut one = [0_u8; GROUP2_BYTES];
        one[GROUP2_BYTES - 1] = 1;
        assert_eq!(
            alice.shared_secret(&one),
            Err(DhError::InvalidGroup2PeerPublicKey)
        );
    }

    #[test]
    fn rejects_short_and_out_of_range_peer_keys() {
        let local = Modp2048KeyPair::generate().unwrap();
        assert_eq!(
            local.shared_secret(&[1, 2]),
            Err(DhError::InvalidPeerKeyLength)
        );
        let zero = [0u8; GROUP14_BYTES];
        assert_eq!(
            local.shared_secret(&zero),
            Err(DhError::InvalidPeerPublicKey)
        );
        let one = {
            let mut bytes = [0u8; GROUP14_BYTES];
            bytes[GROUP14_BYTES - 1] = 1;
            bytes
        };
        assert_eq!(
            local.shared_secret(&one),
            Err(DhError::InvalidPeerPublicKey)
        );
        let p_minus_one = { MODP2048_P.wrapping_sub(&U2048::ONE).to_be_bytes() };
        assert_eq!(
            local.shared_secret(p_minus_one.as_ref()),
            Err(DhError::InvalidPeerPublicKey)
        );
        let p_minus_two = { MODP2048_P.wrapping_sub(&U2048::from(2u8)).to_be_bytes() };
        assert_eq!(
            local.shared_secret(p_minus_two.as_ref()),
            Err(DhError::InvalidPeerPublicKey)
        );
    }

    #[test]
    fn fixed_key_exchange_matches_go_group14_golden_digests() {
        let alice = Modp2048KeyPair::from_private_exponent(private_exponent(
            "d3c4b5a69788776655443322110fedcba98765432100123456789abcdef01234",
        ));
        let bob = Modp2048KeyPair::from_private_exponent(private_exponent(
            "a9b8c7d6e5f40312233445566778899abcdef0123456789fedcba98765432109",
        ));
        assert_eq!(
            digest(alice.public_key()),
            "0542c95e95e0bffb8e1e0003e706261b15b0112734da287fa26e2668bd56123c"
        );
        assert_eq!(
            digest(bob.public_key()),
            "32c89c937f0be8527b2796afdbe38326a3e4827ae0e4795fb6fd23aff1ebbbab"
        );
        let alice_shared = alice.shared_secret(bob.public_key()).unwrap();
        let bob_shared = bob.shared_secret(alice.public_key()).unwrap();
        assert_eq!(*alice_shared, *bob_shared);
        assert_eq!(
            digest(&alice_shared),
            "253a5f642d4dcf8ededf6b9ec01a823a04bb493a997f21b4d6a7a7fa46a3b48e"
        );
    }

    fn private_exponent(value: &str) -> U2048 {
        let mut bytes = Zeroizing::new([0u8; GROUP14_BYTES]);
        let digits = value.as_bytes();
        assert_eq!(digits.len(), 64);
        let (pairs, remainder) = digits.as_chunks::<2>();
        assert!(remainder.is_empty());
        for (index, pair) in pairs.iter().enumerate() {
            bytes[GROUP14_BYTES - 32 + index] = (hex_digit(pair[0]) << 4) | hex_digit(pair[1]);
        }
        U2048::from_be_slice(&bytes[..])
    }

    fn hex_digit(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => panic!("invalid hex digit"),
        }
    }

    fn digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}
