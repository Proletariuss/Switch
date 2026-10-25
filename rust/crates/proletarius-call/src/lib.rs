#![deny(unsafe_code)]

use std::{
    collections::BTreeMap,
    fmt,
    time::{Duration, Instant},
};

pub mod amr;
pub mod amr_wb;
pub mod evs;
pub mod pcma;
pub mod sdp;

pub const CALL_PROTOCOL_CONTRACT: &str = "ims-call-rtp-state-v1";
const RTP_HEADER_LENGTH: usize = 12;
const MAX_RTP_PACKET_LENGTH: usize = 65_535;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallError {
    InvalidTransition,
    InvalidResponse,
    ResponseTransactionMismatch,
    Rejected(u16),
    RtpTooShort,
    RtpTooLarge,
    InvalidRtpVersion,
    InvalidRtpExtension,
    InvalidRtpPadding,
    EmptyRtpPayload,
    InvalidDtmfDigit,
}

impl fmt::Display for CallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition => formatter.write_str("invalid IMS call transition"),
            Self::InvalidResponse => formatter.write_str("invalid SIP INVITE response"),
            Self::ResponseTransactionMismatch => {
                formatter.write_str("SIP response does not match the INVITE transaction")
            }
            Self::Rejected(status) => write!(formatter, "IMS call rejected with SIP {status}"),
            Self::RtpTooShort => formatter.write_str("RTP packet is shorter than its header"),
            Self::RtpTooLarge => formatter.write_str("RTP packet exceeds the configured limit"),
            Self::InvalidRtpVersion => formatter.write_str("unsupported RTP version"),
            Self::InvalidRtpExtension => formatter.write_str("invalid RTP header extension"),
            Self::InvalidRtpPadding => formatter.write_str("invalid RTP padding"),
            Self::EmptyRtpPayload => formatter.write_str("RTP packet has no payload"),
            Self::InvalidDtmfDigit => formatter.write_str("DTMF digit must be 0-9, * or #"),
        }
    }
}

impl std::error::Error for CallError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallPhase {
    Idle,
    Inviting,
    Early,
    Cancelling,
    Active,
    Terminating,
    Ended,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InviteAction {
    Wait,
    StartEarlyMedia,
    SendPrack { start_early_media: bool },
    AckAndStartMedia,
    AckAndBye,
    AckAndEnd,
    AckAndFail { status: u16 },
    AckOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutgoingHangupAction {
    SendCancel { cseq: u32 },
    SendBye { cseq: u32 },
    Ignore,
}

#[derive(Debug)]
pub struct OutgoingCallTracker {
    phase: CallPhase,
    invite_cseq: u32,
    last_local_cseq: u32,
    cancel_requested: bool,
    last_rseq: Option<u32>,
    reliable_media_negotiated: bool,
    final_status: Option<u16>,
}

/// Rust-owned admission state for one incoming INVITE transaction. Socket
/// writes and media allocation remain with the IMS transport, while the
/// transaction phase and retransmission decision stay in this tracker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncomingCallPhase {
    Trying,
    Ringing,
    Connecting,
    Active,
    Ended,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncomingInviteAdmission {
    Accept,
    ResendRinging,
    ResendAnswer,
    Busy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncomingCancelDecision {
    Accept,
    Reject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncomingTimeoutDecision {
    Expire,
    Ignore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncomingByeDecision {
    Accept,
    Reject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncomingLocalHangupDecision {
    Reject,
    Bye { cseq: u32 },
    Ignore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncomingCallTracker {
    cseq: u32,
    phase: IncomingCallPhase,
}

impl IncomingCallTracker {
    pub fn begin(cseq: u32) -> Result<Self, CallError> {
        if cseq == 0 {
            return Err(CallError::InvalidResponse);
        }
        Ok(Self {
            cseq,
            phase: IncomingCallPhase::Trying,
        })
    }

    pub fn cseq(&self) -> u32 {
        self.cseq
    }

    pub fn phase(&self) -> IncomingCallPhase {
        self.phase
    }

    pub fn retransmission(&self, cseq: u32, available: bool) -> IncomingInviteAdmission {
        if self.cseq != cseq {
            return IncomingInviteAdmission::Busy;
        }
        match self.phase {
            IncomingCallPhase::Ringing => IncomingInviteAdmission::ResendRinging,
            IncomingCallPhase::Connecting | IncomingCallPhase::Active => {
                IncomingInviteAdmission::ResendAnswer
            }
            IncomingCallPhase::Trying if available => IncomingInviteAdmission::Accept,
            IncomingCallPhase::Trying | IncomingCallPhase::Ended => IncomingInviteAdmission::Busy,
        }
    }

    pub fn ringing(&mut self) -> Result<(), CallError> {
        if self.phase != IncomingCallPhase::Trying {
            return Err(CallError::InvalidTransition);
        }
        self.phase = IncomingCallPhase::Ringing;
        Ok(())
    }

    pub fn answered(&mut self) -> Result<(), CallError> {
        if self.phase != IncomingCallPhase::Ringing {
            return Err(CallError::InvalidTransition);
        }
        self.phase = IncomingCallPhase::Connecting;
        Ok(())
    }

    pub fn acknowledged(&mut self) -> Result<(), CallError> {
        if self.phase == IncomingCallPhase::Ended {
            // A delayed ACK can arrive after the local endpoint has already
            // committed a BYE. It must not resurrect the ended dialog.
            return Ok(());
        }
        if self.phase != IncomingCallPhase::Connecting {
            return Err(CallError::InvalidTransition);
        }
        self.phase = IncomingCallPhase::Active;
        Ok(())
    }

    /// Chooses a local response to hangup for this incoming dialog. The
    /// caller supplies a Rust-allocated local CSeq only when a BYE is needed.
    pub fn local_hangup(
        &mut self,
        invite_cseq: u32,
        request_cseq: u32,
    ) -> Result<IncomingLocalHangupDecision, CallError> {
        if self.cseq != invite_cseq {
            return Err(CallError::ResponseTransactionMismatch);
        }
        let decision = match self.phase {
            IncomingCallPhase::Trying | IncomingCallPhase::Ringing => {
                if request_cseq != 0 {
                    return Err(CallError::InvalidTransition);
                }
                IncomingLocalHangupDecision::Reject
            }
            IncomingCallPhase::Connecting | IncomingCallPhase::Active => {
                if request_cseq == 0 || request_cseq > i32::MAX as u32 {
                    return Err(CallError::InvalidTransition);
                }
                IncomingLocalHangupDecision::Bye { cseq: request_cseq }
            }
            IncomingCallPhase::Ended => {
                if request_cseq != 0 {
                    return Err(CallError::InvalidTransition);
                }
                return Ok(IncomingLocalHangupDecision::Ignore);
            }
        };
        self.phase = IncomingCallPhase::Ended;
        Ok(decision)
    }

    /// Classifies a CANCEL against the original INVITE transaction. A
    /// matching CANCEL is accepted only before the final answer is committed.
    pub fn cancel(&mut self, invite_cseq: u32) -> IncomingCancelDecision {
        if self.cseq != invite_cseq
            || !matches!(
                self.phase,
                IncomingCallPhase::Trying | IncomingCallPhase::Ringing
            )
        {
            return IncomingCancelDecision::Reject;
        }
        self.phase = IncomingCallPhase::Ended;
        IncomingCancelDecision::Accept
    }

    /// Expires only the same unanswered ringing INVITE transaction.
    pub fn timeout(&mut self, invite_cseq: u32) -> IncomingTimeoutDecision {
        if self.cseq != invite_cseq || self.phase != IncomingCallPhase::Ringing {
            return IncomingTimeoutDecision::Ignore;
        }
        self.phase = IncomingCallPhase::Ended;
        IncomingTimeoutDecision::Expire
    }

    /// Ends the tracked incoming dialog when the stored INVITE transaction
    /// identity still matches. The SIP BYE has its own CSeq; callers pass the
    /// original INVITE CSeq as the tracker key, not the BYE CSeq.
    pub fn bye(&mut self, invite_cseq: u32) -> IncomingByeDecision {
        if self.cseq != invite_cseq || self.phase == IncomingCallPhase::Ended {
            return IncomingByeDecision::Reject;
        }
        self.phase = IncomingCallPhase::Ended;
        IncomingByeDecision::Accept
    }

    pub fn end(&mut self) {
        self.phase = IncomingCallPhase::Ended;
    }
}

impl OutgoingCallTracker {
    pub fn begin(invite_cseq: u32) -> Result<Self, CallError> {
        if invite_cseq == 0 || invite_cseq > i32::MAX as u32 {
            return Err(CallError::InvalidTransition);
        }
        Ok(Self {
            phase: CallPhase::Inviting,
            invite_cseq,
            last_local_cseq: invite_cseq,
            cancel_requested: false,
            last_rseq: None,
            reliable_media_negotiated: false,
            final_status: None,
        })
    }

    pub fn phase(&self) -> CallPhase {
        self.phase
    }

    pub fn cancel(&mut self) -> Result<(), CallError> {
        if !matches!(self.phase, CallPhase::Inviting | CallPhase::Early) {
            return Err(CallError::InvalidTransition);
        }
        self.cancel_requested = true;
        self.phase = CallPhase::Cancelling;
        Ok(())
    }

    /// Chooses the SIP termination request from the Rust-owned dialog phase.
    /// The caller still builds/writes the request and owns transport teardown.
    pub fn hangup(&mut self) -> Result<OutgoingHangupAction, CallError> {
        match self.phase {
            CallPhase::Inviting | CallPhase::Early => {
                self.cancel_requested = true;
                self.phase = CallPhase::Cancelling;
                Ok(OutgoingHangupAction::SendCancel {
                    cseq: self.invite_cseq,
                })
            }
            CallPhase::Active => {
                let cseq = self.next_local_cseq()?;
                self.phase = CallPhase::Terminating;
                Ok(OutgoingHangupAction::SendBye { cseq })
            }
            CallPhase::Idle
            | CallPhase::Cancelling
            | CallPhase::Terminating
            | CallPhase::Ended
            | CallPhase::Failed => Ok(OutgoingHangupAction::Ignore),
        }
    }

    /// Reserves the next in-dialog CSeq for a Rust-approved local request.
    pub fn next_local_cseq(&mut self) -> Result<u32, CallError> {
        let next = self
            .last_local_cseq
            .checked_add(1)
            .filter(|value| *value <= i32::MAX as u32)
            .ok_or(CallError::InvalidTransition)?;
        self.last_local_cseq = next;
        Ok(next)
    }

    /// `has_sdp` means a successfully validated offer/answer, not merely a body.
    /// The caller must scope this tracker to a single SIP dialog.
    pub fn accept_invite_response(
        &mut self,
        status: u16,
        cseq: u32,
        has_sdp: bool,
        reliable_rseq: Option<u32>,
    ) -> Result<InviteAction, CallError> {
        if cseq != self.invite_cseq {
            return Err(CallError::ResponseTransactionMismatch);
        }
        if !(100..=699).contains(&status) {
            return Err(CallError::InvalidResponse);
        }
        if self.final_status == Some(status) {
            return Ok(InviteAction::AckOnly);
        }
        if !matches!(
            self.phase,
            CallPhase::Inviting | CallPhase::Early | CallPhase::Cancelling
        ) {
            return Err(CallError::InvalidTransition);
        }
        match status {
            100 => Ok(InviteAction::Wait),
            101..=199 => {
                if let Some(rseq) = reliable_rseq {
                    if rseq == 0 || (self.last_rseq.is_none() && rseq > i32::MAX as u32) {
                        return Err(CallError::InvalidResponse);
                    }
                    // RFC 3262: duplicates and gaps are not processed; a later
                    // retransmission can supply the missing consecutive RSeq.
                    if self
                        .last_rseq
                        .is_some_and(|previous| previous.checked_add(1) != Some(rseq))
                    {
                        return Ok(InviteAction::Wait);
                    }
                    self.last_rseq = Some(rseq);
                    self.reliable_media_negotiated |= has_sdp;
                    if self.phase != CallPhase::Cancelling {
                        self.phase = CallPhase::Early;
                    }
                    return Ok(InviteAction::SendPrack {
                        start_early_media: has_sdp && !self.cancel_requested,
                    });
                }
                if self.phase != CallPhase::Cancelling {
                    self.phase = CallPhase::Early;
                }
                Ok(if has_sdp && !self.cancel_requested {
                    InviteAction::StartEarlyMedia
                } else {
                    InviteAction::Wait
                })
            }
            200..=299 => {
                self.final_status = Some(status);
                if self.cancel_requested {
                    self.phase = CallPhase::Ended;
                    Ok(InviteAction::AckAndBye)
                } else if has_sdp || self.reliable_media_negotiated {
                    self.phase = CallPhase::Active;
                    Ok(InviteAction::AckAndStartMedia)
                } else {
                    self.phase = CallPhase::Failed;
                    Ok(InviteAction::AckAndFail { status })
                }
            }
            status => {
                self.final_status = Some(status);
                if self.cancel_requested && status == 487 {
                    self.phase = CallPhase::Ended;
                    Ok(InviteAction::AckAndEnd)
                } else {
                    self.phase = CallPhase::Failed;
                    Ok(InviteAction::AckAndFail { status })
                }
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtpPacket<'a> {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: &'a [u8],
}

impl<'a> RtpPacket<'a> {
    pub fn parse(packet: &'a [u8]) -> Result<Self, CallError> {
        if packet.len() < RTP_HEADER_LENGTH {
            return Err(CallError::RtpTooShort);
        }
        if packet.len() > MAX_RTP_PACKET_LENGTH {
            return Err(CallError::RtpTooLarge);
        }
        if packet[0] >> 6 != 2 {
            return Err(CallError::InvalidRtpVersion);
        }
        let csrc_bytes = usize::from(packet[0] & 0x0f)
            .checked_mul(4)
            .ok_or(CallError::RtpTooLarge)?;
        let mut offset = RTP_HEADER_LENGTH
            .checked_add(csrc_bytes)
            .ok_or(CallError::RtpTooLarge)?;
        if offset > packet.len() {
            return Err(CallError::RtpTooShort);
        }
        if packet[0] & 0x10 != 0 {
            let extension = packet
                .get(offset..offset + 4)
                .ok_or(CallError::InvalidRtpExtension)?;
            let words = usize::from(u16::from_be_bytes([extension[2], extension[3]]));
            offset = offset
                .checked_add(4)
                .and_then(|value| value.checked_add(words.checked_mul(4)?))
                .ok_or(CallError::InvalidRtpExtension)?;
            if offset > packet.len() {
                return Err(CallError::InvalidRtpExtension);
            }
        }
        let mut end = packet.len();
        if packet[0] & 0x20 != 0 {
            let padding = usize::from(*packet.last().ok_or(CallError::InvalidRtpPadding)?);
            if padding == 0 || padding > end.saturating_sub(offset) {
                return Err(CallError::InvalidRtpPadding);
            }
            end -= padding;
        }
        if offset >= end {
            return Err(CallError::EmptyRtpPayload);
        }
        Ok(Self {
            marker: packet[1] & 0x80 != 0,
            payload_type: packet[1] & 0x7f,
            sequence: u16::from_be_bytes([packet[2], packet[3]]),
            timestamp: u32::from_be_bytes(packet[4..8].try_into().expect("fixed RTP header")),
            ssrc: u32::from_be_bytes(packet[8..12].try_into().expect("fixed RTP header")),
            payload: &packet[offset..end],
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SequenceDisposition {
    First,
    InOrder,
    Missing(u8),
    DuplicateOrLate,
    StreamRestart,
}

#[derive(Debug, Default)]
pub struct SequenceTracker {
    ssrc: Option<u32>,
    last: u16,
}

impl SequenceTracker {
    pub fn observe(&mut self, ssrc: u32, sequence: u16) -> SequenceDisposition {
        let Some(previous_ssrc) = self.ssrc else {
            self.ssrc = Some(ssrc);
            self.last = sequence;
            return SequenceDisposition::First;
        };
        if previous_ssrc != ssrc {
            self.ssrc = Some(ssrc);
            self.last = sequence;
            return SequenceDisposition::StreamRestart;
        }
        let delta = sequence.wrapping_sub(self.last) as i16;
        if delta <= 0 {
            return SequenceDisposition::DuplicateOrLate;
        }
        self.last = sequence;
        if delta == 1 {
            SequenceDisposition::InOrder
        } else {
            SequenceDisposition::Missing((delta - 1).min(3) as u8)
        }
    }
}

const RTP_REORDER_CAPACITY: usize = 32;
const RTP_REORDER_GAP: Duration = Duration::from_millis(40);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedRtpPacket {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: Vec<u8>,
}

impl From<&RtpPacket<'_>> for OwnedRtpPacket {
    fn from(packet: &RtpPacket<'_>) -> Self {
        Self {
            marker: packet.marker,
            payload_type: packet.payload_type,
            sequence: packet.sequence,
            timestamp: packet.timestamp,
            ssrc: packet.ssrc,
            payload: packet.payload.to_vec(),
        }
    }
}

/// Bounded RTP reordering window used before decoding. It repairs short gaps,
/// then yields the nearest forward packet after 40ms so loss concealment can
/// advance. Playout pacing remains the responsibility of the media worker.
#[derive(Debug, Default)]
pub struct RtpReorder {
    ssrc: Option<u32>,
    next_sequence: u16,
    pending: BTreeMap<u16, OwnedRtpPacket>,
    gap_started: Option<Instant>,
}

impl RtpReorder {
    /// Returns false for duplicate/late packets and packets beyond capacity.
    pub fn push(&mut self, packet: &RtpPacket<'_>, now: Instant) -> bool {
        if self.ssrc != Some(packet.ssrc) {
            self.ssrc = Some(packet.ssrc);
            self.next_sequence = packet.sequence;
            self.pending.clear();
            self.gap_started = None;
        }
        if (packet.sequence.wrapping_sub(self.next_sequence) as i16) < 0
            || self.pending.contains_key(&packet.sequence)
        {
            return false;
        }
        if self.pending.len() >= RTP_REORDER_CAPACITY && packet.sequence != self.next_sequence {
            return false;
        }
        self.pending
            .insert(packet.sequence, OwnedRtpPacket::from(packet));
        if packet.sequence != self.next_sequence && self.gap_started.is_none() {
            self.gap_started = Some(now);
        }
        true
    }

    pub fn pop(&mut self, now: Instant) -> Option<OwnedRtpPacket> {
        let packet = if let Some(packet) = self.pending.remove(&self.next_sequence) {
            packet
        } else {
            let expired = self
                .gap_started
                .is_some_and(|started| now.saturating_duration_since(started) >= RTP_REORDER_GAP);
            if !expired {
                return None;
            }
            let sequence = self
                .pending
                .keys()
                .copied()
                .min_by_key(|sequence| sequence.wrapping_sub(self.next_sequence))?;
            self.pending.remove(&sequence)?
        };

        self.next_sequence = packet.sequence.wrapping_add(1);
        self.gap_started = if self.pending.is_empty() {
            None
        } else {
            Some(now)
        };
        Some(packet)
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        if self.pending.is_empty() || self.pending.contains_key(&self.next_sequence) {
            return None;
        }
        self.gap_started
            .and_then(|started| started.checked_add(RTP_REORDER_GAP))
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

pub fn dtmf_event(digit: char) -> Result<u8, CallError> {
    match digit {
        '0'..='9' => Ok(digit as u8 - b'0'),
        '*' => Ok(10),
        '#' => Ok(11),
        _ => Err(CallError::InvalidDtmfDigit),
    }
}

pub fn dtmf_payload(event: u8, duration: u16, end: bool) -> [u8; 4] {
    let mut payload = [event, 10, 0, 0];
    if end {
        payload[1] |= 0x80;
    }
    payload[2..].copy_from_slice(&duration.to_be_bytes());
    payload
}

/// Build the RFC 4733 send schedule for one key press. The caller owns the
/// RTP socket and packet timing; Rust owns payload bytes and duration
/// progression. `duration_scale` is 1 for an 8 kHz clock or 2 for 16 kHz.
pub fn dtmf_event_schedule(event: u8, duration_scale: u8) -> Result<Vec<[u8; 4]>, CallError> {
    if event > 11 || !matches!(duration_scale, 1 | 2) {
        return Err(CallError::InvalidDtmfDigit);
    }
    Ok([160_u16, 320, 480, 640, 800, 800, 800]
        .into_iter()
        .enumerate()
        .map(|(index, duration)| {
            dtmf_payload(event, duration * u16::from(duration_scale), index >= 4)
        })
        .collect())
}

/// Build a complete RFC 4733 RTP datagram batch for one key press. This is a
/// control operation, not the 20 ms audio hot path; the caller still owns the
/// UDP socket and pacing. Sequence and timestamp state are returned so the
/// call runtime can continue its shared audio sequence space.
pub fn dtmf_rtp_schedule(
    event: u8,
    duration_scale: u8,
    payload_type: u8,
    start_sequence: u16,
    start_timestamp: u32,
    ssrc: u32,
) -> Result<(Vec<Vec<u8>>, u16, u32), CallError> {
    if payload_type > 127 {
        return Err(CallError::InvalidDtmfDigit);
    }
    let payloads = dtmf_event_schedule(event, duration_scale)?;
    let mut packets = Vec::with_capacity(payloads.len());
    for (index, payload) in payloads.iter().enumerate() {
        let mut packet = vec![0_u8; RTP_HEADER_LENGTH + payload.len()];
        packet[0] = 0x80;
        packet[1] = payload_type | if index == 0 { 0x80 } else { 0 };
        packet[2..4].copy_from_slice(&start_sequence.wrapping_add(index as u16).to_be_bytes());
        packet[4..8].copy_from_slice(&start_timestamp.to_be_bytes());
        packet[8..12].copy_from_slice(&ssrc.to_be_bytes());
        packet[RTP_HEADER_LENGTH..].copy_from_slice(payload);
        packets.push(packet);
    }
    let next_sequence = start_sequence.wrapping_add(payloads.len() as u16);
    let next_timestamp = start_timestamp.wrapping_add(1_120_u32 * u32::from(duration_scale));
    Ok((packets, next_sequence, next_timestamp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_invite_tracks_phase_and_retransmissions() {
        let mut call = IncomingCallTracker::begin(11).unwrap();
        assert_eq!(call.phase(), IncomingCallPhase::Trying);
        assert_eq!(
            call.retransmission(11, true),
            IncomingInviteAdmission::Accept
        );
        assert_eq!(call.retransmission(12, true), IncomingInviteAdmission::Busy);

        call.ringing().unwrap();
        assert_eq!(
            call.retransmission(11, false),
            IncomingInviteAdmission::ResendRinging
        );
        call.answered().unwrap();
        assert_eq!(
            call.retransmission(11, false),
            IncomingInviteAdmission::ResendAnswer
        );
        call.acknowledged().unwrap();
        assert_eq!(
            call.retransmission(11, false),
            IncomingInviteAdmission::ResendAnswer
        );
        call.end();
        assert_eq!(call.retransmission(11, true), IncomingInviteAdmission::Busy);
    }

    #[test]
    fn incoming_invite_rejects_invalid_phase_transitions() {
        let mut call = IncomingCallTracker::begin(11).unwrap();
        assert_eq!(call.answered(), Err(CallError::InvalidTransition));
        assert_eq!(call.acknowledged(), Err(CallError::InvalidTransition));
        assert_eq!(
            IncomingCallTracker::begin(0),
            Err(CallError::InvalidResponse)
        );
        call.ringing().unwrap();
        assert_eq!(call.ringing(), Err(CallError::InvalidTransition));
    }

    #[test]
    fn incoming_cancel_requires_matching_invite_and_pre_answer_phase() {
        let mut call = IncomingCallTracker::begin(11).unwrap();
        assert_eq!(call.cancel(12), IncomingCancelDecision::Reject);
        assert_eq!(call.phase(), IncomingCallPhase::Trying);
        call.ringing().unwrap();
        assert_eq!(call.cancel(11), IncomingCancelDecision::Accept);
        assert_eq!(call.phase(), IncomingCallPhase::Ended);

        let mut answered = IncomingCallTracker::begin(13).unwrap();
        answered.ringing().unwrap();
        answered.answered().unwrap();
        assert_eq!(answered.cancel(13), IncomingCancelDecision::Reject);
        answered.acknowledged().unwrap();
        assert_eq!(answered.cancel(13), IncomingCancelDecision::Reject);
    }

    #[test]
    fn incoming_timeout_expires_only_the_matching_ringing_invite() {
        let mut call = IncomingCallTracker::begin(21).unwrap();
        assert_eq!(call.timeout(21), IncomingTimeoutDecision::Ignore);
        call.ringing().unwrap();
        assert_eq!(call.timeout(22), IncomingTimeoutDecision::Ignore);
        assert_eq!(call.phase(), IncomingCallPhase::Ringing);
        assert_eq!(call.timeout(21), IncomingTimeoutDecision::Expire);
        assert_eq!(call.phase(), IncomingCallPhase::Ended);

        let mut answered = IncomingCallTracker::begin(23).unwrap();
        answered.ringing().unwrap();
        answered.answered().unwrap();
        assert_eq!(answered.timeout(23), IncomingTimeoutDecision::Ignore);
    }

    #[test]
    fn incoming_bye_ends_only_the_matching_live_dialog() {
        let mut call = IncomingCallTracker::begin(31).unwrap();
        call.ringing().unwrap();
        call.answered().unwrap();
        call.acknowledged().unwrap();
        assert_eq!(call.bye(32), IncomingByeDecision::Reject);
        assert_eq!(call.phase(), IncomingCallPhase::Active);
        assert_eq!(call.bye(31), IncomingByeDecision::Accept);
        assert_eq!(call.phase(), IncomingCallPhase::Ended);
        assert_eq!(call.bye(31), IncomingByeDecision::Reject);
    }

    #[test]
    fn incoming_local_hangup_rejects_ringing_or_sends_bye_after_answer() {
        let mut ringing = IncomingCallTracker::begin(41).unwrap();
        ringing.ringing().unwrap();
        assert_eq!(
            ringing.local_hangup(41, 0),
            Ok(IncomingLocalHangupDecision::Reject)
        );
        assert_eq!(ringing.phase(), IncomingCallPhase::Ended);

        let mut active = IncomingCallTracker::begin(42).unwrap();
        active.ringing().unwrap();
        active.answered().unwrap();
        assert_eq!(
            active.local_hangup(42, 77),
            Ok(IncomingLocalHangupDecision::Bye { cseq: 77 })
        );
        assert_eq!(active.phase(), IncomingCallPhase::Ended);
        assert_eq!(active.acknowledged(), Ok(()));
        assert_eq!(active.phase(), IncomingCallPhase::Ended);

        let mut mismatched = IncomingCallTracker::begin(43).unwrap();
        mismatched.ringing().unwrap();
        assert_eq!(
            mismatched.local_hangup(44, 0),
            Err(CallError::ResponseTransactionMismatch)
        );
        assert_eq!(mismatched.phase(), IncomingCallPhase::Ringing);
    }

    #[test]
    fn local_cancel_treats_487_as_successful_end() {
        let mut call = OutgoingCallTracker::begin(7).unwrap();
        call.cancel().unwrap();
        assert_eq!(
            call.accept_invite_response(487, 7, false, None).unwrap(),
            InviteAction::AckAndEnd
        );
        assert_eq!(call.phase(), CallPhase::Ended);
        assert_eq!(
            call.accept_invite_response(487, 7, false, None).unwrap(),
            InviteAction::AckOnly
        );
    }

    #[test]
    fn hangup_uses_tracker_phase_and_is_idempotent() {
        let mut ringing = OutgoingCallTracker::begin(5).unwrap();
        assert_eq!(
            ringing.hangup(),
            Ok(OutgoingHangupAction::SendCancel { cseq: 5 })
        );
        assert_eq!(ringing.phase(), CallPhase::Cancelling);
        assert_eq!(ringing.hangup(), Ok(OutgoingHangupAction::Ignore));
        assert_eq!(
            ringing.accept_invite_response(487, 5, false, None).unwrap(),
            InviteAction::AckAndEnd
        );
        assert_eq!(ringing.hangup(), Ok(OutgoingHangupAction::Ignore));

        let mut active = OutgoingCallTracker::begin(6).unwrap();
        active.accept_invite_response(200, 6, true, None).unwrap();
        assert_eq!(
            active.hangup(),
            Ok(OutgoingHangupAction::SendBye { cseq: 7 })
        );
        assert_eq!(active.phase(), CallPhase::Terminating);
        assert_eq!(active.hangup(), Ok(OutgoingHangupAction::Ignore));
        assert_eq!(
            active.accept_invite_response(200, 6, true, None).unwrap(),
            InviteAction::AckOnly
        );
    }

    #[test]
    fn outgoing_tracker_owns_monotonic_dialog_cseq_allocation() {
        let mut call = OutgoingCallTracker::begin(41).unwrap();
        assert_eq!(call.next_local_cseq(), Ok(42));
        call.accept_invite_response(200, 41, true, None).unwrap();
        assert_eq!(
            call.hangup(),
            Ok(OutgoingHangupAction::SendBye { cseq: 43 })
        );

        let mut exhausted = OutgoingCallTracker::begin(i32::MAX as u32).unwrap();
        assert_eq!(
            exhausted.next_local_cseq(),
            Err(CallError::InvalidTransition)
        );
        assert!(matches!(
            OutgoingCallTracker::begin(i32::MAX as u32 + 1),
            Err(CallError::InvalidTransition)
        ));
    }

    #[test]
    fn raced_200_after_cancel_is_acked_then_ended_with_bye() {
        let mut call = OutgoingCallTracker::begin(7).unwrap();
        call.cancel().unwrap();
        assert_eq!(
            call.accept_invite_response(200, 7, true, None).unwrap(),
            InviteAction::AckAndBye
        );
    }

    #[test]
    fn network_487_is_acked_then_reported_as_failure() {
        let mut call = OutgoingCallTracker::begin(7).unwrap();
        assert_eq!(
            call.accept_invite_response(487, 7, false, None).unwrap(),
            InviteAction::AckAndFail { status: 487 }
        );
        assert_eq!(call.phase(), CallPhase::Failed);
    }

    #[test]
    fn reliable_early_media_is_pracked_once() {
        let mut call = OutgoingCallTracker::begin(7).unwrap();
        assert_eq!(
            call.accept_invite_response(183, 7, true, Some(1)).unwrap(),
            InviteAction::SendPrack {
                start_early_media: true
            }
        );
        assert_eq!(
            call.accept_invite_response(183, 7, true, Some(1)).unwrap(),
            InviteAction::Wait
        );
        assert_eq!(
            call.accept_invite_response(200, 7, true, None).unwrap(),
            InviteAction::AckAndStartMedia
        );
        assert_eq!(
            call.accept_invite_response(200, 7, true, None).unwrap(),
            InviteAction::AckOnly
        );
    }

    #[test]
    fn reliable_answer_can_be_reused_by_bodyless_final_response() {
        let mut call = OutgoingCallTracker::begin(7).unwrap();
        call.accept_invite_response(183, 7, true, Some(8)).unwrap();
        assert_eq!(
            call.accept_invite_response(200, 7, false, None).unwrap(),
            InviteAction::AckAndStartMedia
        );
        assert_eq!(call.phase(), CallPhase::Active);
    }

    #[test]
    fn rseq_gap_does_not_consume_answer_or_advance_sequence() {
        let mut call = OutgoingCallTracker::begin(7).unwrap();
        call.accept_invite_response(181, 7, false, Some(8)).unwrap();
        assert_eq!(
            call.accept_invite_response(183, 7, true, Some(10)).unwrap(),
            InviteAction::Wait
        );
        assert_eq!(
            call.accept_invite_response(182, 7, false, Some(9)).unwrap(),
            InviteAction::SendPrack {
                start_early_media: false
            }
        );
        assert_eq!(
            call.accept_invite_response(200, 7, false, None).unwrap(),
            InviteAction::AckAndFail { status: 200 }
        );
    }

    #[test]
    fn cancel_still_pracks_but_does_not_start_media() {
        let mut call = OutgoingCallTracker::begin(7).unwrap();
        call.cancel().unwrap();
        assert_eq!(
            call.accept_invite_response(183, 7, true, Some(1)).unwrap(),
            InviteAction::SendPrack {
                start_early_media: false
            }
        );
        assert_eq!(call.phase(), CallPhase::Cancelling);
    }

    #[test]
    fn rtp_parser_strips_csrc_extension_and_padding() {
        let packet = [
            0xb1, 0x88, 0, 9, 0, 0, 0, 10, 0, 0, 0, 11, 1, 2, 3, 4, 0xbe, 0xde, 0, 1, 9, 8, 7, 6,
            0xaa, 0xbb, 0, 2,
        ];
        let parsed = RtpPacket::parse(&packet).unwrap();
        assert!(parsed.marker);
        assert_eq!(parsed.payload_type, 8);
        assert_eq!(parsed.sequence, 9);
        assert_eq!(parsed.payload, [0xaa, 0xbb]);
    }

    #[test]
    fn sequence_tracker_bounds_concealment_and_resets_ssrc() {
        let mut tracker = SequenceTracker::default();
        assert_eq!(tracker.observe(1, 10), SequenceDisposition::First);
        assert_eq!(tracker.observe(1, 12), SequenceDisposition::Missing(1));
        assert_eq!(tracker.observe(1, 12), SequenceDisposition::DuplicateOrLate);
        assert_eq!(tracker.observe(1, 30), SequenceDisposition::Missing(3));
        assert_eq!(tracker.observe(2, 1), SequenceDisposition::StreamRestart);
    }

    #[test]
    fn rtp_reorder_repairs_sequence_wrap_and_bounds_gap_wait() {
        let now = Instant::now();
        let packet = |sequence, ssrc| RtpPacket {
            marker: false,
            payload_type: 96,
            sequence,
            timestamp: u32::from(sequence),
            ssrc,
            payload: &[0],
        };
        let mut reorder = RtpReorder::default();
        assert!(reorder.push(&packet(65_534, 1), now));
        assert_eq!(reorder.pop(now).unwrap().sequence, 65_534);

        assert!(reorder.push(&packet(0, 1), now));
        assert_eq!(reorder.pop(now + Duration::from_millis(20)), None);
        assert_eq!(reorder.next_deadline(), Some(now + RTP_REORDER_GAP));
        assert!(reorder.push(&packet(65_535, 1), now + Duration::from_millis(20)));
        assert_eq!(
            reorder
                .pop(now + Duration::from_millis(20))
                .unwrap()
                .sequence,
            65_535
        );
        assert_eq!(
            reorder
                .pop(now + Duration::from_millis(20))
                .unwrap()
                .sequence,
            0
        );
        assert!(!reorder.push(&packet(0, 1), now));

        assert!(reorder.push(&packet(2, 1), now));
        assert_eq!(
            reorder
                .pop(now + Duration::from_millis(60))
                .unwrap()
                .sequence,
            2
        );
        assert!(reorder.push(&packet(10, 2), now));
        assert_eq!(reorder.pop(now).unwrap().ssrc, 2);
        assert_eq!(reorder.pending_len(), 0);
    }

    #[test]
    fn rtp_reorder_drops_old_packets_and_caps_future_queue_at_32() {
        let now = Instant::now();
        let packet = |sequence| RtpPacket {
            marker: false,
            payload_type: 96,
            sequence,
            timestamp: 0,
            ssrc: 7,
            payload: &[1, 2, 3],
        };
        let mut reorder = RtpReorder::default();
        assert!(reorder.push(&packet(5), now));
        assert_eq!(reorder.pop(now).unwrap().sequence, 5);
        assert!(!reorder.push(&packet(4), now));
        for sequence in 7..39 {
            assert!(reorder.push(&packet(sequence), now));
        }
        assert_eq!(reorder.pending_len(), RTP_REORDER_CAPACITY);
        assert!(!reorder.push(&packet(39), now));
        assert_eq!(reorder.pending_len(), RTP_REORDER_CAPACITY);
    }

    #[test]
    fn dtmf_matches_rfc4733_events_and_end_bit() {
        assert_eq!(dtmf_event('9').unwrap(), 9);
        assert_eq!(dtmf_event('*').unwrap(), 10);
        assert_eq!(dtmf_event('#').unwrap(), 11);
        assert_eq!(dtmf_payload(1, 800, true), [1, 0x8a, 0x03, 0x20]);
        assert_eq!(dtmf_event('A').unwrap_err(), CallError::InvalidDtmfDigit);
    }

    #[test]
    fn dtmf_schedule_preserves_8k_and_16k_duration_semantics() {
        let at_16khz = dtmf_event_schedule(11, 2).unwrap();
        assert_eq!(at_16khz.len(), 7);
        assert_eq!(at_16khz[0], [11, 10, 1, 64]);
        assert_eq!(at_16khz[3], [11, 10, 5, 0]);
        assert_eq!(at_16khz[4], [11, 0x8a, 6, 64]);
        assert_eq!(at_16khz[6], at_16khz[4]);

        let at_8khz = dtmf_event_schedule(11, 1).unwrap();
        assert_eq!(at_8khz[0], [11, 10, 0, 160]);
        assert_eq!(at_8khz[4], [11, 0x8a, 3, 32]);
        assert!(dtmf_event_schedule(12, 1).is_err());
        assert!(dtmf_event_schedule(1, 3).is_err());
    }

    #[test]
    fn dtmf_rtp_schedule_builds_complete_packets_and_wraps_counters() {
        let (packets, next_sequence, next_timestamp) =
            dtmf_rtp_schedule(11, 2, 110, u16::MAX - 2, u32::MAX - 100, 0x1020_3040).unwrap();
        assert_eq!(packets.len(), 7);
        assert_eq!(next_sequence, 4);
        assert_eq!(next_timestamp, (u32::MAX - 100).wrapping_add(2_240));
        for (index, packet) in packets.iter().enumerate() {
            assert_eq!(packet.len(), RTP_HEADER_LENGTH + 4);
            assert_eq!(packet[0], 0x80);
            assert_eq!(packet[1], 110 | if index == 0 { 0x80 } else { 0 });
            assert_eq!(
                u16::from_be_bytes([packet[2], packet[3]]),
                (u16::MAX - 2).wrapping_add(index as u16)
            );
            assert_eq!(
                u32::from_be_bytes(packet[4..8].try_into().unwrap()),
                u32::MAX - 100
            );
            assert_eq!(
                u32::from_be_bytes(packet[8..12].try_into().unwrap()),
                0x1020_3040
            );
            assert_eq!(packet[12], 11);
            assert_eq!(packet[13] & 0x7f, 10);
            assert_eq!((packet[13] & 0x80 != 0), index >= 4);
        }
    }

    #[test]
    fn dtmf_rtp_schedule_rejects_payload_type_outside_rtp_range() {
        assert!(dtmf_rtp_schedule(1, 2, 128, 0, 0, 1).is_err());
    }
}
