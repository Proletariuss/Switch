//! Reliable-transport INVITE server transactions for non-success finals.
//!
//! These records belong to the signaling owner, rather than a media Call:
//! admission failures and replies from retired dialogs have the same lifetime.
//! TCP has no Timer G; Timer H is 64*T1 and a matching ACK ends Completed
//! immediately (Timer I = 0). The separate tombstone only classifies late ACKs.
use super::*;

pub(super) const MAX_COMPLETED_INVITES: usize = 64;
const TIMER_H: Duration = Duration::from_secs(32);

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub(super) enum CompletedPhase {
    Proceeding,
    Completed,
    Tombstone,
}

pub(super) struct CompletedInvite {
    connection: SipConnectionId,
    call_id: String,
    from_tag: String,
    request_to_tag: Option<String>,
    response_to_tag: Option<String>,
    cseq: u32,
    uri: Uri,
    via_sent_by: String,
    via_branch: String,
    pub(super) phase: CompletedPhase,
    pub(super) response: Option<Response>,
    /// At least the first final write received a successful receipt. A later
    /// replay failure does not erase that historical fact or change the final.
    pub(super) confirmed: bool,
    pub(super) deadline: Option<Instant>,
    cancel_reply: Option<CancelReply>,
}

struct CancelReply {
    response: Response,
    confirmed: bool,
    deadline: Instant,
}

impl Drop for CompletedInvite {
    fn drop(&mut self) {
        if let Some(response) = &mut self.response {
            response.data.body.zeroize();
        }
        if let Some(cancel) = &mut self.cancel_reply {
            cancel.response.data.body.zeroize();
        }
    }
}

fn sws(value: &str) -> &str {
    value.trim_matches(|character| matches!(character, ' ' | '\t'))
}

fn sip_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.' | b'!' | b'%' | b'*' | b'_' | b'+' | b'`' | b'\'' | b'~'
        )
}

fn via_token(value: &str) -> Result<(&str, &str)> {
    let end = value
        .bytes()
        .position(|byte| !sip_token(byte))
        .unwrap_or(value.len());
    if end == 0 {
        return Err(Failure::known("ims_call_via_invalid"));
    }
    Ok(value.split_at(end))
}

fn via_slash(value: &str) -> Result<&str> {
    sws(value)
        .strip_prefix('/')
        .map(sws)
        .ok_or_else(|| Failure::known("ims_call_via_invalid"))
}

fn via_host(host: &str) -> Result<String> {
    let invalid = || Failure::known("ims_call_via_invalid");
    if let Some(address) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        return address
            .parse::<std::net::Ipv6Addr>()
            .map(|address| format!("[{address}]"))
            .map_err(|_| invalid());
    }
    // Parse IPv4 decimal components explicitly: IPv4address permits leading
    // zeros, while their numeric identity does not depend on that spelling.
    if host
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        let octets = host
            .split('.')
            .map(|part| {
                if part.is_empty() || part.len() > 3 {
                    return Err(invalid());
                }
                part.parse::<u8>().map_err(|_| invalid())
            })
            .collect::<Result<Vec<_>>>()?;
        let [a, b, c, d] = octets.as_slice() else {
            return Err(invalid());
        };
        return Ok(format!("{a}.{b}.{c}.{d}"));
    }
    // A DNS host is case-insensitive; retain its label structure and optional
    // final dot rather than equating unrelated host spellings or resolving it.
    let name = host.strip_suffix('.').unwrap_or(host);
    if name.is_empty() || name.len() > 253 {
        return Err(invalid());
    }
    let labels: Vec<&str> = name.split('.').collect();
    if !labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.as_bytes()[0].is_ascii_alphanumeric()
            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) || !labels.last().unwrap().as_bytes()[0].is_ascii_alphabetic()
    {
        return Err(invalid());
    }
    Ok(host.to_ascii_lowercase())
}

fn via_sent_by(value: &str) -> Result<String> {
    let invalid = || Failure::known("ims_call_via_invalid");
    let (protocol, rest) = via_token(sws(value))?;
    let (version, rest) = via_token(via_slash(rest)?)?;
    let (transport, rest) = via_token(via_slash(rest)?)?;
    // Only the registered TCP transport is implemented. Normalize its legal
    // token/LWS spelling, not the independently case-sensitive branch token.
    if !protocol.eq_ignore_ascii_case("SIP")
        || version != "2.0"
        || !transport.eq_ignore_ascii_case("TCP")
    {
        return Err(Failure::known("ims_invite_transport_unsupported"));
    }
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return Err(invalid());
    }
    let sent_by = sws(rest);
    let (host, port) = if sent_by.starts_with('[') {
        let end = sent_by.find(']').ok_or_else(invalid)? + 1;
        (&sent_by[..end], sws(&sent_by[end..]))
    } else {
        sent_by
            .find(':')
            .map_or((sent_by, ""), |colon| sent_by.split_at(colon))
    };
    let host = via_host(sws(host))?;
    let port = if port.is_empty() {
        None
    } else {
        let port = sws(port.strip_prefix(':').ok_or_else(invalid)?);
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        Some(port.parse::<u16>().map_err(|_| invalid())?)
    };
    Ok(match port {
        Some(port) => format!("SIP/2.0/TCP {host}:{port}"),
        None => format!("SIP/2.0/TCP {host}"),
    })
}

fn via_identity(headers: &Headers) -> Result<(String, String)> {
    let via = headers
        .get("Via")
        .ok_or_else(|| Failure::known("ims_call_header_missing"))?
        .value()
        .split(',')
        .next()
        .unwrap_or_default();
    let mut pieces = via.split(';');
    let sent_by = via_sent_by(pieces.next().unwrap_or_default())?;
    let mut branch = None;
    for parameter in pieces {
        let (name, value) = sws(parameter)
            .split_once('=')
            .map_or((sws(parameter), None), |(name, value)| {
                (sws(name), Some(sws(value)))
            });
        if name.eq_ignore_ascii_case("branch") {
            let value = value
                .filter(|value| !value.is_empty() && value.bytes().all(sip_token))
                .ok_or_else(|| Failure::known("ims_call_via_invalid"))?;
            if branch.replace(value).is_some() {
                return Err(Failure::known("ims_call_via_invalid"));
            }
        }
    }
    let branch = branch.ok_or_else(|| Failure::known("ims_call_via_invalid"))?;
    Ok((sent_by, branch.to_owned()))
}

pub(super) fn same_tcp_via_identity(a: &Headers, b: &Headers) -> bool {
    match (via_identity(a), via_identity(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

impl CompletedInvite {
    fn reservation(connection: SipConnectionId, request: &Request) -> Result<Self> {
        let (seq, method) = cseq(&request.data.headers)?;
        if request.method != Method::INVITE || method != Method::INVITE {
            return Err(Failure::known("ims_call_cseq_invalid"));
        }
        let (via_sent_by, via_branch) = via_identity(&request.data.headers)?;
        let from_tag = single(&request.data.headers, "From")?
            .pipe_tag()
            .ok_or_else(|| Failure::known("ims_call_dialog_invalid"))?;
        Ok(Self {
            connection,
            call_id: single(&request.data.headers, "Call-ID")?.to_owned(),
            from_tag,
            request_to_tag: single(&request.data.headers, "To")?.pipe_tag(),
            response_to_tag: None,
            cseq: seq,
            uri: request.uri.clone(),
            via_sent_by,
            via_branch,
            phase: CompletedPhase::Proceeding,
            response: None,
            confirmed: false,
            deadline: None,
            cancel_reply: None,
        })
    }

    fn base_matches(&self, connection: SipConnectionId, request: &Request) -> bool {
        connection == self.connection
            && request.uri == self.uri
            && single(&request.data.headers, "Call-ID").ok() == Some(self.call_id.as_str())
            && single(&request.data.headers, "From").ok().and_then(tag)
                == Some(self.from_tag.as_str())
            && cseq(&request.data.headers)
                .ok()
                .is_some_and(|(seq, method)| seq == self.cseq && method == request.method)
            && via_identity(&request.data.headers).ok()
                == Some((self.via_sent_by.clone(), self.via_branch.clone()))
    }

    fn request_matches(&self, connection: SipConnectionId, request: &Request) -> bool {
        self.base_matches(connection, request)
            && single(&request.data.headers, "To").ok().and_then(tag)
                == self.request_to_tag.as_deref()
    }

    fn ack_matches(&self, connection: SipConnectionId, request: &Request) -> bool {
        request.method == Method::ACK
            && self.phase != CompletedPhase::Proceeding
            && self.base_matches(connection, request)
            && self.response_to_tag.is_some()
            && single(&request.data.headers, "To").ok().and_then(tag)
                == self.response_to_tag.as_deref()
    }

    fn negative_ack_branch_matches(&self, connection: SipConnectionId, request: &Request) -> bool {
        self.phase != CompletedPhase::Proceeding
            && request.method == Method::ACK
            && self.connection == connection
            && single(&request.data.headers, "Call-ID").ok() == Some(self.call_id.as_str())
            && cseq(&request.data.headers).ok() == Some((self.cseq, Method::ACK))
            && via_identity(&request.data.headers).ok()
                == Some((self.via_sent_by.clone(), self.via_branch.clone()))
    }

    fn mark_tombstone(&mut self) {
        self.phase = CompletedPhase::Tombstone;
        if let Some(mut response) = self.response.take() {
            response.data.body.zeroize();
        }
    }

    fn accepted_protection(&self, call: &Call, now: Instant) -> (bool, Option<Instant>) {
        if call.connection != self.connection
            || call.binding.call_id != self.call_id
            || call.local_tag.as_str() != self.response_to_tag.as_deref().unwrap_or_default()
            || call
                .dialog
                .as_ref()
                .map(|dialog| dialog.id.remote_tag.as_str())
                != Some(self.from_tag.as_str())
        {
            return (false, None);
        }
        let mut protected = false;
        let mut next = None;
        if matches!(&call.tracker, Tracker::Incoming(_))
            && call.binding.invite_cseq == self.cseq
            && call
                .initial_reply
                .as_ref()
                .is_some_and(|reply| (200..300).contains(&reply.status))
            && (call.initial_answer.is_some()
                || call
                    .initial_reply_deadline
                    .is_some_and(|deadline| deadline > now))
        {
            protected = true;
            next = call
                .initial_reply_deadline
                .filter(|deadline| *deadline > now);
        }
        for accepted in &call.accepted_reinvites {
            if accepted.cseq == self.cseq
                && (accepted.deadline > now || (!accepted.acked && !call.dialog_terminated))
            {
                protected = true;
                if accepted.deadline > now {
                    next = next.into_iter().chain(Some(accepted.deadline)).max();
                }
            }
        }
        (protected, next)
    }
}

// Keep extraction explicit without retaining a full request body in a ledger
// whose entries can outlive the call and its media resources.
trait OwnedTag {
    fn pipe_tag(&self) -> Option<String>;
}
impl OwnedTag for str {
    fn pipe_tag(&self) -> Option<String> {
        tag(self).map(str::to_owned)
    }
}

impl Service {
    /// Reserve before Call admission, CSeq consumption, or media preparation.
    /// A Proceeding reservation is owned by the TU's decision timer (90s), not
    /// Timer H, which starts only when a final response is sent.
    pub(super) fn reserve_completed_invite(
        &mut self,
        connection: SipConnectionId,
        request: &Request,
    ) -> Result<()> {
        self.prune_completed_invites(Instant::now());
        if self.completed_invites.iter().any(|record| {
            record.request_matches(connection, request) && request.method == Method::INVITE
        }) {
            return Ok(());
        }
        if self.completed_invites.len() >= MAX_COMPLETED_INVITES {
            return Err(Failure::known("ims_invite_reply_capacity"));
        }
        self.completed_invites
            .push_back(CompletedInvite::reservation(connection, request)?);
        Ok(())
    }

    /// Hand the transaction to the separate Accepted lifecycle before a 2xx
    /// write. Never erase a final or its ACK tombstone through this helper.
    pub(super) fn release_completed_invite(
        &mut self,
        connection: SipConnectionId,
        request: &Request,
    ) {
        if let Some(index) = self.completed_invites.iter().position(|record| {
            record.phase == CompletedPhase::Proceeding
                && record.request_matches(connection, request)
        }) {
            self.completed_invites.remove(index);
        }
    }

    pub(super) async fn completed_response(
        &mut self,
        connection: SipConnectionId,
        request: &Request,
        mut response: Response,
    ) -> Result<()> {
        if !(300..700).contains(&response.status) {
            return Err(Failure::known("ims_invite_reply_invalid"));
        }
        self.reserve_completed_invite(connection, request)?;
        let index = self
            .completed_invites
            .iter()
            .position(|record| record.request_matches(connection, request))
            .ok_or_else(|| Failure::known("ims_invite_reply_missing"))?;
        if self.completed_invites[index].phase != CompletedPhase::Proceeding {
            // Callers cannot replace a previously frozen final, even following
            // an unknown write result or a subsequently corrected SDP body.
            if self.completed_invites[index].phase == CompletedPhase::Completed
                && self.completed_invites[index].confirmed
            {
                return self
                    .respond(
                        connection,
                        self.completed_invites[index]
                            .response
                            .as_ref()
                            .unwrap()
                            .clone(),
                    )
                    .await;
            }
            return Ok(());
        }
        if single(&response.data.headers, "To")?.pipe_tag().is_none() {
            let to = format!("{};tag={}", single(&response.data.headers, "To")?, unique());
            response
                .data
                .headers
                .set(Header::new("To", to).map_err(|_| Failure::known("ims_call_header_invalid"))?);
        }
        let record = &mut self.completed_invites[index];
        record.response_to_tag = single(&response.data.headers, "To")?.pipe_tag();
        record.phase = CompletedPhase::Completed;
        record.response = Some(response.clone());
        record.confirmed = false;
        record.deadline = Some(Instant::now() + TIMER_H);
        let write = self.respond(connection, response).await;
        let record = &mut self.completed_invites[index];
        record.confirmed = write.is_ok();
        if write.is_ok() {
            record.deadline = Some(Instant::now() + TIMER_H);
        }
        write
    }

    /// An independent CANCEL transaction can finish while its INVITE remains
    /// Proceeding or Completed. Its write must never acknowledge the INVITE.
    pub(super) async fn completed_cancel_response(
        &mut self,
        connection: SipConnectionId,
        request: &Request,
        response: Response,
    ) -> Result<()> {
        let Some(index) = self.completed_invites.iter().position(|record| {
            request.method == Method::CANCEL && record.request_matches(connection, request)
        }) else {
            return self.respond(connection, response).await;
        };
        if let Some(cancel) = &self.completed_invites[index].cancel_reply {
            return if cancel.confirmed {
                self.respond(connection, cancel.response.clone()).await
            } else {
                Ok(())
            };
        }
        self.completed_invites[index].cancel_reply = Some(CancelReply {
            response: response.clone(),
            confirmed: false,
            deadline: Instant::now() + TIMER_H,
        });
        let write = self.respond(connection, response).await;
        let cancel = self.completed_invites[index].cancel_reply.as_mut().unwrap();
        cancel.confirmed = write.is_ok();
        if write.is_ok() {
            cancel.deadline = Instant::now() + TIMER_H;
        }
        write
    }

    /// Run before Accepted/dialog matching. In particular, a non-2xx ACK uses
    /// the original Via branch; a 2xx ACK deliberately does not.
    pub(super) async fn handle_completed_invite(
        &mut self,
        connection: SipConnectionId,
        request: &Request,
    ) -> Result<bool> {
        self.prune_completed_invites(Instant::now());
        if request.method == Method::ACK {
            // An invalid Via cannot become a 2xx ACK merely because it misses
            // this ledger. Accepted deliberately matches without Via branch.
            if via_identity(&request.data.headers).is_err() {
                return Ok(true);
            }
            if let Some(record) = self
                .completed_invites
                .iter_mut()
                .find(|record| record.ack_matches(connection, request))
            {
                record.mark_tombstone();
                return Ok(true);
            }
            // A non-2xx ACK reuses its INVITE branch, whereas a 2xx ACK is a
            // separate request with a fresh branch. Extra strict URI/tag
            // checks may reject this ACK, but that must not let a known
            // negative branch acknowledge another same-CSeq Accepted reply.
            // Keep the negative transaction waiting for its exact ACK or H.
            if self
                .completed_invites
                .iter()
                .any(|record| record.negative_ack_branch_matches(connection, request))
            {
                return Ok(true);
            }
            return Ok(false);
        }
        if request.method != Method::INVITE && request.method != Method::CANCEL {
            return Ok(false);
        }
        let Some(index) = self
            .completed_invites
            .iter()
            .position(|record| record.request_matches(connection, request))
        else {
            return Ok(false);
        };
        let record = &self.completed_invites[index];
        if request.method == Method::INVITE {
            if record.phase == CompletedPhase::Proceeding {
                return Ok(false);
            }
            if record.phase == CompletedPhase::Completed && record.confirmed {
                self.respond(connection, record.response.as_ref().unwrap().clone())
                    .await?;
            }
            return Ok(true);
        }
        if let Some(cancel) = &record.cancel_reply {
            if cancel.confirmed {
                self.respond(connection, cancel.response.clone()).await?;
            }
            return Ok(true);
        }
        if record.phase == CompletedPhase::Proceeding {
            return Ok(false);
        }
        let (status, reason) = if record.phase == CompletedPhase::Completed {
            (200, "OK")
        } else {
            (481, "Call/Transaction Does Not Exist")
        };
        let response = reply_for(
            request,
            status,
            reason,
            record.response_to_tag.as_deref(),
            vec![],
            None,
        )?;
        self.completed_cancel_response(connection, request, response)
            .await?;
        Ok(true)
    }

    pub(super) fn prune_completed_invites(&mut self, now: Instant) {
        let calls = self.call.iter().chain(self.retired.iter());
        let calls: Vec<&Call> = calls.collect();
        self.completed_invites.retain_mut(|record| {
            if record
                .cancel_reply
                .as_ref()
                .is_some_and(|cancel| cancel.deadline <= now)
            {
                if let Some(mut cancel) = record.cancel_reply.take() {
                    cancel.response.data.body.zeroize();
                }
            }
            if record.phase == CompletedPhase::Proceeding {
                return true;
            }
            if record.deadline.is_some_and(|deadline| deadline > now) {
                return true;
            }
            let protected = calls
                .iter()
                .any(|call| record.accepted_protection(call, now).0);
            if protected || record.cancel_reply.is_some() {
                // H has ended. Only the late-ACK guard remains: never replay
                // the negative final after this transition.
                record.mark_tombstone();
            }
            protected || record.cancel_reply.is_some()
        });
    }

    pub(super) fn completed_next_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        self.completed_invites
            .iter()
            .flat_map(|record| {
                let mut deadlines = Vec::with_capacity(3);
                if let Some(cancel) = &record.cancel_reply {
                    deadlines.push(cancel.deadline);
                }
                if record.phase != CompletedPhase::Proceeding {
                    if let Some(deadline) = record.deadline.filter(|deadline| *deadline > now) {
                        deadlines.push(deadline);
                    } else {
                        let mut protected = false;
                        for call in self.call.iter().chain(self.retired.iter()) {
                            let (active, until) = record.accepted_protection(call, now);
                            protected |= active;
                            deadlines.extend(until);
                        }
                        if !protected && record.cancel_reply.is_none() {
                            // Prompt the owner to prune an expired unprotected
                            // record. Accepted protection and the independent
                            // CANCEL deadline already provide their own wakeup;
                            // repeatedly returning old H would busy-loop.
                            deadlines.extend(record.deadline);
                        }
                    }
                }
                deadlines
            })
            .min()
    }

    pub(super) fn clear_completed_invites(&mut self) {
        self.completed_invites.clear();
    }
}
