//! Bounded, session-local response correlation for MESSAGE and OPTIONS.
//! No retransmission is performed: timeout or write failure may have an
//! unknown remote outcome. Call/registration transactions have other owners.

use std::{collections::BTreeMap, io, time::Duration};

use proletarius_sip::{Headers, Method, Request};
use tokio::time::Instant;

use crate::{registration_coordinator::SipConnectionId, sip_session_runtime::RegisteredSipMessage};

const MAX_PENDING: usize = 64;
const MAX_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ID_LENGTH: usize = 1024;

/// Opaque and valid only within the runtime that issued it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResponseTicket(u64);

// Subscriber identities deliberately have no Debug implementation.
#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct ResponseKey {
    call_id: String,
    cseq: u32,
    method: Method,
    top_via: String,
    branch: String,
}

struct Pending {
    ticket: ResponseTicket,
    deadline: Instant,
}

#[derive(Default)]
pub(super) struct ResponseRegistry {
    next_ticket: u64,
    pending: BTreeMap<ResponseKey, Pending>,
}

impl ResponseRegistry {
    pub fn reserve(&mut self, request: &Request, timeout: Duration) -> io::Result<ResponseTicket> {
        self.reserve_inner(request, timeout, false)
    }
    pub(super) fn reserve_registration(
        &mut self,
        request: &Request,
        timeout: Duration,
    ) -> io::Result<ResponseTicket> {
        if request.method != Method::REGISTER {
            return Err(invalid("registration reservation requires REGISTER"));
        }
        self.reserve_inner(request, timeout, true)
    }
    fn reserve_inner(
        &mut self,
        request: &Request,
        timeout: Duration,
        registration: bool,
    ) -> io::Result<ResponseTicket> {
        if !(matches!(request.method, Method::MESSAGE | Method::OPTIONS)
            || (registration && request.method == Method::REGISTER))
            || timeout.is_zero()
            || timeout > MAX_TIMEOUT
        {
            return Err(invalid("unsupported tracked SIP method or timeout"));
        }
        let key = response_key(&request.data.headers)?;
        if key.method != request.method {
            return Err(invalid("SIP request method differs from CSeq"));
        }
        if self.pending.contains_key(&key) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "SIP request already pending",
            ));
        }
        if self.pending.len() >= MAX_PENDING {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "IMS response registry is full",
            ));
        }
        self.next_ticket = self
            .next_ticket
            .checked_add(1)
            .ok_or_else(|| invalid("IMS response ticket exhausted"))?;
        let ticket = ResponseTicket(self.next_ticket);
        self.pending.insert(
            key,
            Pending {
                ticket,
                deadline: Instant::now() + timeout,
            },
        );
        Ok(ticket)
    }

    pub fn arm(&mut self, ticket: ResponseTicket, deadline: Instant) -> io::Result<()> {
        let entry = self
            .pending
            .values_mut()
            .find(|entry| entry.ticket == ticket)
            .ok_or_else(|| invalid("IMS response ticket missing"))?;
        entry.deadline = deadline;
        Ok(())
    }

    pub fn cancel(&mut self, ticket: ResponseTicket) {
        self.pending.retain(|_, entry| entry.ticket != ticket);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|entry| entry.deadline).min()
    }

    pub fn expire_one(&mut self, now: Instant) -> Option<ResponseTicket> {
        let ticket = self
            .pending
            .values()
            .filter(|entry| entry.deadline <= now)
            .min_by_key(|entry| entry.deadline)
            .map(|entry| entry.ticket)?;
        self.cancel(ticket);
        Some(ticket)
    }

    pub fn correlate(
        &mut self,
        connection: SipConnectionId,
        message: &RegisteredSipMessage,
    ) -> Option<ResponseTicket> {
        // send_tracked_request writes only on the primary association. An
        // otherwise identical response on a Port-S connection cannot claim it.
        if connection != SipConnectionId::Primary {
            return None;
        }
        let RegisteredSipMessage::Response {
            status, headers, ..
        } = message
        else {
            return None;
        };
        if !(100..=699).contains(status) {
            return None;
        }
        let key = response_key(headers).ok()?;
        let entry = self.pending.get(&key)?;
        if entry.deadline <= Instant::now() {
            return None;
        }
        let ticket = entry.ticket;
        if *status >= 200 {
            self.pending.remove(&key);
        }
        Some(ticket)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn unique<'a>(headers: &'a Headers, name: &str) -> io::Result<&'a str> {
    let mut values = headers.all(name);
    let value = values
        .next()
        .ok_or_else(|| invalid("missing SIP correlation header"))?
        .value()
        .trim();
    if values.next().is_some() || value.is_empty() || value.len() > MAX_ID_LENGTH {
        return Err(invalid("ambiguous or oversized SIP correlation header"));
    }
    Ok(value)
}

fn response_key(headers: &Headers) -> io::Result<ResponseKey> {
    let call_id = unique(headers, "Call-ID")?;
    let mut cseq = unique(headers, "CSeq")?.split_ascii_whitespace();
    let number = cseq.next().ok_or_else(|| invalid("missing SIP CSeq"))?;
    if !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("invalid SIP CSeq"));
    }
    let number: u32 = number.parse().map_err(|_| invalid("invalid SIP CSeq"))?;
    if number >= (1 << 31) {
        return Err(invalid("SIP CSeq exceeds protocol range"));
    }
    let method = Method::new(
        cseq.next()
            .ok_or_else(|| invalid("missing SIP CSeq method"))?,
    )
    .map_err(|_| invalid("invalid SIP CSeq method"))?;
    if cseq.next().is_some() {
        return Err(invalid("trailing SIP CSeq data"));
    }
    let via = headers
        .get("Via")
        .ok_or_else(|| invalid("missing SIP Via"))?
        .value();
    let top = via
        .split(',')
        .next()
        .ok_or_else(|| invalid("missing top SIP Via"))?;
    if top.len() > MAX_ID_LENGTH {
        return Err(invalid("oversized top SIP Via"));
    }
    let mut fields = top.split(';');
    let sent_by = fields.next().unwrap_or_default().trim();
    if !sent_by.starts_with("SIP/2.0/") {
        return Err(invalid("invalid SIP Via"));
    }
    let mut branch = None;
    for field in fields {
        if let Some((name, value)) = field.trim().split_once('=')
            && name.trim().eq_ignore_ascii_case("branch")
        {
            if branch.is_some() {
                return Err(invalid("duplicate SIP Via branch"));
            }
            branch = Some(value.trim());
        }
    }
    let branch = branch
        .filter(|value| value.starts_with("z9hG4bK") && value.len() > 7)
        .ok_or_else(|| invalid("missing SIP transaction branch"))?;
    Ok(ResponseKey {
        call_id: call_id.to_owned(),
        cseq: number,
        method,
        top_via: sent_by.to_owned(),
        branch: branch.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proletarius_sip::{Header, Message, Parser};
    use zeroize::Zeroizing;

    fn request(sequence: u32, method: &str, branch: &str) -> Request {
        let wire = format!(
            "{method} sip:ims.example SIP/2.0\r\nVia: SIP/2.0/TCP host:5060;branch=z9hG4bK{branch}\r\nFrom: <sip:a@ims.example>;tag=a\r\nTo: <sip:b@ims.example>\r\nCall-ID: synthetic-call\r\nCSeq: {sequence} {method}\r\nContent-Length: 0\r\n\r\n"
        );
        let (Message::Request(request), _) =
            Parser::default().parse(wire.as_bytes(), false).unwrap()
        else {
            panic!("request fixture")
        };
        request
    }

    fn response(request: &Request, status: u16) -> RegisteredSipMessage {
        RegisteredSipMessage::Response {
            status,
            reason: "Test".into(),
            version: "SIP/2.0".into(),
            headers: request.data.headers.clone(),
            body: Zeroizing::new(Vec::new()),
        }
    }

    #[test]
    fn provisional_then_final_then_duplicate_completes_once() {
        let mut registry = ResponseRegistry::default();
        let request = request(1, "MESSAGE", "first");
        let ticket = registry.reserve(&request, Duration::from_secs(20)).unwrap();
        assert_eq!(
            registry.correlate(SipConnectionId::Primary, &response(&request, 100)),
            Some(ticket)
        );
        assert_eq!(registry.pending.len(), 1);
        assert_eq!(
            registry.correlate(SipConnectionId::Primary, &response(&request, 202)),
            Some(ticket)
        );
        assert!(registry.pending.is_empty());
        assert_eq!(
            registry.correlate(SipConnectionId::Primary, &response(&request, 202)),
            None
        );
    }

    #[test]
    fn connection_branch_call_id_sequence_and_method_must_all_match() {
        let mut registry = ResponseRegistry::default();
        let original = request(7, "MESSAGE", "one");
        let ticket = registry
            .reserve(&original, Duration::from_secs(20))
            .unwrap();
        assert_eq!(
            registry.correlate(SipConnectionId::Incoming(1), &response(&original, 200)),
            None
        );
        for changed in [
            request(8, "MESSAGE", "one"),
            request(7, "OPTIONS", "one"),
            request(7, "MESSAGE", "two"),
        ] {
            assert_eq!(
                registry.correlate(SipConnectionId::Primary, &response(&changed, 200)),
                None
            );
        }
        let mut changed = original.clone();
        let mut headers = Headers::default();
        for header in changed.data.headers.iter() {
            headers.push(if header.is_named("Call-ID") {
                Header::new("Call-ID", "another-call").unwrap()
            } else {
                header.clone()
            });
        }
        changed.data.headers = headers;
        assert_eq!(
            registry.correlate(SipConnectionId::Primary, &response(&changed, 200)),
            None
        );
        assert_eq!(
            registry.correlate(SipConnectionId::Primary, &response(&original, 403)),
            Some(ticket)
        );
    }

    #[test]
    fn multiple_concurrent_requests_can_complete_in_reverse_order() {
        let mut registry = ResponseRegistry::default();
        let a = request(10, "MESSAGE", "a");
        let b = request(11, "OPTIONS", "b");
        let ta = registry.reserve(&a, Duration::from_secs(20)).unwrap();
        let tb = registry.reserve(&b, Duration::from_secs(20)).unwrap();
        assert_ne!(ta, tb);
        assert_eq!(
            registry.correlate(SipConnectionId::Primary, &response(&b, 200)),
            Some(tb)
        );
        assert_eq!(
            registry.correlate(SipConnectionId::Primary, &response(&a, 200)),
            Some(ta)
        );
    }

    #[test]
    fn deadline_removes_one_wait_once_and_late_reply_cannot_claim_new_attempt() {
        let mut registry = ResponseRegistry::default();
        let old = request(1, "MESSAGE", "old");
        let old_ticket = registry.reserve(&old, Duration::from_secs(1)).unwrap();
        let next = request(2, "MESSAGE", "new");
        let next_ticket = registry.reserve(&next, Duration::from_secs(30)).unwrap();
        assert_eq!(
            registry.expire_one(Instant::now() + Duration::from_secs(2)),
            Some(old_ticket)
        );
        assert_eq!(
            registry.expire_one(Instant::now() + Duration::from_secs(2)),
            None
        );
        assert_eq!(
            registry.correlate(SipConnectionId::Primary, &response(&old, 200)),
            None
        );
        assert_eq!(
            registry.correlate(SipConnectionId::Primary, &response(&next, 200)),
            Some(next_ticket)
        );
    }

    #[test]
    fn expired_response_is_unmatched_even_before_timeout_is_drained() {
        let mut registry = ResponseRegistry::default();
        let request = request(1, "MESSAGE", "late");
        let ticket = registry.reserve(&request, Duration::from_secs(1)).unwrap();
        registry.pending.values_mut().next().unwrap().deadline = Instant::now();
        assert_eq!(
            registry.correlate(SipConnectionId::Primary, &response(&request, 200)),
            None
        );
        assert_eq!(registry.expire_one(Instant::now()), Some(ticket));
    }

    #[test]
    fn capacity_duplicate_and_cancellation_do_not_replace_other_waiters() {
        let mut registry = ResponseRegistry::default();
        let first = request(1, "OPTIONS", "first");
        let ticket = registry.reserve(&first, Duration::from_secs(20)).unwrap();
        assert_eq!(
            registry
                .reserve(&first, Duration::from_secs(20))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        for i in 2..=64 {
            registry
                .reserve(
                    &request(i, "OPTIONS", &i.to_string()),
                    Duration::from_secs(20),
                )
                .unwrap();
        }
        let next = request(65, "OPTIONS", "next");
        assert_eq!(
            registry
                .reserve(&next, Duration::from_secs(20))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        registry.cancel(ticket);
        registry.cancel(ticket);
        registry.reserve(&next, Duration::from_secs(20)).unwrap();
        assert_eq!(registry.pending.len(), 64);
    }

    #[test]
    fn unsupported_methods_timeouts_and_ambiguous_headers_fail_before_admission() {
        let mut registry = ResponseRegistry::default();
        for method in ["INVITE", "ACK", "REGISTER", "BYE"] {
            assert!(
                registry
                    .reserve(&request(1, method, "bad"), Duration::from_secs(20))
                    .is_err()
            );
        }
        let valid = request(1, "MESSAGE", "valid");
        for timeout in [Duration::ZERO, Duration::from_secs(61)] {
            assert!(registry.reserve(&valid, timeout).is_err());
        }
        for name in ["Call-ID", "CSeq"] {
            let mut bad = valid.clone();
            let duplicate = bad.data.headers.get(name).unwrap().clone();
            bad.data.headers.push(duplicate);
            assert!(registry.reserve(&bad, Duration::from_secs(20)).is_err());
        }
        let mut bad = valid.clone();
        bad.method = Method::OPTIONS;
        assert!(registry.reserve(&bad, Duration::from_secs(20)).is_err());
        assert!(registry.pending.is_empty());
    }

    #[test]
    fn malformed_cseq_and_duplicate_via_branch_are_rejected() {
        for (name, value) in [
            ("CSeq", "1 MESSAGE extra"),
            ("CSeq", "+1 MESSAGE"),
            ("CSeq", "2147483648 MESSAGE"),
            (
                "Via",
                "SIP/2.0/TCP host:5060;branch=z9hG4bKa;branch=z9hG4bKb",
            ),
        ] {
            let original = request(1, "MESSAGE", "a");
            let mut headers = Headers::default();
            for header in original.data.headers.iter() {
                headers.push(if header.is_named(name) {
                    Header::new(name, value).unwrap()
                } else {
                    header.clone()
                });
            }
            assert!(response_key(&headers).is_err());
        }
    }
}
