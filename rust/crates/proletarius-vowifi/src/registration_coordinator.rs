//! One-owner IMS REGISTER orchestration across the plain and protected TCP
//! associations. The coordinator keeps socket retirement ahead of SA removal
//! and never exposes separable security policy/continuation parts.

use crate::{
    EapDirectAuthMaterial,
    ims_profile::{
        NativeImsDeviceIdentity, NativeImsOptionsPolicy, NativeImsProfileError,
        NativeImsRegisterAttempt, NativeImsRegisterProfile, NativeImsRuntime, NativeImsSessionMode,
        NativeImsSubscriberIdentity, NativeSecurityEndpoints,
    },
    netstack_tcp::{ManagedTcpStream, PendingTcpClose, PendingTcpListener, TcpControl},
    security_runtime::{
        InstalledSecAgree, LeasedRegistrationConnection, SecurityControl, SecurityControlError,
        SecurityLease,
    },
    sms_transaction::{SmsRequestContext, SmsSubmission, build_sms_message},
};
use proletarius_ims::{
    PlainSecurityDisposition, SecAgreeMode, SipError, SipResponse,
    aka_authorization::{AuthorizationHeader, AuthorizationRequest},
    aka_exchange::{AuthenticatedChallenge, ExchangeError, SimAnswer, answer_challenge},
    register_lease::evaluate_register_lease_headers,
    registration_connection::{
        ChallengeResolution, PlainAdvanceError, ProtectedRegisterAttempt, RegistrationConnection,
        RegistrationProgress, SecAgreeParameters, SecAgreePrepareError,
    },
    registration_io::RegisterWaitError,
    security_offer::ClientSecurityMechanism,
    security_policy::LocalEndpoints,
    stream::{SipStreamItem, SipStreamReader},
};
use proletarius_sip::{Message as WireMessage, Parser as WireParser};
use std::{
    future::Future,
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, ReadHalf, WriteHalf},
    sync::{Mutex as AsyncMutex, mpsc, watch},
    time::Instant,
};
use zeroize::Zeroizing;

const SESSION_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
mod incoming;
pub use incoming::{
    IncomingDiagnostics, IncomingObserver, IncomingReachability, IncomingWriteError,
};
const SESSION_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_MESSAGE_LIMIT: usize = 64 * 1024;
const MAX_INCOMING_SIGNAL_STREAMS: usize = 2;

#[derive(Clone, Copy)]
pub struct RegistrationPlan<'a> {
    pub call_id: &'a str,
    pub initial_cseq: u32,
    pub sec_agree: SecAgreeMode,
    pub plain_peer: SocketAddr,
    pub plain_local_port: u16,
    pub local_ip: std::net::IpAddr,
    pub local_endpoints: LocalEndpoints,
    pub client_mechanisms: &'a [ClientSecurityMechanism<'a>],
    pub strict_security: bool,
}

pub enum RegisterAttempt<'a> {
    Initial {
        cseq: u32,
        local_port: u16,
    },
    PlainAuthenticated {
        cseq: u32,
        local_port: u16,
        authorization: &'a AuthorizationHeader,
        security_disposition: PlainSecurityDisposition,
    },
    Protected {
        cseq: u32,
        attempt: ProtectedRegisterAttempt<'a>,
    },
}

/// Compiles a REGISTER variant for an arbitrary attempt lifetime. The trait
/// form lets profile-backed builders retain carrier state without weakening
/// the coordinator's borrowed Authorization/Protected-REGISTER inputs.
pub trait RegisterRequestBuilder {
    fn build(&mut self, attempt: RegisterAttempt<'_>) -> Result<Zeroizing<Vec<u8>>, SipError>;
    fn next_initial(
        &mut self,
        _error: &RegisterWaitError,
        _cseq: u32,
        _local_port: u16,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SipError> {
        Ok(None)
    }
}

impl<F> RegisterRequestBuilder for F
where
    F: for<'a> FnMut(RegisterAttempt<'a>) -> Result<Zeroizing<Vec<u8>>, SipError>,
{
    fn build(&mut self, attempt: RegisterAttempt<'_>) -> Result<Zeroizing<Vec<u8>>, SipError> {
        self(attempt)
    }
}

struct ClosureRequestBuilder<F>(F);

impl<F> RegisterRequestBuilder for ClosureRequestBuilder<F>
where
    F: for<'a> FnMut(RegisterAttempt<'a>) -> Result<Zeroizing<Vec<u8>>, SipError>,
{
    fn build(&mut self, attempt: RegisterAttempt<'_>) -> Result<Zeroizing<Vec<u8>>, SipError> {
        (self.0)(attempt)
    }
}

#[derive(Clone, Copy)]
pub struct RegistrationServices<'a> {
    tcp: &'a TcpControl,
    security: &'a SecurityControl,
    events: &'a mpsc::Sender<SipStreamItem>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SipConnectionId {
    Primary,
    Incoming(u64),
}

pub struct SessionSipEvent {
    connection: SipConnectionId,
    item: SipStreamItem,
}

impl SessionSipEvent {
    pub fn connection(&self) -> SipConnectionId {
        self.connection
    }

    pub fn item(&self) -> &SipStreamItem {
        &self.item
    }

    pub fn into_item(self) -> SipStreamItem {
        self.item
    }
}

impl<'a> RegistrationServices<'a> {
    pub const fn new(
        tcp: &'a TcpControl,
        security: &'a SecurityControl,
        events: &'a mpsc::Sender<SipStreamItem>,
    ) -> Self {
        Self {
            tcp,
            security,
            events,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationStage {
    Validate,
    PlainDial,
    PlainConnect,
    InitialRegister,
    Authenticate,
    PlainAuthenticatedRegister,
    PrepareSecurity,
    RetirePlain,
    InstallSecurity,
    ProtectedListen,
    ProtectedDial,
    ProtectedConnect,
    ProtectedTemplate,
    ProtectedRegister,
    Finalize,
}

#[derive(Debug)]
pub enum RegistrationError<E> {
    InvalidPlan(&'static str),
    Cancelled(RegistrationStage),
    Deadline(RegistrationStage),
    Io {
        stage: RegistrationStage,
        source: io::Error,
    },
    Sip {
        stage: RegistrationStage,
        source: SipError,
    },
    Transport {
        stage: RegistrationStage,
        source: RegisterWaitError,
    },
    Authentication(E),
    SecurityPreparation(SecAgreePrepareError),
    SecurityInstall(SecurityControlError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupStage {
    PrimarySocket,
    IncomingListener,
    SecurityAssociation,
    Invariant,
}

#[derive(Debug)]
pub struct CleanupIssue {
    stage: CleanupStage,
    message: String,
}

impl CleanupIssue {
    pub fn stage(&self) -> CleanupStage {
        self.stage
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Default)]
pub struct CleanupReport {
    issues: Vec<CleanupIssue>,
}

impl CleanupReport {
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn issues(&self) -> &[CleanupIssue] {
        &self.issues
    }

    fn push(&mut self, stage: CleanupStage, error: impl std::fmt::Display) {
        self.issues.push(CleanupIssue {
            stage,
            message: error.to_string(),
        });
    }
}

#[derive(Debug)]
pub struct RegistrationFailure<E> {
    cause: RegistrationError<E>,
    cleanup: CleanupReport,
}

/// A profile-aware native REGISTER failure. `profile_error` is populated when
/// the carrier template rejected a generated REGISTER; the coordinator
/// failure is retained as well because it contains the exact cleanup report.
pub struct NativeProfileRegistrationFailure<E> {
    registration: RegistrationFailure<ExchangeError<E>>,
    profile_error: Option<NativeImsProfileError>,
}

impl<E> NativeProfileRegistrationFailure<E> {
    pub fn registration(&self) -> &RegistrationFailure<ExchangeError<E>> {
        &self.registration
    }

    pub fn profile_error(&self) -> Option<&NativeImsProfileError> {
        self.profile_error.as_ref()
    }
}

struct NativeProfileRequestBuilder<'a> {
    profile: &'a NativeImsRegisterProfile,
    device_identity: &'a NativeImsDeviceIdentity,
    subscriber_identity: Option<&'a NativeImsSubscriberIdentity>,
    runtime: NativeImsRuntime<'a>,
    endpoints: NativeSecurityEndpoints,
    eap_direct_material: Option<&'a EapDirectAuthMaterial>,
    profile_error: Arc<Mutex<Option<NativeImsProfileError>>>,
    initial_variants: &'a [proletarius_ims::register_fallback::InitialRegisterVariant],
    initial_index: usize,
    fallback_environment: bool,
    initial_tokens: Option<(String, String)>,
}

impl RegisterRequestBuilder for NativeProfileRequestBuilder<'_> {
    fn build(&mut self, attempt: RegisterAttempt<'_>) -> Result<Zeroizing<Vec<u8>>, SipError> {
        let attempt = match attempt {
            RegisterAttempt::Initial { cseq, local_port } => NativeImsRegisterAttempt::Initial {
                cseq,
                local_port,
                endpoints: self.endpoints,
            },
            RegisterAttempt::PlainAuthenticated {
                cseq,
                local_port,
                authorization,
                security_disposition,
            } => NativeImsRegisterAttempt::PlainAuthenticated {
                cseq,
                local_port,
                endpoints: self.endpoints,
                authorization,
                security_disposition,
            },
            RegisterAttempt::Protected { cseq, attempt } => {
                let context = attempt.context;
                NativeImsRegisterAttempt::Protected {
                    cseq,
                    endpoints: self.endpoints,
                    authorization: attempt.authorization,
                    security_verify: context.security_verify(),
                    remote_port_s: context.remote_port_s(),
                }
            }
        };
        let runtime = NativeImsRuntime {
            from_tag: self
                .initial_tokens
                .as_ref()
                .map_or(self.runtime.from_tag, |t| &t.0),
            via_branch: self
                .initial_tokens
                .as_ref()
                .map_or(self.runtime.via_branch, |t| &t.1),
            ..self.runtime
        };
        let compiled = match self.subscriber_identity {
            Some(subscriber_identity) => self.profile.compile_with_initial_variant(
                self.device_identity,
                runtime,
                subscriber_identity,
                attempt,
                self.eap_direct_material,
                self.initial_variants.get(self.initial_index),
            ),
            None => self.profile.compile_with_eap_direct_material(
                self.device_identity,
                runtime,
                attempt,
                self.eap_direct_material,
            ),
        };
        compiled.map_err(|error| {
            let sip_error = match &error {
                NativeImsProfileError::Sip(sip_error) => sip_error.clone(),
                _ => SipError::InvalidHeader,
            };
            *self
                .profile_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
            sip_error
        })
    }
    fn next_initial(
        &mut self,
        error: &RegisterWaitError,
        cseq: u32,
        local_port: u16,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SipError> {
        if self.initial_index + 1 >= self.initial_variants.len() {
            return Ok(None);
        }
        let allowed = match error {
            RegisterWaitError::Sip(SipError::RegistrationRejected(status)) => {
                self.profile
                    .initial_register_fallback(self.fallback_environment, Some(*status))
                    .retry
            }
            RegisterWaitError::Disconnected => {
                self.profile
                    .initial_register_fallback(self.fallback_environment, None)
                    .variants
                    .len()
                    > 1
            }
            RegisterWaitError::Io(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::UnexpectedEof
                ) =>
            {
                self.profile
                    .initial_register_fallback(self.fallback_environment, None)
                    .variants
                    .len()
                    > 1
            }
            _ => false,
        };
        if !allowed {
            return Ok(None);
        }
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| SipError::InvalidHeader)?;
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        self.initial_tokens = Some((
            hex(&random[..16]),
            format!("z9hG4bK-{}", hex(&random[16..])),
        ));
        self.initial_index += 1;
        self.build(RegisterAttempt::Initial { cseq, local_port })
            .map(Some)
    }
}

impl<E> RegistrationFailure<E> {
    pub fn cause(&self) -> &RegistrationError<E> {
        &self.cause
    }

    pub fn cleanup(&self) -> &CleanupReport {
        &self.cleanup
    }
}

pub type RegistrationLeaseObserver = std::sync::Arc<dyn Fn(u64, Instant, Instant) + Send + Sync>;

/// A disabled probe is an explicit frozen policy, distinct from an absent or
/// invalid configuration. Refresh never reconstructs this policy from context.
pub(crate) enum RegisteredOptionsConfiguration {
    Probe(crate::sip_session_runtime::HeartbeatConfiguration),
    DisabledForSmsOnlyVodafoneUk,
}

pub struct RegisteredImsSession {
    registration_lease_observer: Option<RegistrationLeaseObserver>,
    refresh_configuration: Option<crate::sip_session_runtime::refresh::Configuration>,
    register_request: Option<Zeroizing<Vec<u8>>>,
    registration_expiry: Option<Instant>,
    heartbeat_configuration: Option<RegisteredOptionsConfiguration>,
    heartbeat_health: crate::sip_session_runtime::HeartbeatHealth,
    heartbeat_observer: Option<crate::sip_session_runtime::HeartbeatObserver>,
    incoming_facts: IncomingDiagnostics,
    incoming_observer: Option<IncomingObserver>,
    tcp_owner: Option<TcpControl>,
    primary: SignalStream,
    listener: Option<PendingTcpListener>,
    listener_config: Option<IncomingListenerConfig>,
    incoming: [Option<IncomingSignalStream>; MAX_INCOMING_SIGNAL_STREAMS],
    next_incoming_id: u64,
    stop: watch::Receiver<bool>,
    cancellation_owner: Option<watch::Sender<bool>>,
    lease: Option<SecurityLease>,
    final_response: SipResponse,
    registration_expires_seconds: Option<u64>,
    registration_renewal_deadline: Option<Instant>,
    startup_events: std::collections::VecDeque<SipStreamItem>,
    call_context: Option<RegisteredCallContext>,
    register_contact: Option<Zeroizing<String>>,
    register_peer: SocketAddr,
    register_port_s: u16,
    register_cseq: u32,
    message_local_port: u16,
    message_local_ip: std::net::IpAddr,
    message_security_verify: Option<zeroize::Zeroizing<String>>,
}

/// Immutable call inputs derived only from a completed profile-backed REGISTER.
/// The private constructor prevents a public response/snapshot DTO from granting
/// dialog authority or inventing protected listener endpoints.
pub struct RegisteredCallContext {
    carrier_id: String,
    domain: String,
    realm: String,
    phone_context: String,
    template_id: String,
    encoded_icsi: String,
    public_identity: Zeroizing<String>,
    user_agent: String,
    pani: String,
    contact: Zeroizing<String>,
    routing: crate::message_routing::RegisteredMessageRouting,
    local_ip: std::net::IpAddr,
    port_c: u16,
    port_s: u16,
    register_cseq: u32,
    peer: SocketAddr,
    security_verify: Option<Zeroizing<String>>,
}
impl RegisteredCallContext {
    pub fn carrier_id(&self) -> &str {
        &self.carrier_id
    }
    pub fn domain(&self) -> &str {
        &self.domain
    }
    pub fn realm(&self) -> &str {
        &self.realm
    }
    pub fn phone_context(&self) -> &str {
        &self.phone_context
    }
    pub fn template_id(&self) -> &str {
        &self.template_id
    }
    pub fn encoded_icsi(&self) -> &str {
        &self.encoded_icsi
    }
    pub fn public_identity(&self) -> &str {
        &self.public_identity
    }
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }
    pub fn pani(&self) -> &str {
        &self.pani
    }
    pub fn contact(&self) -> &str {
        &self.contact
    }
    pub fn routing(&self) -> &crate::message_routing::RegisteredMessageRouting {
        &self.routing
    }
    pub fn local_ip(&self) -> std::net::IpAddr {
        self.local_ip
    }
    pub fn port_c(&self) -> u16 {
        self.port_c
    }
    pub fn port_s(&self) -> u16 {
        self.port_s
    }
    pub fn register_cseq(&self) -> u32 {
        self.register_cseq
    }
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }
    pub fn security_verify(&self) -> Option<&str> {
        self.security_verify.as_deref().map(String::as_str)
    }
}

#[derive(Clone)]
struct IncomingListenerConfig {
    tcp: TcpControl,
    local_port: u16,
    expected_peer: SocketAddr,
}

struct IncomingSignalStream {
    id: u64,
    stream: SignalStream,
}

struct OpenSignalStream {
    reader: SipStreamReader<ReadHalf<ManagedTcpStream>>,
    writer: WriteHalf<ManagedTcpStream>,
    pending: Option<PendingSipWrite>,
}

enum SignalStream {
    Open(OpenSignalStream),
    Closing {
        close: PendingTcpClose,
        cause: Option<StoredIoError>,
    },
    Closed,
}

struct PendingSipWrite {
    bytes: Zeroizing<Vec<u8>>,
    offset: usize,
    flushing: bool,
    deadline: Instant,
    response: bool,
}

#[derive(Clone)]
struct StoredIoError {
    kind: io::ErrorKind,
    message: String,
}

impl StoredIoError {
    fn new(kind: io::ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn into_error(self) -> io::Error {
        io::Error::new(self.kind, self.message)
    }
}

impl From<io::Error> for StoredIoError {
    fn from(error: io::Error) -> Self {
        Self::new(error.kind(), error.to_string())
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ConnectionProgress {
    Open,
    Closed,
}

enum SessionReady {
    Primary(io::Result<Option<SipStreamItem>>),
    Incoming(usize, io::Result<Option<SipStreamItem>>),
    Accepted(io::Result<ManagedTcpStream>),
    Cancelled,
}

impl SignalStream {
    fn open(
        reader: SipStreamReader<ReadHalf<ManagedTcpStream>>,
        writer: WriteHalf<ManagedTcpStream>,
    ) -> Self {
        Self::Open(OpenSignalStream {
            reader,
            writer,
            pending: None,
        })
    }
}

impl RegisteredImsSession {
    /// Opt in only with credentials bound to the same immutable session card.
    pub fn enable_registration_refresh(
        &mut self,
        username: String,
        authenticate: crate::sip_session_runtime::RefreshAuthenticate,
    ) -> io::Result<()> {
        self.refresh_configuration = Some(crate::sip_session_runtime::refresh::Configuration::new(
            self.register_request
                .as_deref()
                .ok_or_else(|| io::Error::other("REGISTER refresh seed unavailable"))?,
            username,
            authenticate,
            self.registration_expiry
                .ok_or_else(|| io::Error::other("REGISTER expiry unavailable"))?,
            self.registration_renewal_deadline
                .ok_or_else(|| io::Error::other("REGISTER renewal unavailable"))?,
            self.stop.clone(),
            self.is_secured(),
        )?);
        Ok(())
    }
    pub(crate) fn take_refresh_configuration(
        &mut self,
    ) -> Option<crate::sip_session_runtime::refresh::Configuration> {
        self.refresh_configuration.take()
    }
    pub(crate) fn refreshed(&mut self, seconds: u64, expiry: Instant, renewal: Instant) {
        self.registration_expires_seconds = Some(seconds);
        self.registration_expiry = Some(expiry);
        self.registration_renewal_deadline = Some(renewal);
        if let Some(observer) = &self.registration_lease_observer {
            observer(seconds, expiry, renewal);
        }
    }
    /// Read-only registration facts; this never schedules or initiates refresh.
    pub fn set_registration_lease_observer(&mut self, observer: RegistrationLeaseObserver) {
        if let (Some(seconds), Some(expiry), Some(renewal)) = (
            self.registration_expires_seconds,
            self.registration_expiry,
            self.refresh_configuration
                .as_ref()
                .map(|c| c.renewal_deadline())
                .or(self.registration_renewal_deadline),
        ) {
            observer(seconds, expiry, renewal);
        }
        self.registration_lease_observer = Some(observer);
    }

    /// Retain only the cancellation channel used to create this exact session.
    /// Startup may own its sole sender; successful REGISTER transfers that
    /// ownership here until every signaling socket/security owner is retired.
    pub fn retain_cancellation_owner(
        &mut self,
        owner: watch::Sender<bool>,
    ) -> Result<(), &'static str> {
        if !owner.subscribe().same_channel(&self.stop) {
            return Err("registered_cancellation_owner_mismatch");
        }
        self.cancellation_owner = Some(owner);
        Ok(())
    }

    /// Read-only handle for command admission; REGISTER does not prove that a
    /// later OPTIONS probe succeeded. Each session owns a distinct health cell.
    pub fn heartbeat_health(&self) -> crate::sip_session_runtime::HeartbeatHealth {
        self.heartbeat_health.clone()
    }
    pub fn set_heartbeat_observer(
        &mut self,
        observer: crate::sip_session_runtime::HeartbeatObserver,
    ) {
        self.heartbeat_observer = Some(observer);
    }
    pub(crate) fn publish_heartbeat_health(&self, healthy: bool) {
        self.heartbeat_health.set(healthy);
        if let Some(observer) = &self.heartbeat_observer {
            observer(healthy);
        }
    }
    pub(crate) fn take_heartbeat_configuration(
        &mut self,
    ) -> Option<RegisteredOptionsConfiguration> {
        self.heartbeat_configuration.take()
    }
    /// Queue synchronously before any await. The pending frame is resumed by
    /// the same writer after next_event cancellation; it is never queued twice.
    pub(crate) fn queue_heartbeat(&mut self, wire: &[u8]) -> io::Result<()> {
        if wire.len() > SESSION_MESSAGE_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "IMS heartbeat exceeds message limit",
            ));
        }
        let SignalStream::Open(open) = &mut self.primary else {
            return Err(connection_closed(SipConnectionId::Primary));
        };
        if open.pending.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "IMS primary writer is busy",
            ));
        }
        open.pending = Some(PendingSipWrite {
            bytes: Zeroizing::new(wire.to_vec()),
            offset: 0,
            flushing: false,
            deadline: Instant::now() + SESSION_WRITE_TIMEOUT,
            response: false,
        });
        Ok(())
    }
    pub(crate) async fn finish_heartbeat_write(&mut self) -> io::Result<()> {
        match self.drive_connection(SipConnectionId::Primary).await? {
            ConnectionProgress::Open => Ok(()),
            ConnectionProgress::Closed => Err(connection_closed(SipConnectionId::Primary)),
        }
    }
    /// Pure read: no accept poll, frame read, rearm, or network command.
    pub fn incoming_diagnostics(&self) -> IncomingDiagnostics {
        use crate::netstack_tcp::TcpListenerState;
        let mut facts = self.incoming_facts;
        facts.listener_started = self
            .listener
            .as_ref()
            .is_some_and(|l| l.state() == TcpListenerState::Armed);
        facts.listener_closing = self
            .listener
            .as_ref()
            .is_some_and(|l| l.state() == TcpListenerState::Closing);
        facts.active_connections = self
            .incoming
            .iter()
            .flatten()
            .filter(|c| matches!(c.stream, SignalStream::Open(_)))
            .count() as u32;
        facts.closing_connections = self
            .incoming
            .iter()
            .flatten()
            .filter(|c| matches!(c.stream, SignalStream::Closing { .. }))
            .count() as u32;
        facts
    }
    /// A callback may only copy facts/update a short lock; it must not perform
    /// I/O or reenter the mutable session. Each session owns its callback.
    pub fn set_incoming_observer(&mut self, observer: IncomingObserver) {
        self.incoming_observer = Some(observer);
        self.publish_incoming();
    }
    fn publish_incoming(&self) {
        if let Some(observer) = &self.incoming_observer {
            observer(self.incoming_diagnostics());
        }
    }
    pub(super) fn observe_incoming_sip(&mut self) {
        self.incoming_facts.last_sip_at = Some(std::time::SystemTime::now());
        self.incoming_facts.reachability = IncomingReachability::Observed;
        self.publish_incoming();
    }
    pub fn call_context(&self) -> Result<&RegisteredCallContext, &'static str> {
        self.call_context
            .as_ref()
            .ok_or("registered_call_context_unavailable")
    }

    pub fn belongs_to_tcp(&self, tcp: &TcpControl) -> bool {
        self.tcp_owner
            .as_ref()
            .is_some_and(|owner| owner.same_registry(tcp))
    }

    pub fn message_context(
        &self,
        configured_identity: &str,
        fallback_routes: &[String],
    ) -> Result<crate::message_routing::RegisteredMessageContext, &'static str> {
        Ok(crate::message_routing::RegisteredMessageContext::new(
            self.message_routing(configured_identity, fallback_routes)?,
            self.message_local_ip,
            self.message_local_port,
            self.message_security_verify(),
        ))
    }

    pub fn message_routing(
        &self,
        configured_identity: &str,
        fallback_routes: &[String],
    ) -> Result<crate::message_routing::RegisteredMessageRouting, &'static str> {
        crate::message_routing::RegisteredMessageRouting::from_response(
            &self.final_response,
            configured_identity,
            fallback_routes,
        )
    }

    /// Endpoint/security metadata captured from the successful association.
    pub fn message_local_port(&self) -> u16 {
        self.message_local_port
    }
    pub fn message_security_verify(&self) -> Option<&str> {
        self.message_security_verify
            .as_ref()
            .map(|value| value.as_str())
    }

    pub fn signaling_peer(&self) -> SocketAddr {
        self.register_peer
    }

    pub fn final_response(&self) -> &SipResponse {
        &self.final_response
    }

    /// Lease granted for the exact Contact binding sent in the successful
    /// REGISTER. `None` means the response/request could not be matched safely.
    pub fn registration_expires_seconds(&self) -> Option<u64> {
        self.registration_expires_seconds
    }

    /// Conservative in-session renewal point derived from the registrar's
    /// granted lease. The supervisor may use this to rebuild the session; this
    /// value does not itself send a refresh REGISTER.
    pub fn registration_renewal_deadline(&self) -> Option<Instant> {
        self.registration_renewal_deadline
    }

    pub fn is_secured(&self) -> bool {
        self.lease.is_some()
    }

    /// Return the next SIP item together with the reliable association on which
    /// it arrived. Responses to an incoming request must be sent back through
    /// the same `SipConnectionId` with `send_on`.
    pub async fn next_event(&mut self) -> io::Result<Option<SessionSipEvent>> {
        loop {
            if self.recover_pending_connections().await? == ConnectionProgress::Closed {
                return Ok(None);
            }
            if *self.stop.borrow() || self.stop.has_changed().is_err() {
                self.startup_events.clear();
            } else if let Some(item) = self.startup_events.pop_front() {
                return Ok(Some(SessionSipEvent {
                    connection: SipConnectionId::Primary,
                    item,
                }));
            }
            self.ensure_listener().await?;
            match self.select_next().await {
                SessionReady::Cancelled => {
                    self.begin_connection_close(
                        SipConnectionId::Primary,
                        Some(session_cancelled().into()),
                    )?;
                    return match self.drive_connection(SipConnectionId::Primary).await {
                        Err(error) => Err(error),
                        Ok(_) => Err(session_cancelled()),
                    };
                }
                SessionReady::Primary(result) => match result {
                    Ok(Some(item)) => {
                        return Ok(Some(SessionSipEvent {
                            connection: SipConnectionId::Primary,
                            item,
                        }));
                    }
                    Ok(None) => {
                        self.begin_connection_close(SipConnectionId::Primary, None)?;
                        self.drive_connection(SipConnectionId::Primary).await?;
                        return Ok(None);
                    }
                    Err(error) => {
                        self.begin_connection_close(SipConnectionId::Primary, Some(error.into()))?;
                        return match self.drive_connection(SipConnectionId::Primary).await {
                            Err(error) => Err(error),
                            Ok(_) => Err(io::Error::other(
                                "IMS primary signaling stream failed without a stored cause",
                            )),
                        };
                    }
                },
                SessionReady::Incoming(index, result) => match result {
                    Ok(Some(item)) => {
                        let id = self.incoming[index]
                            .as_ref()
                            .expect("selected incoming stream remains installed")
                            .id;
                        return Ok(Some(SessionSipEvent {
                            connection: SipConnectionId::Incoming(id),
                            item,
                        }));
                    }
                    Ok(None) => {
                        self.close_incoming(index).await?;
                    }
                    Err(error) => {
                        let id = self.incoming[index]
                            .as_ref()
                            .expect("selected incoming stream remains installed")
                            .id;
                        self.begin_connection_close(
                            SipConnectionId::Incoming(id),
                            Some(error.into()),
                        )?;
                        let result = self.drive_connection(SipConnectionId::Incoming(id)).await;
                        self.remove_closed_incoming(id);
                        return match result {
                            Err(error) => Err(error),
                            Ok(_) => Err(io::Error::other(
                                "IMS incoming signaling stream failed without a stored cause",
                            )),
                        };
                    }
                },
                SessionReady::Accepted(result) => {
                    // The one-shot listener has been consumed regardless of the
                    // handshake result. A successful connection is installed
                    // before the next loop immediately rearms port-s.
                    self.listener.take();
                    let stream = result?;
                    let Some(index) = self.incoming.iter().position(Option::is_none) else {
                        stream.close().await?;
                        continue;
                    };
                    let id = self.next_incoming_id;
                    let Some(next_id) = self.next_incoming_id.checked_add(1) else {
                        stream.close().await?;
                        return Err(io::Error::other(
                            "IMS incoming connection identifier exhausted",
                        ));
                    };
                    self.next_incoming_id = next_id;
                    let (reader, writer) = tokio::io::split(stream);
                    self.incoming[index] = Some(IncomingSignalStream {
                        id,
                        stream: SignalStream::open(SipStreamReader::new(reader), writer),
                    });
                    self.incoming_facts.accept_count =
                        self.incoming_facts.accept_count.saturating_add(1);
                    self.incoming_facts.reachability = IncomingReachability::Observed;
                    self.publish_incoming();
                }
            }
        }
    }

    pub async fn send(&mut self, message: &[u8]) -> io::Result<()> {
        self.send_on(SipConnectionId::Primary, message).await
    }

    /// Submit one encoded SMS part through the authenticated primary IMS
    /// association. The batch is marked in-flight before the write; if the
    /// future is cancelled or the transport fails, its drop guard freezes the
    /// result as unknown so the caller cannot silently replay it.
    pub async fn submit_sms_part(
        &mut self,
        submission: &mut SmsSubmission,
        context: &SmsRequestContext<'_>,
        rpdu: &[u8],
    ) -> io::Result<()> {
        let wire = build_sms_message(context, rpdu)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
        submission
            .mark_submitted(context.call_id, context.cseq)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
        let mut guard = UnknownOnSmsSubmitDrop {
            submission,
            finished: false,
        };
        let result = self.send(&wire).await;
        if result.is_ok() {
            guard.finished = true;
        }
        result
    }

    pub async fn send_on(&mut self, connection: SipConnectionId, message: &[u8]) -> io::Result<()> {
        self.send_on_until(connection, message, Instant::now() + SESSION_WRITE_TIMEOUT)
            .await
    }

    /// Send exactly one complete SIP frame before `deadline`.
    ///
    /// The in-flight frame and its progress remain owned by the selected
    /// connection. Cancelling this future never admits a later frame ahead of
    /// a partial prefix: the next send on this connection (or `next_event`)
    /// first resumes the old bytes and flush.
    pub async fn send_on_until(
        &mut self,
        connection: SipConnectionId,
        message: &[u8],
        deadline: Instant,
    ) -> io::Result<()> {
        let result = self
            .send_on_until_inner(connection, message, deadline)
            .await;
        if matches!(connection, SipConnectionId::Incoming(_))
            && message.starts_with(b"SIP/2.0 ")
            && let Err(error) = &result
        {
            self.incoming_facts.last_response_write_error =
                Some(IncomingWriteError::from_error(error));
            self.publish_incoming();
        }
        result
    }

    async fn send_on_until_inner(
        &mut self,
        connection: SipConnectionId,
        message: &[u8],
        deadline: Instant,
    ) -> io::Result<()> {
        if message.len() > SESSION_MESSAGE_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "IMS SIP message exceeds the session write limit",
            ));
        }
        if self.drive_connection(connection).await? == ConnectionProgress::Closed {
            return Err(connection_closed(connection));
        }
        let state = self.connection_state_mut(connection)?;
        let SignalStream::Open(open) = state else {
            return Err(connection_closed(connection));
        };
        debug_assert!(open.pending.is_none());
        open.pending = Some(PendingSipWrite {
            bytes: Zeroizing::new(message.to_vec()),
            offset: 0,
            flushing: message.is_empty(),
            deadline,
            response: message.starts_with(b"SIP/2.0 "),
        });
        match self.drive_connection(connection).await? {
            ConnectionProgress::Open => Ok(()),
            ConnectionProgress::Closed => Err(connection_closed(connection)),
        }
    }

    /// Close every TCP owner before clearing the exact security generation.
    pub async fn shutdown(self) -> Result<(), CleanupReport> {
        let mut facts = self.incoming_diagnostics();
        let Self {
            incoming_observer,
            tcp_owner: _,
            primary,
            listener,
            listener_config: _,
            incoming,
            next_incoming_id: _,
            stop: _,
            cancellation_owner: _cancellation_owner,
            lease,
            final_response: _,
            ..
        } = self;
        let mut cleanup = CleanupReport::default();
        if let Err(error) = close_signal_stream(primary).await {
            cleanup.push(CleanupStage::PrimarySocket, error);
        }
        for connection in incoming.into_iter().flatten() {
            let was_open = matches!(connection.stream, SignalStream::Open(_));
            let was_closed = matches!(connection.stream, SignalStream::Closed);
            if was_open {
                facts.active_connections = facts.active_connections.saturating_sub(1);
                facts.closing_connections = facts.closing_connections.saturating_add(1);
            }
            if let Some(observer) = &incoming_observer {
                observer(facts);
            }
            match close_signal_stream(connection.stream).await {
                Ok(()) if !was_closed => {
                    facts.closing_connections = facts.closing_connections.saturating_sub(1);
                    facts.closed_connections = facts.closed_connections.saturating_add(1);
                }
                Err(error) => cleanup.push(CleanupStage::PrimarySocket, error),
                _ => {}
            }
            if let Some(observer) = &incoming_observer {
                observer(facts);
            }
        }
        if let Some(listener) = listener {
            facts.listener_started = false;
            facts.listener_closing =
                listener.state() != crate::netstack_tcp::TcpListenerState::Retired;
            if let Some(observer) = &incoming_observer {
                observer(facts);
            }
            match listener.close().await {
                Ok(()) => facts.listener_closing = false,
                Err(error) => cleanup.push(CleanupStage::IncomingListener, error),
            }
            if let Some(observer) = &incoming_observer {
                observer(facts);
            }
        }
        if let Some(lease) = lease
            && let Err(error) = lease.clear().await
        {
            cleanup.push(CleanupStage::SecurityAssociation, error);
        }
        if cleanup.is_clean() {
            Ok(())
        } else {
            Err(cleanup)
        }
    }

    async fn recover_pending_connections(&mut self) -> io::Result<ConnectionProgress> {
        if self.drive_connection(SipConnectionId::Primary).await? == ConnectionProgress::Closed {
            return Ok(ConnectionProgress::Closed);
        }
        let ids = self
            .incoming
            .each_ref()
            .map(|connection| connection.as_ref().map(|connection| connection.id));
        for id in ids.into_iter().flatten() {
            let result = self.drive_connection(SipConnectionId::Incoming(id)).await;
            match result {
                Ok(ConnectionProgress::Open) => {}
                Ok(ConnectionProgress::Closed) => self.remove_closed_incoming(id),
                Err(error) => {
                    self.remove_closed_incoming(id);
                    return Err(error);
                }
            }
        }
        Ok(ConnectionProgress::Open)
    }

    async fn drive_connection(
        &mut self,
        connection: SipConnectionId,
    ) -> io::Result<ConnectionProgress> {
        loop {
            let state = self.connection_state_mut(connection)?;
            match state {
                SignalStream::Closed => return Ok(ConnectionProgress::Closed),
                SignalStream::Closing { close, cause } => {
                    let cause = cause.clone();
                    let close_result = close.wait_cancel_safe().await;
                    if close.removal_confirmed() {
                        *state = SignalStream::Closed;
                        if matches!(connection, SipConnectionId::Incoming(_)) {
                            self.incoming_facts.closed_connections =
                                self.incoming_facts.closed_connections.saturating_add(1);
                            self.publish_incoming();
                        }
                    }
                    return finish_connection_close(connection, cause, close_result);
                }
                SignalStream::Open(open) if open.pending.is_none() => {
                    return Ok(ConnectionProgress::Open);
                }
                SignalStream::Open(_) => {}
            }

            let deadline = match self.connection_state_mut(connection)? {
                SignalStream::Open(open) => {
                    open.pending
                        .as_ref()
                        .expect("pending write state checked")
                        .deadline
                }
                _ => continue,
            };
            let mut stop = self.stop.clone();
            let failure = if *stop.borrow() || stop.has_changed().is_err() {
                Some(session_cancelled())
            } else if Instant::now() >= deadline {
                Some(write_deadline_exceeded(connection))
            } else {
                let state = self.connection_state_mut(connection)?;
                let SignalStream::Open(open) = state else {
                    continue;
                };
                let pending = open.pending.as_mut().expect("pending write state checked");
                if !pending.flushing {
                    let bytes = &pending.bytes[pending.offset..];
                    let result = tokio::select! {
                        biased;
                        _ = wait_for_stop(&mut stop) => Err(session_cancelled()),
                        _ = tokio::time::sleep_until(deadline) => Err(write_deadline_exceeded(connection)),
                        result = open.writer.write(bytes) => result,
                    };
                    match result {
                        Ok(0) => Some(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "IMS signaling connection accepted zero bytes",
                        )),
                        Ok(count) => {
                            pending.offset += count;
                            if pending.offset == pending.bytes.len() {
                                pending.flushing = true;
                            }
                            None
                        }
                        Err(error) => Some(error),
                    }
                } else {
                    let result = tokio::select! {
                        biased;
                        _ = wait_for_stop(&mut stop) => Err(session_cancelled()),
                        _ = tokio::time::sleep_until(deadline) => Err(write_deadline_exceeded(connection)),
                        result = open.writer.flush() => result,
                    };
                    match result {
                        Ok(()) => {
                            open.pending = None;
                            return Ok(ConnectionProgress::Open);
                        }
                        Err(error) => Some(error),
                    }
                }
            };

            if let Some(error) = failure {
                if matches!(connection, SipConnectionId::Incoming(_))
                    && matches!(self.connection_state_mut(connection)?, SignalStream::Open(open)
                        if open.pending.as_ref().is_some_and(|pending| pending.response))
                {
                    self.incoming_facts.last_response_write_error =
                        Some(IncomingWriteError::from_error(&error));
                }
                self.begin_connection_close(connection, Some(error.into()))?;
            }
        }
    }

    fn connection_state_mut(
        &mut self,
        connection: SipConnectionId,
    ) -> io::Result<&mut SignalStream> {
        match connection {
            SipConnectionId::Primary => Ok(&mut self.primary),
            SipConnectionId::Incoming(id) => self
                .incoming
                .iter_mut()
                .flatten()
                .find(|candidate| candidate.id == id)
                .map(|candidate| &mut candidate.stream)
                .ok_or_else(|| connection_closed(connection)),
        }
    }

    fn begin_connection_close(
        &mut self,
        connection: SipConnectionId,
        cause: Option<StoredIoError>,
    ) -> io::Result<()> {
        let state = self.connection_state_mut(connection)?;
        let current = std::mem::replace(state, SignalStream::Closed);
        *state = match current {
            SignalStream::Open(open) => {
                let stream = open.reader.into_inner().unsplit(open.writer);
                let close = if cause.is_some() {
                    stream.begin_abort()
                } else {
                    stream.begin_close()
                };
                SignalStream::Closing { close, cause }
            }
            SignalStream::Closing {
                close,
                cause: previous,
            } => SignalStream::Closing {
                close,
                cause: previous.or(cause),
            },
            SignalStream::Closed => return Err(connection_closed(connection)),
        };
        if matches!(connection, SipConnectionId::Incoming(_)) {
            self.publish_incoming();
        }
        Ok(())
    }

    fn remove_closed_incoming(&mut self, id: u64) {
        if let Some(slot) = self.incoming.iter_mut().find(|slot| {
            slot.as_ref().is_some_and(|connection| {
                connection.id == id && matches!(connection.stream, SignalStream::Closed)
            })
        }) {
            *slot = None;
        }
    }

    async fn ensure_listener(&mut self) -> io::Result<()> {
        if self.listener.is_some()
            || self.incoming.iter().all(Option::is_some)
            || self.listener_config.is_none()
        {
            return Ok(());
        }
        let config = self
            .listener_config
            .as_ref()
            .expect("listener configuration checked")
            .clone();
        let mut stop = self.stop.clone();
        let listener = tokio::select! {
            biased;
            _ = wait_for_stop(&mut stop) => return Err(session_cancelled()),
            _ = tokio::time::sleep(SESSION_CONTROL_TIMEOUT) => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "IMS incoming listener rearm timed out"));
            }
            result = config.tcp.listen(config.local_port, Some(config.expected_peer)) => result?,
        };
        self.listener = Some(listener);
        self.publish_incoming();
        Ok(())
    }

    async fn select_next(&mut self) -> SessionReady {
        let first_active = self.incoming[0]
            .as_ref()
            .is_some_and(|connection| matches!(connection.stream, SignalStream::Open(_)));
        let second_active = self.incoming[1]
            .as_ref()
            .is_some_and(|connection| matches!(connection.stream, SignalStream::Open(_)));
        let listener_active = self.listener.is_some();
        let Self {
            primary,
            listener,
            incoming,
            stop,
            ..
        } = self;
        let SignalStream::Open(primary) = primary else {
            return SessionReady::Primary(Ok(None));
        };
        let reader = &mut primary.reader;
        let (first, second) = incoming.split_at_mut(1);
        match (first_active, second_active, listener_active) {
            (false, false, false) => tokio::select! {
                _ = wait_for_stop(stop) => SessionReady::Cancelled,
                result = reader.next() => SessionReady::Primary(result),
            },
            (false, false, true) => tokio::select! {
                _ = wait_for_stop(stop) => SessionReady::Cancelled,
                result = reader.next() => SessionReady::Primary(result),
                result = listener.as_mut().expect("listener state checked").accept_cancel_safe() => SessionReady::Accepted(result),
            },
            (true, false, false) => tokio::select! {
                _ = wait_for_stop(stop) => SessionReady::Cancelled,
                result = reader.next() => SessionReady::Primary(result),
                result = incoming_reader(first[0].as_mut().expect("incoming state checked")).next() => SessionReady::Incoming(0, result),
            },
            (true, false, true) => tokio::select! {
                _ = wait_for_stop(stop) => SessionReady::Cancelled,
                result = reader.next() => SessionReady::Primary(result),
                result = incoming_reader(first[0].as_mut().expect("incoming state checked")).next() => SessionReady::Incoming(0, result),
                result = listener.as_mut().expect("listener state checked").accept_cancel_safe() => SessionReady::Accepted(result),
            },
            (false, true, false) => tokio::select! {
                _ = wait_for_stop(stop) => SessionReady::Cancelled,
                result = reader.next() => SessionReady::Primary(result),
                result = incoming_reader(second[0].as_mut().expect("incoming state checked")).next() => SessionReady::Incoming(1, result),
            },
            (false, true, true) => tokio::select! {
                _ = wait_for_stop(stop) => SessionReady::Cancelled,
                result = reader.next() => SessionReady::Primary(result),
                result = incoming_reader(second[0].as_mut().expect("incoming state checked")).next() => SessionReady::Incoming(1, result),
                result = listener.as_mut().expect("listener state checked").accept_cancel_safe() => SessionReady::Accepted(result),
            },
            (true, true, _) => tokio::select! {
                _ = wait_for_stop(stop) => SessionReady::Cancelled,
                result = reader.next() => SessionReady::Primary(result),
                result = incoming_reader(first[0].as_mut().expect("incoming state checked")).next() => SessionReady::Incoming(0, result),
                result = incoming_reader(second[0].as_mut().expect("incoming state checked")).next() => SessionReady::Incoming(1, result),
            },
        }
    }

    async fn close_incoming(&mut self, index: usize) -> io::Result<()> {
        let id = self.incoming[index]
            .as_ref()
            .map(|connection| connection.id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotConnected,
                    "IMS incoming signaling connection is no longer active",
                )
            })?;
        self.begin_connection_close(SipConnectionId::Incoming(id), None)?;
        let result = self.drive_connection(SipConnectionId::Incoming(id)).await;
        self.remove_closed_incoming(id);
        result.map(|_| ())
    }
}

struct UnknownOnSmsSubmitDrop<'a> {
    submission: &'a mut SmsSubmission,
    finished: bool,
}

impl Drop for UnknownOnSmsSubmitDrop<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // The state is AwaitingResponse from the moment the transport
            // accepts the attempt. A dropped future cannot prove that no bytes
            // reached the peer, so preserve uncertainty and forbid replay.
            let _ = self.submission.mark_outcome_unknown();
        }
    }
}

fn incoming_reader(
    connection: &mut IncomingSignalStream,
) -> &mut SipStreamReader<ReadHalf<ManagedTcpStream>> {
    let SignalStream::Open(open) = &mut connection.stream else {
        unreachable!("only open incoming streams participate in selection")
    };
    &mut open.reader
}

async fn close_signal_stream(stream: SignalStream) -> io::Result<()> {
    let mut close = match stream {
        SignalStream::Open(open) => open.reader.into_inner().unsplit(open.writer).begin_close(),
        SignalStream::Closing { close, .. } => close,
        SignalStream::Closed => return Ok(()),
    };
    let result = close.wait_cancel_safe().await;
    // This stage reports physical retirement only. The failed write already
    // returned unknown to its action owner and is never promoted to success.
    if close.removal_confirmed() {
        Ok(())
    } else {
        result
    }
}

fn finish_connection_close(
    connection: SipConnectionId,
    cause: Option<StoredIoError>,
    close_result: io::Result<()>,
) -> io::Result<ConnectionProgress> {
    match (cause, close_result) {
        (None, Ok(())) => Ok(ConnectionProgress::Closed),
        (None, Err(error)) => Err(error),
        (Some(cause), Ok(())) => Err(cause.into_error()),
        (Some(cause), Err(close_error)) => Err(io::Error::new(
            cause.kind,
            format!(
                "{}; exact {connection:?} socket close also failed: {close_error}",
                cause.message
            ),
        )),
    }
}

fn write_deadline_exceeded(connection: SipConnectionId) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("IMS {connection:?} signaling write deadline exceeded"),
    )
}

fn connection_closed(connection: SipConnectionId) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        format!("IMS {connection:?} signaling connection is closed"),
    )
}

async fn wait_for_stop(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow() || stop.changed().await.is_err() {
            return;
        }
    }
}

fn session_cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "IMS registration session cancelled",
    )
}

pub async fn register<B, A, AF, E>(
    services: RegistrationServices<'_>,
    plan: RegistrationPlan<'_>,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    build: B,
    authenticate: A,
) -> Result<RegisteredImsSession, RegistrationFailure<E>>
where
    B: for<'a> FnMut(RegisterAttempt<'a>) -> Result<Zeroizing<Vec<u8>>, SipError>,
    A: FnMut(SipResponse) -> AF,
    AF: Future<Output = Result<AuthenticatedChallenge, E>>,
{
    register_with_builder(
        services,
        plan,
        deadline,
        stop,
        ClosureRequestBuilder(build),
        authenticate,
    )
    .await
}

/// Trait-based entry for stateful request builders that must retain a profile
/// snapshot or construction diagnostics across all REGISTER phases.
pub async fn register_with_builder<B, A, AF, E>(
    services: RegistrationServices<'_>,
    plan: RegistrationPlan<'_>,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    mut build: B,
    mut authenticate: A,
) -> Result<RegisteredImsSession, RegistrationFailure<E>>
where
    B: RegisterRequestBuilder,
    A: FnMut(SipResponse) -> AF,
    AF: Future<Output = Result<AuthenticatedChallenge, E>>,
{
    validate_plan(plan).map_err(|cause| failure(cause, CleanupReport::default()))?;
    check_abort(RegistrationStage::Validate, deadline, &stop)
        .map_err(|cause| failure(cause, CleanupReport::default()))?;

    let mut initial_request = build
        .build(RegisterAttempt::Initial {
            cseq: plan.initial_cseq,
            local_port: plan.plain_local_port,
        })
        .map_err(|source| {
            failure(
                RegistrationError::Sip {
                    stage: RegistrationStage::InitialRegister,
                    source,
                },
                CleanupReport::default(),
            )
        })?;
    let mut final_request_lease = registration_request_lease(&initial_request);

    let mut current_cseq = plan.initial_cseq;
    let mut progress = loop {
        let pending = bounded(
            services.tcp.dial(plan.plain_peer, plan.plain_local_port),
            RegistrationStage::PlainDial,
            deadline,
            stop.clone(),
        )
        .await
        .map_err(|cause| failure(cause, CleanupReport::default()))?
        .map_err(|source| {
            failure(
                RegistrationError::Io {
                    stage: RegistrationStage::PlainDial,
                    source,
                },
                CleanupReport::default(),
            )
        })?;
        let stream = pending
            .connect_until(deadline, stop.clone())
            .await
            .map_err(|source| {
                failure(
                    RegistrationError::Io {
                        stage: RegistrationStage::PlainConnect,
                        source,
                    },
                    CleanupReport::default(),
                )
            })?;
        let connection =
            RegistrationConnection::new(stream, plan.call_id, current_cseq, plan.sec_agree)
                .map(RegistrationConnection::with_retained_events)
                .map_err(|source| {
                    failure(
                        RegistrationError::Sip {
                            stage: RegistrationStage::Validate,
                            source,
                        },
                        CleanupReport::default(),
                    )
                })?;

        let result = connection
            .advance_initial_with_fallback(
                &initial_request,
                deadline,
                stop.clone(),
                services.events,
                |status, cseq| {
                    let request = build.next_initial(
                        &RegisterWaitError::Sip(SipError::RegistrationRejected(status)),
                        cseq,
                        plan.plain_local_port,
                    )?;
                    if let Some(request) = request.as_ref() {
                        current_cseq = cseq;
                        final_request_lease = registration_request_lease(request);
                    }
                    Ok(request)
                },
            )
            .await;
        match result {
            Ok(progress) => break progress,
            Err(failed) => {
                let (source, retired) = failed.into_error_and_retired();
                let cleanup = retire_plain_transport(retired).await;
                if cleanup.is_clean()
                    && !*stop.borrow()
                    && stop.has_changed().is_ok()
                    && Instant::now() < deadline
                {
                    let cseq = current_cseq.checked_add(1).ok_or_else(|| {
                        failure(
                            RegistrationError::Sip {
                                stage: RegistrationStage::InitialRegister,
                                source: SipError::InvalidCSeq,
                            },
                            CleanupReport::default(),
                        )
                    })?;
                    match build.next_initial(&source, cseq, plan.plain_local_port) {
                        Ok(Some(request)) => {
                            initial_request = request;
                            current_cseq = cseq;
                            final_request_lease = registration_request_lease(&initial_request);
                            continue;
                        }
                        Ok(None) => {}
                        Err(source) => {
                            return Err(failure(
                                RegistrationError::Sip {
                                    stage: RegistrationStage::InitialRegister,
                                    source,
                                },
                                cleanup,
                            ));
                        }
                    }
                }
                return Err(failure(
                    registration_transport_error(RegistrationStage::InitialRegister, source),
                    cleanup,
                ));
            }
        }
    };

    loop {
        let challenge = match progress {
            RegistrationProgress::Registered(mut connection, response) => {
                let startup_events = connection.take_retained_events();
                let registration_expires_seconds = final_request_lease
                    .as_ref()
                    .and_then(|request| granted_registration_lease(request, &response));
                let registration_renewal_deadline = registration_expires_seconds
                    .and_then(crate::registration_renewal_delay_seconds)
                    .map(|delay| Instant::now() + Duration::from_secs(delay));
                let (reader, writer) = connection.into_registered_parts().map_err(|source| {
                    failure(
                        RegistrationError::Sip {
                            stage: RegistrationStage::Finalize,
                            source,
                        },
                        CleanupReport::default(),
                    )
                })?;
                return Ok(RegisteredImsSession {
                    refresh_configuration: None,
                    registration_lease_observer: None,
                    register_request: final_request_lease.as_ref().map(|r| r.wire.clone()),
                    registration_expiry: registration_expires_seconds
                        .map(|s| Instant::now() + Duration::from_secs(s)),
                    heartbeat_configuration: None,
                    heartbeat_health: crate::sip_session_runtime::HeartbeatHealth::new(),
                    heartbeat_observer: None,
                    incoming_facts: Default::default(),
                    incoming_observer: None,
                    tcp_owner: Some(services.tcp.clone()),
                    primary: SignalStream::open(reader, writer),
                    listener: None,
                    listener_config: None,
                    incoming: std::array::from_fn(|_| None),
                    next_incoming_id: 1,
                    stop,
                    cancellation_owner: None,
                    lease: None,
                    final_response: response,
                    registration_expires_seconds,
                    registration_renewal_deadline,
                    startup_events,
                    call_context: None,
                    register_contact: final_request_lease
                        .as_ref()
                        .and_then(|request| single_register_contact(&request.contacts)),
                    register_peer: plan.plain_peer,
                    register_port_s: plan.plain_local_port,
                    register_cseq: final_request_lease
                        .as_ref()
                        .map_or(0, |request| request.cseq),
                    message_local_port: plan.plain_local_port,
                    message_local_ip: plan.local_ip,
                    message_security_verify: None,
                });
            }
            RegistrationProgress::Challenge(challenge) => challenge,
        };

        let answer = match bounded(
            authenticate(challenge.response().clone()),
            RegistrationStage::Authenticate,
            deadline,
            stop.clone(),
        )
        .await
        {
            Ok(Ok(answer)) => answer,
            result => {
                let cleanup = retire_plain_transport(challenge.into_retired_transport()).await;
                let cause = match result {
                    Ok(Err(error)) => RegistrationError::Authentication(error),
                    Err(cause) => cause,
                    Ok(Ok(_)) => unreachable!(),
                };
                return Err(failure(cause, cleanup));
            }
        };

        match challenge.resolve(answer) {
            ChallengeResolution::Plain(plain) => {
                progress = match plain
                    .advance_with(
                        deadline,
                        stop.clone(),
                        services.events,
                        |cseq, authorization, security_disposition| {
                            let request = build.build(RegisterAttempt::PlainAuthenticated {
                                cseq,
                                local_port: plan.plain_local_port,
                                authorization,
                                security_disposition,
                            })?;
                            final_request_lease = registration_request_lease(&request);
                            Ok(request)
                        },
                    )
                    .await
                {
                    Ok(progress) => progress,
                    Err(failed) => {
                        let (error, retired) = failed.into_error_and_retired();
                        let cleanup = retire_plain_transport(retired).await;
                        let cause = match error {
                            PlainAdvanceError::Template(source) => RegistrationError::Sip {
                                stage: RegistrationStage::PlainAuthenticatedRegister,
                                source,
                            },
                            PlainAdvanceError::Transport(source) => registration_transport_error(
                                RegistrationStage::PlainAuthenticatedRegister,
                                source,
                            ),
                        };
                        return Err(failure(cause, cleanup));
                    }
                };
            }
            ChallengeResolution::Security(pending) => {
                let pending = match pending
                    .reject_retained_requests(deadline, stop.clone())
                    .await
                {
                    Ok(pending) => pending,
                    Err(failed) => {
                        let (error, retired) = failed.into_error_and_retired();
                        let cleanup = retire_plain_transport(retired).await;
                        let cause = match error {
                            PlainAdvanceError::Transport(source) => {
                                registration_transport_error(RegistrationStage::RetirePlain, source)
                            }
                            PlainAdvanceError::Template(source) => RegistrationError::Sip {
                                stage: RegistrationStage::RetirePlain,
                                source,
                            },
                        };
                        return Err(failure(cause, cleanup));
                    }
                };
                let prepared = match pending.prepare_with_retirement(SecAgreeParameters {
                    local_ip: plan.local_ip,
                    remote_ip: plan.plain_peer.ip(),
                    local_endpoints: plan.local_endpoints,
                    client_mechanisms: plan.client_mechanisms,
                    strict: plan.strict_security,
                }) {
                    Ok(prepared) => prepared,
                    Err((error, retired)) => {
                        let cleanup = retire_plain_transport(retired).await;
                        return Err(failure(
                            RegistrationError::SecurityPreparation(error),
                            cleanup,
                        ));
                    }
                };
                let ready = prepared.retire_plain().await.map_err(|source| {
                    failure(
                        RegistrationError::Io {
                            stage: RegistrationStage::RetirePlain,
                            source,
                        },
                        CleanupReport::default(),
                    )
                })?;
                return register_protected(
                    ready,
                    services.tcp,
                    services.security,
                    plan.local_ip,
                    deadline,
                    stop,
                    services.events,
                    &mut build,
                )
                .await;
            }
        }
    }
}

/// Run the existing type-state coordinator using a carrier-resolved native
/// IMS profile for all three REGISTER variants and SIM AKA for the challenge.
///
/// The caller must already own the matching tunnel's live `TcpControl` and
/// `SecurityControl`; this function does not resolve P-CSCF names, open a
/// reader, create a tunnel, or publish a production readiness state. Keeping
/// this adapter beside the coordinator makes the profile/security mapping a
/// single tested path instead of leaving each future session actor to
/// reconstruct it independently.
pub async fn register_native_profile<A, AFut, E>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device_identity: &NativeImsDeviceIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    authenticate: A,
) -> Result<RegisteredImsSession, NativeProfileRegistrationFailure<E>>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, E>> + Send,
    E: Send,
{
    register_native_profile_inner(
        services,
        profile,
        device_identity,
        None,
        runtime,
        endpoints,
        None,
        None,
        None,
        false,
        1,
        NativeImsSessionMode::VoiceAndSms,
        deadline,
        stop,
        authenticate,
    )
    .await
}

/// Run native REGISTER against an identity already resolved from the exact
/// generation-bound reader lease owned by the enclosing session. A mismatch
/// is rejected before the coordinator can dial the P-CSCF.
pub async fn register_native_profile_with_identity<A, AFut, E>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device_identity: &NativeImsDeviceIdentity,
    subscriber_identity: &NativeImsSubscriberIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    authenticate: A,
) -> Result<RegisteredImsSession, NativeProfileRegistrationFailure<E>>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, E>> + Send,
    E: Send,
{
    register_native_profile_with_identity_and_eap_direct_material(
        services,
        profile,
        device_identity,
        subscriber_identity,
        runtime,
        endpoints,
        None,
        deadline,
        stop,
        authenticate,
    )
    .await
}

/// Native profile registration with optional EAP-direct credentials captured
/// by this same IKE generation. The credentials are borrowed only by the
/// initial REGISTER builder; no second SIM AKA is initiated.
pub async fn register_native_profile_with_identity_and_eap_direct_material<A, AFut, E>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device_identity: &NativeImsDeviceIdentity,
    subscriber_identity: &NativeImsSubscriberIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    eap_direct_material: Option<&EapDirectAuthMaterial>,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    authenticate: A,
) -> Result<RegisteredImsSession, NativeProfileRegistrationFailure<E>>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, E>> + Send,
    E: Send,
{
    register_native_profile_with_identity_eap_direct_and_transport_peer(
        services,
        profile,
        device_identity,
        subscriber_identity,
        runtime,
        endpoints,
        eap_direct_material,
        None,
        deadline,
        stop,
        authenticate,
    )
    .await
}

/// Keep the SIP registrar distinct from the actual tunnel gateway selected by
/// the existing registrar-candidate policy. No DNS or identity lookup occurs.
#[allow(clippy::too_many_arguments)]
pub async fn register_native_profile_with_identity_eap_direct_and_transport_peer<A, AFut, E>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device_identity: &NativeImsDeviceIdentity,
    subscriber_identity: &NativeImsSubscriberIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    eap_direct_material: Option<&EapDirectAuthMaterial>,
    transport_peer: Option<SocketAddr>,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    authenticate: A,
) -> Result<RegisteredImsSession, NativeProfileRegistrationFailure<E>>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, E>> + Send,
    E: Send,
{
    register_native_profile_with_initial_variants(
        services,
        profile,
        device_identity,
        subscriber_identity,
        runtime,
        endpoints,
        eap_direct_material,
        transport_peer,
        None,
        false,
        1,
        deadline,
        stop,
        authenticate,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn register_native_profile_with_initial_variants<A, AFut, E>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device_identity: &NativeImsDeviceIdentity,
    subscriber_identity: &NativeImsSubscriberIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    eap_direct_material: Option<&EapDirectAuthMaterial>,
    transport_peer: Option<SocketAddr>,
    initial_variants: Option<&[proletarius_ims::register_fallback::InitialRegisterVariant]>,
    fallback_environment: bool,
    initial_cseq: u32,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    authenticate: A,
) -> Result<RegisteredImsSession, NativeProfileRegistrationFailure<E>>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, E>> + Send,
    E: Send,
{
    register_native_profile_with_initial_variants_and_session_mode(
        services,
        profile,
        device_identity,
        subscriber_identity,
        runtime,
        endpoints,
        eap_direct_material,
        transport_peer,
        initial_variants,
        fallback_environment,
        initial_cseq,
        NativeImsSessionMode::VoiceAndSms,
        deadline,
        stop,
        authenticate,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn register_native_profile_with_initial_variants_and_session_mode<A, AFut, E>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device_identity: &NativeImsDeviceIdentity,
    subscriber_identity: &NativeImsSubscriberIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    eap_direct_material: Option<&EapDirectAuthMaterial>,
    transport_peer: Option<SocketAddr>,
    initial_variants: Option<&[proletarius_ims::register_fallback::InitialRegisterVariant]>,
    fallback_environment: bool,
    initial_cseq: u32,
    session_mode: NativeImsSessionMode,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    authenticate: A,
) -> Result<RegisteredImsSession, NativeProfileRegistrationFailure<E>>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, E>> + Send,
    E: Send,
{
    register_native_profile_inner(
        services,
        profile,
        device_identity,
        Some(subscriber_identity),
        runtime,
        endpoints,
        eap_direct_material,
        transport_peer,
        initial_variants,
        fallback_environment,
        initial_cseq,
        session_mode,
        deadline,
        stop,
        authenticate,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn register_native_profile_inner<A, AFut, E>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device_identity: &NativeImsDeviceIdentity,
    subscriber_identity: Option<&NativeImsSubscriberIdentity>,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    eap_direct_material: Option<&EapDirectAuthMaterial>,
    transport_peer: Option<SocketAddr>,
    initial_variants: Option<&[proletarius_ims::register_fallback::InitialRegisterVariant]>,
    fallback_environment: bool,
    initial_cseq: u32,
    session_mode: NativeImsSessionMode,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    authenticate: A,
) -> Result<RegisteredImsSession, NativeProfileRegistrationFailure<E>>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, E>> + Send,
    E: Send,
{
    let transport_peer = transport_peer.unwrap_or(runtime.pcscf);
    if transport_peer.ip().is_unspecified()
        || transport_peer.port() == 0
        || transport_peer.is_ipv4() != runtime.local_ip.is_ipv4()
    {
        return Err(NativeProfileRegistrationFailure {
            registration: failure(
                RegistrationError::InvalidPlan("invalid REGISTER transport gateway"),
                CleanupReport::default(),
            ),
            profile_error: None,
        });
    }
    if let Some(subscriber_identity) = subscriber_identity
        && let Err(error) = profile.validate_subscriber_identity(subscriber_identity, runtime.imsi)
    {
        return Err(NativeProfileRegistrationFailure {
            registration: failure::<ExchangeError<E>>(
                RegistrationError::InvalidPlan(
                    "subscriber identity does not match the selected SIM/profile",
                ),
                CleanupReport::default(),
            ),
            profile_error: Some(error),
        });
    }

    let interval = match crate::sip_session_runtime::HeartbeatConfiguration::validate_interval(
        profile.options_ping_interval_seconds(),
    ) {
        Ok(value) => value,
        Err(_) => {
            return Err(NativeProfileRegistrationFailure {
                registration: failure(
                    RegistrationError::InvalidPlan("invalid IMS OPTIONS heartbeat interval"),
                    CleanupReport::default(),
                ),
                profile_error: None,
            });
        }
    };
    let options_policy = profile.options_policy_for_session(session_mode);
    let tcp = match services
        .tcp
        .with_keepalive(Duration::from_secs(profile.tcp_keepalive_seconds()))
    {
        Ok(value) => value,
        Err(_) => {
            return Err(NativeProfileRegistrationFailure {
                registration: failure(
                    RegistrationError::InvalidPlan("invalid IMS TCP keepalive interval"),
                    CleanupReport::default(),
                ),
                profile_error: None,
            });
        }
    };
    let services = RegistrationServices::new(&tcp, services.security, services.events);

    let local_endpoints = LocalEndpoints {
        spi_c: endpoints.spi_c,
        spi_s: endpoints.spi_s,
        port_c: endpoints.port_c,
        port_s: endpoints.port_s,
    };
    let client_mechanisms = profile.template().client_security_mechanisms();
    let plan = RegistrationPlan {
        call_id: runtime.call_id,
        initial_cseq,
        sec_agree: profile.template().sec_agree(),
        plain_peer: transport_peer,
        plain_local_port: profile.local_port(),
        local_ip: runtime.local_ip,
        local_endpoints,
        client_mechanisms: &client_mechanisms,
        strict_security: profile.template().strict_security_server_offer(),
    };

    let request_uri = format!("sip:{}", profile.domain());
    let profile_error = Arc::new(Mutex::new(None));
    let username = match subscriber_identity {
        Some(identity) => Zeroizing::new(identity.private_identity().to_owned()),
        None => match profile.resolve_subscriber_identity(runtime) {
            Ok(identity) => Zeroizing::new(identity.private_identity().to_owned()),
            Err(error) => {
                *profile_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                // The first REGISTER compilation will return the same profile
                // error before any network exchange. This placeholder is never
                // used for a challenge, but lets the coordinator preserve its
                // normal typed cleanup/error path.
                Zeroizing::new(format!("{}@{}", runtime.imsi, profile.realm()))
            }
        },
    };
    // The actor supplies a fresh, unguessable Call-ID for each registration;
    // reusing it as cnonce avoids a second entropy path while remaining
    // transaction-unique.
    let cnonce = runtime.call_id.to_owned();
    let auth_stop = stop.clone();
    let authenticate = Arc::new(AsyncMutex::new(authenticate));
    let mut challenge = move |response: SipResponse| {
        let stop = auth_stop.clone();
        let request_uri = request_uri.clone();
        let username = username.clone();
        let cnonce = cnonce.clone();
        let authenticate = Arc::clone(&authenticate);
        async move {
            let request = AuthorizationRequest {
                username: &username,
                method: "REGISTER",
                uri: &request_uri,
                body: b"",
                cnonce: &cnonce,
                nonce_count: 1,
            };
            answer_challenge(&response, &request, deadline, stop, move |rand, autn| {
                let authenticate = Arc::clone(&authenticate);
                async move {
                    let mut authenticate = authenticate.lock().await;
                    authenticate(rand, autn).await
                }
            })
            .await
        }
    };

    let build = NativeProfileRequestBuilder {
        profile,
        device_identity,
        subscriber_identity,
        runtime,
        endpoints,
        eap_direct_material,
        profile_error: profile_error.clone(),
        initial_variants: initial_variants.unwrap_or(&[]),
        initial_index: 0,
        fallback_environment,
        initial_tokens: None,
    };

    let result = register_with_builder(services, plan, deadline, stop, build, &mut challenge).await;
    if let Ok(mut session) = result {
        // The successful request, negotiated association and frozen subscriber
        // profile are still owned here; never reconstruct these from a DTO.
        let identity = match subscriber_identity {
            Some(identity) => Some(Zeroizing::new(identity.public_identity().to_owned())),
            None => profile
                .resolve_subscriber_identity(runtime)
                .ok()
                .map(|identity| Zeroizing::new(identity.public_identity().to_owned())),
        };
        if let (Some(identity), Some(contact)) = (identity, session.register_contact.as_ref()) {
            let fallback = vec![format!("<sip:{};lr>", runtime.pcscf)];
            if let Ok(routing) = session.message_routing(&identity, &fallback) {
                session.call_context = Some(RegisteredCallContext {
                    carrier_id: profile.carrier_id().to_owned(),
                    domain: profile.domain().to_owned(),
                    realm: profile.realm().to_owned(),
                    phone_context: profile.phone_context().to_owned(),
                    template_id: profile.template().id().to_owned(),
                    encoded_icsi: profile.encoded_call_icsi(),
                    public_identity: identity,
                    user_agent: device_identity.user_agent().to_owned(),
                    pani: profile.authenticated_call_pani(),
                    contact: contact.clone(),
                    routing,
                    local_ip: session.message_local_ip,
                    port_c: session.message_local_port,
                    port_s: session.register_port_s,
                    register_cseq: session.register_cseq,
                    // Route remains the selected P-CSCF address, matching the
                    // established initial-dialog routing (not its IPsec port).
                    peer: runtime.pcscf,
                    security_verify: session.message_security_verify.clone(),
                });
                session.heartbeat_configuration =
                    session
                        .call_context
                        .as_ref()
                        .map(|context| match options_policy {
                            NativeImsOptionsPolicy::Probe { .. } => {
                                RegisteredOptionsConfiguration::Probe(
                                    crate::sip_session_runtime::HeartbeatConfiguration::freeze(
                                        context, interval,
                                    ),
                                )
                            }
                            NativeImsOptionsPolicy::DisabledForSmsOnlyVodafoneUk => {
                                RegisteredOptionsConfiguration::DisabledForSmsOnlyVodafoneUk
                            }
                        });
            }
        }
        return Ok(session);
    }
    result.map_err(|registration| NativeProfileRegistrationFailure {
        registration,
        profile_error: profile_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn register_protected<B, E>(
    ready: proletarius_ims::registration_connection::ReadySecAgree,
    tcp: &TcpControl,
    security: &SecurityControl,
    local_ip: std::net::IpAddr,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    events: &mpsc::Sender<SipStreamItem>,
    build: &mut B,
) -> Result<RegisteredImsSession, RegistrationFailure<E>>
where
    B: RegisterRequestBuilder,
{
    // The command endpoints are intentionally awaited through the same
    // absolute registration deadline and stop signal as the SIP exchange.
    // Dropping an install waiter is safe: a late runtime reply drops the fused
    // proof/lease and queues generation-scoped SA cleanup.
    let installed = match bounded(
        security.install_registration(ready),
        RegistrationStage::InstallSecurity,
        deadline,
        stop.clone(),
    )
    .await
    {
        Ok(Ok(installed)) => installed,
        Ok(Err(error)) => {
            return Err(failure(
                RegistrationError::SecurityInstall(error),
                CleanupReport::default(),
            ));
        }
        Err(cause) => return Err(failure(cause, CleanupReport::default())),
    };

    let context = installed.context().clone();
    let expected_peer = SocketAddr::new(context.remote_ip(), context.remote_port_c());
    // TcpControl closes a socket returned to a dropped reply receiver, so a
    // timed-out/cancelled command cannot leave an unowned listener behind.
    let listener = match bounded(
        tcp.listen(context.local_port_s(), Some(expected_peer)),
        RegistrationStage::ProtectedListen,
        deadline,
        stop.clone(),
    )
    .await
    {
        Ok(Ok(listener)) => listener,
        Ok(Err(source)) => {
            let cleanup = rollback_installed(installed).await;
            return Err(failure(
                RegistrationError::Io {
                    stage: RegistrationStage::ProtectedListen,
                    source,
                },
                cleanup,
            ));
        }
        Err(cause) => {
            let cleanup = rollback_installed(installed).await;
            return Err(failure(cause, cleanup));
        }
    };

    let peer = SocketAddr::new(context.remote_ip(), context.remote_port_s());
    // As with listeners, a late PendingTcpStream reply is precisely reclaimed
    // by TcpSocketRegistry when this bounded waiter is dropped.
    let pending = match bounded(
        tcp.dial(peer, context.local_port_c()),
        RegistrationStage::ProtectedDial,
        deadline,
        stop.clone(),
    )
    .await
    {
        Ok(Ok(pending)) => pending,
        Ok(Err(source)) => {
            let cleanup = rollback_listener_and_installed(listener, installed).await;
            return Err(failure(
                RegistrationError::Io {
                    stage: RegistrationStage::ProtectedDial,
                    source,
                },
                cleanup,
            ));
        }
        Err(cause) => {
            let cleanup = rollback_listener_and_installed(listener, installed).await;
            return Err(failure(cause, cleanup));
        }
    };
    let stream = match pending.connect_until(deadline, stop.clone()).await {
        Ok(stream) => stream,
        Err(source) => {
            let cleanup = rollback_listener_and_installed(listener, installed).await;
            return Err(failure(
                RegistrationError::Io {
                    stage: RegistrationStage::ProtectedConnect,
                    source,
                },
                cleanup,
            ));
        }
    };

    let connection = installed.bind(stream).with_retained_events();
    let cseq = match connection.next_cseq() {
        Ok(cseq) => cseq,
        Err(source) => {
            let cleanup = rollback_connection(connection, listener).await;
            return Err(failure(
                RegistrationError::Sip {
                    stage: RegistrationStage::ProtectedTemplate,
                    source,
                },
                cleanup,
            ));
        }
    };
    let request = match build.build(RegisterAttempt::Protected {
        cseq,
        attempt: connection.request_context(),
    }) {
        Ok(request) => request,
        Err(source) => {
            let cleanup = rollback_connection(connection, listener).await;
            return Err(failure(
                RegistrationError::Sip {
                    stage: RegistrationStage::ProtectedTemplate,
                    source,
                },
                cleanup,
            ));
        }
    };
    let request_lease = registration_request_lease(&request);
    let session_stop = stop.clone();
    let (mut connection, response) = match connection
        .exchange_authenticated(&request, deadline, stop, events)
        .await
    {
        Ok(result) => result,
        Err(failed) => {
            let failed = *failed;
            return match failed.into_error_retired_and_lease() {
                Ok((source, retired, lease)) => {
                    let cleanup = rollback_retired(retired, listener, lease).await;
                    Err(failure(
                        registration_transport_error(RegistrationStage::ProtectedRegister, source),
                        cleanup,
                    ))
                }
                Err(error) => {
                    let (source, lease) = *error;
                    let cleanup = rollback_listener_and_lease(listener, lease).await;
                    Err(failure(
                        RegistrationError::Sip {
                            stage: RegistrationStage::ProtectedRegister,
                            source,
                        },
                        cleanup,
                    ))
                }
            };
        }
    };
    let startup_events = connection.take_retained_events();
    let parts = match connection.into_registered_parts() {
        Ok(parts) => parts,
        Err(error) => {
            let (source, lease) = *error;
            let cleanup = rollback_listener_and_lease(listener, lease).await;
            return Err(failure(
                RegistrationError::Sip {
                    stage: RegistrationStage::Finalize,
                    source,
                },
                cleanup,
            ));
        }
    };
    let registration_expires_seconds = request_lease
        .as_ref()
        .and_then(|request| granted_registration_lease(request, &response));
    let registration_renewal_deadline = registration_expires_seconds
        .and_then(crate::registration_renewal_delay_seconds)
        .map(|delay| Instant::now() + Duration::from_secs(delay));
    Ok(RegisteredImsSession {
        refresh_configuration: None,
        registration_lease_observer: None,
        register_request: request_lease.as_ref().map(|r| r.wire.clone()),
        registration_expiry: registration_expires_seconds
            .map(|s| Instant::now() + Duration::from_secs(s)),
        heartbeat_configuration: None,
        heartbeat_health: crate::sip_session_runtime::HeartbeatHealth::new(),
        heartbeat_observer: None,
        incoming_facts: Default::default(),
        incoming_observer: None,
        tcp_owner: Some(tcp.clone()),
        primary: SignalStream::open(parts.reader, parts.writer),
        listener: Some(listener),
        listener_config: Some(IncomingListenerConfig {
            tcp: tcp.clone(),
            local_port: context.local_port_s(),
            expected_peer,
        }),
        incoming: std::array::from_fn(|_| None),
        next_incoming_id: 1,
        stop: session_stop,
        cancellation_owner: None,
        lease: Some(parts.lease),
        final_response: response,
        registration_expires_seconds,
        registration_renewal_deadline,
        startup_events,
        call_context: None,
        register_contact: request_lease
            .as_ref()
            .and_then(|request| single_register_contact(&request.contacts)),
        register_peer: peer,
        register_port_s: context.local_port_s(),
        register_cseq: cseq,
        message_local_port: context.local_port_c(),
        message_local_ip: local_ip,
        message_security_verify: Some(zeroize::Zeroizing::new(
            context.security_verify().to_owned(),
        )),
    })
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RegistrationRequestLease {
    wire: Zeroizing<Vec<u8>>,
    cseq: u32,
    contacts: Vec<String>,
    requested_seconds: i64,
}

fn single_register_contact(contacts: &[String]) -> Option<Zeroizing<String>> {
    let [contact] = contacts else {
        return None;
    };
    if contact.is_empty()
        || contact.len() > 4096
        || contact.bytes().any(|byte| byte.is_ascii_control())
    {
        return None;
    }
    Some(Zeroizing::new(contact.clone()))
}

pub(crate) fn registration_request_lease(request: &[u8]) -> Option<RegistrationRequestLease> {
    let wire = Zeroizing::new(request.to_vec());
    let (message, consumed) = WireParser::default().parse(request, false).ok()?;
    if consumed != request.len() {
        return None;
    }
    let WireMessage::Request(request) = message else {
        return None;
    };
    if request.method.as_str() != "REGISTER" {
        return None;
    }
    let contacts = request
        .data
        .headers
        .all("Contact")
        .map(|header| header.value().to_owned())
        .collect::<Vec<_>>();
    let requested_seconds = request
        .data
        .headers
        .get("Expires")?
        .value()
        .parse::<i64>()
        .ok()?;
    if contacts.is_empty() || requested_seconds <= 0 {
        return None;
    }
    let cseq = request
        .data
        .headers
        .get("CSeq")
        .and_then(|header| header.value().split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Some(RegistrationRequestLease {
        wire,
        cseq,
        contacts,
        requested_seconds,
    })
}

pub(crate) fn granted_registration_lease(
    request: &RegistrationRequestLease,
    response: &SipResponse,
) -> Option<u64> {
    if response.status != 200 {
        return None;
    }
    let contacts = response
        .headers("Contact")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let global_expires = response
        .headers("Expires")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    evaluate_register_lease_headers(
        &request.contacts,
        &contacts,
        &global_expires,
        request.requested_seconds,
    )
    .ok()
    .and_then(|seconds| u64::try_from(seconds).ok())
}

fn validate_plan<E>(plan: RegistrationPlan<'_>) -> Result<(), RegistrationError<E>> {
    if plan.call_id.trim().is_empty() {
        return Err(RegistrationError::InvalidPlan("empty Call-ID"));
    }
    if plan.initial_cseq == 0 {
        return Err(RegistrationError::InvalidPlan("zero initial CSeq"));
    }
    if plan.plain_local_port == 0 || plan.plain_peer.port() == 0 {
        return Err(RegistrationError::InvalidPlan("zero plain TCP port"));
    }
    if plan.local_ip.is_unspecified()
        || plan.local_ip.is_multicast()
        || plan.plain_peer.ip().is_unspecified()
        || plan.plain_peer.ip().is_multicast()
        || plan.local_ip.is_ipv4() != plan.plain_peer.ip().is_ipv4()
    {
        return Err(RegistrationError::InvalidPlan(
            "invalid or mismatched IMS address family",
        ));
    }
    Ok(())
}

fn check_abort<E>(
    stage: RegistrationStage,
    deadline: Instant,
    stop: &watch::Receiver<bool>,
) -> Result<(), RegistrationError<E>> {
    if *stop.borrow() || stop.has_changed().is_err() {
        return Err(RegistrationError::Cancelled(stage));
    }
    if Instant::now() >= deadline {
        return Err(RegistrationError::Deadline(stage));
    }
    Ok(())
}

async fn bounded<F, T, E>(
    future: F,
    stage: RegistrationStage,
    deadline: Instant,
    mut stop: watch::Receiver<bool>,
) -> Result<T, RegistrationError<E>>
where
    F: Future<Output = T>,
{
    check_abort(stage, deadline, &stop)?;
    tokio::pin!(future);
    loop {
        tokio::select! {
            biased;
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return Err(RegistrationError::Cancelled(stage));
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(RegistrationError::Deadline(stage));
            }
            result = &mut future => return Ok(result),
        }
    }
}

fn registration_transport_error<E>(
    stage: RegistrationStage,
    source: RegisterWaitError,
) -> RegistrationError<E> {
    match source {
        RegisterWaitError::Cancelled => RegistrationError::Cancelled(stage),
        RegisterWaitError::Deadline => RegistrationError::Deadline(stage),
        source => RegistrationError::Transport { stage, source },
    }
}

fn failure<E>(cause: RegistrationError<E>, cleanup: CleanupReport) -> RegistrationFailure<E> {
    RegistrationFailure { cause, cleanup }
}

async fn retire_plain_transport(
    retired: proletarius_ims::registration_connection::RetiredRegistrationTransport<
        ManagedTcpStream,
    >,
) -> CleanupReport {
    let mut cleanup = CleanupReport::default();
    if let Err(error) = retired.retire().await {
        cleanup.push(CleanupStage::PrimarySocket, error);
    }
    cleanup
}

async fn rollback_installed(installed: InstalledSecAgree) -> CleanupReport {
    let mut cleanup = CleanupReport::default();
    if let Err(error) = installed.rollback().await {
        cleanup.push(CleanupStage::SecurityAssociation, error);
    }
    cleanup
}

async fn rollback_listener_and_installed(
    listener: PendingTcpListener,
    installed: InstalledSecAgree,
) -> CleanupReport {
    let mut cleanup = CleanupReport::default();
    if let Err(error) = listener.close().await {
        cleanup.push(CleanupStage::IncomingListener, error);
    }
    if let Err(error) = installed.rollback().await {
        cleanup.push(CleanupStage::SecurityAssociation, error);
    }
    cleanup
}

async fn rollback_connection(
    connection: LeasedRegistrationConnection<ManagedTcpStream>,
    listener: PendingTcpListener,
) -> CleanupReport {
    match connection.into_retired_and_lease() {
        Ok((retired, lease)) => rollback_retired(retired, listener, lease).await,
        Err(error) => {
            let (source, lease) = *error;
            let mut cleanup = rollback_listener_and_lease(listener, lease).await;
            cleanup.push(CleanupStage::Invariant, source);
            cleanup
        }
    }
}

async fn rollback_retired(
    retired: proletarius_ims::registration_connection::RetiredRegistrationTransport<
        ManagedTcpStream,
    >,
    listener: PendingTcpListener,
    lease: SecurityLease,
) -> CleanupReport {
    let mut cleanup = CleanupReport::default();
    if let Err(error) = retired.retire().await {
        cleanup.push(CleanupStage::PrimarySocket, error);
    }
    if let Err(error) = listener.close().await {
        cleanup.push(CleanupStage::IncomingListener, error);
    }
    if let Err(error) = lease.clear().await {
        cleanup.push(CleanupStage::SecurityAssociation, error);
    }
    cleanup
}

async fn rollback_listener_and_lease(
    listener: PendingTcpListener,
    lease: SecurityLease,
) -> CleanupReport {
    let mut cleanup = CleanupReport::default();
    if let Err(error) = listener.close().await {
        cleanup.push(CleanupStage::IncomingListener, error);
    }
    if let Err(error) = lease.clear().await {
        cleanup.push(CleanupStage::SecurityAssociation, error);
    }
    cleanup
}

#[cfg(test)]
mod tests {
    mod heartbeat;
    mod security_suites;
    use super::*;
    use crate::{
        TunnelConfiguration, netstack_runtime::TunnelNetwork, netstack_tcp::TcpSocketRegistry,
        security_runtime::SecurityNetworkRuntime,
    };
    use proletarius_ims::{
        aka_exchange::{SecurityKeys, SimAnswer},
        security_offer::parse_security_server,
        security_policy::SecurityPolicy,
    };
    use smoltcp::iface::SocketSet;
    use std::{convert::Infallible, time::Duration};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn registered_session(
        stream: ManagedTcpStream,
        stop: watch::Receiver<bool>,
    ) -> RegisteredImsSession {
        let (reader, writer) = tokio::io::split(stream);
        RegisteredImsSession {
            refresh_configuration:None,registration_lease_observer:None,register_request:None,registration_expiry:None,
            heartbeat_configuration:None,heartbeat_health:crate::sip_session_runtime::HeartbeatHealth::new(),heartbeat_observer:None,
            incoming_facts: Default::default(),
            incoming_observer: None,
            tcp_owner: None,
            primary: SignalStream::open(SipStreamReader::new(reader), writer),
            listener: None,
            listener_config: None,
            incoming: std::array::from_fn(|_| None),
            next_incoming_id: 1,
            stop,
            cancellation_owner: None,
            lease: None,
            registration_expires_seconds: None,
            registration_renewal_deadline: None,
            startup_events: Default::default(),
            call_context: None,
            register_contact: None,
            register_peer: "127.0.0.1:5060".parse().unwrap(),
            register_port_s: 5060,
            register_cseq: 1,
            message_local_port: 5064,
            message_local_ip: "10.0.0.2".parse().unwrap(),
            message_security_verify: None,
            final_response: SipResponse::parse(
                b"SIP/2.0 200 OK\r\nCall-ID: registered\r\nCSeq: 2 REGISTER\r\nContent-Length: 0\r\n\r\n",
            )
            .unwrap(),
        }
    }

    #[test]
    fn registered_lease_uses_successful_contact_binding_and_granted_expiry() {
        let request = registration_request_lease(
            b"REGISTER sip:ims.example SIP/2.0\r\nContact: <sip:user@10.0.0.2:5064>;expires=600\r\nExpires: 600\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
        let response = SipResponse::parse(
            b"SIP/2.0 200 OK\r\nContact: <sip:other@10.0.0.2:5064>;expires=20\r\nContact: <sip:user@10.0.0.2:5064>;expires=1800\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();

        assert_eq!(granted_registration_lease(&request, &response), Some(1800));
        assert_eq!(crate::registration_renewal_delay_seconds(1800), Some(1500));
    }

    #[test]
    fn registered_lease_falls_back_to_global_expiry_only_for_own_binding() {
        let request = registration_request_lease(
            b"REGISTER sip:ims.example SIP/2.0\r\nContact: <sip:user@10.0.0.2:5064>\r\nExpires: 600\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
        let response = SipResponse::parse(
            b"SIP/2.0 200 OK\r\nContact: <sip:user@10.0.0.2:5064>\r\nExpires: 1200\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(granted_registration_lease(&request, &response), Some(1200));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn three_hk_successful_eap_direct_register_freezes_real_contact_and_routing() {
        let configuration = |address: &str| TunnelConfiguration {
            ipv4_addresses: vec![address.parse().unwrap()],
            ..Default::default()
        };
        let local = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let remote = TunnelNetwork::new(&configuration("10.20.30.9")).unwrap();
        let (lw_tx, lw) = watch::channel(0_u64);
        let (rw_tx, rw) = watch::channel(0_u64);
        let (ltcp, mut lr) = TcpSocketRegistry::new(lw_tx.clone());
        let (rtcp, rr) = TcpSocketRegistry::new(rw_tx);
        let (security, runtime) = SecurityNetworkRuntime::new(
            move |interface: &mut smoltcp::iface::Interface, sockets: &mut SocketSet<'static>| {
                lr.service(interface, sockets);
                Ok::<_, Infallible>(())
            },
            lw_tx,
        );
        let (lo, ri) = mpsc::channel(64);
        let (ro, li) = mpsc::channel(64);
        let (stop_tx, stop) = watch::channel(false);
        let local_runner =
            local.run_managed(SocketSet::new(vec![]), li, lo, stop.clone(), lw, runtime);
        let remote_runner = remote.run_managed(SocketSet::new(vec![]), ri, ro, stop, rw, rr);
        let (ready_tx, ready) = tokio::sync::oneshot::channel();
        let server = async {
            let listener = rtcp
                .listen(5060, Some("10.0.0.2:5060".parse().unwrap()))
                .await
                .unwrap();
            ready_tx.send(()).unwrap();
            let stream = listener.accept().await.unwrap();
            let (read, mut write) = tokio::io::split(stream);
            let mut reader = SipStreamReader::new(read);
            let Some(SipStreamItem::Message(wire)) = reader.next().await.unwrap() else {
                panic!("missing 3HK REGISTER")
            };
            let (WireMessage::Request(register), consumed) =
                WireParser::default().parse(&wire, false).unwrap()
            else {
                panic!("not REGISTER")
            };
            assert_eq!(consumed, wire.len());
            let expected_authorization =
                proletarius_ims::aka_authorization::build_eap_direct_initial_authorization(
                    "subscriber@ims.example",
                    "ims.mnc003.mcc454.3gppnetwork.org",
                    "ims.mnc003.mcc454.3gppnetwork.org",
                    &[0x44; 8],
                    &[0x11; 16],
                    &[0x22; 16],
                )
                .unwrap();
            assert_eq!(
                register.data.headers.get("Authorization").unwrap().value(),
                expected_authorization.value()
            );
            let contact = register.data.headers.get("Contact").unwrap().value();
            assert!(contact.contains("10.0.0.2:5060"));
            let response = format!(
                "SIP/2.0 200 OK\r\nCall-ID: three-hk-call-config\r\nCSeq: 1 REGISTER\r\nContact: {contact}\r\nExpires: 7200\r\nP-Associated-URI: <tel:+85291234567>\r\nService-Route: <sip:first@ims.example;lr>\r\nService-Route: <sip:second@ims.example;lr>\r\nContent-Length: 0\r\n\r\n"
            );
            write.write_all(response.as_bytes()).await.unwrap();
            write.flush().await.unwrap();
            assert!(reader.next().await.unwrap().is_none());
            reader.into_inner().unsplit(write).close().await.unwrap();
        };
        let client = async {
            ready.await.unwrap();
            let sim = crate::carrier::SimIdentity {
                mcc: "454".into(),
                mnc: "03".into(),
                imsi: "454030000000001".into(),
                ..Default::default()
            };
            let carrier = crate::carrier::Registry::embedded()
                .unwrap()
                .resolve(&sim)
                .unwrap()
                .unwrap();
            let overrides = crate::carrier_overrides::OverrideSet::parse(
                b"carrier_overrides:\n  '454/03': {ims_identity_source: isim}\n",
            )
            .unwrap();
            let profile = carrier
                .native_ims_register_profile(&sim, overrides.get("454", "03"))
                .unwrap();
            let device = profile
                .resolve_device_identity(Some("353936932685355"))
                .unwrap();
            let input = NativeImsRuntime {
                imsi: &sim.imsi,
                isim_private_identity: Some("subscriber@ims.example"),
                isim_public_identity: Some("sip:configured@ims.example"),
                local_ip: "10.0.0.2".parse().unwrap(),
                pcscf: "10.20.30.9:5060".parse().unwrap(),
                call_id: "three-hk-call-config",
                from_tag: "three-hk",
                via_branch: "z9hG4bK-three-hk",
                cell_id: None,
                mcc: &sim.mcc,
                mnc: &sim.mnc,
            };
            let identity = profile.resolve_subscriber_identity(input).unwrap();
            let material =
                EapDirectAuthMaterial::new(&[0x44; 8], &[0x11; 16], &[0x22; 16]).unwrap();
            let (_session_stop_tx, session_stop) = watch::channel(false);
            let (events, _output) = mpsc::channel(8);
            let session = register_native_profile_with_identity_and_eap_direct_material(
                RegistrationServices::new(&ltcp, &security, &events),
                &profile,
                &device,
                &identity,
                input,
                NativeSecurityEndpoints {
                    spi_c: 333,
                    spi_s: 444,
                    port_c: 5064,
                    port_s: 5063,
                },
                Some(&material),
                Instant::now() + Duration::from_secs(4),
                session_stop,
                |_, _| async { Err::<SimAnswer, _>("unexpected challenge") },
            )
            .await
            .unwrap_or_else(|_| panic!("3HK actual REGISTER failed"));
            let calls = session.call_context().unwrap();
            assert_eq!(calls.template_id(), "3_hk");
            assert_eq!(calls.port_c(), 5060);
            assert_eq!(calls.port_s(), 5060);
            assert_eq!(calls.register_cseq(), 1);
            assert_eq!(calls.public_identity(), "sip:configured@ims.example");
            assert_eq!(calls.routing().registered_msisdn(), Some("+85291234567"));
            assert_eq!(
                calls.routing().routes(),
                ["<sip:first@ims.example;lr>", "<sip:second@ims.example;lr>"]
            );
            assert!(calls.contact().contains("10.0.0.2:5060"));
            assert!(calls.security_verify().is_none());
            assert!(calls.pani().is_empty());
            session.shutdown().await.unwrap();
            stop_tx.send(true).unwrap();
        };
        tokio::time::timeout(Duration::from_secs(6), async {
            let (local, remote, (), ()) = tokio::join!(local_runner, remote_runner, server, client);
            local.unwrap();
            remote.unwrap();
        })
        .await
        .expect("3HK REGISTER timed out");
    }

    #[test]
    fn registered_lease_fails_closed_for_mismatched_or_revoked_contact() {
        let request = registration_request_lease(
            b"REGISTER sip:ims.example SIP/2.0\r\nContact: <sip:user@10.0.0.2:5064>\r\nExpires: 600\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
        let mismatched = SipResponse::parse(
            b"SIP/2.0 200 OK\r\nContact: <sip:other@10.0.0.2:5064>;expires=1200\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
        let revoked = SipResponse::parse(
            b"SIP/2.0 200 OK\r\nContact: <sip:user@10.0.0.2:5064>;expires=0\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();

        assert_eq!(granted_registration_lease(&request, &mismatched), None);
        assert_eq!(granted_registration_lease(&request, &revoked), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tracked_response_timeout_preserves_partial_frame_and_next_request() {
        use crate::sip_session_runtime::{RegisteredSipEvent, RegisteredSipRuntime};
        use proletarius_sip::{Message, Parser};
        let configuration = |address: &str| TunnelConfiguration {
            ipv4_addresses: vec![address.parse().unwrap()],
            ..Default::default()
        };
        let local_network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let remote_network = TunnelNetwork::new(&configuration("10.20.30.9")).unwrap();
        let (local_wake_tx, local_wake) = watch::channel(0_u64);
        let (remote_wake_tx, remote_wake) = watch::channel(0_u64);
        let (local_tcp, local_registry) = TcpSocketRegistry::new(local_wake_tx);
        let (remote_tcp, remote_registry) = TcpSocketRegistry::new(remote_wake_tx);
        let (local_out, remote_in) = mpsc::channel(64);
        let (remote_out, local_in) = mpsc::channel(64);
        let (stop_tx, stop) = watch::channel(false);
        let local_runner = local_network.run_managed(
            SocketSet::new(vec![]),
            local_in,
            local_out,
            stop.clone(),
            local_wake,
            local_registry,
        );
        let remote_runner = remote_network.run_managed(
            SocketSet::new(vec![]),
            remote_in,
            remote_out,
            stop,
            remote_wake,
            remote_registry,
        );
        let application = async {
            let listener = remote_tcp
                .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
                .await
                .unwrap();
            let dial = local_tcp
                .dial("10.20.30.9:6060".parse().unwrap(), 5064)
                .await
                .unwrap();
            let (local, remote) = tokio::join!(dial.connect(), listener.accept());
            let (_session_stop, session_stop) = watch::channel(false);
            let mut session = registered_session(local.unwrap(), session_stop);
            session.registration_renewal_deadline = Some(Instant::now());
            let mut runtime = RegisteredSipRuntime::new(session);
            assert!(matches!(
                runtime.next_event().await.unwrap(),
                Some(RegisteredSipEvent::RegistrationRenewalDue)
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(5), runtime.next_event())
                    .await
                    .is_err()
            );
            let (read, mut write) = tokio::io::split(remote.unwrap());
            let mut reader = SipStreamReader::new(read);
            let (expired_tx, expired_rx) = tokio::sync::oneshot::channel();
            let (partial_tx, partial_rx) = tokio::sync::oneshot::channel();
            let headers = |number: u32| {
                format!(
                    "Via: SIP/2.0/TCP host:5064;branch=z9hG4bKtest{number}\r\nFrom: <sip:a@ims.example>;tag=a\r\nTo: <sip:b@ims.example>\r\nCall-ID: test-{number}\r\nCSeq: {number} MESSAGE\r\nContent-Length: 0\r\n\r\n"
                )
            };
            let client = async {
                let request = |number| {
                    let wire = format!("MESSAGE sip:ims.example SIP/2.0\r\n{}", headers(number));
                    let (Message::Request(request), _) =
                        Parser::default().parse(wire.as_bytes(), false).unwrap()
                    else {
                        panic!("request fixture")
                    };
                    request
                };
                use crate::sms_transaction::{SmsResponseDisposition, SmsSubmission};
                let mut submission = SmsSubmission::new(1).unwrap();
                submission.mark_submitted("test-1", 1).unwrap();
                let first = runtime
                    .send_tracked_request(request(1), Duration::from_millis(150))
                    .await
                    .unwrap();
                let mut report = crate::sms_transaction::DeliveryReportSubmission::new(first);
                partial_rx.await.unwrap();
                let expiry = runtime.next_event().await.unwrap().unwrap();
                assert!(
                    matches!(&expiry, RegisteredSipEvent::ResponseTimedOut { ticket } if *ticket == first)
                );
                assert!(report.observe(&expiry));
                assert_eq!(
                    report.state(),
                    crate::sms_transaction::DeliveryReportState::OutcomeUnknown
                );
                submission.mark_outcome_unknown().unwrap();
                expired_tx.send(()).unwrap();
                let late = runtime.next_event().await.unwrap().unwrap();
                assert!(matches!(
                    &late,
                    RegisteredSipEvent::Message {
                        response_ticket: None,
                        ..
                    }
                ));
                assert!(!report.observe(&late));
                assert_eq!(
                    late.observe_sms_response(&mut submission).unwrap(),
                    SmsResponseDisposition::LateResultResolved {
                        accepted: true,
                        accepted_parts: 1
                    }
                );
                assert_eq!(
                    late.observe_sms_response(&mut submission).unwrap(),
                    SmsResponseDisposition::Ignored
                );
                let second = runtime
                    .send_tracked_request(request(2), Duration::from_secs(2))
                    .await
                    .unwrap();
                let mut second_report =
                    crate::sms_transaction::DeliveryReportSubmission::new(second);
                let final_response = runtime.next_event().await.unwrap().unwrap();
                assert!(
                    matches!(&final_response, RegisteredSipEvent::Message { response_ticket: Some(ticket), .. } if *ticket == second)
                );
                assert!(second_report.observe(&final_response));
                assert_eq!(
                    second_report.state(),
                    crate::sms_transaction::DeliveryReportState::Accepted
                );
                runtime.shutdown().await.unwrap();
            };
            let server = async {
                assert!(matches!(
                    reader.next().await.unwrap(),
                    Some(SipStreamItem::Message(_))
                ));
                write.write_all(b"SIP/2.0 200").await.unwrap();
                write.flush().await.unwrap();
                partial_tx.send(()).unwrap();
                expired_rx.await.unwrap();
                write
                    .write_all(format!(" OK\r\n{}", headers(1)).as_bytes())
                    .await
                    .unwrap();
                write.flush().await.unwrap();
                let Some(SipStreamItem::Message(second)) = reader.next().await.unwrap() else {
                    panic!("missing second request")
                };
                assert!(
                    String::from_utf8_lossy(&second).contains("CSeq: 2 MESSAGE"),
                    "timed-out request was resent"
                );
                write
                    .write_all(format!("SIP/2.0 200 OK\r\n{}", headers(2)).as_bytes())
                    .await
                    .unwrap();
                write.flush().await.unwrap();
                assert!(reader.next().await.unwrap().is_none());
                reader.into_inner().unsplit(write).close().await.unwrap();
            };
            tokio::join!(client, server);
            stop_tx.send(true).unwrap();
        };
        let (local, remote, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(local_runner, remote_runner, application)
        })
        .await
        .expect("tracked response integration timed out");
        local.unwrap();
        remote.unwrap();
    }

    #[test]
    fn dropped_in_flight_sms_write_guard_freezes_the_batch_as_unknown() {
        let mut submission = SmsSubmission::new(1).unwrap();
        submission.mark_submitted("cancelled-sms", 1).unwrap();
        {
            let guard = UnknownOnSmsSubmitDrop {
                submission: &mut submission,
                finished: false,
            };
            drop(guard);
        }
        assert!(matches!(
            submission.status(),
            crate::sms_transaction::SmsSubmissionStatus::OutcomeUnknown { .. }
        ));
        assert!(submission.mark_submitted("must-not-replay", 2).is_err());
    }

    fn full_gate_client_policy_for(algorithm: &str, encryption: &str) -> SecurityPolicy {
        let remote = parse_security_server(
            &format!("ipsec-3gpp;q=0.98;alg={algorithm};ealg={encryption};prot=esp;mod=trans;spi-c=111;spi-s=222;port-c=6050;port-s=6060"),
        )
        .unwrap()
        .remove(0);
        SecurityPolicy::negotiated(
            "10.0.0.2".parse().unwrap(),
            "10.20.30.9".parse().unwrap(),
            LocalEndpoints {
                spi_c: 333,
                spi_s: 444,
                port_c: 5064,
                port_s: 5063,
            },
            &remote,
            SecurityKeys {
                ck: Zeroizing::new([2; 16]),
                ik: Zeroizing::new([3; 16]),
            },
        )
        .unwrap()
    }

    fn full_gate_server_policy() -> SecurityPolicy {
        full_gate_server_policy_for("hmac-sha-1-96", "aes-cbc")
    }

    fn full_gate_server_policy_for(algorithm: &str, encryption: &str) -> SecurityPolicy {
        let remote = parse_security_server(
            &format!("ipsec-3gpp;q=0.98;alg={algorithm};ealg={encryption};prot=esp;mod=trans;spi-c=333;spi-s=444;port-c=5064;port-s=5063"),
        )
        .unwrap()
        .remove(0);
        SecurityPolicy::negotiated(
            "10.20.30.9".parse().unwrap(),
            "10.0.0.2".parse().unwrap(),
            LocalEndpoints {
                spi_c: 111,
                spi_s: 222,
                port_c: 6050,
                port_s: 6060,
            },
            &remote,
            SecurityKeys {
                ck: Zeroizing::new([2; 16]),
                ik: Zeroizing::new([3; 16]),
            },
        )
        .unwrap()
    }

    #[test]
    fn invalid_plan_fails_before_network_or_security_work() {
        let plan = RegistrationPlan {
            call_id: " ",
            initial_cseq: 1,
            sec_agree: SecAgreeMode::Required,
            plain_peer: "10.0.0.9:5060".parse().unwrap(),
            plain_local_port: 5060,
            local_ip: "10.0.0.2".parse().unwrap(),
            local_endpoints: LocalEndpoints {
                spi_c: 1,
                spi_s: 2,
                port_c: 5062,
                port_s: 5063,
            },
            client_mechanisms: &[],
            strict_security: true,
        };
        assert!(matches!(
            validate_plan::<()>(plan),
            Err(RegistrationError::InvalidPlan("empty Call-ID"))
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_plain_register_retires_exact_socket_before_same_tuple_retry() {
        let configuration = |address: &str| TunnelConfiguration {
            ipv4_addresses: vec![address.parse().unwrap()],
            ..Default::default()
        };
        let local_network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let remote_network = TunnelNetwork::new(&configuration("10.20.30.9")).unwrap();
        let (local_wake_sender, local_wake) = watch::channel(0_u64);
        let (remote_wake_sender, remote_wake) = watch::channel(0_u64);
        let (local_tcp, local_registry) = TcpSocketRegistry::new(local_wake_sender.clone());
        let (remote_tcp, remote_registry) = TcpSocketRegistry::new(remote_wake_sender);
        let (security, _unserviced_security) = SecurityNetworkRuntime::new(
            |_: &mut smoltcp::iface::Interface, _: &mut SocketSet<'static>| Ok::<(), Infallible>(()),
            local_wake_sender,
        );
        let (local_to_remote, remote_inbound) = mpsc::channel(64);
        let (remote_to_local, local_inbound) = mpsc::channel(64);
        let (network_stop_sender, network_stop) = watch::channel(false);

        let local_runner = local_network.run_managed(
            SocketSet::new(vec![]),
            local_inbound,
            local_to_remote,
            network_stop.clone(),
            local_wake,
            local_registry,
        );
        let remote_runner = remote_network.run_managed(
            SocketSet::new(vec![]),
            remote_inbound,
            remote_to_local,
            network_stop,
            remote_wake,
            remote_registry,
        );
        let application = async {
            let request = b"REGISTER sip:ims.example SIP/2.0\r\nCall-ID: retry\r\nCSeq: 1 REGISTER\r\nContent-Length: 0\r\n\r\n";
            let (_session_stop_sender, session_stop) = watch::channel(false);
            let (events, _events_rx) = mpsc::channel(4);
            let plan = RegistrationPlan {
                call_id: "retry",
                initial_cseq: 1,
                sec_agree: SecAgreeMode::Disabled,
                plain_peer: "10.20.30.9:6060".parse().unwrap(),
                plain_local_port: 5060,
                local_ip: "10.0.0.2".parse().unwrap(),
                local_endpoints: LocalEndpoints {
                    spi_c: 1,
                    spi_s: 2,
                    port_c: 5062,
                    port_s: 5063,
                },
                client_mechanisms: &[],
                strict_security: false,
            };
            let remote_first = async {
                let listener = remote_tcp
                    .listen(6060, Some("10.0.0.2:5060".parse().unwrap()))
                    .await
                    .unwrap();
                let mut stream = listener.accept().await.unwrap();
                let mut received = vec![0; request.len()];
                stream.read_exact(&mut received).await.unwrap();
                assert_eq!(received, request);
                // EOF without a SIP response exercises the owned initial
                // REGISTER failure path rather than a pre-connect failure.
                stream.close().await.unwrap();
            };
            let local_first = register(
                RegistrationServices::new(&local_tcp, &security, &events),
                plan,
                Instant::now() + Duration::from_secs(2),
                session_stop,
                |attempt| match attempt {
                    RegisterAttempt::Initial { .. } => Ok(Zeroizing::new(request.to_vec())),
                    _ => panic!("plain failure test unexpectedly advanced registration"),
                },
                |_| async { Err::<AuthenticatedChallenge, ()>(()) },
            );
            let (result, ()) = tokio::join!(local_first, remote_first);
            let failure = match result {
                Ok(_) => panic!("EOF unexpectedly completed initial REGISTER"),
                Err(failure) => failure,
            };
            assert!(matches!(
                failure.cause(),
                RegistrationError::Transport {
                    stage: RegistrationStage::InitialRegister,
                    ..
                }
            ));
            assert!(
                failure.cleanup().is_clean(),
                "exact plain retirement failed: {:?}",
                failure.cleanup().issues()
            );

            // The coordinator has returned only after the exact local socket
            // entry was removed, so immediate reuse cannot observe AddrInUse.
            let retry_listener = remote_tcp
                .listen(6060, Some("10.0.0.2:5060".parse().unwrap()))
                .await
                .unwrap();
            let retry_dial = local_tcp
                .dial("10.20.30.9:6060".parse().unwrap(), 5060)
                .await
                .expect("failed REGISTER left its four-tuple active");
            let (local_retry, remote_retry) =
                tokio::join!(retry_dial.connect(), retry_listener.accept());
            local_retry.unwrap().close().await.unwrap();
            remote_retry.unwrap().close().await.unwrap();
            network_stop_sender.send(true).unwrap();
        };

        let (local, remote, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(local_runner, remote_runner, application)
        })
        .await
        .expect("plain REGISTER retirement test timed out");
        local.unwrap();
        remote.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_sec_agree_register_preserves_coalesced_invite_and_reuses_ports_after_shutdown() {
        exercise_selected_security_suite("hmac-sha-1-96", "aes-cbc").await;
    }

    async fn exercise_selected_security_suite(algorithm: &str, encryption: &str) {
        let server_offer = format!(
            "ipsec-3gpp;q=0.98;alg={algorithm};ealg={encryption};prot=esp;mod=trans;spi-c=111;spi-s=222;port-c=6050;port-s=6060"
        );
        const INVITE: &[u8] = b"INVITE sip:user@ims.example SIP/2.0\r\nCall-ID: incoming-full-gate\r\nCSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";
        const EARLY: &[u8] = b"MESSAGE sip:user@ims.example SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:6050;branch=z9hG4bK-early\r\nFrom: <sip:sender@ims.example>;tag=early\r\nTo: <sip:user@ims.example>\r\nCall-ID: protected-early\r\nCSeq: 7 MESSAGE\r\nContent-Length: 0\r\n\r\n";
        const EARLY_OK: &[u8] = b"SIP/2.0 200 OK\r\nVia: SIP/2.0/TCP 10.20.30.9:6050;branch=z9hG4bK-early\r\nFrom: <sip:sender@ims.example>;tag=early\r\nTo: <sip:user@ims.example>\r\nCall-ID: protected-early\r\nCSeq: 7 MESSAGE\r\nContent-Length: 0\r\n\r\n";
        const RINGING: &[u8] = b"SIP/2.0 180 Ringing\r\nCall-ID: incoming-full-gate\r\nCSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";

        let configuration = |address: &str| TunnelConfiguration {
            ipv4_addresses: vec![address.parse().unwrap()],
            ..Default::default()
        };
        let local_network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let remote_network = TunnelNetwork::new(&configuration("10.20.30.9")).unwrap();
        let (local_wake_sender, local_wake) = watch::channel(0_u64);
        let (remote_wake_sender, remote_wake) = watch::channel(0_u64);
        let (local_tcp, mut local_tcp_runtime) = TcpSocketRegistry::new(local_wake_sender.clone());
        let (remote_tcp, mut remote_tcp_runtime) =
            TcpSocketRegistry::new(remote_wake_sender.clone());
        let (local_security, local_runtime) = SecurityNetworkRuntime::new(
            move |interface: &mut smoltcp::iface::Interface, sockets: &mut SocketSet<'static>| {
                local_tcp_runtime.service(interface, sockets);
                Ok::<_, Infallible>(())
            },
            local_wake_sender,
        );
        let (remote_security, remote_runtime) = SecurityNetworkRuntime::new(
            move |interface: &mut smoltcp::iface::Interface, sockets: &mut SocketSet<'static>| {
                remote_tcp_runtime.service(interface, sockets);
                Ok::<_, Infallible>(())
            },
            remote_wake_sender,
        );
        let (local_to_remote, remote_inbound) = mpsc::channel(64);
        let (remote_to_local, local_inbound) = mpsc::channel(64);
        let (network_stop_sender, network_stop) = watch::channel(false);
        let (server_ready_sender, server_ready) = tokio::sync::oneshot::channel();
        let (ringing_seen_sender, ringing_seen) = tokio::sync::oneshot::channel();
        let (reuse_ready_sender, reuse_ready) = tokio::sync::oneshot::channel();

        let local_runner = local_network.run_managed(
            SocketSet::new(vec![]),
            local_inbound,
            local_to_remote,
            network_stop.clone(),
            local_wake,
            local_runtime,
        );
        let remote_runner = remote_network.run_managed(
            SocketSet::new(vec![]),
            remote_inbound,
            remote_to_local,
            network_stop,
            remote_wake,
            remote_runtime,
        );
        // Erase the runtime closure's associated error type before joining the
        // two opaque `run_managed` futures (rustc cannot normalize both RPIT
        // outputs inside one `tokio::join!` expansion).
        let local_runner = async move { local_runner.await.unwrap() };
        let remote_runner = async move { remote_runner.await.unwrap() };

        let remote_server = async {
            // The mirror policy makes this a real ESP round-trip. An
            // uninstalled/plaintext protected SYN is rejected by the peer.
            let remote_lease = remote_security
                .install(full_gate_server_policy_for(algorithm, encryption))
                .await
                .unwrap();
            let protected_listener = remote_tcp
                .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
                .await
                .unwrap();
            let plain_listener = remote_tcp
                .listen(5060, Some("10.0.0.2:5050".parse().unwrap()))
                .await
                .unwrap();
            server_ready_sender.send(()).unwrap();
            let plain = plain_listener.accept().await.unwrap();
            let (plain_read, mut plain_write) = tokio::io::split(plain);
            let mut plain_reader = SipStreamReader::new(plain_read);
            let Some(SipStreamItem::Message(initial)) = plain_reader.next().await.unwrap() else {
                panic!("missing initial plaintext REGISTER")
            };
            let initial = String::from_utf8_lossy(&initial);
            assert!(initial.contains("CSeq: 1 REGISTER"));
            assert!(initial.contains("From: <sip:+447700900123@ims.example>"));
            assert!(initial.contains("Authorization: Digest username=\"subscriber@ims.example\""));
            let challenge = format!(
                "SIP/2.0 401 Challenge\r\nCall-ID: full-gate\r\nCSeq: 1 REGISTER\r\nWWW-Authenticate: Digest realm=\"ims.example\",nonce=\"{}\",algorithm=AKAv1-MD5\r\nSecurity-Server: {server_offer}\r\nContent-Length: 0\r\n\r\n",
                "01".repeat(32)
            );
            for method in ["MESSAGE", "INVITE", "CANCEL", "ACK"] {
                let request = format!(
                    "{method} sip:user@ims.example SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:5060;branch=z9hG4bK-old\r\nFrom: <sip:sender@ims.example>;tag=old\r\nTo: <sip:user@ims.example>\r\nCall-ID: old-unprotected\r\nCSeq: 3 {method}\r\nContent-Length: 0\r\n\r\n"
                );
                plain_write.write_all(request.as_bytes()).await.unwrap();
            }
            plain_write.write_all(challenge.as_bytes()).await.unwrap();
            plain_write.flush().await.unwrap();
            for (method, status) in [("MESSAGE", 503), ("INVITE", 503), ("CANCEL", 481)] {
                let Some(SipStreamItem::Message(wire)) = plain_reader.next().await.unwrap() else {
                    panic!("old request was not rejected on original writer")
                };
                let response = SipResponse::parse(&wire).unwrap();
                assert_eq!(response.status, status);
                assert_eq!(response.header("Call-ID"), Some("old-unprotected"));
                assert_eq!(
                    response.header("CSeq"),
                    Some(format!("3 {method}").as_str())
                );
                assert_eq!(
                    response.header("Via"),
                    Some("SIP/2.0/TCP 10.20.30.9:5060;branch=z9hG4bK-old")
                );
            }
            assert!(
                plain_reader.next().await.unwrap().is_none(),
                "client did not retire the exact plaintext association"
            );
            plain_reader
                .into_inner()
                .unsplit(plain_write)
                .close()
                .await
                .unwrap();

            let protected = protected_listener.accept().await.unwrap();
            let (protected_read, mut protected_write) = tokio::io::split(protected);
            let mut protected_reader = SipStreamReader::new(protected_read);
            let Some(SipStreamItem::Message(authenticated)) =
                protected_reader.next().await.unwrap()
            else {
                panic!("missing protected REGISTER")
            };
            let authenticated = String::from_utf8_lossy(&authenticated);
            assert!(authenticated.contains("CSeq: 2 REGISTER"));
            assert!(authenticated.contains("From: <sip:+447700900123@ims.example>"));
            assert!(
                authenticated.contains("Authorization: Digest username=\"subscriber@ims.example\"")
            );
            assert!(authenticated.contains("Security-Verify: ipsec-3gpp"));
            assert!(authenticated.contains("integrity-protected=yes"));

            // Deliberately coalesce the final REGISTER response and the first
            // MT request. The session must preserve the buffered INVITE and
            // attribute its response to this exact protected association.
            let registered = b"SIP/2.0 200 OK\r\nCall-ID: full-gate\r\nCSeq: 2 REGISTER\r\nContent-Length: 0\r\n\r\n";
            let mut coalesced = Vec::with_capacity(registered.len() + INVITE.len());
            coalesced.extend_from_slice(EARLY);
            coalesced.extend_from_slice(registered);
            coalesced.extend_from_slice(INVITE);
            protected_write.write_all(&coalesced).await.unwrap();
            protected_write.flush().await.unwrap();
            let Some(SipStreamItem::Message(early_reply)) = protected_reader.next().await.unwrap()
            else {
                panic!("lost retained protected MESSAGE reply")
            };
            assert_eq!(&*early_reply, EARLY_OK);
            let Some(SipStreamItem::Message(ringing)) = protected_reader.next().await.unwrap()
            else {
                panic!("missing same-connection response to coalesced INVITE")
            };
            assert_eq!(&*ringing, RINGING);
            // Retire the peer half after observing the response. The local
            // session must observe the exact registry close acknowledgement
            // before it can clear the installed SA and reuse either port.
            protected_reader
                .into_inner()
                .unsplit(protected_write)
                .close()
                .await
                .unwrap();
            ringing_seen_sender.send(()).unwrap();

            remote_lease.clear().await.unwrap();
            let remote_replacement = remote_security
                .install(full_gate_server_policy_for(algorithm, encryption))
                .await
                .expect("peer SA was not replaceable after protected close");
            let reuse_listener = remote_tcp
                .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
                .await
                .expect("protected server port was not reusable after close acknowledgement");
            reuse_ready_sender.send(()).unwrap();
            let reuse = reuse_listener.accept().await.unwrap();
            reuse.close().await.unwrap();
            remote_replacement.clear().await.unwrap();
        };

        let local_client = async {
            server_ready.await.unwrap();
            let (_session_stop_sender, session_stop) = watch::channel(false);
            let deadline = Instant::now() + Duration::from_secs(4);
            let (events, _event_receiver) = mpsc::channel(8);
            let sim = crate::carrier::SimIdentity {
                mcc: "234".into(),
                mnc: "33".into(),
                imsi: "234330123456789".into(),
                ..Default::default()
            };
            let carrier = crate::carrier::Registry::embedded()
                .unwrap()
                .resolve(&sim)
                .unwrap()
                .unwrap();
            let overrides = crate::carrier_overrides::OverrideSet::parse(
                format!("carrier_overrides:\n  '234/33':\n    ims_domain: ims.example\n    ims_realm: ims.example\n    ims_local_port: 5050\n    ims_identity_source: isim\n    ims_register_template:\n      security_client_mechanisms: [{{alg: {algorithm}, ealg: '{encryption}', prot: esp, mode: trans}}]\n").as_bytes(),
            )
            .unwrap();
            let profile = carrier
                .native_ims_register_profile(&sim, overrides.get("234", "33"))
                .unwrap();
            let device_identity = profile
                .resolve_device_identity(Some("353936932685355"))
                .unwrap();
            let ims_runtime = NativeImsRuntime {
                imsi: &sim.imsi,
                isim_private_identity: Some("subscriber@ims.example"),
                isim_public_identity: Some("sip:+447700900123@ims.example"),
                local_ip: "10.0.0.2".parse().unwrap(),
                pcscf: "10.20.30.9:5060".parse().unwrap(),
                call_id: "full-gate",
                from_tag: "full-gate",
                via_branch: "z9hG4bK-full-gate",
                cell_id: None,
                mcc: &sim.mcc,
                mnc: &sim.mnc,
            };
            let subscriber_identity = profile
                .resolve_subscriber_identity(ims_runtime)
                .expect("generation-bound SIM snapshot must resolve IMS identity");
            let mut session = register_native_profile_with_identity(
                RegistrationServices::new(&local_tcp, &local_security, &events),
                &profile,
                &device_identity,
                &subscriber_identity,
                ims_runtime,
                NativeSecurityEndpoints {
                    spi_c: 333,
                    spi_s: 444,
                    port_c: 5064,
                    port_s: 5063,
                },
                deadline,
                session_stop,
                move |rand, autn| async move {
                    assert_eq!(*rand, [1; 16]);
                    assert_eq!(*autn, [1; 16]);
                    Ok::<_, Infallible>(SimAnswer::Success {
                        res: Zeroizing::new(vec![1; 8]),
                        ck: Zeroizing::new([2; 16]),
                        ik: Zeroizing::new([3; 16]),
                    })
                },
            )
            .await
            .unwrap_or_else(|_| panic!("native carrier REGISTER integration failed"));
            assert_eq!(session.final_response().status, 200);
            assert!(session.is_secured());
            let incoming = session.incoming_diagnostics();
            assert!(incoming.listener_started);
            assert_eq!(incoming.accept_count, 0);
            assert!(incoming.last_sip_at.is_none());
            assert_eq!(incoming.reachability, IncomingReachability::Unverified);
            assert!(session.belongs_to_tcp(&local_tcp));
            assert!(!session.belongs_to_tcp(&remote_tcp));
            assert_eq!(session.message_local_port(), 5064);
            let calls = session
                .call_context()
                .expect("successful REGISTER did not freeze call inputs");
            assert_eq!(calls.public_identity(), "sip:+447700900123@ims.example");
            assert_eq!(calls.port_c(), 5064);
            assert_eq!(calls.port_s(), 5063);
            assert_eq!(calls.register_cseq(), 2);
            assert_eq!(calls.peer(), "10.20.30.9:5060".parse().unwrap());
            assert_eq!(session.signaling_peer(), "10.20.30.9:6060".parse().unwrap());
            assert!(calls.contact().contains("10.0.0.2:5063"));
            assert_eq!(calls.security_verify(), session.message_security_verify());

            assert!(
                session
                    .message_security_verify()
                    .unwrap()
                    .contains("ipsec-3gpp")
            );
            let snapshot = session
                .message_context(
                    "sip:registered@ims.example",
                    &["<sip:fallback@ims.example;lr>".into()],
                )
                .unwrap();
            assert_eq!(snapshot.local_port(), session.message_local_port());
            assert_eq!(
                snapshot.local_ip(),
                "10.0.0.2".parse::<std::net::IpAddr>().unwrap()
            );
            assert_eq!(
                snapshot.security_verify(),
                session.message_security_verify()
            );
            assert!(!snapshot.routing().identity().is_empty());
            assert!(session.listener.is_some(), "protected port-s was not armed");
            let early = session
                .next_event()
                .await
                .unwrap()
                .expect("pre-200 protected MESSAGE lost");
            assert_eq!(early.connection(), SipConnectionId::Primary);
            assert_eq!(session.incoming_diagnostics().accept_count, 0);
            assert!(session.incoming_diagnostics().last_sip_at.is_none());
            let SipStreamItem::Message(frame) = early.into_item() else {
                panic!("wrong retained event")
            };
            assert_eq!(
                &*frame, EARLY,
                "old plaintext event was relabelled protected"
            );
            session
                .send_on(SipConnectionId::Primary, EARLY_OK)
                .await
                .unwrap();
            let event = session
                .next_event()
                .await
                .unwrap()
                .expect("coalesced INVITE was lost at the REGISTER boundary");
            assert_eq!(event.connection(), SipConnectionId::Primary);
            let SipStreamItem::Message(invite) = event.into_item() else {
                panic!("coalesced INVITE was decoded as keepalive")
            };
            assert_eq!(&*invite, INVITE);
            session
                .send_on(SipConnectionId::Primary, RINGING)
                .await
                .unwrap();
            ringing_seen.await.unwrap();
            session.shutdown().await.unwrap();

            // A successful replacement proves the exact session generation
            // was cleared only after both TCP close acknowledgements. Reusing
            // both local protected ports then proves no registry owner leaked.
            let replacement = local_security
                .install(full_gate_client_policy_for(algorithm, encryption))
                .await
                .expect("session shutdown returned before clearing its SA generation");
            let reused_port_s = local_tcp
                .listen(5063, Some("10.20.30.9:6050".parse().unwrap()))
                .await
                .expect("protected port-s was not reusable after shutdown");
            reused_port_s.close().await.unwrap();
            reuse_ready.await.unwrap();
            let reused_port_c = local_tcp
                .dial("10.20.30.9:6060".parse().unwrap(), 5064)
                .await
                .expect("protected port-c was not reusable after shutdown");
            let reused_port_c = reused_port_c.connect().await.unwrap();
            reused_port_c.close().await.unwrap();
            replacement.clear().await.unwrap();
        };

        let exercise = async {
            tokio::join!(local_client, remote_server);
            network_stop_sender.send(true).unwrap();
        };
        let (_, _, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(local_runner, remote_runner, exercise)
        })
        .await
        .expect("full sec-agree registration gate timed out");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn incoming_call_is_dispatched_and_replied_on_its_own_connection() {
        let configuration = |address: &str| TunnelConfiguration {
            ipv4_addresses: vec![address.parse().unwrap()],
            ..Default::default()
        };
        let local_network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let remote_network = TunnelNetwork::new(&configuration("10.20.30.9")).unwrap();
        let (local_wake_sender, local_wake) = watch::channel(0_u64);
        let (remote_wake_sender, remote_wake) = watch::channel(0_u64);
        let (local_tcp, local_registry) = TcpSocketRegistry::new(local_wake_sender);
        let (remote_tcp, remote_registry) = TcpSocketRegistry::new(remote_wake_sender);
        let (local_to_remote, remote_inbound) = mpsc::channel(64);
        let (remote_to_local, local_inbound) = mpsc::channel(64);
        let (network_stop_sender, network_stop) = watch::channel(false);

        let local_runner = local_network.run_managed(
            SocketSet::new(vec![]),
            local_inbound,
            local_to_remote,
            network_stop.clone(),
            local_wake,
            local_registry,
        );
        let remote_runner = remote_network.run_managed(
            SocketSet::new(vec![]),
            remote_inbound,
            remote_to_local,
            network_stop,
            remote_wake,
            remote_registry,
        );
        let application = async {
            let remote_primary_listener = remote_tcp
                .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
                .await
                .unwrap();
            let local_primary_dial = local_tcp
                .dial("10.20.30.9:6060".parse().unwrap(), 5064)
                .await
                .unwrap();
            let incoming_listener = local_tcp
                .listen(5063, Some("10.20.30.9:6050".parse().unwrap()))
                .await
                .unwrap();
            let (local_primary, remote_primary) = tokio::join!(
                local_primary_dial.connect(),
                remote_primary_listener.accept()
            );
            let local_primary = local_primary.unwrap();
            let remote_primary = remote_primary.unwrap();
            let (primary_reader, primary_writer) = tokio::io::split(local_primary);
            let (_session_stop_sender, session_stop) = watch::channel(false);
            let mut session = RegisteredImsSession {
                refresh_configuration:None,registration_lease_observer:None,register_request:None,registration_expiry:None,
            heartbeat_configuration:None,heartbeat_health:crate::sip_session_runtime::HeartbeatHealth::new(),heartbeat_observer:None,
                incoming_facts: Default::default(),
                incoming_observer: None,
                tcp_owner: None,
                primary: SignalStream::open(
                    SipStreamReader::new(primary_reader),
                    primary_writer,
                ),
                listener: Some(incoming_listener),
                listener_config: Some(IncomingListenerConfig {
                    tcp: local_tcp.clone(),
                    local_port: 5063,
                    expected_peer: "10.20.30.9:6050".parse().unwrap(),
                }),
                incoming: std::array::from_fn(|_| None),
                next_incoming_id: 1,
                stop: session_stop,
                cancellation_owner: None,
                lease: None,
                registration_expires_seconds: None,
                registration_renewal_deadline: None,
                startup_events: Default::default(),
                call_context: None,
                register_contact: None,
                register_peer: "127.0.0.1:5060".parse().unwrap(),
                register_port_s: 5060,
            register_cseq: 1,
                message_local_port: 5064,
            message_local_ip: "10.0.0.2".parse().unwrap(),
            message_security_verify: None,
            final_response: SipResponse::parse(
                    b"SIP/2.0 200 OK\r\nCall-ID: registered\r\nCSeq: 2 REGISTER\r\nContent-Length: 0\r\n\r\n",
                )
                .unwrap(),
            };
            let incoming_dial = remote_tcp
                .dial("10.0.0.2:5063".parse().unwrap(), 6050)
                .await
                .unwrap();
            let request = b"INVITE sip:user@ims.example SIP/2.0\r\nCall-ID: incoming\r\nCSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";
            let response = b"SIP/2.0 180 Ringing\r\nCall-ID: incoming\r\nCSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";
            let initial = session.incoming_diagnostics();
            assert!(initial.listener_started);
            assert_eq!(initial.accept_count, 0);
            assert_eq!(initial.reachability, IncomingReachability::Unverified);
            assert!(initial.last_sip_at.is_none());
            let (diagnostics, mut observation) = watch::channel(initial);
            session.set_incoming_observer(Arc::new(move |facts| {
                diagnostics.send_replace(facts);
            }));
            let mut runtime = crate::sip_session_runtime::RegisteredSipRuntime::new(session);
            let (send_data, data_allowed) = tokio::sync::oneshot::channel();
            let remote_call = async {
                let mut stream = incoming_dial.connect().await.unwrap();
                data_allowed.await.unwrap();
                stream.write_all(request).await.unwrap();
                let mut received = vec![0; response.len()];
                stream.read_exact(&mut received).await.unwrap();
                assert_eq!(received, response);
                stream.close().await.unwrap();
                let second = remote_tcp
                    .dial("10.0.0.2:5063".parse().unwrap(), 6050)
                    .await
                    .unwrap();
                let mut stream = second.connect().await.unwrap();
                stream.write_all(request).await.unwrap();
                let mut eof = [0_u8; 1];
                assert_eq!(stream.read(&mut eof).await.unwrap(), 0);
                stream.close().await.unwrap();
            };
            let local_call = async {
                {
                    let event = runtime.next_event();
                    tokio::pin!(event);
                    while observation.borrow().accept_count == 0
                        || !observation.borrow().listener_started
                    {
                        tokio::select! {
                            result = &mut event => { let _ = result; panic!("no SIP was sent yet"); },
                            changed = observation.changed() => changed.unwrap(),
                        }
                    }
                }
                let accepted = *observation.borrow();
                assert_eq!(accepted.accept_count, 1);
                assert_eq!(accepted.active_connections, 1);
                assert!(accepted.listener_started, "port-s must immediately rearm");
                assert_eq!(accepted.reachability, IncomingReachability::Observed);
                assert!(
                    accepted.last_sip_at.is_none(),
                    "TCP accept alone is not SIP data"
                );
                send_data.send(()).unwrap();
                let event = runtime
                    .next_event()
                    .await
                    .unwrap()
                    .expect("missing incoming INVITE");
                let crate::sip_session_runtime::RegisteredSipEvent::Message {
                    connection: SipConnectionId::Incoming(connection),
                    message,
                    ..
                } = event
                else {
                    panic!("incoming INVITE must retain its actual association");
                };
                assert!(matches!(
                    message.as_ref(),
                    crate::sip_session_runtime::RegisteredSipMessage::Request {
                        method: proletarius_sip::Method::INVITE,
                        ..
                    }
                ));
                assert!(runtime.incoming_diagnostics().last_sip_at.is_some());
                let (proletarius_sip::Message::Response(response), _) =
                    proletarius_sip::Parser::default()
                        .parse(response, false)
                        .unwrap()
                else {
                    panic!("response");
                };
                runtime
                    .send_dialog_response(SipConnectionId::Incoming(connection), response)
                    .await
                    .unwrap();
                let event = runtime
                    .next_event()
                    .await
                    .unwrap()
                    .expect("missing second incoming INVITE");
                let crate::sip_session_runtime::RegisteredSipEvent::Message {
                    connection: SipConnectionId::Incoming(second),
                    ..
                } = event
                else {
                    panic!("second incoming");
                };
                assert_ne!(connection, second);
                let rearmed = runtime.incoming_diagnostics();
                assert_eq!(rearmed.accept_count, 2);
                assert_eq!(rearmed.active_connections, 1);
                assert_eq!(rearmed.closed_connections, 1);
                assert!(rearmed.listener_started);
                runtime.shutdown().await.unwrap();
                let retired = *observation.borrow();
                assert!(!retired.listener_started && !retired.listener_closing);
                assert_eq!(retired.active_connections, 0);
                assert_eq!(retired.closing_connections, 0);
                assert_eq!(retired.closed_connections, 2);
                assert_eq!(retired.accept_count, 2);
                assert_eq!(retired.reachability, IncomingReachability::Observed);
                assert!(retired.last_response_write_error.is_none());
            };
            tokio::join!(remote_call, local_call);
            remote_primary.close().await.unwrap();
            network_stop_sender.send(true).unwrap();
        };

        let (local, remote, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(local_runner, remote_runner, application)
        })
        .await
        .expect("incoming call test timed out");
        local.unwrap();
        remote.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_partial_send_finishes_old_frame_before_admitting_new_frame() {
        let configuration = |address: &str| TunnelConfiguration {
            ipv4_addresses: vec![address.parse().unwrap()],
            ..Default::default()
        };
        let local_network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let remote_network = TunnelNetwork::new(&configuration("10.20.30.9")).unwrap();
        let (local_wake_sender, local_wake) = watch::channel(0_u64);
        let (remote_wake_sender, remote_wake) = watch::channel(0_u64);
        let (local_tcp, local_registry) = TcpSocketRegistry::new(local_wake_sender);
        let (remote_tcp, remote_registry) = TcpSocketRegistry::new(remote_wake_sender);
        let (local_to_remote, remote_inbound) = mpsc::channel(64);
        let (remote_to_local, local_inbound) = mpsc::channel(64);
        let (network_stop_sender, network_stop) = watch::channel(false);

        let local_runner = local_network.run_managed(
            SocketSet::new(vec![]),
            local_inbound,
            local_to_remote,
            network_stop.clone(),
            local_wake,
            local_registry,
        );
        let remote_runner = remote_network.run_managed(
            SocketSet::new(vec![]),
            remote_inbound,
            remote_to_local,
            network_stop,
            remote_wake,
            remote_registry,
        );
        let application = async {
            let listener = remote_tcp
                .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
                .await
                .unwrap();
            let dial = local_tcp
                .dial("10.20.30.9:6060".parse().unwrap(), 5064)
                .await
                .unwrap();
            let (local, remote) = tokio::join!(dial.connect(), listener.accept());
            let local = local.unwrap();
            let mut remote = remote.unwrap();
            let (_session_stop_sender, session_stop) = watch::channel(false);
            let mut session = registered_session(local, session_stop);
            let filler = vec![b'f'; 32 * 1024];
            let old = vec![b'o'; SESSION_MESSAGE_LIMIT];
            let new = b"new-frame";

            tokio::time::timeout(Duration::from_secs(1), session.send(&filler))
                .await
                .expect("filler did not reach the bounded transport")
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(25), session.send(&old))
                    .await
                    .is_err(),
                "backpressured send unexpectedly completed"
            );
            let offset = match &session.primary {
                SignalStream::Open(open) => {
                    open.pending
                        .as_ref()
                        .expect("cancelled send lost its pending frame")
                        .offset
                }
                _ => panic!("cancelled send closed the primary connection"),
            };
            assert!(
                offset > 0 && offset < old.len(),
                "test did not cancel after a partial write: {offset}"
            );

            let receive = async {
                let mut bytes = vec![0; filler.len() + old.len() + new.len()];
                remote.read_exact(&mut bytes).await.unwrap();
                assert_eq!(&bytes[..filler.len()], filler.as_slice());
                assert_eq!(
                    &bytes[filler.len()..filler.len() + old.len()],
                    old.as_slice()
                );
                assert_eq!(&bytes[filler.len() + old.len()..], new);
            };
            let (send_result, ()) = tokio::join!(session.send(new), receive);
            send_result.unwrap();

            let rpdu = [0x00, 0x2a, 0x00, 0x02, 0x81, 0x21, 0x01, 0x01];
            let routes = ["<sip:pcscf.example.test;lr>"];
            let context = SmsRequestContext {
                target: "+8613800138000",
                public_identity: "sip:+8613800000000@ims.example.test",
                call_id: "offline-sms-fixture",
                from_tag: "offline-tag",
                via_branch: "z9hG4bK-offline-sms",
                local_host: "10.0.0.2",
                local_port: 5064,
                cseq: 90,
                route_headers: &routes,
                security_verify: Some("ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc"),
                access_network_info: None,
            };
            let expected = build_sms_message(&context, &rpdu).unwrap();
            let final_response = b"SIP/2.0 202 Accepted\r\nVia: SIP/2.0/TCP 10.0.0.2:5064;branch=z9hG4bK-offline-sms\r\nFrom: <sip:+8613800000000@ims.example.test>;tag=offline-tag\r\nTo: <tel:+8613800138000>;tag=offline-peer\r\nCall-ID: offline-sms-fixture\r\nCSeq: 90 MESSAGE\r\nContent-Length: 0\r\n\r\n";
            let mut submission = SmsSubmission::new(1).unwrap();
            let local_sms = async {
                session
                    .submit_sms_part(&mut submission, &context, &rpdu)
                    .await
                    .unwrap();
                let event = session
                    .next_event()
                    .await
                    .unwrap()
                    .expect("missing synthetic SMS response");
                let SipStreamItem::Message(response) = event.into_item() else {
                    panic!("expected a SIP response frame")
                };
                assert_eq!(
                    submission.observe_response(&response).unwrap(),
                    crate::sms_transaction::SmsResponseDisposition::PartAccepted {
                        completed: true
                    }
                );
                assert_eq!(
                    submission.status(),
                    &crate::sms_transaction::SmsSubmissionStatus::Accepted { parts: 1 }
                );
                session.shutdown().await.unwrap();
            };
            let remote_sms = async {
                let mut bytes = vec![0; expected.len()];
                remote.read_exact(&mut bytes).await.unwrap();
                assert_eq!(bytes, expected);
                remote.write_all(final_response).await.unwrap();
                remote.flush().await.unwrap();
                let mut eof = [0];
                assert_eq!(remote.read(&mut eof).await.unwrap(), 0);
                remote.close().await.unwrap();
            };
            tokio::join!(local_sms, remote_sms);
            network_stop_sender.send(true).unwrap();
        };

        let (local, remote, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(local_runner, remote_runner, application)
        })
        .await
        .expect("cancel-safe session send test timed out");
        local.unwrap();
        remote.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn incoming_write_deadline_closes_only_the_target_connection() {
        let configuration = |address: &str| TunnelConfiguration {
            ipv4_addresses: vec![address.parse().unwrap()],
            ..Default::default()
        };
        let local_network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let remote_network = TunnelNetwork::new(&configuration("10.20.30.9")).unwrap();
        let (local_wake_sender, local_wake) = watch::channel(0_u64);
        let (remote_wake_sender, remote_wake) = watch::channel(0_u64);
        let (local_tcp, local_registry) = TcpSocketRegistry::new(local_wake_sender);
        let (remote_tcp, remote_registry) = TcpSocketRegistry::new(remote_wake_sender);
        let (local_to_remote, remote_inbound) = mpsc::channel(64);
        let (remote_to_local, local_inbound) = mpsc::channel(64);
        let (network_stop_sender, network_stop) = watch::channel(false);

        let local_runner = local_network.run_managed(
            SocketSet::new(vec![]),
            local_inbound,
            local_to_remote,
            network_stop.clone(),
            local_wake,
            local_registry,
        );
        let remote_runner = remote_network.run_managed(
            SocketSet::new(vec![]),
            remote_inbound,
            remote_to_local,
            network_stop,
            remote_wake,
            remote_registry,
        );
        let application = async {
            let primary_listener = remote_tcp
                .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
                .await
                .unwrap();
            let incoming_listener = remote_tcp
                .listen(6050, Some("10.0.0.2:5065".parse().unwrap()))
                .await
                .unwrap();
            let primary_dial = local_tcp
                .dial("10.20.30.9:6060".parse().unwrap(), 5064)
                .await
                .unwrap();
            let incoming_dial = local_tcp
                .dial("10.20.30.9:6050".parse().unwrap(), 5065)
                .await
                .unwrap();
            let (primary_pair, incoming_pair) = tokio::join!(
                async { tokio::join!(primary_dial.connect(), primary_listener.accept()) },
                async { tokio::join!(incoming_dial.connect(), incoming_listener.accept()) },
            );
            let (local_primary, remote_primary) = primary_pair;
            let (local_incoming, remote_incoming) = incoming_pair;
            let local_primary = local_primary.unwrap();
            let mut remote_primary = remote_primary.unwrap();
            let local_incoming = local_incoming.unwrap();
            let remote_incoming = remote_incoming.unwrap();
            let (_session_stop_sender, session_stop) = watch::channel(false);
            let mut session = registered_session(local_primary, session_stop);
            let (reader, writer) = tokio::io::split(local_incoming);
            session.incoming[0] = Some(IncomingSignalStream {
                id: 1,
                stream: SignalStream::open(SipStreamReader::new(reader), writer),
            });

            let filler = vec![b'f'; 32 * 1024];
            tokio::time::timeout(
                Duration::from_secs(1),
                session.send_on(SipConnectionId::Incoming(1), &filler),
            )
            .await
            .expect("incoming filler write stalled")
            .unwrap();
            let mut response = vec![b'x'; SESSION_MESSAGE_LIMIT];
            response[..12].copy_from_slice(b"SIP/2.0 200 ");
            let error = session
                .send_on_until(
                    SipConnectionId::Incoming(1),
                    &response,
                    Instant::now() + Duration::from_millis(25),
                )
                .await
                .expect_err("backpressured incoming send ignored its deadline");
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(
                session.incoming_diagnostics().last_response_write_error,
                Some(IncomingWriteError::Timeout)
            );
            assert_eq!(session.incoming_diagnostics().closed_connections, 1);
            assert_eq!(session.incoming_diagnostics().active_connections, 0);
            assert!(matches!(session.primary, SignalStream::Open(_)));
            assert!(matches!(
                session.incoming[0]
                    .as_ref()
                    .map(|connection| &connection.stream),
                Some(SignalStream::Closed)
            ));

            let primary_message = b"primary-remains-independent";
            let receive_primary = async {
                let mut bytes = vec![0; primary_message.len()];
                remote_primary.read_exact(&mut bytes).await.unwrap();
                assert_eq!(bytes, primary_message);
            };
            let (send_result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(session.send(primary_message), receive_primary)
            })
            .await
            .expect("independent primary connection stalled");
            send_result.unwrap();
            tokio::time::timeout(Duration::from_secs(1), session.shutdown())
                .await
                .expect("local session shutdown stalled")
                .unwrap();
            tokio::time::timeout(Duration::from_secs(1), remote_primary.close())
                .await
                .expect("remote primary close stalled")
                .unwrap();
            tokio::time::timeout(Duration::from_secs(1), remote_incoming.close())
                .await
                .expect("remote incoming close stalled")
                .unwrap();
            network_stop_sender.send(true).unwrap();
        };

        let (local, remote, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(local_runner, remote_runner, application)
        })
        .await
        .expect("per-connection write deadline test timed out");
        local.unwrap();
        remote.unwrap();
    }

    #[tokio::test]
    async fn unserviced_tcp_command_observes_stop_without_hanging() {
        let (wake, mut wake_events) = watch::channel(0_u64);
        let (tcp, _unserviced_registry) = TcpSocketRegistry::new(wake);
        let (stop_sender, stop) = watch::channel(false);
        let deadline = Instant::now() + Duration::from_secs(30);

        let waiter = tokio::spawn(async move {
            bounded::<_, _, ()>(
                tcp.listen(5063, None),
                RegistrationStage::ProtectedListen,
                deadline,
                stop,
            )
            .await
        });

        // TcpControl notifies only after the command has entered the runtime
        // queue, making this an in-flight cancellation test rather than an
        // already-cancelled precondition check.
        tokio::time::timeout(Duration::from_secs(1), wake_events.changed())
            .await
            .expect("TCP command was not submitted")
            .expect("TCP runtime wake channel closed");
        stop_sender.send(true).unwrap();

        let result = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("bounded TCP command ignored stop")
            .expect("bounded TCP command task panicked");
        assert!(matches!(
            result,
            Err(RegistrationError::Cancelled(
                RegistrationStage::ProtectedListen
            ))
        ));
    }

    #[tokio::test]
    async fn unserviced_tcp_command_observes_deadline_without_hanging() {
        let (wake, mut wake_events) = watch::channel(0_u64);
        let (tcp, _unserviced_registry) = TcpSocketRegistry::new(wake);
        let (_stop_sender, stop) = watch::channel(false);
        let deadline = Instant::now() + Duration::from_millis(100);

        let waiter = tokio::spawn(async move {
            bounded::<_, _, ()>(
                tcp.dial("10.0.0.9:5062".parse().unwrap(), 5061),
                RegistrationStage::ProtectedDial,
                deadline,
                stop,
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(1), wake_events.changed())
            .await
            .expect("TCP command was not submitted")
            .expect("TCP runtime wake channel closed");

        let result = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("bounded TCP command ignored deadline")
            .expect("bounded TCP command task panicked");
        assert!(matches!(
            result,
            Err(RegistrationError::Deadline(
                RegistrationStage::ProtectedDial
            ))
        ));
    }
}
