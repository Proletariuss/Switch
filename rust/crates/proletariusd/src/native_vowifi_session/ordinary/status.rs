//! Facts for one captured reader and native IMS generation. No subscriber
//! identity or diagnostic error is automatically serialized into public status.
use super::{Phase, SessionGeneration};
use crate::native_vowifi_supervisor::ReaderSessionKey;
use proletarius_vowifi::{TunnelConfiguration, registration_coordinator::RegisteredImsSession};
use std::sync::Arc;
use tokio::sync::watch;
use zeroize::Zeroizing;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    pub(crate) reader: String,
    pub(crate) reader_generation: u64,
    pub(crate) session_generation: SessionGeneration,
    pub(super) key: ReaderSessionKey,
}
impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding")
            .field("reader", &self.reader)
            .field("reader_generation", &self.reader_generation)
            .field("session_generation", &self.session_generation)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndReason {
    UserStop,
    ReaderChanged,
    RegistrationRenewal,
    PlannedReauthentication,
    StartupFailure,
    SetupFailure,
    RemoteFailure,
    NetworkFailure,
    InternalFailure,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetryClass {
    Stop,
    Permanent,
    RetryAfterRelease,
}
#[derive(Clone)]
pub(crate) struct Termination {
    pub(crate) reason: EndReason,
    pub(crate) retry: RetryClass,
    pub(crate) public_error: Option<&'static str>,
    pub(crate) startup_failure: Option<super::super::startup_diagnostics::Diagnostic>,
    detail: Option<Arc<Zeroizing<String>>>,
}
impl Termination {
    pub(super) fn new(
        reason: EndReason,
        retry: RetryClass,
        public_error: Option<&'static str>,
        detail: Option<String>,
    ) -> Self {
        Self {
            reason,
            retry,
            public_error,
            startup_failure: None,
            detail: detail.map(|s| Arc::new(Zeroizing::new(s))),
        }
    }
    /// Internal diagnostics only; public APIs use the fixed, redacted code.
    pub(crate) fn detail(&self) -> Option<&str> {
        self.detail.as_ref().map(|s| s.as_str())
    }
}
impl std::fmt::Debug for Termination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Termination")
            .field("reason", &self.reason)
            .field("retry", &self.retry)
            .field("public_error", &self.public_error)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub(crate) binding: Binding,
    pub(crate) phase: Phase,
    pub(crate) tunnel_started: bool,
    pub(crate) tunnel_ready: bool,
    pub(crate) register_started: bool,
    pub(crate) registered: bool,
    pub(crate) renewal_due: bool,
    pub(crate) registration_expires_at_ms: Option<i64>,
    pub(crate) registration_renew_at_ms: Option<i64>,
    pub(crate) sms_route_ready: bool,
    pub(crate) sms_ready: bool,
    pub(crate) call_ready: bool,
    voice_enabled: bool,
    pub(crate) local_ipv4: Vec<std::net::Ipv4Addr>,
    pub(crate) local_ipv6: Vec<std::net::Ipv6Addr>,
    pub(crate) pcscf: Vec<std::net::IpAddr>,
    pub(crate) registered_msisdn: Option<String>,
    pub(crate) sim_phone_number: Option<String>,
    pub(crate) registered_number_source: Option<&'static str>,
    pub(crate) registration_identity_error: Option<&'static str>,
    pub(crate) sms_archive_warning: Option<crate::ims_sms_transactions::NativeSmsArchiveWarning>,
    pub(crate) ims_transport: Option<&'static str>,
    pub(crate) ims_secured: bool,
    pub(crate) signaling_peer: Option<std::net::SocketAddr>,
    pub(crate) incoming: proletarius_vowifi::registration_coordinator::IncomingDiagnostics,
    pub(crate) termination: Option<Termination>,
    pub(crate) cleanup_confirmed: bool,
    pub(crate) lease_released: bool,
    pub(crate) cleanup_error: Option<&'static str>,
    cleanup_detail: Option<Diagnostic>,
}
#[derive(Clone)]
struct Diagnostic(Arc<Zeroizing<String>>);
impl std::fmt::Debug for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[private diagnostic]")
    }
}
impl Snapshot {
    pub(crate) fn active_sms_archive_warning(
        &self,
    ) -> Option<crate::ims_sms_transactions::NativeSmsArchiveWarning> {
        (self.phase == Phase::Registered
            && self.registered
            && self.termination.is_none()
            && !self.lease_released)
            .then_some(self.sms_archive_warning)
            .flatten()
    }
    pub(crate) fn cleanup_detail(&self) -> Option<&str> {
        self.cleanup_detail.as_ref().map(|s| s.0.as_str())
    }
    pub(crate) fn may_restart(&self) -> bool {
        self.phase == Phase::Released
            && self.cleanup_confirmed
            && self.lease_released
            && self
                .termination
                .as_ref()
                .is_some_and(|t| t.retry == RetryClass::RetryAfterRelease)
    }
}

#[derive(Clone)]
pub(crate) struct Reporter {
    state: watch::Sender<Snapshot>,
}
impl Reporter {
    #[cfg(test)]
    pub(super) fn set_test_snapshot(&self, snapshot: Snapshot) {
        assert_eq!(self.state.borrow().binding, snapshot.binding);
        self.state.send_replace(snapshot);
    }
    pub(super) fn new(binding: Binding) -> (Self, watch::Receiver<Snapshot>) {
        let (state, rx) = watch::channel(Snapshot {
            binding,
            phase: Phase::Starting,
            tunnel_started: false,
            tunnel_ready: false,
            register_started: false,
            registered: false,
            renewal_due: false,
            registration_expires_at_ms: None,
            registration_renew_at_ms: None,
            sms_route_ready: false,
            sms_ready: false,
            call_ready: false,
            voice_enabled: false,
            local_ipv4: Vec::new(),
            local_ipv6: Vec::new(),
            pcscf: Vec::new(),
            registered_msisdn: None,
            sim_phone_number: None,
            registered_number_source: None,
            registration_identity_error: None,
            sms_archive_warning: None,
            ims_transport: None,
            ims_secured: false,
            signaling_peer: None,
            incoming: Default::default(),
            termination: None,
            cleanup_confirmed: false,
            lease_released: false,
            cleanup_error: None,
            cleanup_detail: None,
        });
        (Self { state }, rx)
    }
    pub(super) fn sim_phone_number(&self, number: &str) {
        if !crate::profile_notes::is_missing_phone(number) {
            self.state
                .send_modify(|s| s.sim_phone_number = Some(number.trim().to_owned()));
        }
    }
    pub(crate) fn tunnel_started(&self) {
        self.state.send_modify(|s| {
            if s.phase == Phase::Starting {
                s.tunnel_started = true;
            }
        });
    }
    pub(crate) fn tunnel_ready(&self, config: &TunnelConfiguration) {
        self.state.send_modify(|s| {
            if s.phase != Phase::Starting {
                return;
            }
            s.tunnel_started = true;
            s.tunnel_ready = true;
            s.local_ipv4 = config.ipv4_addresses.clone();
            s.local_ipv6 = config.ipv6_addresses.iter().map(|(ip, _)| *ip).collect();
            s.pcscf = config.pcscf_servers.clone();
        });
    }
    pub(crate) fn register_started(&self) {
        self.state.send_modify(|s| {
            if s.phase == Phase::Starting && s.tunnel_ready {
                s.register_started = true;
            }
        });
    }
    pub(crate) fn registered(&self, session: &RegisteredImsSession) {
        self.state.send_modify(|s| {
            if s.phase != Phase::Starting || !s.register_started {
                return;
            }
            s.registered = true;
            s.ims_transport = Some("TCP");
            s.ims_secured = session.is_secured();
            s.signaling_peer = Some(session.signaling_peer());
            s.incoming = session.incoming_diagnostics();
            s.registered_msisdn = session
                .call_context()
                .ok()
                .and_then(|c| c.routing().registered_msisdn().map(str::to_owned));
        });
    }
    pub(super) fn registration_identity(
        &self,
        number: Option<String>,
        source: Option<&'static str>,
        error: Option<&'static str>,
    ) {
        self.state.send_modify(|s| {
            if s.phase == Phase::Starting && s.registered && !s.lease_released {
                s.registered_msisdn = number;
                s.registered_number_source = source;
                s.registration_identity_error = error;
            }
        });
    }
    pub(super) fn sms_archive_warning_observer(
        &self,
    ) -> crate::native_sms_consumer::ArchiveWarningObserver {
        let reporter = self.clone();
        crate::native_sms_consumer::ArchiveWarningObserver::new(move |generation, warning| {
            reporter.state.send_if_modified(|s| {
                if s.binding.session_generation != generation
                    || s.phase != Phase::Registered
                    || !s.registered
                    || s.termination.is_some()
                    || s.lease_released
                    || s.sms_archive_warning == Some(warning)
                {
                    return false;
                }
                s.sms_archive_warning = Some(warning);
                true
            });
        })
    }
    pub(super) fn incoming(
        &self,
        facts: proletarius_vowifi::registration_coordinator::IncomingDiagnostics,
    ) {
        self.state.send_modify(|s| {
            if !s.lease_released {
                s.incoming = facts;
            }
        });
    }
    pub(super) fn consumers_ready(&self, smsc: &str, target: &str) {
        self.profile_consumers_ready(smsc, target, true);
    }
    pub(super) fn profile_consumers_ready(&self, smsc: &str, target: &str, voice: bool) {
        self.state.send_modify(|s| {
            if s.phase == Phase::Starting && s.registered {
                s.phase = Phase::Registered;
                s.sms_route_ready = !smsc.trim().is_empty() && !target.trim().is_empty();
                s.sms_ready = s.sms_route_ready;
                s.voice_enabled = voice;
                s.call_ready = voice;
            }
        });
    }
    pub(super) fn heartbeat_health(&self, healthy: bool) {
        self.state.send_modify(|s| {
            // Registration and consumer startup are independent facts. Late
            // recovery cannot recreate readiness after drain or exact release.
            if s.phase == Phase::Registered && s.registered && !s.lease_released {
                s.sms_ready = healthy && s.sms_route_ready;
                s.call_ready = healthy && s.voice_enabled;
            }
        });
    }
    pub(super) fn registration_lease(
        &self,
        _seconds: u64,
        expiry: tokio::time::Instant,
        renewal: tokio::time::Instant,
    ) {
        let instant = tokio::time::Instant::now();
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let absolute = |deadline: tokio::time::Instant| {
            wall.saturating_add(
                deadline
                    .saturating_duration_since(instant)
                    .as_millis()
                    .min(i64::MAX as u128) as i64,
            )
        };
        self.state.send_modify(|s| {
            if matches!(s.phase, Phase::Starting | Phase::Registered) && s.termination.is_none() {
                s.registration_expires_at_ms = Some(absolute(expiry));
                s.registration_renew_at_ms = Some(absolute(renewal));
                s.renewal_due = false;
            }
        });
    }
    pub(super) fn renewal_due(&self) {
        self.state.send_modify(|s| s.renewal_due = true);
    }
    pub(super) fn draining(&self) {
        self.state.send_modify(|s| {
            if !matches!(s.phase, Phase::Released | Phase::CleanupUnconfirmed) {
                s.phase = Phase::Draining;
            }
            s.sms_ready = false;
            s.call_ready = false;
        });
    }
    pub(super) fn terminated(&self, termination: Termination) {
        self.draining();
        self.state
            .send_modify(|s| s.termination = Some(termination));
    }
    pub(super) fn cleanup_confirmed(&self) {
        self.state.send_modify(|s| s.cleanup_confirmed = true);
    }
    /// Called only by the physical release acknowledgement path, including
    /// retained recovery of the original lease. A closed watch is never proof.
    pub(crate) fn lease_released(&self) {
        self.state.send_modify(|s| {
            s.cleanup_confirmed = true;
            s.lease_released = true;
            s.cleanup_error = None;
            s.phase = Phase::Released;
            s.sms_ready = false;
            s.call_ready = false;
            s.incoming.listener_started = false;
            s.incoming.listener_closing = false;
            s.incoming.closed_connections = s.incoming.closed_connections.saturating_add(
                u64::from(s.incoming.active_connections)
                    + u64::from(s.incoming.closing_connections),
            );
            s.incoming.active_connections = 0;
            s.incoming.closing_connections = 0;
        });
    }
    pub(super) fn cleanup_unconfirmed(&self) {
        self.state.send_modify(|s| {
            if !s.lease_released {
                s.phase = Phase::CleanupUnconfirmed;
                s.cleanup_error = Some("native_vowifi_cleanup_unconfirmed");
            }
            s.sms_ready = false;
            s.call_ready = false;
        });
    }
    pub(super) fn release_failed(&self, detail: String) {
        self.state.send_modify(|s| {
            // Even if retained recovery confirmed release just before this
            // return, keep the original failed waiter's diagnostic receipt.
            s.cleanup_detail = Some(Diagnostic(Arc::new(Zeroizing::new(detail))));
        });
        self.cleanup_unconfirmed();
    }
}

#[cfg(test)]
mod tests;
