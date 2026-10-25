#![forbid(unsafe_code)]
pub mod aka_authorization;
pub mod aka_digest;
pub mod aka_exchange;
pub mod aka_nonce;
pub mod call_rejection;
pub mod call_target;
pub mod contact_header;
mod digest_parameters;
pub mod register_auth_progress;
pub mod register_fallback;
pub mod register_lease;
pub mod register_request;
pub mod register_retry;
pub mod registrar_candidates;
pub mod registration_connection;
pub mod registration_io;
pub mod security_esp;
pub mod security_offer;
pub mod security_policy;
pub mod security_slot;
pub mod security_transport;
pub mod sip_request;
pub mod stream;
pub mod ussd_dialog;
pub mod ussd_xml;

use std::fmt;

pub const IMS_PROTOCOL_CONTRACT: &str = "ims-sip-register-state-v1";
const MAX_SIP_MESSAGE_LENGTH: usize = 65_536;
const MAX_HEADER_COUNT: usize = 128;
const MAX_HEADER_LINE_LENGTH: usize = 8_192;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SipError {
    MessageTooLarge,
    MissingHeaderBoundary,
    InvalidStatusLine,
    InvalidStatusCode,
    InvalidHeader,
    HeaderLineTooLong,
    TooManyHeaders,
    DuplicateContentLength,
    DuplicateTransactionHeader(&'static str),
    InvalidContentLength,
    BodyLengthMismatch { declared: usize, actual: usize },
    MissingTransactionHeader(&'static str),
    InvalidCSeq,
    UnexpectedTransaction,
    InvalidTransition,
    MissingAuthenticationChallenge,
    MissingSecurityServer,
    ChallengeLimitExceeded,
    RegistrationRejected(u16),
    EntropyUnavailable,
}

impl fmt::Display for SipError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MessageTooLarge => {
                formatter.write_str("SIP message exceeds the configured limit")
            }
            Self::MissingHeaderBoundary => {
                formatter.write_str("SIP message has no header boundary")
            }
            Self::InvalidStatusLine => formatter.write_str("invalid SIP response status line"),
            Self::InvalidStatusCode => formatter.write_str("invalid SIP response status code"),
            Self::InvalidHeader => formatter.write_str("invalid SIP response header"),
            Self::HeaderLineTooLong => {
                formatter.write_str("SIP header line exceeds the configured limit")
            }
            Self::TooManyHeaders => formatter.write_str("SIP response has too many headers"),
            Self::DuplicateContentLength => {
                formatter.write_str("SIP response contains duplicate Content-Length")
            }
            Self::DuplicateTransactionHeader(name) => {
                write!(formatter, "SIP response contains duplicate {name}")
            }
            Self::InvalidContentLength => formatter.write_str("invalid SIP Content-Length"),
            Self::BodyLengthMismatch { declared, actual } => write!(
                formatter,
                "SIP body length mismatch: declared {declared}, actual {actual}"
            ),
            Self::MissingTransactionHeader(name) => {
                write!(formatter, "SIP response is missing {name}")
            }
            Self::InvalidCSeq => formatter.write_str("invalid SIP CSeq"),
            Self::UnexpectedTransaction => {
                formatter.write_str("SIP response does not match the REGISTER transaction")
            }
            Self::InvalidTransition => formatter.write_str("invalid IMS registration transition"),
            Self::MissingAuthenticationChallenge => {
                formatter.write_str("401/407 response has no AKA authentication challenge")
            }
            Self::MissingSecurityServer => {
                formatter.write_str("sec-agree requires Security-Server")
            }
            Self::ChallengeLimitExceeded => {
                formatter.write_str("IMS authentication challenge limit exceeded")
            }
            Self::RegistrationRejected(status) => {
                write!(formatter, "IMS registration rejected with SIP {status}")
            }
            Self::EntropyUnavailable => formatter.write_str("system entropy is unavailable"),
        }
    }
}

impl std::error::Error for SipError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SipHeader {
    name: String,
    value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SipResponse {
    pub status: u16,
    pub reason: String,
    headers: Vec<SipHeader>,
    pub body: Vec<u8>,
}

impl SipResponse {
    pub fn parse(message: &[u8]) -> Result<Self, SipError> {
        if message.len() > MAX_SIP_MESSAGE_LENGTH {
            return Err(SipError::MessageTooLarge);
        }
        let boundary = message
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or(SipError::MissingHeaderBoundary)?;
        let head =
            std::str::from_utf8(&message[..boundary]).map_err(|_| SipError::InvalidHeader)?;
        let mut lines = head.split("\r\n");
        let status_line = lines.next().ok_or(SipError::InvalidStatusLine)?;
        let mut status_parts = status_line.splitn(3, ' ');
        if status_parts.next() != Some("SIP/2.0") {
            return Err(SipError::InvalidStatusLine);
        }
        let status_text = status_parts.next().ok_or(SipError::InvalidStatusLine)?;
        if status_text.len() != 3 || !status_text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(SipError::InvalidStatusCode);
        }
        let status = status_text
            .parse()
            .map_err(|_| SipError::InvalidStatusCode)?;
        if !(100..=699).contains(&status) {
            return Err(SipError::InvalidStatusCode);
        }
        let reason = status_parts.next().unwrap_or_default().trim().to_owned();
        let mut headers = Vec::new();
        let mut content_length = None;
        let mut pending: Option<(String, String)> = None;
        let mut physical_header_count = 0usize;
        for line in lines {
            physical_header_count += 1;
            if physical_header_count > MAX_HEADER_COUNT {
                return Err(SipError::TooManyHeaders);
            }
            if line.len() > MAX_HEADER_LINE_LENGTH {
                return Err(SipError::HeaderLineTooLong);
            }
            if line.starts_with([' ', '\t']) {
                let Some((_, value)) = pending.as_mut() else {
                    return Err(SipError::InvalidHeader);
                };
                value.push(' ');
                value.push_str(line.trim_matches([' ', '\t']));
                continue;
            }
            if let Some((name, value)) = pending.take() {
                record_response_header(name, value, &mut headers, &mut content_length)?;
            }
            let (name, value) = line.split_once(':').ok_or(SipError::InvalidHeader)?;
            let name = name.trim();
            if name.is_empty() || !name.bytes().all(is_header_name_byte) {
                return Err(SipError::InvalidHeader);
            }
            pending = Some((name.to_owned(), value.trim().to_owned()));
        }
        if let Some((name, value)) = pending {
            record_response_header(name, value, &mut headers, &mut content_length)?;
        }
        let body = &message[boundary + 4..];
        let declared = content_length.unwrap_or(0);
        if body.len() != declared {
            return Err(SipError::BodyLengthMismatch {
                declared,
                actual: body.len(),
            });
        }
        Ok(Self {
            status,
            reason,
            headers,
            body: body.to_vec(),
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    }

    pub fn header_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
    }

    pub fn headers(&self, name: &str) -> impl Iterator<Item = &str> {
        self.headers
            .iter()
            .filter(move |header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    }

    pub fn register_transaction(&self) -> Result<RegisterTransaction<'_>, SipError> {
        let transaction = self.transaction()?;
        if !transaction.method.eq_ignore_ascii_case("REGISTER") {
            return Err(SipError::InvalidCSeq);
        }
        Ok(RegisterTransaction {
            call_id: transaction.call_id,
            cseq: transaction.cseq,
        })
    }

    /// Parse the transaction tuple from any SIP response. SIP method tokens
    /// are preserved exactly so each caller can enforce the method it owns.
    pub fn transaction(&self) -> Result<SipTransaction<'_>, SipError> {
        let call_id = self.single_header(&["Call-ID", "i"], "Call-ID")?;
        let cseq = self.single_header(&["CSeq"], "CSeq")?;
        let mut parts = cseq.split_ascii_whitespace();
        let number = parts
            .next()
            .ok_or(SipError::InvalidCSeq)?
            .parse::<u32>()
            .map_err(|_| SipError::InvalidCSeq)?;
        let method = parts.next().ok_or(SipError::InvalidCSeq)?;
        if parts.next().is_some() || method.is_empty() {
            return Err(SipError::InvalidCSeq);
        }
        Ok(SipTransaction {
            call_id: call_id.trim(),
            cseq: number,
            method,
        })
    }

    pub fn has_aka_challenge(&self) -> bool {
        let name = match self.status {
            401 => "WWW-Authenticate",
            407 => "Proxy-Authenticate",
            _ => return false,
        };
        self.headers(name).any(is_aka_digest_challenge)
    }

    pub fn has_security_server(&self) -> bool {
        self.headers("Security-Server").any(|value| {
            value
                .split(',')
                .any(|offer| offer.trim().to_ascii_lowercase().starts_with("ipsec-3gpp"))
        })
    }

    fn single_header(
        &self,
        aliases: &[&str],
        display_name: &'static str,
    ) -> Result<&str, SipError> {
        let mut values = self.headers.iter().filter(|header| {
            aliases
                .iter()
                .any(|name| header.name.eq_ignore_ascii_case(name))
        });
        let value = values
            .next()
            .ok_or(SipError::MissingTransactionHeader(display_name))?;
        if values.next().is_some() {
            return Err(SipError::DuplicateTransactionHeader(display_name));
        }
        Ok(value.value.as_str())
    }
}

fn record_response_header(
    name: String,
    value: String,
    headers: &mut Vec<SipHeader>,
    content_length: &mut Option<usize>,
) -> Result<(), SipError> {
    let value = value.trim();
    if value
        .bytes()
        .any(|byte| byte.is_ascii_control() && byte != b'\t')
    {
        return Err(SipError::InvalidHeader);
    }
    if name.eq_ignore_ascii_case("Content-Length") || name.eq_ignore_ascii_case("l") {
        if content_length.is_some() {
            return Err(SipError::DuplicateContentLength);
        }
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(SipError::InvalidContentLength);
        }
        *content_length = Some(
            value
                .parse::<usize>()
                .map_err(|_| SipError::InvalidContentLength)?,
        );
    }
    headers.push(SipHeader {
        name,
        value: value.to_owned(),
    });
    if headers.len() > MAX_HEADER_COUNT {
        return Err(SipError::TooManyHeaders);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SipTransaction<'a> {
    pub call_id: &'a str,
    pub cseq: u32,
    pub method: &'a str,
}

fn is_aka_digest_challenge(value: &str) -> bool {
    let Some(parameters) = digest_parameters::parse(value) else {
        return false;
    };
    let mut algorithm = None;
    let mut has_nonce = false;
    for (name, value) in &parameters {
        if name.eq_ignore_ascii_case("algorithm") {
            algorithm = Some(value.as_str());
        } else if name.eq_ignore_ascii_case("nonce") && !value.is_empty() {
            has_nonce = true;
        }
    }
    has_nonce
        && algorithm.is_some_and(|value| {
            value.eq_ignore_ascii_case("AKAv1-MD5") || value.eq_ignore_ascii_case("AKAv2-MD5")
        })
}

fn is_header_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.' | b'!' | b'%' | b'*' | b'_' | b'+' | b'`' | b'\'' | b'~'
        )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegisterTransaction<'a> {
    pub call_id: &'a str,
    pub cseq: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecAgreeMode {
    Disabled,
    Auto,
    Required,
}

/// Whether a plain authenticated REGISTER retains the initial sec-agree
/// offer.  `Auto` must remove the offer after a 401/407 that does not contain
/// a usable `Security-Server`; retaining it would ask the P-CSCF to negotiate
/// the same unavailable association again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlainSecurityDisposition {
    RetainOffer,
    OmitOffer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterPhase {
    Idle,
    AwaitingInitial,
    ChallengeReceived,
    AwaitingAuthenticated,
    Registered,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterAction {
    Wait,
    AnswerAka { install_ipsec: bool },
    Registered { ipsec: bool },
}

#[derive(Debug)]
pub struct RegisterTracker {
    call_id: String,
    cseq: u32,
    phase: RegisterPhase,
    sec_agree: SecAgreeMode,
    challenge_rounds: u8,
    security_server: Option<String>,
}

impl RegisterTracker {
    pub fn begin(
        call_id: impl Into<String>,
        cseq: u32,
        sec_agree: SecAgreeMode,
    ) -> Result<Self, SipError> {
        let call_id = call_id.into();
        if call_id.trim().is_empty() || cseq == 0 {
            return Err(SipError::InvalidTransition);
        }
        Ok(Self {
            call_id,
            cseq,
            phase: RegisterPhase::AwaitingInitial,
            sec_agree,
            challenge_rounds: 0,
            security_server: None,
        })
    }

    pub fn phase(&self) -> RegisterPhase {
        self.phase
    }

    pub fn accept(&mut self, response: &SipResponse) -> Result<RegisterAction, SipError> {
        let transaction = response.register_transaction()?;
        if transaction.call_id != self.call_id || transaction.cseq != self.cseq {
            return Err(SipError::UnexpectedTransaction);
        }
        if (100..200).contains(&response.status) {
            return Ok(RegisterAction::Wait);
        }
        match self.phase {
            RegisterPhase::AwaitingInitial | RegisterPhase::AwaitingAuthenticated => {
                self.accept_final(response)
            }
            _ => Err(SipError::InvalidTransition),
        }
    }

    fn accept_final(&mut self, response: &SipResponse) -> Result<RegisterAction, SipError> {
        let mut security_headers = response.headers("Security-Server");
        if let Some(value) = security_headers.next() {
            if security_headers.next().is_some() {
                self.phase = RegisterPhase::Failed;
                return Err(SipError::InvalidHeader);
            }
            if value
                .split(',')
                .any(|offer| offer.trim().to_ascii_lowercase().starts_with("ipsec-3gpp"))
            {
                self.security_server = Some(value.trim().to_owned());
            }
        }
        match response.status {
            200..=299 => {
                let ipsec =
                    self.sec_agree != SecAgreeMode::Disabled && self.security_server.is_some();
                if self.sec_agree == SecAgreeMode::Required && !ipsec {
                    self.phase = RegisterPhase::Failed;
                    return Err(SipError::MissingSecurityServer);
                }
                self.phase = RegisterPhase::Registered;
                Ok(RegisterAction::Registered { ipsec })
            }
            401 | 407 => {
                if !response.has_aka_challenge() {
                    self.phase = RegisterPhase::Failed;
                    return Err(SipError::MissingAuthenticationChallenge);
                }
                self.challenge_rounds = self.challenge_rounds.saturating_add(1);
                if self.challenge_rounds > 3 {
                    self.phase = RegisterPhase::Failed;
                    return Err(SipError::ChallengeLimitExceeded);
                }
                if self.sec_agree == SecAgreeMode::Required && self.security_server.is_none() {
                    self.phase = RegisterPhase::Failed;
                    return Err(SipError::MissingSecurityServer);
                }
                self.phase = RegisterPhase::ChallengeReceived;
                Ok(RegisterAction::AnswerAka {
                    install_ipsec: self.sec_agree != SecAgreeMode::Disabled
                        && self.security_server.is_some(),
                })
            }
            status => {
                self.phase = RegisterPhase::Failed;
                Err(SipError::RegistrationRejected(status))
            }
        }
    }

    pub fn authenticated_request_sent(&mut self, cseq: u32) -> Result<(), SipError> {
        if self.phase != RegisterPhase::ChallengeReceived || cseq <= self.cseq {
            return Err(SipError::InvalidTransition);
        }
        self.cseq = cseq;
        self.phase = RegisterPhase::AwaitingAuthenticated;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, cseq: u32, extra: &str) -> SipResponse {
        let raw = format!(
            "SIP/2.0 {status} Test\r\nVia: SIP/2.0/TCP [2001:db8::1]:5060\r\nCall-ID: reg-1\r\nCSeq: {cseq} REGISTER\r\n{extra}Content-Length: 0\r\n\r\n"
        );
        SipResponse::parse(raw.as_bytes()).unwrap()
    }

    #[test]
    fn parses_sanitized_giffgaff_challenge() {
        let reply = response(
            401,
            1,
            "WWW-Authenticate: Digest realm=\"ims.mnc010.mcc234.3gppnetwork.org\",nonce=\"redacted\",algorithm=AKAv1-MD5\r\nSecurity-Server: ipsec-3gpp;q=0.98;alg=hmac-sha-1-96;ealg=aes-cbc\r\n",
        );
        assert!(reply.has_aka_challenge());
        assert!(reply.has_security_server());
        assert_eq!(reply.register_transaction().unwrap().call_id, "reg-1");
    }

    #[test]
    fn tracks_giffgaff_401_ipsec_and_200_registration() {
        let mut tracker = RegisterTracker::begin("reg-1", 1, SecAgreeMode::Auto).unwrap();
        let action = tracker.accept(&response(401, 1, "WWW-Authenticate: Digest nonce=\"redacted\",algorithm=AKAv1-MD5\r\nSecurity-Server: ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc\r\n")).unwrap();
        assert_eq!(
            action,
            RegisterAction::AnswerAka {
                install_ipsec: true
            }
        );
        tracker.authenticated_request_sent(2).unwrap();
        assert_eq!(
            tracker.accept(&response(200, 2, "")).unwrap(),
            RegisterAction::Registered { ipsec: true }
        );
        assert_eq!(tracker.phase(), RegisterPhase::Registered);
    }

    #[test]
    fn auto_mode_can_disable_sec_agree_when_server_omits_offer() {
        let mut tracker = RegisterTracker::begin("reg-1", 1, SecAgreeMode::Auto).unwrap();
        assert_eq!(
            tracker
                .accept(&response(
                    401,
                    1,
                    "WWW-Authenticate: Digest nonce=\"redacted\",algorithm=AKAv1-MD5\r\n"
                ))
                .unwrap(),
            RegisterAction::AnswerAka {
                install_ipsec: false
            }
        );
        tracker.authenticated_request_sent(2).unwrap();
        assert_eq!(
            tracker.accept(&response(200, 2, "")).unwrap(),
            RegisterAction::Registered { ipsec: false }
        );
    }

    #[test]
    fn disabled_mode_never_enables_ipsec_from_server_offer() {
        let mut tracker = RegisterTracker::begin("reg-1", 1, SecAgreeMode::Disabled).unwrap();
        let challenge = response(
            401,
            1,
            "WWW-Authenticate: Digest nonce=\"redacted\",algorithm=\"AKAv1-MD5\"\r\nSecurity-Server: ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc\r\n",
        );
        assert_eq!(
            tracker.accept(&challenge).unwrap(),
            RegisterAction::AnswerAka {
                install_ipsec: false
            }
        );
        tracker.authenticated_request_sent(2).unwrap();
        assert_eq!(
            tracker.accept(&response(200, 2, "")).unwrap(),
            RegisterAction::Registered { ipsec: false }
        );
    }

    #[test]
    fn authentication_challenges_are_bounded() {
        let mut tracker = RegisterTracker::begin("reg-1", 1, SecAgreeMode::Auto).unwrap();
        for cseq in 1..=3 {
            let challenge = response(
                401,
                cseq,
                "WWW-Authenticate: Digest nonce=\"redacted\",algorithm=AKAv1-MD5\r\n",
            );
            assert!(matches!(
                tracker.accept(&challenge).unwrap(),
                RegisterAction::AnswerAka { .. }
            ));
            tracker.authenticated_request_sent(cseq + 1).unwrap();
        }
        assert_eq!(
            tracker
                .accept(&response(
                    401,
                    4,
                    "WWW-Authenticate: Digest nonce=\"redacted\",algorithm=AKAv1-MD5\r\n"
                ))
                .unwrap_err(),
            SipError::ChallengeLimitExceeded
        );
    }

    #[test]
    fn required_security_rejects_missing_offer() {
        let mut tracker = RegisterTracker::begin("reg-1", 1, SecAgreeMode::Required).unwrap();
        assert_eq!(
            tracker
                .accept(&response(
                    401,
                    1,
                    "WWW-Authenticate: Digest nonce=\"redacted\",algorithm=AKAv1-MD5\r\n"
                ))
                .unwrap_err(),
            SipError::MissingSecurityServer
        );
    }

    #[test]
    fn rejects_smuggling_wrong_body_and_transaction() {
        let duplicate = b"SIP/2.0 200 OK\r\nCall-ID: reg-1\r\nCSeq: 1 REGISTER\r\nContent-Length: 0\r\nl: 0\r\n\r\n";
        assert_eq!(
            SipResponse::parse(duplicate).unwrap_err(),
            SipError::DuplicateContentLength
        );
        let wrong_body =
            b"SIP/2.0 200 OK\r\nCall-ID: reg-1\r\nCSeq: 1 REGISTER\r\nContent-Length: 1\r\n\r\n";
        assert!(matches!(
            SipResponse::parse(wrong_body),
            Err(SipError::BodyLengthMismatch { .. })
        ));
        let mut tracker = RegisterTracker::begin("reg-1", 1, SecAgreeMode::Auto).unwrap();
        assert_eq!(
            tracker.accept(&response(200, 2, "")).unwrap_err(),
            SipError::UnexpectedTransaction
        );
    }

    #[test]
    fn accepts_folded_headers_and_rejects_orphaned_folds_and_non_register_cseq() {
        let folded = b"SIP/2.0 401 Unauthorized\r\nCall-ID: reg-1\r\nCSeq: 1 REGISTER\r\nWWW-Authenticate: Digest realm=ims.example,\r\n nonce=x\r\nContent-Length: 0\r\n\r\n";
        let parsed = SipResponse::parse(folded).unwrap();
        assert_eq!(
            parsed.header("WWW-Authenticate"),
            Some("Digest realm=ims.example, nonce=x")
        );
        let orphaned = b"SIP/2.0 401 Unauthorized\r\n Call-ID: reg-1\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            SipResponse::parse(orphaned).unwrap_err(),
            SipError::InvalidHeader
        );
        let options =
            b"SIP/2.0 200 OK\r\nCall-ID: reg-1\r\nCSeq: 1 OPTIONS\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            SipResponse::parse(options)
                .unwrap()
                .register_transaction()
                .unwrap_err(),
            SipError::InvalidCSeq
        );
    }

    #[test]
    fn generic_transaction_parser_preserves_message_method_and_exact_identity() {
        let message = b"SIP/2.0 202 Accepted\r\nCall-ID: sms-1\r\nCSeq: 19 MESSAGE\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            SipResponse::parse(message).unwrap().transaction().unwrap(),
            SipTransaction {
                call_id: "sms-1",
                cseq: 19,
                method: "MESSAGE",
            }
        );
        let lowercase_method = b"SIP/2.0 202 Accepted\r\nCall-ID: sms-1\r\nCSeq: 19 message\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            SipResponse::parse(lowercase_method)
                .unwrap()
                .transaction()
                .unwrap()
                .method,
            "message"
        );
    }

    #[test]
    fn accepts_digest_parameter_whitespace_but_rejects_duplicate_transactions() {
        let challenge = response(
            401,
            1,
            "WWW-Authenticate: Digest realm = \"ims.example\", nonce = \"redacted\", algorithm = \"AKAv1-MD5\"\r\n",
        );
        assert!(challenge.has_aka_challenge());
        let duplicate = b"SIP/2.0 200 OK\r\nCall-ID: reg-1\r\ni: reg-2\r\nCSeq: 1 REGISTER\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            SipResponse::parse(duplicate)
                .unwrap()
                .register_transaction()
                .unwrap_err(),
            SipError::DuplicateTransactionHeader("Call-ID")
        );
    }
}

pub mod options_heartbeat;
