//! One ordinary reader generation owns startup, SMS, voice, and final release.
//! Public admission stays gated until recovery and API equivalence are verified.
#![allow(
    dead_code,
    reason = "ordinary native API remains gated during integration"
)]
use super::*;
use crate::native_vowifi_supervisor::{NativeSessionPermit, SessionGeneration};
use crate::{native_ims_actor, native_registered_session as registered};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{mpsc, watch};

mod registration_identity;
pub(crate) mod status;
use status::{Binding, EndReason, Reporter, RetryClass, Snapshot, Termination};

type Supervisor = Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>;

pub(super) struct Inputs {
    pub(super) sms_archive: crate::ims_sms_transactions::NativeSmsArchive,
    pub(super) reader_gate: Arc<crate::reader_reservation::Gate>,
    pub(super) settings: NativeRegisteredStartupSettings,
    pub(super) data_root: PathBuf,
    pub(super) transactions: crate::ims_sms_transactions::Service,
    pub(super) call_facts: crate::call_facts::CallFacts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Starting,
    Registered,
    Draining,
    Released,
    CleanupUnconfirmed,
}

/// Control waiters do not own the worker; dropping a UI handle cannot abort it.
#[derive(Clone)]
pub(crate) struct Handle {
    pub(super) generation: SessionGeneration,
    pub(super) controls: mpsc::Sender<registered::Control>,
    pub(super) phase: watch::Receiver<Phase>,
    status: watch::Receiver<Snapshot>,
    reporter: Reporter,
    stop: watch::Sender<bool>,
    user_stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Handle {
    pub(crate) fn binding(&self) -> Binding {
        self.status.borrow().binding.clone()
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.status.borrow().clone()
    }
    pub(crate) fn request_stop(&self) {
        self.user_stop
            .store(true, std::sync::atomic::Ordering::Release);
        self.request_shutdown();
    }
    /// Internal failed-owner cleanup must not invent a human stop decision.
    pub(crate) fn request_shutdown(&self) {
        self.reporter.draining();
        self.stop.send_replace(true);
    }
    pub(crate) async fn changes(&mut self) -> Result<(), &'static str> {
        tokio::select! {
            result = self.status.changed() => result.map_err(|_| "native_ordinary_status_closed"),
            result = self.phase.changed() => result.map_err(|_| "native_ordinary_worker_closed"),
        }
    }
}

pub(super) enum Outcome {
    Startup {
        permit: NativeSessionPermit,
        error: NativeRegisteredStartupFailure,
    },
    Setup {
        permit: NativeSessionPermit,
        reason: String,
        cleanup: NativeRegisteredStartupFailure,
        call_archive: Result<(), String>,
        call_archive_retired: bool,
    },
    Running(Result<NativeRegisteredWorkerExit, tokio::task::JoinError>),
}

pub(super) struct SessionResult {
    pub(super) termination: Termination,
    pub(super) outcome: Outcome,
    pub(super) lease_release: Option<Result<(), proletarius_domain::BackendFailure>>,
    // Preserve bounded startup/unhandled evidence rather than silently dropping
    // messages which arrived before the registered dispatcher took ownership.
    pub(super) startup_events: Vec<SipStreamItem>,
    pub(super) notices: Vec<registered::Notice>,
    pub(super) overflowed: bool,
    pub(super) omitted_notices: u64,
    lease: SessionCard,
}

impl NativeLifecycle {
    pub(super) fn take_ordinary_result(
        &self,
        generation: SessionGeneration,
    ) -> Option<Result<SessionResult, &'static str>> {
        let Ok(mut state) = self.state.lock() else {
            return Some(Err("native_ordinary_owner_unavailable"));
        };
        let owner = state.owner.as_mut()?;
        owner.reap_ordinary_tasks();
        if let Some(index) = owner
            .ordinary_results
            .iter()
            .position(|(g, _)| *g == generation)
        {
            return owner
                .ordinary_results
                .remove(index)
                .map(|(_, result)| Ok(result));
        }
        if !owner.ordinary_tasks.contains_key(&generation) && owner.session_task_failure.is_some() {
            return Some(Err("native_ordinary_worker_failed"));
        }
        None
    }
    pub(super) fn launch_ordinary(
        &self,
        context: NativeSessionContext,
        inputs: Inputs,
    ) -> Result<Handle, (NativeSessionAdmissionError, NativeSessionContext)> {
        let Ok(mut state) = self.state.lock() else {
            return Err((NativeSessionAdmissionError::SupervisorUnavailable, context));
        };
        if state.stopping || state.finishing || state.cleanup_failure.is_some() {
            return Err((NativeSessionAdmissionError::Stopping, context));
        }
        let Some(owner) = state.owner.as_mut() else {
            return Err((NativeSessionAdmissionError::Stopping, context));
        };
        owner.launch_ordinary(context, inputs)
    }
}

impl NativeSessionServiceOwner {
    fn launch_ordinary(
        &mut self,
        context: NativeSessionContext,
        inputs: Inputs,
    ) -> Result<Handle, (NativeSessionAdmissionError, NativeSessionContext)> {
        self.launch_profile(context, inputs, false)
    }
    pub(super) fn launch_profile(
        &mut self,
        context: NativeSessionContext,
        inputs: Inputs,
        sms_only: bool,
    ) -> Result<Handle, (NativeSessionAdmissionError, NativeSessionContext)> {
        self.reap_ordinary_tasks();
        if self.session_task_failure.is_some() {
            return Err((
                NativeSessionAdmissionError::PreviousSessionTaskFailed,
                context,
            ));
        }
        let permit = match self.reserve_session(&context.session_key) {
            Ok(permit) => permit,
            Err(error) => return Err((error, context)),
        };
        let generation = permit.generation();
        if self.session_tasks.contains_key(&generation)
            || self.ordinary_tasks.contains_key(&generation)
        {
            if let Ok(mut owner) = self.supervisor.lock() {
                owner.worker_finished(&permit);
            }
            return Err((NativeSessionAdmissionError::GenerationCollision, context));
        }
        let (controls, input) = registered::controls();
        let (phase, receiver) = watch::channel(Phase::Starting);
        let (reporter, status) = Reporter::new(Binding {
            reader: context.reader.clone(),
            reader_generation: context.reader_generation,
            session_generation: generation,
            key: context.session_key.clone(),
        });
        reporter.sim_phone_number(&context.card.identity().phone_number);
        let stop = permit.cancellation_sender();
        let user_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = spawn_profile(
            context,
            permit,
            Arc::downgrade(&self.supervisor),
            inputs,
            input,
            phase,
            reporter.clone(),
            user_stop.clone(),
            sms_only,
        );
        self.ordinary_tasks.insert(generation, task);
        Ok(Handle {
            generation,
            controls,
            phase: receiver,
            status,
            reporter,
            stop,
            user_stop,
        })
    }
}

fn spawn(
    context: NativeSessionContext,
    permit: NativeSessionPermit,
    supervisor: Weak<Supervisor>,
    inputs: Inputs,
    controls: mpsc::Receiver<registered::Control>,
    phase: watch::Sender<Phase>,
    reporter: Reporter,
    user_stop: Arc<std::sync::atomic::AtomicBool>,
) -> native_ims_actor::Task<SessionResult> {
    spawn_profile(
        context, permit, supervisor, inputs, controls, phase, reporter, user_stop, false,
    )
}
fn spawn_profile(
    context: NativeSessionContext,
    permit: NativeSessionPermit,
    supervisor: Weak<Supervisor>,
    inputs: Inputs,
    controls: mpsc::Receiver<registered::Control>,
    phase: watch::Sender<Phase>,
    reporter: Reporter,
    user_stop: Arc<std::sync::atomic::AtomicBool>,
    sms_only: bool,
) -> native_ims_actor::Task<SessionResult> {
    let cancel = permit.cancellation_sender();
    native_ims_actor::Task::spawn(cancel.clone(), async move {
        let lease = context.card.clone();
        let recovery = Arc::new(NativeLeaseRecovery {
            phase: Some(phase.clone()),
            status: Some(reporter.clone()),
            lease: lease.clone(),
            cleanup_confirmed: std::sync::atomic::AtomicBool::new(false),
        });
        let retained = permit.matches_key(&context.session_key)
            && supervisor.upgrade().is_some_and(|owner| {
                owner
                    .lock()
                    .is_ok_and(|mut owner| owner.retain_recovery(&permit, recovery.clone()))
            });
        if !retained {
            cancel.send_replace(true);
        }
        let (startup_tx, mut startup_rx) = mpsc::channel(32);
        let mut startup_events = Vec::new();
        let mut overflowed = false;
        let startup_result = {
            let startup = context.start_registered_ims_observed(
                &permit,
                &startup_tx,
                inputs.settings,
                if sms_only {
                    NativeImsSessionMode::SmsOnly
                } else {
                    NativeImsSessionMode::VoiceAndSms
                },
                Some(reporter.clone()),
            );
            tokio::pin!(startup);
            loop {
                tokio::select! {
                    result = &mut startup => break result,
                    Some(event) = startup_rx.recv() => {
                        if startup_events.len() < 32 { startup_events.push(event); }
                        else { overflowed = true; cancel.send_replace(true); }
                    }
                }
            }
        };
        while let Ok(event) = startup_rx.try_recv() {
            if startup_events.len() < 32 {
                startup_events.push(event);
            } else {
                overflowed = true;
                cancel.send_replace(true);
            }
        }
        let durable_nwait = matches!(
            &inputs.sms_archive,
            crate::ims_sms_transactions::NativeSmsArchive::Nwait { .. }
        );
        let mut nwait_store_failed = false;
        let mut notices = Vec::new();
        let mut omitted_notices = 0_u64;
        let outcome = match startup_result {
            Err(error) => Outcome::Startup { permit, error },
            Ok((network, mut session)) => {
                registration_identity::observe(
                    &context,
                    &session,
                    &permit,
                    inputs.reader_gate.clone(),
                    inputs.data_root.clone(),
                    &reporter,
                )
                .await;
                let incoming = reporter.clone();
                session.set_incoming_observer(Arc::new(move |facts| incoming.incoming(facts)));
                let health_reporter = reporter.clone();
                session.set_heartbeat_observer(Arc::new(move |healthy| {
                    health_reporter.heartbeat_health(healthy)
                }));

                let refresh = context.enable_registration_refresh(&mut session);
                let lease_reporter = reporter.clone();
                session.set_registration_lease_observer(Arc::new(
                    move |seconds, expiry, renewal| {
                        lease_reporter.registration_lease(seconds, expiry, renewal)
                    },
                ));
                let setup = refresh.and_then(|()| {
                    make_setup(
                        &context,
                        &session,
                        &permit,
                        inputs.data_root,
                        inputs.transactions,
                        inputs.call_facts,
                        sms_only,
                        inputs.sms_archive,
                        durable_nwait.then(|| reporter.sms_archive_warning_observer()),
                    )
                });
                let (commands, command_input, events, event_input) = native_ims_actor::channels();
                let mut retained_facts = None;
                let consumers = match setup {
                    Ok(setup) => {
                        retained_facts = Some(setup.call_facts.clone());
                        network
                            .registered_profile_consumers(
                                &permit,
                                &session,
                                setup,
                                commands.clone(),
                                sms_only,
                            )
                            .map_err(str::to_owned)
                    }
                    Err(error) => Err(error),
                };
                let running = supervisor.upgrade().is_some_and(|owner| {
                    owner.lock().is_ok_and(|mut owner| {
                        !*permit.stop_signal().borrow()
                            && consumers.is_ok()
                            && owner.mark_running(&permit).is_ok()
                    })
                });
                match consumers {
                    Ok((consumers, facts)) if running => {
                        phase.send_replace(Phase::Registered);
                        reporter.profile_consumers_ready(
                            &context.card.identity().smsc,
                            &context.card.identity().smsc_target,
                            !sms_only,
                        );
                        let (notice_tx, mut notice_rx) = mpsc::channel(32);
                        let worker = network.spawn_registered_consumers(
                            permit,
                            session,
                            consumers,
                            commands,
                            command_input,
                            events,
                            event_input,
                            controls,
                            notice_tx,
                            facts,
                        );
                        let finished = worker.finish();
                        tokio::pin!(finished);
                        let result = loop {
                            tokio::select! {
                                result = &mut finished => break result,
                                Some(notice) = notice_rx.recv() => {
                                    // Lifecycle notices are bounded diagnostics. Successful
                                    // SMS contents are already durably stored by the consumer.
                                    match &notice {
                                        registered::Notice::Failed(error) if durable_nwait && error.code == "sms_store_unavailable" => {
                                            nwait_store_failed = true;
                                            reporter.draining();
                                            cancel.send_replace(true);
                                        },
                                        registered::Notice::Sms(crate::native_sms_consumer::SessionOutcome::RegistrationRenewalDue { .. }) => { reporter.renewal_due(); },
                                        registered::Notice::Sms(crate::native_sms_consumer::SessionOutcome::ReportSubmitted(_)
                                            | crate::native_sms_consumer::SessionOutcome::ReportFinished { .. }) => {
                                            // Integration tests observe the real ticket's final
                                            // result; an empty pending queue also permits timeout.
                                            #[cfg(not(test))]
                                            continue;
                                        },
                                        _ => {},
                                    }
                                    record_notice(&mut notices, &mut omitted_notices, notice);
                                }
                            }
                        };
                        while let Ok(notice) = notice_rx.try_recv() {
                            if matches!(&notice, registered::Notice::Sms(crate::native_sms_consumer::SessionOutcome::RegistrationRenewalDue { .. })) {
                                reporter.renewal_due();
                            }
                            record_notice(&mut notices, &mut omitted_notices, notice);
                        }
                        Outcome::Running(result)
                    }
                    rejected => {
                        cancel.send_replace(true);
                        let reason = rejected
                            .err()
                            .unwrap_or_else(|| "native_ordinary_start_cancelled".into());
                        phase.send_replace(Phase::Draining);
                        reporter.draining();
                        let retirement = network.retirement.clone();
                        let (cleanup, network) =
                            network.finish_after_registered_shutdown(session).await;
                        let call_archive = match retained_facts.as_ref() {
                            Some(facts) => retire_call_archive(facts).await,
                            None => Ok(()),
                        };
                        Outcome::Setup {
                            permit,
                            reason,
                            cleanup: NativeRegisteredStartupFailure::ServiceGone {
                                cleanup,
                                network,
                                local_retired: retirement.confirmed(),
                            },
                            call_archive,
                            call_archive_retired: retained_facts
                                .as_ref()
                                .is_none_or(|facts| facts.worker_retired()),
                        }
                    }
                }
            }
        };
        // Capture the cause before cleanup sets the shared cancellation flag.
        // A renewal/network shutdown is never manufactured into user intent.
        let mut termination = classify_outcome(
            &outcome,
            user_stop.load(std::sync::atomic::Ordering::Acquire),
        );
        if nwait_store_failed
            && !matches!(
                termination.reason,
                EndReason::UserStop | EndReason::ReaderChanged
            )
        {
            termination = Termination::new(
                EndReason::InternalFailure,
                RetryClass::RetryAfterRelease,
                Some("native_multi_sms_store_unavailable"),
                Some("Nwait incoming durability failed".into()),
            );
        }
        reporter.terminated(termination.clone());
        cancel.send_replace(true);
        phase.send_replace(Phase::Draining);
        let releasable = match &outcome {
            Outcome::Startup { permit, error } if error.cleanup_confirmed() => Some(permit),
            Outcome::Setup {
                permit,
                cleanup,
                call_archive_retired,
                ..
            } if cleanup.cleanup_confirmed() && *call_archive_retired => Some(permit),
            Outcome::Running(Ok(exit)) if exit.clean() => {
                exit.actor.as_ref().ok().map(|exit| &exit.permit)
            }
            _ => None,
        };
        let lease_release = if let Some(permit) = releasable {
            recovery.confirm_cleanup();
            reporter.cleanup_confirmed();
            release_with_claim(permit, lease.release_with_confirmation(), &supervisor)
                .await
                .map(|(result, _)| result)
        } else {
            None
        };
        if matches!(&lease_release, Some(Ok(()))) {
            recovery.publish_released();
        } else {
            if let Some(Err(error)) = &lease_release {
                reporter.release_failed(format!("native reader lease release: {error:?}"));
            }
            // Recovery may have won the release claim and completed already.
            // Never overwrite that physical confirmation with a stale result.
            phase.send_if_modified(|current| {
                if *current == Phase::Released {
                    false
                } else {
                    *current = Phase::CleanupUnconfirmed;
                    true
                }
            });
            reporter.cleanup_unconfirmed();
        }
        SessionResult {
            termination,
            outcome,
            lease_release,
            startup_events,
            notices,
            overflowed,
            omitted_notices,
            lease,
        }
    })
}

/// The consumer has already handled protocol replies and durable SMS state.
/// A long-lived session must not disconnect merely because diagnostic history
/// filled with normal OPTIONS or unsolicited responses.
fn record_notice(
    notices: &mut Vec<registered::Notice>,
    omitted: &mut u64,
    notice: registered::Notice,
) {
    if notices.len() == 32 {
        notices.remove(0);
        *omitted = omitted.saturating_add(1);
    }
    notices.push(notice);
}

fn startup_detail(error: &NativeRegisteredStartupFailure) -> String {
    match error {
        NativeRegisteredStartupFailure::SessionMismatch => "native session key mismatch".into(),
        NativeRegisteredStartupFailure::Tunnel(error) => format!("tunnel startup: {error:?}"),
        NativeRegisteredStartupFailure::Registration {
            error,
            network,
            local_retired,
            ..
        } => {
            let registration = match error {
                NativeImsRegistrationError::SessionMismatch => {
                    "registration session mismatch".into()
                }
                NativeImsRegistrationError::TransportClosed => {
                    "registration transport closed".into()
                }
                NativeImsRegistrationError::Endpoint(error) => {
                    format!("registration endpoint: {error:?}")
                }
                NativeImsRegistrationError::Entropy => "registration entropy unavailable".into(),
                NativeImsRegistrationError::CancellationOwner(cleanup) => {
                    format!("registered cancellation owner mismatch; cleanup={cleanup:?}")
                }
                NativeImsRegistrationError::Registration(error) => format!(
                    "registration: {:?}; profile: {:?}",
                    error.registration(),
                    error.profile_error()
                ),
            };
            format!("{registration}; network: {network:?}; local retired: {local_retired}")
        }
        NativeRegisteredStartupFailure::ServiceGone {
            cleanup,
            network,
            local_retired,
        } => {
            format!(
                "registered service gone; cleanup: {cleanup:?}; network: {network:?}; local retired: {local_retired}"
            )
        }
    }
}

// Transport wrappers also carry invalid configuration, proxy authentication
// rejection and permanent SIP errors. Classify the typed cause, never its text.
fn transient_startup_io(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        error.kind(),
        ErrorKind::TimedOut
            | ErrorKind::WouldBlock
            | ErrorKind::Interrupted
            | ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::NotConnected
            | ErrorKind::AddrNotAvailable
            | ErrorKind::NetworkDown
            | ErrorKind::NetworkUnreachable
            | ErrorKind::HostUnreachable
            | ErrorKind::UnexpectedEof
    )
}

fn startup_datagram_retry(error: &DatagramTransportError) -> RetryClass {
    use proletarius_vowifi::{RelayError, SocksCodecError};
    let transient = match error {
        DatagramTransportError::Io(error)
        | DatagramTransportError::Relay(RelayError::Io(error)) => transient_startup_io(error),
        DatagramTransportError::Relay(RelayError::AssociateTimeout | RelayError::ControlClosed) => {
            true
        }
        // SOCKS network/host unreachable, connection refused and TTL expiry.
        // Policy/authentication/unsupported-command replies are permanent.
        DatagramTransportError::Relay(RelayError::Codec(SocksCodecError::AssociateRejected(
            3..=6,
        ))) => true,
        _ => false,
    };
    if transient {
        RetryClass::RetryAfterRelease
    } else {
        RetryClass::Permanent
    }
}

fn startup_registration_retry<E>(
    error: &proletarius_vowifi::registration_coordinator::RegistrationError<E>,
) -> RetryClass {
    use proletarius_ims::{SipError, registration_io::RegisterWaitError};
    use proletarius_vowifi::registration_coordinator::RegistrationError;
    match error {
        RegistrationError::Cancelled(_) => RetryClass::Stop,
        RegistrationError::Deadline(_) => RetryClass::RetryAfterRelease,
        RegistrationError::Io { source, .. } => {
            if transient_startup_io(source) {
                RetryClass::RetryAfterRelease
            } else {
                RetryClass::Permanent
            }
        }
        RegistrationError::Transport { source, .. } => match source {
            RegisterWaitError::Disconnected | RegisterWaitError::Deadline => {
                RetryClass::RetryAfterRelease
            }
            RegisterWaitError::Cancelled => RetryClass::Stop,
            RegisterWaitError::Io(error) => {
                if transient_startup_io(error) {
                    RetryClass::RetryAfterRelease
                } else {
                    RetryClass::Permanent
                }
            }
            // These final REGISTER statuses explicitly describe timeout or
            // temporary server/gateway failure. 401/403/407 and malformed SIP
            // remain permanent, even though wrapped as Transport errors.
            RegisterWaitError::Sip(SipError::RegistrationRejected(408 | 500 | 502 | 503 | 504)) => {
                RetryClass::RetryAfterRelease
            }
            _ => RetryClass::Permanent,
        },
        _ => RetryClass::Permanent,
    }
}

fn startup_retry(error: &NativeRegisteredStartupFailure) -> RetryClass {
    match error {
        NativeRegisteredStartupFailure::Tunnel(
            NativeTunnelStartError::Transport(error)
            | NativeTunnelStartError::Startup(TunnelStartupError::Transport(error)),
        ) => startup_datagram_retry(error),
        NativeRegisteredStartupFailure::Tunnel(NativeTunnelStartError::Startup(
            TunnelStartupError::Deadline,
        ))
        | NativeRegisteredStartupFailure::Registration {
            error: NativeImsRegistrationError::TransportClosed,
            ..
        } => RetryClass::RetryAfterRelease,
        NativeRegisteredStartupFailure::Registration {
            error: NativeImsRegistrationError::Registration(error),
            ..
        } if error.profile_error().is_none() => {
            startup_registration_retry(error.registration().cause())
        }
        NativeRegisteredStartupFailure::Tunnel(
            NativeTunnelStartError::Stopped
            | NativeTunnelStartError::Startup(TunnelStartupError::Cancelled),
        ) => RetryClass::Stop,
        _ => RetryClass::Permanent,
    }
}

fn classify_outcome(outcome: &Outcome, user_stop: bool) -> Termination {
    let detail = match outcome {
        Outcome::Startup { error, .. } => startup_detail(error),
        Outcome::Setup {
            reason,
            cleanup,
            call_archive,
            ..
        } => format!(
            "setup: {reason}; {}; call archive: {call_archive:?}",
            startup_detail(cleanup)
        ),
        Outcome::Running(Err(error)) => format!("ordinary worker join: {error}"),
        Outcome::Running(Ok(exit)) => format!(
            "actor: {:?}; actor cleanup: {:?}; consumer: {:?}; timers: {:?}; media cleanup: {:?}; network: {:?}; call archive: {:?}; local retired: {}",
            exit.actor.as_ref().map(|e| e.reason),
            exit.actor.as_ref().map(|e| &e.cleanup),
            exit.consumer.as_ref().map(|e| e.reason),
            exit.consumer.as_ref().map(|e| &e.timer_failures),
            exit.consumer.as_ref().map(|e| &e.media_cleanup),
            exit.network,
            exit.call_archive,
            exit.local_retired
        ),
    };
    let (reason, retry, code) = if user_stop {
        (EndReason::UserStop, RetryClass::Stop, None)
    } else {
        match outcome {
            Outcome::Startup {
                error: NativeRegisteredStartupFailure::Tunnel(NativeTunnelStartError::LeaseLost),
                ..
            } => (
                EndReason::ReaderChanged,
                RetryClass::Stop,
                Some("native_reader_changed"),
            ),
            Outcome::Startup { error, .. } => (
                EndReason::StartupFailure,
                startup_retry(error),
                Some("native_vowifi_startup_failed"),
            ),
            Outcome::Setup { .. } => (
                EndReason::SetupFailure,
                RetryClass::Permanent,
                Some("native_vowifi_setup_failed"),
            ),
            Outcome::Running(Err(_)) => (
                EndReason::InternalFailure,
                RetryClass::Permanent,
                Some("native_vowifi_worker_failed"),
            ),
            Outcome::Running(Ok(exit)) => classify_running(exit),
        }
    };
    let mut termination = Termination::new(reason, retry, code, Some(detail));
    if !user_stop && let Outcome::Startup { error, .. } = outcome {
        termination.startup_failure = Some(startup_diagnostics::startup(error));
    }
    termination
}

fn classify_running(
    exit: &NativeRegisteredWorkerExit,
) -> (EndReason, RetryClass, Option<&'static str>) {
    use native_ims_actor::StopReason as Actor;
    use registered::StopReason as Consumer;
    if exit.lease_lost
        || exit
            .actor
            .as_ref()
            .is_ok_and(|e| e.reason == Actor::LeaseLost)
        || exit
            .consumer
            .as_ref()
            .is_ok_and(|e| e.reason == Consumer::LeaseLost)
    {
        return (
            EndReason::ReaderChanged,
            RetryClass::Stop,
            Some("native_reader_changed"),
        );
    }
    if matches!(
        &exit.network,
        Ok(Err(NativeNetworkError::Tunnel(
            proletarius_vowifi::IkeAuthError::PlannedReauthentication
        )))
    ) {
        return (
            EndReason::PlannedReauthentication,
            RetryClass::RetryAfterRelease,
            None,
        );
    }
    if !matches!(&exit.network, Ok(Ok(()))) {
        return (
            EndReason::NetworkFailure,
            RetryClass::RetryAfterRelease,
            Some("native_vowifi_network_failed"),
        );
    }
    if exit
        .actor
        .as_ref()
        .is_ok_and(|actor| actor.reason == Actor::HeartbeatFailed)
    {
        return (
            EndReason::NetworkFailure,
            RetryClass::RetryAfterRelease,
            Some("native_ims_heartbeat_failed"),
        );
    }
    if exit
        .consumer
        .as_ref()
        .is_ok_and(|e| e.reason == Consumer::RenewalDue)
    {
        return (
            EndReason::RegistrationRenewal,
            RetryClass::RetryAfterRelease,
            None,
        );
    }
    if exit
        .actor
        .as_ref()
        .is_ok_and(|e| matches!(e.reason, Actor::StreamClosed | Actor::StreamFailed))
    {
        return (
            EndReason::RemoteFailure,
            RetryClass::RetryAfterRelease,
            Some("native_vowifi_remote_closed"),
        );
    }
    // Requested here can be the cleanup flag set by a sibling owner. Only the
    // captured Handle's explicit stop flag is evidence of user intent.
    (
        EndReason::InternalFailure,
        RetryClass::Permanent,
        Some("native_vowifi_dispatcher_stopped"),
    )
}

fn make_setup(
    context: &NativeSessionContext,
    session: &RegisteredImsSession,
    permit: &NativeSessionPermit,
    data_root: PathBuf,
    transactions: crate::ims_sms_transactions::Service,
    call_facts: crate::call_facts::CallFacts,
    sms_only: bool,
    archive: crate::ims_sms_transactions::NativeSmsArchive,
    archive_warning_observer: Option<crate::native_sms_consumer::ArchiveWarningObserver>,
) -> Result<NativeRegisteredSetup, String> {
    let calls = crate::native_ims_call_setup::prepare(
        session,
        &context.ims,
        &context.runtime.profile,
        sms_only,
    )?;
    let identity = session.call_context().map_err(str::to_owned)?;
    let minimum_cseq = identity
        .register_cseq()
        .checked_add(1)
        .ok_or("registered_cseq_exhausted")?;
    let module = context.card.kind() == card::SessionKind::Module;
    let session_id = format!(
        "native-{}-ims-{}",
        if module { "module" } else { "reader" },
        permit.generation().value()
    );
    let facts = if module {
        crate::call_facts::NativeHistory::start_module_ims(call_facts, session_id.clone(), 64)
    } else {
        crate::call_facts::NativeHistory::start_reader_ims(call_facts, session_id.clone(), 64)
    };
    Ok(NativeRegisteredSetup {
        sms: NativeSmsSetup {
            archive,
            archive_warning_observer,
            data_root,
            subscription_id: context.sms_subscription_id.clone(),
            configured_identity: identity.public_identity().into(),
            fallback_routes: identity.routing().routes().to_vec(),
            access_network_info: Some(identity.pani().into()),
            minimum_cseq,
            transaction_service: transactions,
            // The transaction service requires at least 16 characters, including
            // the first (single-digit) native session generation.
            transaction_session_id: session_id,
        },
        smsc: context.card.identity().smsc.clone(),
        sms_target: context.card.identity().smsc_target.clone(),
        calls,
        call_facts: facts,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod sms_binding_test_support;
