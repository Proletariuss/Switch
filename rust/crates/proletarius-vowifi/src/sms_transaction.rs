//! IMS SMS MESSAGE construction and submission transaction lifecycle.
//!
//! This module deliberately owns no socket, SIM handle, persistent store, or
//! retry loop. The registered IMS session actor supplies unique transaction
//! identities, writes the returned SIP frame once, and feeds responses here.
//! A timeout after submission becomes an explicit unknown result; callers
//! must reconcile it and must not silently replay the SMS.

use std::{collections::BTreeSet, fmt, net::IpAddr};

use proletarius_sip::{Header, Headers, Message, MessageData, Method, Parser, Request, Uri};

/// Result of the SIP MESSAGE carrying an RP-ACK, not an outgoing SMS delivery result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryReportState {
    Pending,
    Accepted,
    Rejected(u16),
    OutcomeUnknown,
}

/// Retain this handle in the registered session until a correlated final event.
/// Timeout does not authorize replay of the report or removal of the archived SMS.
pub struct DeliveryReportSubmission {
    ticket: crate::sip_session_runtime::ResponseTicket,
    state: DeliveryReportState,
}

impl DeliveryReportSubmission {
    pub fn new(ticket: crate::sip_session_runtime::ResponseTicket) -> Self {
        Self {
            ticket,
            state: DeliveryReportState::Pending,
        }
    }

    pub fn ticket(&self) -> crate::sip_session_runtime::ResponseTicket {
        self.ticket
    }

    pub fn state(&self) -> DeliveryReportState {
        self.state
    }

    pub fn observe(&mut self, event: &crate::sip_session_runtime::RegisteredSipEvent) -> bool {
        use crate::{
            registration_coordinator::SipConnectionId,
            sip_session_runtime::{RegisteredSipEvent, RegisteredSipMessage},
        };
        if self.state != DeliveryReportState::Pending {
            return false;
        }
        let next = match event {
            RegisteredSipEvent::ResponseTimedOut { ticket } if *ticket == self.ticket => {
                DeliveryReportState::OutcomeUnknown
            }
            RegisteredSipEvent::Message {
                connection: SipConnectionId::Primary,
                message,
                response_ticket: Some(ticket),
            } if *ticket == self.ticket => {
                let RegisteredSipMessage::Response { status, .. } = message.as_ref() else {
                    return false;
                };
                match status {
                    200..=299 => DeliveryReportState::Accepted,
                    300..=699 => DeliveryReportState::Rejected(*status),
                    _ => return false,
                }
            }
            _ => return false,
        };
        self.state = next;
        true
    }

    /// Session teardown after a confirmed write cannot prove remote acceptance.
    pub fn disconnected(&mut self) {
        if self.state == DeliveryReportState::Pending {
            self.state = DeliveryReportState::OutcomeUnknown;
        }
    }
}

/// Match the existing inbound report policy: P-Asserted-Identity supplies
/// the IP-SM-GW target; From is not a substitute for this network identity.
pub fn incoming_delivery_report_target(headers: &Headers) -> Result<String, SmsTransactionError> {
    let mut values = headers.all("P-Asserted-Identity");
    let value = values
        .next()
        .ok_or(SmsTransactionError::InvalidContext("p_asserted_identity"))?
        .value()
        .trim();
    if value.len() > 4096 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(SmsTransactionError::InvalidContext("p_asserted_identity"));
    }
    let address = if let Some((_, rest)) = value.split_once('<') {
        rest.split_once('>')
            .ok_or(SmsTransactionError::InvalidContext("p_asserted_identity"))?
            .0
            .trim()
    } else {
        value
            .split(',')
            .next()
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
    };
    let (scheme, _) = address
        .split_once(':')
        .ok_or(SmsTransactionError::InvalidContext("p_asserted_identity"))?;
    if !scheme.eq_ignore_ascii_case("sip") && !scheme.eq_ignore_ascii_case("sips") {
        return Err(SmsTransactionError::InvalidContext("p_asserted_identity"));
    }
    let uri = Uri::parse(address)?;
    if uri.wildcard || uri.host.is_empty() {
        return Err(SmsTransactionError::InvalidContext("p_asserted_identity"));
    }
    Ok(uri.to_string())
}

const MAX_SMS_RPDU_LENGTH: usize = 255;
const MAX_ROUTES: usize = 8;

#[derive(Debug)]
pub enum SmsTransactionError {
    InvalidContext(&'static str),
    InvalidRpdu,
    InvalidSipMessage,
    Sip(proletarius_sip::Error),
    InvalidBatchSize,
    InvalidTransition,
}

impl fmt::Display for SmsTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidContext(field) => write!(formatter, "invalid IMS SMS context: {field}"),
            Self::InvalidRpdu => formatter.write_str("invalid SMS-SUBMIT RP-DATA body"),
            Self::InvalidSipMessage => formatter.write_str("invalid SIP MESSAGE response"),
            Self::Sip(error) => write!(formatter, "SIP message construction failed: {error}"),
            Self::InvalidBatchSize => formatter.write_str("SMS batch must contain 1..=255 parts"),
            Self::InvalidTransition => {
                formatter.write_str("invalid IMS SMS transaction transition")
            }
        }
    }
}

impl std::error::Error for SmsTransactionError {}

impl From<proletarius_sip::Error> for SmsTransactionError {
    fn from(error: proletarius_sip::Error) -> Self {
        Self::Sip(error)
    }
}

/// Immutable signaling facts captured from one registered IMS generation.
/// The caller creates fresh Call-ID, branch and From-tag values for each part.
pub struct SmsRequestContext<'a> {
    pub target: &'a str,
    pub public_identity: &'a str,
    pub call_id: &'a str,
    pub from_tag: &'a str,
    pub via_branch: &'a str,
    pub local_host: &'a str,
    pub local_port: u16,
    pub cseq: u32,
    pub route_headers: &'a [&'a str],
    pub security_verify: Option<&'a str>,
    pub access_network_info: Option<&'a str>,
}

/// Signaling context for the RP-ACK delivery report sent after a mobile-
/// terminated SIP MESSAGE has been durably accepted. The stream/session owner
/// supplies the registered identity and route snapshot; this crate owns the
/// exact SIP frame construction.
pub struct DeliveryReportRequestContext<'a> {
    pub target: &'a str,
    pub in_reply_to: &'a str,
    pub public_identity: &'a str,
    pub call_id: &'a str,
    pub from_tag: &'a str,
    pub via_branch: &'a str,
    pub local_host: &'a str,
    pub local_port: u16,
    pub cseq: u32,
    pub route_headers: &'a [&'a str],
    pub security_verify: Option<&'a str>,
    pub access_network_info: Option<&'a str>,
}

/// Build a single 3GPP IMS SMS SIP MESSAGE from an already encoded MO RP-DATA.
/// SIP header values are validated before serialization to prevent injection.
pub fn build_sms_message(
    context: &SmsRequestContext<'_>,
    rpdu: &[u8],
) -> Result<Vec<u8>, SmsTransactionError> {
    validate_rpdu(rpdu)?;
    if context.cseq == 0 || context.cseq >= (1 << 31) {
        return Err(SmsTransactionError::InvalidContext("cseq"));
    }
    if context.local_port == 0 {
        return Err(SmsTransactionError::InvalidContext("local_port"));
    }
    let target = parse_uri(context.target, true)?;
    let identity = parse_uri(context.public_identity, false)?;
    if !matches!(identity.scheme.as_str(), "sip" | "sips") {
        return Err(SmsTransactionError::InvalidContext("public_identity"));
    }
    validate_token(context.call_id, "call_id", 256)?;
    validate_token(context.from_tag, "from_tag", 128)?;
    validate_token(context.via_branch, "via_branch", 256)?;
    if context.route_headers.len() > MAX_ROUTES {
        return Err(SmsTransactionError::InvalidContext("route_headers"));
    }

    let parsed_local_host = context
        .local_host
        .parse::<IpAddr>()
        .map_err(|_| SmsTransactionError::InvalidContext("local_host"))?;
    let local_host = match parsed_local_host {
        IpAddr::V4(_) => context.local_host.trim().to_owned(),
        IpAddr::V6(_) => format!("[{}]", context.local_host),
    };
    if local_host.is_empty()
        || local_host.len() > 255
        || local_host.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(SmsTransactionError::InvalidContext("local_host"));
    }

    let mut headers = Headers::default();
    push_header(
        &mut headers,
        "Via",
        format!(
            "SIP/2.0/TCP {local_host}:{};branch={}",
            context.local_port, context.via_branch
        ),
    )?;
    push_header(
        &mut headers,
        "From",
        format!("<{identity}>;tag={}", context.from_tag),
    )?;
    push_header(&mut headers, "To", format!("<{target}>"))?;
    push_header(&mut headers, "Call-ID", context.call_id)?;
    push_header(&mut headers, "CSeq", format!("{} MESSAGE", context.cseq))?;
    push_header(&mut headers, "Max-Forwards", "70")?;
    for route in context.route_headers {
        push_header(&mut headers, "Route", *route)?;
    }
    push_header(
        &mut headers,
        "Accept-Contact",
        "*;+g.3gpp.smsip;require;explicit",
    )?;
    push_header(&mut headers, "Request-Disposition", "fork, parallel")?;
    push_header(
        &mut headers,
        "P-Preferred-Identity",
        format!("<{identity}>"),
    )?;
    if let Some(value) = context.access_network_info {
        push_header(&mut headers, "P-Access-Network-Info", value)?;
    }
    if let Some(value) = context.security_verify {
        push_header(&mut headers, "Security-Verify", value)?;
        push_header(&mut headers, "Require", "sec-agree")?;
        push_header(&mut headers, "Proxy-Require", "sec-agree")?;
    }
    push_header(&mut headers, "Content-Type", "application/vnd.3gpp.sms")?;
    push_header(&mut headers, "Content-Transfer-Encoding", "binary")?;

    let request = Request {
        method: Method::Message,
        uri: target,
        data: MessageData::new("SIP/2.0", headers, rpdu.to_vec()),
    };
    Ok(Message::Request(request).encode())
}

/// Build the network delivery report that acknowledges an inbound RP-DATA.
/// Unlike an originating SMS-SUBMIT, this request is correlated with the
/// received SIP MESSAGE through `In-Reply-To` and carries RP-ACK bytes.
pub fn build_sms_delivery_report(
    context: &DeliveryReportRequestContext<'_>,
    reference: u8,
) -> Result<Vec<u8>, SmsTransactionError> {
    if context.cseq == 0 || context.cseq >= (1 << 31) {
        return Err(SmsTransactionError::InvalidContext("cseq"));
    }
    if context.local_port == 0 {
        return Err(SmsTransactionError::InvalidContext("local_port"));
    }
    let target = parse_uri(context.target, false)?;
    if !matches!(target.scheme.as_str(), "sip" | "sips") {
        return Err(SmsTransactionError::InvalidContext("target"));
    }
    let identity = parse_uri(context.public_identity, false)?;
    if !matches!(identity.scheme.as_str(), "sip" | "sips") {
        return Err(SmsTransactionError::InvalidContext("public_identity"));
    }
    validate_token(context.call_id, "call_id", 256)?;
    validate_token(context.in_reply_to, "in_reply_to", 256)?;
    validate_token(context.from_tag, "from_tag", 128)?;
    validate_token(context.via_branch, "via_branch", 256)?;
    if context.route_headers.len() > MAX_ROUTES {
        return Err(SmsTransactionError::InvalidContext("route_headers"));
    }

    let parsed_local_host = context
        .local_host
        .parse::<IpAddr>()
        .map_err(|_| SmsTransactionError::InvalidContext("local_host"))?;
    let local_host = match parsed_local_host {
        IpAddr::V4(_) => context.local_host.trim().to_owned(),
        IpAddr::V6(_) => format!("[{}]", context.local_host),
    };
    if local_host.is_empty()
        || local_host.len() > 255
        || local_host.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(SmsTransactionError::InvalidContext("local_host"));
    }

    let mut headers = Headers::default();
    push_header(
        &mut headers,
        "Via",
        format!(
            "SIP/2.0/TCP {local_host}:{};branch={}",
            context.local_port, context.via_branch
        ),
    )?;
    push_header(
        &mut headers,
        "From",
        format!("<{identity}>;tag={}", context.from_tag),
    )?;
    push_header(&mut headers, "To", format!("<{target}>"))?;
    push_header(&mut headers, "Call-ID", context.call_id)?;
    push_header(&mut headers, "CSeq", format!("{} MESSAGE", context.cseq))?;
    push_header(&mut headers, "Max-Forwards", "70")?;
    for route in context.route_headers {
        push_header(&mut headers, "Route", *route)?;
    }
    push_header(&mut headers, "In-Reply-To", context.in_reply_to)?;
    push_header(
        &mut headers,
        "P-Preferred-Identity",
        format!("<{identity}>"),
    )?;
    if let Some(value) = context.access_network_info {
        push_header(&mut headers, "P-Access-Network-Info", value)?;
    }
    if let Some(value) = context.security_verify {
        push_header(&mut headers, "Security-Verify", value)?;
        push_header(&mut headers, "Require", "sec-agree")?;
        push_header(&mut headers, "Proxy-Require", "sec-agree")?;
    }
    push_header(&mut headers, "Content-Type", "application/vnd.3gpp.sms")?;
    push_header(&mut headers, "Content-Transfer-Encoding", "binary")?;

    let request = Request {
        method: Method::Message,
        uri: target,
        data: MessageData::new(
            "SIP/2.0",
            headers,
            proletarius_sms::build_ack(reference).to_vec(),
        ),
    };
    Ok(Message::Request(request).encode())
}

fn parse_uri(value: &str, allow_phone_target: bool) -> Result<Uri, SmsTransactionError> {
    let value = value.trim();
    let value = match (value.strip_prefix('<'), value.strip_suffix('>')) {
        (Some(inner), Some(_)) => inner
            .strip_suffix('>')
            .ok_or(SmsTransactionError::InvalidContext("uri"))?,
        (None, None) => value,
        _ => return Err(SmsTransactionError::InvalidContext("uri")),
    };
    if value.is_empty() || value.len() > 2_048 {
        return Err(SmsTransactionError::InvalidContext("uri"));
    }
    let has_sip_scheme = value
        .get(..4)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("sip:"));
    let has_sips_scheme = value
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("sips:"));
    let has_tel_scheme = value
        .get(..4)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("tel:"));
    let normalized = if allow_phone_target && !has_sip_scheme && !has_sips_scheme && !has_tel_scheme
    {
        format!("tel:{value}")
    } else {
        value.to_owned()
    };
    if normalized.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(SmsTransactionError::InvalidContext("uri"));
    }
    Uri::parse(&normalized).map_err(SmsTransactionError::Sip)
}

fn validate_token(
    value: &str,
    field: &'static str,
    max_len: usize,
) -> Result<(), SmsTransactionError> {
    if value.is_empty()
        || value.len() > max_len
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b';')
    {
        return Err(SmsTransactionError::InvalidContext(field));
    }
    Ok(())
}

fn push_header(
    headers: &mut Headers,
    name: impl Into<String>,
    value: impl Into<String>,
) -> Result<(), SmsTransactionError> {
    let name = name.into();
    let value = value.into();
    if value.len() > 16_384 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(SmsTransactionError::InvalidContext("header_value"));
    }
    headers.push(Header::new(name, value).map_err(SmsTransactionError::Sip)?);
    Ok(())
}

fn validate_rpdu(data: &[u8]) -> Result<u8, SmsTransactionError> {
    if data.len() < 7 || data.len() > MAX_SMS_RPDU_LENGTH || data[0] != 0x00 || data[2] != 0x00 {
        return Err(SmsTransactionError::InvalidRpdu);
    }
    let address_len = usize::from(data[3]);
    let length_offset = 4usize
        .checked_add(address_len)
        .ok_or(SmsTransactionError::InvalidRpdu)?;
    let tpdu_len = usize::from(
        *data
            .get(length_offset)
            .ok_or(SmsTransactionError::InvalidRpdu)?,
    );
    let address = data
        .get(4..length_offset)
        .ok_or(SmsTransactionError::InvalidRpdu)?;
    if address.len() < 2
        || address[0] & 0x80 == 0
        || address[1..].iter().enumerate().any(|(index, octet)| {
            let low = octet & 0x0f;
            let high = octet >> 4;
            low > 9 || (high > 9 && !(high == 0x0f && index + 1 == address.len() - 1))
        })
    {
        return Err(SmsTransactionError::InvalidRpdu);
    }
    let tpdu_start = length_offset + 1;
    if address_len == 0
        || tpdu_len == 0
        || length_offset
            .checked_add(1)
            .and_then(|offset| offset.checked_add(tpdu_len))
            != Some(data.len())
    {
        return Err(SmsTransactionError::InvalidRpdu);
    }
    if data[tpdu_start] & 0x03 != 0x01 {
        return Err(SmsTransactionError::InvalidRpdu);
    }
    Ok(data[1])
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SmsPartIdentity {
    pub call_id: String,
    pub cseq: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SmsSubmissionStatus {
    Ready {
        accepted_parts: u16,
        total_parts: u16,
    },
    AwaitingResponse {
        identity: SmsPartIdentity,
        part: u16,
        accepted_parts: u16,
        total_parts: u16,
    },
    OutcomeUnknown {
        identity: SmsPartIdentity,
        part: u16,
        accepted_parts: u16,
        total_parts: u16,
    },
    Rejected {
        status: u16,
        accepted_parts: u16,
        total_parts: u16,
    },
    Accepted {
        parts: u16,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmsResponseDisposition {
    Ignored,
    Provisional,
    PartAccepted { completed: bool },
    Rejected { status: u16, accepted_parts: u16 },
    LateResultResolved { accepted: bool, accepted_parts: u16 },
}

/// One sequential multipart submission. A part is opened only after its
/// predecessor receives a final 2xx response. There is intentionally no retry
/// method for an `OutcomeUnknown` state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmsSubmission {
    status: SmsSubmissionStatus,
    seen_transactions: BTreeSet<SmsPartIdentity>,
}

impl SmsSubmission {
    pub fn new(total_parts: usize) -> Result<Self, SmsTransactionError> {
        if !(1..=255).contains(&total_parts) {
            return Err(SmsTransactionError::InvalidBatchSize);
        }
        Ok(Self {
            status: SmsSubmissionStatus::Ready {
                accepted_parts: 0,
                total_parts: total_parts as u16,
            },
            seen_transactions: BTreeSet::new(),
        })
    }

    #[must_use]
    pub fn status(&self) -> &SmsSubmissionStatus {
        &self.status
    }

    /// Reserve this transaction immediately before its one write attempt.
    /// Any failed or cancelled write remains uncertain because bytes may have
    /// reached the peer; callers must not try the part again.
    pub fn mark_submitted(
        &mut self,
        call_id: impl Into<String>,
        cseq: u32,
    ) -> Result<(), SmsTransactionError> {
        let SmsSubmissionStatus::Ready {
            accepted_parts,
            total_parts,
        } = self.status
        else {
            return Err(SmsTransactionError::InvalidTransition);
        };
        let call_id = call_id.into();
        validate_token(&call_id, "call_id", 256)?;
        if cseq == 0 || cseq >= (1 << 31) || accepted_parts >= total_parts {
            return Err(SmsTransactionError::InvalidTransition);
        }
        let identity = SmsPartIdentity { call_id, cseq };
        if !self.seen_transactions.insert(identity.clone()) {
            return Err(SmsTransactionError::InvalidTransition);
        }
        self.status = SmsSubmissionStatus::AwaitingResponse {
            identity,
            part: accepted_parts + 1,
            accepted_parts,
            total_parts,
        };
        Ok(())
    }

    /// Freeze a submitted part after timeout or disconnect. A later matching
    /// response may still resolve it, but callers cannot start it again.
    pub fn mark_outcome_unknown(&mut self) -> Result<(), SmsTransactionError> {
        let SmsSubmissionStatus::AwaitingResponse {
            identity,
            part,
            accepted_parts,
            total_parts,
        } = &self.status
        else {
            return Err(SmsTransactionError::InvalidTransition);
        };
        self.status = SmsSubmissionStatus::OutcomeUnknown {
            identity: identity.clone(),
            part: *part,
            accepted_parts: *accepted_parts,
            total_parts: *total_parts,
        };
        Ok(())
    }

    /// Observe one complete synthetic or transport-delivered SIP response.
    /// Unrelated Call-ID/CSeq responses are ignored so another SIP
    /// transaction's event cannot acknowledge this SMS.
    pub fn observe_response(
        &mut self,
        wire: &[u8],
    ) -> Result<SmsResponseDisposition, SmsTransactionError> {
        let (message, consumed) = Parser::default()
            .parse(wire, true)
            .map_err(|_| SmsTransactionError::InvalidSipMessage)?;
        if consumed != wire.len() {
            return Err(SmsTransactionError::InvalidSipMessage);
        }
        let Message::Response(response) = message else {
            return Err(SmsTransactionError::InvalidSipMessage);
        };
        self.observe_response_headers(response.status, &response.data.headers)
    }

    /// Consume the runtime's already-parsed response without rebuilding or
    /// cloning the frame/body. The caller retains the session/connection gate.
    pub fn observe_response_headers(
        &mut self,
        status: u16,
        headers: &Headers,
    ) -> Result<SmsResponseDisposition, SmsTransactionError> {
        if !(100..=699).contains(&status)
            || headers.all("Call-ID").count() != 1
            || headers.all("CSeq").count() != 1
        {
            return Err(SmsTransactionError::InvalidSipMessage);
        }
        let response_call_id = headers.get("Call-ID").unwrap().value();
        let mut fields = headers
            .get("CSeq")
            .unwrap()
            .value()
            .split_ascii_whitespace();
        let number = fields
            .next()
            .ok_or(SmsTransactionError::InvalidSipMessage)?;
        if !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(SmsTransactionError::InvalidSipMessage);
        }
        let response_cseq: u32 = number
            .parse()
            .map_err(|_| SmsTransactionError::InvalidSipMessage)?;
        let method = fields
            .next()
            .ok_or(SmsTransactionError::InvalidSipMessage)?;
        if response_cseq >= (1 << 31) || fields.next().is_some() || response_call_id.is_empty() {
            return Err(SmsTransactionError::InvalidSipMessage);
        }

        let (identity, accepted_parts, total_parts, was_unknown) = match &self.status {
            SmsSubmissionStatus::AwaitingResponse {
                identity,
                accepted_parts,
                total_parts,
                ..
            } => (identity, *accepted_parts, *total_parts, false),
            SmsSubmissionStatus::OutcomeUnknown {
                identity,
                accepted_parts,
                total_parts,
                ..
            } => (identity, *accepted_parts, *total_parts, true),
            _ => return Ok(SmsResponseDisposition::Ignored),
        };
        if response_call_id != identity.call_id
            || response_cseq != identity.cseq
            || method != "MESSAGE"
        {
            return Ok(SmsResponseDisposition::Ignored);
        }
        if status < 200 {
            return Ok(SmsResponseDisposition::Provisional);
        }
        if (200..300).contains(&status) {
            let accepted = accepted_parts + 1;
            let completed = accepted == total_parts;
            self.status = if completed {
                SmsSubmissionStatus::Accepted { parts: accepted }
            } else {
                SmsSubmissionStatus::Ready {
                    accepted_parts: accepted,
                    total_parts,
                }
            };
            return Ok(if was_unknown {
                SmsResponseDisposition::LateResultResolved {
                    accepted: true,
                    accepted_parts: accepted,
                }
            } else {
                SmsResponseDisposition::PartAccepted { completed }
            });
        }
        self.status = SmsSubmissionStatus::Rejected {
            status: status,
            accepted_parts,
            total_parts,
        };
        Ok(if was_unknown {
            SmsResponseDisposition::LateResultResolved {
                accepted: false,
                accepted_parts,
            }
        } else {
            SmsResponseDisposition::Rejected {
                status: status,
                accepted_parts,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proletarius_sip::{Message, Parser, call_id, cseq};

    fn rpdu(reference: u8) -> Vec<u8> {
        // RP-DATA(SUBMIT): message reference, empty RP-OA, SMSC RP-DA and one
        // byte TPDU. It is synthetic and never sent to a device or network.
        vec![0x00, reference, 0x00, 0x02, 0x81, 0x21, 0x01, 0x01]
    }

    fn context<'a>(call_id: &'a str, cseq: u32) -> SmsRequestContext<'a> {
        SmsRequestContext {
            target: "+8613800138000",
            public_identity: "sip:+8613800000000@ims.example.test",
            call_id,
            from_tag: "tag-fixture",
            via_branch: "z9hG4bK-fixture",
            local_host: "192.0.2.10",
            local_port: 5060,
            cseq,
            route_headers: &["<sip:pcscf.example.test;lr>"],
            security_verify: Some("ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc"),
            access_network_info: Some("IEEE-802.11;i-wlan-node-id=fixture"),
        }
    }

    fn delivery_report_context<'a>(
        call_id: &'a str,
        cseq: u32,
    ) -> DeliveryReportRequestContext<'a> {
        DeliveryReportRequestContext {
            target: "sip:gw@ims.example.test",
            in_reply_to: "incoming-message@example.test",
            public_identity: "sip:+8613800000000@ims.example.test",
            call_id,
            from_tag: "report-tag",
            via_branch: "z9hG4bK-report",
            local_host: "192.0.2.10",
            local_port: 5060,
            cseq,
            route_headers: &["<sip:scscf.example.test;lr>"],
            security_verify: Some("ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc"),
            access_network_info: Some("IEEE-802.11;i-wlan-node-id=fixture"),
        }
    }

    fn response(call_id: &str, cseq: u32, method: &str, status: u16) -> Vec<u8> {
        format!(
            "SIP/2.0 {status} Fixture\r\nVia: SIP/2.0/TCP 192.0.2.10;branch=z9hG4bK-fixture\r\nFrom: <sip:a@ims.example.test>;tag=a\r\nTo: <sip:b@ims.example.test>;tag=b\r\nCall-ID: {call_id}\r\nCSeq: {cseq} {method}\r\nContent-Length: 0\r\n\r\n"
        )
        .into_bytes()
    }

    fn report_fixture() -> (
        crate::sip_response_registry::ResponseRegistry,
        crate::sip_session_runtime::ResponseTicket,
        Request,
    ) {
        let wire =
            build_sms_delivery_report(&delivery_report_context("report-test", 81), 42).unwrap();
        let (Message::Request(request), _) = Parser::default().parse(&wire, false).unwrap() else {
            panic!("request");
        };
        let mut registry = crate::sip_response_registry::ResponseRegistry::default();
        let ticket = registry
            .reserve(&request, std::time::Duration::from_secs(20))
            .unwrap();
        (registry, ticket, request)
    }

    fn report_response(
        request: &Request,
        status: u16,
    ) -> crate::sip_session_runtime::RegisteredSipMessage {
        crate::sip_session_runtime::RegisteredSipMessage::Response {
            status,
            reason: "fixture".into(),
            version: "SIP/2.0".into(),
            headers: request.data.headers.clone(),
            body: zeroize::Zeroizing::new(Vec::new()),
        }
    }

    #[test]
    fn incoming_report_route_uses_asserted_identity_not_from() {
        for (value, expected) in [
            ("<sip:gw@ims.example>", "sip:gw@ims.example"),
            (
                "\"Gateway\" <sips:gw@ims.example;transport=tcp>",
                "sips:gw@ims.example;transport=tcp",
            ),
            ("sip:gw@ims.example;tag=x, tel:+123", "sip:gw@ims.example"),
            (
                "<sip:first@ims.example>, <sip:second@ims.example>",
                "sip:first@ims.example",
            ),
        ] {
            let mut headers = Headers::default();
            headers.push(Header::new("From", "<sip:wrong@ims.example>").unwrap());
            headers.push(Header::new("P-Asserted-Identity", value).unwrap());
            assert_eq!(incoming_delivery_report_target(&headers).unwrap(), expected);
        }
        let mut headers = Headers::default();
        headers.push(Header::new("From", "<sip:wrong@ims.example>").unwrap());
        assert!(incoming_delivery_report_target(&headers).is_err());
        for value in [
            "tel:+123",
            "<sip:gw@ims.example",
            "https://ims.example",
            "",
            "sip:*",
        ] {
            let mut headers = Headers::default();
            headers.push(Header::new("P-Asserted-Identity", value).unwrap());
            assert!(incoming_delivery_report_target(&headers).is_err());
        }
        headers.push(Header::new("P-Asserted-Identity", "<sip:first@ims.example>").unwrap());
        headers.push(Header::new("P-Asserted-Identity", "<sip:second@ims.example>").unwrap());
        assert_eq!(
            incoming_delivery_report_target(&headers).unwrap(),
            "sip:first@ims.example"
        );
    }

    #[test]
    fn sms_and_report_builders_reject_out_of_range_cseq() {
        for number in [0, 1 << 31, u32::MAX] {
            assert!(build_sms_message(&context("sms", number), &rpdu(1)).is_err());
            assert!(
                build_sms_delivery_report(&delivery_report_context("report", number), 1).is_err()
            );
            let mut submission = SmsSubmission::new(1).unwrap();
            assert!(submission.mark_submitted("sms", number).is_err());
        }
        assert!(build_sms_message(&context("sms", (1 << 31) - 1), &rpdu(1)).is_ok());
        assert!(
            build_sms_delivery_report(&delivery_report_context("report", (1 << 31) - 1), 1).is_ok()
        );
    }

    #[test]
    fn report_final_response_requires_exact_registry_correlation_and_is_terminal() {
        use crate::{
            registration_coordinator::SipConnectionId, sip_session_runtime::RegisteredSipEvent,
        };
        for status in [200, 202, 403, 503] {
            let (mut registry, ticket, request) = report_fixture();
            let mut report = DeliveryReportSubmission::new(ticket);
            let provisional = report_response(&request, 100);
            let correlated = registry.correlate(SipConnectionId::Primary, &provisional);
            assert!(!report.observe(&RegisteredSipEvent::Message {
                connection: SipConnectionId::Primary,
                message: Box::new(provisional),
                response_ticket: correlated
            }));
            assert_eq!(report.state(), DeliveryReportState::Pending);
            let final_reply = report_response(&request, status);
            assert_eq!(
                registry.correlate(SipConnectionId::Incoming(6), &final_reply),
                None
            );
            assert!(!report.observe(&RegisteredSipEvent::Message {
                connection: SipConnectionId::Incoming(6),
                message: Box::new(report_response(&request, status)),
                response_ticket: Some(ticket)
            }));
            let correlated = registry.correlate(SipConnectionId::Primary, &final_reply);
            assert_eq!(correlated, Some(ticket));
            assert!(report.observe(&RegisteredSipEvent::Message {
                connection: SipConnectionId::Primary,
                message: Box::new(final_reply),
                response_ticket: correlated
            }));
            let expected = if status < 300 {
                DeliveryReportState::Accepted
            } else {
                DeliveryReportState::Rejected(status)
            };
            assert_eq!(report.state(), expected);
            report.disconnected();
            assert_eq!(report.state(), expected);
            assert!(!report.observe(&RegisteredSipEvent::ResponseTimedOut { ticket }));
        }
    }

    #[test]
    fn report_timeout_or_disconnect_remains_unknown_and_unrelated_events_are_ignored() {
        use crate::{
            registration_coordinator::SipConnectionId, sip_session_runtime::RegisteredSipEvent,
        };
        let (mut registry, ticket, request) = report_fixture();
        let mut other = request.clone();
        other
            .data
            .headers
            .set(Header::new("Call-ID", "other-report").unwrap());
        let other_ticket = registry
            .reserve(&other, std::time::Duration::from_secs(20))
            .unwrap();
        let mut report = DeliveryReportSubmission::new(ticket);
        assert!(!report.observe(&RegisteredSipEvent::ResponseTimedOut {
            ticket: other_ticket
        }));
        assert_eq!(report.state(), DeliveryReportState::Pending);
        assert!(report.observe(&RegisteredSipEvent::ResponseTimedOut { ticket }));
        assert_eq!(report.state(), DeliveryReportState::OutcomeUnknown);
        assert!(!report.observe(&RegisteredSipEvent::Message {
            connection: SipConnectionId::Primary,
            message: Box::new(report_response(&request, 200)),
            response_ticket: Some(ticket)
        }));
        let mut disconnected = DeliveryReportSubmission::new(other_ticket);
        disconnected.disconnected();
        assert_eq!(disconnected.state(), DeliveryReportState::OutcomeUnknown);
    }

    #[test]
    fn builds_a_bounded_binary_sms_message_with_matching_transaction_headers() {
        let wire = build_sms_message(&context("sms-fixture-1", 11), &rpdu(7)).unwrap();
        let (message, consumed) = Parser::default().parse(&wire, true).unwrap();
        assert_eq!(consumed, wire.len());
        assert_eq!(call_id(&message), Some("sms-fixture-1"));
        assert_eq!(cseq(&message), Some((11, "MESSAGE")));
        assert_eq!(message.body(), rpdu(7));
        assert_eq!(
            message.headers().get("Content-Type").unwrap().value(),
            "application/vnd.3gpp.sms"
        );
        assert_eq!(
            message
                .headers()
                .get("Content-Transfer-Encoding")
                .unwrap()
                .value(),
            "binary"
        );
        assert_eq!(
            message.headers().get("Security-Verify").unwrap().value(),
            "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc"
        );
    }

    #[test]
    fn builds_tel_uri_when_target_is_a_plain_number() {
        let wire = build_sms_message(&context("sms-fixture-2", 12), &rpdu(8)).unwrap();
        let (message, _) = Parser::default().parse(&wire, true).unwrap();
        let Message::Request(request) = message else {
            panic!("expected SIP request")
        };
        assert_eq!(request.uri.scheme, "tel");
        assert_eq!(request.uri.address(), "tel:+8613800138000");
    }

    #[test]
    fn builds_binary_delivery_report_with_in_reply_to_and_security_context() {
        let wire = build_sms_delivery_report(&delivery_report_context("report-1", 19), 42).unwrap();
        let (message, consumed) = Parser::default().parse(&wire, true).unwrap();
        assert_eq!(consumed, wire.len());
        let Message::Request(request) = message else {
            panic!("expected SIP request")
        };
        assert_eq!(request.method, Method::Message);
        assert_eq!(request.uri.address(), "sip:gw@ims.example.test");
        assert_eq!(
            request.data.headers.get("Call-ID").unwrap().value(),
            "report-1"
        );
        assert_eq!(
            request.data.headers.get("CSeq").unwrap().value(),
            "19 MESSAGE"
        );
        assert_eq!(request.data.body, [0x02, 42]);
        assert_eq!(
            request.data.headers.get("In-Reply-To").unwrap().value(),
            "incoming-message@example.test"
        );
        assert_eq!(
            request.data.headers.get("Route").unwrap().value(),
            "<sip:scscf.example.test;lr>"
        );
        assert_eq!(
            request.data.headers.get("Security-Verify").unwrap().value(),
            "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc"
        );
        assert_eq!(
            request
                .data
                .headers
                .get("P-Access-Network-Info")
                .unwrap()
                .value(),
            "IEEE-802.11;i-wlan-node-id=fixture"
        );
        assert!(request.data.headers.get("Accept-Contact").is_none());
        assert!(request.data.headers.get("Request-Disposition").is_none());
    }

    #[test]
    fn delivery_report_rejects_injected_correlation_and_invalid_route() {
        let mut bad = delivery_report_context("report-2", 20);
        bad.in_reply_to = "safe\r\nX-Injected: yes";
        assert!(build_sms_delivery_report(&bad, 1).is_err());
        let mut bad_target = delivery_report_context("report-2", 20);
        bad_target.target = "tel:+10000000000";
        assert!(build_sms_delivery_report(&bad_target, 1).is_err());
    }

    #[test]
    fn rejects_malformed_rpdu_and_header_injection() {
        assert!(matches!(
            build_sms_message(&context("sms-fixture-3", 13), &[0x00, 1, 0]),
            Err(SmsTransactionError::InvalidRpdu)
        ));
        let mut bad = context("sms-fixture-3", 13);
        bad.call_id = "safe\r\nX-Injected: yes";
        assert!(build_sms_message(&bad, &rpdu(9)).is_err());
        let mut bad_route = context("sms-fixture-3", 13);
        let bad_route_headers = ["<sip:valid.test;lr>\r\nX: injected"];
        bad_route.route_headers = &bad_route_headers;
        assert!(build_sms_message(&bad_route, &rpdu(9)).is_err());
    }

    #[test]
    fn response_dispatch_requires_exact_call_id_cseq_and_message_method() {
        let mut batch = SmsSubmission::new(1).unwrap();
        batch.mark_submitted("sms-fixture-4", 14).unwrap();
        assert_eq!(
            batch
                .observe_response(&response("other", 14, "MESSAGE", 202))
                .unwrap(),
            SmsResponseDisposition::Ignored
        );
        assert_eq!(
            batch
                .observe_response(&response("sms-fixture-4", 15, "MESSAGE", 202))
                .unwrap(),
            SmsResponseDisposition::Ignored
        );
        assert_eq!(
            batch
                .observe_response(&response("sms-fixture-4", 14, "OPTIONS", 202))
                .unwrap(),
            SmsResponseDisposition::Ignored
        );
        assert_eq!(
            batch
                .observe_response(&response("SMS-FIXTURE-4", 14, "MESSAGE", 202))
                .unwrap(),
            SmsResponseDisposition::Ignored
        );
        assert_eq!(
            batch
                .observe_response(&response("sms-fixture-4", 14, "MESSAGE", 100))
                .unwrap(),
            SmsResponseDisposition::Provisional
        );
        assert_eq!(
            batch
                .observe_response(&response("sms-fixture-4", 14, "MESSAGE", 202))
                .unwrap(),
            SmsResponseDisposition::PartAccepted { completed: true }
        );
        assert_eq!(batch.status(), &SmsSubmissionStatus::Accepted { parts: 1 });
    }

    #[test]
    fn multipart_submission_waits_for_each_final_response() {
        let mut batch = SmsSubmission::new(2).unwrap();
        batch.mark_submitted("sms-part-1", 21).unwrap();
        assert_eq!(
            batch
                .observe_response(&response("sms-part-1", 21, "MESSAGE", 200))
                .unwrap(),
            SmsResponseDisposition::PartAccepted { completed: false }
        );
        assert!(matches!(
            batch.status(),
            SmsSubmissionStatus::Ready {
                accepted_parts: 1,
                total_parts: 2
            }
        ));
        batch.mark_submitted("sms-part-2", 22).unwrap();
        assert_eq!(
            batch
                .observe_response(&response("sms-part-2", 22, "MESSAGE", 202))
                .unwrap(),
            SmsResponseDisposition::PartAccepted { completed: true }
        );
    }

    #[test]
    fn timeout_freezes_submission_but_a_late_exact_response_can_resolve_it() {
        let mut batch = SmsSubmission::new(2).unwrap();
        batch.mark_submitted("sms-uncertain", 31).unwrap();
        batch.mark_outcome_unknown().unwrap();
        assert!(matches!(
            batch.status(),
            SmsSubmissionStatus::OutcomeUnknown { .. }
        ));
        assert!(matches!(
            batch.mark_submitted("retry-must-not-start", 32),
            Err(SmsTransactionError::InvalidTransition)
        ));
        assert_eq!(
            batch
                .observe_response(&response("sms-uncertain", 31, "MESSAGE", 202))
                .unwrap(),
            SmsResponseDisposition::LateResultResolved {
                accepted: true,
                accepted_parts: 1
            }
        );
        assert!(matches!(
            batch.status(),
            SmsSubmissionStatus::Ready {
                accepted_parts: 1,
                total_parts: 2
            }
        ));
    }

    #[test]
    fn rejection_is_known_and_reports_previously_accepted_parts() {
        let mut batch = SmsSubmission::new(2).unwrap();
        batch.mark_submitted("sms-part-1", 41).unwrap();
        batch
            .observe_response(&response("sms-part-1", 41, "MESSAGE", 202))
            .unwrap();
        batch.mark_submitted("sms-part-2", 42).unwrap();
        assert_eq!(
            batch
                .observe_response(&response("sms-part-2", 42, "MESSAGE", 403))
                .unwrap(),
            SmsResponseDisposition::Rejected {
                status: 403,
                accepted_parts: 1
            }
        );
        assert!(matches!(
            batch.status(),
            SmsSubmissionStatus::Rejected {
                status: 403,
                accepted_parts: 1,
                total_parts: 2
            }
        ));
    }
}
