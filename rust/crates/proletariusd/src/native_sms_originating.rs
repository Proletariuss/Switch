//! Native originating SMS, driven by the registered session's single dispatcher.
//! No stream reader, JSON command adapter or retransmission worker exists here.
use crate::{
    ims_sms_transactions::{
        NativePreparedPart, NativeReportOwner, NativeSmsRouting, NativeTextPart,
        prepare_native_text,
    },
    native_ims_actor::{Command, Event},
    native_vowifi_supervisor::SessionGeneration,
};
use proletarius_vowifi::{
    message_routing::RegisteredMessageContext,
    registration_coordinator::SipConnectionId,
    sip_session_runtime::{RegisteredSipEvent, RegisteredSipMessage, ResponseTicket},
    sms_transaction::{SmsPartIdentity, SmsResponseDisposition, SmsSubmissionStatus},
};
use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};
use zeroize::Zeroizing;

const RESPONSE_WAIT: Duration = Duration::from_secs(15);
const MAX_ACTIVE_BATCHES: usize = 128;
const RECENT_BATCHES: usize = 256;

pub(crate) struct SessionConfig<'a> {
    pub(crate) generation: SessionGeneration,
    pub(crate) snapshot: &'a RegisteredMessageContext,
    pub(crate) minimum_cseq: u32,
    pub(crate) smsc: &'a str,
    pub(crate) target: &'a str,
    pub(crate) access_network_info: Option<&'a str>,
}
/// Plain input is immediately moved into zeroizing storage. No Debug output.
pub(crate) struct TextSend {
    pub(crate) generation: SessionGeneration,
    pub(crate) batch_id: String,
    pub(crate) recipient: Zeroizing<String>,
    pub(crate) text: Zeroizing<String>,
    pub(crate) request_status_report: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    Prepared,
    AwaitingResponse,
    Accepted,
    Rejected,
    OutcomeUnknown,
    /// A late result resolved one uncertain part, but remaining parts were
    /// deliberately never submitted. Partial success is not a replay license.
    Partial,
    Aborted,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RpResult {
    Ack,
    Error(u8),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Receipt {
    pub(crate) generation: SessionGeneration,
    pub(crate) batch_id: String,
    pub(crate) phase: Phase,
    pub(crate) accepted_parts: u16,
    pub(crate) total_parts: u16,
    pub(crate) sip_status: Option<u16>,
    pub(crate) error: Option<&'static str>,
    /// RP results do not redefine the legacy final-SIP acceptance contract.
    pub(crate) rp_results: Vec<(u16, RpResult)>,
}
impl Receipt {
    pub(crate) fn outcome_unknown(&self) -> bool {
        matches!(self.phase, Phase::OutcomeUnknown | Phase::Partial)
            || (matches!(self.phase, Phase::Rejected | Phase::Aborted) && self.accepted_parts > 0)
    }
}
struct PartRecord {
    identity: SmsPartIdentity,
    via: Zeroizing<String>,
    reference: u8,
    part: u16,
    ticket: Option<ResponseTicket>,
    rp: Option<RpResult>,
}
struct Write {
    reply: oneshot::Receiver<io::Result<ResponseTicket>>,
    deadline: Instant,
}
struct Batch {
    receipt: Receipt,
    parts: VecDeque<NativeTextPart>,
    prepared: Option<NativePreparedPart>,
    records: Vec<PartRecord>,
    write: Option<Write>,
    deadline: Option<Instant>,
}
pub(crate) struct Originating {
    owner: Arc<NativeReportOwner>,
    generation: SessionGeneration,
    public_identity: Zeroizing<String>,
    local_host: Zeroizing<String>,
    local_port: u16,
    minimum_cseq: u32,
    routes: Zeroizing<Vec<String>>,
    security: Option<Zeroizing<String>>,
    pani: Option<Zeroizing<String>>,
    smsc: Zeroizing<String>,
    target: Zeroizing<String>,
    commands: mpsc::Sender<Command>,
    stop_signal: watch::Receiver<bool>,
    lease_lost: watch::Receiver<bool>,
    stopped: bool,
    active: Option<String>,
    batches: HashMap<String, Batch>,
    recent: VecDeque<String>,
}
impl Originating {
    pub(crate) fn new(
        owner: Arc<NativeReportOwner>,
        config: SessionConfig<'_>,
        commands: mpsc::Sender<Command>,
        stop: watch::Receiver<bool>,
        lease_lost: watch::Receiver<bool>,
    ) -> Result<Self, &'static str> {
        if config.smsc.len() > 64
            || config.target.len() > 2048
            || config.target.bytes().any(|b| b.is_ascii_control())
        {
            return Err("native_sms_routing_unavailable");
        }
        owner.bind_generation(config.generation)?;
        Ok(Self {
            owner,
            generation: config.generation,
            public_identity: Zeroizing::new(config.snapshot.routing().identity().to_owned()),
            local_host: Zeroizing::new(config.snapshot.local_ip().to_string()),
            local_port: config.snapshot.local_port(),
            minimum_cseq: config.minimum_cseq,
            routes: Zeroizing::new(config.snapshot.routing().routes().to_vec()),
            security: config
                .snapshot
                .security_verify()
                .map(|v| Zeroizing::new(v.to_owned())),
            pani: config
                .access_network_info
                .map(|v| Zeroizing::new(v.to_owned())),
            smsc: Zeroizing::new(config.smsc.to_owned()),
            target: Zeroizing::new(config.target.to_owned()),
            commands,
            stop_signal: stop,
            lease_lost,
            stopped: false,
            active: None,
            batches: HashMap::new(),
            recent: VecDeque::new(),
        })
    }
    pub(crate) fn generation(&self) -> SessionGeneration {
        self.generation
    }
    fn running(&self) -> bool {
        !self.stopped
            && !*self.stop_signal.borrow()
            && !*self.lease_lost.borrow()
            && self.stop_signal.has_changed().is_ok()
            && self.lease_lost.has_changed().is_ok()
    }
    fn routing(&self) -> NativeSmsRouting<'_> {
        NativeSmsRouting {
            generation: self.generation,
            minimum_cseq: self.minimum_cseq,
            public_identity: &self.public_identity,
            local_host: &self.local_host,
            local_port: self.local_port,
            route_headers: &self.routes,
            security_verify: self.security.as_deref().map(|v| v.as_str()),
            access_network_info: self.pani.as_deref().map(|v| v.as_str()),
        }
    }
    pub(crate) fn start(&mut self, input: TextSend) -> Result<Receipt, &'static str> {
        if input.generation != self.generation {
            return Err("native_sms_session_mismatch");
        }
        if !self.running() {
            return Err("native_sms_session_stopped");
        }
        // Missing optional SIM SMS routing must not prevent ordinary IMS registration.
        // Reject only the originating action before admitting a batch or writing SIP.
        if self.smsc.trim().is_empty() || self.target.trim().is_empty() {
            return Err("native_sms_routing_unavailable");
        }
        if self.batches.contains_key(&input.batch_id) {
            return Err("sms_batch_already_started");
        }
        if self.active.is_some() {
            return Err("another_sms_batch_active");
        }
        if input.text.len() > 128 * 1024 || input.recipient.len() > 128 {
            return Err("invalid_sms_text");
        }
        if self
            .batches
            .values()
            .filter(|b| {
                matches!(
                    b.receipt.phase,
                    Phase::Prepared | Phase::AwaitingResponse | Phase::OutcomeUnknown
                )
            })
            .count()
            >= MAX_ACTIVE_BATCHES
        {
            return Err("transaction_capacity");
        }
        let parts = prepare_native_text(
            &input.recipient,
            &self.smsc,
            &input.text,
            input.request_status_report,
        )?;
        let prepared = self.owner.prepare_part(
            &input.batch_id,
            parts.len(),
            &self.target,
            &self.routing(),
            parts.first().ok_or("invalid_sms_text")?.rpdu.as_slice(),
        )?;
        self.minimum_cseq = prepared.identity.cseq;
        let receipt = Receipt {
            generation: self.generation,
            batch_id: input.batch_id.clone(),
            phase: Phase::Prepared,
            accepted_parts: 0,
            total_parts: parts.len() as u16,
            sip_status: None,
            error: None,
            rp_results: vec![],
        };
        let id = input.batch_id;
        self.batches.insert(
            id.clone(),
            Batch {
                receipt,
                parts: parts.into(),
                prepared: Some(prepared),
                records: vec![],
                write: None,
                deadline: Some(Instant::now() + RESPONSE_WAIT),
            },
        );
        self.active = Some(id.clone());
        self.tick()?;
        self.snapshot(&id).ok_or("sms_batch_not_found")
    }
    pub(crate) fn snapshot(&self, batch_id: &str) -> Option<Receipt> {
        self.batches.get(batch_id).map(|b| b.receipt.clone())
    }
    pub(crate) fn cancel(
        &mut self,
        generation: SessionGeneration,
        batch_id: &str,
    ) -> Result<Receipt, &'static str> {
        if generation != self.generation {
            return Err("native_sms_session_mismatch");
        }
        let phase = self
            .batches
            .get(batch_id)
            .ok_or("sms_batch_not_found")?
            .receipt
            .phase;
        match phase {
            Phase::Prepared => self.abort_ready(batch_id, "native_sms_cancelled")?,
            Phase::AwaitingResponse => self.freeze_unknown(batch_id, "native_sms_cancelled")?,
            _ => {}
        }
        self.snapshot(batch_id).ok_or("sms_batch_not_found")
    }
    pub(crate) fn snapshots(&self) -> Vec<Receipt> {
        self.batches.values().map(|b| b.receipt.clone()).collect()
    }
    pub(crate) fn is_busy(&self) -> bool {
        self.active.is_some()
    }
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.batches
            .values()
            .flat_map(|batch| {
                [
                    batch.deadline,
                    (batch.write.is_some() || batch.prepared.is_some())
                        .then(|| Instant::now() + Duration::from_millis(25)),
                ]
            })
            .flatten()
            .min()
    }
    /// Poll actor write completions/deadlines and queue at most one next part.
    /// This never waits for a reply or reads the SIP stream.
    pub(crate) fn tick(&mut self) -> Result<(), &'static str> {
        if !self.running() {
            self.stop();
            return Ok(());
        }
        let ids = self.batches.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let next = self.batches.get_mut(&id).and_then(|batch| {
                let write = batch.write.as_mut()?;
                match write.reply.try_recv() {
                    Ok(Ok(ticket)) => Some(Ok(ticket)),
                    Ok(Err(_)) | Err(oneshot::error::TryRecvError::Closed) => {
                        Some(Err("native_sms_write_unknown"))
                    }
                    Err(oneshot::error::TryRecvError::Empty)
                        if write.deadline <= Instant::now() =>
                    {
                        Some(Err("native_sms_write_unknown"))
                    }
                    Err(oneshot::error::TryRecvError::Empty) => None,
                }
            });
            match next {
                Some(Ok(ticket)) => {
                    let batch = self.batches.get_mut(&id).unwrap();
                    batch.write = None;
                    if let Some(record) = batch.records.last_mut() {
                        record.ticket = Some(ticket);
                    }
                }
                Some(Err(code)) => {
                    self.freeze_unknown(&id, code)?;
                }
                None => {}
            }
            let Some(batch) = self.batches.get(&id) else {
                continue;
            };
            if batch.deadline.is_some_and(|d| d <= Instant::now()) {
                match batch.receipt.phase {
                    Phase::Prepared => self.abort_ready(&id, "native_sms_queue_timeout")?,
                    Phase::AwaitingResponse => {
                        self.freeze_unknown(&id, "native_sms_response_unknown")?
                    }
                    _ => {}
                }
            }
        }
        let Some(id) = self.active.clone() else {
            return Ok(());
        };
        if self.batches.get(&id).unwrap().receipt.phase != Phase::Prepared {
            return Ok(());
        }
        let sender = self.commands.clone();
        let permit = match sender.try_reserve() {
            Ok(permit) => permit,
            Err(mpsc::error::TrySendError::Full(_)) => return Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.abort_ready(&id, "native_sms_actor_closed")?;
                return Ok(());
            }
        };
        // Slot reservation precedes state submission; after submission exactly
        // one actor command owns the request, even if its receipt is abandoned.
        if !self.running() {
            drop(permit);
            self.stop();
            return Ok(());
        }
        let batch = self.batches.get_mut(&id).unwrap();
        let prepared = batch.prepared.take().ok_or("sms_part_not_prepared")?;
        if batch.parts.front().is_none() {
            return Err("sms_part_not_prepared");
        }
        let via = prepared
            .request
            .data
            .headers
            .get("Via")
            .ok_or("invalid_sms_context")?
            .value()
            .to_owned();
        self.owner.submit_part(&id, &prepared.identity)?;
        let part = batch
            .parts
            .pop_front()
            .expect("validated prepared text part");
        let (reply, receive) = oneshot::channel();
        batch.records.push(PartRecord {
            identity: prepared.identity,
            via: Zeroizing::new(via),
            reference: part.reference,
            part: prepared.part,
            ticket: None,
            rp: None,
        });
        batch.receipt.phase = Phase::AwaitingResponse;
        batch.deadline = Some(Instant::now() + RESPONSE_WAIT);
        batch.write = Some(Write {
            reply: receive,
            deadline: Instant::now() + RESPONSE_WAIT,
        });
        permit.send(Command::Send {
            generation: self.generation,
            request: prepared.request,
            timeout: RESPONSE_WAIT,
            reply,
        });
        Ok(())
    }
    fn freeze_unknown(&mut self, id: &str, code: &'static str) -> Result<(), &'static str> {
        let batch = self.batches.get_mut(id).ok_or("sms_batch_not_found")?;
        if matches!(
            batch.receipt.phase,
            Phase::AwaitingResponse | Phase::OutcomeUnknown
        ) {
            let state = self.owner.unknown_part(id)?;
            apply_status(&mut batch.receipt, &state.status);
            batch.receipt.error = Some(code);
            batch.parts.clear();
            batch.write = None;
            batch.deadline = None;
            if self.active.as_deref() == Some(id) {
                self.active = None;
            }
        }
        Ok(())
    }
    fn abort_ready(&mut self, id: &str, code: &'static str) -> Result<(), &'static str> {
        self.owner.abort_part(id)?;
        let batch = self.batches.get_mut(id).ok_or("sms_batch_not_found")?;
        batch.receipt.phase = if batch.receipt.accepted_parts == 0 {
            Phase::Aborted
        } else {
            Phase::Partial
        };
        batch.receipt.error = Some(code);
        batch.parts.clear();
        batch.prepared = None;
        batch.deadline = None;
        if self.active.as_deref() == Some(id) {
            self.active = None;
        }
        self.remember(id);
        Ok(())
    }
    fn remember(&mut self, id: &str) {
        if !self.recent.iter().any(|v| v == id) {
            self.recent.push_back(id.to_owned());
        }
        while self.recent.len() > RECENT_BATCHES {
            if let Some(id) = self.recent.pop_front() {
                self.batches.remove(&id);
            }
        }
    }
    /// Stop freezes already-submitted work; unsent parts are never sent later.
    /// The actor/session owner still performs the actual transport drain.
    pub(crate) fn stop(&mut self) {
        self.stopped = true;
        for id in self.batches.keys().cloned().collect::<Vec<_>>() {
            let Some(phase) = self.batches.get(&id).map(|batch| batch.receipt.phase) else {
                continue;
            };
            let result = match phase {
                Phase::Prepared => self.abort_ready(&id, "native_sms_session_stopped"),
                Phase::AwaitingResponse => self.freeze_unknown(&id, "native_sms_session_stopped"),
                _ => Ok(()),
            };
            if let Err(code) = result
                && let Some(batch) = self.batches.get_mut(&id)
            {
                batch.receipt.error = Some(code);
            }
        }
        self.active = None;
    }
    /// Same-generation final/late SIP results and RP reports, never a stream read.
    /// A consumed incoming RP MESSAGE still requires the dispatcher to send its
    /// ordinary SIP response; it must not be archived as an incoming RP-DATA.
    pub(crate) fn process(&mut self, event: &Event) -> Result<bool, &'static str> {
        if event.generation != self.generation {
            return Ok(false);
        }
        self.tick()?;
        if let RegisteredSipEvent::Message {
            connection: SipConnectionId::Primary,
            message,
            ..
        } = &event.event
            && let RegisteredSipMessage::Request {
                method: proletarius_sip::Method::MESSAGE,
                headers,
                body,
                ..
            } = message.as_ref()
        {
            return self.process_rp(headers, body);
        }
        let ids = self.batches.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let batch = self.batches.get(&id).unwrap();
            if !matches!(
                batch.receipt.phase,
                Phase::AwaitingResponse | Phase::OutcomeUnknown
            ) {
                continue;
            }
            let Some(record) = batch.records.last() else {
                continue;
            };
            let matched = match &event.event {
                RegisteredSipEvent::ResponseTimedOut { ticket } => record.ticket == Some(*ticket),
                RegisteredSipEvent::Message {
                    connection: SipConnectionId::Primary,
                    message,
                    response_ticket,
                } => {
                    let RegisteredSipMessage::Response { headers, .. } = message.as_ref() else {
                        continue;
                    };
                    let identity_matches = headers
                        .get("Call-ID")
                        .is_some_and(|h| h.value() == record.identity.call_id)
                        && headers
                            .get("CSeq")
                            .is_some_and(|h| matching_cseq(h.value(), record.identity.cseq))
                        && headers
                            .get("Via")
                            .is_some_and(|h| matching_via(record.via.as_str(), h.value()));
                    identity_matches
                        && match response_ticket {
                            Some(ticket) => {
                                record.ticket.is_none() || record.ticket == Some(*ticket)
                            }
                            None => batch.receipt.phase == Phase::OutcomeUnknown,
                        }
                }
                _ => false,
            };
            if !matched {
                continue;
            }
            if matches!(event.event, RegisteredSipEvent::ResponseTimedOut { .. }) {
                self.freeze_unknown(&id, "native_sms_response_unknown")?;
                return Ok(true);
            }
            let observed = self.owner.observe_part(&id, &event.event)?;
            if observed.disposition == SmsResponseDisposition::Ignored {
                return Ok(false);
            }
            let late = matches!(
                observed.disposition,
                SmsResponseDisposition::LateResultResolved { .. }
            );
            let batch = self.batches.get_mut(&id).unwrap();
            apply_status(&mut batch.receipt, &observed.state.status);
            if let RegisteredSipEvent::Message { message, .. } = &event.event
                && let RegisteredSipMessage::Response { status, .. } = message.as_ref()
                && *status >= 200
            {
                batch.receipt.sip_status = Some(*status);
            }
            if observed.disposition == SmsResponseDisposition::Provisional {
                return Ok(true);
            }
            batch.write = None;
            batch.deadline = None;
            if matches!(observed.state.status, SmsSubmissionStatus::Ready { .. }) {
                if late || !self.running() {
                    self.abort_ready(&id, "native_sms_partial_submission")?;
                } else {
                    let rpdu = self
                        .batches
                        .get(&id)
                        .unwrap()
                        .parts
                        .front()
                        .ok_or("sms_part_not_prepared")?;
                    let next = self.owner.prepare_part(
                        &id,
                        observed.state.total_parts as usize,
                        &self.target,
                        &self.routing(),
                        rpdu.rpdu.as_slice(),
                    );
                    match next {
                        Ok(prepared) => {
                            self.minimum_cseq = prepared.identity.cseq;
                            let batch = self.batches.get_mut(&id).unwrap();
                            batch.prepared = Some(prepared);
                            batch.receipt.phase = Phase::Prepared;
                            batch.deadline = Some(Instant::now() + RESPONSE_WAIT);
                        }
                        Err(code) => {
                            self.abort_ready(&id, code)?;
                        }
                    }
                    self.tick()?;
                }
            } else {
                let batch = self.batches.get_mut(&id).unwrap();
                batch.parts.clear();
                batch.prepared = None;
                if self.active.as_deref() == Some(&id) {
                    self.active = None;
                }
                self.remember(&id);
            }
            return Ok(true);
        }
        Ok(false)
    }
    fn process_rp(
        &mut self,
        headers: &proletarius_sip::Headers,
        body: &[u8],
    ) -> Result<bool, &'static str> {
        if headers.all("Content-Type").count() != 1
            || headers.all("In-Reply-To").count() > 1
            || !headers.get("Content-Type").is_some_and(|h| {
                h.value()
                    .split(';')
                    .next()
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/vnd.3gpp.sms"))
            })
            || !matches!(body.first(), Some(0x03 | 0x05))
        {
            return Ok(false);
        }
        let report =
            proletarius_sms::DeliveryReport::parse(body).map_err(|_| "invalid_sms_rp_report")?;
        let reply = headers.get("In-Reply-To").map(|h| h.value());
        let mut matches = vec![];
        for (id, batch) in &self.batches {
            for (i, part) in batch.records.iter().enumerate() {
                if part.reference == report.reference()
                    && reply.is_none_or(|call| call == part.identity.call_id)
                {
                    matches.push((id.clone(), i));
                }
            }
        }
        // A reused 8-bit RP reference alone is not enough to claim another SMS.
        if matches.len() != 1 {
            return Ok(false);
        }
        let (id, i) = matches.pop().unwrap();
        let batch = self.batches.get_mut(&id).unwrap();
        let record = &mut batch.records[i];
        let result = match report {
            proletarius_sms::DeliveryReport::Ack { .. } => RpResult::Ack,
            proletarius_sms::DeliveryReport::Error { cause, .. } => RpResult::Error(cause),
        };
        if record.rp.is_none() {
            record.rp = Some(result);
            batch.receipt.rp_results.push((record.part, result));
        }
        Ok(true)
    }
}
impl Drop for Originating {
    fn drop(&mut self) {
        self.stop();
    }
}
fn apply_status(receipt: &mut Receipt, status: &SmsSubmissionStatus) {
    match status {
        SmsSubmissionStatus::Ready { accepted_parts, .. } => {
            receipt.accepted_parts = *accepted_parts;
            receipt.phase = Phase::Prepared;
        }
        SmsSubmissionStatus::AwaitingResponse { accepted_parts, .. } => {
            receipt.accepted_parts = *accepted_parts;
            receipt.phase = Phase::AwaitingResponse;
        }
        SmsSubmissionStatus::OutcomeUnknown { accepted_parts, .. } => {
            receipt.accepted_parts = *accepted_parts;
            receipt.phase = Phase::OutcomeUnknown;
        }
        SmsSubmissionStatus::Rejected {
            accepted_parts,
            status,
            ..
        } => {
            receipt.accepted_parts = *accepted_parts;
            receipt.phase = Phase::Rejected;
            receipt.sip_status = Some(*status);
        }
        SmsSubmissionStatus::Accepted { parts } => {
            receipt.accepted_parts = *parts;
            receipt.phase = Phase::Accepted;
        }
    }
}

fn matching_cseq(value: &str, expected: u32) -> bool {
    let mut fields = value.split_ascii_whitespace();
    fields
        .next()
        .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()) && n.parse::<u32>() == Ok(expected))
        && fields.next() == Some("MESSAGE")
        && fields.next().is_none()
}
fn matching_via(expected: &str, actual: &str) -> bool {
    fn key(value: &str) -> Option<(&str, &str)> {
        let mut fields = value.split(',').next()?.split(';');
        let sent_by = fields.next()?.trim();
        let mut branch = None;
        for part in fields {
            let Some((name, value)) = part.trim().split_once('=') else {
                continue;
            };
            if name.eq_ignore_ascii_case("branch") {
                if branch.is_some() || value.is_empty() {
                    return None;
                }
                branch = Some(value.trim());
            }
        }
        Some((sent_by, branch?))
    }
    key(expected).is_some() && key(expected) == key(actual)
}
#[cfg(test)]
pub(crate) mod tests;
