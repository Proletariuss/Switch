//! Typed, single-owner event access to a registered IMS signaling session.
//!
//! The native Rust owner uses this boundary to parse exactly
//! one bounded SIP frame, retain the primary/Port-S connection identity, and
//! write transaction responses back on that same reliable association. It
//! refreshes REGISTER in-place when supplied same-session credentials. Other
//! owners retain the one-shot renewal event; transport replacement stays with
//! the supervisor.

use std::{io, time::Duration};

use proletarius_ims::stream::SipStreamItem;
use proletarius_sip::{
    Error as SipError, Header, Headers, Message, MessageData, Method, Parser, Request, Response,
    Uri,
};
use tokio::time::Instant;
use zeroize::Zeroizing;

use crate::sip_response_registry::ResponseRegistry;
pub use crate::sip_response_registry::ResponseTicket;

use crate::registration_coordinator::{
    CleanupReport, RegisteredImsSession, SessionSipEvent, SipConnectionId,
};

const MAX_REASON_LENGTH: usize = 256;
pub(crate) mod refresh;
pub use refresh::RefreshAuthenticate;
mod heartbeat;
pub(crate) use heartbeat::Configuration as HeartbeatConfiguration;
use heartbeat::{Heartbeat, Phase as HeartbeatPhase};
pub use heartbeat::{HeartbeatFailure, HeartbeatHealth, HeartbeatObserver};

/// A parsed SIP item, tagged with the exact protected TCP association it came
/// from. Keep this value within the owning session worker; it contains SIP
/// subscriber data and intentionally has no `Debug` implementation.
pub enum RegisteredSipEvent {
    /// The registrar-derived renewal point has arrived. This is an advisory,
    /// one-shot lifecycle signal; an owning supervisor must decide when active
    /// calls and transactions permit a controlled session replacement.
    RegistrationRenewalDue,
    /// A primary miss withdraws new-business readiness; one matching final
    /// confirmation 2xx restores it. Neither state implies Port-S reachability.
    HeartbeatHealthChanged {
        healthy: bool,
    },
    /// Outcome is unknown; callers must not automatically resend an SMS.
    ResponseTimedOut {
        ticket: ResponseTicket,
    },
    KeepAlive {
        connection: SipConnectionId,
    },
    Message {
        connection: SipConnectionId,
        message: Box<RegisteredSipMessage>,
        /// Provisional responses retain the ticket; a final response consumes
        /// it. Unsolicited, malformed-key, duplicate and late replies use None.
        response_ticket: Option<ResponseTicket>,
    },
}

impl RegisteredSipEvent {
    /// Feed both timely and late primary-flow replies into the SMS outcome
    /// state machine. A missing response ticket must not discard a late result
    /// for an already-unknown SMS. Other associations and requests are ignored.
    pub fn observe_sms_response(
        &self,
        submission: &mut crate::sms_transaction::SmsSubmission,
    ) -> Result<
        crate::sms_transaction::SmsResponseDisposition,
        crate::sms_transaction::SmsTransactionError,
    > {
        if let Self::Message {
            connection: SipConnectionId::Primary,
            message,
            ..
        } = self
            && let RegisteredSipMessage::Response {
                status, headers, ..
            } = message.as_ref()
        {
            return submission.observe_response_headers(*status, headers);
        }
        Ok(crate::sms_transaction::SmsResponseDisposition::Ignored)
    }
}

/// Parsed signaling content. The SIP body is zeroized when the message is
/// dropped and no `Debug` implementation is provided, since it may contain
/// SMS payloads or subscriber identities.
pub enum RegisteredSipMessage {
    Request {
        method: Method,
        uri: Uri,
        version: String,
        headers: Headers,
        body: Zeroizing<Vec<u8>>,
    },
    Response {
        status: u16,
        reason: String,
        version: String,
        headers: Headers,
        body: Zeroizing<Vec<u8>>,
    },
}

/// Owns the only mutable access to the registered session. Callers should
/// drive `next_event`, process the returned item, then use `send` or
/// `respond_to` before advancing the stream again.
pub struct RegisteredSipRuntime {
    session: RegisteredImsSession,
    responses: ResponseRegistry,
    renewal_deadline: Option<Instant>,
    refresh: Option<refresh::Refresh>,
    heartbeat: Option<Heartbeat>,
    heartbeat_unavailable: bool,
}

impl RegisteredSipRuntime {
    #[must_use]
    pub fn new(mut session: RegisteredImsSession) -> Self {
        let refresh = session
            .take_refresh_configuration()
            .map(refresh::Refresh::new);
        let renewal_deadline = if refresh.is_none() {
            session.registration_renewal_deadline()
        } else {
            None
        };
        Self {
            session,
            responses: ResponseRegistry::default(),
            renewal_deadline,
            refresh,
            heartbeat: None,
            heartbeat_unavailable: false,
        }
    }

    /// Use the explicit policy frozen by successful native REGISTER. A narrow
    /// SMS-only carrier exemption differs from missing configuration, which
    /// remains an error. Neither policy changes call context or REGISTER refresh.
    pub fn new_with_heartbeat(mut session: RegisteredImsSession) -> Self {
        use crate::registration_coordinator::RegisteredOptionsConfiguration;
        let (heartbeat, heartbeat_unavailable) = match session.take_heartbeat_configuration() {
            Some(RegisteredOptionsConfiguration::Probe(configuration)) => {
                (Some(Heartbeat::new(configuration)), false)
            }
            Some(RegisteredOptionsConfiguration::DisabledForSmsOnlyVodafoneUk) => (None, false),
            None => (None, true),
        };
        let mut runtime = Self::new(session);
        runtime.heartbeat = heartbeat;
        runtime.heartbeat_unavailable = heartbeat_unavailable;
        runtime
    }

    pub fn heartbeat_deadline(&self) -> Option<Instant> {
        [
            self.heartbeat.as_ref().and_then(Heartbeat::deadline),
            self.refresh.as_ref().map(refresh::Refresh::deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }
    pub fn heartbeat_health(&self) -> HeartbeatHealth {
        self.session.heartbeat_health()
    }

    /// Progress owned heartbeat and REGISTER refresh work without reading SIP. The
    /// actor uses this while an already-read event waits for consumer capacity.
    pub async fn heartbeat_tick(&mut self) -> io::Result<()> {
        let expiry = self.refresh.as_ref().map(refresh::Refresh::expiry);
        let work = async {
            if !self.refresh.as_ref().is_some_and(refresh::Refresh::writing) {
                self.heartbeat_tick_inner().await?;
            }
            if let Some(refresh) = &mut self.refresh {
                refresh.tick(&mut self.session, &mut self.responses).await?;
            }
            Ok(())
        };
        let result = if let Some(expiry) = expiry {
            tokio::select! {biased; _=tokio::time::sleep_until(expiry)=>Err(io::Error::other("IMS registration lease expired")), result=work=>result}
        } else {
            work.await
        };
        if result.is_err() {
            self.session.publish_heartbeat_health(false);
        }
        result
    }
    async fn heartbeat_tick_inner(&mut self) -> io::Result<()> {
        if self.heartbeat_unavailable {
            return Err(heartbeat::failed(
                "registered IMS heartbeat configuration unavailable",
            ));
        }
        let now = Instant::now();
        if let Some(hb) = &self.heartbeat {
            if matches!(hb.phase, HeartbeatPhase::Failed) {
                return Err(heartbeat::failed("IMS OPTIONS confirmation failed"));
            }
        } else {
            return Ok(());
        }
        if let Some((ticket, deadline)) = self.heartbeat.as_ref().and_then(|hb| match hb.phase {
            HeartbeatPhase::Pending {
                ticket, deadline, ..
            } => Some((ticket, deadline)),
            _ => None,
        }) {
            if deadline <= now {
                self.responses.cancel(ticket);
                self.complete_heartbeat(0);
            }
        }
        if let Some((request, confirmation)) =
            self.heartbeat.as_mut().unwrap().start(Instant::now())?
        {
            let timeout = self.heartbeat.as_ref().unwrap().timeout();
            // Reserve before queueing; keep the complete write in session state
            // before any await so dropping next_event cannot duplicate it.
            let ticket = self
                .responses
                .reserve(&request, timeout + Duration::from_secs(10))?;
            let wire = Zeroizing::new(Message::Request(request).encode());
            if let Err(error) = self.session.queue_heartbeat(&wire) {
                self.responses.cancel(ticket);
                return Err(error);
            }
            self.heartbeat.as_mut().unwrap().phase = HeartbeatPhase::Writing {
                ticket,
                confirmation,
            };
        }
        if self.heartbeat.as_ref().unwrap().write_pending() {
            if let Err(error) = self.session.finish_heartbeat_write().await {
                if let Some(hb) = &mut self.heartbeat {
                    if let HeartbeatPhase::Writing { ticket, .. } = hb.phase {
                        self.responses.cancel(ticket);
                    }
                    hb.phase = HeartbeatPhase::Failed;
                }
                self.session.publish_heartbeat_health(false);
                return Err(error);
            }
            let now = Instant::now();
            let hb = self.heartbeat.as_mut().unwrap();
            if let HeartbeatPhase::Writing { ticket, .. } = hb.phase {
                self.responses.arm(ticket, now + hb.timeout())?;
            }
            hb.written(now);
        }
        if self
            .heartbeat
            .as_ref()
            .is_some_and(|hb| matches!(hb.phase, HeartbeatPhase::Failed))
        {
            self.session.publish_heartbeat_health(false);
            return Err(heartbeat::failed("IMS OPTIONS confirmation failed"));
        }
        Ok(())
    }
    fn complete_heartbeat(&mut self, status: u16) {
        if let Some(healthy) = self
            .heartbeat
            .as_mut()
            .and_then(|hb| hb.complete(status, Instant::now()))
        {
            self.session.publish_heartbeat_health(healthy);
        }
    }

    #[cfg(test)]
    pub(crate) fn heartbeat_write_pending(&self) -> bool {
        self.heartbeat
            .as_ref()
            .is_some_and(Heartbeat::write_pending)
    }
    #[cfg(test)]
    pub(crate) fn shorten_heartbeat_budgets(&mut self, response: Duration, confirm: Duration) {
        let hb = self
            .heartbeat
            .as_mut()
            .expect("actual successful profile REGISTER");
        hb.response_timeout = response;
        hb.confirm_delay = confirm;
    }

    pub fn is_secured(&self) -> bool {
        self.session.is_secured()
    }
    pub fn incoming_diagnostics(&self) -> crate::registration_coordinator::IncomingDiagnostics {
        self.session.incoming_diagnostics()
    }

    /// Read and parse one complete SIP item. Stream framing and this parser
    /// both enforce size/content-length bounds; extra trailing bytes are
    /// rejected instead of silently reinterpreted as a second message.
    pub async fn next_event(&mut self) -> io::Result<Option<RegisteredSipEvent>> {
        let result = self.next_event_inner().await;
        if !matches!(&result, Ok(Some(_))) {
            self.session.publish_heartbeat_health(false);
        }
        result
    }
    async fn next_event_inner(&mut self) -> io::Result<Option<RegisteredSipEvent>> {
        loop {
            self.heartbeat_tick().await?;
            if let Some(healthy) = self.heartbeat.as_mut().and_then(Heartbeat::notice) {
                return Ok(Some(RegisteredSipEvent::HeartbeatHealthChanged { healthy }));
            }
            if let Some(ticket) = self.responses.expire_one(Instant::now()) {
                if self
                    .refresh
                    .as_ref()
                    .is_some_and(|refresh| refresh.owns(ticket))
                {
                    return Err(io::Error::other("REGISTER refresh response timed out"));
                }
                if self.heartbeat.as_ref().is_some_and(|hb| hb.owns(ticket)) {
                    self.complete_heartbeat(0);
                    continue;
                }
                return Ok(Some(RegisteredSipEvent::ResponseTimedOut { ticket }));
            }
            if self
                .renewal_deadline
                .is_some_and(|deadline| deadline <= Instant::now())
            {
                self.renewal_deadline = None;
                return Ok(Some(RegisteredSipEvent::RegistrationRenewalDue));
            }
            let deadline = [
                self.responses.next_deadline(),
                self.renewal_deadline,
                self.heartbeat_deadline(),
            ]
            .into_iter()
            .flatten()
            .min();
            let sleep = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            let event =
                tokio::select! {biased; _=sleep=>continue,event=self.session.next_event()=>event?};
            let Some(event) = event else {
                return Ok(None);
            };
            let mut event = decode_event(event)?;
            if matches!(
                &event,
                RegisteredSipEvent::Message {
                    connection: SipConnectionId::Incoming(_),
                    ..
                }
            ) {
                self.session.observe_incoming_sip();
            }
            if let RegisteredSipEvent::Message {
                connection,
                message,
                response_ticket,
            } = &mut event
            {
                *response_ticket = self.responses.correlate(*connection, message);
                if response_ticket.is_some_and(|ticket| {
                    self.refresh
                        .as_ref()
                        .is_some_and(|refresh| refresh.owns(ticket))
                }) {
                    self.refresh
                        .as_mut()
                        .unwrap()
                        .response(message, &mut self.session)?;
                    continue;
                }
                if response_ticket
                    .is_some_and(|ticket| self.heartbeat.as_ref().is_some_and(|hb| hb.owns(ticket)))
                {
                    if let RegisteredSipMessage::Response { status, .. } = message.as_ref() {
                        if *status >= 200 {
                            self.complete_heartbeat(*status);
                        }
                        // Provisional responses never reset the response budget.
                        continue;
                    }
                }
            }
            return Ok(Some(event));
        }
    }

    /// Reserve before writing, so a fast response cannot race registration.
    /// At most 64 MESSAGE/OPTIONS requests may wait, for up to 60 seconds.
    /// Each new attempt must use a fresh SIP transaction identity. Cancellation
    /// during write retains the bounded reservation until its deadline; a
    /// write error has an unknown remote outcome and is never retried here.
    pub async fn send_tracked_request(
        &mut self,
        request: Request,
        timeout: Duration,
    ) -> io::Result<ResponseTicket> {
        if !matches!(request.method, Method::MESSAGE | Method::OPTIONS) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported external tracked SIP method",
            ));
        }
        let ticket = self.responses.reserve(&request, timeout)?;
        let wire = Zeroizing::new(Message::Request(request).encode());
        if let Err(error) = self.session.send(&wire).await {
            self.responses.cancel(ticket);
            return Err(error);
        }
        Ok(ticket)
    }

    pub fn cancel_response_wait(&mut self, ticket: ResponseTicket) {
        self.responses.cancel(ticket);
    }

    /// Send a pre-encoded complete SIP message on the protected primary
    /// association. Transaction policy and request correlation remain with
    /// the Rust caller.
    pub async fn send(&mut self, message: &[u8]) -> io::Result<()> {
        self.session.send(message).await
    }

    /// Build a transaction response from the request's original routing and
    /// correlation headers, then send it on the association that received the
    /// request. This does not add dialog policy such as a To-tag or Contact.
    pub async fn respond_to(
        &mut self,
        connection: SipConnectionId,
        request: &RegisteredSipMessage,
        status: u16,
        reason: &str,
        allow: Option<&str>,
    ) -> io::Result<()> {
        let RegisteredSipMessage::Request {
            version, headers, ..
        } = request
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot respond to a SIP response as a request",
            ));
        };
        let response = response_for_request(version, headers, status, reason, allow)?;
        self.session
            .send_on(connection, &Message::Response(response).encode())
            .await
    }

    /// Dialog requests use their own generation-bound transaction owner. ACKs
    /// never reserve a MESSAGE/OPTIONS response ticket.
    pub async fn send_dialog_request(&mut self, request: Request) -> io::Result<()> {
        if !matches!(
            request.method,
            Method::INVITE
                | Method::ACK
                | Method::CANCEL
                | Method::BYE
                | Method::PRACK
                | Method::UPDATE
                | Method::INFO
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported dialog method",
            ));
        }
        let wire = checked_dialog_wire(Message::Request(request))?;
        self.session.send(&wire).await
    }

    /// Reply on the exact association that delivered the request, retaining
    /// the native dialog's To-tag, Contact and SDP instead of rebuilding them.
    pub async fn send_dialog_response(
        &mut self,
        connection: SipConnectionId,
        response: proletarius_sip::Response,
    ) -> io::Result<()> {
        if !(100..=699).contains(&response.status) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid dialog status",
            ));
        }
        let wire = checked_dialog_wire(Message::Response(response))?;
        self.session.send_on(connection, &wire).await
    }

    /// Close every signaling association before releasing the security lease.
    pub async fn shutdown(mut self) -> Result<(), CleanupReport> {
        self.refresh.take();
        self.session.publish_heartbeat_health(false);
        self.session.shutdown().await
    }
}

fn decode_event(event: SessionSipEvent) -> io::Result<RegisteredSipEvent> {
    let connection = event.connection();
    match event.into_item() {
        SipStreamItem::KeepAliveLine => Ok(RegisteredSipEvent::KeepAlive { connection }),
        SipStreamItem::Message(frame) => {
            let parser = Parser::default();
            let (message, consumed) = parser.parse(&frame, false).map_err(sip_framing_error)?;
            if consumed != frame.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "IMS SIP stream item contains trailing bytes",
                ));
            }
            Ok(RegisteredSipEvent::Message {
                connection,
                message: Box::new(sensitive_message(message)),
                response_ticket: None,
            })
        }
    }
}

fn sensitive_message(message: Message) -> RegisteredSipMessage {
    match message {
        Message::Request(request) => RegisteredSipMessage::Request {
            method: request.method,
            uri: request.uri,
            version: request.data.version,
            headers: request.data.headers,
            body: Zeroizing::new(request.data.body),
        },
        Message::Response(response) => RegisteredSipMessage::Response {
            status: response.status,
            reason: response.reason,
            version: response.data.version,
            headers: response.data.headers,
            body: Zeroizing::new(response.data.body),
        },
    }
}

fn sip_framing_error(error: SipError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn response_for_request(
    version: &str,
    request_headers: &Headers,
    status: u16,
    reason: &str,
    allow: Option<&str>,
) -> io::Result<Response> {
    if !(100..=699).contains(&status)
        || reason.is_empty()
        || reason.len() > MAX_REASON_LENGTH
        || reason.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid IMS SIP response status or reason",
        ));
    }

    let mut headers = Headers::default();
    let vias: Vec<_> = request_headers.all("Via").collect();
    if vias.is_empty() {
        return Err(missing_header("Via"));
    }
    for via in vias {
        headers.push(Header::new(via.name(), via.value()).map_err(sip_framing_error)?);
    }
    for required in ["From", "To", "Call-ID", "CSeq"] {
        let mut values = request_headers.all(required);
        let Some(header) = values.next() else {
            return Err(missing_header(required));
        };
        if values.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("duplicate IMS SIP {required} header"),
            ));
        }
        headers.push(Header::new(header.name(), header.value()).map_err(sip_framing_error)?);
    }
    if let Some(allow) = allow {
        headers.push(Header::new("Allow", allow).map_err(sip_framing_error)?);
    }
    Ok(Response {
        status,
        reason: reason.to_owned(),
        data: MessageData::new(version.to_owned(), headers, Vec::new()),
    })
}

fn missing_header(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("IMS SIP request is missing {name}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proletarius_sip::{Message, Method, Request, Uri};

    fn parsed_request() -> Request {
        let wire = b"MESSAGE sip:ims.example SIP/2.0\r\nVia: SIP/2.0/TCP 10.0.0.2:5060;branch=z9hG4bKone\r\nVia: SIP/2.0/TCP 10.0.0.3:5060;branch=z9hG4bKtwo\r\nFrom: <sip:alice@ims.example>;tag=from1\r\nTo: <sip:service@ims.example>\r\nCall-ID: msg-1@device\r\nCSeq: 7 MESSAGE\r\nContent-Type: application/vnd.3gpp.sms\r\nContent-Length: 0\r\n\r\n";
        let (Message::Request(request), consumed) = Parser::default().parse(wire, false).unwrap()
        else {
            panic!("expected request")
        };
        assert_eq!(consumed, wire.len());
        request
    }

    #[test]
    fn transaction_response_preserves_all_vias_and_required_headers() {
        let request = parsed_request();
        let response = response_for_request(
            &request.data.version,
            &request.data.headers,
            200,
            "OK",
            None,
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.data.headers.all("Via").count(), 2);
        assert_eq!(
            response.data.headers.get("From").unwrap().value(),
            "<sip:alice@ims.example>;tag=from1"
        );
        assert_eq!(
            response.data.headers.get("To").unwrap().value(),
            "<sip:service@ims.example>"
        );
        assert_eq!(
            response.data.headers.get("Call-ID").unwrap().value(),
            "msg-1@device"
        );
        assert_eq!(
            response.data.headers.get("CSeq").unwrap().value(),
            "7 MESSAGE"
        );
        assert_eq!(response.data.headers.content_length().unwrap(), 0);
    }

    #[test]
    fn method_not_allowed_response_can_advertise_runtime_capabilities() {
        let request = parsed_request();
        let response = response_for_request(
            &request.data.version,
            &request.data.headers,
            405,
            "Method Not Allowed",
            Some("MESSAGE, OPTIONS"),
        )
        .unwrap();
        assert_eq!(
            response.data.headers.get("Allow").unwrap().value(),
            "MESSAGE, OPTIONS"
        );
    }

    #[test]
    fn response_builder_rejects_header_injection_and_missing_transaction_identity() {
        let request = parsed_request();
        assert!(
            response_for_request(
                &request.data.version,
                &request.data.headers,
                200,
                "OK\r\nX-Bad: yes",
                None
            )
            .is_err()
        );
        assert!(
            response_for_request(
                &request.data.version,
                &request.data.headers,
                700,
                "Bad",
                None
            )
            .is_err()
        );

        let mut missing_call_id = request.clone();
        let mut headers = Headers::default();
        for header in missing_call_id.data.headers.iter() {
            if !header.is_named("Call-ID") {
                headers.push(header.clone());
            }
        }
        missing_call_id.data.headers = headers;
        assert!(
            response_for_request(
                &missing_call_id.data.version,
                &missing_call_id.data.headers,
                200,
                "OK",
                None
            )
            .is_err()
        );
    }

    #[test]
    fn sms_response_requires_primary_connection_and_preserves_unknown_until_confirmation() {
        use crate::sms_transaction::{SmsResponseDisposition, SmsSubmission};
        let mut submission = SmsSubmission::new(1).unwrap();
        submission.mark_submitted("msg-1@device", 7).unwrap();
        submission.mark_outcome_unknown().unwrap();
        let event = |connection| RegisteredSipEvent::Message {
            connection,
            message: Box::new(RegisteredSipMessage::Response {
                status: 200,
                reason: "OK".into(),
                version: "SIP/2.0".into(),
                headers: parsed_request().data.headers,
                body: Zeroizing::new(Vec::new()),
            }),
            response_ticket: None,
        };
        assert_eq!(
            event(SipConnectionId::Incoming(1))
                .observe_sms_response(&mut submission)
                .unwrap(),
            SmsResponseDisposition::Ignored
        );
        assert_eq!(
            event(SipConnectionId::Primary)
                .observe_sms_response(&mut submission)
                .unwrap(),
            SmsResponseDisposition::LateResultResolved {
                accepted: true,
                accepted_parts: 1
            }
        );
        assert_eq!(
            event(SipConnectionId::Primary)
                .observe_sms_response(&mut submission)
                .unwrap(),
            SmsResponseDisposition::Ignored
        );
    }

    #[test]
    fn typed_sms_response_rejects_ambiguous_cseq_without_mutating_submission() {
        use crate::sms_transaction::SmsSubmission;
        for invalid_cseq in ["7 MESSAGE extra", "+7 MESSAGE", "2147483648 MESSAGE"] {
            let mut submission = SmsSubmission::new(1).unwrap();
            submission.mark_submitted("msg-1@device", 7).unwrap();
            let before = submission.clone();
            let mut headers = Headers::default();
            for header in parsed_request().data.headers.iter() {
                headers.push(if header.is_named("CSeq") {
                    Header::new("CSeq", invalid_cseq).unwrap()
                } else {
                    header.clone()
                });
            }
            assert!(submission.observe_response_headers(200, &headers).is_err());
            assert_eq!(submission, before);
        }
    }

    #[test]
    fn parser_fixture_uses_known_sip_method_and_uri() {
        let request = parsed_request();
        assert_eq!(request.method, Method::MESSAGE);
        assert_eq!(request.uri, Uri::parse("sip:ims.example").unwrap());
    }
}

fn checked_dialog_wire(message: Message) -> io::Result<Zeroizing<Vec<u8>>> {
    let data = match &message {
        Message::Request(request) => &request.data,
        Message::Response(response) => {
            if response.reason.is_empty()
                || response.reason.len() > MAX_REASON_LENGTH
                || response.reason.bytes().any(|b| b.is_ascii_control())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid dialog reason",
                ));
            }
            &response.data
        }
    };
    if data.version != "SIP/2.0"
        || data.headers.len() > proletarius_sip::MAX_HEADER_COUNT
        || data.body.len() > proletarius_sip::MAX_MESSAGE_SIZE
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid typed dialog frame bounds",
        ));
    }
    let wire = Zeroizing::new(message.encode());
    if wire.len() > proletarius_sip::MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "dialog SIP frame too large",
        ));
    }
    let (_, consumed) = proletarius_sip::Parser::default()
        .parse(&wire, false)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid dialog SIP frame"))?;
    if consumed != wire.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "trailing dialog SIP data",
        ));
    }
    Ok(wire)
}

#[cfg(test)]
mod dialog_frame_tests {
    use super::*;
    #[test]
    fn typed_dialog_frames_bound_size_headers_and_preserve_sdp_and_tags() {
        let wire=b"INVITE sip:b@ims.test SIP/2.0\r\nVia: SIP/2.0/TCP a;branch=z9hG4bK1\r\nFrom: <sip:a@ims.test>;tag=a\r\nTo: <sip:b@ims.test>\r\nCall-ID: call1\r\nCSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";
        let (message, _) = Parser::default().parse(wire, false).unwrap();
        assert!(checked_dialog_wire(message.clone()).is_ok());
        let Message::Request(mut request) = message else {
            panic!("request");
        };
        request.data.body = vec![0; proletarius_sip::MAX_MESSAGE_SIZE];
        assert!(checked_dialog_wire(Message::Request(request.clone())).is_err());
        request.data.body.clear();
        for _ in 0..proletarius_sip::MAX_HEADER_COUNT {
            request
                .data
                .headers
                .push(Header::new("X-Test", "x").unwrap());
        }
        assert!(checked_dialog_wire(Message::Request(request)).is_err());
    }
}
