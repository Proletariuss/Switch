use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha384, Sha512};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyDerivationError {
    UnsupportedPrf(u16),
    EmptyNonce,
    InvalidNonceLength,
    EmptySharedSecret,
    MissingSpi,
    InvalidKeyLengths,
    OutputTooLarge,
    InvalidAuthInput,
}

impl std::fmt::Display for KeyDerivationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPrf(id) => write!(formatter, "unsupported IKE PRF transform {id}"),
            Self::EmptyNonce => formatter.write_str("IKE nonces must not be empty"),
            Self::InvalidNonceLength => {
                formatter.write_str("IKE nonce length must be between 16 and 256 bytes")
            }
            Self::EmptySharedSecret => formatter.write_str("IKE DH shared secret is empty"),
            Self::MissingSpi => formatter.write_str("IKE SA SPIs must be non-zero"),
            Self::InvalidKeyLengths => formatter.write_str("invalid IKE SA key lengths"),
            Self::OutputTooLarge => formatter.write_str("IKE PRF+ output exceeds its limit"),
            Self::InvalidAuthInput => formatter.write_str(
                "IKE EAP AUTH requires original handshake bytes and complete key material",
            ),
        }
    }
}

impl std::error::Error for KeyDerivationError {}

/// Initial CHILD_SA keys in initiator wire direction. Encryption lengths
/// include the algorithm's salt; AEAD uses zero integrity-key bytes.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ChildSaKeyMaterial {
    pub initiator_encryption: Vec<u8>,
    pub initiator_integrity: Vec<u8>,
    pub responder_encryption: Vec<u8>,
    pub responder_integrity: Vec<u8>,
}

impl std::fmt::Debug for ChildSaKeyMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChildSaKeyMaterial([REDACTED])")
    }
}

/// RFC 7296 2.17: initial CHILD_SA only, not CREATE_CHILD_SA/PFS.
pub fn derive_initial_child_sa_keys(
    prf_id: u16,
    sk_d: &[u8],
    ni: &[u8],
    nr: &[u8],
    encryption_key_len: usize,
    integrity_key_len: usize,
) -> Result<ChildSaKeyMaterial, KeyDerivationError> {
    derive_rekey_child_sa_keys(
        prf_id,
        sk_d,
        &[],
        ni,
        nr,
        encryption_key_len,
        integrity_key_len,
    )
}
/// RFC 7296 2.17: CREATE_CHILD_SA key seed is g^ir | Ni | Nr.
#[allow(clippy::too_many_arguments)]
pub fn derive_rekey_child_sa_keys(
    prf_id: u16,
    sk_d: &[u8],
    shared: &[u8],
    ni: &[u8],
    nr: &[u8],
    encryption_key_len: usize,
    integrity_key_len: usize,
) -> Result<ChildSaKeyMaterial, KeyDerivationError> {
    if shared.len() > 512 {
        return Err(KeyDerivationError::InvalidKeyLengths);
    }
    if !(16..=256).contains(&ni.len()) || !(16..=256).contains(&nr.len()) {
        return Err(KeyDerivationError::InvalidNonceLength);
    }
    if sk_d.is_empty()
        || sk_d.len() > 64
        || encryption_key_len == 0
        || encryption_key_len > 64
        || integrity_key_len > 64
    {
        return Err(KeyDerivationError::InvalidKeyLengths);
    }
    let mut seed = Zeroizing::new(Vec::with_capacity(shared.len() + ni.len() + nr.len()));
    seed.extend_from_slice(shared);
    seed.extend_from_slice(ni);
    seed.extend_from_slice(nr);
    let keymat = prf_plus(
        prf_id,
        sk_d,
        &seed,
        2 * (encryption_key_len + integrity_key_len),
    )?;
    let mut offset = 0;
    let mut take = |len: usize| {
        let value = keymat[offset..offset + len].to_vec();
        offset += len;
        value
    };
    Ok(ChildSaKeyMaterial {
        initiator_encryption: take(encryption_key_len),
        initiator_integrity: take(integrity_key_len),
        responder_encryption: take(encryption_key_len),
        responder_integrity: take(integrity_key_len),
    })
}

/// IKEv2 SA key material in RFC 7296 order. Secret bytes are erased on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct IkeSaKeyMaterial {
    pub sk_d: Vec<u8>,
    pub sk_ai: Vec<u8>,
    pub sk_ar: Vec<u8>,
    pub sk_ei: Vec<u8>,
    pub sk_er: Vec<u8>,
    pub sk_pi: Vec<u8>,
    pub sk_pr: Vec<u8>,
}

impl std::fmt::Debug for IkeSaKeyMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("IkeSaKeyMaterial([REDACTED])")
    }
}

// These arguments are the exact RFC 7296 SKEYSEED inputs and negotiated key
// lengths; grouping them would obscure call-site ordering without reducing state.
#[allow(clippy::too_many_arguments)]
pub fn derive_ike_sa_key_material(
    prf_id: u16,
    ni: &[u8],
    nr: &[u8],
    dh_shared_secret: &[u8],
    spi_i: u64,
    spi_r: u64,
    encryption_key_len: usize,
    integrity_key_len: usize,
) -> Result<IkeSaKeyMaterial, KeyDerivationError> {
    derive_ike_keys(
        prf_id,
        ni,
        nr,
        dh_shared_secret,
        spi_i,
        spi_r,
        encryption_key_len,
        integrity_key_len,
        None,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn derive_rekey_ike_sa_key_material(
    prf_id: u16,
    old_sk_d: &[u8],
    ni: &[u8],
    nr: &[u8],
    shared: &[u8],
    spi_i: u64,
    spi_r: u64,
    encryption_key_len: usize,
    integrity_key_len: usize,
) -> Result<IkeSaKeyMaterial, KeyDerivationError> {
    if old_sk_d.is_empty()
        || old_sk_d.len() > 64
        || !(16..=256).contains(&ni.len())
        || !(16..=256).contains(&nr.len())
    {
        return Err(KeyDerivationError::InvalidKeyLengths);
    }
    derive_ike_keys(
        prf_id,
        ni,
        nr,
        shared,
        spi_i,
        spi_r,
        encryption_key_len,
        integrity_key_len,
        Some(old_sk_d),
    )
}
#[allow(clippy::too_many_arguments)]
fn derive_ike_keys(
    prf_id: u16,
    ni: &[u8],
    nr: &[u8],
    dh_shared_secret: &[u8],
    spi_i: u64,
    spi_r: u64,
    encryption_key_len: usize,
    integrity_key_len: usize,
    old_sk_d: Option<&[u8]>,
) -> Result<IkeSaKeyMaterial, KeyDerivationError> {
    if ni.is_empty() || nr.is_empty() {
        return Err(KeyDerivationError::EmptyNonce);
    }
    if dh_shared_secret.is_empty() {
        return Err(KeyDerivationError::EmptySharedSecret);
    }
    if spi_i == 0 || spi_r == 0 {
        return Err(KeyDerivationError::MissingSpi);
    }
    if encryption_key_len == 0 {
        return Err(KeyDerivationError::InvalidKeyLengths);
    }

    let prf_len: usize = match prf_id {
        2 => 20,
        5 => 32,
        6 => 48,
        7 => 64,
        other => return Err(KeyDerivationError::UnsupportedPrf(other)),
    };
    let total_len = prf_len
        .checked_mul(3)
        .and_then(|len| len.checked_add(integrity_key_len.checked_mul(2)?))
        .and_then(|len| len.checked_add(encryption_key_len.checked_mul(2)?))
        .ok_or(KeyDerivationError::OutputTooLarge)?;
    let rounds = total_len.div_ceil(prf_len);
    if rounds > u8::MAX as usize {
        return Err(KeyDerivationError::OutputTooLarge);
    }

    let mut nonce_seed = Zeroizing::new(Vec::with_capacity(ni.len() + nr.len()));
    nonce_seed.extend_from_slice(ni);
    nonce_seed.extend_from_slice(nr);
    let skeyseed = if let Some(old_sk_d) = old_sk_d {
        let mut seed = Zeroizing::new(dh_shared_secret.to_vec());
        seed.extend_from_slice(&nonce_seed);
        prf(prf_id, old_sk_d, &seed)?
    } else {
        prf(prf_id, &nonce_seed, dh_shared_secret)?
    };

    let mut key_seed = Zeroizing::new(Vec::with_capacity(ni.len() + nr.len() + 16));
    key_seed.extend_from_slice(ni);
    key_seed.extend_from_slice(nr);
    key_seed.extend_from_slice(&spi_i.to_be_bytes());
    key_seed.extend_from_slice(&spi_r.to_be_bytes());
    let keymat = prf_plus(prf_id, &skeyseed, &key_seed, total_len)?;

    let mut offset = 0;
    let mut take = |length: usize| {
        let value = keymat[offset..offset + length].to_vec();
        offset += length;
        value
    };
    let sk_d = take(prf_len);
    let sk_ai = take(integrity_key_len);
    let sk_ar = take(integrity_key_len);
    let sk_ei = take(encryption_key_len);
    let sk_er = take(encryption_key_len);
    let sk_pi = take(prf_len);
    let sk_pr = take(prf_len);
    Ok(IkeSaKeyMaterial {
        sk_d,
        sk_ai,
        sk_ar,
        sk_ei,
        sk_er,
        sk_pi,
        sk_pr,
    })
}

/// Computes shared-key AUTH after EAP using the original IKE_SA_INIT message,
/// peer nonce, and exact ID payload body (excluding the generic payload header).
/// The caller selects SK_pi for initiator AUTH or SK_pr for responder AUTH.
pub fn compute_eap_auth(
    prf_id: u16,
    msk: &[u8],
    initial_message: &[u8],
    peer_nonce: &[u8],
    sk_p: &[u8],
    identity_body: &[u8],
) -> Result<Zeroizing<Vec<u8>>, KeyDerivationError> {
    if msk.len() != 64
        || sk_p.is_empty()
        || !(28..=1_048_576).contains(&initial_message.len())
        || !(16..=256).contains(&peer_nonce.len())
        || !(5..=2052).contains(&identity_body.len())
    {
        return Err(KeyDerivationError::InvalidAuthInput);
    }
    let auth_key = prf(prf_id, msk, b"Key Pad for IKEv2")?;
    let signed = auth_signed_octets(prf_id, initial_message, peer_nonce, sk_p, identity_body)?;
    prf(prf_id, &auth_key, &signed)
}

/// RFC 7296 section 2.15. `initial_message` is the exact successful SA_INIT
/// message including its real IKE header, excluding a NAT-T marker.
pub fn auth_signed_octets(
    prf_id: u16,
    initial_message: &[u8],
    peer_nonce: &[u8],
    sk_p: &[u8],
    identity_body: &[u8],
) -> Result<Zeroizing<Vec<u8>>, KeyDerivationError> {
    if sk_p.is_empty()
        || !(28..=1_048_576).contains(&initial_message.len())
        || !(16..=256).contains(&peer_nonce.len())
        || !(5..=2052).contains(&identity_body.len())
    {
        return Err(KeyDerivationError::InvalidAuthInput);
    }
    let identity_hash = prf(prf_id, sk_p, identity_body)?;
    let mut signed = Zeroizing::new(Vec::with_capacity(
        initial_message.len() + peer_nonce.len() + identity_hash.len(),
    ));
    signed.extend_from_slice(initial_message);
    signed.extend_from_slice(peer_nonce);
    signed.extend_from_slice(&identity_hash);
    Ok(signed)
}

fn prf(prf_id: u16, key: &[u8], data: &[u8]) -> Result<Zeroizing<Vec<u8>>, KeyDerivationError> {
    macro_rules! calculate {
        ($digest:ty) => {{
            let mut mac =
                Hmac::<$digest>::new_from_slice(key).expect("HMAC accepts keys of any size");
            mac.update(data);
            Ok(Zeroizing::new(mac.finalize().into_bytes().to_vec()))
        }};
    }
    match prf_id {
        2 => calculate!(Sha1),
        5 => calculate!(Sha256),
        6 => calculate!(Sha384),
        7 => calculate!(Sha512),
        other => Err(KeyDerivationError::UnsupportedPrf(other)),
    }
}

fn prf_plus(
    prf_id: u16,
    key: &[u8],
    seed: &[u8],
    output_len: usize,
) -> Result<Zeroizing<Vec<u8>>, KeyDerivationError> {
    let mut output = Zeroizing::new(Vec::with_capacity(output_len));
    let mut previous = Zeroizing::new(Vec::new());
    let mut counter: u8 = 1;
    while output.len() < output_len {
        let mut input = Zeroizing::new(Vec::with_capacity(previous.len() + seed.len() + 1));
        input.extend_from_slice(&previous);
        input.extend_from_slice(seed);
        input.push(counter);
        previous = prf(prf_id, key, &input)?;
        output.extend_from_slice(&previous);
        if output.len() < output_len {
            counter = counter
                .checked_add(1)
                .ok_or(KeyDerivationError::OutputTooLarge)?;
        }
    }
    output.truncate(output_len);
    Ok(output)
}

#[cfg(test)]
mod tests {
    #[test]
    fn initial_child_key_layout_matches_go() {
        for (enc, integ, digest) in [
            (
                20,
                0,
                "5bdcd5b059dc47412c71c4d6acd2c39565880eb3edcc8bdf9bf733d9087735aa",
            ),
            (
                16,
                20,
                "cac0241e0fad35ca1a2e4ded8d8a2bb6ac1ec9d7226c2c807b3d4e5e666e32e4",
            ),
        ] {
            let keys =
                derive_initial_child_sa_keys(5, &[1; 32], &[2; 32], &[3; 32], enc, integ).unwrap();
            assert_eq!(keys.initiator_encryption.len(), enc);
            assert_eq!(keys.responder_encryption.len(), enc);
            assert_eq!(keys.initiator_integrity.len(), integ);
            assert_eq!(keys.responder_integrity.len(), integ);
            assert_ne!(keys.initiator_encryption, keys.responder_encryption);
            let bytes = [
                &keys.initiator_encryption[..],
                &keys.initiator_integrity,
                &keys.responder_encryption,
                &keys.responder_integrity,
            ]
            .concat();
            assert_eq!(format!("{:x}", sha2::Sha256::digest(bytes)), digest);
            assert_eq!(format!("{keys:?}"), "ChildSaKeyMaterial([REDACTED])");
        }
        for (enc, integ) in [(0, 0), (usize::MAX, 0), (20, usize::MAX)] {
            assert!(
                derive_initial_child_sa_keys(5, &[1; 32], &[2; 32], &[3; 32], enc, integ).is_err()
            );
        }
        assert!(derive_initial_child_sa_keys(5, &[], &[2; 32], &[3; 32], 20, 0).is_err());
        assert!(derive_initial_child_sa_keys(5, &[1; 32], &[], &[3; 32], 20, 0).is_err());
        assert!(derive_initial_child_sa_keys(999, &[1; 32], &[2; 32], &[3; 32], 20, 0).is_err());
    }
    #[test]
    fn initial_child_aes256_sha256_keymat_matches_independent_hmac_vector() {
        // Independent Node/OpenSSL PRF+ over Ni|Nr. Each direction consumes
        // encryption before integrity per RFC 7296 section 2.17.
        let sk_d: Vec<u8> = (0..32).collect();
        let ni: Vec<u8> = (32..64).collect();
        let nr: Vec<u8> = (64..96).collect();
        let keys = derive_initial_child_sa_keys(5, &sk_d, &ni, &nr, 32, 32).unwrap();
        let keymat = [
            &keys.initiator_encryption[..],
            &keys.initiator_integrity,
            &keys.responder_encryption,
            &keys.responder_integrity,
        ]
        .concat();
        let actual: String = keymat.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            actual,
            "7676c7ad3107b5b9a5ec63e9747656801948fc75041b99bb4270a59d301920439e6d08df81510a08f7392ce50690b231212f56961425d46373d1b08cdf38890ac68e39a50d51ddf3d2d5a498c6e83703f29e193f6116d472f84882a9a53b64960e42c442d39b917b072eeda3e655d0da1eb6f6d076d9870fda222ee465bd3b19"
        );
    }
    #[test]
    fn eap_auth_matches_go_custom_identity_golden_vector() {
        let identity = [b"\x03\0\0\0".as_slice(), b"custom@example"].concat();
        let result =
            super::compute_eap_auth(5, &[1; 64], &[3; 28], &[4; 32], &[2; 32], &identity).unwrap();
        let hex: String = result.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            hex,
            "f9eb1b3c8b166e136bd843b332f8a2ceab1a169f599c5f6df4a76deda60f0ed2"
        );
        assert!(super::compute_eap_auth(5, &[], &[3; 28], &[4; 32], &[2; 32], &identity).is_err());
        assert!(super::compute_eap_auth(5, &[1; 64], &[], &[4; 32], &[2; 32], &identity).is_err());
        assert!(super::compute_eap_auth(5, &[1; 64], &[3; 28], &[4; 32], &[2; 32], &[]).is_err());
    }
    use super::*;
    use sha2::Digest;

    #[test]
    fn sha256_key_layout_matches_go_ike_sa_keymat_golden_vector() {
        let keys = derive_ike_sa_key_material(
            5,
            b"initiator nonce",
            b"responder nonce",
            b"shared dh secret",
            0x0102_0304_0506_0708,
            0x1112_1314_1516_1718,
            20,
            32,
        )
        .unwrap();
        let full_keymat = [
            keys.sk_d.as_slice(),
            keys.sk_ai.as_slice(),
            keys.sk_ar.as_slice(),
            keys.sk_ei.as_slice(),
            keys.sk_er.as_slice(),
            keys.sk_pi.as_slice(),
            keys.sk_pr.as_slice(),
        ]
        .concat();
        assert_eq!(
            full_keymat,
            hex(
                "0df4219b2621bd4e400a3dad1e98eb87194b41a72c5b0a00991d8710869709ac719333acd42d03428cfe78a9adfc6f3fa3d9f9ad339b62143e055f506c27e22cf874de15d05332a45e181a01ccb025c5e768290ab2109ebc0955157e97364db3599badffafeaca277a412f5c736fbc74a2644028ef2e076b83060e00669e376e7bdc9953c64580ed36e6c741323ff6893c2f4043978f453f43eee7e19f4da117ab3ff33dd8262cff33bb6b3b5c39daf1db3243422d6a28ecad73a04db9e281e0f8aa074f0913066e"
            )
        );
        assert_eq!(keys.sk_ai.len(), 32);
        assert_eq!(keys.sk_ar.len(), 32);
        assert_eq!(keys.sk_ei.len(), 20);
        assert_eq!(keys.sk_er.len(), 20);
        assert_eq!(keys.sk_pi.len(), 32);
        assert_eq!(keys.sk_pr.len(), 32);
    }

    #[test]
    fn aead_layout_omits_integrity_keys_and_keeps_salt_in_encryption_length() {
        let keys = derive_ike_sa_key_material(5, b"ni", b"nr", b"dh", 1, 2, 20, 0).unwrap();
        assert!(keys.sk_ai.is_empty());
        assert!(keys.sk_ar.is_empty());
        assert_eq!(keys.sk_ei.len(), 20);
        assert_eq!(keys.sk_er.len(), 20);
    }

    #[test]
    fn supported_prfs_match_go_keymat_digests() {
        let vectors = [
            (
                2,
                "73639edf17a26150fe2549a13b796987113a72db89b300178ebfab0f3cdd29b1",
            ),
            (
                5,
                "5ff2f6669a10aaafd0a0aa1c29bfd6ab310d9ec78d776a6cc0902901a463455b",
            ),
            (
                6,
                "aaf69e443ccd1d4742fde4ac05b9a37cb2a29189133fa387be5427be4e831bf4",
            ),
            (
                7,
                "9724f834c5e28493b32d9850aa8641961679c40bc49677cf5c7dddb8a49d8c6e",
            ),
        ];
        for (prf_id, expected_hash) in vectors {
            let keys = derive_ike_sa_key_material(
                prf_id,
                b"initiator nonce",
                b"responder nonce",
                b"shared dh secret",
                0x0102_0304_0506_0708,
                0x1112_1314_1516_1718,
                20,
                32,
            )
            .unwrap();
            let keymat = [
                keys.sk_d.as_slice(),
                keys.sk_ai.as_slice(),
                keys.sk_ar.as_slice(),
                keys.sk_ei.as_slice(),
                keys.sk_er.as_slice(),
                keys.sk_pi.as_slice(),
                keys.sk_pr.as_slice(),
            ]
            .concat();
            assert_eq!(
                hex(&format!("{:x}", Sha256::digest(&keymat))),
                hex(expected_hash)
            );
        }
    }

    #[test]
    fn unsupported_and_invalid_inputs_fail_closed() {
        assert_eq!(
            derive_ike_sa_key_material(1, b"ni", b"nr", b"dh", 1, 2, 16, 16).unwrap_err(),
            KeyDerivationError::UnsupportedPrf(1)
        );
        assert_eq!(
            derive_ike_sa_key_material(5, b"", b"nr", b"dh", 1, 2, 16, 16).unwrap_err(),
            KeyDerivationError::EmptyNonce
        );
        assert_eq!(
            derive_ike_sa_key_material(5, b"ni", b"nr", b"dh", 0, 2, 16, 16).unwrap_err(),
            KeyDerivationError::MissingSpi
        );
    }

    fn hex(value: &str) -> Vec<u8> {
        let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
        assert!(remainder.is_empty());
        pairs
            .iter()
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    _ => panic!("invalid hex"),
                };
                digit(pair[0]) * 16 + digit(pair[1])
            })
            .collect()
    }
}
