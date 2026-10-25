//! One post-registration dispatcher owns SMS, voice and USSD event consumers.
//! It never reads a socket: the generation-bound IMS actor is the sole reader
//! and writer. Dropping an HTTP/control waiter cannot cancel accepted work.
use crate::{
    native_ims_actor::{self, Event, IncomingReply},
    native_ims_calls,
    native_sms_consumer::{RegisteredConsumer, SessionOutcome},
    native_sms_originating::{Originating, Receipt, TextSend},
    native_vowifi_supervisor::SessionGeneration,
};
use proletarius_vowifi::{
    sip_session_runtime::{RegisteredSipEvent, RegisteredSipMessage},
    sms_transaction::DeliveryReportState,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};

const CAPACITY: usize = 32;
const REPORT_DRAIN: Duration = Duration::from_secs(30);

pub(crate) enum CallCommand {
    Dial(String),
    Answer,
    Reject,
    Hangup,
    Dtmf(char),
    Snapshot,
}
pub(crate) enum UssdCommand {
    Start(String),
    Continue { session_id: String, input: String },
    Cancel { session_id: String },
}
/// The dispatcher returns a pending network result without awaiting it: only
/// the same event loop can advance the USSD dialog and complete that receiver.
pub(crate) enum UssdReply {
    Pending(native_ims_calls::ussd::Pending),
    Complete(native_ims_calls::ussd::UssdResult),
}
pub(crate) enum Control {
    /// Atomic idle check and admission closure in the same event loop that
    /// receives INVITE. Success is permission to await cleanup, not RF release.
    PrepareProfileSwitch {
        generation: SessionGeneration,
        reply: oneshot::Sender<Result<(), Failure>>,
    },
    Ussd {
        generation: SessionGeneration,
        command: UssdCommand,
        reply: oneshot::Sender<Result<UssdReply, Failure>>,
    },
    Call {
        generation: SessionGeneration,
        command: CallCommand,
        reply: oneshot::Sender<Result<native_ims_calls::Snapshot, Failure>>,
    },
    SendText {
        input: TextSend,
        reply: oneshot::Sender<Result<Receipt, Failure>>,
    },
    SmsCancel {
        generation: SessionGeneration,
        batch_id: String,
        reply: oneshot::Sender<Result<Receipt, Failure>>,
    },
    SmsStatus {
        generation: SessionGeneration,
        batch_id: String,
        reply: oneshot::Sender<Result<Option<Receipt>, Failure>>,
    },
}
#[derive(Debug, Clone)]
pub(crate) struct Failure {
    pub(crate) code: &'static str,
    pub(crate) uncertain: bool,
    pub(crate) sip_status: Option<u16>,
}
impl From<&'static str> for Failure {
    fn from(code: &'static str) -> Self {
        Self {
            code,
            uncertain: false,
            sip_status: None,
        }
    }
}
impl From<native_ims_calls::Failure> for Failure {
    fn from(value: native_ims_calls::Failure) -> Self {
        Self {
            code: value.code,
            uncertain: value.uncertain,
            sip_status: value.sip_status,
        }
    }
}

/// Owned by the same service as the actor task, never by a UI request.
pub(crate) struct Consumers {
    incoming: RegisteredConsumer,
    outgoing: Originating,
    calls: native_ims_calls::Service,
    profile_switch: bool,
    heartbeat_health: Option<proletarius_vowifi::sip_session_runtime::HeartbeatHealth>,
}
pub(crate) enum Notice {
    HeartbeatHealthChanged { healthy: bool },
    Sms(SessionOutcome),
    Unhandled(Box<Event>),
    Failed(Failure),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopReason {
    Requested,
    LeaseLost,
    ActorClosed,
    ObserverGone,
    RenewalDue,
    DeadlineFailed,
}
pub(crate) struct Exit {
    pub(crate) generation: SessionGeneration,
    pub(crate) reason: StopReason,
    pub(crate) undelivered_notice: Option<Notice>,
    /// At most the SMS and call timer can fail in one turn. Preserve both
    /// separately from a backpressured notice while controlling session exit.
    pub(crate) timer_failures: Vec<Failure>,
    pub(crate) pending_reports: Vec<(
        proletarius_vowifi::sip_session_runtime::ResponseTicket,
        DeliveryReportState,
    )>,
    pub(crate) sms: Vec<Receipt>,
    pub(crate) media_cleanup: Result<(), native_ims_calls::Failure>,
}

pub(crate) fn controls() -> (mpsc::Sender<Control>, mpsc::Receiver<Control>) {
    mpsc::channel(CAPACITY)
}
fn signaled(stop: &watch::Receiver<bool>, lease: &watch::Receiver<bool>) -> Option<StopReason> {
    if *stop.borrow() || stop.has_changed().is_err() {
        Some(StopReason::Requested)
    } else if *lease.borrow() || lease.has_changed().is_err() {
        Some(StopReason::LeaseLost)
    } else {
        None
    }
}
fn stale(generation: SessionGeneration, expected: SessionGeneration) -> Result<(), Failure> {
    if generation == expected {
        Ok(())
    } else {
        Err("native_session_stale_generation".into())
    }
}
fn reject(control: Control, code: &'static str) {
    match control {
        Control::PrepareProfileSwitch { reply, .. } => {
            let _ = reply.send(Err(code.into()));
        }
        Control::Ussd { reply, .. } => {
            let _ = reply.send(Err(code.into()));
        }
        Control::SmsCancel { reply, .. } => {
            let _ = reply.send(Err(code.into()));
        }
        Control::Call { reply, .. } => {
            let _ = reply.send(Err(code.into()));
        }
        Control::SendText { reply, .. } => {
            let _ = reply.send(Err(code.into()));
        }
        Control::SmsStatus { reply, .. } => {
            let _ = reply.send(Err(code.into()));
        }
    }
}

impl Consumers {
    pub(crate) fn new(
        incoming: RegisteredConsumer,
        outgoing: Originating,
        calls: native_ims_calls::Service,
    ) -> Result<Self, Failure> {
        let generation = incoming.generation();
        stale(outgoing.generation(), generation)?;
        stale(calls.generation(), generation)?;
        Ok(Self {
            incoming,
            outgoing,
            calls,
            heartbeat_health: None,
            profile_switch: false,
        })
    }

    pub(crate) fn with_heartbeat_health(
        mut self,
        health: proletarius_vowifi::sip_session_runtime::HeartbeatHealth,
    ) -> Self {
        self.heartbeat_health = Some(health);
        self
    }
    fn healthy(&self) -> bool {
        self.heartbeat_health
            .as_ref()
            .is_none_or(|health| health.is_healthy())
    }

    async fn control(&mut self, control: Control, renewing: bool) {
        let generation = self.incoming.generation();
        match control {
            Control::PrepareProfileSwitch {
                generation: requested,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let result = stale(requested, generation).and_then(|()| {
                    if matches!(
                        self.calls.snapshot().state.as_str(),
                        "idle" | "ended" | "failed" | "unavailable"
                    ) {
                        self.profile_switch = true;
                        Ok(())
                    } else {
                        Err("module_vowifi_call_active".into())
                    }
                });
                let _ = reply.send(result);
            }
            Control::Ussd {
                generation: requested,
                command,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let result = if let Err(error) = stale(requested, generation) {
                    Err(error)
                } else if !self.healthy() && matches!(&command, UssdCommand::Start(_)) {
                    Err("native_ims_heartbeat_degraded".into())
                } else if renewing && matches!(&command, UssdCommand::Start(_)) {
                    Err("native_session_renewing".into())
                } else {
                    match command {
                        UssdCommand::Start(input) => self
                            .calls
                            .start_ussd(generation, &input)
                            .await
                            .map(UssdReply::Pending)
                            .map_err(Into::into),
                        UssdCommand::Continue { session_id, input } => self
                            .calls
                            .continue_ussd(generation, &session_id, &input)
                            .await
                            .map(UssdReply::Pending)
                            .map_err(Into::into),
                        UssdCommand::Cancel { session_id } => self
                            .calls
                            .cancel_ussd(generation, &session_id)
                            .await
                            .map(UssdReply::Complete)
                            .map_err(Into::into),
                    }
                };
                let _ = reply.send(result);
            }
            Control::SmsCancel {
                generation: requested,
                batch_id,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let result = stale(requested, generation).and_then(|()| {
                    self.outgoing
                        .cancel(generation, &batch_id)
                        .map_err(Into::into)
                });
                let _ = reply.send(result);
            }
            Control::Call {
                generation: requested,
                command,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let result = if let Err(error) = stale(requested, generation) {
                    Err(error)
                } else if !self.healthy() && matches!(&command, CallCommand::Dial(_)) {
                    Err("native_ims_heartbeat_degraded".into())
                } else if renewing && matches!(&command, CallCommand::Dial(_)) {
                    Err("native_session_renewing".into())
                } else {
                    match command {
                        CallCommand::Dial(peer) => {
                            self.calls.dial(generation, &peer).await.map_err(Into::into)
                        }
                        CallCommand::Answer => {
                            self.calls.answer(generation).await.map_err(Into::into)
                        }
                        CallCommand::Reject => {
                            self.calls.reject(generation).await.map_err(Into::into)
                        }
                        CallCommand::Hangup => {
                            self.calls.hangup(generation).await.map_err(Into::into)
                        }
                        CallCommand::Dtmf(digit) => self
                            .calls
                            .dtmf(generation, digit)
                            .await
                            .map(|()| self.calls.snapshot())
                            .map_err(Into::into),
                        CallCommand::Snapshot => Ok(self.calls.snapshot()),
                    }
                };
                let _ = reply.send(result);
            }
            Control::SendText { input, reply } => {
                if reply.is_closed() {
                    return;
                }
                let result = stale(input.generation, generation).and_then(|()| {
                    if !self.healthy() {
                        Err("native_ims_heartbeat_degraded".into())
                    } else if renewing {
                        Err("native_session_renewing".into())
                    } else {
                        self.outgoing.start(input).map_err(Into::into)
                    }
                });
                let _ = reply.send(result);
            }
            Control::SmsStatus {
                generation: requested,
                batch_id,
                reply,
            } => {
                let _ = reply
                    .send(stale(requested, generation).map(|()| self.outgoing.snapshot(&batch_id)));
            }
        }
    }

    async fn event(
        &mut self,
        event: Event,
        commands: &mpsc::Sender<native_ims_actor::Command>,
    ) -> Result<Option<Notice>, Failure> {
        // Delayed events from a retired actor carry no authority over this
        // consumer generation and must not create I/O or a blocking notice.
        if event.generation != self.incoming.generation() {
            return Ok(None);
        }
        if let RegisteredSipEvent::HeartbeatHealthChanged { healthy } = &event.event {
            return Ok(Some(Notice::HeartbeatHealthChanged { healthy: *healthy }));
        }
        if self.outgoing.process(&event)? {
            // RP-ACK/ERROR is a separate inbound transaction. Acknowledge the
            // correlated report once, without archiving or reflecting an RP-ACK.
            if let RegisteredSipEvent::Message {
                connection,
                message,
                ..
            } = event.event
                && matches!(
                    message.as_ref(),
                    RegisteredSipMessage::Request {
                        method: proletarius_sip::Method::MESSAGE,
                        ..
                    }
                )
                && event.reply_state == IncomingReply::Unanswered
            {
                let (reply, result) = oneshot::channel();
                commands
                    .send(native_ims_actor::Command::Respond {
                        generation: event.generation,
                        connection,
                        request: message,
                        status: 200,
                        reason: "OK".into(),
                        reply,
                    })
                    .await
                    .map_err(|_| Failure::from("native_rp_report_actor_closed"))?;
                result
                    .await
                    .map_err(|_| Failure {
                        code: "native_rp_response_unknown",
                        uncertain: true,
                        sip_status: None,
                    })?
                    .map_err(|_| Failure {
                        code: "native_rp_response_unknown",
                        uncertain: true,
                        sip_status: None,
                    })?;
            }
            return Ok(None);
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
            .ok_or("native_sms_clock_invalid")?;
        match self.incoming.process(event, timestamp).await? {
            SessionOutcome::Unhandled(event) => {
                if self.calls.handle_event(&event).await? {
                    Ok(None)
                } else {
                    Ok(Some(Notice::Unhandled(event)))
                }
            }
            outcome => Ok(Some(Notice::Sms(outcome))),
        }
    }

    /// Runs the sole post-registration consumer. A due renewal waits for live
    /// voice and bounds the pending SMS drain; unknown submissions survive exit.
    /// The parent must await this result before releasing the reader lease.
    pub(crate) async fn run(
        mut self,
        mut events: mpsc::Receiver<Event>,
        commands: mpsc::Sender<native_ims_actor::Command>,
        mut controls: mpsc::Receiver<Control>,
        notices: mpsc::Sender<Notice>,
    ) -> Exit {
        let generation = self.incoming.generation();
        let mut stop = self.incoming.stop_signal();
        let mut lease = self.incoming.lease_signal();
        let mut pending = None;
        let mut renewal = None;
        let mut controls_open = true;
        let mut timer_failures = Vec::with_capacity(2);
        let reason = loop {
            if self.profile_switch {
                break StopReason::Requested;
            }
            if let Some(reason) = signaled(&stop, &lease) {
                break reason;
            }
            if let Some(deadline) = renewal {
                if !self.calls.active()
                    && ((!self.outgoing.is_busy() && self.incoming.pending_reports() == 0)
                        || Instant::now() >= deadline)
                {
                    break StopReason::RenewalDue;
                }
            }
            let deadline = [
                self.outgoing.next_deadline(),
                self.calls.next_deadline(),
                renewal.filter(|_| !self.calls.active()),
            ]
            .into_iter()
            .flatten()
            .min();
            let wait = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            // Backpressure preserves the exact notice while still allowing
            // user call controls and stop. No second event consumer is spawned.
            if pending.is_some() {
                tokio::select! {
                    biased;
                    _ = stop.changed() => {},
                    _ = lease.changed() => {},
                    slot = notices.reserve() => match slot {
                        Ok(slot) => { slot.send(pending.take().expect("pending notice")); },
                        Err(_) => break StopReason::ObserverGone,
                    },
                    _ = wait => {
                        if let Err(error) = self.outgoing.tick() { timer_failures.push(error.into()); }
                        if let Err(error) = self.calls.tick(generation, Instant::now()).await { timer_failures.push(error.into()); }
                        if !timer_failures.is_empty() { break StopReason::DeadlineFailed; }
                    },
                    control = controls.recv(), if controls_open => match control {
                        Some(control) => {
                            if signaled(&stop, &lease).is_some() { reject(control, "native_session_stopped"); }
                            else { self.control(control, renewal.is_some()).await; }
                        },
                        None => controls_open=false,
                    },
                }
                continue;
            }
            tokio::select! {
                _ = stop.changed() => {},
                _ = lease.changed() => {},
                _ = notices.closed() => break StopReason::ObserverGone,
                control = controls.recv(), if controls_open => match control {
                    Some(control) => {
                        if signaled(&stop, &lease).is_some() { reject(control, "native_session_stopped"); }
                        else { self.control(control, renewal.is_some()).await; }
                    },
                    None => controls_open=false,
                },
                event = events.recv() => {
                    let Some(event) = event else { break StopReason::ActorClosed; };
                    if signaled(&stop, &lease).is_some() { continue; }
                    if event.generation == generation && matches!(&event.event, RegisteredSipEvent::RegistrationRenewalDue) {
                        renewal.get_or_insert(Instant::now()+REPORT_DRAIN);
                    }
                    pending = match self.event(event, &commands).await {
                        Ok(notice) => notice,
                        Err(error) => Some(Notice::Failed(error)),
                    };
                },
                _ = wait => {
                    if let Err(error) = self.outgoing.tick() { timer_failures.push(error.into()); }
                    if let Err(error) = self.calls.tick(generation, Instant::now()).await { timer_failures.push(error.into()); }
                    if !timer_failures.is_empty() { break StopReason::DeadlineFailed; }
                },
            }
        };
        controls.close();
        while let Some(control) = controls.recv().await {
            reject(control, "native_session_stopped");
        }
        self.outgoing.stop();
        let pending_reports = self.incoming.disconnected();
        let media_cleanup = self.calls.shutdown(generation).await;
        Exit {
            generation,
            reason,
            undelivered_notice: pending,
            timer_failures,
            pending_reports,
            sms: self.outgoing.snapshots(),
            media_cleanup,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
