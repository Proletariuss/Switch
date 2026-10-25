//! Native MESSAGE consumer: archive commit -> SIP 200 -> tracked RP-ACK.
//! No network retry is performed here; a write error can be outcome-unknown.
#![allow(
    dead_code,
    reason = "native SMS consumer awaits full native owner activation"
)]

use crate::{
    native_ims_actor::{Command, Event},
    native_vowifi_supervisor::SessionGeneration,
};
use proletarius_sip::{Message, Parser};
use proletarius_vowifi::{
    sip_session_runtime::RegisteredSipEvent,
    sms_transaction::{DeliveryReportState, DeliveryReportSubmission},
};
use std::{path::PathBuf, time::Duration};
use tokio::sync::{mpsc, oneshot, watch};

/// A synchronous, content-free status observer owned by one session. It cannot
/// grant protocol I/O and does not add another queue or detached task.
#[derive(Clone)]
pub(crate) struct ArchiveWarningObserver(
    std::sync::Arc<
        dyn Fn(SessionGeneration, crate::ims_sms_transactions::NativeSmsArchiveWarning)
            + Send
            + Sync,
    >,
);
impl ArchiveWarningObserver {
    pub(crate) fn new(
        observe: impl Fn(SessionGeneration, crate::ims_sms_transactions::NativeSmsArchiveWarning)
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self(std::sync::Arc::new(observe))
    }
    pub(crate) fn observe(
        &self,
        generation: SessionGeneration,
        warning: crate::ims_sms_transactions::NativeSmsArchiveWarning,
    ) {
        (self.0)(generation, warning);
    }
}

pub(crate) struct Consumer {
    report_owner: std::sync::Arc<crate::ims_sms_transactions::NativeReportOwner>,
    archive: crate::ims_sms_transactions::NativeSmsArchive,
    archive_warning_observer: Option<ArchiveWarningObserver>,
    data_root: PathBuf,
    subscription_id: String,
    generation: SessionGeneration,
    commands: mpsc::Sender<Command>,
    stop: watch::Receiver<bool>,
    lease_lost: watch::Receiver<bool>,
}

pub(crate) enum Outcome {
    Unhandled(Box<Event>),
    ReportSubmitted(ReportSubmission),
}

/// Bound to one actor generation; another session's local ticket cannot claim it.
pub(crate) struct ReportSubmission {
    generation: SessionGeneration,
    transaction: DeliveryReportSubmission,
}

impl ReportSubmission {
    pub(crate) fn observe(&mut self, event: &Event) -> bool {
        event.generation == self.generation && self.transaction.observe(&event.event)
    }
    pub(crate) fn state(&self) -> DeliveryReportState {
        self.transaction.state()
    }
    pub(crate) fn disconnected(&mut self) {
        self.transaction.disconnected();
    }
}

/// Session event dispatcher owns submitted reports until completion/teardown.
/// One registered actor's response registry admits at most 64 transactions.
pub(crate) struct SessionConsumer {
    consumer: Consumer,
    pending: Vec<ReportSubmission>,
    registration_renewal: RegistrationRenewalDrain,
}

pub(crate) enum SessionOutcome {
    Unhandled(Box<Event>),
    ReportSubmitted(proletarius_vowifi::sip_session_runtime::ResponseTicket),
    ReportFinished {
        ticket: proletarius_vowifi::sip_session_runtime::ResponseTicket,
        state: DeliveryReportState,
        /// True only when due REGISTER renewal was waiting for reports and
        /// this completion drained the final report. This alone is not a full
        /// actor quiescence or permission to rebuild the IKE/IMS session.
        renewal_reports_drained: bool,
    },
    RegistrationRenewalDue {
        outstanding_reports: usize,
    },
}

#[derive(Default)]
struct RegistrationRenewalDrain {
    due: bool,
}

impl RegistrationRenewalDrain {
    fn due(&mut self, outstanding_reports: usize) -> SessionOutcome {
        self.due = outstanding_reports > 0;
        SessionOutcome::RegistrationRenewalDue {
            outstanding_reports,
        }
    }

    fn report_finished(&mut self, outstanding_reports: usize) -> bool {
        if self.due && outstanding_reports == 0 {
            self.due = false;
            true
        } else {
            false
        }
    }
}

/// Own the registered metadata for the complete consumer lifetime. Callers
/// cannot swap the routing snapshot between two incoming messages.
pub(crate) struct RegisteredConsumer {
    session: SessionConsumer,
    snapshot: proletarius_vowifi::message_routing::RegisteredMessageContext,
    local_host: zeroize::Zeroizing<String>,
    access_network_info: Option<zeroize::Zeroizing<String>>,
    minimum_cseq: u32,
}
pub(crate) enum LoopNotice {
    Outcome(SessionOutcome),
    Failed(&'static str),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LoopStop {
    Requested,
    LeaseLost,
    ActorClosed,
    ObserverGone,
}
pub(crate) struct LoopExit {
    pub(crate) undelivered_notice: Option<LoopNotice>,
    pub(crate) reason: LoopStop,
    pub(crate) last_error: Option<&'static str>,
    pub(crate) unconfirmed_reports: Vec<(
        proletarius_vowifi::sip_session_runtime::ResponseTicket,
        DeliveryReportState,
    )>,
}

fn loop_stop(stop: &watch::Receiver<bool>, lease: &watch::Receiver<bool>) -> Option<LoopStop> {
    if *stop.borrow() || stop.has_changed().is_err() {
        Some(LoopStop::Requested)
    } else if *lease.borrow() || lease.has_changed().is_err() {
        Some(LoopStop::LeaseLost)
    } else {
        None
    }
}
async fn forward_notice(
    notice: &mut Option<LoopNotice>,
    output: &mpsc::Sender<LoopNotice>,
    stop: &mut watch::Receiver<bool>,
    lease: &mut watch::Receiver<bool>,
) -> Result<(), LoopStop> {
    loop {
        if let Some(reason) = loop_stop(stop, lease) {
            return Err(reason);
        }
        tokio::select! {
            biased;
            _ = stop.changed() => {},
            _ = lease.changed() => {},
            slot = output.reserve() => {
                let slot = slot.map_err(|_| LoopStop::ObserverGone)?;
                if let Some(reason) = loop_stop(stop, lease) { return Err(reason); }
                slot.send(notice.take().expect("pending loop notice"));
                return Ok(());
            }
        }
    }
}

impl RegisteredConsumer {
    pub(crate) fn generation(&self) -> SessionGeneration {
        self.session.consumer.generation
    }
    pub(crate) fn stop_signal(&self) -> watch::Receiver<bool> {
        self.session.consumer.stop.clone()
    }
    pub(crate) fn lease_signal(&self) -> watch::Receiver<bool> {
        self.session.consumer.lease_lost.clone()
    }
    pub(crate) fn pending_reports(&self) -> usize {
        self.session.pending.len()
    }

    /// Serial event owner; actor continues servicing commands while its event
    /// queue is full. No detached archive/ack task or automatic resend is spawned.
    pub(crate) async fn run(
        mut self,
        mut input: mpsc::Receiver<Event>,
        output: mpsc::Sender<LoopNotice>,
    ) -> LoopExit {
        let mut stop = self.session.consumer.stop.clone();
        let mut lease = self.session.consumer.lease_lost.clone();
        let mut last_error = None;
        let mut undelivered_notice = None;
        let reason = loop {
            if let Some(reason) = loop_stop(&stop, &lease) {
                break reason;
            }
            let event = tokio::select! {
                biased;
                _ = stop.changed() => { continue; },
                _ = lease.changed() => { continue; },
                _ = output.closed() => { break LoopStop::ObserverGone; },
                event = input.recv() => { match event { Some(event) => event, None => break LoopStop::ActorClosed } }
            };
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok());
            let result = match timestamp {
                Some(timestamp) => self.process(event, timestamp).await,
                None => Err("native_sms_clock_invalid"),
            };
            undelivered_notice = Some(match result {
                Ok(outcome) => LoopNotice::Outcome(outcome),
                Err(code) => {
                    last_error = Some(code);
                    LoopNotice::Failed(code)
                }
            });
            if let Err(reason) =
                forward_notice(&mut undelivered_notice, &output, &mut stop, &mut lease).await
            {
                break reason;
            }
        };
        LoopExit {
            undelivered_notice,
            reason,
            last_error,
            unconfirmed_reports: self.disconnected(),
        }
    }

    pub(crate) fn new(
        consumer: Consumer,
        snapshot: proletarius_vowifi::message_routing::RegisteredMessageContext,
        access_network_info: Option<String>,
        minimum_cseq: u32,
    ) -> Self {
        let local_host = snapshot.local_ip().to_string();
        Self {
            session: SessionConsumer::new(consumer),
            snapshot,
            local_host: zeroize::Zeroizing::new(local_host),
            access_network_info: access_network_info.map(zeroize::Zeroizing::new),
            minimum_cseq,
        }
    }
    pub(crate) async fn process(
        &mut self,
        event: Event,
        received_at_unix_ms: i64,
    ) -> Result<SessionOutcome, &'static str> {
        let routing = crate::ims_sms_transactions::NativeSmsRouting::session(
            self.session.consumer.generation,
            self.minimum_cseq,
            &self.snapshot,
            &self.local_host,
            self.access_network_info
                .as_ref()
                .map(|value| value.as_str()),
        );
        self.session
            .process(event, received_at_unix_ms, &routing)
            .await
    }
    pub(crate) fn disconnected(
        &mut self,
    ) -> Vec<(
        proletarius_vowifi::sip_session_runtime::ResponseTicket,
        DeliveryReportState,
    )> {
        self.session.disconnected()
    }
}

impl SessionConsumer {
    pub(crate) fn new(consumer: Consumer) -> Self {
        Self {
            consumer,
            pending: Vec::new(),
            registration_renewal: RegistrationRenewalDrain::default(),
        }
    }

    pub(crate) async fn process(
        &mut self,
        event: Event,
        received_at_unix_ms: i64,
        context: &crate::ims_sms_transactions::NativeSmsRouting<'_>,
    ) -> Result<SessionOutcome, &'static str> {
        if event.generation != self.consumer.generation {
            return Err("native_sms_session_mismatch");
        }
        if !self.consumer.active() {
            return Err("native_sms_session_stopped");
        }
        if matches!(&event.event, RegisteredSipEvent::RegistrationRenewalDue) {
            return Ok(self.registration_renewal.due(self.pending.len()));
        }
        if let Some(index) = self
            .pending
            .iter_mut()
            .position(|report| report.observe(&event))
        {
            let finished = self.pending.remove(index);
            let renewal_reports_drained = self
                .registration_renewal
                .report_finished(self.pending.len());
            return Ok(SessionOutcome::ReportFinished {
                ticket: finished.transaction.ticket(),
                state: finished.state(),
                renewal_reports_drained,
            });
        }
        // Admit no additional incoming SMS report while the bounded owner is
        // full. Return the original event for the session's overload policy;
        // OPTIONS/voice/control and final responses remain dispatchable.
        if self.pending.len() >= 64
            && matches!(&event.event,
            RegisteredSipEvent::Message { message, .. }
            if matches!(message.as_ref(), proletarius_vowifi::sip_session_runtime::RegisteredSipMessage::Request { method: proletarius_sip::Method::MESSAGE, .. }))
        {
            return Ok(SessionOutcome::Unhandled(Box::new(event)));
        }
        match self
            .consumer
            .receive(event, received_at_unix_ms, context)
            .await?
        {
            Outcome::Unhandled(event) => Ok(SessionOutcome::Unhandled(event)),
            Outcome::ReportSubmitted(report) => {
                let ticket = report.transaction.ticket();
                self.pending.push(report);
                Ok(SessionOutcome::ReportSubmitted(ticket))
            }
        }
    }

    /// Must be called by the event loop on every terminal actor exit.
    pub(crate) fn disconnected(
        &mut self,
    ) -> Vec<(
        proletarius_vowifi::sip_session_runtime::ResponseTicket,
        DeliveryReportState,
    )> {
        self.pending
            .drain(..)
            .map(|mut report| {
                report.disconnected();
                (report.transaction.ticket(), report.state())
            })
            .collect()
    }
}

impl Consumer {
    pub(crate) fn new(
        data_root: PathBuf,
        subscription_id: String,
        generation: SessionGeneration,
        commands: mpsc::Sender<Command>,
        stop: watch::Receiver<bool>,
        lease_lost: watch::Receiver<bool>,
        report_owner: impl Into<std::sync::Arc<crate::ims_sms_transactions::NativeReportOwner>>,
    ) -> Self {
        Self {
            archive: Default::default(),
            archive_warning_observer: None,
            report_owner: report_owner.into(),
            data_root,
            subscription_id,
            generation,
            commands,
            stop,
            lease_lost,
        }
    }

    pub(crate) fn with_archive(
        mut self,
        archive: crate::ims_sms_transactions::NativeSmsArchive,
    ) -> Self {
        self.archive = archive;
        self
    }

    pub(crate) fn with_archive_warning_observer(
        mut self,
        observer: Option<ArchiveWarningObserver>,
    ) -> Self {
        self.archive_warning_observer = observer;
        self
    }

    fn active(&self) -> bool {
        !*self.stop.borrow()
            && self.stop.has_changed().is_ok()
            && !*self.lease_lost.borrow()
            && self.lease_lost.has_changed().is_ok()
    }

    /// Routing must match this registered generation. The committed receipt
    /// and shared transaction service supply incoming/report identities.
    pub(crate) async fn receive(
        &self,
        event: Event,
        received_at_unix_ms: i64,
        report_context: &crate::ims_sms_transactions::NativeSmsRouting<'_>,
    ) -> Result<Outcome, &'static str> {
        if !self.active() {
            return Err("native_sms_session_stopped");
        }
        let Some(commit) = crate::ims_sms_transactions::archive_native_sms_event_to(
            self.data_root.clone(),
            &self.subscription_id,
            self.generation,
            &event,
            received_at_unix_ms,
            &self.archive,
        )
        .await?
        else {
            return Ok(Outcome::Unhandled(Box::new(event)));
        };
        if !self.active() {
            return Err("native_sms_session_stopped");
        }
        // Preserve the durable archive fact even if route preparation or the
        // subsequent SIP/RP writes fail. This never authorizes further I/O.
        if let Some(warning) = commit.archive_warning()
            && let Some(observer) = &self.archive_warning_observer
        {
            observer.observe(self.generation, warning);
        }
        let prepared =
            commit.prepare_report(self.generation, &self.report_owner, report_context)?;
        let wire = prepared.wire;
        let (Message::Request(report), consumed) = Parser::default()
            .parse(&wire, false)
            .map_err(|_| "invalid_delivery_report_frame")?
        else {
            return Err("invalid_delivery_report_frame");
        };
        if consumed != wire.len() {
            return Err("invalid_delivery_report_frame");
        }
        if !self.active() {
            return Err("native_sms_session_stopped");
        }
        let RegisteredSipEvent::Message {
            connection,
            message,
            ..
        } = event.event
        else {
            return Err("invalid_incoming_sms_event");
        };
        let (reply, replied) = oneshot::channel();
        self.enqueue(Command::Respond {
            generation: self.generation,
            connection,
            request: message,
            status: 200,
            reason: "OK".into(),
            reply,
        })
        .await?;
        self.result(replied)
            .await
            .map_err(|_| "native_sms_response_write_unknown")?
            .map_err(|_| "native_sms_response_write_unknown")?;
        // The actor also checks generation and stop immediately before IO.
        // A stopped consumer never submits the second MESSAGE command.
        if !self.active() {
            return Err("native_sms_session_stopped");
        }
        let (reply, submitted) = oneshot::channel();
        self.enqueue(Command::Send {
            generation: self.generation,
            request: report,
            timeout: Duration::from_secs(15),
            reply,
        })
        .await?;
        let ticket = self
            .result(submitted)
            .await
            .map_err(|_| "native_sms_report_write_unknown")?
            .map_err(|_| "native_sms_report_write_unknown")?;
        Ok(Outcome::ReportSubmitted(ReportSubmission {
            generation: self.generation,
            transaction: DeliveryReportSubmission::new(ticket),
        }))
    }

    async fn enqueue(&self, command: Command) -> Result<(), &'static str> {
        let mut stop = self.stop.clone();
        let mut lease = self.lease_lost.clone();
        loop {
            if !self.active() {
                return Err("native_sms_session_stopped");
            }
            tokio::select! {
                biased;
                _ = stop.changed() => {},
                _ = lease.changed() => {},
                slot = self.commands.reserve() => {
                    let slot = slot.map_err(|_| "native_sms_actor_closed")?;
                    if !self.active() { return Err("native_sms_session_stopped"); }
                    slot.send(command);
                    return Ok(());
                }
            }
        }
    }

    async fn result<T>(&self, mut result: oneshot::Receiver<T>) -> Result<T, &'static str> {
        let mut stop = self.stop.clone();
        let mut lease = self.lease_lost.clone();
        loop {
            // A completed actor write receipt remains evidence even if stop
            // becomes ready in the same turn. Callers still gate subsequent IO.
            match result.try_recv() {
                Ok(value) => return Ok(value),
                Err(oneshot::error::TryRecvError::Closed) => return Err("native_sms_actor_closed"),
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
            if !self.active() {
                return Err("native_sms_session_stopped");
            }
            tokio::select! {
                biased;
                result = &mut result => { return result.map_err(|_| "native_sms_actor_closed"); }
                _ = stop.changed() => {},
                _ = lease.changed() => {},
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
    use proletarius_sip::{Headers, MessageData, Method, Request, Uri};

    #[test]
    fn renewal_due_reports_zero_when_no_delivery_reports_are_pending() {
        let mut renewal = RegistrationRenewalDrain::default();
        assert!(matches!(
            renewal.due(0),
            SessionOutcome::RegistrationRenewalDue {
                outstanding_reports: 0,
            }
        ));
        assert!(!renewal.report_finished(0));
    }

    #[test]
    fn renewal_waits_until_the_last_delivery_report_finishes() {
        let mut renewal = RegistrationRenewalDrain::default();
        assert!(matches!(
            renewal.due(2),
            SessionOutcome::RegistrationRenewalDue {
                outstanding_reports: 2,
            }
        ));
        assert!(!renewal.report_finished(1));
        assert!(renewal.report_finished(0));
        assert!(!renewal.report_finished(0));
    }

    #[test]
    fn report_completion_without_due_renewal_does_not_request_rebuild() {
        let mut renewal = RegistrationRenewalDrain::default();
        assert!(!renewal.report_finished(0));
    }

    fn command(generation: SessionGeneration) -> Command {
        let (reply, _result) = oneshot::channel();
        Command::Send {
            generation,
            request: Request {
                method: Method::OPTIONS,
                uri: Uri::parse("sip:ims.example").unwrap(),
                data: MessageData::new("SIP/2.0", Headers::default(), Vec::new()),
            },
            timeout: Duration::from_secs(1),
            reply,
        }
    }

    fn fixture() -> (
        Consumer,
        NativeVoWiFiSupervisor,
        watch::Sender<bool>,
        mpsc::Receiver<Command>,
    ) {
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor
            .reserve(ReaderSessionKey::new("fixture", "8901234567890123456").unwrap())
            .unwrap();
        let (commands, input) = mpsc::channel(1);
        let (lease, lease_rx) = watch::channel(false);
        let consumer = Consumer::new(
            PathBuf::new(),
            String::new(),
            permit.generation(),
            commands,
            permit.stop_signal(),
            lease_rx,
            crate::ims_sms_transactions::NativeReportOwner::new(
                crate::ims_sms_transactions::Service::default(),
                "native-session-fixture-0001".into(),
            )
            .unwrap(),
        );
        (consumer, supervisor, lease, input)
    }

    #[tokio::test]
    async fn consumer_loop_notice_backpressure_is_stoppable_and_preserves_false_updates() {
        let (consumer, mut supervisor, lease, _input) = fixture();
        let (output, mut inbox) = mpsc::channel(1);
        output
            .send(LoopNotice::Failed("occupied"))
            .await
            .unwrap_or_else(|_| panic!("queue"));
        let mut stop = consumer.stop.clone();
        let mut lease_rx = consumer.lease_lost.clone();
        let mut pending = Some(LoopNotice::Failed("pending"));
        let forward = forward_notice(&mut pending, &output, &mut stop, &mut lease_rx);
        tokio::pin!(forward);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut forward)
                .await
                .is_err()
        );
        lease.send_replace(false);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut forward)
                .await
                .is_err()
        );
        assert!(matches!(
            inbox.recv().await,
            Some(LoopNotice::Failed("occupied"))
        ));
        assert_eq!(forward.await, Ok(()));
        assert!(matches!(
            inbox.recv().await,
            Some(LoopNotice::Failed("pending"))
        ));
        output
            .send(LoopNotice::Failed("occupied"))
            .await
            .unwrap_or_else(|_| panic!("queue"));
        supervisor.request_stop_all();
        let mut unsent = Some(LoopNotice::Failed("never forwarded"));
        assert_eq!(
            forward_notice(
                &mut unsent,
                &output,
                &mut consumer.stop.clone(),
                &mut consumer.lease_lost.clone()
            )
            .await,
            Err(LoopStop::Requested)
        );
        assert!(matches!(
            unsent,
            Some(LoopNotice::Failed("never forwarded"))
        ));
        assert!(matches!(
            inbox.recv().await,
            Some(LoopNotice::Failed("occupied"))
        ));
        assert!(inbox.try_recv().is_err());
    }

    #[tokio::test]
    async fn consumer_loop_notice_observer_loss_and_lease_loss_are_distinct() {
        let (consumer, _supervisor, lease, _input) = fixture();
        let (output, inbox) = mpsc::channel(1);
        drop(inbox);
        assert_eq!(
            forward_notice(
                &mut Some(LoopNotice::Failed("pending")),
                &output,
                &mut consumer.stop.clone(),
                &mut consumer.lease_lost.clone()
            )
            .await,
            Err(LoopStop::ObserverGone)
        );
        lease.send_replace(true);
        assert_eq!(
            forward_notice(
                &mut Some(LoopNotice::Failed("pending")),
                &output,
                &mut consumer.stop.clone(),
                &mut consumer.lease_lost.clone()
            )
            .await,
            Err(LoopStop::LeaseLost)
        );
    }

    #[tokio::test]
    async fn completed_write_receipt_survives_simultaneous_stop_but_cannot_admit_more_io() {
        let (consumer, mut supervisor, _lease, mut input) = fixture();
        let (reply, result) = oneshot::channel();
        reply.send(42_u8).unwrap();
        supervisor.request_stop_all();
        assert_eq!(consumer.result(result).await, Ok(42));
        assert_eq!(
            consumer.enqueue(command(consumer.generation)).await,
            Err("native_sms_session_stopped")
        );
        assert!(input.try_recv().is_err());
    }

    #[tokio::test]
    async fn full_command_queue_stop_revokes_waiting_admission_without_extra_command() {
        let (consumer, mut supervisor, _lease, mut input) = fixture();
        consumer
            .commands
            .send(command(consumer.generation))
            .await
            .unwrap();
        let enqueue = consumer.enqueue(command(consumer.generation));
        tokio::pin!(enqueue);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut enqueue)
                .await
                .is_err()
        );
        supervisor.request_stop_all();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), enqueue)
                .await
                .unwrap(),
            Err("native_sms_session_stopped")
        );
        assert!(input.recv().await.is_some());
        assert!(input.try_recv().is_err());
    }

    #[tokio::test]
    async fn false_watch_update_preserves_one_waiting_command() {
        let (consumer, _supervisor, lease, mut input) = fixture();
        consumer
            .commands
            .send(command(consumer.generation))
            .await
            .unwrap();
        let enqueue = consumer.enqueue(command(consumer.generation));
        tokio::pin!(enqueue);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut enqueue)
                .await
                .is_err()
        );
        lease.send_replace(false);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut enqueue)
                .await
                .is_err()
        );
        assert!(input.recv().await.is_some());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), enqueue)
                .await
                .unwrap(),
            Ok(())
        );
        assert!(input.recv().await.is_some());
        assert!(input.try_recv().is_err());
    }

    #[tokio::test]
    async fn lease_loss_cancels_wait_for_write_result_and_closes_receipt() {
        let (consumer, _supervisor, lease, _input) = fixture();
        let (reply, result) = oneshot::channel::<()>();
        let wait = consumer.result(result);
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut wait)
                .await
                .is_err()
        );
        lease.send_replace(true);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), wait)
                .await
                .unwrap(),
            Err("native_sms_session_stopped")
        );
        assert!(reply.is_closed());
    }

    #[tokio::test]
    async fn dropped_actor_result_and_control_sender_fail_closed() {
        let (consumer, _supervisor, lease, _input) = fixture();
        let (reply, result) = oneshot::channel::<()>();
        drop(reply);
        assert_eq!(
            consumer.result(result).await,
            Err("native_sms_actor_closed")
        );
        drop(lease);
        assert_eq!(
            consumer.enqueue(command(consumer.generation)).await,
            Err("native_sms_session_stopped")
        );
    }
}
