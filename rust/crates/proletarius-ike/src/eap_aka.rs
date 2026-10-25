use hmac::{Hmac, Mac};
use sha1::{Digest, Sha1, Sha1Core, compress, digest::core_api::Block};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

type HmacSha1 = Hmac<Sha1>;
type ParsedAttributes<'a> = std::collections::HashMap<u8, (usize, &'a [u8])>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EapAkaError {
    InvalidPacket,
    InvalidAttribute,
    DuplicateAttribute(u8),
    MissingAttribute(u8),
    InvalidChallenge,
    MacMismatch,
    CheckcodeMismatch,
    MissingIdentityTranscript,
    InvalidResultLength,
    InvalidAutsLength,
}

impl std::fmt::Display for EapAkaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPacket => formatter.write_str("invalid EAP-AKA packet"),
            Self::InvalidAttribute => formatter.write_str("invalid EAP-AKA attribute"),
            Self::DuplicateAttribute(kind) => {
                write!(formatter, "duplicate EAP-AKA attribute {kind}")
            }
            Self::MissingAttribute(kind) => write!(formatter, "missing EAP-AKA attribute {kind}"),
            Self::InvalidChallenge => formatter.write_str("invalid EAP-AKA challenge fields"),
            Self::MacMismatch => formatter.write_str("EAP-AKA AT_MAC validation failed"),
            Self::CheckcodeMismatch => {
                formatter.write_str("EAP-AKA AT_CHECKCODE validation failed")
            }
            Self::MissingIdentityTranscript => {
                formatter.write_str("EAP-AKA checkcode requires the EAP-Identity transcript")
            }
            Self::InvalidResultLength => {
                formatter.write_str("EAP-AKA RES length must be 4..=16 bytes")
            }
            Self::InvalidAutsLength => formatter.write_str("EAP-AKA AUTS must contain 14 bytes"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AkaCheckcodeMode {
    Minimal,
    Omit,
    Checkcode,
    Recompute,
}

impl std::error::Error for EapAkaError {}

/// Response to an EAP-AKA Challenge using CK/IK obtained from the USIM.
/// MSK and K_aut are zeroized when dropped and never included in Debug output.
pub struct EapAkaResponse {
    pub packet: Vec<u8>,
    pub msk: Zeroizing<Vec<u8>>,
    pub k_aut: Zeroizing<Vec<u8>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct EapAkaChallenge {
    pub identifier: u8,
    pub rand: [u8; 16],
    pub autn: [u8; 16],
}

impl std::fmt::Debug for EapAkaChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EapAkaChallenge")
            .field("identifier", &self.identifier)
            .field("rand", &"[REDACTED]")
            .field("autn", &"[REDACTED]")
            .finish()
    }
}

/// Parses the SIM inputs from an EAP-AKA Challenge without performing card
/// I/O. AT_MAC is required before any SIM transaction is attempted.
pub fn parse_eap_aka_challenge(request: &[u8]) -> Result<EapAkaChallenge, EapAkaError> {
    let (identifier, attributes) = parse_request(request)?;
    let rand = tail16(required(&attributes, 1)?)?;
    let autn = tail16(required(&attributes, 2)?)?;
    tail16(required(&attributes, 11)?)?;
    Ok(EapAkaChallenge {
        identifier,
        rand: rand.try_into().map_err(|_| EapAkaError::InvalidChallenge)?,
        autn: autn.try_into().map_err(|_| EapAkaError::InvalidChallenge)?,
    })
}

/// Builds the EAP-Response/AKA-Synchronization-Failure packet returned when
/// the SIM rejects the sequence number and supplies AUTS.
pub fn build_eap_aka_sync_failure(identifier: u8, auts: &[u8]) -> Result<Vec<u8>, EapAkaError> {
    if auts.len() != 14 {
        return Err(EapAkaError::InvalidAutsLength);
    }
    let mut attributes = Vec::with_capacity(16);
    encode_attribute(4, auts, &mut attributes)?;
    let length = 8_usize
        .checked_add(attributes.len())
        .filter(|length| *length <= u16::MAX as usize)
        .ok_or(EapAkaError::InvalidPacket)?;
    let mut packet = Vec::with_capacity(length);
    packet.extend_from_slice(&[2, identifier]);
    packet.extend_from_slice(&(length as u16).to_be_bytes());
    packet.extend_from_slice(&[23, 4, 0, 0]);
    packet.extend_from_slice(&attributes);
    Ok(packet)
}

impl std::fmt::Debug for EapAkaResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EapAkaResponse([KEYS REDACTED])")
    }
}

impl Drop for EapAkaResponse {
    fn drop(&mut self) {
        self.packet.zeroize();
    }
}

/// Validates a network EAP-AKA challenge and creates a MACed response.
/// This operates on a successful USIM AKA result; APDU transport and AKA sync
/// failure remain the caller's responsibility. `identity` is the exact NAI
/// returned for EAP-Identity and is used to derive MK per RFC 4187.
pub fn answer_eap_aka_challenge(
    request: &[u8],
    identity: &[u8],
    res: &[u8],
    ck: &[u8],
    ik: &[u8],
) -> Result<EapAkaResponse, EapAkaError> {
    answer_eap_aka_challenge_with_checkcode(
        request,
        identity,
        res,
        ck,
        ik,
        &[],
        AkaCheckcodeMode::Minimal,
    )
}

pub fn answer_eap_aka_challenge_with_checkcode(
    request: &[u8],
    identity: &[u8],
    res: &[u8],
    ck: &[u8],
    ik: &[u8],
    identity_transcript: &[u8],
    checkcode_mode: AkaCheckcodeMode,
) -> Result<EapAkaResponse, EapAkaError> {
    if !(4..=16).contains(&res.len()) {
        return Err(EapAkaError::InvalidResultLength);
    }
    if identity.is_empty() || ck.len() != 16 || ik.len() != 16 {
        return Err(EapAkaError::InvalidChallenge);
    }
    let (identifier, attributes) = parse_request(request)?;
    let rand = tail16(required(&attributes, 1)?)?;
    let autn = tail16(required(&attributes, 2)?)?;
    // Ensure both challenge attributes are actually well-formed even though
    // the USIM already consumed RAND/AUTN to produce RES/CK/IK.
    if rand.len() != 16 || autn.len() != 16 {
        return Err(EapAkaError::InvalidChallenge);
    }
    let received_mac = tail16(required(&attributes, 11)?)?;
    let mut matched = None;
    for (first, second) in [(ik, ck), (ck, ik)] {
        let mut mk_input = Zeroizing::new(Vec::with_capacity(identity.len() + 32));
        mk_input.extend_from_slice(identity);
        mk_input.extend_from_slice(first);
        mk_input.extend_from_slice(second);
        let mk = Zeroizing::new(Sha1::digest(&mk_input).to_vec());
        let mut keymat = fips186_2_prf(&mk, 96);
        let k_aut = Zeroizing::new(keymat[16..32].to_vec());
        let mut check = request.to_vec();
        let mac_offset = attributes
            .get(&11)
            .ok_or(EapAkaError::MissingAttribute(11))?
            .0;
        let mac_start = mac_offset
            .checked_add(4)
            .ok_or(EapAkaError::InvalidAttribute)?;
        let mac_end = mac_start
            .checked_add(16)
            .ok_or(EapAkaError::InvalidAttribute)?;
        if mac_end > check.len() {
            return Err(EapAkaError::InvalidAttribute);
        }
        check[mac_start..mac_end].fill(0);
        let mut mac = HmacSha1::new_from_slice(&k_aut).expect("HMAC accepts any key size");
        mac.update(&check);
        let full_mac = mac.finalize().into_bytes();
        if bool::from(full_mac[..16].ct_eq(received_mac)) {
            let msk = Zeroizing::new(keymat[32..96].to_vec());
            keymat.fill(0);
            matched = Some((k_aut, msk));
            break;
        }
        keymat.fill(0);
    }
    let (k_aut, msk) = matched.ok_or(EapAkaError::MacMismatch)?;

    let mut response_attrs = Zeroizing::new(Vec::with_capacity(32));
    let mut res_value = Zeroizing::new(Vec::with_capacity(res.len() + 2));
    res_value.extend_from_slice(
        &u16::try_from(res.len() * 8)
            .map_err(|_| EapAkaError::InvalidResultLength)?
            .to_be_bytes(),
    );
    res_value.extend_from_slice(res);
    encode_attribute(3, &res_value, &mut response_attrs)?;
    if attributes.contains_key(&135) {
        encode_attribute(135, &[0, 0], &mut response_attrs)?;
    }
    let has_checkcode = attributes.contains_key(&23);
    let include_checkcode = match checkcode_mode {
        AkaCheckcodeMode::Minimal | AkaCheckcodeMode::Omit => false,
        AkaCheckcodeMode::Checkcode => has_checkcode,
        AkaCheckcodeMode::Recompute => true,
    };
    if include_checkcode {
        if identity_transcript.is_empty() {
            return Err(EapAkaError::MissingIdentityTranscript);
        }
        let digest = Sha1::digest(identity_transcript);
        if let Some((_, received)) = attributes.get(&23)
            && (received.len() < digest.len()
                || !bool::from(digest.as_slice().ct_eq(&received[..digest.len()])))
        {
            return Err(EapAkaError::CheckcodeMismatch);
        }
        encode_attribute(23, &digest, &mut response_attrs)?;
    }
    let mac_attribute_offset = response_attrs.len();
    encode_attribute(11, &[0; 18], &mut response_attrs)?;
    let total_length = 8_usize
        .checked_add(response_attrs.len())
        .filter(|length| *length <= u16::MAX as usize)
        .ok_or(EapAkaError::InvalidPacket)?;
    let mut packet = Vec::with_capacity(total_length);
    packet.extend_from_slice(&[2, identifier]);
    packet.extend_from_slice(&(total_length as u16).to_be_bytes());
    packet.extend_from_slice(&[23, 1, 0, 0]);
    packet.extend_from_slice(&response_attrs);
    let mac_start = 8 + mac_attribute_offset + 4;
    let mut mac = HmacSha1::new_from_slice(&k_aut).expect("HMAC accepts any key size");
    mac.update(&packet);
    let digest = mac.finalize().into_bytes();
    packet[mac_start..mac_start + 16].copy_from_slice(&digest[..16]);
    Ok(EapAkaResponse { packet, msk, k_aut })
}

fn parse_request(packet: &[u8]) -> Result<(u8, ParsedAttributes<'_>), EapAkaError> {
    if packet.len() < 8
        || packet.len() > u16::MAX as usize
        || packet[0] != 1
        || packet[2..4] != (packet.len() as u16).to_be_bytes()
        || packet[4..8] != [23, 1, 0, 0]
    {
        return Err(EapAkaError::InvalidPacket);
    }
    let mut offset = 8;
    let mut attributes = std::collections::HashMap::new();
    while offset < packet.len() {
        let header = packet
            .get(offset..offset + 2)
            .ok_or(EapAkaError::InvalidAttribute)?;
        let length = usize::from(header[1])
            .checked_mul(4)
            .ok_or(EapAkaError::InvalidAttribute)?;
        let end = offset
            .checked_add(length)
            .ok_or(EapAkaError::InvalidAttribute)?;
        if length < 4 || end > packet.len() {
            return Err(EapAkaError::InvalidAttribute);
        }
        if attributes
            .insert(header[0], (offset, &packet[offset + 2..end]))
            .is_some()
        {
            return Err(EapAkaError::DuplicateAttribute(header[0]));
        }
        offset = end;
    }
    Ok((packet[1], attributes))
}

fn required<'a>(
    attrs: &'a std::collections::HashMap<u8, (usize, &'a [u8])>,
    kind: u8,
) -> Result<&'a [u8], EapAkaError> {
    attrs
        .get(&kind)
        .map(|(_, value)| *value)
        .ok_or(EapAkaError::MissingAttribute(kind))
}

fn tail16(value: &[u8]) -> Result<&[u8], EapAkaError> {
    value
        .get(value.len().saturating_sub(16)..)
        .filter(|tail| tail.len() == 16)
        .ok_or(EapAkaError::InvalidChallenge)
}

fn encode_attribute(kind: u8, value: &[u8], output: &mut Vec<u8>) -> Result<(), EapAkaError> {
    let raw_length = value
        .len()
        .checked_add(2)
        .ok_or(EapAkaError::InvalidAttribute)?;
    let padded_length = raw_length
        .checked_add(3)
        .ok_or(EapAkaError::InvalidAttribute)?
        & !3;
    let words = padded_length / 4;
    let words = u8::try_from(words).map_err(|_| EapAkaError::InvalidAttribute)?;
    output.extend_from_slice(&[kind, words]);
    output.extend_from_slice(value);
    output.resize(output.len() + padded_length - raw_length, 0);
    Ok(())
}

fn fips186_2_prf(key: &[u8], output_length: usize) -> Zeroizing<Vec<u8>> {
    let mut xkey = Zeroizing::new([0_u8; 20]);
    if key.len() >= 20 {
        xkey.copy_from_slice(&key[key.len() - 20..]);
    } else {
        xkey[20 - key.len()..].copy_from_slice(key);
    }
    let mut output = Zeroizing::new(Vec::with_capacity(output_length));
    while output.len() < output_length {
        let mut state = Zeroizing::new(Vec::with_capacity(40));
        let mut xseed = Zeroizing::new([0_u8; 20]);
        for _ in 0..2 {
            let mut xval = add_mod_160(&xkey, &xseed);
            let mut block = Block::<Sha1Core>::default();
            block[..20].copy_from_slice(&xval);
            let mut sha_state = [
                0x6745_2301,
                0xefcd_ab89,
                0x98ba_dcfe,
                0x1032_5476,
                0xc3d2_e1f0,
            ];
            compress(&mut sha_state, &[block]);
            let mut digest: [u8; 20] = sha_state
                .iter()
                .flat_map(|word| word.to_be_bytes())
                .collect::<Vec<_>>()
                .try_into()
                .expect("SHA-1 state has fixed length");
            state.extend_from_slice(&digest);
            *xkey = add_mod_160(
                &add_mod_160(&xkey, &digest),
                &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            );
            xseed.fill(0);
            xval.zeroize();
            digest.zeroize();
            sha_state.zeroize();
            block.fill(0);
        }
        let take = (output_length - output.len()).min(state.len());
        output.extend_from_slice(&state[..take]);
    }
    xkey.fill(0);
    output
}

fn add_mod_160(left: &[u8; 20], right: &[u8; 20]) -> [u8; 20] {
    let mut output = [0_u8; 20];
    let mut carry = 0_u16;
    for index in (0..20).rev() {
        let sum = u16::from(left[index]) + u16::from(right[index]) + carry;
        output[index] = sum as u8;
        carry = sum >> 8;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Sha256;

    #[test]
    fn fips186_2_sha1_prf_matches_go_golden_digest() {
        let key = b"eap-aka fixed MK vector";
        let output = fips186_2_prf(key, 96);
        assert_eq!(
            format!("{:x}", Sha256::digest(&output)),
            "c1535ef2c0d408d7ee6b70598eaf36502a904c04672682eab956900ff09a0d7c"
        );
    }

    #[test]
    fn eap_aka_challenge_checks_request_mac_and_builds_result_indication() {
        let identity = b"123456789012345@wlan.mnc001.mcc001.3gppnetwork.org";
        let res = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let ck = [0x31; 16];
        let ik = [0x42; 16];
        let mut mk_input = Vec::from(identity.as_slice());
        mk_input.extend_from_slice(&ik);
        mk_input.extend_from_slice(&ck);
        let mk = Sha1::digest(&mk_input);
        let keymat = fips186_2_prf(&mk, 96);
        let k_aut = &keymat[16..32];

        let transcript = b"eap identity request and response";
        let checkcode = Sha1::digest(transcript);
        let mut attrs = Vec::new();
        let rand_attr = [vec![0, 0], vec![0; 16]].concat();
        let autn_attr = [vec![0, 0], vec![1; 16]].concat();
        encode_attribute(1, &rand_attr, &mut attrs).unwrap();
        encode_attribute(2, &autn_attr, &mut attrs).unwrap();
        encode_attribute(135, &[0, 0], &mut attrs).unwrap();
        encode_attribute(23, &checkcode, &mut attrs).unwrap();
        encode_attribute(11, &[0; 18], &mut attrs).unwrap();
        let length = 8 + attrs.len();
        let mut request = vec![1, 9];
        request.extend_from_slice(&(length as u16).to_be_bytes());
        request.extend_from_slice(&[23, 1, 0, 0]);
        request.extend_from_slice(&attrs);
        let mac_start = request.len() - 16;
        let mut hmac = HmacSha1::new_from_slice(k_aut).unwrap();
        hmac.update(&request);
        request[mac_start..].copy_from_slice(&hmac.finalize().into_bytes()[..16]);
        assert!(parse_request(&request).is_ok());

        let response = answer_eap_aka_challenge(&request, identity, &res, &ck, &ik).unwrap();
        assert_eq!(
            &response.packet[..8],
            &[2, 9, 0, response.packet.len() as u8, 23, 1, 0, 0]
        );
        assert_eq!(response.msk.len(), 64);
        assert_eq!(&*response.k_aut, k_aut);
        let response_mac_start = response.packet.len() - 16;
        let mut expected = response.packet.clone();
        expected[response_mac_start..].fill(0);
        let mut hmac = HmacSha1::new_from_slice(k_aut).unwrap();
        hmac.update(&expected);
        assert_eq!(
            &response.packet[response_mac_start..],
            &hmac.finalize().into_bytes()[..16]
        );
        assert!(!response.packet.windows(2).any(|header| header == [23, 6]));

        let checkcode_response = answer_eap_aka_challenge_with_checkcode(
            &request,
            identity,
            &res,
            &ck,
            &ik,
            transcript,
            AkaCheckcodeMode::Checkcode,
        )
        .unwrap();
        assert!(
            checkcode_response
                .packet
                .windows(2)
                .any(|header| header == [23, 6])
        );
        assert_eq!(
            answer_eap_aka_challenge_with_checkcode(
                &request,
                identity,
                &res,
                &ck,
                &ik,
                &[],
                AkaCheckcodeMode::Checkcode,
            )
            .unwrap_err(),
            EapAkaError::MissingIdentityTranscript
        );

        let mut bad_request = request;
        *bad_request.last_mut().unwrap() ^= 1;
        assert_eq!(
            answer_eap_aka_challenge(&bad_request, identity, &res, &ck, &ik).unwrap_err(),
            EapAkaError::MacMismatch
        );
    }

    #[test]
    fn challenge_parser_requires_rand_autn_and_mac_before_sim_io() {
        let mut attributes = Vec::new();
        let rand = [vec![0, 0], vec![0x11; 16]].concat();
        let autn = [vec![0, 0], vec![0x22; 16]].concat();
        encode_attribute(1, &rand, &mut attributes).unwrap();
        encode_attribute(2, &autn, &mut attributes).unwrap();
        encode_attribute(11, &[0; 18], &mut attributes).unwrap();
        let length = 8 + attributes.len();
        let mut request = vec![1, 41];
        request.extend_from_slice(&(length as u16).to_be_bytes());
        request.extend_from_slice(&[23, 1, 0, 0]);
        request.extend_from_slice(&attributes);
        let parsed = parse_eap_aka_challenge(&request).unwrap();
        assert_eq!(parsed.identifier, 41);
        assert_eq!(parsed.rand, [0x11; 16]);
        assert_eq!(parsed.autn, [0x22; 16]);
        assert!(!format!("{parsed:?}").contains("111111"));

        let mut missing_mac = Vec::new();
        encode_attribute(1, &rand, &mut missing_mac).unwrap();
        encode_attribute(2, &autn, &mut missing_mac).unwrap();
        let length = 8 + missing_mac.len();
        let mut request = vec![1, 42];
        request.extend_from_slice(&(length as u16).to_be_bytes());
        request.extend_from_slice(&[23, 1, 0, 0]);
        request.extend_from_slice(&missing_mac);
        assert_eq!(
            parse_eap_aka_challenge(&request).unwrap_err(),
            EapAkaError::MissingAttribute(11)
        );

        let mut truncated_mac = Vec::new();
        encode_attribute(1, &rand, &mut truncated_mac).unwrap();
        encode_attribute(2, &autn, &mut truncated_mac).unwrap();
        encode_attribute(11, &[0; 4], &mut truncated_mac).unwrap();
        let length = 8 + truncated_mac.len();
        let mut request = vec![1, 43];
        request.extend_from_slice(&(length as u16).to_be_bytes());
        request.extend_from_slice(&[23, 1, 0, 0]);
        request.extend_from_slice(&truncated_mac);
        assert_eq!(
            parse_eap_aka_challenge(&request).unwrap_err(),
            EapAkaError::InvalidChallenge
        );
    }

    #[test]
    fn synchronization_failure_emits_go_compatible_aka_auts_frame() {
        let packet = build_eap_aka_sync_failure(9, &[0x5a; 14]).unwrap();
        assert_eq!(&packet[..8], &[2, 9, 0, 24, 23, 4, 0, 0]);
        assert_eq!(&packet[8..10], &[4, 4]);
        assert_eq!(&packet[10..], &[0x5a; 14]);
        assert_eq!(
            build_eap_aka_sync_failure(9, &[0; 13]).unwrap_err(),
            EapAkaError::InvalidAutsLength
        );
    }
}
