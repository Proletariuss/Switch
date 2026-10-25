use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use proletarius_ike::{
    DhError, ExchangeType, IkeHeader, IkeProtocolError, IkeSaKeyMaterial, KeyDerivationError,
    Modp1024KeyPair, Modp2048KeyPair, NatDetectionKind, PayloadChainError, PayloadFrame,
    ResponseDisposition, SaDecodeError, SaProposal, SaTransform, SessionTracker,
    decode_payload_chain, decode_sa_proposals, derive_ike_sa_key_material, encode_payload_chain,
    nat_detection_notify,
};
use tokio::time::timeout;

use crate::{
    DatagramTransportError, EpdgDatagramTransport, EpdgPacket, transport::NatDetectionEvidence,
};

#[derive(Debug)]
pub enum SaInitError {
    InvalidAttempts,
    Timeout,
    UnexpectedPacket,
    Protocol(IkeProtocolError),
    Payload(PayloadChainError),
    MissingMandatoryPayload,
    InvalidKeyExchange,
    UnexpectedDhGroup { expected: u16, actual: u16 },
    Proposal(SaDecodeError),
    UnofferedProposal,
    InvalidNotify,
    CookieRequired(Vec<u8>),
    InvalidCookie,
    ProposalRejected,
    Dh(DhError),
    KeyDerivation(KeyDerivationError),
    UnsupportedEncryption(u16),
    UnsupportedIntegrity(u16),
    InvalidKeyLengthAttribute,
    Transport(DatagramTransportError),
}

impl fmt::Display for SaInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAttempts => formatter.write_str("SA_INIT needs at least one attempt"),
            Self::Timeout => formatter.write_str("SA_INIT timed out after all attempts"),
            Self::UnexpectedPacket => formatter.write_str("SA_INIT received non-IKE traffic"),
            Self::Protocol(error) => error.fmt(formatter),
            Self::Payload(error) => error.fmt(formatter),
            Self::MissingMandatoryPayload => {
                formatter.write_str("SA_INIT response lacks SA, KE or Nonce")
            }
            Self::InvalidKeyExchange => formatter.write_str("SA_INIT KE payload is malformed"),
            Self::UnexpectedDhGroup { expected, actual } => {
                write!(
                    formatter,
                    "SA_INIT DH group {actual} differs from offered group {expected}"
                )
            }
            Self::Proposal(error) => error.fmt(formatter),
            Self::UnofferedProposal => {
                formatter.write_str("ePDG selected an unoffered IKE proposal")
            }
            Self::InvalidNotify => formatter.write_str("SA_INIT notification is truncated"),
            Self::CookieRequired(_) => formatter.write_str("ePDG requires an IKE COOKIE retry"),
            Self::InvalidCookie => formatter.write_str("IKE COOKIE must contain 1 to 64 bytes"),
            Self::ProposalRejected => formatter.write_str("ePDG rejected the IKE proposal"),
            Self::Dh(error) => error.fmt(formatter),
            Self::KeyDerivation(error) => error.fmt(formatter),
            Self::UnsupportedEncryption(id) => {
                write!(formatter, "unsupported IKE encryption transform {id}")
            }
            Self::UnsupportedIntegrity(id) => {
                write!(formatter, "unsupported IKE integrity transform {id}")
            }
            Self::InvalidKeyLengthAttribute => {
                formatter.write_str("IKE encryption transform has invalid key length")
            }
            Self::Transport(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SaInitError {}

impl From<IkeProtocolError> for SaInitError {
    fn from(value: IkeProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl From<DatagramTransportError> for SaInitError {
    fn from(value: DatagramTransportError) -> Self {
        Self::Transport(value)
    }
}

impl From<PayloadChainError> for SaInitError {
    fn from(value: PayloadChainError) -> Self {
        Self::Payload(value)
    }
}

impl From<SaDecodeError> for SaInitError {
    fn from(value: SaDecodeError) -> Self {
        Self::Proposal(value)
    }
}

impl From<DhError> for SaInitError {
    fn from(value: DhError) -> Self {
        Self::Dh(value)
    }
}

impl From<KeyDerivationError> for SaInitError {
    fn from(value: KeyDerivationError) -> Self {
        Self::KeyDerivation(value)
    }
}

fn match_selected_proposal(
    offered: &[u8],
    selected: &[u8],
    ke_group: u16,
) -> Result<SaProposal, SaInitError> {
    let offers = decode_sa_proposals(offered)?;
    let mut choices = decode_sa_proposals(selected)?;
    if choices.len() != 1 {
        return Err(SaInitError::UnofferedProposal);
    }
    let choice = choices.remove(0);
    if choice.protocol != 1 || !choice.spi.is_empty() || choice.transforms.is_empty() {
        return Err(SaInitError::UnofferedProposal);
    }
    let offer = offers
        .iter()
        .find(|offer| offer.number == choice.number && offer.protocol == choice.protocol)
        .ok_or(SaInitError::UnofferedProposal)?;
    let mut seen = [false; 5];
    for selected_transform in &choice.transforms {
        let kind = usize::from(selected_transform.kind);
        if kind == 0 || kind >= seen.len() || seen[kind] {
            return Err(SaInitError::UnofferedProposal);
        }
        seen[kind] = true;
        if !offer.transforms.contains(selected_transform) {
            return Err(SaInitError::UnofferedProposal);
        }
    }
    if !seen[1]
        || !seen[2]
        || !seen[4]
        || choice
            .transforms
            .iter()
            .find(|item| item.kind == 4)
            .map(|item| item.id)
            != Some(ke_group)
    {
        return Err(SaInitError::UnofferedProposal);
    }
    let encryption = choice
        .transforms
        .iter()
        .find(|item| item.kind == 1)
        .ok_or(SaInitError::UnofferedProposal)?;
    let aead = matches!(encryption.id, 14..=16 | 18..=20);
    if seen[3] == aead {
        return Err(SaInitError::UnofferedProposal);
    }
    Ok(choice)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaInitResponse {
    pub request_packet: Vec<u8>,
    pub raw_packet: Vec<u8>,
    pub responder_spi: u64,
    pub selected_sa: Vec<u8>,
    pub selected_proposal: Option<SaProposal>,
    pub dh_group: u16,
    pub peer_public_key: Vec<u8>,
    pub responder_nonce: Vec<u8>,
}

#[derive(Debug)]
pub struct SaInitKeySchedule {
    pub initiator_nonce: Vec<u8>,
    pub prf_id: u16,
    pub initiator_spi: u64,
    pub encryption_transform: u16,
    pub integrity_transform: Option<u16>,
    pub response: SaInitResponse,
    pub keys: IkeSaKeyMaterial,
}

fn selected_transform(proposal: &SaProposal, kind: u8) -> Result<&SaTransform, SaInitError> {
    let mut matches = proposal.transforms.iter().filter(|item| item.kind == kind);
    let transform = matches.next().ok_or(SaInitError::UnofferedProposal)?;
    if matches.next().is_some() {
        return Err(SaInitError::UnofferedProposal);
    }
    Ok(transform)
}

fn encryption_key_lengths(proposal: &SaProposal) -> Result<(usize, usize), SaInitError> {
    let encryption = selected_transform(proposal, 1)?;
    let key_bits = encryption
        .attributes
        .iter()
        .filter(|attribute| attribute.kind == 14)
        .try_fold(None, |value, attribute| {
            if value.is_some() || attribute.value.len() != 2 {
                return Err(SaInitError::InvalidKeyLengthAttribute);
            }
            Ok(Some(u16::from_be_bytes([
                attribute.value[0],
                attribute.value[1],
            ])))
        })?
        .ok_or(SaInitError::InvalidKeyLengthAttribute)?;
    if !matches!(key_bits, 128 | 192 | 256) {
        return Err(SaInitError::InvalidKeyLengthAttribute);
    }
    let cipher_key_bytes = usize::from(key_bits / 8);
    let (encryption_key_bytes, aead) = match encryption.id {
        12 => (cipher_key_bytes, false),
        18..=20 => (cipher_key_bytes + 4, true),
        other => return Err(SaInitError::UnsupportedEncryption(other)),
    };
    let integrity_key_bytes = if aead {
        0
    } else {
        match selected_transform(proposal, 3)?.id {
            2 => 20,
            12 => 32,
            13 => 48,
            14 => 64,
            other => return Err(SaInitError::UnsupportedIntegrity(other)),
        }
    };
    Ok((encryption_key_bytes, integrity_key_bytes))
}

pub(crate) enum ModpKeyPair {
    Group2(Modp1024KeyPair),
    Group14(Modp2048KeyPair),
}

impl ModpKeyPair {
    pub(crate) fn generate(group: u16) -> Result<Self, SaInitError> {
        match group {
            2 => Ok(Self::Group2(Modp1024KeyPair::generate()?)),
            14 => Ok(Self::Group14(Modp2048KeyPair::generate()?)),
            other => Err(SaInitError::UnexpectedDhGroup {
                expected: 2,
                actual: other,
            }),
        }
    }

    pub(crate) fn public_key(&self) -> &[u8] {
        match self {
            Self::Group2(key) => key.public_key(),
            Self::Group14(key) => key.public_key(),
        }
    }

    pub(crate) fn shared_secret(
        &self,
        peer: &[u8],
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, SaInitError> {
        match self {
            Self::Group2(key) => Ok(key.shared_secret(peer)?),
            Self::Group14(key) => Ok(key.shared_secret(peer)?),
        }
    }

    pub(crate) fn group(&self) -> u16 {
        match self {
            Self::Group2(_) => 2,
            Self::Group14(_) => 14,
        }
    }
}

/// Executes the explicitly configured MODP group for IKE_SA_INIT and derives
/// IKE SA keys. Group 2 is accepted only when the exact proposal requests it;
/// no fallback or retry downgrades group 14.
pub async fn perform_sa_init_configured(
    transport: &EpdgDatagramTransport,
    initiator_spi: u64,
    frames: &[PayloadFrame],
    attempts: u8,
    attempt_timeout: Duration,
) -> Result<SaInitKeySchedule, SaInitError> {
    perform_sa_init_configured_with_cookie(
        transport,
        initiator_spi,
        frames,
        attempts,
        attempt_timeout,
        false,
    )
    .await
}
pub async fn perform_sa_init_configured_with_cookie(
    transport: &EpdgDatagramTransport,
    initiator_spi: u64,
    frames: &[PayloadFrame],
    attempts: u8,
    attempt_timeout: Duration,
    cookie_as_sa: bool,
) -> Result<SaInitKeySchedule, SaInitError> {
    if frames.iter().filter(|frame| frame.kind == 33).count() != 1
        || frames.iter().filter(|frame| frame.kind == 40).count() != 1
        || frames.iter().filter(|frame| frame.kind == 34).count() > 1
    {
        return Err(SaInitError::MissingMandatoryPayload);
    }
    let mut request_frames = frames.to_vec();
    let configured_ke = request_frames
        .iter()
        .find(|frame| frame.kind == 34)
        .ok_or(SaInitError::MissingMandatoryPayload)?;
    let (group, _) = ke_parts(&configured_ke.body)?;
    let dh = ModpKeyPair::generate(group)?;
    let mut ke_body = Vec::with_capacity(4 + dh.public_key().len());
    ke_body.extend_from_slice(&dh.group().to_be_bytes());
    ke_body.extend_from_slice(&[0, 0]);
    ke_body.extend_from_slice(dh.public_key());
    request_frames.retain(|frame| frame.kind != 34);
    let sa_position = request_frames
        .iter()
        .position(|frame| frame.kind == 33)
        .ok_or(SaInitError::MissingMandatoryPayload)?;
    request_frames.insert(
        sa_position + 1,
        PayloadFrame {
            kind: 34,
            critical: false,
            body: ke_body,
        },
    );

    let initiator_nonce = request_frames
        .iter()
        .find(|frame| frame.kind == 40)
        .ok_or(SaInitError::MissingMandatoryPayload)?
        .body
        .clone();
    insert_nat_detection_frames(
        &mut request_frames,
        initiator_spi,
        transport.nat_detection_evidence()?,
    )?;
    let response = perform_sa_init_payloads_with_cookie(
        transport,
        initiator_spi,
        &request_frames,
        attempts,
        attempt_timeout,
        cookie_as_sa,
    )
    .await?;
    let proposal = response
        .selected_proposal
        .as_ref()
        .ok_or(SaInitError::UnofferedProposal)?;
    let encryption_transform = selected_transform(proposal, 1)?.id;
    let integrity_transform = proposal
        .transforms
        .iter()
        .find(|transform| transform.kind == 3)
        .map(|transform| transform.id);
    let prf_id = selected_transform(proposal, 2)?.id;
    let (encryption_key_len, integrity_key_len) = encryption_key_lengths(proposal)?;
    let shared_secret = dh.shared_secret(&response.peer_public_key)?;
    let keys = derive_ike_sa_key_material(
        prf_id,
        &initiator_nonce,
        &response.responder_nonce,
        &shared_secret,
        initiator_spi,
        response.responder_spi,
        encryption_key_len,
        integrity_key_len,
    )?;
    Ok(SaInitKeySchedule {
        initiator_nonce,
        prf_id,
        initiator_spi,
        encryption_transform,
        integrity_transform,
        response,
        keys,
    })
}

fn insert_nat_detection_frames(
    frames: &mut Vec<PayloadFrame>,
    initiator_spi: u64,
    evidence: NatDetectionEvidence,
) -> Result<(), SaInitError> {
    let (source, destination) = match evidence {
        NatDetectionEvidence::Exact {
            source,
            destination,
        } => (source, destination),
        NatDetectionEvidence::ForceNat => {
            // The SOCKS relay owns the real outer tuple and keeps the ePDG
            // numeric address private. Port zero cannot be the UDP/500 source
            // or destination observed by the peer, so these two deterministic
            // hashes force NAT traversal without a local DNS lookup or a
            // direct socket. Distinct address families avoid duplicate hashes.
            (
                SocketAddr::from(([0, 0, 0, 0], 0)),
                SocketAddr::from(([0_u16; 8], 0)),
            )
        }
    };
    let nonce = frames
        .iter()
        .position(|frame| frame.kind == 40)
        .ok_or(SaInitError::MissingMandatoryPayload)?;
    frames.splice(
        nonce + 1..nonce + 1,
        [
            nat_detection_notify(NatDetectionKind::Source, initiator_spi, 0, source),
            nat_detection_notify(NatDetectionKind::Destination, initiator_spi, 0, destination),
        ],
    );
    Ok(())
}

/// Compatibility wrapper which preserves the legacy group-14-only contract.
pub async fn perform_sa_init_group14(
    transport: &EpdgDatagramTransport,
    initiator_spi: u64,
    frames: &[PayloadFrame],
    attempts: u8,
    attempt_timeout: Duration,
) -> Result<SaInitKeySchedule, SaInitError> {
    let mut legacy_frames = frames.to_vec();
    if let Some(offered_ke) = legacy_frames.iter().find(|frame| frame.kind == 34) {
        let (group, _) = ke_parts(&offered_ke.body)?;
        if group != 14 {
            return Err(SaInitError::UnexpectedDhGroup {
                expected: 14,
                actual: group,
            });
        }
    } else {
        let position = legacy_frames
            .iter()
            .position(|frame| frame.kind == 33)
            .ok_or(SaInitError::MissingMandatoryPayload)?;
        legacy_frames.insert(
            position + 1,
            PayloadFrame {
                kind: 34,
                critical: false,
                body: vec![0, 14, 0, 0, 0],
            },
        );
    }
    perform_sa_init_configured(
        transport,
        initiator_spi,
        &legacy_frames,
        attempts,
        attempt_timeout,
    )
    .await
}

fn ke_parts(body: &[u8]) -> Result<(u16, &[u8]), SaInitError> {
    if body.len() <= 4 || body[2..4] != [0, 0] || body[..2] == [0, 0] {
        return Err(SaInitError::InvalidKeyExchange);
    }
    Ok((u16::from_be_bytes([body[0], body[1]]), &body[4..]))
}

fn validate_sa_init_response(packet: &[u8]) -> Result<SaInitResponse, SaInitError> {
    let (header, body) = IkeHeader::decode(packet)?;
    let frames = decode_payload_chain(header.next_payload, body)?;
    let mut sa = None;
    let mut ke = None;
    let mut nonce = None;
    for frame in frames {
        match frame.kind {
            33 => sa = Some(frame.body),
            34 => {
                let (group, public_key) = ke_parts(&frame.body)?;
                ke = Some((group, public_key.to_vec()));
            }
            40 => nonce = Some(frame.body),
            41 => {
                if frame.body.len() < 4 {
                    return Err(SaInitError::InvalidNotify);
                }
                let spi_length = usize::from(frame.body[1]);
                if frame.body.len() < 4 + spi_length {
                    return Err(SaInitError::InvalidNotify);
                }
                let kind = u16::from_be_bytes([frame.body[2], frame.body[3]]);
                match kind {
                    14 => return Err(SaInitError::ProposalRejected),
                    16390 => {
                        let cookie = frame.body[4 + spi_length..].to_vec();
                        if !(1..=64).contains(&cookie.len()) {
                            return Err(SaInitError::InvalidCookie);
                        }
                        return Err(SaInitError::CookieRequired(cookie));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    let (Some(selected_sa), Some((dh_group, peer_public_key)), Some(responder_nonce)) =
        (sa, ke, nonce)
    else {
        return Err(SaInitError::MissingMandatoryPayload);
    };
    if selected_sa.is_empty() || responder_nonce.is_empty() {
        return Err(SaInitError::MissingMandatoryPayload);
    }
    Ok(SaInitResponse {
        raw_packet: packet.to_vec(),
        request_packet: Vec::new(),
        responder_spi: header.responder_spi,
        selected_sa,
        selected_proposal: None,
        dh_group,
        peer_public_key,
        responder_nonce,
    })
}

pub async fn perform_sa_init_payloads(
    transport: &EpdgDatagramTransport,
    initiator_spi: u64,
    frames: &[PayloadFrame],
    attempts: u8,
    attempt_timeout: Duration,
) -> Result<SaInitResponse, SaInitError> {
    perform_sa_init_payloads_with_cookie(
        transport,
        initiator_spi,
        frames,
        attempts,
        attempt_timeout,
        false,
    )
    .await
}
pub async fn perform_sa_init_payloads_with_cookie(
    transport: &EpdgDatagramTransport,
    initiator_spi: u64,
    frames: &[PayloadFrame],
    attempts: u8,
    attempt_timeout: Duration,
    cookie_as_sa: bool,
) -> Result<SaInitResponse, SaInitError> {
    let offered_ke = frames
        .iter()
        .find(|frame| frame.kind == 34)
        .ok_or(SaInitError::MissingMandatoryPayload)?;
    let (offered_group, _) = ke_parts(&offered_ke.body)?;
    let (next_payload, payload) = encode_payload_chain(frames)?;
    let response = perform_sa_init_exchange(
        transport,
        initiator_spi,
        next_payload,
        &payload,
        attempts,
        attempt_timeout,
    )
    .await;
    let (request, response) = match response {
        Err(SaInitError::CookieRequired(cookie)) => {
            let mut retry_frames = Vec::with_capacity(frames.len() + 1);
            let mut notify = vec![0, 0, 0x40, 0x06];
            notify.extend_from_slice(&cookie);
            retry_frames.push(PayloadFrame {
                kind: if cookie_as_sa { 33 } else { 41 },
                critical: false,
                body: notify,
            });
            retry_frames.extend_from_slice(frames);
            let (first, bytes) = encode_payload_chain(&retry_frames)?;
            perform_sa_init_exchange(
                transport,
                initiator_spi,
                first,
                &bytes,
                attempts,
                attempt_timeout,
            )
            .await?
        }
        other => other?,
    };
    let mut parsed = validate_sa_init_response(&response)?;
    parsed.request_packet = request;
    if parsed.dh_group != offered_group {
        return Err(SaInitError::UnexpectedDhGroup {
            expected: offered_group,
            actual: parsed.dh_group,
        });
    }
    let offered_sa = frames
        .iter()
        .find(|frame| frame.kind == 33)
        .ok_or(SaInitError::MissingMandatoryPayload)?;
    parsed.selected_proposal = Some(match_selected_proposal(
        &offered_sa.body,
        &parsed.selected_sa,
        parsed.dh_group,
    )?);
    Ok(parsed)
}

/// Runs only the first IKEv2 exchange. Crypto payload construction and later
/// EAP-AKA/Child-SA exchanges remain outside this transport-level primitive.
/// Each retry sends exactly the same IKE bytes and uses an independent deadline.
pub async fn perform_sa_init(
    transport: &EpdgDatagramTransport,
    initiator_spi: u64,
    next_payload: u8,
    payload: &[u8],
    attempts: u8,
    attempt_timeout: Duration,
) -> Result<Vec<u8>, SaInitError> {
    perform_sa_init_exchange(
        transport,
        initiator_spi,
        next_payload,
        payload,
        attempts,
        attempt_timeout,
    )
    .await
    .map(|(_, response)| response)
}

async fn perform_sa_init_exchange(
    transport: &EpdgDatagramTransport,
    initiator_spi: u64,
    next_payload: u8,
    payload: &[u8],
    attempts: u8,
    attempt_timeout: Duration,
) -> Result<(Vec<u8>, Vec<u8>), SaInitError> {
    if attempts == 0 {
        return Err(SaInitError::InvalidAttempts);
    }
    let mut tracker = SessionTracker::new(initiator_spi)?;
    let message_id = tracker.begin_request(ExchangeType::IkeSaInit)?;
    let request = IkeHeader {
        initiator_spi,
        responder_spi: 0,
        next_payload,
        version: 0x20,
        exchange: ExchangeType::IkeSaInit,
        flags: IkeHeader::INITIATOR_FLAG,
        message_id,
    }
    .encode(payload)?;

    for _ in 0..attempts {
        transport.send_ike(&request).await?;
        let response = timeout(attempt_timeout, async {
            loop {
                match transport.receive_packet().await? {
                    EpdgPacket::Ike(packet) => {
                        let (header, _) = IkeHeader::decode(&packet)?;
                        if header.responder_spi == 0 {
                            if header.initiator_spi != initiator_spi {
                                return Err(SaInitError::Protocol(
                                    IkeProtocolError::InitiatorSpiChanged,
                                ));
                            }
                            if !header.is_response()
                                || header.flags & IkeHeader::INITIATOR_FLAG != 0
                                || header.exchange != ExchangeType::IkeSaInit
                                || header.message_id != message_id
                            {
                                return Err(SaInitError::Protocol(
                                    IkeProtocolError::UnexpectedResponse,
                                ));
                            }
                            return match validate_sa_init_response(&packet) {
                                Err(error) => Err(error),
                                Ok(_) => Err(SaInitError::Protocol(
                                    IkeProtocolError::ResponderSpiMissing,
                                )),
                            };
                        }
                        match tracker.accept_response(&header)? {
                            ResponseDisposition::Accepted => return Ok(packet),
                            ResponseDisposition::Duplicate => continue,
                        }
                    }
                    EpdgPacket::Keepalive | EpdgPacket::Esp(_) => {
                        return Err(SaInitError::UnexpectedPacket);
                    }
                }
            }
        })
        .await;
        match response {
            Ok(result) => return result.map(|response| (request, response)),
            Err(_) => continue,
        }
    }
    Err(SaInitError::Timeout)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use proletarius_ike::{NatDetectionKind, nat_detection_notify};
    use tokio::net::UdpSocket;

    use super::*;
    use crate::ProxyMode;

    fn test_sa() -> Vec<u8> {
        vec![
            0, 0, 0, 36, 1, 1, 0, 3, 3, 0, 0, 12, 1, 0, 0, 20, 0x80, 14, 0, 128, 3, 0, 0, 8, 2, 0,
            0, 5, 0, 0, 0, 8, 4, 0, 0, 14,
        ]
    }

    fn response_with(frames: &[PayloadFrame]) -> Vec<u8> {
        let (first, body) = encode_payload_chain(frames).unwrap();
        IkeHeader {
            initiator_spi: 7,
            responder_spi: 9,
            next_payload: first,
            version: 0x20,
            exchange: ExchangeType::IkeSaInit,
            flags: IkeHeader::RESPONSE_FLAG,
            message_id: 0,
        }
        .encode(&body)
        .unwrap()
    }

    #[test]
    fn sa_init_response_requires_mandatory_payloads_and_exposes_operator_errors() {
        assert!(matches!(
            validate_sa_init_response(&response_with(&[])),
            Err(SaInitError::MissingMandatoryPayload)
        ));
        let cookie = response_with(&[PayloadFrame {
            kind: 41,
            critical: false,
            body: vec![0, 0, 0x40, 0x06, 0xaa, 0xbb],
        }]);
        assert!(matches!(
            validate_sa_init_response(&cookie),
            Err(SaInitError::CookieRequired(bytes)) if bytes == [0xaa, 0xbb]
        ));
        let rejected = response_with(&[PayloadFrame {
            kind: 41,
            critical: false,
            body: vec![0, 0, 0, 14],
        }]);
        assert!(matches!(
            validate_sa_init_response(&rejected),
            Err(SaInitError::ProposalRejected)
        ));
        assert!(matches!(
            ke_parts(&[0, 14, 1, 0, 3]),
            Err(SaInitError::InvalidKeyExchange)
        ));
        let offered = test_sa();
        let mut different = offered.clone();
        different[15] = 19;
        assert!(matches!(
            match_selected_proposal(&offered, &different, 14),
            Err(SaInitError::UnofferedProposal)
        ));
        let mut malformed = offered.clone();
        malformed[10..12].copy_from_slice(&0_u16.to_be_bytes());
        assert!(matches!(
            match_selected_proposal(&offered, &malformed, 14),
            Err(SaInitError::Proposal(SaDecodeError::InvalidLength))
        ));
    }

    #[test]
    fn aes_cbc_key_schedule_selects_integrity_key_size_from_negotiated_transform() {
        let mut proposal = decode_sa_proposals(&test_sa()).unwrap().remove(0);
        proposal.transforms[0].id = 12;
        proposal.transforms.insert(
            2,
            SaTransform {
                kind: 3,
                id: 12,
                attributes: Vec::new(),
            },
        );
        assert_eq!(encryption_key_lengths(&proposal).unwrap(), (16, 32));
    }

    #[tokio::test]
    async fn sa_init_rejects_unoffered_dh_group() {
        let epdg = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = epdg.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let mut buffer = [0; 256];
            let (_, source) = epdg.recv_from(&mut buffer).await.unwrap();
            let response = response_with(&[
                PayloadFrame {
                    kind: 33,
                    critical: false,
                    body: test_sa(),
                },
                PayloadFrame {
                    kind: 34,
                    critical: false,
                    body: vec![0, 15, 0, 0, 3],
                },
                PayloadFrame {
                    kind: 40,
                    critical: false,
                    body: vec![7; 16],
                },
            ]);
            epdg.send_to(&response, source).await.unwrap();
        });
        let transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "unused.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap();
        let request = [
            PayloadFrame {
                kind: 33,
                critical: false,
                body: test_sa(),
            },
            PayloadFrame {
                kind: 34,
                critical: false,
                body: vec![0, 14, 0, 0, 3],
            },
            PayloadFrame {
                kind: 40,
                critical: false,
                body: vec![7; 16],
            },
        ];
        assert!(matches!(
            perform_sa_init_payloads(&transport, 7, &request, 1, Duration::from_millis(100)).await,
            Err(SaInitError::UnexpectedDhGroup {
                expected: 14,
                actual: 15
            })
        ));
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn sa_init_accepts_matching_response_and_preserves_request_bytes() {
        let epdg = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = epdg.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let mut buffer = [0; 256];
            let (length, source) = epdg.recv_from(&mut buffer).await.unwrap();
            let (request, payload) = IkeHeader::decode(&buffer[..length]).unwrap();
            assert_eq!(request.initiator_spi, 7);
            assert_eq!(request.message_id, 0);
            assert_eq!(request.next_payload, 33);
            assert_eq!(
                decode_payload_chain(request.next_payload, payload).unwrap(),
                [
                    PayloadFrame {
                        kind: 33,
                        critical: false,
                        body: test_sa()
                    },
                    PayloadFrame {
                        kind: 34,
                        critical: false,
                        body: vec![0, 14, 0, 0, 3]
                    },
                ]
            );
            let (first, body) = encode_payload_chain(&[
                PayloadFrame {
                    kind: 33,
                    critical: false,
                    body: test_sa(),
                },
                PayloadFrame {
                    kind: 34,
                    critical: false,
                    body: vec![0, 14, 0, 0, 1],
                },
                PayloadFrame {
                    kind: 40,
                    critical: false,
                    body: vec![7; 16],
                },
            ])
            .unwrap();
            let reply = IkeHeader {
                initiator_spi: 7,
                responder_spi: 9,
                next_payload: first,
                version: 0x20,
                exchange: ExchangeType::IkeSaInit,
                flags: IkeHeader::RESPONSE_FLAG,
                message_id: 0,
            }
            .encode(&body)
            .unwrap();
            epdg.send_to(&reply, source).await.unwrap();
            reply
        });
        let transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "unused.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap();
        let response = perform_sa_init_payloads(
            &transport,
            7,
            &[
                PayloadFrame {
                    kind: 33,
                    critical: false,
                    body: test_sa(),
                },
                PayloadFrame {
                    kind: 34,
                    critical: false,
                    body: vec![0, 14, 0, 0, 3],
                },
            ],
            2,
            Duration::from_millis(100),
        )
        .await
        .unwrap();
        assert_eq!(response.raw_packet, peer.await.unwrap());
        assert_eq!(response.dh_group, 14);
        assert_eq!(response.peer_public_key, [1]);
        assert_eq!(response.responder_nonce, [7; 16]);
    }

    #[tokio::test]
    async fn group14_sa_init_builds_ke_and_derives_the_selected_key_schedule() {
        let epdg = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = epdg.local_addr().unwrap();
        let responder_dh = Modp2048KeyPair::generate().unwrap();
        let responder_public = responder_dh.public_key().to_vec();
        let responder = tokio::spawn(async move {
            let mut buffer = [0; 2048];
            let (length, source) = epdg.recv_from(&mut buffer).await.unwrap();
            let (header, payload) = IkeHeader::decode(&buffer[..length]).unwrap();
            let first_request = decode_payload_chain(header.next_payload, payload).unwrap();
            assert_eq!(
                first_request
                    .iter()
                    .map(|frame| frame.kind)
                    .collect::<Vec<_>>(),
                [33, 34, 40, 41, 41]
            );
            assert_eq!(
                first_request[3],
                nat_detection_notify(NatDetectionKind::Source, 7, 0, source)
            );
            assert_eq!(
                first_request[4],
                nat_detection_notify(NatDetectionKind::Destination, 7, 0, address)
            );
            let original_ke = first_request[1].body.clone();
            let (group, public_key) = ke_parts(&original_ke).unwrap();
            assert_eq!(group, 14);
            assert_eq!(public_key.len(), 256);
            assert_eq!(first_request[2].body, [4; 32]);

            let cookie_packet = response_with(&[PayloadFrame {
                kind: 41,
                critical: false,
                body: vec![0, 0, 0x40, 0x06, 0xaa, 0xbb],
            }]);
            let mut cookie_packet = cookie_packet;
            cookie_packet[8..16].fill(0);
            epdg.send_to(&cookie_packet, source).await.unwrap();

            let (retry_length, retry_source) = epdg.recv_from(&mut buffer).await.unwrap();
            assert_eq!(retry_source, source);
            let (retry_header, retry_payload) = IkeHeader::decode(&buffer[..retry_length]).unwrap();
            let retry = decode_payload_chain(retry_header.next_payload, retry_payload).unwrap();
            assert_eq!(
                retry.iter().map(|frame| frame.kind).collect::<Vec<_>>(),
                [41, 33, 34, 40, 41, 41]
            );
            assert_eq!(retry[0].body, [0, 0, 0x40, 0x06, 0xaa, 0xbb]);
            assert_eq!(&retry[1..], first_request);
            assert_eq!(retry[2].body, original_ke);
            assert_eq!(retry[3].body, [4; 32]);

            let mut peer_ke = vec![0, 14, 0, 0];
            peer_ke.extend_from_slice(&responder_public);
            let response = response_with(&[
                PayloadFrame {
                    kind: 33,
                    critical: false,
                    body: test_sa(),
                },
                PayloadFrame {
                    kind: 34,
                    critical: false,
                    body: peer_ke,
                },
                PayloadFrame {
                    kind: 40,
                    critical: false,
                    body: vec![7; 32],
                },
            ]);
            epdg.send_to(&response, source).await.unwrap();
        });
        let transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "unused.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap();
        let schedule = perform_sa_init_group14(
            &transport,
            7,
            &[
                PayloadFrame {
                    kind: 33,
                    critical: false,
                    body: test_sa(),
                },
                PayloadFrame {
                    kind: 40,
                    critical: false,
                    body: vec![4; 32],
                },
            ],
            1,
            Duration::from_millis(500),
        )
        .await
        .unwrap();
        assert_eq!(schedule.response.dh_group, 14);
        assert_eq!(schedule.keys.sk_d.len(), 32);
        assert!(schedule.keys.sk_ai.is_empty());
        assert!(schedule.keys.sk_ar.is_empty());
        assert_eq!(schedule.keys.sk_ei.len(), 20);
        assert_eq!(schedule.keys.sk_er.len(), 20);
        assert_eq!(schedule.keys.sk_pi.len(), 32);
        assert_eq!(schedule.keys.sk_pr.len(), 32);
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn explicitly_configured_group2_sha1_sa_init_completes_offline_exchange() {
        let epdg = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = epdg.local_addr().unwrap();
        let responder_dh = Modp1024KeyPair::generate().unwrap();
        let responder_public = responder_dh.public_key().to_vec();
        let selected_sa = proletarius_ike::build_native_proposals(
            &["aes128-sha1-prfsha1-modp1024".into()],
            proletarius_ike::ProposalProtocol::Ike,
            &[],
        )
        .unwrap();
        let responder = tokio::spawn(async move {
            let mut buffer = [0; 2048];
            let (length, source) = epdg.recv_from(&mut buffer).await.unwrap();
            let (header, payload) = IkeHeader::decode(&buffer[..length]).unwrap();
            let request = decode_payload_chain(header.next_payload, payload).unwrap();
            let (group, peer_public) = ke_parts(&request[1].body).unwrap();
            assert_eq!(group, 2);
            assert_eq!(peer_public.len(), 128);
            let mut peer_ke = vec![0, 2, 0, 0];
            peer_ke.extend_from_slice(&responder_public);
            let response = response_with(&[
                PayloadFrame {
                    kind: 33,
                    critical: false,
                    body: selected_sa,
                },
                PayloadFrame {
                    kind: 34,
                    critical: false,
                    body: peer_ke,
                },
                PayloadFrame {
                    kind: 40,
                    critical: false,
                    body: vec![7; 32],
                },
            ]);
            epdg.send_to(&response, source).await.unwrap();
        });
        let transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "unused.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap();
        let proposals = proletarius_ike::build_native_proposals(
            &["aes128-sha1-prfsha1-modp1024".into()],
            proletarius_ike::ProposalProtocol::Ike,
            &[],
        )
        .unwrap();
        let schedule = perform_sa_init_configured(
            &transport,
            7,
            &[
                PayloadFrame {
                    kind: 33,
                    critical: false,
                    body: proposals,
                },
                PayloadFrame {
                    kind: 34,
                    critical: false,
                    body: vec![0, 2, 0, 0, 0],
                },
                PayloadFrame {
                    kind: 40,
                    critical: false,
                    body: vec![4; 32],
                },
            ],
            1,
            Duration::from_millis(500),
        )
        .await
        .unwrap();
        assert_eq!(schedule.response.dh_group, 2);
        assert_eq!(schedule.prf_id, 2);
        assert_eq!(schedule.integrity_transform, Some(2));
        assert_eq!(schedule.keys.sk_ai.len(), 20);
        assert_eq!(schedule.keys.sk_ar.len(), 20);
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn sa_init_retries_identical_packet_then_times_out() {
        let epdg = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = epdg.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let mut buffer = [0; 256];
            let (first_length, _) = epdg.recv_from(&mut buffer).await.unwrap();
            let first = buffer[..first_length].to_vec();
            let (second_length, _) = epdg.recv_from(&mut buffer).await.unwrap();
            assert_eq!(&buffer[..second_length], first);
        });
        let transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "unused.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            perform_sa_init(&transport, 7, 33, b"proposal", 2, Duration::from_millis(20)).await,
            Err(SaInitError::Timeout)
        ));
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn sa_init_rejects_wrong_spi_without_accepting_foreign_session() {
        let epdg = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = epdg.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let mut buffer = [0; 256];
            let (_, source) = epdg.recv_from(&mut buffer).await.unwrap();
            let reply = IkeHeader {
                initiator_spi: 8,
                responder_spi: 9,
                next_payload: 0,
                version: 0x20,
                exchange: ExchangeType::IkeSaInit,
                flags: IkeHeader::RESPONSE_FLAG,
                message_id: 0,
            }
            .encode(&[])
            .unwrap();
            epdg.send_to(&reply, source).await.unwrap();
        });
        let transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "unused.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            perform_sa_init(
                &transport,
                7,
                33,
                b"proposal",
                2,
                Duration::from_millis(100)
            )
            .await,
            Err(SaInitError::Protocol(IkeProtocolError::InitiatorSpiChanged))
        ));
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn sa_init_cookie_with_zero_responder_spi_is_reported() {
        let epdg = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = epdg.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let mut buffer = [0; 256];
            let (_, source) = epdg.recv_from(&mut buffer).await.unwrap();
            let (first, body) = encode_payload_chain(&[PayloadFrame {
                kind: 41,
                critical: false,
                body: vec![0, 0, 0x40, 0x06, 0xaa],
            }])
            .unwrap();
            let reply = IkeHeader {
                initiator_spi: 7,
                responder_spi: 0,
                next_payload: first,
                version: 0x20,
                exchange: ExchangeType::IkeSaInit,
                flags: IkeHeader::RESPONSE_FLAG,
                message_id: 0,
            }
            .encode(&body)
            .unwrap();
            epdg.send_to(&reply, source).await.unwrap();
        });
        let transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "unused.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            perform_sa_init(&transport, 7, 33, b"proposal", 2, Duration::from_millis(100)).await,
            Err(SaInitError::CookieRequired(cookie)) if cookie == [0xaa]
        ));
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn sa_init_payloads_retry_cookie_first_without_changing_original_payloads() {
        let epdg = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = epdg.local_addr().unwrap();
        let initial = vec![
            PayloadFrame {
                kind: 33,
                critical: false,
                body: test_sa(),
            },
            PayloadFrame {
                kind: 34,
                critical: false,
                body: vec![0, 14, 0, 0, 3],
            },
            PayloadFrame {
                kind: 40,
                critical: false,
                body: vec![7; 16],
            },
        ];
        let expected = initial.clone();
        let peer = tokio::spawn(async move {
            let mut buffer = [0; 512];
            let (length, source) = epdg.recv_from(&mut buffer).await.unwrap();
            let (first_header, first_body) = IkeHeader::decode(&buffer[..length]).unwrap();
            assert_eq!(
                decode_payload_chain(first_header.next_payload, first_body).unwrap(),
                expected
            );
            let cookie = response_with(&[PayloadFrame {
                kind: 41,
                critical: false,
                body: vec![0, 0, 0x40, 0x06, 0xaa],
            }]);
            let mut cookie = cookie;
            cookie[8..16].fill(0);
            epdg.send_to(&cookie, source).await.unwrap();
            let (length, retry_source) = epdg.recv_from(&mut buffer).await.unwrap();
            assert_eq!(retry_source, source);
            let (retry_header, retry_body) = IkeHeader::decode(&buffer[..length]).unwrap();
            assert_eq!(retry_header.initiator_spi, first_header.initiator_spi);
            assert_eq!(retry_header.message_id, 0);
            let retry = decode_payload_chain(retry_header.next_payload, retry_body).unwrap();
            assert_eq!(retry[0].kind, 41);
            assert_eq!(retry[0].body, [0, 0, 0x40, 0x06, 0xaa]);
            assert_eq!(&retry[1..], expected);
            let mut success = response_with(&[
                PayloadFrame {
                    kind: 33,
                    critical: false,
                    body: test_sa(),
                },
                PayloadFrame {
                    kind: 34,
                    critical: false,
                    body: vec![0, 14, 0, 0, 3],
                },
                PayloadFrame {
                    kind: 40,
                    critical: false,
                    body: vec![8; 16],
                },
            ]);
            success[0..8].copy_from_slice(&first_header.initiator_spi.to_be_bytes());
            epdg.send_to(&success, source).await.unwrap();
        });
        let transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "unused.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap();
        perform_sa_init_payloads(&transport, 7, &initial, 2, Duration::from_millis(100))
            .await
            .unwrap();
        peer.await.unwrap();
    }

    #[test]
    fn nat_detection_insertion_forces_socks_nat_and_preserves_cert_order() {
        let mut frames = vec![
            PayloadFrame {
                kind: 33,
                critical: false,
                body: test_sa(),
            },
            PayloadFrame {
                kind: 34,
                critical: false,
                body: vec![0, 14, 0, 0, 3],
            },
            PayloadFrame {
                kind: 40,
                critical: false,
                body: vec![7; 16],
            },
            PayloadFrame {
                kind: 41,
                critical: false,
                body: vec![0, 0, 0x40, 0x2e],
            },
        ];
        let baseline = frames.clone();
        insert_nat_detection_frames(&mut frames, 7, NatDetectionEvidence::ForceNat).unwrap();
        assert_eq!(frames.len(), baseline.len() + 2);
        assert_eq!(&frames[..3], &baseline[..3]);
        assert_eq!(
            frames[3],
            nat_detection_notify(
                NatDetectionKind::Source,
                7,
                0,
                SocketAddr::from(([0, 0, 0, 0], 0)),
            )
        );
        assert_eq!(
            frames[4],
            nat_detection_notify(
                NatDetectionKind::Destination,
                7,
                0,
                SocketAddr::from(([0_u16; 8], 0)),
            )
        );
        assert_eq!(frames[5], baseline[3]);

        let mut frames = baseline.clone();
        let source: SocketAddr = "192.0.2.10:500".parse().unwrap();
        let destination: SocketAddr = "198.51.100.20:500".parse().unwrap();
        insert_nat_detection_frames(
            &mut frames,
            7,
            NatDetectionEvidence::Exact {
                source,
                destination,
            },
        )
        .unwrap();
        assert_eq!(frames.len(), baseline.len() + 2);
        assert_eq!(&frames[..3], &baseline[..3]);
        assert_eq!(
            frames[3],
            nat_detection_notify(NatDetectionKind::Source, 7, 0, source)
        );
        assert_eq!(
            frames[4],
            nat_detection_notify(NatDetectionKind::Destination, 7, 0, destination)
        );
        assert_eq!(frames[5], baseline[3]);
    }

    #[tokio::test]
    async fn sa_init_cookie_as_sa_preserves_nat_detection_frames() {
        let epdg = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = epdg.local_addr().unwrap();
        let responder_dh = Modp2048KeyPair::generate().unwrap();
        let responder_public = responder_dh.public_key().to_vec();
        let peer = tokio::spawn(async move {
            let mut buffer = [0; 2048];
            let (length, source) = epdg.recv_from(&mut buffer).await.unwrap();
            let (header, first_payload) = IkeHeader::decode(&buffer[..length]).unwrap();
            let first = decode_payload_chain(header.next_payload, first_payload).unwrap();
            assert_eq!(
                first.iter().map(|frame| frame.kind).collect::<Vec<_>>(),
                [33, 34, 40, 41, 41]
            );
            assert_eq!(
                first[3],
                nat_detection_notify(NatDetectionKind::Source, 7, 0, source)
            );
            assert_eq!(
                first[4],
                nat_detection_notify(NatDetectionKind::Destination, 7, 0, address)
            );

            let mut cookie = response_with(&[PayloadFrame {
                kind: 41,
                critical: false,
                body: vec![0, 0, 0x40, 0x06, 0xaa],
            }]);
            cookie[8..16].fill(0);
            epdg.send_to(&cookie, source).await.unwrap();
            let (length, retry_source) = epdg.recv_from(&mut buffer).await.unwrap();
            assert_eq!(retry_source, source);
            let (retry_header, retry_payload) = IkeHeader::decode(&buffer[..length]).unwrap();
            let retry = decode_payload_chain(retry_header.next_payload, retry_payload).unwrap();
            assert_eq!(retry_header.initiator_spi, header.initiator_spi);
            assert_eq!(
                retry.iter().map(|frame| frame.kind).collect::<Vec<_>>(),
                [33, 33, 34, 40, 41, 41]
            );
            assert_eq!(retry[0].body, [0, 0, 0x40, 0x06, 0xaa]);
            assert_eq!(&retry[1..], first);

            let mut peer_ke = vec![0, 14, 0, 0];
            peer_ke.extend_from_slice(&responder_public);
            epdg.send_to(
                &response_with(&[
                    PayloadFrame {
                        kind: 33,
                        critical: false,
                        body: test_sa(),
                    },
                    PayloadFrame {
                        kind: 34,
                        critical: false,
                        body: peer_ke,
                    },
                    PayloadFrame {
                        kind: 40,
                        critical: false,
                        body: vec![8; 32],
                    },
                ]),
                source,
            )
            .await
            .unwrap();
        });
        let transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "unused.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap();
        let schedule = perform_sa_init_configured_with_cookie(
            &transport,
            7,
            &[
                PayloadFrame {
                    kind: 33,
                    critical: false,
                    body: test_sa(),
                },
                PayloadFrame {
                    kind: 34,
                    critical: false,
                    body: vec![0, 14, 0, 0, 0],
                },
                PayloadFrame {
                    kind: 40,
                    critical: false,
                    body: vec![7; 32],
                },
            ],
            2,
            Duration::from_millis(500),
            true,
        )
        .await
        .unwrap();
        assert_eq!(schedule.response.dh_group, 14);
        peer.await.unwrap();
    }
}
