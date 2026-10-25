#![forbid(unsafe_code)]

use std::fmt;

pub mod cert_auth;
pub mod eap_aka;
mod keymat;
mod modp;
mod nat_detection;
mod payload;
mod proposal_config;
mod sa;
pub use proposal_config::{
    ProposalConfigError, ProposalProtocol, build_gcm_proposals, build_native_proposals,
};
pub mod esp;
pub mod sk;
pub use keymat::{
    ChildSaKeyMaterial, IkeSaKeyMaterial, KeyDerivationError, auth_signed_octets, compute_eap_auth,
    derive_ike_sa_key_material, derive_initial_child_sa_keys, derive_rekey_child_sa_keys,
    derive_rekey_ike_sa_key_material,
};
pub use modp::{DhError, Modp1024KeyPair, Modp2048KeyPair};
pub use nat_detection::{NatDetectionKind, nat_detection_notify};
pub use payload::{PayloadChainError, PayloadFrame, decode_payload_chain, encode_payload_chain};
pub use sa::{SaAttribute, SaDecodeError, SaProposal, SaTransform, decode_sa_proposals};

pub const IKE_WIRE_CONTRACT: &str = "ikev2-natt-state-v1";
pub const IKE_HEADER_LENGTH: usize = 28;
pub const NON_ESP_MARKER: [u8; 4] = [0, 0, 0, 0];
const MAX_IKE_PACKET_LENGTH: usize = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ExchangeType {
    IkeSaInit = 34,
    IkeAuth = 35,
    CreateChildSa = 36,
    Informational = 37,
    SessionResume = 38,
}

impl TryFrom<u8> for ExchangeType {
    type Error = IkeProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            34 => Ok(Self::IkeSaInit),
            35 => Ok(Self::IkeAuth),
            36 => Ok(Self::CreateChildSa),
            37 => Ok(Self::Informational),
            38 => Ok(Self::SessionResume),
            other => Err(IkeProtocolError::UnsupportedExchange(other)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IkeProtocolError {
    PacketTooShort,
    PacketTooLarge,
    InvalidVersion(u8),
    UnsupportedExchange(u8),
    InvalidDeclaredLength { declared: usize, actual: usize },
    MissingNonEspMarker,
    EspPacketOnIkeChannel,
    InvalidTransition,
    RequestAlreadyPending,
    UnexpectedResponse,
    InitiatorSpiMissing,
    InitiatorSpiChanged,
    ResponderSpiMissing,
    ResponderSpiChanged,
}

impl fmt::Display for IkeProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketTooShort => formatter.write_str("IKE packet is shorter than its header"),
            Self::PacketTooLarge => formatter.write_str("IKE packet exceeds the configured limit"),
            Self::InvalidVersion(version) => {
                write!(formatter, "unsupported IKE version {version:#04x}")
            }
            Self::UnsupportedExchange(exchange) => {
                write!(formatter, "unsupported IKE exchange {exchange}")
            }
            Self::InvalidDeclaredLength { declared, actual } => {
                write!(
                    formatter,
                    "IKE length mismatch: declared {declared}, actual {actual}"
                )
            }
            Self::MissingNonEspMarker => {
                formatter.write_str("NAT-T IKE packet is missing the non-ESP marker")
            }
            Self::EspPacketOnIkeChannel => {
                formatter.write_str("ESP packet was delivered to the IKE decoder")
            }
            Self::InvalidTransition => formatter.write_str("invalid IKE session transition"),
            Self::RequestAlreadyPending => formatter.write_str("an IKE request is already pending"),
            Self::UnexpectedResponse => {
                formatter.write_str("IKE response does not match the pending request")
            }
            Self::InitiatorSpiMissing => formatter.write_str("IKE initiator SPI must be non-zero"),
            Self::InitiatorSpiChanged => {
                formatter.write_str("IKE initiator SPI changed within the session")
            }
            Self::ResponderSpiMissing => {
                formatter.write_str("IKE_SA_INIT response has no responder SPI")
            }
            Self::ResponderSpiChanged => {
                formatter.write_str("IKE responder SPI changed within the session")
            }
        }
    }
}

impl std::error::Error for IkeProtocolError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IkeHeader {
    pub initiator_spi: u64,
    pub responder_spi: u64,
    pub next_payload: u8,
    pub version: u8,
    pub exchange: ExchangeType,
    pub flags: u8,
    pub message_id: u32,
}

impl IkeHeader {
    pub const INITIATOR_FLAG: u8 = 0x08;
    pub const VERSION_FLAG: u8 = 0x10;
    pub const RESPONSE_FLAG: u8 = 0x20;

    pub fn is_response(&self) -> bool {
        self.flags & Self::RESPONSE_FLAG != 0
    }

    pub fn encode(&self, payload: &[u8]) -> Result<Vec<u8>, IkeProtocolError> {
        let length = IKE_HEADER_LENGTH
            .checked_add(payload.len())
            .ok_or(IkeProtocolError::PacketTooLarge)?;
        if length > MAX_IKE_PACKET_LENGTH || length > u32::MAX as usize {
            return Err(IkeProtocolError::PacketTooLarge);
        }
        if self.version >> 4 != 2 {
            return Err(IkeProtocolError::InvalidVersion(self.version));
        }
        let mut packet = Vec::with_capacity(length);
        packet.extend_from_slice(&self.initiator_spi.to_be_bytes());
        packet.extend_from_slice(&self.responder_spi.to_be_bytes());
        packet.push(self.next_payload);
        packet.push(self.version);
        packet.push(self.exchange as u8);
        packet.push(self.flags);
        packet.extend_from_slice(&self.message_id.to_be_bytes());
        packet.extend_from_slice(&(length as u32).to_be_bytes());
        packet.extend_from_slice(payload);
        Ok(packet)
    }

    pub fn decode(packet: &[u8]) -> Result<(Self, &[u8]), IkeProtocolError> {
        if packet.len() < IKE_HEADER_LENGTH {
            return Err(IkeProtocolError::PacketTooShort);
        }
        if packet.len() > MAX_IKE_PACKET_LENGTH {
            return Err(IkeProtocolError::PacketTooLarge);
        }
        let version = packet[17];
        if version >> 4 != 2 {
            return Err(IkeProtocolError::InvalidVersion(version));
        }
        let declared =
            u32::from_be_bytes(packet[24..28].try_into().expect("fixed header")) as usize;
        if declared != packet.len() {
            return Err(IkeProtocolError::InvalidDeclaredLength {
                declared,
                actual: packet.len(),
            });
        }
        Ok((
            Self {
                initiator_spi: u64::from_be_bytes(packet[0..8].try_into().expect("fixed header")),
                responder_spi: u64::from_be_bytes(packet[8..16].try_into().expect("fixed header")),
                next_payload: packet[16],
                version,
                exchange: packet[18].try_into()?,
                flags: packet[19],
                message_id: u32::from_be_bytes(packet[20..24].try_into().expect("fixed header")),
            },
            &packet[IKE_HEADER_LENGTH..],
        ))
    }
}

pub fn encode_natt(packet: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(NON_ESP_MARKER.len() + packet.len());
    framed.extend_from_slice(&NON_ESP_MARKER);
    framed.extend_from_slice(packet);
    framed
}

pub fn decode_natt(datagram: &[u8]) -> Result<&[u8], IkeProtocolError> {
    if datagram.len() < NON_ESP_MARKER.len() {
        return Err(IkeProtocolError::MissingNonEspMarker);
    }
    if datagram[..4] != NON_ESP_MARKER {
        return Err(IkeProtocolError::EspPacketOnIkeChannel);
    }
    Ok(&datagram[4..])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPhase {
    Idle,
    SaInitPending,
    Authenticating,
    Established,
    Deleting,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseDisposition {
    Accepted,
    Duplicate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingRequest {
    exchange: ExchangeType,
    message_id: u32,
}

#[derive(Debug)]
pub struct SessionTracker {
    initiator_spi: u64,
    phase: SessionPhase,
    next_message_id: u32,
    pending: Option<PendingRequest>,
    last_accepted: Option<PendingRequest>,
    responder_spi: u64,
    local_initiator: bool,
}

impl SessionTracker {
    pub fn new(initiator_spi: u64) -> Result<Self, IkeProtocolError> {
        if initiator_spi == 0 {
            return Err(IkeProtocolError::InitiatorSpiMissing);
        }
        Ok(Self {
            initiator_spi,
            phase: SessionPhase::Idle,
            next_message_id: 0,
            pending: None,
            last_accepted: None,
            responder_spi: 0,
            local_initiator: true,
        })
    }

    /// Starts the message-ID sequence immediately after a validated
    /// IKE_SA_INIT response. IKE_AUTH begins at message ID 1.
    pub fn from_sa_init_response(header: &IkeHeader) -> Result<Self, IkeProtocolError> {
        if header.exchange != ExchangeType::IkeSaInit
            || !header.is_response()
            || header.flags & IkeHeader::INITIATOR_FLAG != 0
            || header.message_id != 0
        {
            return Err(IkeProtocolError::UnexpectedResponse);
        }
        if header.initiator_spi == 0 {
            return Err(IkeProtocolError::InitiatorSpiMissing);
        }
        if header.responder_spi == 0 {
            return Err(IkeProtocolError::ResponderSpiMissing);
        }
        Ok(Self {
            initiator_spi: header.initiator_spi,
            phase: SessionPhase::Authenticating,
            next_message_id: 1,
            pending: None,
            last_accepted: Some(PendingRequest {
                exchange: ExchangeType::IkeSaInit,
                message_id: 0,
            }),
            responder_spi: header.responder_spi,
            local_initiator: true,
        })
    }
    /// Caller must already validate the authenticated rekey offer/KE and keys.
    pub fn from_rekey(
        spi_i: u64,
        spi_r: u64,
        local_initiator: bool,
    ) -> Result<Self, IkeProtocolError> {
        let mut tracker = Self::new(spi_i)?;
        if spi_r == 0 {
            return Err(IkeProtocolError::ResponderSpiMissing);
        }
        tracker.phase = SessionPhase::Established;
        tracker.responder_spi = spi_r;
        tracker.local_initiator = local_initiator;
        Ok(tracker)
    }
    pub fn phase(&self) -> SessionPhase {
        self.phase
    }

    /// Closes the SA after a failed authenticated exchange so callers cannot
    /// accidentally continue with a consumed message ID or uncertain state.
    pub fn fail_and_close(&mut self) {
        self.pending = None;
        self.phase = SessionPhase::Closed;
    }

    pub fn begin_request(&mut self, exchange: ExchangeType) -> Result<u32, IkeProtocolError> {
        if self.pending.is_some() {
            return Err(IkeProtocolError::RequestAlreadyPending);
        }
        let valid = matches!(
            (self.phase, exchange),
            (SessionPhase::Idle, ExchangeType::IkeSaInit)
                | (SessionPhase::Authenticating, ExchangeType::IkeAuth)
                | (SessionPhase::Established, ExchangeType::CreateChildSa)
                | (SessionPhase::Established, ExchangeType::Informational)
                | (SessionPhase::Deleting, ExchangeType::Informational)
        );
        if !valid {
            return Err(IkeProtocolError::InvalidTransition);
        }
        let request = PendingRequest {
            exchange,
            message_id: self.next_message_id,
        };
        self.pending = Some(request);
        if exchange == ExchangeType::IkeSaInit {
            self.phase = SessionPhase::SaInitPending;
        }
        Ok(request.message_id)
    }

    pub fn accept_response(
        &mut self,
        header: &IkeHeader,
    ) -> Result<ResponseDisposition, IkeProtocolError> {
        let response = PendingRequest {
            exchange: header.exchange,
            message_id: header.message_id,
        };
        if !header.is_response()
            || (header.flags & IkeHeader::INITIATOR_FLAG != 0) == self.local_initiator
        {
            return Err(IkeProtocolError::UnexpectedResponse);
        }
        if header.initiator_spi != self.initiator_spi {
            return Err(IkeProtocolError::InitiatorSpiChanged);
        }
        if header.exchange == ExchangeType::IkeSaInit && header.responder_spi == 0 {
            return Err(IkeProtocolError::ResponderSpiMissing);
        }
        if self.responder_spi != 0 && header.responder_spi != self.responder_spi {
            return Err(IkeProtocolError::ResponderSpiChanged);
        }
        // A retransmitted response can arrive while the next request is
        // pending. Validate both SPIs and response flags before ignoring it;
        // a matching message ID alone is not evidence of the same IKE SA.
        if self.last_accepted == Some(response) {
            return Ok(ResponseDisposition::Duplicate);
        }
        if self.pending != Some(response) {
            return Err(IkeProtocolError::UnexpectedResponse);
        }
        self.pending = None;
        self.last_accepted = Some(response);
        self.responder_spi = header.responder_spi;
        self.next_message_id = self.next_message_id.wrapping_add(1);
        self.phase = match (self.phase, header.exchange) {
            (SessionPhase::SaInitPending, ExchangeType::IkeSaInit) => SessionPhase::Authenticating,
            (SessionPhase::Deleting, ExchangeType::Informational) => SessionPhase::Closed,
            (phase, _) => phase,
        };
        Ok(ResponseDisposition::Accepted)
    }

    /// Completes IKE_AUTH only after final AUTH, configuration, Child SA, and
    /// traffic-selector verification. Intermediate EAP rounds remain in the
    /// authenticating phase and may use subsequent message IDs.
    pub fn complete_authentication(&mut self) -> Result<(), IkeProtocolError> {
        if self.phase != SessionPhase::Authenticating || self.pending.is_some() {
            return Err(IkeProtocolError::InvalidTransition);
        }
        self.phase = SessionPhase::Established;
        Ok(())
    }

    pub fn begin_delete(&mut self) -> Result<(), IkeProtocolError> {
        if self.phase != SessionPhase::Established || self.pending.is_some() {
            return Err(IkeProtocolError::InvalidTransition);
        }
        self.phase = SessionPhase::Deleting;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExchangeType, IkeHeader, IkeProtocolError, ResponseDisposition, SessionPhase,
        SessionTracker, decode_natt, encode_natt,
    };

    fn response(exchange: ExchangeType, message_id: u32, responder_spi: u64) -> IkeHeader {
        IkeHeader {
            initiator_spi: 1,
            responder_spi,
            next_payload: 0,
            version: 0x20,
            exchange,
            flags: IkeHeader::RESPONSE_FLAG,
            message_id,
        }
    }

    #[test]
    fn header_round_trip_matches_ikev2_wire_layout() {
        let header = IkeHeader {
            initiator_spi: 0x0102_0304_0506_0708,
            responder_spi: 0,
            next_payload: 33,
            version: 0x20,
            exchange: ExchangeType::IkeSaInit,
            flags: IkeHeader::INITIATOR_FLAG,
            message_id: 0,
        };
        let packet = header.encode(&[1, 2, 3]).expect("encode IKE header");
        assert_eq!(packet.len(), 31);
        assert_eq!(&packet[24..28], &[0, 0, 0, 31]);
        let (decoded, payload) = IkeHeader::decode(&packet).expect("decode IKE header");
        assert_eq!(decoded, header);
        assert_eq!(payload, [1, 2, 3]);
    }

    #[test]
    fn rejects_truncation_length_mismatch_and_ikev1() {
        assert_eq!(
            IkeHeader::decode(&[0; 27]),
            Err(IkeProtocolError::PacketTooShort)
        );
        let mut packet = response(ExchangeType::IkeAuth, 1, 2)
            .encode(&[])
            .expect("packet");
        packet[27] = 29;
        assert!(matches!(
            IkeHeader::decode(&packet),
            Err(IkeProtocolError::InvalidDeclaredLength { .. })
        ));
        packet[27] = 28;
        packet[17] = 0x10;
        assert_eq!(
            IkeHeader::decode(&packet),
            Err(IkeProtocolError::InvalidVersion(0x10))
        );
    }

    #[test]
    fn natt_marker_never_confuses_esp_with_ike() {
        let framed = encode_natt(&[1, 2, 3]);
        assert_eq!(decode_natt(&framed), Ok(&[1, 2, 3][..]));
        assert_eq!(
            decode_natt(&[0x12, 0x34, 0x56, 0x78, 1]),
            Err(IkeProtocolError::EspPacketOnIkeChannel)
        );
    }

    #[test]
    fn session_window_accepts_only_matching_responses_and_duplicates() {
        assert!(matches!(
            SessionTracker::new(0),
            Err(IkeProtocolError::InitiatorSpiMissing)
        ));
        let mut tracker = SessionTracker::new(1).unwrap();
        assert_eq!(tracker.begin_request(ExchangeType::IkeSaInit), Ok(0));
        assert_eq!(
            tracker.accept_response(&response(ExchangeType::IkeAuth, 0, 2)),
            Err(IkeProtocolError::UnexpectedResponse)
        );
        let init_response = response(ExchangeType::IkeSaInit, 0, 2);
        assert_eq!(
            tracker.accept_response(&init_response),
            Ok(ResponseDisposition::Accepted)
        );
        assert_eq!(tracker.phase(), SessionPhase::Authenticating);
        assert_eq!(
            tracker.accept_response(&init_response),
            Ok(ResponseDisposition::Duplicate)
        );
        let mut spoofed_duplicate = init_response.clone();
        spoofed_duplicate.initiator_spi = 9;
        assert_eq!(
            tracker.accept_response(&spoofed_duplicate),
            Err(IkeProtocolError::InitiatorSpiChanged)
        );
        spoofed_duplicate = init_response.clone();
        spoofed_duplicate.responder_spi = 9;
        assert_eq!(
            tracker.accept_response(&spoofed_duplicate),
            Err(IkeProtocolError::ResponderSpiChanged)
        );
        spoofed_duplicate = init_response.clone();
        spoofed_duplicate.flags = IkeHeader::INITIATOR_FLAG;
        assert_eq!(
            tracker.accept_response(&spoofed_duplicate),
            Err(IkeProtocolError::UnexpectedResponse)
        );
        assert_eq!(tracker.begin_request(ExchangeType::IkeAuth), Ok(1));
        assert_eq!(
            tracker.accept_response(&init_response),
            Ok(ResponseDisposition::Duplicate)
        );
        assert_eq!(
            tracker.begin_request(ExchangeType::IkeAuth),
            Err(IkeProtocolError::RequestAlreadyPending)
        );
        let mut wrong_spi = response(ExchangeType::IkeAuth, 1, 2);
        wrong_spi.initiator_spi = 9;
        assert_eq!(
            tracker.accept_response(&wrong_spi),
            Err(IkeProtocolError::InitiatorSpiChanged)
        );
        assert_eq!(
            tracker.accept_response(&response(ExchangeType::IkeAuth, 1, 2)),
            Ok(ResponseDisposition::Accepted)
        );
        assert_eq!(tracker.phase(), SessionPhase::Authenticating);
        assert_eq!(tracker.begin_request(ExchangeType::IkeAuth), Ok(2));
        assert_eq!(
            tracker.accept_response(&response(ExchangeType::IkeAuth, 2, 2)),
            Ok(ResponseDisposition::Accepted)
        );
        tracker.complete_authentication().unwrap();
        assert_eq!(tracker.phase(), SessionPhase::Established);
    }

    #[test]
    fn session_can_resume_from_validated_sa_init_at_message_id_one() {
        let init = response(ExchangeType::IkeSaInit, 0, 2);
        let mut tracker = SessionTracker::from_sa_init_response(&init).unwrap();
        assert_eq!(tracker.phase(), SessionPhase::Authenticating);
        assert_eq!(tracker.begin_request(ExchangeType::IkeAuth), Ok(1));
        assert_eq!(
            tracker.accept_response(&init),
            Ok(ResponseDisposition::Duplicate)
        );
        assert_eq!(
            tracker.accept_response(&response(ExchangeType::IkeAuth, 1, 2)),
            Ok(ResponseDisposition::Accepted)
        );
        tracker.fail_and_close();
        assert_eq!(tracker.phase(), SessionPhase::Closed);
        assert_eq!(
            tracker.begin_request(ExchangeType::IkeAuth),
            Err(IkeProtocolError::InvalidTransition)
        );
    }

    #[test]
    fn delete_requires_established_session_and_closes_on_response() {
        let mut tracker = SessionTracker::new(1).unwrap();
        assert_eq!(
            tracker.begin_delete(),
            Err(IkeProtocolError::InvalidTransition)
        );
        tracker.begin_request(ExchangeType::IkeSaInit).unwrap();
        tracker
            .accept_response(&response(ExchangeType::IkeSaInit, 0, 2))
            .unwrap();
        tracker.begin_request(ExchangeType::IkeAuth).unwrap();
        tracker
            .accept_response(&response(ExchangeType::IkeAuth, 1, 2))
            .unwrap();
        tracker.complete_authentication().unwrap();
        tracker.begin_delete().unwrap();
        assert_eq!(tracker.begin_request(ExchangeType::Informational), Ok(2));
        tracker
            .accept_response(&response(ExchangeType::Informational, 2, 2))
            .unwrap();
        assert_eq!(tracker.phase(), SessionPhase::Closed);
    }
}
