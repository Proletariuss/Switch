//! Native reader and module VoWiFi session preparation.
//!
//! The ordinary reader service owns the complete retained IKE/IMS, SMS and
//! call runtime. Preparation atomically captures the card generation, carrier
//! overrides and proxy choice before network work. The module Service retains
//! its USB and RF capability while sharing the native network runtime and
//! ordinary admission gate with the reader Service.

use std::{
    convert::Infallible,
    env, fs,
    future::Future,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Incoming};
use proletarius_ims::{aka_exchange::SimAnswer, stream::SipStreamItem};
use proletarius_vowifi::{
    DatagramTransportError, EpdgDatagramTransport, NativeNetworkError, NativeTunnel,
    NativeTunnelSettings, SimAkaResult, TunnelConfiguration, TunnelStartupError,
    carrier::{NativeTunnelOverrides, Registry, SimIdentity},
    carrier_overrides::OverrideSet,
    ims_network_runtime::ImsNetworkRuntime,
    ims_profile::{
        NativeImsDeviceIdentity, NativeImsProfileError, NativeImsRegisterProfile, NativeImsRuntime,
        NativeImsSessionMode, NativeImsSubscriberIdentity, NativeSecurityEndpoints,
    },
    netstack_tcp::TcpControl,
    registration_coordinator::{
        NativeProfileRegistrationFailure, RegisteredImsSession, RegistrationServices,
    },
    security_runtime::SecurityControl,
};
use serde::Deserialize;
use serde_json::json;
use zeroize::{Zeroize, Zeroizing};

use crate::transport_selection::{SelectedTransport, SessionSelectionError};

const REQUEST_LIMIT: usize = 4_096;
const OVERRIDES_ENVIRONMENT: &str = "PROLETARIUS_VOWIFI_CONFIG";
static CARRIER_REGISTRY: OnceLock<Result<Registry, String>> = OnceLock::new();

mod api;
pub(crate) mod card;
pub(crate) mod module;
pub(crate) mod multi;
mod registration_refresh;
use card::SessionCard;
pub(crate) mod configuration;
pub(crate) use configuration::preview_reader_status;
mod intent;
mod operation;
mod ordinary;
mod registration_candidates;
mod service;
mod startup_diagnostics;
mod startup_transport;
pub(crate) use service::NativeSmsBinding;

/// Compile-time evidence for the ordinary reader owner. The retained actor
/// owns all five resources together; the shared admission gate still excludes
/// a concurrent legacy module owner. This is not an environment feature flag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeOwnerReadiness {
    cross_process_reader_lease: bool,
    ble_aka_transport_bridge: bool,
    keyed_lifecycle_actor: bool,
    protected_ims_runtime: bool,
    live_sms_call_runtime: bool,
}

impl NativeOwnerReadiness {
    const fn current() -> Self {
        Self {
            cross_process_reader_lease: true,
            // The lease-bound AKA path now submits BLE USIM/ISIM APDUs via
            // Rust BleApduTransport and the authenticated Swift system bridge.
            ble_aka_transport_bridge: true,
            // Public reader admission now uses the same retained Service,
            // LiveDriver and protected IMS/SMS/call runtime as the production
            // network compositions. These gates do not admit module sessions.
            keyed_lifecycle_actor: true,
            protected_ims_runtime: true,
            live_sms_call_runtime: true,
        }
    }

    const fn ready(self) -> bool {
        self.cross_process_reader_lease
            && self.ble_aka_transport_bridge
            && self.keyed_lifecycle_actor
            && self.protected_ims_runtime
            && self.live_sms_call_runtime
    }

    fn missing(self) -> Vec<&'static str> {
        let mut missing = Vec::with_capacity(5);
        if !self.cross_process_reader_lease {
            missing.push("cross_process_reader_lease");
        }
        if !self.ble_aka_transport_bridge {
            missing.push("ble_aka_transport_bridge");
        }
        if !self.keyed_lifecycle_actor {
            missing.push("keyed_lifecycle_actor");
        }
        if !self.protected_ims_runtime {
            missing.push("protected_ims_runtime");
        }
        if !self.live_sms_call_runtime {
            missing.push("live_sms_call_runtime");
        }
        missing
    }
}

#[derive(Deserialize)]
pub(crate) struct PlanRequest {
    pub(crate) reader: String,
    #[serde(default)]
    pub(crate) proxy: String,
    #[serde(default)]
    pub(crate) epdg: String,
    #[serde(default)]
    pub(crate) epdg_port: Option<u16>,
}

/// One immutable reader/SIM session context. This deliberately has no
/// `Clone`, `Debug` or serialization implementation: it contains the bound
/// AKA identity and is the only future owner allowed to launch a tunnel.
pub(crate) struct NativeSessionContext {
    reader: String,
    reader_generation: u64,
    /// Opaque reader/SIM key derived from the same physical-card snapshot as
    /// the AKA binding. It never comes from the HTTP request and is retained
    /// for the future per-session supervisor.
    #[allow(dead_code)] // Consumed when the gated native actor takes ownership.
    session_key: crate::native_vowifi_supervisor::ReaderSessionKey,
    /// Stable SMS archive identity derived from this exact leased-card
    /// snapshot, compatible with the former Go session owner.
    #[allow(dead_code)] // Consumed when the native SMS consumer is launched.
    sms_subscription_id: Option<String>,
    mcc: String,
    mnc: String,
    carrier_id: String,
    config: proletarius_vowifi::carrier::NativeCarrierTunnelConfig,
    runtime: proletarius_vowifi::carrier_session_config::NativeTunnelRuntimeConfig,
    access_isim: bool,
    /// Resolved from the same SIM/template/override snapshot as the tunnel.
    /// It is not live yet; the future registered-session owner consumes it.
    ims: NativeImsRegisterProfile,
    /// Resolve and validate IMPI/IMPU before proxy/tunnel startup; these
    /// zeroizing values are bound to the same generation as IKE AKA.
    #[allow(dead_code, reason = "consumed by the gated native IMS session owner")]
    subscriber_identity: NativeImsSubscriberIdentity,
    /// Read once from the existing Go-owned persistent identity and bound to
    /// this context. It must never appear in plan diagnostics or logs.
    #[allow(dead_code)] // Consumed by the gated native IMS runtime owner.
    device_identity: NativeImsDeviceIdentity,
    /// Optional 3GPP IKE identity from the same frozen carrier/IMEI snapshot.
    ike_device_identity_imei: Option<Zeroizing<String>>,
    configuration_status: serde_json::Value,
    transport: SelectedTransport,
    /// The immutable card capability supplies both IKE and IMS AKA. Ordinary
    /// sessions retain their existing lease; N-wait borrows a short physical
    /// workflow for its frozen profile without pinning the reader for network IO.
    #[allow(dead_code)] // Moved into the keyed actor when live session routes are enabled.
    card: SessionCard,
}

#[allow(dead_code, reason = "gated native startup owner settings")]
struct NativeRegisteredStartupSettings {
    endpoints: NativeSecurityEndpoints,
    deadline: tokio::time::Instant,
    probe_interval: Duration,
    retry_interval: Duration,
    attempts: u8,
}

#[allow(
    dead_code,
    reason = "gated startup preserves both registration and network failure"
)]
enum NativeRegisteredStartupFailure {
    ServiceGone {
        cleanup: Result<
            Result<(), proletarius_vowifi::registration_coordinator::CleanupReport>,
            tokio::task::JoinError,
        >,
        network: Result<Result<(), NativeNetworkError<Infallible>>, tokio::task::JoinError>,
        local_retired: bool,
    },
    SessionMismatch,
    Tunnel(NativeTunnelStartError),
    Registration {
        error: NativeImsRegistrationError,
        network: Result<Result<(), NativeNetworkError<Infallible>>, tokio::task::JoinError>,
        network_ended_before_cleanup: bool,
        local_retired: bool,
        traffic: proletarius_vowifi::TunnelTrafficSnapshot,
    },
}

impl NativeRegisteredStartupFailure {
    fn cleanup_confirmed(&self) -> bool {
        match self {
            Self::ServiceGone {
                cleanup,
                network,
                local_retired,
            } => *local_retired || (matches!(cleanup, Ok(Ok(()))) && matches!(network, Ok(Ok(())))),
            Self::Registration {
                error,
                network,
                local_retired,
                ..
            } => {
                *local_retired
                    || (matches!(network, Ok(Ok(())))
                        && match error {
                            NativeImsRegistrationError::Endpoint(_)
                            | NativeImsRegistrationError::Entropy => true,
                            NativeImsRegistrationError::CancellationOwner(cleanup) => {
                                cleanup.is_ok()
                            }
                            NativeImsRegistrationError::Registration(failure) => {
                                failure.registration().cleanup().is_clean()
                            }
                            NativeImsRegistrationError::SessionMismatch
                            | NativeImsRegistrationError::TransportClosed => false,
                        })
            }
            // start_tunnel returns only after its heartbeat and stop forwarder
            // are joined; failed startup owns no running network task. Physical
            // AKA occupancy is still verified by release_with_confirmation.
            Self::Tunnel(_) => true,
            Self::SessionMismatch => false,
        }
    }
}

#[allow(dead_code, reason = "gated native SMS startup inputs")]
struct NativeSmsStartupInputs {
    settings: NativeRegisteredStartupSettings,
    setup: NativeSmsSetup,
    sip_events: tokio::sync::mpsc::Sender<SipStreamItem>,
    notices: tokio::sync::mpsc::Sender<crate::native_sms_consumer::LoopNotice>,
}

#[allow(dead_code, reason = "failed startup retains its supervisor permit")]
struct NativeSmsStartupFailure {
    permit: crate::native_vowifi_supervisor::NativeSessionPermit,
    error: NativeRegisteredStartupFailure,
}

/// A retained lease is releasable only after the owner has proven IMS/network cleanup.
#[allow(dead_code, reason = "gated failed-session recovery record")]
struct NativeLeaseRecovery {
    phase: Option<tokio::sync::watch::Sender<ordinary::Phase>>,
    status: Option<ordinary::status::Reporter>,
    lease: SessionCard,
    cleanup_confirmed: std::sync::atomic::AtomicBool,
}

impl NativeLeaseRecovery {
    fn publish_released(&self) {
        if let Some(status) = &self.status {
            status.lease_released();
        }
        if let Some(phase) = &self.phase {
            phase.send_replace(ordinary::Phase::Released);
        }
    }
    fn confirm_cleanup(&self) {
        self.cleanup_confirmed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[allow(
        dead_code,
        reason = "consumed by future generation-bound recovery scheduler"
    )]
    fn releasable_lease(&self) -> Option<SessionCard> {
        self.cleanup_confirmed
            .load(std::sync::atomic::Ordering::Acquire)
            .then(|| self.lease.clone())
    }
}

#[allow(
    dead_code,
    reason = "gated session completion retains its exact card lease"
)]
struct NativeSmsSessionResult {
    lease_release: Option<Result<(), proletarius_domain::BackendFailure>>,
    worker: Result<Result<NativeSmsWorkerExit, tokio::task::JoinError>, NativeSmsStartupFailure>,
    lease: SessionCard,
}

#[allow(
    dead_code,
    reason = "gated typed recovery diagnosis, not yet exposed to UI"
)]
enum NativeRecoveryFailure {
    Release(proletarius_domain::BackendFailure),
    Worker(tokio::task::JoinError),
    Observer(tokio::task::JoinError),
}

struct NativeRecoveryFailureRecord {
    generation: Option<crate::native_vowifi_supervisor::SessionGeneration>,
    failure: NativeRecoveryFailure,
}

struct NativeRecoveryTask {
    generation: crate::native_vowifi_supervisor::SessionGeneration,
    task: crate::native_ims_actor::Task<Result<(), proletarius_domain::BackendFailure>>,
}

impl NativeRecoveryTask {
    async fn finish(
        self,
    ) -> Result<Result<(), proletarius_domain::BackendFailure>, tokio::task::JoinError> {
        self.task.finish().await
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum NativeRecoveryStatus {
    Confirmed,
    ReleaseFailed,
    WorkerFailed,
    ObserverFailed,
}

#[derive(Clone, Copy, Default)]
struct NativeRecoveryProgress {
    revision: u64,
    confirmed: u64,
    failed: u64,
    task_failed: u64,
    omitted_failures: u64,
    generation: Option<crate::native_vowifi_supervisor::SessionGeneration>,
    status: Option<NativeRecoveryStatus>,
}

#[derive(Default)]
#[allow(dead_code, reason = "gated recovery loop aggregate result")]
struct NativeRecoveryLoopExit {
    revision: u64,
    last_generation: Option<crate::native_vowifi_supervisor::SessionGeneration>,
    last_status: Option<NativeRecoveryStatus>,
    confirmed: u64,
    failed: u64,
    task_failed: u64,
    failures: std::collections::VecDeque<NativeRecoveryFailureRecord>,
    omitted_failures: u64,
}

impl NativeRecoveryLoopExit {
    fn progress(&self) -> NativeRecoveryProgress {
        NativeRecoveryProgress {
            revision: self.revision,
            confirmed: self.confirmed,
            failed: self.failed,
            task_failed: self.task_failed,
            omitted_failures: self.omitted_failures,
            generation: self.last_generation,
            status: self.last_status,
        }
    }

    fn record_join(
        &mut self,
        result: Result<
            (
                crate::native_vowifi_supervisor::SessionGeneration,
                Result<Result<(), proletarius_domain::BackendFailure>, tokio::task::JoinError>,
            ),
            tokio::task::JoinError,
        >,
    ) {
        match result {
            Ok((generation, outcome)) => self.record_for(Some(generation), Ok(outcome)),
            Err(error) => self.record_for(None, Err(error)),
        }
    }

    fn record(
        &mut self,
        result: Result<
            Result<Result<(), proletarius_domain::BackendFailure>, tokio::task::JoinError>,
            tokio::task::JoinError,
        >,
    ) {
        self.record_for(None, result);
    }

    fn record_for(
        &mut self,
        generation: Option<crate::native_vowifi_supervisor::SessionGeneration>,
        result: Result<
            Result<Result<(), proletarius_domain::BackendFailure>, tokio::task::JoinError>,
            tokio::task::JoinError,
        >,
    ) {
        self.revision = self.revision.saturating_add(1);
        self.last_generation = generation;
        let failure = match result {
            Ok(Ok(Ok(()))) => {
                self.last_status = Some(NativeRecoveryStatus::Confirmed);
                self.confirmed = self.confirmed.saturating_add(1);
                None
            }
            Ok(Ok(Err(error))) => {
                self.last_status = Some(NativeRecoveryStatus::ReleaseFailed);
                self.failed = self.failed.saturating_add(1);
                Some(NativeRecoveryFailure::Release(error))
            }
            Ok(Err(error)) => {
                self.last_status = Some(NativeRecoveryStatus::WorkerFailed);
                self.task_failed = self.task_failed.saturating_add(1);
                Some(NativeRecoveryFailure::Worker(error))
            }
            Err(error) => {
                self.last_status = Some(NativeRecoveryStatus::ObserverFailed);
                self.task_failed = self.task_failed.saturating_add(1);
                Some(NativeRecoveryFailure::Observer(error))
            }
        };
        if let Some(failure) = failure {
            if self.failures.len() == 8 {
                self.failures.pop_front();
                self.omitted_failures = self.omitted_failures.saturating_add(1);
            }
            self.failures.push_back(NativeRecoveryFailureRecord {
                generation,
                failure,
            });
        }
    }
}

#[allow(dead_code, reason = "gated native service lifetime owner")]
struct NativeSessionServiceOwner {
    stopping: bool,
    supervisor:
        std::sync::Arc<std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>>,
    session_tasks: std::collections::HashMap<
        crate::native_vowifi_supervisor::SessionGeneration,
        crate::native_ims_actor::Task<NativeSmsSessionResult>,
    >,
    ordinary_tasks: std::collections::HashMap<
        crate::native_vowifi_supervisor::SessionGeneration,
        crate::native_ims_actor::Task<ordinary::SessionResult>,
    >,
    ordinary_results: std::collections::VecDeque<(
        crate::native_vowifi_supervisor::SessionGeneration,
        ordinary::SessionResult,
    )>,
    session_task_failure: Option<tokio::task::JoinError>,
    recovery: Option<crate::native_ims_actor::Task<NativeRecoveryLoopExit>>,
    progress: tokio::sync::watch::Receiver<NativeRecoveryProgress>,
}

#[derive(Default)]
struct NativeLifecycleState {
    cleanup_failure: Option<&'static str>,
    owner: Option<NativeSessionServiceOwner>,
    stopping: bool,
    finishing: bool,
}

#[derive(Default)]
pub(crate) struct NativeLifecycle {
    state: std::sync::Mutex<NativeLifecycleState>,
    ordinary_admission: Arc<crate::ordinary_vowifi_admission::Gate>,
    ordinary_service: OnceLock<Result<service::Service, String>>,
    module_service: OnceLock<Result<module::Service, String>>,
}

pub(crate) struct ReaderObservation {
    pub(crate) status: serde_json::Value,
    pub(crate) reader: String,
    pub(crate) reader_generation: u64,
    pub(crate) sim_id: String,
    pub(crate) session_active: bool,
    pub(crate) identity_valid: bool,
    pub(crate) sms_ready: bool,
    pub(crate) subscription_id: Option<String>,
}
impl ReaderObservation {
    fn unavailable() -> Self {
        Self {
            status: serde_json::json!({"available":false,"tunnel_state":"disconnected","ims_state":"idle","sms_ready":false,"sms_state":"unavailable","call_ready":false,"call_state":"unavailable","incoming_listener_started":false,"incoming_accept_count":0,"incoming_reachability":"unknown","progress_step":0,"message":"原生会话状态暂不可用"}),
            reader: String::new(),
            reader_generation: 0,
            sim_id: String::new(),
            session_active: false,
            identity_valid: false,
            sms_ready: false,
            subscription_id: None,
        }
    }
}

impl NativeLifecycle {
    pub(crate) fn reader_observation(&self) -> Option<ReaderObservation> {
        match self.ordinary_service.get() {
            None => None,
            Some(Err(_)) => Some(ReaderObservation::unavailable()),
            Some(Ok(service)) => service.reader_observation(),
        }
    }
    pub(crate) fn ordinary_admission(&self) -> Arc<crate::ordinary_vowifi_admission::Gate> {
        self.ordinary_admission.clone()
    }

    pub(crate) fn module_call_service(
        &self,
        method: &hyper::Method,
        path: &str,
    ) -> Option<module::Service> {
        let service = self.retained_module_service()?;
        let call = api::call_route_owner(method, path, true, false).is_some();
        let ussd = *method == hyper::Method::POST && path == "/api/reader/vowifi/ussd";
        (service.owns_session() && (call || ussd)).then_some(service)
    }

    pub(crate) fn handles_public_request(&self, method: &hyper::Method, path: &str) -> bool {
        // The current module is selected first by main. Shared aliases otherwise
        // resolve the retained reader (or a native unavailable/conflict response),
        // never fall back to a second Go ordinary IMS executor.
        if api::call_route_owner(method, path, true, false).is_some()
            || (*method == hyper::Method::POST && path == "/api/reader/vowifi/ussd")
        {
            return true;
        }
        service::public_route(method, path)
            && (NativeOwnerReadiness::current().ready() || self.reader_observation().is_some())
    }

    /// Activate the daemon-owned session supervisor and its bounded lease
    /// recovery worker. Session admission remains closed until the public
    /// VoWiFi start path is connected to the same owner and all SMS/call gates
    /// are satisfied; starting this owner alone never reports service ready.
    pub(crate) fn activate(&self) -> Result<(), &'static str> {
        let owner = NativeSessionServiceOwner::new(Duration::from_secs(1))?;
        self.install(owner).map_err(|(reason, _owner)| reason)
    }

    /// Stop exactly the daemon-issued session generation. A caller supplies
    /// no reader/SIM identity, so lifecycle control cannot disclose ICCID or
    /// IMSI and cannot accidentally stop a replacement generation.
    #[allow(
        dead_code,
        reason = "native session API remains gated until startup inputs are authoritative"
    )]
    fn stop_generation(&self, generation: u64) -> crate::native_vowifi_supervisor::StopDisposition {
        let Some(generation) =
            crate::native_vowifi_supervisor::SessionGeneration::from_value(generation)
        else {
            return crate::native_vowifi_supervisor::StopDisposition::Missing;
        };
        let Ok(mut state) = self.state.lock() else {
            return crate::native_vowifi_supervisor::StopDisposition::Missing;
        };
        let Some(owner) = state.owner.as_mut() else {
            return crate::native_vowifi_supervisor::StopDisposition::Missing;
        };
        owner.stop_generation(generation)
    }

    #[allow(
        dead_code,
        reason = "native session status remains private until keyed startup is wired"
    )]
    fn snapshot_generation(
        &self,
        generation: u64,
    ) -> Option<crate::native_vowifi_supervisor::NativeSessionSnapshot> {
        let generation =
            crate::native_vowifi_supervisor::SessionGeneration::from_value(generation)?;
        let state = self.state.lock().ok()?;
        let owner = state.owner.as_ref()?;
        owner
            .supervisor
            .lock()
            .ok()?
            .snapshot_generation(generation)
    }

    #[allow(dead_code, reason = "installed only after native startup acceptance")]
    fn install(
        &self,
        owner: NativeSessionServiceOwner,
    ) -> Result<(), (&'static str, NativeSessionServiceOwner)> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return Err(("native_lifecycle_unavailable", owner)),
        };
        if state.stopping || state.finishing || state.owner.is_some() {
            return Err(("native_lifecycle_not_accepting", owner));
        }
        state.owner = Some(owner);
        Ok(())
    }

    pub(crate) fn begin_shutdown(&self) -> Result<(), &'static str> {
        let (mut state, poisoned) = match self.state.lock() {
            Ok(state) => (state, false),
            Err(error) => (error.into_inner(), true),
        };
        state.stopping = true;
        self.ordinary_admission.begin_shutdown();
        if let Some(service) = self.ordinary_service() {
            service.begin_shutdown();
        }
        if let Some(service) = self.retained_module_service() {
            service.begin_shutdown();
        }
        if let Some(owner) = state.owner.as_mut() {
            owner
                .begin_shutdown()
                .map_err(|_| "native_lifecycle_unavailable")?;
        }
        if poisoned {
            self.state.clear_poison();
        }
        Ok(())
    }

    pub(crate) async fn finish_shutdown(
        self: &std::sync::Arc<Self>,
        deadline: tokio::time::Instant,
    ) -> Result<(), &'static str> {
        self.begin_shutdown()?;
        if let Some(service) = self.ordinary_service() {
            service.finish_before(deadline).await?;
        }
        if let Some(service) = self.retained_module_service() {
            service.stop_before(deadline).await?;
        }
        let owner = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "native_lifecycle_unavailable")?;
            if let Some(reason) = state.cleanup_failure {
                return Err(reason);
            }
            if state.finishing {
                return Err("native_shutdown_in_progress");
            }
            let Some(owner) = state.owner.take() else {
                return Ok(());
            };
            state.finishing = true;
            owner
        };
        let lifecycle = self.clone();
        let (cancel, _) = tokio::sync::watch::channel(false);
        crate::native_ims_actor::Task::spawn(cancel, async move {
            let outcome = owner.finish_before(deadline).await;
            let mut state = match lifecycle.state.lock() {
                Ok(state) => state,
                Err(error) => error.into_inner(),
            };
            state.finishing = false;
            state.stopping = true;
            let result = match outcome {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(_)) => {
                    state.cleanup_failure = Some("native_recovery_task_failed");
                    Err("native_recovery_task_failed")
                }
                Err((_, owner)) => {
                    state.owner = Some(owner);
                    Err("native_shutdown_unconfirmed")
                }
            };
            lifecycle.state.clear_poison();
            result
        })
        .finish()
        .await
        .map_err(|_| {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(error) => error.into_inner(),
            };
            state.finishing = false;
            state.stopping = true;
            state.cleanup_failure = Some("native_shutdown_task_failed");
            self.state.clear_poison();
            "native_shutdown_task_failed"
        })?
    }
}

enum NativeSessionShutdownWaitError {
    Deadline,
    SupervisorUnavailable,
}

enum NativeSessionAdmissionError {
    Stopping,
    SupervisorUnavailable,
    GenerationCollision,
    PreviousSessionTaskFailed,
    Reserve(crate::native_vowifi_supervisor::ReserveError),
}

#[allow(dead_code, reason = "gated native service lifetime assembly")]
impl NativeSessionServiceOwner {
    fn new(poll_interval: Duration) -> Result<Self, &'static str> {
        let supervisor = std::sync::Arc::new(std::sync::Mutex::new(
            crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default(),
        ));
        let (recovery, progress) =
            spawn_lease_recovery_loop_with_progress(&supervisor, poll_interval)?;
        Ok(Self {
            stopping: false,
            supervisor,
            session_tasks: std::collections::HashMap::new(),
            ordinary_tasks: std::collections::HashMap::new(),
            ordinary_results: std::collections::VecDeque::new(),
            session_task_failure: None,
            recovery: Some(recovery),
            progress,
        })
    }

    fn reserve_session(
        &self,
        key: &crate::native_vowifi_supervisor::ReaderSessionKey,
    ) -> Result<crate::native_vowifi_supervisor::NativeSessionPermit, NativeSessionAdmissionError>
    {
        if self.stopping {
            return Err(NativeSessionAdmissionError::Stopping);
        }
        self.supervisor
            .lock()
            .map_err(|_| NativeSessionAdmissionError::SupervisorUnavailable)?
            .reserve(key.clone())
            .map_err(NativeSessionAdmissionError::Reserve)
    }

    async fn launch(
        &mut self,
        context: NativeSessionContext,
        inputs: NativeSmsStartupInputs,
    ) -> Result<
        crate::native_vowifi_supervisor::SessionGeneration,
        (NativeSessionAdmissionError, NativeSessionContext),
    > {
        self.reap_finished_session_tasks().await;
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
        if self.session_tasks.contains_key(&generation) {
            if let Ok(mut supervisor) = self.supervisor.lock() {
                supervisor.worker_finished(&permit);
            }
            return Err((NativeSessionAdmissionError::GenerationCollision, context));
        }
        let task = context.spawn_sms_session(permit, self.supervisor.clone(), inputs);
        self.session_tasks.insert(generation, task);
        Ok(generation)
    }

    fn stop_generation(
        &mut self,
        generation: crate::native_vowifi_supervisor::SessionGeneration,
    ) -> crate::native_vowifi_supervisor::StopDisposition {
        let disposition = self
            .supervisor
            .lock()
            .map(|mut supervisor| supervisor.request_stop_generation(generation))
            .unwrap_or(crate::native_vowifi_supervisor::StopDisposition::Missing);
        if matches!(
            disposition,
            crate::native_vowifi_supervisor::StopDisposition::Signalled
                | crate::native_vowifi_supervisor::StopDisposition::AlreadyStopping
        ) && let Some(task) = self.session_tasks.get(&generation)
        {
            task.request_stop();
        }
        if let Some(task) = self.ordinary_tasks.get(&generation) {
            task.request_stop();
        }
        disposition
    }

    fn reap_ordinary_tasks(&mut self) {
        let finished: Vec<_> = self
            .ordinary_tasks
            .iter_mut()
            .filter_map(|(generation, task)| task.try_finish().map(|result| (*generation, result)))
            .collect();
        for (generation, result) in finished {
            self.ordinary_tasks.remove(&generation);
            match result {
                Ok(result) => {
                    if self.ordinary_results.len() == 16 {
                        self.ordinary_results.pop_front();
                    }
                    self.ordinary_results.push_back((generation, result));
                }
                Err(error) if self.session_task_failure.is_none() => {
                    self.session_task_failure = Some(error)
                }
                Err(_) => {}
            }
        }
    }

    async fn reap_finished_session_tasks(&mut self) {
        self.reap_ordinary_tasks();
        let finished: Vec<_> = self
            .session_tasks
            .iter()
            .filter_map(|(generation, task)| task.is_finished().then_some(*generation))
            .collect();
        for generation in finished {
            let Some(task) = self.session_tasks.remove(&generation) else {
                continue;
            };
            if let Err(error) = task.finish().await
                && self.session_task_failure.is_none()
            {
                self.session_task_failure = Some(error);
            }
        }
    }

    fn begin_shutdown(&mut self) -> Result<(), NativeSessionAdmissionError> {
        self.stopping = true;
        match self.supervisor.lock() {
            Ok(mut owner) => {
                owner.request_stop_all();
            }
            Err(_) => {
                // Even when the supervisor lock is poisoned, the daemon still
                // owns these tasks and must request cooperative cleanup before
                // returning an unconfirmed-shutdown error.
                for task in self.session_tasks.values() {
                    task.request_stop();
                }
                for task in self.ordinary_tasks.values() {
                    task.request_stop();
                }
                if let Some(recovery) = self.recovery.as_ref() {
                    recovery.request_stop();
                }
                return Err(NativeSessionAdmissionError::SupervisorUnavailable);
            }
        }
        for task in self.session_tasks.values() {
            task.request_stop();
        }
        for task in self.ordinary_tasks.values() {
            task.request_stop();
        }
        Ok(())
    }

    /// Wait for slots while the caller independently pumps authenticated callbacks.
    async fn finish_before(
        mut self,
        deadline: tokio::time::Instant,
    ) -> Result<
        Result<NativeRecoveryLoopExit, tokio::task::JoinError>,
        (NativeSessionShutdownWaitError, Self),
    > {
        if self.begin_shutdown().is_err() {
            return Err((NativeSessionShutdownWaitError::SupervisorUnavailable, self));
        }
        loop {
            self.reap_finished_session_tasks().await;
            let empty = self.supervisor.lock().ok().map(|owner| owner.is_empty());
            let Some(empty) = empty else {
                return Err((NativeSessionShutdownWaitError::SupervisorUnavailable, self));
            };
            if empty && self.session_tasks.is_empty() && self.ordinary_tasks.is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err((NativeSessionShutdownWaitError::Deadline, self));
            }
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {},
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
        }
        self.finish_after_sessions()
            .await
            .map_err(|owner| (NativeSessionShutdownWaitError::SupervisorUnavailable, owner))
    }

    /// Refuse final shutdown while any generation still awaits cleanup proof.
    async fn finish_after_sessions(
        mut self,
    ) -> Result<Result<NativeRecoveryLoopExit, tokio::task::JoinError>, Self> {
        if self.begin_shutdown().is_err() {
            return Err(self);
        }
        let empty = self
            .supervisor
            .lock()
            .ok()
            .is_some_and(|owner| owner.is_empty());
        if !empty || !self.session_tasks.is_empty() || !self.ordinary_tasks.is_empty() {
            return Err(self);
        }
        let task = self.recovery.take().expect("recovery owner consumed once");
        task.request_stop();
        let recovery = task.finish().await;
        if let Some(session_failure) = self.session_task_failure.take()
            && recovery.is_ok()
        {
            return Ok(Err(session_failure));
        }
        Ok(recovery)
    }
}

impl Drop for NativeSessionServiceOwner {
    fn drop(&mut self) {
        if let Ok(mut owner) = self.supervisor.lock() {
            owner.request_stop_all();
        }
        // Dropping the recovery Task requests stop and leaves its drain running.
    }
}

/// Stop polling first, then drain already-started release operations without abort.
#[allow(dead_code, reason = "gated native recovery background owner")]
fn spawn_lease_recovery_loop(
    supervisor: &std::sync::Arc<
        std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
    >,
    poll_interval: Duration,
) -> Result<crate::native_ims_actor::Task<NativeRecoveryLoopExit>, &'static str> {
    spawn_lease_recovery_loop_with_progress(supervisor, poll_interval).map(|(task, _)| task)
}

fn spawn_lease_recovery_loop_with_progress(
    supervisor: &std::sync::Arc<
        std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
    >,
    poll_interval: Duration,
) -> Result<
    (
        crate::native_ims_actor::Task<NativeRecoveryLoopExit>,
        tokio::sync::watch::Receiver<NativeRecoveryProgress>,
    ),
    &'static str,
> {
    if poll_interval.is_zero() {
        return Err("native_recovery_interval_invalid");
    }
    let owner = std::sync::Arc::downgrade(supervisor);
    let (cancel, mut stop) = tokio::sync::watch::channel(false);
    let (progress, receiver) = tokio::sync::watch::channel(NativeRecoveryProgress::default());
    let task = crate::native_ims_actor::Task::spawn(cancel, async move {
        let mut tasks = tokio::task::JoinSet::new();
        let mut result = NativeRecoveryLoopExit::default();
        let mut ticks = tokio::time::interval(poll_interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *stop.borrow() || stop.has_changed().is_err() {
                break;
            }
            tokio::select! {
                biased;
                _ = stop.changed() => {},
                outcome = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(outcome) = outcome {
                        result.record_join(outcome);
                        progress.send_replace(result.progress());
                    }
                }
                _ = ticks.tick() => {
                    let Some(owner) = owner.upgrade() else { break; };
                    for task in dispatch_lease_recovery(owner, 8_usize.saturating_sub(tasks.len())) {
                        let generation = task.generation;
                        tasks.spawn(async move { (generation, task.finish().await) });
                    }
                }
            }
        }
        while let Some(outcome) = tasks.join_next().await {
            result.record_join(outcome);
            progress.send_replace(result.progress());
        }
        result
    });
    Ok((task, receiver))
}

/// One explicit bounded recovery pass; no timer or automatic retry policy here.
#[allow(dead_code, reason = "gated bounded native lease recovery dispatch")]
fn dispatch_lease_recovery(
    supervisor: std::sync::Arc<
        std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
    >,
    limit: usize,
) -> Vec<NativeRecoveryTask> {
    let candidates = match supervisor.lock() {
        Ok(owner) => owner.recovery_candidates::<NativeLeaseRecovery>(limit, |record| {
            record.releasable_lease().is_some()
        }),
        Err(_) => return Vec::new(),
    };
    candidates
        .into_iter()
        .filter_map(|(key, generation)| {
            spawn_retained_lease_recovery(&key, generation, supervisor.clone())
                .map(|task| NativeRecoveryTask { generation, task })
        })
        .collect()
}

#[allow(dead_code, reason = "gated retained-session recovery dispatch")]
fn spawn_retained_lease_recovery(
    key: &crate::native_vowifi_supervisor::ReaderSessionKey,
    generation: crate::native_vowifi_supervisor::SessionGeneration,
    supervisor: std::sync::Arc<
        std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
    >,
) -> Option<crate::native_ims_actor::Task<Result<(), proletarius_domain::BackendFailure>>> {
    let permit = supervisor.lock().ok()?.recovery_permit(key, generation)?;
    spawn_lease_recovery(permit, supervisor)
}

/// Explicit retry only: no recovery can issue IO without proven cleanup.
#[allow(dead_code, reason = "gated generation-bound lease recovery")]
fn spawn_lease_recovery(
    permit: crate::native_vowifi_supervisor::NativeSessionPermit,
    supervisor: std::sync::Arc<
        std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
    >,
) -> Option<crate::native_ims_actor::Task<Result<(), proletarius_domain::BackendFailure>>> {
    let (lease, record) = {
        let mut owner = supervisor.lock().ok()?;
        let record = owner.recovery::<NativeLeaseRecovery>(&permit)?;
        let lease = record.releasable_lease()?;
        if !owner.claim_recovery(&permit) {
            return None;
        }
        (lease, record)
    };
    let cancel = permit.cancellation_sender();
    let owner = std::sync::Arc::downgrade(&supervisor);
    Some(crate::native_ims_actor::Task::spawn(cancel, async move {
        let result = lease.release_with_confirmation().await;
        if let Some(owner) = owner.upgrade() {
            if let Ok(mut owner) = owner.lock() {
                if result.is_ok() {
                    owner.worker_finished(&permit);
                } else {
                    owner.recovery_failed(&permit);
                }
            }
        }
        if result.is_ok() {
            record.publish_released();
        }
        result
    }))
}

impl NativeSessionContext {
    /// Cancellation of the waiting handle requests stop without aborting startup,
    /// registration rollback or the registered SMS worker's ordered cleanup.
    #[allow(dead_code, reason = "gated native SMS startup ownership")]
    fn spawn_sms_session(
        self,
        permit: crate::native_vowifi_supervisor::NativeSessionPermit,
        supervisor: std::sync::Arc<
            std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
        >,
        inputs: NativeSmsStartupInputs,
    ) -> crate::native_ims_actor::Task<NativeSmsSessionResult> {
        let cancel = permit.cancellation_sender();
        let mut inputs = inputs;
        inputs.setup.subscription_id = resolve_native_sms_subscription_id(
            inputs.setup.subscription_id.take(),
            self.sms_subscription_id.clone(),
        );
        // Do not let this task keep the public service alive indefinitely.
        let supervisor = std::sync::Arc::downgrade(&supervisor);
        let lease = self.card.clone();
        let recovery = std::sync::Arc::new(NativeLeaseRecovery {
            phase: None,
            status: None,
            lease: lease.clone(),
            cleanup_confirmed: std::sync::atomic::AtomicBool::new(false),
        });
        crate::native_ims_actor::Task::spawn(cancel.clone(), async move {
            let retained = permit.matches_key(&self.session_key)
                && supervisor.upgrade().is_some_and(|owner| {
                    owner
                        .lock()
                        .is_ok_and(|mut owner| owner.retain_recovery(&permit, recovery.clone()))
                });
            if !retained {
                cancel.send_replace(true);
            }
            let startup = self
                .start_registered_ims(&permit, &inputs.sip_events, inputs.settings)
                .await;
            let mut worker = match startup {
                Ok((network, session)) => {
                    let Some(owner) = supervisor.upgrade() else {
                        cancel.send_replace(true);
                        let retirement = network.retirement.clone();
                        let (cleanup, network) =
                            network.finish_after_registered_shutdown(session).await;
                        let error = NativeRegisteredStartupFailure::ServiceGone {
                            cleanup,
                            network,
                            local_retired: retirement.confirmed(),
                        };
                        let lease_release = if error.cleanup_confirmed() {
                            recovery.confirm_cleanup();
                            Some(lease.release_with_confirmation().await)
                        } else {
                            None
                        };
                        return NativeSmsSessionResult {
                            lease_release,
                            worker: Err(NativeSmsStartupFailure { permit, error }),
                            lease,
                        };
                    };
                    Ok(network
                        .spawn_registered_sms_with_completion(
                            permit,
                            session,
                            owner,
                            inputs.setup,
                            inputs.notices,
                            false,
                        )
                        .finish()
                        .await)
                }
                Err(error) => Err(NativeSmsStartupFailure { permit, error }),
            };
            let lease_release = if let Ok(Ok(worker)) = &mut worker {
                if let Ok((exit, network, _)) = &worker.actor
                    && registered_cleanup_complete(exit, network)
                {
                    // Keep proof even when the global recovery quota postpones IO.
                    recovery.confirm_cleanup();
                }
                finish_registered_lease(worker, lease.release_with_confirmation(), &supervisor)
                    .await
            } else if let Err(failure) = &worker {
                if failure.error.cleanup_confirmed() {
                    recovery.confirm_cleanup();
                    release_with_claim(
                        &failure.permit,
                        lease.release_with_confirmation(),
                        &supervisor,
                    )
                    .await
                    .map(|(released, _)| released)
                } else {
                    None
                }
            } else {
                None
            };
            NativeSmsSessionResult {
                worker,
                lease,
                lease_release,
            }
        })
    }

    /// The caller retains the permit until returned owners have been retired.
    /// This does not publish readiness or release a supervisor generation.
    #[allow(dead_code, reason = "gated native startup composition")]
    async fn start_registered_ims(
        &self,
        permit: &crate::native_vowifi_supervisor::NativeSessionPermit,
        events: &tokio::sync::mpsc::Sender<SipStreamItem>,
        settings: NativeRegisteredStartupSettings,
    ) -> Result<(NativeImsNetworkControl, RegisteredImsSession), NativeRegisteredStartupFailure>
    {
        self.start_registered_ims_observed(
            permit,
            events,
            settings,
            NativeImsSessionMode::VoiceAndSms,
            None,
        )
        .await
    }

    async fn start_registered_ims_observed(
        &self,
        permit: &crate::native_vowifi_supervisor::NativeSessionPermit,
        events: &tokio::sync::mpsc::Sender<SipStreamItem>,
        settings: NativeRegisteredStartupSettings,
        session_mode: NativeImsSessionMode,
        reporter: Option<ordinary::status::Reporter>,
    ) -> Result<(NativeImsNetworkControl, RegisteredImsSession), NativeRegisteredStartupFailure>
    {
        if !permit.matches_key(&self.session_key) {
            return Err(NativeRegisteredStartupFailure::SessionMismatch);
        }
        let stop = permit.stop_signal();
        if *stop.borrow() || stop.has_changed().is_err() {
            return Err(NativeRegisteredStartupFailure::Tunnel(
                NativeTunnelStartError::Stopped,
            ));
        }
        if let Some(reporter) = &reporter {
            reporter.tunnel_started();
        }
        let tunnel = self
            .start_tunnel(permit.stop_signal())
            .await
            .map_err(NativeRegisteredStartupFailure::Tunnel)?;
        if let Some(reporter) = &reporter {
            reporter.tunnel_ready(tunnel.tunnel.configuration());
        }
        let mut network = tunnel.start_ims_network(
            Duration::from_secs(self.runtime.dpd_interval_seconds),
            settings.retry_interval,
            settings.attempts,
        );
        if let Some(reporter) = &reporter {
            reporter.register_started();
        }
        match self
            .register_ims_with_session_mode(
                &mut network,
                events,
                settings.endpoints,
                settings.deadline,
                permit.stop_signal(),
                session_mode,
            )
            .await
        {
            Ok(session) => {
                if let Some(reporter) = &reporter {
                    reporter.registered(&session);
                }
                Ok((network, session))
            }
            Err(error) => {
                // register_ims has awaited coordinator rollback. Keep its report
                // even if subsequent IKE/network cleanup also fails.
                let network_ended_before_cleanup = network.network_task.is_finished();
                network.request_ike_stop();
                let retirement = network.retirement.clone();
                let result = network.network_task.await;
                let traffic = network.traffic_probe.snapshot();
                network.lease_heartbeat.stop().await;
                Err(NativeRegisteredStartupFailure::Registration {
                    error,
                    network: result,
                    network_ended_before_cleanup,
                    local_retired: retirement.confirmed(),
                    traffic,
                })
            }
        }
    }

    #[allow(dead_code)] // Becomes live only after protected IMS runtime takeover.
    async fn authenticate_ike(
        &self,
        challenge: proletarius_ike::eap_aka::EapAkaChallenge,
    ) -> Result<SimAkaResult, NativeAkaError> {
        self.card
            .authenticate_ike(&challenge.rand, &challenge.autn)
            .await
            .map_err(NativeAkaError::Lease)
    }

    /// Register over controls serviced by the same authenticated tunnel and
    /// SIM lease. Numeric carrier P-CSCF overrides precede IKE CFG_REPLY;
    /// this path never resolves IMS names through local DNS. Initial, AKA and protected
    /// REGISTER all consume the identity prepared with this exact lease.
    #[allow(dead_code, reason = "consumed by the gated keyed native session actor")]
    async fn register_ims(
        &self,
        network: &mut NativeImsNetworkControl,
        events: &tokio::sync::mpsc::Sender<SipStreamItem>,
        endpoints: NativeSecurityEndpoints,
        deadline: tokio::time::Instant,
        stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<RegisteredImsSession, NativeImsRegistrationError> {
        self.register_ims_with_session_mode(
            network,
            events,
            endpoints,
            deadline,
            stop,
            NativeImsSessionMode::VoiceAndSms,
        )
        .await
    }

    async fn register_ims_with_session_mode(
        &self,
        network: &mut NativeImsNetworkControl,
        events: &tokio::sync::mpsc::Sender<SipStreamItem>,
        endpoints: NativeSecurityEndpoints,
        deadline: tokio::time::Instant,
        stop: tokio::sync::watch::Receiver<bool>,
        session_mode: NativeImsSessionMode,
    ) -> Result<RegisteredImsSession, NativeImsRegistrationError> {
        if self.session_key != network.session_key {
            return Err(NativeImsRegistrationError::SessionMismatch);
        }
        if network.tcp.is_closed() {
            return Err(NativeImsRegistrationError::TransportClosed);
        }
        let candidates = registration_candidates::select(
            &self.ims,
            network.configuration(),
            self.config.ip_stack,
        )
        .map_err(NativeImsRegistrationError::Endpoint)?;
        let local_ip = candidates[0].local_ip;
        let pcscf = candidates[0].registrar;
        let tokens = NativeSipTransactionIds::generate()
            .map_err(|()| NativeImsRegistrationError::Entropy)?;
        let runtime = NativeImsRuntime {
            imsi: self.card.identity().imsi.as_str(),
            isim_private_identity: Some(self.card.identity().private_identity.as_str()),
            isim_public_identity: Some(self.card.identity().public_identity.as_str()),
            local_ip,
            pcscf,
            call_id: &tokens.call_id,
            from_tag: &tokens.from_tag,
            via_branch: &tokens.via_branch,
            cell_id: None,
            mcc: &self.mcc,
            mnc: &self.mnc,
        };
        let eap_direct_material = network.take_eap_direct_auth_material();
        let services = network.registration_services(events);
        let lease = self.card.clone();
        let initially_stopped = *stop.borrow()
            || stop.has_changed().is_err()
            || *network.lease_lost.borrow()
            || network.lease_lost.has_changed().is_err()
            || network.tcp.is_closed();
        let (registration_stop, registration_signal) =
            tokio::sync::watch::channel(initially_stopped);
        let ims_isim = self.subscriber_identity.uses_isim();
        let registration = registration_candidates::register_with_session_mode(
            services,
            &self.ims,
            &self.device_identity,
            &self.subscriber_identity,
            runtime,
            endpoints,
            eap_direct_material.as_ref(),
            &candidates,
            session_mode,
            deadline,
            registration_signal,
            move |rand, autn| {
                let lease = lease.clone();
                async move {
                    let result = if ims_isim {
                        lease.authenticate_ims(&rand, &autn).await
                    } else {
                        lease.authenticate_ike(&rand, &autn).await
                    }
                    .map_err(NativeAkaError::Lease)?;
                    Ok::<_, NativeAkaError>(ims_sim_answer(result))
                }
            },
        );
        let result = finish_owned_registration(
            registration,
            wait_registration_stop(&network.tcp, stop, network.lease_lost.clone()),
            registration_stop,
        )
        .await;
        result
    }

    /// Starts the IKE owner from this exact context. This intentionally has no
    /// public HTTP route yet: a tunnel alone must never be represented as
    /// IMS/SMS/voice readiness before the protected runtime and its lifecycle
    /// manager are attached. The eventual supervisor owns this context and
    /// invokes this method at most once per generation.
    #[allow(dead_code)] // Enabled only by the next gated daemon owner.
    async fn start_tunnel(
        &self,
        mut actor_stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<NativeTunnelOwner, NativeTunnelStartError> {
        if *actor_stop.borrow() || actor_stop.has_changed().is_err() {
            return Err(NativeTunnelStartError::Stopped);
        }
        let peer_certificate = crate::native_ike_certificate::policy(&self.config)
            .map_err(|_| NativeTunnelStartError::CertificateValidationUnavailable)?;
        let mut heartbeat = self.card.heartbeat();
        if self.card.kind() == card::SessionKind::Module && self.runtime.rf_off_delay_seconds > 0 {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(self.runtime.rf_off_delay_seconds)) => {},
                _ = actor_stop.changed() => {
                    heartbeat.stop().await;
                    return Err(NativeTunnelStartError::Stopped);
                },
                _ = heartbeat.wait_lost() => {
                    heartbeat.stop().await;
                    return Err(NativeTunnelStartError::LeaseLost);
                },
            }
            if *actor_stop.borrow() || actor_stop.has_changed().is_err() {
                heartbeat.stop().await;
                return Err(NativeTunnelStartError::Stopped);
            }
        }
        // Never give the supervisor's stop receiver directly to IKE. Once IMS
        // is registered, the keyed actor must first shut down the protected
        // REGISTER/SIP owner, then signal IKE to send its protected DELETE.
        let (ike_stop, ike_stop_receiver) = tokio::sync::watch::channel(false);
        let actor_stop_forwarder = forward_startup_stop(actor_stop, ike_stop.clone());
        let transport = tokio::select! {
            result = EpdgDatagramTransport::open_with_local_port(
                self.transport.mode.clone(),
                &self.config.epdg,
                self.config.epdg_port,
                self.transport.proxy,
                self.runtime.ike_local_port,
            ) => match result {
                Ok(transport) => transport,
                Err(error) => {
                    actor_stop_forwarder.abort();
                    let _ = actor_stop_forwarder.await;
                    heartbeat.stop().await;
                    return Err(NativeTunnelStartError::Transport(error));
                }
            },
            _ = heartbeat.wait_lost() => {
                actor_stop_forwarder.abort();
                let _ = actor_stop_forwarder.await;
                heartbeat.stop().await;
                return Err(NativeTunnelStartError::LeaseLost);
            }
        };
        let lease = &self.card;
        let settings = NativeTunnelSettings {
            identity: self.config.identity(),
            apn: &self.config.apn,
            device_identity_imei: self.ike_device_identity_imei.as_deref().map(String::as_str),
            ike_proposals: &self.config.proposals.ike,
            esp_proposals: &self.config.proposals.esp,
            ip_stack: self.config.ip_stack,
            checkcode_mode: self.config.aka_challenge_mode,
            initial_contact: true,
            retain_eap_direct_material: self.ims.template().requires_eap_direct_material(),
        };
        let startup = proletarius_vowifi::start_configured_native_tunnel_with_options(
            transport,
            settings,
            proletarius_vowifi::NativeTunnelOptions {
                local_identity: self
                    .config
                    .ike_local_identity()
                    .map(|identity| Zeroizing::new(identity.to_vec())),
                peer_certificate,
                disable_switch_to_natt_port: self.runtime.disable_switch_to_natt_port,
                nat_keepalive: Duration::from_secs(self.runtime.nat_keepalive_seconds),
                disable_dpd: self.runtime.disable_dpd,
                reauth_interval: (self.runtime.reauth_interval_seconds > 0)
                    .then(|| Duration::from_secs(self.runtime.reauth_interval_seconds)),
                reauth_jitter: true,
                cp_in_final_auth: self.runtime.cp_in_final_auth,
                cookie_as_sa: self.runtime.cookie_payload_type == "sa",
                rekey_pfs: self.runtime.rekey_pfs,
                child_rekey_interval: Some(Duration::from_secs(1800)),
                ike_rekey_interval: Some(Duration::from_secs(1500)),
                rekey_jitter: Duration::from_secs(30),
                startup_timeout: Duration::from_secs(75),
            },
            move |challenge| async move {
                let result = if self.access_isim {
                    lease
                        .authenticate_ims(&challenge.rand, &challenge.autn)
                        .await
                } else {
                    lease
                        .authenticate_ike(&challenge.rand, &challenge.autn)
                        .await
                };
                result.map_err(NativeAkaError::Lease)
            },
            ike_stop_receiver,
        );
        tokio::pin!(startup);
        let result = tokio::select! {
            result = &mut startup => result,
            _ = heartbeat.wait_lost() => {
                let _ = ike_stop.send(true);
                // If IKE completed just before the lease failure, retain the
                // tunnel long enough for its owner to send the ordered DELETE.
                let startup_result = startup.await;
                actor_stop_forwarder.abort();
                let _ = actor_stop_forwarder.await;
                return match startup_result {
                    Ok(tunnel) => Ok(NativeTunnelOwner {
                        session_key: self.session_key.clone(),
                        tunnel,
                        lease_heartbeat: heartbeat,
                        ike_stop,
                    }),
                    Err(_) => {
                        heartbeat.stop().await;
                        Err(NativeTunnelStartError::LeaseLost)
                    }
                };
            }
        };
        actor_stop_forwarder.abort();
        let _ = actor_stop_forwarder.await;
        match result {
            Ok(tunnel) => Ok(NativeTunnelOwner {
                session_key: self.session_key.clone(),
                tunnel,
                lease_heartbeat: heartbeat,
                ike_stop,
            }),
            Err(error) => {
                heartbeat.stop().await;
                Err(NativeTunnelStartError::Startup(error))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeImsEndpointSelectionError {
    MissingTunnelPcscf,
    NoMatchingTunnelAddress,
    InvalidCandidates,
}

#[cfg(test)]
fn select_ims_runtime_endpoints(
    profile: &NativeImsRegisterProfile,
    tunnel: &TunnelConfiguration,
) -> Result<(IpAddr, SocketAddr), NativeImsEndpointSelectionError> {
    let candidate = registration_candidates::select(
        profile,
        tunnel,
        proletarius_vowifi::RequestedIpStack::Dual,
    )?[0];
    Ok((candidate.local_ip, candidate.registrar))
}

fn tunnel_local_address(tunnel: &TunnelConfiguration, ipv4: bool) -> Option<IpAddr> {
    if ipv4 {
        tunnel.ipv4_addresses.first().copied().map(IpAddr::V4)
    } else {
        tunnel
            .ipv6_addresses
            .first()
            .map(|(address, _)| IpAddr::V6(*address))
    }
}

struct NativeSipTransactionIds {
    call_id: String,
    from_tag: String,
    via_branch: String,
}

impl NativeSipTransactionIds {
    fn generate() -> Result<Self, ()> {
        let mut entropy = [0_u8; 40];
        if getrandom::fill(&mut entropy).is_err() {
            entropy.zeroize();
            return Err(());
        }
        let call_id = format!("{}@proletarius", hex(&entropy[..16]));
        let from_tag = hex(&entropy[16..24]);
        let via_branch = format!("z9hG4bK{}", hex(&entropy[24..]));
        entropy.zeroize();
        Ok(Self {
            call_id,
            from_tag,
            via_branch,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn ims_sim_answer(mut result: SimAkaResult) -> SimAnswer {
    match &mut result {
        SimAkaResult::Success { res, ck, ik } => SimAnswer::Success {
            res: Zeroizing::new(std::mem::take(res)),
            ck: Zeroizing::new(*ck),
            ik: Zeroizing::new(*ik),
        },
        SimAkaResult::SynchronizationFailure { auts } => {
            SimAnswer::SynchronizationFailure(Zeroizing::new(*auts))
        }
        SimAkaResult::AuthenticationRejected => SimAnswer::Rejected,
    }
}

enum NativeImsRegistrationError {
    SessionMismatch,
    TransportClosed,
    Endpoint(NativeImsEndpointSelectionError),
    Entropy,
    Registration(Box<NativeProfileRegistrationFailure<NativeAkaError>>),
    CancellationOwner(Result<(), proletarius_vowifi::registration_coordinator::CleanupReport>),
}

async fn finish_owned_registration(
    registration: impl Future<Output = Result<RegisteredImsSession, NativeImsRegistrationError>>,
    stopped: impl Future<Output = ()>,
    owner: tokio::sync::watch::Sender<bool>,
) -> Result<RegisteredImsSession, NativeImsRegistrationError> {
    let mut session = finish_registration_after_stop(registration, stopped, owner.clone()).await?;
    if session.retain_cancellation_owner(owner).is_err() {
        // Even an invariant violation cannot abandon the established sockets.
        return Err(NativeImsRegistrationError::CancellationOwner(
            session.shutdown().await,
        ));
    }
    Ok(session)
}

async fn finish_registration_after_stop<T>(
    registration: impl Future<Output = T>,
    stopped: impl Future<Output = ()>,
    stop: tokio::sync::watch::Sender<bool>,
) -> T {
    tokio::pin!(registration);
    tokio::select! {
        result = &mut registration => result,
        _ = stopped => {
            stop.send_replace(true);
            // The coordinator owns rollback and its exact failure report.
            registration.await
        }
    }
}

async fn wait_registration_stop(
    tcp: &TcpControl,
    mut stop: tokio::sync::watch::Receiver<bool>,
    mut lease: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if *stop.borrow()
            || stop.has_changed().is_err()
            || *lease.borrow()
            || lease.has_changed().is_err()
        {
            return;
        }
        tokio::select! {
            _ = tcp.closed() => return,
            _ = stop.changed() => {},
            _ = lease.changed() => {},
        }
    }
}

fn forward_startup_stop(
    mut actor_stop: tokio::sync::watch::Receiver<bool>,
    ike_stop: tokio::sync::watch::Sender<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if *actor_stop.borrow() || actor_stop.has_changed().is_err() {
                let _ = ike_stop.send(true);
                return;
            }
            if actor_stop.changed().await.is_err() {
                let _ = ike_stop.send(true);
                return;
            }
        }
    })
}

/// Owns the authenticated tunnel and the reader lease heartbeat together.
/// Dropping one without the other is impossible; the future keyed session
/// actor must watch `lease_lost_receiver` and orderly-stop IMS before IKE.
#[allow(dead_code, reason = "consumed by the gated keyed native session actor")]
struct NativeTunnelOwner {
    session_key: crate::native_vowifi_supervisor::ReaderSessionKey,
    tunnel: NativeTunnel,
    lease_heartbeat: card::Heartbeat,
    ike_stop: tokio::sync::watch::Sender<bool>,
}

impl NativeTunnelOwner {
    /// Compose the live IMS socket and IPsec controls with this exact
    /// authenticated tunnel. The network task runs independently; its returned
    /// control retains the lease heartbeat until startup cleanup or all
    /// registered IMS consumers have joined, including on network failure.
    #[allow(dead_code, reason = "consumed by the gated keyed native session actor")]
    fn start_ims_network(
        self,
        probe_interval: Duration,
        retry_interval: Duration,
        attempts: u8,
    ) -> NativeImsNetworkControl {
        let configuration = self.tunnel.configuration().clone();
        let traffic_probe = self.tunnel.traffic_probe();
        let lease_lost = self.lease_heartbeat.lost_receiver();
        let ike_stop = self.ike_stop.clone();
        let ike_stop_receiver = self.ike_stop.subscribe();
        let (wake_tx, wake_rx) = tokio::sync::watch::channel(0_u64);
        let (tcp, security, udp, runtime) = ImsNetworkRuntime::new_with_udp(wake_tx);
        let retirement = runtime.retirement();
        let NativeTunnelOwner {
            session_key,
            mut tunnel,
            lease_heartbeat,
            ike_stop: _,
        } = self;
        let eap_direct_material = tunnel.take_eap_direct_auth_material();
        let network_task = tokio::spawn(async move {
            let result = tunnel
                .run_network_with_runtime(
                    ike_stop_receiver,
                    wake_rx,
                    probe_interval,
                    retry_interval,
                    attempts,
                    runtime,
                )
                .await;
            result
        });
        NativeImsNetworkControl {
            lease_heartbeat,
            session_key,
            configuration,
            tcp,
            security,
            udp,
            ike_stop,
            lease_lost,
            eap_direct_material,
            retirement,
            network_task,
            traffic_probe,
        }
    }

    #[allow(
        dead_code,
        reason = "used by the single network task that owns this IKE tunnel"
    )]
    fn ike_stop_receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.ike_stop.subscribe()
    }

    #[allow(dead_code, reason = "observed by the keyed native session actor")]
    fn lease_lost_receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.lease_heartbeat.lost_receiver()
    }

    #[allow(
        dead_code,
        reason = "sent after RegisteredImsSession::shutdown by the keyed actor"
    )]
    fn request_ike_stop(&self) {
        let _ = self.ike_stop.send(true);
    }

    /// Run protected IMS cleanup independently of the caller's task, and only
    /// then release IKE to send its protected DELETE. Dropping the returned
    /// JoinHandle detaches the cleanup task; it does not cancel the ordering
    /// barrier if the keyed actor itself is being torn down.
    #[allow(dead_code, reason = "consumed by the gated keyed native session actor")]
    fn shutdown_registered_ims(
        &self,
        session: proletarius_vowifi::registration_coordinator::RegisteredImsSession,
    ) -> tokio::task::JoinHandle<
        Result<(), proletarius_vowifi::registration_coordinator::CleanupReport>,
    > {
        spawn_ims_shutdown_before_ike(self.ike_stop.clone(), session.shutdown())
    }
}

/// Control handles bound to the exact network task created from one
/// `NativeTunnelOwner`. Registration cannot accidentally receive controls
/// belonging to a different tunnel or session generation.
#[allow(dead_code, reason = "consumed by the gated keyed native session actor")]
struct NativeImsNetworkControl {
    // Keep the physical loss producer alive until IMS/consumer owners join.
    // A normal network exit must not masquerade as a lost reader merely by
    // closing the false-valued heartbeat watch before those owners observe it.
    lease_heartbeat: card::Heartbeat,
    retirement: proletarius_vowifi::ims_network_runtime::Retirement,
    session_key: crate::native_vowifi_supervisor::ReaderSessionKey,
    configuration: TunnelConfiguration,
    tcp: TcpControl,
    security: SecurityControl,
    udp: proletarius_vowifi::netstack_udp::UdpControl,
    ike_stop: tokio::sync::watch::Sender<bool>,
    lease_lost: tokio::sync::watch::Receiver<bool>,
    eap_direct_material: Option<proletarius_vowifi::EapDirectAuthMaterial>,
    network_task: tokio::task::JoinHandle<Result<(), NativeNetworkError<Infallible>>>,
    traffic_probe: proletarius_vowifi::TunnelTrafficProbe,
}

/// Setup facts supplied by the registered profile/daemon owner; none are learned
/// from an incoming SMS. Identity and routes are authorized by REGISTER below.
#[allow(dead_code, reason = "native production startup remains gated")]
struct NativeSmsSetup {
    archive: crate::ims_sms_transactions::NativeSmsArchive,
    archive_warning_observer: Option<crate::native_sms_consumer::ArchiveWarningObserver>,
    data_root: std::path::PathBuf,
    /// `None` uses the lease-bound identity; `Some("")` preserves sources
    /// which intentionally have no subscription identifier.
    subscription_id: Option<String>,
    configured_identity: String,
    fallback_routes: Vec<String>,
    access_network_info: Option<String>,
    minimum_cseq: u32,
    transaction_service: crate::ims_sms_transactions::Service,
    transaction_session_id: String,
}

/// Complete ordinary-profile consumer inputs; voice is mandatory here. N-wait
/// can still use the distinct SMS-only worker without changing that contract.
struct NativeRegisteredSetup {
    sms: NativeSmsSetup,
    smsc: zeroize::Zeroizing<String>,
    sms_target: zeroize::Zeroizing<String>,
    calls: crate::native_ims_calls::Config,
    call_facts: crate::call_facts::NativeHistory,
}

struct NativeRegisteredWorkerExit {
    // Frozen before intentionally stopping the heartbeat. Network cancellation
    // can make both consumers report Requested even if physical loss was true.
    lease_lost: bool,
    local_retired: bool,
    actor: Result<crate::native_ims_actor::Exit, tokio::task::JoinError>,
    network: Result<Result<(), NativeNetworkError<Infallible>>, tokio::task::JoinError>,
    consumer: Result<crate::native_registered_session::Exit, tokio::task::JoinError>,
    call_archive: Result<(), String>,
    call_archive_retired: bool,
}

impl NativeRegisteredWorkerExit {
    fn clean(&self) -> bool {
        self.actor.is_ok()
            && self.local_retired
            && self
                .consumer
                .as_ref()
                .is_ok_and(|exit| exit.media_cleanup.is_ok())
            && self.call_archive_retired
    }
}

/// Drain media and the registered SIP association while the IKE/network task
/// can still acknowledge actual socket closure. Parent cancellation never
/// aborts either owner. Network failure retains both cleanup outcomes.
async fn join_registered_consumers(
    mut actor: tokio::task::JoinHandle<crate::native_ims_actor::Exit>,
    mut consumer: tokio::task::JoinHandle<crate::native_registered_session::Exit>,
    mut network: tokio::task::JoinHandle<Result<(), NativeNetworkError<Infallible>>>,
    cancel: tokio::sync::watch::Sender<bool>,
    ike_stop: tokio::sync::watch::Sender<bool>,
    facts: crate::call_facts::NativeHistory,
    retirement: proletarius_vowifi::ims_network_runtime::Retirement,
    lease_lost: tokio::sync::watch::Receiver<bool>,
) -> NativeRegisteredWorkerExit {
    let (actor, consumer, network) = tokio::select! {
        result = &mut actor => {
            cancel.send_replace(true);
            let consumer = consumer.await;
            ike_stop.send_replace(true);
            (result, consumer, network.await)
        }
        result = &mut consumer => {
            cancel.send_replace(true);
            let actor = actor.await;
            ike_stop.send_replace(true);
            (actor, result, network.await)
        }
        result = &mut network => {
            cancel.send_replace(true);
            (actor.await, consumer.await, result)
        }
    };
    let call_archive = retire_call_archive(&facts).await;
    NativeRegisteredWorkerExit {
        lease_lost: *lease_lost.borrow() || lease_lost.has_changed().is_err(),
        local_retired: retirement.confirmed(),
        actor,
        network,
        consumer,
        call_archive,
        call_archive_retired: facts.worker_retired(),
    }
}

/// Closing admission may report an archive error, but the worker must still be
/// joined before its retirement receipt can permit releasing the physical card.
async fn retire_call_archive(facts: &crate::call_facts::NativeHistory) -> Result<(), String> {
    let retired = facts.retire(time::OffsetDateTime::now_utc());
    let drained = facts.drain().await;
    match (retired, drained) {
        (Err(retire), Err(drain)) if retire != drain => {
            Err(format!("{retire}; call archive drain: {drain}"))
        }
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn validate_registered_sms_start(
    permit: &crate::native_vowifi_supervisor::NativeSessionPermit,
    expected_key: &crate::native_vowifi_supervisor::ReaderSessionKey,
    lease_lost: &tokio::sync::watch::Receiver<bool>,
) -> Result<(), &'static str> {
    if !permit.matches_key(expected_key) {
        return Err("native_sms_session_mismatch");
    }
    if *permit.stop_signal().borrow() {
        return Err("native_sms_session_stopped");
    }
    if *lease_lost.borrow() || lease_lost.has_changed().is_err() {
        return Err("native_sms_lease_lost");
    }
    Ok(())
}

type NativeRegisteredActorResult = (
    crate::native_ims_actor::Exit,
    Result<Result<(), NativeNetworkError<Infallible>>, tokio::task::JoinError>,
    Option<crate::native_vowifi_supervisor::CompletionDisposition>,
);
#[allow(dead_code, reason = "gated native combined worker result")]
struct NativeSmsWorkerExit {
    actor: Result<NativeRegisteredActorResult, tokio::task::JoinError>,
    consumer:
        Result<Result<crate::native_sms_consumer::LoopExit, tokio::task::JoinError>, &'static str>,
}

/// Observe network termination while IMS is alive, preserving both cleanup results.
async fn join_registered_network<A, N>(
    actor: impl Future<Output = A>,
    mut network: tokio::task::JoinHandle<N>,
    actor_stop: tokio::sync::watch::Sender<bool>,
    ike_stop: tokio::sync::watch::Sender<bool>,
) -> (A, Result<N, tokio::task::JoinError>) {
    tokio::pin!(actor);
    tokio::select! {
        result = &mut network => {
            actor_stop.send_replace(true);
            let actor = actor.await;
            ike_stop.send_replace(true);
            (actor, result)
        }
        result = &mut actor => {
            // IMS shutdown has completed before requesting IKE shutdown.
            ike_stop.send_replace(true);
            (result, network.await)
        }
    }
}

fn registered_cleanup_complete(
    exit: &crate::native_ims_actor::Exit,
    network: &Result<Result<(), NativeNetworkError<Infallible>>, tokio::task::JoinError>,
) -> bool {
    exit.cleanup.is_ok() && matches!(network, Ok(Ok(())))
}

async fn finish_registered_lease<E>(
    worker: &mut NativeSmsWorkerExit,
    release: impl Future<Output = Result<(), E>>,
    owner: &std::sync::Weak<
        std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
    >,
) -> Option<Result<(), E>> {
    let Ok((exit, network, completion)) = &mut worker.actor else {
        return None;
    };
    if !registered_cleanup_complete(exit, network) {
        return None;
    }
    let (released, disposition) = release_with_claim(&exit.permit, release, owner).await?;
    *completion = disposition;
    Some(released)
}

async fn release_with_claim<E>(
    permit: &crate::native_vowifi_supervisor::NativeSessionPermit,
    release: impl Future<Output = Result<(), E>>,
    owner: &std::sync::Weak<
        std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
    >,
) -> Option<(
    Result<(), E>,
    Option<crate::native_vowifi_supervisor::CompletionDisposition>,
)> {
    if let Some(owner) = owner.upgrade() {
        if !owner.lock().ok()?.claim_recovery(permit) {
            return None;
        }
    }
    let released = release.await;
    let disposition = owner.upgrade().and_then(|owner| {
        let mut owner = owner.lock().ok()?;
        if released.is_ok() {
            Some(owner.worker_finished(permit))
        } else {
            owner.recovery_failed(permit);
            None
        }
    });
    Some((released, disposition))
}

async fn join_registered_sms_workers(
    actor: crate::native_ims_actor::Task<NativeRegisteredActorResult>,
    consumer: Result<tokio::task::JoinHandle<crate::native_sms_consumer::LoopExit>, &'static str>,
    cancel: tokio::sync::watch::Sender<bool>,
    completion_owner: std::sync::Weak<
        std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
    >,
) -> NativeSmsWorkerExit {
    let mut actor = actor.finish().await;
    cancel.send_replace(true);
    let consumer = match consumer {
        Ok(task) => Ok(task.await),
        Err(code) => Err(code),
    };
    if let Ok((exit, network, completion)) = actor.as_mut()
        && registered_cleanup_complete(exit, network)
    {
        *completion = completion_owner.upgrade().and_then(|owner| {
            owner
                .lock()
                .ok()
                .map(|mut owner| owner.worker_finished(&exit.permit))
        });
    }
    NativeSmsWorkerExit { actor, consumer }
}

impl NativeImsNetworkControl {
    /// Transfers the same IKE generation's verified RES/RAND/AUTN exactly
    /// once to the initial REGISTER compiler.
    #[allow(dead_code, reason = "consumed by the gated native IMS REGISTER owner")]
    fn take_eap_direct_auth_material(
        &mut self,
    ) -> Option<proletarius_vowifi::EapDirectAuthMaterial> {
        self.eap_direct_material.take()
    }

    /// Assembles the actual voice, RTP/audio and SMS owners around the single
    /// registered actor. Prepared identities and the network registry must all
    /// belong to this exact reader lease and supervisor generation.
    fn registered_consumers(
        &self,
        permit: &crate::native_vowifi_supervisor::NativeSessionPermit,
        session: &RegisteredImsSession,
        setup: NativeRegisteredSetup,
        commands: tokio::sync::mpsc::Sender<crate::native_ims_actor::Command>,
    ) -> Result<
        (
            crate::native_registered_session::Consumers,
            crate::call_facts::NativeHistory,
        ),
        &'static str,
    > {
        self.registered_profile_consumers(permit, session, setup, commands, false)
    }

    fn registered_profile_consumers(
        &self,
        permit: &crate::native_vowifi_supervisor::NativeSessionPermit,
        session: &RegisteredImsSession,
        setup: NativeRegisteredSetup,
        commands: tokio::sync::mpsc::Sender<crate::native_ims_actor::Command>,
        sms_only: bool,
    ) -> Result<
        (
            crate::native_registered_session::Consumers,
            crate::call_facts::NativeHistory,
        ),
        &'static str,
    > {
        validate_registered_sms_start(permit, &self.session_key, &self.lease_lost)?;
        if !session.belongs_to_tcp(&self.tcp) || self.tcp.is_closed() {
            return Err("native_registered_transport_mismatch");
        }
        if setup.calls.sms_only != sms_only {
            return Err("native_session_media_policy_mismatch");
        }
        let snapshot =
            session.message_context(&setup.sms.configured_identity, &setup.sms.fallback_routes)?;
        if setup.calls.local_ip != snapshot.local_ip()
            || setup.calls.port_c != snapshot.local_port()
        {
            return Err("native_registered_call_transport_mismatch");
        }
        let transaction = std::sync::Arc::new(crate::ims_sms_transactions::NativeReportOwner::new(
            setup.sms.transaction_service,
            setup.sms.transaction_session_id,
        )?);
        let outgoing = crate::native_sms_originating::Originating::new(
            transaction.clone(),
            crate::native_sms_originating::SessionConfig {
                generation: permit.generation(),
                snapshot: &snapshot,
                minimum_cseq: setup.sms.minimum_cseq,
                smsc: &setup.smsc,
                target: &setup.sms_target,
                access_network_info: setup.sms.access_network_info.as_deref(),
            },
            commands.clone(),
            permit.stop_signal(),
            self.lease_lost.clone(),
        )?;
        let media = crate::native_ims_calls::media::Owner::new(
            permit.generation(),
            snapshot.local_ip(),
            self.udp.clone(),
        );
        let mut calls = crate::native_ims_calls::Service::new(
            permit.generation(),
            setup.calls,
            commands.clone(),
            media,
        )
        .map_err(|error| error.code)?;
        let facts = setup.call_facts.clone();
        calls.set_observer(std::sync::Arc::new(move |generation, snapshot| {
            if snapshot.call_id.is_empty() {
                return;
            }
            // NativeSink retains the exact failed head and exposes an archive
            // error if bounded admission fails; no direct best-effort write.
            let _ = facts.try_push(crate::call_facts::NativeFact {
                owner_id: format!("native-reader-ims-{}", generation.value()),
                owner_epoch: generation.value(),
                at: time::OffsetDateTime::now_utc(),
                action: crate::call_facts::NativeFactAction::Ims {
                    incoming: snapshot.direction == "incoming",
                    number: snapshot.peer,
                    call_id: snapshot.call_id,
                    state: snapshot.state,
                },
            });
        }));
        let incoming = crate::native_sms_consumer::RegisteredConsumer::new(
            crate::native_sms_consumer::Consumer::new(
                setup.sms.data_root,
                setup.sms.subscription_id.unwrap_or_default(),
                permit.generation(),
                commands,
                permit.stop_signal(),
                self.lease_lost.clone(),
                transaction,
            )
            .with_archive(setup.sms.archive)
            .with_archive_warning_observer(setup.sms.archive_warning_observer),
            snapshot,
            setup.sms.access_network_info,
            setup.sms.minimum_cseq,
        );
        let consumers = crate::native_registered_session::Consumers::new(incoming, outgoing, calls)
            .map_err(|error| error.code)?
            .with_heartbeat_health(session.heartbeat_health());
        Ok((consumers, setup.call_facts))
    }

    fn spawn_registered_consumers(
        self,
        permit: crate::native_vowifi_supervisor::NativeSessionPermit,
        session: RegisteredImsSession,
        consumers: crate::native_registered_session::Consumers,
        commands: tokio::sync::mpsc::Sender<crate::native_ims_actor::Command>,
        command_input: tokio::sync::mpsc::Receiver<crate::native_ims_actor::Command>,
        events: tokio::sync::mpsc::Sender<crate::native_ims_actor::Event>,
        event_input: tokio::sync::mpsc::Receiver<crate::native_ims_actor::Event>,
        controls: tokio::sync::mpsc::Receiver<crate::native_registered_session::Control>,
        notices: tokio::sync::mpsc::Sender<crate::native_registered_session::Notice>,
        facts: crate::call_facts::NativeHistory,
    ) -> crate::native_ims_actor::Task<NativeRegisteredWorkerExit> {
        let cancel = permit.cancellation_sender();
        let actor_cancel = cancel.clone();
        crate::native_ims_actor::Task::spawn(cancel, async move {
            let consumer = tokio::spawn(consumers.run(event_input, commands, controls, notices));
            let lease_lost = self.lease_lost.clone();
            let key = self.session_key;
            let actor = tokio::spawn(async move {
                crate::native_ims_actor::run(
                    proletarius_vowifi::sip_session_runtime::RegisteredSipRuntime::new_with_heartbeat(session),
                    &key,
                    permit,
                    self.lease_lost,
                    command_input,
                    events,
                )
                .await
            });
            let result = join_registered_consumers(
                actor,
                consumer,
                self.network_task,
                actor_cancel,
                self.ike_stop,
                facts,
                self.retirement,
                lease_lost,
            )
            .await;
            self.lease_heartbeat.stop().await;
            result
        })
    }

    /// Parent cancellation signals the same generation, while the detached-safe
    /// task waits for both consumer exit and IMS -> IKE -> network cleanup.
    #[allow(dead_code, reason = "native combined startup remains gated")]
    fn spawn_registered_sms(
        self,
        permit: crate::native_vowifi_supervisor::NativeSessionPermit,
        session: RegisteredImsSession,
        supervisor: std::sync::Arc<
            std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
        >,
        setup: NativeSmsSetup,
        notices: tokio::sync::mpsc::Sender<crate::native_sms_consumer::LoopNotice>,
    ) -> crate::native_ims_actor::Task<NativeSmsWorkerExit> {
        self.spawn_registered_sms_with_completion(permit, session, supervisor, setup, notices, true)
    }

    fn spawn_registered_sms_with_completion(
        self,
        permit: crate::native_vowifi_supervisor::NativeSessionPermit,
        session: RegisteredImsSession,
        supervisor: std::sync::Arc<
            std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
        >,
        setup: NativeSmsSetup,
        notices: tokio::sync::mpsc::Sender<crate::native_sms_consumer::LoopNotice>,
        finalize: bool,
    ) -> crate::native_ims_actor::Task<NativeSmsWorkerExit> {
        let cancel = permit.cancellation_sender();
        let worker_cancel = cancel.clone();
        crate::native_ims_actor::Task::spawn(cancel, async move {
            let (commands, command_input, events, event_input) =
                crate::native_ims_actor::channels();
            let prepared = self.registered_sms_consumer(&permit, &session, setup, commands);
            let consumer = match prepared {
                Ok(consumer) => Ok(tokio::spawn(consumer.run(event_input, notices))),
                Err(code) => {
                    worker_cancel.send_replace(true);
                    drop(event_input);
                    Err(code)
                }
            };
            // Even preparation failure owns a live registered socket and tunnel.
            // Feed them through the normal stopped actor cleanup, never abandon.
            let completion_owner = if finalize {
                std::sync::Arc::downgrade(&supervisor)
            } else {
                std::sync::Weak::new()
            };
            let actor = self.spawn_registered_actor_with_completion(
                permit,
                session,
                supervisor,
                command_input,
                events,
                false,
            );
            join_registered_sms_workers(actor, consumer, worker_cancel, completion_owner).await
        })
    }

    #[allow(dead_code, reason = "consumed by gated native registered startup")]
    fn registered_sms_consumer(
        &self,
        permit: &crate::native_vowifi_supervisor::NativeSessionPermit,
        session: &RegisteredImsSession,
        setup: NativeSmsSetup,
        commands: tokio::sync::mpsc::Sender<crate::native_ims_actor::Command>,
    ) -> Result<crate::native_sms_consumer::RegisteredConsumer, &'static str> {
        validate_registered_sms_start(permit, &self.session_key, &self.lease_lost)?;
        if !session.belongs_to_tcp(&self.tcp) {
            return Err("native_sms_transport_mismatch");
        }
        if self.tcp.is_closed() {
            return Err("native_sms_transport_closed");
        }
        let snapshot =
            session.message_context(&setup.configured_identity, &setup.fallback_routes)?;
        let report_owner = crate::ims_sms_transactions::NativeReportOwner::new(
            setup.transaction_service,
            setup.transaction_session_id,
        )?;
        let consumer = crate::native_sms_consumer::Consumer::new(
            setup.data_root,
            setup.subscription_id.unwrap_or_default(),
            permit.generation(),
            commands,
            permit.stop_signal(),
            self.lease_lost.clone(),
            report_owner,
        )
        .with_archive(setup.archive)
        .with_archive_warning_observer(setup.archive_warning_observer);
        Ok(crate::native_sms_consumer::RegisteredConsumer::new(
            consumer,
            snapshot,
            setup.access_network_info,
            setup.minimum_cseq,
        ))
    }

    /// Return a detached-safe owner task. Dropping its JoinHandle does not
    /// abort IMS cleanup or let a replacement claim this generation early.
    #[allow(
        dead_code,
        reason = "native actor remains gated pending live SMS/call handlers"
    )]
    fn spawn_registered_actor(
        self,
        permit: crate::native_vowifi_supervisor::NativeSessionPermit,
        session: RegisteredImsSession,
        supervisor: std::sync::Arc<
            std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
        >,
        commands: tokio::sync::mpsc::Receiver<crate::native_ims_actor::Command>,
        events: tokio::sync::mpsc::Sender<crate::native_ims_actor::Event>,
    ) -> crate::native_ims_actor::Task<(
        crate::native_ims_actor::Exit,
        Result<Result<(), NativeNetworkError<Infallible>>, tokio::task::JoinError>,
        Option<crate::native_vowifi_supervisor::CompletionDisposition>,
    )> {
        self.spawn_registered_actor_with_completion(
            permit, session, supervisor, commands, events, true,
        )
    }

    fn spawn_registered_actor_with_completion(
        self,
        permit: crate::native_vowifi_supervisor::NativeSessionPermit,
        session: RegisteredImsSession,
        supervisor: std::sync::Arc<
            std::sync::Mutex<crate::native_vowifi_supervisor::NativeVoWiFiSupervisor>,
        >,
        commands: tokio::sync::mpsc::Receiver<crate::native_ims_actor::Command>,
        events: tokio::sync::mpsc::Sender<crate::native_ims_actor::Event>,
        finalize: bool,
    ) -> crate::native_ims_actor::Task<(
        crate::native_ims_actor::Exit,
        Result<Result<(), NativeNetworkError<Infallible>>, tokio::task::JoinError>,
        Option<crate::native_vowifi_supervisor::CompletionDisposition>,
    )> {
        if !session.belongs_to_tcp(&self.tcp) || self.tcp.is_closed() {
            permit.cancellation_sender().send_replace(true);
        }
        let supervisor = std::sync::Arc::downgrade(&supervisor);
        let cancel = permit.cancellation_sender();
        let owner_cancel = cancel.clone();
        crate::native_ims_actor::Task::spawn(cancel, async move {
            let running = supervisor.upgrade().and_then(|owner| {
                owner
                    .lock()
                    .ok()
                    .and_then(|mut owner| owner.mark_running(&permit).ok())
            });
            if running.is_none() {
                owner_cancel.send_replace(true);
            }
            let actor = crate::native_ims_actor::run(
                proletarius_vowifi::sip_session_runtime::RegisteredSipRuntime::new(session),
                &self.session_key,
                permit,
                self.lease_lost.clone(),
                commands,
                events,
            );
            let (exit, network) =
                join_registered_network(actor, self.network_task, owner_cancel, self.ike_stop)
                    .await;
            self.lease_heartbeat.stop().await;
            // Completion does not depend on anyone awaiting the task handle.
            let completion = if finalize && registered_cleanup_complete(&exit, &network) {
                supervisor.upgrade().and_then(|owner| {
                    owner
                        .lock()
                        .ok()
                        .map(|mut owner| owner.worker_finished(&exit.permit))
                })
            } else {
                None
            };
            (exit, network, completion)
        })
    }
    fn configuration(&self) -> &TunnelConfiguration {
        &self.configuration
    }

    fn registration_services<'a>(
        &'a self,
        events: &'a tokio::sync::mpsc::Sender<SipStreamItem>,
    ) -> RegistrationServices<'a> {
        RegistrationServices::new(&self.tcp, &self.security, events)
    }

    #[allow(dead_code, reason = "observed by the gated keyed session actor")]
    fn lease_lost_receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.lease_lost.clone()
    }

    #[allow(
        dead_code,
        reason = "sent after IMS cleanup by the keyed session actor"
    )]
    fn request_ike_stop(&self) {
        let _ = self.ike_stop.send(true);
    }

    #[allow(dead_code, reason = "consumed by the gated keyed session actor")]
    fn shutdown_registered_ims(
        &self,
        session: RegisteredImsSession,
    ) -> tokio::task::JoinHandle<
        Result<(), proletarius_vowifi::registration_coordinator::CleanupReport>,
    > {
        spawn_ims_shutdown_before_ike(self.ike_stop.clone(), session.shutdown())
    }

    #[allow(dead_code, reason = "consumed by the gated keyed native session actor")]
    async fn finish_after_registered_shutdown(
        self,
        session: RegisteredImsSession,
    ) -> (
        Result<
            Result<(), proletarius_vowifi::registration_coordinator::CleanupReport>,
            tokio::task::JoinError,
        >,
        Result<Result<(), NativeNetworkError<Infallible>>, tokio::task::JoinError>,
    ) {
        let cleanup = self.shutdown_registered_ims(session).await;
        let network = self.network_task.await;
        self.lease_heartbeat.stop().await;
        (cleanup, network)
    }

    #[allow(dead_code, reason = "consumed by the gated keyed native session actor")]
    async fn stop_before_registration(
        self,
    ) -> Result<Result<(), NativeNetworkError<Infallible>>, tokio::task::JoinError> {
        self.request_ike_stop();
        let result = self.network_task.await;
        self.lease_heartbeat.stop().await;
        result
    }
}

fn spawn_ims_shutdown_before_ike<T, F>(
    ike_stop: tokio::sync::watch::Sender<bool>,
    shutdown: F,
) -> tokio::task::JoinHandle<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    tokio::spawn(async move {
        let result = shutdown.await;
        let _ = ike_stop.send(true);
        result
    })
}

#[derive(Debug)]
#[allow(dead_code)] // Returned by the gated native tunnel owner.
enum NativeAkaError {
    Lease(proletarius_domain::BackendFailure),
}

#[derive(Debug)]
#[allow(dead_code)] // Returned by the gated daemon owner.
enum NativeTunnelStartError {
    Stopped,
    LeaseLost,
    CertificateValidationUnavailable,
    Transport(DatagramTransportError),
    Startup(TunnelStartupError<NativeAkaError>),
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    go_socket_path: &Path,
    app_token: Option<&str>,
    reader_pcsc: crate::reader_pcsc::Owner,
    reader_gate: std::sync::Arc<crate::reader_reservation::Gate>,
) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        REQUEST_LIMIT,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(body) => body,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => {
            return static_failure(
                StatusCode::BAD_REQUEST,
                "invalid_native_session_request",
                false,
            );
        }
        Err(crate::http_server::BodyReadError::Timeout) => {
            return static_failure(StatusCode::REQUEST_TIMEOUT, "request_timeout", true);
        }
    };
    let Ok(request) = serde_json::from_slice::<PlanRequest>(&body) else {
        return static_failure(
            StatusCode::BAD_REQUEST,
            "invalid_native_session_request",
            false,
        );
    };
    if let Err(error) = validate_request(&request) {
        return failure(error);
    }
    let lease = match crate::reader_session_lease::NativeReaderSessionLease::acquire(
        go_socket_path,
        app_token,
        request.reader.trim(),
        reader_gate,
        reader_pcsc,
    )
    .await
    {
        Ok(lease) => lease,
        Err(_) => {
            return static_failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_session_lease_unavailable",
                true,
            );
        }
    };
    let plan = match prepare_context(request, lease, false).await {
        Ok(plan) => plan,
        Err((error, lease)) => {
            if lease.release().await.is_err() {
                return static_failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "reader_session_lease_release_failed",
                    true,
                );
            }
            return failure(error);
        }
    };
    let readiness = native_session_owner_readiness();
    let response = json!({
        "ok": true,
        "reader": &plan.reader,
        "reader_generation": plan.reader_generation,
        "native_actor_ready": readiness.ready(),
        "owner_missing": readiness.missing(),
        "mcc": &plan.mcc,
        "mnc": &plan.mnc,
        "carrier_id": &plan.carrier_id,
        "epdg": &plan.config.epdg,
        "epdg_port": plan.config.epdg_port,
        "epdg_source": &plan.config.epdg_source,
        "apn": &plan.config.apn,
        "ip_stack": ip_stack_name(plan.config.ip_stack),
        "ike_proposals": &plan.config.proposals.ike,
        "esp_proposals": &plan.config.proposals.esp,
        "transport": plan.transport.mode.as_str(),
        "proxy_source": plan.transport.proxy_source,
        "proxy_error": &plan.transport.proxy_error,
        "ims": {
            "domain": plan.ims.domain(),
            "realm": plan.ims.realm(),
            "phone_context": plan.ims.phone_context(),
            "configured_pcscf": plan.ims.configured_pcscf().map(|address| address.to_string()),
            "transport": plan.ims.transport().as_str(),
            "local_port": plan.ims.local_port(),
            "tcp_keepalive_seconds": plan.ims.tcp_keepalive_seconds(),
            "options_ping_interval_seconds": plan.ims.options_ping_interval_seconds(),
            "register_template": plan.ims.template().id(),
            "register_expires_seconds": plan.ims.template().expires_seconds(),
            "sec_agree": ims_sec_agree_name(plan.ims.template().sec_agree()),
            "strict_security_server_offer": plan.ims.template().strict_security_server_offer(),
            "device_identity": {
                "mode": plan.ims.device_identity_mode(),
                "enabled": plan.device_identity.enabled(),
            },
        },
    });
    if plan.card.release().await.is_err() {
        return static_failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_session_lease_release_failed",
            true,
        );
    }
    crate::json_response(StatusCode::OK, &response)
}

/// This stays fixed until all owner gates are backed by the same real native
/// actor. Do not replace it with an environment flag: that would let a
/// diagnostic request violate Go's live reader ownership boundary.
const fn native_session_owner_readiness() -> NativeOwnerReadiness {
    NativeOwnerReadiness::current()
}

#[allow(dead_code)]
fn native_session_owner_not_ready(readiness: NativeOwnerReadiness) -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::CONFLICT,
        &json!({
            "error": "native_session_owner_not_ready",
            "code": "native_session_owner_not_ready",
            "retryable": false,
            "missing": readiness.missing(),
        }),
    )
}

async fn prepare(
    request: PlanRequest,
    lease: crate::reader_session_lease::NativeReaderSessionLease,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<NativeSessionContext, (PlanError, SessionCard)> {
    prepare_card_context_with_stop(
        request,
        SessionCard::leased(Arc::new(lease)),
        true,
        Some(stop),
    )
    .await
}

async fn prepare_context(
    request: PlanRequest,
    lease: crate::reader_session_lease::NativeReaderSessionLease,
    live: bool,
) -> Result<NativeSessionContext, (PlanError, SessionCard)> {
    prepare_card_context(request, SessionCard::leased(Arc::new(lease)), live).await
}

pub(crate) async fn prepare_card_context(
    request: PlanRequest,
    card: SessionCard,
    live: bool,
) -> Result<NativeSessionContext, (PlanError, SessionCard)> {
    prepare_card_context_with_stop(request, card, live, None).await
}

pub(crate) async fn prepare_card_context_with_stop(
    request: PlanRequest,
    card: SessionCard,
    live: bool,
    stop: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<NativeSessionContext, (PlanError, SessionCard)> {
    macro_rules! fail {
        ($error:expr) => {
            return Err(($error, card))
        };
    }
    let reader = request.reader.trim();
    if card.reader() != reader || card.reader_generation() == 0 {
        fail!(PlanError::ReaderIdentity("reader_identity_changed"));
    }
    let identity = card.identity();
    if identity.mnc.is_empty() {
        fail!(PlanError::Carrier);
    }
    let session_key = match crate::native_vowifi_supervisor::ReaderSessionKey::new(
        reader,
        identity.iccid.as_str(),
    ) {
        Ok(key) => key,
        Err(_) => fail!(PlanError::ReaderIdentity("reader_identity_failed")),
    };
    let sms_subscription_id =
        native_device_sms_subscription_id(identity.iccid.as_str(), identity.imsi.as_str());
    // Diagnostic preparation stays observational. Live admission shares the
    // same native first-use/update persistence lock as the public IMEI API.
    let device_source = || {
        if live {
            match crate::data_root::resolve()
                .map_err(|_| {
                    crate::reader_device_identity::PersistedDeviceIdentityError::Unreadable
                })
                .and_then(|root| {
                    crate::reader_device_identity::Store::new(&root).resolve_or_create()
                }) {
                Ok(value) => Ok(Some(Zeroizing::new(value))),
                Err(error) => return Err(PlanError::DeviceIdentityConfig(error)),
            }
        } else {
            match crate::reader_device_identity::load_persisted_imei() {
                Ok(value) => Ok(value),
                Err(error) => return Err(PlanError::DeviceIdentityConfig(error)),
            }
        }
    };
    let (overrides, configuration_path) = match load_overrides() {
        Ok(value) => value,
        Err(error) => fail!(error),
    };
    let prepared = match configuration::resolve(
        identity,
        &request.epdg,
        request.epdg_port,
        overrides,
        device_source,
    ) {
        Ok(value) => value,
        Err(error) => fail!(error),
    };
    let configuration::Resolved {
        sim,
        carrier_id,
        config,
        runtime,
        access_isim,
        ims,
        subscriber_identity,
        device_identity,
        ike_device_identity_imei,
        status: mut configuration_status,
    } = prepared;
    configuration_status["carrier_config_path"] =
        configuration::path_status_value(&configuration_path);
    let selection = if live {
        startup_transport::select(request.proxy.trim(), &config.epdg, stop).await
    } else {
        crate::transport_selection::select_for_session(request.proxy.trim(), &config.epdg)
            .await
            .map_err(PlanError::Transport)
    };
    let transport = match selection {
        Ok(value) => value,
        Err(error) => fail!(error),
    };
    // Keep only bounded, non-secret transport facts in the frozen public
    // projection. Reader and module sessions share this preparation path, so
    // every carrier exposes the same evidence without leaking a proxy address
    // or allowing a later system-proxy change to rewrite an active attempt.
    configuration_status["transport"] = json!(transport.mode.as_str());
    configuration_status["proxy_source"] = json!(transport.proxy_source);
    let reader_generation = card.reader_generation();
    Ok(NativeSessionContext {
        reader: reader.to_owned(),
        reader_generation,
        session_key,
        sms_subscription_id,
        mcc: sim.mcc,
        mnc: sim.mnc,
        carrier_id,
        config,
        runtime,
        access_isim,
        ims,
        subscriber_identity,
        device_identity,
        ike_device_identity_imei,
        configuration_status,
        transport,
        card,
    })
}

fn resolve_session_subscriber_identity(
    profile: &NativeImsRegisterProfile,
    identity: &crate::reader_session_lease::LeaseIdentity,
) -> Result<NativeImsSubscriberIdentity, NativeImsProfileError> {
    profile.resolve_subscriber_identity_from_snapshot(
        identity.imsi.as_str(),
        Some(identity.private_identity.as_str()),
        Some(identity.public_identity.as_str()),
    )
}

/// Match the archived subscription identity previously produced by Go's
/// `deviceSMSSubscriptionID`. This takes only a prepared SIM snapshot; a
/// caller-supplied ICCID/IMSI can never relabel the SMS archive.
fn native_device_sms_subscription_id(iccid: &str, imsi: &str) -> Option<String> {
    let iccid = iccid.trim();
    let imsi = imsi.trim();
    if !sms_identity_digits(iccid, 18, 22) || !sms_identity_digits(imsi, 10, 15) {
        return None;
    }
    use sha2::Digest;
    let mut digest = sha2::Sha256::new();
    digest.update(b"switch-device-subscription-v1\0");
    digest.update(iccid.as_bytes());
    digest.update(b"\0");
    digest.update(imsi.as_bytes());
    Some(format!("sub1_{}", hex(&digest.finalize())))
}

fn sms_identity_digits(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.bytes().any(|byte| byte != b'0')
}

fn resolve_native_sms_subscription_id(
    explicit: Option<String>,
    lease_bound: Option<String>,
) -> Option<String> {
    explicit.or(lease_bound)
}

/// Reject malformed caller input before acquiring a PC/SC transaction or
/// starting DNS/route/proxy discovery. Carrier-derived ePDG values are still
/// validated by `select_for_session` after the same SIM snapshot resolves.
fn validate_request(request: &PlanRequest) -> Result<(), PlanError> {
    let reader = request.reader.trim();
    if reader.is_empty()
        || reader.len() > 512
        || reader.bytes().any(|byte| byte.is_ascii_control())
        || request.epdg_port == Some(0)
    {
        return Err(PlanError::InvalidRequest);
    }
    let epdg = request.epdg.trim();
    if !epdg.is_empty() && !crate::transport_selection::valid_hostname(epdg) {
        return Err(PlanError::InvalidEpdg);
    }
    let proxy = request.proxy.trim();
    if proxy.len() > 512 || proxy.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(PlanError::InvalidRequest);
    }
    Ok(())
}

fn apply_request_overrides(
    overrides: &mut NativeTunnelOverrides,
    epdg: String,
    epdg_port: Option<u16>,
) -> Result<(), PlanError> {
    let epdg = epdg.trim();
    if !epdg.is_empty() {
        overrides.epdg = Some(epdg.to_owned());
    }
    if let Some(port) = epdg_port {
        if port == 0 {
            return Err(PlanError::InvalidRequest);
        }
        overrides.epdg_port = Some(port);
    }
    Ok(())
}

fn load_overrides() -> Result<(OverrideSet, PathBuf), PlanError> {
    let path = override_path()?;
    configuration::read_overrides(&path).map(|overrides| (overrides, path))
}
fn override_path() -> Result<PathBuf, PlanError> {
    configuration::config_path_with_default(|| {
        crate::data_root::resolve().map_err(|_| PlanError::Overrides)
    })
}

fn ip_stack_name(value: proletarius_vowifi::RequestedIpStack) -> &'static str {
    match value {
        proletarius_vowifi::RequestedIpStack::Ipv4 => "ipv4",
        proletarius_vowifi::RequestedIpStack::Ipv6 => "ipv6",
        proletarius_vowifi::RequestedIpStack::Dual => "ipv4v6",
    }
}

fn ims_sec_agree_name(value: proletarius_ims::SecAgreeMode) -> &'static str {
    match value {
        proletarius_ims::SecAgreeMode::Disabled => "disabled",
        proletarius_ims::SecAgreeMode::Auto => "auto",
        proletarius_ims::SecAgreeMode::Required => "required",
    }
}

#[derive(Debug)]
pub(crate) enum PlanError {
    InvalidRequest,
    InvalidEpdg,
    #[allow(
        dead_code,
        reason = "preserves typed reader-worker error mapping for the native actor route"
    )]
    Reader(crate::reader_task::ReaderTaskError),
    ReaderIdentity(&'static str),
    Carrier,
    Overrides,
    SubscriberIdentity(NativeImsProfileError),
    DeviceIdentity(NativeImsProfileError),
    DeviceIdentityConfig(crate::reader_device_identity::PersistedDeviceIdentityError),
    Transport(SessionSelectionError),
    SurgeRecovery(&'static str),
}

impl PlanError {
    fn status(&self) -> StatusCode {
        match self {
            Self::InvalidRequest | Self::InvalidEpdg => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Reader(crate::reader_task::ReaderTaskError::Timeout) => {
                StatusCode::GATEWAY_TIMEOUT
            }
            Self::Reader(crate::reader_task::ReaderTaskError::WorkerFailed) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Reader(crate::reader_task::ReaderTaskError::Busy) => StatusCode::CONFLICT,
            Self::ReaderIdentity("reader_busy") => StatusCode::SERVICE_UNAVAILABLE,
            Self::ReaderIdentity(
                "reader_card_absent" | "reader_unavailable" | "reader_identity_changed",
            ) => StatusCode::CONFLICT,
            Self::ReaderIdentity(_) => StatusCode::BAD_GATEWAY,
            Self::Carrier
            | Self::Overrides
            | Self::SubscriberIdentity(_)
            | Self::DeviceIdentity(_)
            | Self::DeviceIdentityConfig(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Transport(SessionSelectionError::TimedOut) => StatusCode::GATEWAY_TIMEOUT,
            Self::Transport(_) | Self::SurgeRecovery(_) => StatusCode::BAD_GATEWAY,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::SurgeRecovery(code) => code,
            Self::InvalidRequest => "invalid_native_session_request",
            Self::InvalidEpdg => "invalid_epdg_hostname",
            Self::Reader(crate::reader_task::ReaderTaskError::Timeout) => {
                "reader_operation_timeout"
            }
            Self::Reader(crate::reader_task::ReaderTaskError::WorkerFailed) => "reader_task_failed",
            Self::Reader(crate::reader_task::ReaderTaskError::Busy) => "card_busy",
            Self::ReaderIdentity(code) => code,
            Self::Carrier => "native_carrier_selection_failed",
            Self::Overrides => "native_carrier_overrides_invalid",
            Self::SubscriberIdentity(NativeImsProfileError::IncompleteIsimIdentity) => {
                "native_ims_identity_incomplete"
            }
            Self::SubscriberIdentity(_) => "native_ims_identity_invalid",
            Self::DeviceIdentity(NativeImsProfileError::MissingPersistentDeviceIdentity) => {
                "native_device_identity_missing"
            }
            Self::DeviceIdentity(_) => "native_device_identity_invalid",
            Self::DeviceIdentityConfig(
                crate::reader_device_identity::PersistedDeviceIdentityError::Unreadable,
            ) => "native_device_identity_unreadable",
            Self::DeviceIdentityConfig(
                crate::reader_device_identity::PersistedDeviceIdentityError::Oversized,
            ) => "native_device_identity_oversized",
            Self::Transport(SessionSelectionError::InvalidEpdg) => "invalid_epdg_hostname",
            Self::Transport(SessionSelectionError::TimedOut) => "transport_selection_timeout",
            Self::Transport(SessionSelectionError::MissingVerifiedProxy) => {
                "proxy_address_unavailable"
            }
            Self::Transport(SessionSelectionError::Policy { .. }) => "native_transport_unavailable",
        }
    }

    fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Reader(crate::reader_task::ReaderTaskError::Timeout)
                | Self::ReaderIdentity("reader_busy" | "reader_identity_failed")
                | Self::Transport(_)
                | Self::SurgeRecovery(_)
        )
    }
}

fn failure(error: PlanError) -> Response<Full<Bytes>> {
    let status = error.status();
    let code = error.code();
    static_failure(status, code, error.retryable())
}

fn static_failure(status: StatusCode, code: &str, retryable: bool) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error": code, "code": code, "retryable": retryable}),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_sms_subscription_id_matches_legacy_go_identity_and_fails_closed() {
        assert_eq!(
            super::native_device_sms_subscription_id("8901234567890123456", "234150123456789")
                .as_deref(),
            Some("sub1_0aaff327037ab6c610753d2c87aed4cbaa36680e2ac42bd45f04a3e143990109")
        );
        assert_eq!(
            super::native_device_sms_subscription_id(" 8901234567890123456 ", " 234150123456789 "),
            super::native_device_sms_subscription_id("8901234567890123456", "234150123456789")
        );
        for (iccid, imsi) in [
            ("", "234150123456789"),
            ("8901234567890123456", ""),
            ("0000000000000000000", "234150123456789"),
            ("8901234567890123456", "0000000000"),
            ("890123456789012345x", "234150123456789"),
            ("8901234567890123456", "23415012345678x"),
        ] {
            assert_eq!(super::native_device_sms_subscription_id(iccid, imsi), None);
        }
    }

    #[test]
    fn native_sms_subscription_override_preserves_explicit_empty_identity() {
        assert_eq!(
            super::resolve_native_sms_subscription_id(None, Some("lease-id".into())),
            Some("lease-id".into())
        );
        assert_eq!(
            super::resolve_native_sms_subscription_id(Some(String::new()), Some("lease-id".into())),
            Some(String::new())
        );
        assert_eq!(super::resolve_native_sms_subscription_id(None, None), None);
    }

    #[tokio::test]
    async fn shutdown_requests_owned_task_cleanup_before_reporting_poisoned_supervisor() {
        let mut owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let key = crate::native_vowifi_supervisor::ReaderSessionKey::new(
            "poisoned-shutdown-reader",
            "8901234567890123456",
        )
        .unwrap();
        let permit = owner.reserve_session(&key).ok().unwrap();
        let generation = permit.generation();
        let stop = permit.cancellation_sender();
        let mut stopped = stop.subscribe();
        owner.session_tasks.insert(
            generation,
            crate::native_ims_actor::Task::spawn(stop, async move {
                let _ = stopped.changed().await;
                panic!("controlled poisoned-shutdown session task")
            }),
        );
        let supervisor = owner.supervisor.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = supervisor.lock().unwrap();
                panic!("poison native supervisor mutex");
            })
            .join()
            .is_err()
        );

        assert!(matches!(
            owner.begin_shutdown(),
            Err(super::NativeSessionAdmissionError::SupervisorUnavailable)
        ));
        assert!(owner.stopping);
        assert!(*permit.stop_signal().borrow());

        tokio::time::timeout(Duration::from_secs(1), async {
            while !owner
                .session_tasks
                .get(&generation)
                .is_some_and(crate::native_ims_actor::Task::is_finished)
                || !owner
                    .recovery
                    .as_ref()
                    .is_some_and(crate::native_ims_actor::Task::is_finished)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        owner.reap_finished_session_tasks().await;
        assert!(!owner.session_tasks.contains_key(&generation));
        assert!(
            owner
                .session_task_failure
                .as_ref()
                .is_some_and(|error| error.is_panic())
        );
        assert!(
            owner
                .recovery
                .take()
                .expect("recovery worker remains owned")
                .finish()
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn lifecycle_generation_stop_signals_supervisor_and_owner_held_task() {
        let lifecycle = std::sync::Arc::new(super::NativeLifecycle::default());
        let mut owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let key = crate::native_vowifi_supervisor::ReaderSessionKey::new(
            "lifecycle-stop-reader",
            "8901234567890123456",
        )
        .unwrap();
        let permit = owner.reserve_session(&key).ok().unwrap();
        let generation = permit.generation();
        let stop = permit.cancellation_sender();
        let mut stopped = stop.subscribe();
        owner.session_tasks.insert(
            generation,
            crate::native_ims_actor::Task::spawn(stop, async move {
                let _ = stopped.changed().await;
                panic!("controlled lifecycle stop worker panic")
            }),
        );
        assert!(lifecycle.install(owner).is_ok());

        assert_eq!(
            lifecycle.stop_generation(generation.value()),
            crate::native_vowifi_supervisor::StopDisposition::Signalled
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let state = lifecycle.state.lock().unwrap();
                let owner = state.owner.as_ref().expect("installed owner");
                let task = owner
                    .session_tasks
                    .get(&generation)
                    .expect("generation task remains owned until reaped");
                if task.is_finished() {
                    break;
                }
                drop(state);
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut state = lifecycle.state.lock().unwrap();
        let owner = state.owner.as_mut().expect("installed owner");
        assert_eq!(
            owner.supervisor.lock().unwrap().worker_finished(&permit),
            crate::native_vowifi_supervisor::CompletionDisposition::Removed
        );
        owner.reap_finished_session_tasks().await;
        assert!(owner.session_tasks.is_empty());
        assert!(
            owner
                .session_task_failure
                .as_ref()
                .is_some_and(|error| error.is_panic())
        );
    }

    #[tokio::test]
    async fn service_owner_retains_generation_task_until_reaped_and_preserves_panic() {
        let mut owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let key = crate::native_vowifi_supervisor::ReaderSessionKey::new(
            "owner-held-task-reader",
            "8901234567890123456",
        )
        .unwrap();
        let permit = owner.reserve_session(&key).ok().unwrap();
        let generation = permit.generation();
        let task = crate::native_ims_actor::Task::spawn(permit.cancellation_sender(), async move {
            panic!("controlled session worker panic")
        });
        owner.session_tasks.insert(generation, task);

        assert!(owner.session_tasks.contains_key(&generation));
        assert_eq!(
            owner.stop_generation(generation),
            crate::native_vowifi_supervisor::StopDisposition::Signalled
        );
        assert!(*permit.stop_signal().borrow());
        tokio::time::timeout(Duration::from_secs(1), async {
            while !owner
                .session_tasks
                .get(&generation)
                .is_some_and(crate::native_ims_actor::Task::is_finished)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        owner.reap_finished_session_tasks().await;
        assert!(!owner.session_tasks.contains_key(&generation));
        assert!(
            owner
                .session_task_failure
                .as_ref()
                .is_some_and(|error| error.is_panic())
        );
        assert_eq!(
            owner.supervisor.lock().unwrap().worker_finished(&permit),
            crate::native_vowifi_supervisor::CompletionDisposition::Removed
        );
        assert!(matches!(
            owner.finish_after_sessions().await,
            Ok(Err(error)) if error.is_panic()
        ));
    }

    #[tokio::test]
    async fn shutdown_waits_for_owner_held_task_even_after_supervisor_slot_is_removed() {
        let mut owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let key = crate::native_vowifi_supervisor::ReaderSessionKey::new(
            "owner-held-drain-reader",
            "8901234567890123456",
        )
        .unwrap();
        let permit = owner.reserve_session(&key).ok().unwrap();
        let generation = permit.generation();
        assert_eq!(
            owner.supervisor.lock().unwrap().worker_finished(&permit),
            crate::native_vowifi_supervisor::CompletionDisposition::Removed
        );
        let (release, wait) = tokio::sync::oneshot::channel();
        let task = crate::native_ims_actor::Task::spawn(permit.cancellation_sender(), async move {
            let _ = wait.await;
            panic!("controlled late owner task panic")
        });
        owner.session_tasks.insert(generation, task);

        let owner = match owner
            .finish_before(tokio::time::Instant::now() + Duration::from_millis(20))
            .await
        {
            Err((super::NativeSessionShutdownWaitError::Deadline, owner)) => owner,
            _ => panic!("shutdown must wait for the owner-held task"),
        };
        assert!(owner.session_tasks.contains_key(&generation));
        assert!(owner.stopping);
        release.send(()).unwrap();
        let result = match tokio::time::timeout(
            Duration::from_secs(1),
            owner.finish_before(tokio::time::Instant::now() + Duration::from_secs(1)),
        )
        .await
        {
            Ok(Ok(result)) => result,
            _ => panic!("owner-held task did not drain before the deadline"),
        };
        assert!(matches!(result, Err(error) if error.is_panic()));
    }

    #[tokio::test]
    async fn activated_lifecycle_owns_and_drains_its_recovery_worker() {
        let lifecycle = std::sync::Arc::new(super::NativeLifecycle::default());
        lifecycle.activate().unwrap();
        {
            let state = lifecycle.state.lock().unwrap();
            let owner = state.owner.as_ref().expect("activated owner");
            assert!(!owner.stopping);
            assert!(owner.recovery.is_some());
        }
        lifecycle
            .finish_shutdown(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        let state = lifecycle.state.lock().unwrap();
        assert!(state.owner.is_none());
        assert!(state.stopping);
    }

    #[tokio::test]
    async fn activated_lifecycle_never_reopens_after_shutdown_admission_closes() {
        let lifecycle = std::sync::Arc::new(super::NativeLifecycle::default());
        lifecycle.begin_shutdown().unwrap();
        assert_eq!(lifecycle.activate(), Err("native_lifecycle_not_accepting"));
        assert!(lifecycle.state.lock().unwrap().owner.is_none());
    }

    #[tokio::test]
    async fn recovery_task_panic_stays_unconfirmed_after_owner_is_consumed() {
        let lifecycle = std::sync::Arc::new(super::NativeLifecycle::default());
        let mut owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let (stop, _) = tokio::sync::watch::channel(false);
        owner.recovery = Some(crate::native_ims_actor::Task::spawn(stop, async {
            panic!("controlled recovery worker failure");
        }));
        assert!(lifecycle.install(owner).is_ok());
        for _ in 0..2 {
            assert_eq!(
                lifecycle.finish_shutdown(tokio::time::Instant::now()).await,
                Err("native_recovery_task_failed")
            );
        }
        let state = lifecycle.state.lock().unwrap();
        assert!(state.owner.is_none());
        assert!(state.stopping);
        assert!(!state.finishing);
    }

    #[tokio::test]
    async fn failed_cleanup_is_not_reclassified_as_an_empty_success() {
        for reason in ["native_recovery_task_failed", "native_shutdown_task_failed"] {
            let lifecycle = std::sync::Arc::new(super::NativeLifecycle::default());
            lifecycle.state.lock().unwrap().cleanup_failure = Some(reason);
            for _ in 0..2 {
                assert_eq!(
                    lifecycle.finish_shutdown(tokio::time::Instant::now()).await,
                    Err(reason)
                );
            }
            let owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
            let (_, owner) = lifecycle.install(owner).err().unwrap();
            assert!(
                owner
                    .finish_before(tokio::time::Instant::now())
                    .await
                    .is_ok()
            );
        }
    }

    #[tokio::test]
    async fn cancelling_shared_shutdown_waiter_does_not_abort_owned_cleanup() {
        let lifecycle = std::sync::Arc::new(super::NativeLifecycle::default());
        let owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let key = crate::native_vowifi_supervisor::ReaderSessionKey::new(
            "cancel-lifecycle-reader",
            "8901234567890123456",
        )
        .unwrap();
        let permit = owner.reserve_session(&key).ok().unwrap();
        let supervisor = owner.supervisor.clone();
        assert!(lifecycle.install(owner).is_ok());
        let waiting = lifecycle.clone();
        let caller = tokio::spawn(async move {
            waiting
                .finish_shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if lifecycle.state.lock().unwrap().finishing {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            lifecycle
                .finish_shutdown(tokio::time::Instant::now())
                .await
                .unwrap_err(),
            "native_shutdown_in_progress"
        );
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(lifecycle.state.lock().unwrap().finishing);
        assert!(supervisor.lock().unwrap().snapshot(&key).is_some());
        supervisor.lock().unwrap().worker_finished(&permit);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !lifecycle.state.lock().unwrap().finishing {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(lifecycle.state.lock().unwrap().owner.is_none());
        assert!(lifecycle.state.lock().unwrap().stopping);
    }

    #[tokio::test]
    async fn poisoned_lifecycle_restoration_keeps_unclean_owner_and_reports_unavailable() {
        let lifecycle = std::sync::Arc::new(super::NativeLifecycle::default());
        let owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let key = crate::native_vowifi_supervisor::ReaderSessionKey::new(
            "poison-lifecycle-reader",
            "8901234567890123456",
        )
        .unwrap();
        let permit = owner.reserve_session(&key).ok().unwrap();
        let supervisor = owner.supervisor.clone();
        assert!(lifecycle.install(owner).is_ok());
        let waiting = lifecycle.clone();
        let caller = tokio::spawn(async move {
            waiting
                .finish_shutdown(tokio::time::Instant::now() + Duration::from_millis(200))
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if lifecycle.state.lock().unwrap().finishing {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _state = lifecycle.state.lock().unwrap();
            panic!("fixture lifecycle lock poison");
        }));
        assert!(poisoned.is_err());
        assert_eq!(
            caller.await.unwrap().unwrap_err(),
            "native_shutdown_unconfirmed"
        );
        {
            let state = lifecycle.state.lock().unwrap();
            assert!(state.stopping);
            assert!(!state.finishing);
            assert!(state.owner.as_ref().unwrap().recovery.is_some());
            assert!(supervisor.lock().unwrap().snapshot(&key).is_some());
            assert!(*permit.stop_signal().borrow());
        }
        supervisor.lock().unwrap().worker_finished(&permit);
        lifecycle
            .finish_shutdown(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        assert!(lifecycle.state.lock().unwrap().owner.is_none());
    }

    #[tokio::test]
    async fn shared_lifecycle_restores_unclean_owner_and_rejects_new_install() {
        let lifecycle = std::sync::Arc::new(super::NativeLifecycle::default());
        let owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let key = crate::native_vowifi_supervisor::ReaderSessionKey::new(
            "lifecycle-reader",
            "8901234567890123456",
        )
        .unwrap();
        let permit = owner.reserve_session(&key).ok().unwrap();
        let supervisor = owner.supervisor.clone();
        assert!(lifecycle.install(owner).is_ok());
        let error = lifecycle
            .finish_shutdown(tokio::time::Instant::now() + Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(error, "native_shutdown_unconfirmed");
        assert!(lifecycle.state.lock().unwrap().owner.is_some());
        assert!(supervisor.lock().unwrap().snapshot(&key).is_some());
        let rejected = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        assert!(lifecycle.install(rejected).is_err());
        supervisor.lock().unwrap().worker_finished(&permit);
        lifecycle
            .finish_shutdown(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        assert!(lifecycle.state.lock().unwrap().owner.is_none());
    }

    #[tokio::test]
    async fn shutdown_wait_deadline_returns_original_owner_and_keeps_active_slot() {
        let owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let key = crate::native_vowifi_supervisor::ReaderSessionKey::new(
            "deadline-reader",
            "8901234567890123456",
        )
        .unwrap();
        let permit = owner.reserve_session(&key).ok().unwrap();
        let owner = match owner
            .finish_before(tokio::time::Instant::now() + Duration::from_millis(10))
            .await
        {
            Err((super::NativeSessionShutdownWaitError::Deadline, owner)) => owner,
            _ => panic!("unclean session must not report successful shutdown"),
        };
        assert!(*permit.stop_signal().borrow());
        assert!(owner.recovery.is_some());
        assert!(owner.supervisor.lock().unwrap().snapshot(&key).is_some());
        assert!(matches!(
            owner.reserve_session(&key),
            Err(super::NativeSessionAdmissionError::Stopping)
        ));
        owner.supervisor.lock().unwrap().worker_finished(&permit);
        match owner
            .finish_before(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
        {
            Ok(result) => assert!(result.is_ok()),
            Err(_) => panic!("completed original owner must remain finishable"),
        }
    }

    #[tokio::test]
    async fn begin_shutdown_quiesces_admission_without_consuming_recovery_owner() {
        use crate::native_vowifi_supervisor::ReaderSessionKey;
        let mut owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let key = ReaderSessionKey::new("quiesce-reader", "8901234567890123456").unwrap();
        let permit = owner.reserve_session(&key).ok().unwrap();
        assert!(owner.begin_shutdown().is_ok());
        assert!(*permit.stop_signal().borrow());
        assert!(owner.recovery.is_some());
        assert!(matches!(
            owner.reserve_session(&key),
            Err(super::NativeSessionAdmissionError::Stopping)
        ));
        assert!(owner.begin_shutdown().is_ok(), "quiesce is idempotent");
        owner.supervisor.lock().unwrap().worker_finished(&permit);
        match owner.finish_after_sessions().await {
            Ok(result) => {
                assert!(result.is_ok());
            }
            Err(_) => panic!("completed quiesced owner must finish"),
        }
    }

    #[tokio::test]
    async fn service_owner_refuses_final_shutdown_until_session_completion() {
        use crate::native_vowifi_supervisor::ReaderSessionKey;
        let owner = super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap();
        let key = ReaderSessionKey::new("owner-reader", "8901234567890123456").unwrap();
        let permit = owner.reserve_session(&key).ok().unwrap();
        let owner = match owner.finish_after_sessions().await {
            Err(owner) => owner,
            Ok(_) => panic!("active slot cannot be bypassed during shutdown"),
        };
        assert!(*permit.stop_signal().borrow());
        let another = ReaderSessionKey::new("new-owner-reader", "8901234567890123456").unwrap();
        assert!(matches!(
            owner.reserve_session(&another),
            Err(super::NativeSessionAdmissionError::Stopping)
        ));
        assert!(
            owner
                .supervisor
                .lock()
                .unwrap()
                .snapshot(&another)
                .is_none()
        );
        owner.supervisor.lock().unwrap().worker_finished(&permit);
        let result = match owner.finish_after_sessions().await {
            Ok(result) => result.unwrap(),
            Err(_) => panic!("completed service must finish"),
        };
        assert_eq!(
            (result.confirmed, result.failed, result.task_failed),
            (0, 0, 0)
        );
    }

    #[test]
    fn recovery_loop_keeps_bounded_typed_failure_details_and_reports_overflow() {
        let mut result = super::NativeRecoveryLoopExit::default();
        for index in 0..10 {
            result.record(Ok(Ok(Err(
                proletarius_domain::BackendFailure::GoBackendUnavailable(format!(
                    "failure-{index}"
                )),
            ))));
        }
        result.record(Ok(Ok(Ok(()))));
        assert_eq!(
            (
                result.confirmed,
                result.failed,
                result.task_failed,
                result.omitted_failures
            ),
            (1, 10, 0, 2)
        );
        assert_eq!(result.failures.len(), 8);
        assert!(
            matches!(result.failures.front().map(|record| &record.failure), Some(super::NativeRecoveryFailure::Release(proletarius_domain::BackendFailure::GoBackendUnavailable(message))) if message == "failure-2")
        );
        assert!(
            matches!(result.failures.back().map(|record| &record.failure), Some(super::NativeRecoveryFailure::Release(proletarius_domain::BackendFailure::GoBackendUnavailable(message))) if message == "failure-9")
        );
    }

    #[tokio::test]
    async fn recovery_loop_rejects_zero_interval_and_does_not_keep_service_alive() {
        let owner = std::sync::Arc::new(std::sync::Mutex::new(
            crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default(),
        ));
        assert!(super::spawn_lease_recovery_loop(&owner, Duration::ZERO).is_err());
        let task = super::spawn_lease_recovery_loop(&owner, Duration::from_millis(1)).unwrap();
        drop(owner);
        let result = tokio::time::timeout(Duration::from_secs(1), task.finish())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (result.confirmed, result.failed, result.task_failed),
            (0, 0, 0)
        );
    }

    #[tokio::test]
    async fn retained_recovery_retries_exact_lease_over_real_unix_ipc() {
        retained_recovery_exchange(RecoveryFixtureMode::Direct).await;
    }

    #[tokio::test]
    async fn abandoned_recovery_waiter_keeps_claim_until_ipc_finishes() {
        retained_recovery_exchange(RecoveryFixtureMode::Abandoned).await;
    }

    #[tokio::test]
    async fn recovery_loop_stop_drains_started_unix_release_before_returning() {
        retained_recovery_exchange(RecoveryFixtureMode::BackgroundStop).await;
    }

    #[tokio::test]
    async fn service_shutdown_waits_for_background_retry_to_confirm_original_lease() {
        retained_recovery_exchange(RecoveryFixtureMode::ServiceShutdown).await;
    }

    #[tokio::test]
    async fn lost_release_response_is_resolved_by_proof_without_repeating_mutation() {
        retained_recovery_exchange(RecoveryFixtureMode::LostConfirmation).await;
    }

    enum RecoveryFixtureMode {
        Direct,
        Abandoned,
        BackgroundStop,
        ServiceShutdown,
        LostConfirmation,
    }

    // These recovery tests exercise the native lease and physical owner directly.
    // A bounded wait may lose confirmation; only the retained worker's terminal
    // proof can release the original claim, and no retry starts a new transaction.
    struct RecoverySupplier {
        session: crate::reader_pcsc::LifecycleBinding,
        captured: std::sync::Mutex<Option<crate::reader_reservation::NativePinBinding>>,
    }
    impl crate::reader_session_lease::NativeSessionSupplier for RecoverySupplier {
        fn pre_admit(
            self: std::sync::Arc<Self>,
            _: String,
            _: crate::reader_reservation::NativePinPolicy,
            _ordinary: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
            _maintenance: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
        ) -> crate::reader_session_lease::PreAdmissionFuture {
            Box::pin(async { Ok(4) })
        }
        fn admit(
            self: std::sync::Arc<Self>,
            _: crate::reader_reservation::NativePinBinding,
        ) -> crate::reader_session_lease::AdmissionFuture {
            Box::pin(async { Ok(()) })
        }
        fn snapshot(
            &self,
            binding: &crate::reader_reservation::NativePinBinding,
        ) -> Result<serde_json::Value, String> {
            *self.captured.lock().unwrap() = Some(binding.clone());
            Ok(serde_json::json!({
                "reader": binding.reader, "reader_generation": binding.reader_generation,
                "session": self.session,
                "identity": {"iccid":"8985200014631959072","imsi":"234100896392986",
                    "mcc":"234","mnc":"33","private_identity":"subscriber@ims.example.test",
                    "public_identity":"sip:+447700900123@ims.example.test"}
            }))
        }
        fn finalize(
            self: std::sync::Arc<Self>,
            _: crate::reader_reservation::NativePinBinding,
            _: Option<crate::reader_session_lease::NativeCardContext>,
            _: String,
            _: String,
        ) -> crate::reader_session_lease::SupplierFuture {
            Box::pin(async { Err("recovery must never replay finalization".into()) })
        }
    }
    struct RecoveryPhysical {
        started: Option<tokio::sync::oneshot::Sender<()>>,
        finish: std::sync::mpsc::Receiver<()>,
        transactions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        ends: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl proletarius_sim::ApduTransport for RecoveryPhysical {
        type Error = crate::reader_pcsc::Failure;
        fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
            panic!("lease recovery must not submit a card mutation")
        }
    }
    impl crate::reader_pcsc::Physical for RecoveryPhysical {
        fn transaction(
            &mut self,
            body: &mut dyn FnMut(
                &mut dyn proletarius_sim::ApduTransport<Error = crate::reader_pcsc::Failure>,
            ),
        ) -> Result<(), crate::reader_pcsc::Failure> {
            self.transactions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            body(self);
            self.started.take().unwrap().send(()).unwrap();
            self.finish.recv_timeout(Duration::from_secs(5)).unwrap();
            self.ends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn close(&mut self) -> Result<(), crate::reader_pcsc::Failure> {
            Ok(())
        }
    }

    async fn retained_recovery_exchange(mode: RecoveryFixtureMode) {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        let abandon = matches!(mode, RecoveryFixtureMode::Abandoned);
        let background = matches!(mode, RecoveryFixtureMode::BackgroundStop);
        let service_shutdown = matches!(mode, RecoveryFixtureMode::ServiceShutdown);
        let lost_confirmation = matches!(mode, RecoveryFixtureMode::LostConfirmation);
        const READER: &str = "Native recovery PCSC fixture";
        const PRODUCER: &str = "retained-native-pin-producer-000001";
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let (proceed, proceed_rx) = std::sync::mpsc::channel();
        let transactions = Arc::new(AtomicUsize::new(0));
        let ends = Arc::new(AtomicUsize::new(0));
        let physical = Arc::new(Mutex::new(Some(RecoveryPhysical {
            started: Some(ready),
            finish: proceed_rx,
            transactions: transactions.clone(),
            ends: ends.clone(),
        })));
        let physical_owner = crate::reader_pcsc::Owner::with_factory(
            PRODUCER.into(),
            Arc::new(move |_| Ok(Box::new(physical.lock().unwrap().take().unwrap()))),
            Arc::new(|| Ok(vec![READER.into()])),
            Arc::new(|_, _| panic!("recovery must not reset/reopen the physical owner")),
        );
        let session = physical_owner.native_open(READER).unwrap();
        let reader_gate = Arc::new(crate::reader_reservation::Gate::default());
        reader_gate.initialize_business_producer("go-recovery-fixture");
        reader_gate.initialize_native_reader_producer(PRODUCER);
        reader_gate.test_seed_selection(READER, 4);
        let supplier = Arc::new(RecoverySupplier {
            session: session.clone(),
            captured: Mutex::new(None),
        });
        let erased: Arc<dyn crate::reader_session_lease::NativeSessionSupplier> = supplier.clone();
        reader_gate.set_native_session_supplier(Arc::downgrade(&erased));
        let lease = Arc::new(
            crate::reader_session_lease::NativeReaderSessionLease::acquire(
                std::path::Path::new("unused-no-go-recovery.sock"),
                None,
                READER,
                reader_gate.clone(),
                physical_owner.clone(),
            )
            .await
            .unwrap(),
        );
        let binding = lease
            .pcsc_context(physical_owner.clone())
            .unwrap()
            .binding()
            .clone();
        let original = supplier.captured.lock().unwrap().clone().unwrap();
        assert_eq!(binding.producer_id, PRODUCER);
        assert_eq!(binding.session_id, session.session_id);
        assert_eq!(binding.lease_id, original.lease_id);
        lease.set_release_timeout(Duration::from_millis(120));
        let key = ReaderSessionKey::new(READER, "8985200014631959072").unwrap();
        let service = service_shutdown
            .then(|| super::NativeSessionServiceOwner::new(Duration::from_millis(1)).unwrap());
        let owner = service
            .as_ref()
            .map(|service| service.supervisor.clone())
            .unwrap_or_else(|| Arc::new(Mutex::new(NativeVoWiFiSupervisor::default())));
        let permit = owner.lock().unwrap().reserve(key.clone()).unwrap();
        let generation = permit.generation();
        let record = Arc::new(super::NativeLeaseRecovery {
            phase: None,
            status: None,
            lease: SessionCard::leased(lease.clone()),
            cleanup_confirmed: std::sync::atomic::AtomicBool::new(false),
        });
        assert!(
            owner
                .lock()
                .unwrap()
                .retain_recovery(&permit, record.clone())
        );
        owner.lock().unwrap().request_stop(&permit);
        drop(permit);
        assert!(
            super::spawn_retained_lease_recovery(&key, generation, owner.clone()).is_none(),
            "cleanup proof is mandatory"
        );
        record.confirm_cleanup(); // Controlled tunnel cleanup proof; physical cleanup remains real.

        if let Some(service) = service {
            let service = match service.finish_after_sessions().await {
                Err(service) => service,
                Ok(_) => panic!("unreleased slot must prevent close"),
            };
            assert!(matches!(
                service.reserve_session(&key),
                Err(super::NativeSessionAdmissionError::Stopping)
            ));
            tokio::time::timeout(Duration::from_secs(1), ready_rx)
                .await
                .unwrap()
                .unwrap();
            // Wait for the first bounded release waiter to report uncertainty.
            // Service shutdown must continue holding the original recovery slot.
            tokio::time::timeout(Duration::from_secs(2), async {
                while service.progress.borrow().failed == 0 {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(ends.load(Ordering::SeqCst), 0);
            assert!(reader_gate.admit(READER).is_err());
            assert!(owner.lock().unwrap().reserve(key.clone()).is_err());
            proceed.send(()).unwrap();
            let progress = service.progress.clone();
            let result = match service
                .finish_before(tokio::time::Instant::now() + Duration::from_secs(3))
                .await
            {
                Ok(result) => result.unwrap(),
                Err(_) => panic!("terminal physical proof must allow close"),
            };
            assert_eq!((result.confirmed, result.task_failed), (1, 0));
            assert!(result.failed >= 1);
            let progress = *progress.borrow();
            assert_eq!(
                (progress.revision, progress.confirmed, progress.failed),
                (result.failed + 1, 1, result.failed)
            );
            assert_eq!(progress.generation, Some(generation));
            assert!(progress.status == Some(super::NativeRecoveryStatus::Confirmed));
            assert!(owner.lock().unwrap().reserve(key).is_ok());
        } else {
            let (background_task, progress) = if background {
                let (task, progress) = super::spawn_lease_recovery_loop_with_progress(
                    &owner,
                    Duration::from_millis(1),
                )
                .unwrap();
                (Some(task), Some(progress))
            } else {
                (None, None)
            };
            let first = if background {
                None
            } else {
                Some(
                    super::dispatch_lease_recovery(owner.clone(), 1)
                        .pop()
                        .unwrap(),
                )
            };
            tokio::time::timeout(Duration::from_secs(1), ready_rx)
                .await
                .unwrap()
                .unwrap();
            assert!(
                reader_gate.admit(READER).is_err(),
                "unknown release must fence physical admission"
            );
            assert_eq!(ends.load(Ordering::SeqCst), 0);
            assert!(
                super::spawn_retained_lease_recovery(&key, generation, owner.clone()).is_none()
            );
            if background {
                let task = background_task.unwrap();
                task.request_stop();
                let mut join = tokio::spawn(task.finish());
                assert!(
                    tokio::time::timeout(Duration::from_millis(10), &mut join)
                        .await
                        .is_err(),
                    "loop stop must drain its started release waiter"
                );
                let result = tokio::time::timeout(Duration::from_secs(2), join)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    (result.confirmed, result.failed, result.task_failed),
                    (0, 1, 0)
                );
                assert_eq!(result.omitted_failures, 0);
                let progress = *progress.as_ref().unwrap().borrow();
                assert_eq!(
                    (
                        progress.revision,
                        progress.confirmed,
                        progress.failed,
                        progress.task_failed,
                        progress.omitted_failures
                    ),
                    (1, 0, 1, 0, 0)
                );
                assert_eq!(progress.generation, Some(generation));
                assert!(progress.status == Some(super::NativeRecoveryStatus::ReleaseFailed));
                assert!(matches!(
                    result.failures.front().map(|record| &record.failure),
                    Some(super::NativeRecoveryFailure::Release(_))
                ));
            } else if abandon {
                drop(first);
                assert!(
                    super::spawn_retained_lease_recovery(&key, generation, owner.clone()).is_none(),
                    "abandoning the view cannot drop the running recovery claim"
                );
            } else {
                assert!(
                    first.unwrap().finish().await.unwrap().is_err(),
                    "a lost bounded confirmation must retain recovery"
                );
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                while owner
                    .lock()
                    .unwrap()
                    .recovery_permit(&key, generation)
                    .is_none()
                {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .unwrap();
            assert!(owner.lock().unwrap().reserve(key.clone()).is_err());
            assert_eq!(
                owner.lock().unwrap().snapshot(&key).unwrap().generation,
                generation
            );
            assert_eq!(ends.load(Ordering::SeqCst), 0);
            assert!(reader_gate.admit(READER).is_err());
            proceed.send(()).unwrap();
            lease.set_release_timeout(Duration::from_secs(2));
            let second = super::dispatch_lease_recovery(owner.clone(), 1)
                .pop()
                .unwrap();
            second.finish().await.unwrap().unwrap();
            assert!(owner.lock().unwrap().snapshot(&key).is_none());
            let replacement = owner.lock().unwrap().reserve(key.clone()).unwrap();
            assert_ne!(replacement.generation(), generation);
            if lost_confirmation {
                assert!(
                    super::spawn_retained_lease_recovery(&key, generation, owner.clone()).is_none()
                );
                assert_eq!(
                    owner.lock().unwrap().snapshot(&key).unwrap().generation,
                    replacement.generation()
                );
                // Retry observes exact terminal proof. It never replays EndTransaction,
                // an APDU mutation, or opens a new physical session.
                lease.release().await.unwrap();
            }
        }
        assert!(supplier.captured.lock().unwrap().as_ref() == Some(&original));
        assert_eq!(transactions.load(Ordering::SeqCst), 1);
        assert_eq!(ends.load(Ordering::SeqCst), 1);
        assert!(lease.release_confirmed().await.unwrap());
        assert!(reader_gate.admit(READER).is_ok());
        physical_owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn initial_release_cannot_overlap_an_existing_recovery_claim() {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        let owner = std::sync::Arc::new(std::sync::Mutex::new(NativeVoWiFiSupervisor::default()));
        let key = ReaderSessionKey::new("release-claim-reader", "8901234567890123456").unwrap();
        let permit = owner.lock().unwrap().reserve(key.clone()).unwrap();
        assert!(
            owner
                .lock()
                .unwrap()
                .retain_recovery(&permit, std::sync::Arc::new(1_u32))
        );
        assert!(owner.lock().unwrap().claim_recovery(&permit));
        let weak = std::sync::Arc::downgrade(&owner);
        let release = async {
            panic!("occupied recovery claim must not perform release IO");
            #[allow(unreachable_code)]
            Ok::<(), ()>(())
        };
        assert!(
            super::release_with_claim(&permit, release, &weak)
                .await
                .is_none()
        );
        assert!(owner.lock().unwrap().reserve(key).is_err());
        assert!(!owner.lock().unwrap().claim_recovery(&permit));
    }

    #[tokio::test]
    async fn uncertain_cleanup_never_polls_release_or_frees_card_slot() {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        for case in 0..3 {
            let key = ReaderSessionKey::new("uncertain-reader", "8901234567890123456").unwrap();
            let owner =
                std::sync::Arc::new(std::sync::Mutex::new(NativeVoWiFiSupervisor::default()));
            let permit = owner.lock().unwrap().reserve(key.clone()).unwrap();
            let cancelled = tokio::spawn(std::future::pending::<()>());
            cancelled.abort();
            let join_error = cancelled.await.unwrap_err();
            let actor = if case == 2 {
                drop(permit);
                Err(join_error)
            } else {
                Ok((
                    crate::native_ims_actor::Exit {
                        undelivered: Vec::new(),
                        permit,
                        reason: crate::native_ims_actor::StopReason::Requested,
                        cleanup: if case == 0 {
                            Err(Default::default())
                        } else {
                            Ok(())
                        },
                    },
                    if case == 1 {
                        Err(join_error)
                    } else {
                        Ok(Ok(()))
                    },
                    None,
                ))
            };
            let mut worker = super::NativeSmsWorkerExit {
                actor,
                consumer: Err("fixture_consumer_already_stopped"),
            };
            let weak = std::sync::Arc::downgrade(&owner);
            let release = async {
                panic!("uncertain cleanup must not release the card");
                #[allow(unreachable_code)]
                Ok::<(), ()>(())
            };
            assert_eq!(
                super::finish_registered_lease(&mut worker, release, &weak).await,
                None
            );
            assert!(owner.lock().unwrap().reserve(key).is_err());
            if let Ok((_, _, completion)) = worker.actor {
                assert!(completion.is_none());
            }
        }
    }

    #[tokio::test]
    async fn lease_completion_waits_for_confirmation_and_retains_slot_on_failure() {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        for succeeds in [true, false] {
            let key = ReaderSessionKey::new("lease-reader", "8901234567890123456").unwrap();
            let owner =
                std::sync::Arc::new(std::sync::Mutex::new(NativeVoWiFiSupervisor::default()));
            let permit = owner.lock().unwrap().reserve(key.clone()).unwrap();
            assert!(
                owner
                    .lock()
                    .unwrap()
                    .retain_recovery(&permit, std::sync::Arc::new(1_u32))
            );
            let mut worker = super::NativeSmsWorkerExit {
                actor: Ok((
                    crate::native_ims_actor::Exit {
                        undelivered: Vec::new(),
                        permit,
                        reason: crate::native_ims_actor::StopReason::Requested,
                        cleanup: Ok(()),
                    },
                    Ok(Ok(())),
                    None,
                )),
                consumer: Err("fixture_consumer_already_stopped"),
            };
            let weak = std::sync::Arc::downgrade(&owner);
            let (entered, observed) = tokio::sync::oneshot::channel();
            let (finish, confirmation) = tokio::sync::oneshot::channel();
            let mut task = tokio::spawn(async move {
                let result = super::finish_registered_lease(
                    &mut worker,
                    async {
                        entered.send(()).unwrap();
                        confirmation.await.unwrap();
                        if succeeds {
                            Ok(())
                        } else {
                            Err("release_unconfirmed")
                        }
                    },
                    &weak,
                )
                .await;
                (worker, result)
            });
            tokio::time::timeout(std::time::Duration::from_secs(1), observed)
                .await
                .unwrap()
                .unwrap();
            assert!(owner.lock().unwrap().reserve(key.clone()).is_err());
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), &mut task)
                    .await
                    .is_err()
            );
            finish.send(()).unwrap();
            let (worker, result) = task.await.unwrap();
            assert_eq!(
                result,
                Some(if succeeds {
                    Ok(())
                } else {
                    Err("release_unconfirmed")
                })
            );
            assert_eq!(worker.actor.unwrap().2.is_some(), succeeds);
            assert_eq!(owner.lock().unwrap().reserve(key).is_ok(), succeeds);
        }
    }

    #[tokio::test]
    async fn registration_stop_waits_for_rollback_and_preserves_result() {
        let (stop, mut signal) = tokio::sync::watch::channel(false);
        let (observed, observation) = tokio::sync::oneshot::channel();
        let (release, rollback) = tokio::sync::oneshot::channel();
        let registration = async move {
            signal.changed().await.unwrap();
            assert!(*signal.borrow());
            observed.send(()).unwrap();
            rollback.await.unwrap();
            Err::<(), _>("rollback_failed")
        };
        let mut owner = tokio::spawn(super::finish_registration_after_stop(
            registration,
            async {},
            stop,
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), observation)
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut owner)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        assert_eq!(owner.await.unwrap(), Err("rollback_failed"));
    }

    #[tokio::test]
    async fn completed_registration_does_not_request_stop() {
        let (stop, signal) = tokio::sync::watch::channel(false);
        let result = super::finish_registration_after_stop(
            async { Ok::<_, ()>(7) },
            std::future::pending(),
            stop,
        )
        .await;
        assert_eq!(result, Ok(7));
        assert!(!*signal.borrow());
    }

    #[tokio::test]
    async fn registration_monitor_waits_for_real_tcp_retirement() {
        let (wake, _) = tokio::sync::watch::channel(0_u64);
        let (tcp, registry) = proletarius_vowifi::netstack_tcp::TcpSocketRegistry::new(wake);
        let (_stop, stop) = tokio::sync::watch::channel(false);
        let (_lease, lease) = tokio::sync::watch::channel(false);
        let waiting = super::wait_registration_stop(&tcp, stop, lease);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        drop(registry);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn registration_monitor_ignores_false_updates_and_observes_lease_loss() {
        let (wake, _) = tokio::sync::watch::channel(0_u64);
        let (tcp, _registry) = proletarius_vowifi::netstack_tcp::TcpSocketRegistry::new(wake);
        let (stop_tx, stop) = tokio::sync::watch::channel(false);
        let (lease_tx, lease) = tokio::sync::watch::channel(false);
        let waiting = super::wait_registration_stop(&tcp, stop, lease);
        tokio::pin!(waiting);
        stop_tx.send_replace(false);
        lease_tx.send_replace(false);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        lease_tx.send_replace(true);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn network_exit_stops_actor_and_preserves_failure() {
        let (stop, mut signal) = tokio::sync::watch::channel(false);
        let (ike, ike_signal) = tokio::sync::watch::channel(false);
        let actor = async move {
            signal.changed().await.unwrap();
            assert!(*signal.borrow());
            assert!(!*ike_signal.borrow(), "IKE stop precedes IMS cleanup");
            42
        };
        let network = tokio::spawn(async { Err::<(), _>("network_failed") });
        let (actor, result) = super::join_registered_network(actor, network, stop, ike).await;
        assert_eq!(actor, 42);
        assert_eq!(result.unwrap(), Err("network_failed"));
    }

    #[tokio::test]
    async fn actor_cleanup_requests_ike_stop_before_waiting_network() {
        let (stop, stop_signal) = tokio::sync::watch::channel(false);
        let (ike, mut ike_signal) = tokio::sync::watch::channel(false);
        let network = tokio::spawn(async move {
            ike_signal.changed().await.unwrap();
            assert!(*ike_signal.borrow());
            7
        });
        let (actor, result) =
            super::join_registered_network(async { 42 }, network, stop, ike).await;
        assert_eq!(actor, 42);
        assert_eq!(result.unwrap(), 7);
        assert!(!*stop_signal.borrow());
    }

    #[tokio::test]
    async fn combined_cleanup_errors_keep_slot_until_external_cleanup_is_proven() {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        for failure in 0..3 {
            let owner =
                std::sync::Arc::new(std::sync::Mutex::new(NativeVoWiFiSupervisor::default()));
            let key = ReaderSessionKey::new("reader", "8901234567890123456").unwrap();
            let permit = owner.lock().unwrap().reserve(key.clone()).unwrap();
            owner.lock().unwrap().mark_running(&permit).unwrap();
            let cancel = permit.cancellation_sender();
            let actor = crate::native_ims_actor::Task::spawn(cancel.clone(), async move {
                let cleanup = if failure == 0 {
                    Err(proletarius_vowifi::registration_coordinator::CleanupReport::default())
                } else {
                    Ok(())
                };
                let network = if failure == 1 {
                    Err(
                        tokio::spawn(async { panic!("controlled network fixture failure") })
                            .await
                            .unwrap_err(),
                    )
                } else if failure == 2 {
                    Ok(Err(super::NativeNetworkError::Configuration(
                        proletarius_vowifi::netstack_runtime::NetworkConfigError::MissingAddress,
                    )))
                } else {
                    Ok(Ok(()))
                };
                (
                    crate::native_ims_actor::Exit {
                        undelivered: Vec::new(),
                        permit,
                        reason: crate::native_ims_actor::StopReason::Requested,
                        cleanup,
                    },
                    network,
                    None,
                )
            });
            let finished = super::join_registered_sms_workers(
                actor,
                Err("fixture_not_started"),
                cancel,
                std::sync::Arc::downgrade(&owner),
            )
            .await;
            let (exit, network, completion) = finished.actor.unwrap();
            assert!(!super::registered_cleanup_complete(&exit, &network));
            assert!(completion.is_none());
            assert!(
                owner.lock().unwrap().reserve(key).is_err(),
                "failed cleanup released the slot"
            );
        }
    }

    #[tokio::test]
    async fn combined_consumer_panic_is_preserved_after_actor_cleanup() {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        let owner = std::sync::Arc::new(std::sync::Mutex::new(NativeVoWiFiSupervisor::default()));
        let key = ReaderSessionKey::new("reader", "8901234567890123456").unwrap();
        let permit = owner.lock().unwrap().reserve(key.clone()).unwrap();
        owner.lock().unwrap().mark_running(&permit).unwrap();
        let cancel = permit.cancellation_sender();
        let actor = crate::native_ims_actor::Task::spawn(cancel.clone(), async move {
            (
                crate::native_ims_actor::Exit {
                    undelivered: Vec::new(),
                    permit,
                    reason: crate::native_ims_actor::StopReason::Requested,
                    cleanup: Ok(()),
                },
                Ok(Ok(())),
                None,
            )
        });
        let consumer: tokio::task::JoinHandle<crate::native_sms_consumer::LoopExit> =
            tokio::spawn(async { panic!("controlled consumer fixture failure") });
        let finished = super::join_registered_sms_workers(
            actor,
            Ok(consumer),
            cancel,
            std::sync::Arc::downgrade(&owner),
        )
        .await;
        assert!(matches!(finished.consumer, Ok(Err(error)) if error.is_panic()));
        assert!(finished.actor.unwrap().2.is_some());
        assert!(owner.lock().unwrap().reserve(key).is_ok());
    }

    #[tokio::test]
    async fn combined_actor_panic_requests_stop_without_fabricating_cleanup_completion() {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        let owner = std::sync::Arc::new(std::sync::Mutex::new(NativeVoWiFiSupervisor::default()));
        let key = ReaderSessionKey::new("reader", "8901234567890123456").unwrap();
        let permit = owner.lock().unwrap().reserve(key.clone()).unwrap();
        owner.lock().unwrap().mark_running(&permit).unwrap();
        let cancel = permit.cancellation_sender();
        let stopped = permit.stop_signal();
        let actor: crate::native_ims_actor::Task<super::NativeRegisteredActorResult> =
            crate::native_ims_actor::Task::spawn(cancel.clone(), async move {
                drop(permit);
                panic!("controlled actor fixture failure");
            });
        let finished = super::join_registered_sms_workers(
            actor,
            Err("fixture_not_started"),
            cancel,
            std::sync::Arc::downgrade(&owner),
        )
        .await;
        assert!(matches!(finished.actor, Err(error) if error.is_panic()));
        assert!(*stopped.borrow());
        assert!(
            owner.lock().unwrap().reserve(key).is_err(),
            "unknown cleanup incorrectly released the slot"
        );
    }

    #[tokio::test]
    async fn abandoning_combined_parent_keeps_cleanup_and_completion_running() {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        let owner = std::sync::Arc::new(std::sync::Mutex::new(NativeVoWiFiSupervisor::default()));
        let key = ReaderSessionKey::new("reader", "8901234567890123456").unwrap();
        let permit = owner.lock().unwrap().reserve(key.clone()).unwrap();
        owner.lock().unwrap().mark_running(&permit).unwrap();
        let cancel = permit.cancellation_sender();
        let stopped = permit.stop_signal();
        let (actor_release, actor_released) = tokio::sync::oneshot::channel();
        let (consumer_release, consumer_released) = tokio::sync::oneshot::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        let actor = crate::native_ims_actor::Task::spawn(cancel.clone(), async move {
            actor_released.await.unwrap();
            (
                crate::native_ims_actor::Exit {
                    undelivered: Vec::new(),
                    permit,
                    reason: crate::native_ims_actor::StopReason::Requested,
                    cleanup: Ok(()),
                },
                Ok(Ok(())),
                None,
            )
        });
        let consumer = tokio::spawn(async move {
            consumer_released.await.unwrap();
            crate::native_sms_consumer::LoopExit {
                reason: crate::native_sms_consumer::LoopStop::Requested,
                last_error: None,
                undelivered_notice: None,
                unconfirmed_reports: Vec::new(),
            }
        });
        let weak = std::sync::Arc::downgrade(&owner);
        let worker_cancel = cancel.clone();
        let parent = crate::native_ims_actor::Task::spawn(cancel, async move {
            let result =
                super::join_registered_sms_workers(actor, Ok(consumer), worker_cancel, weak).await;
            let _ = done.send(());
            result
        });
        drop(parent);
        assert!(*stopped.borrow());
        assert!(owner.lock().unwrap().reserve(key.clone()).is_err());
        actor_release.send(()).unwrap();
        tokio::task::yield_now().await;
        assert!(owner.lock().unwrap().reserve(key.clone()).is_err());
        consumer_release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), finished)
            .await
            .unwrap()
            .unwrap();
        assert!(owner.lock().unwrap().reserve(key).is_ok());
    }

    #[tokio::test]
    async fn combined_worker_holds_slot_until_consumer_finishes_after_network_exit() {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        let owner = std::sync::Arc::new(std::sync::Mutex::new(NativeVoWiFiSupervisor::default()));
        let key = ReaderSessionKey::new("reader", "8901234567890123456").unwrap();
        let permit = owner.lock().unwrap().reserve(key.clone()).unwrap();
        owner.lock().unwrap().mark_running(&permit).unwrap();
        let cancel = permit.cancellation_sender();
        let mut stopped = permit.stop_signal();
        let actor = crate::native_ims_actor::Task::spawn(cancel.clone(), async move {
            (
                crate::native_ims_actor::Exit {
                    undelivered: Vec::new(),
                    permit,
                    reason: crate::native_ims_actor::StopReason::Requested,
                    cleanup: Ok(()),
                },
                Ok(Ok(())),
                None,
            )
        });
        let (release, released) = tokio::sync::oneshot::channel();
        let consumer = tokio::spawn(async move {
            released.await.unwrap();
            crate::native_sms_consumer::LoopExit {
                reason: crate::native_sms_consumer::LoopStop::Requested,
                last_error: None,
                undelivered_notice: None,
                unconfirmed_reports: Vec::new(),
            }
        });
        let join = tokio::spawn(super::join_registered_sms_workers(
            actor,
            Ok(consumer),
            cancel,
            std::sync::Arc::downgrade(&owner),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), stopped.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(
            *stopped.borrow(),
            "consumer stop was not requested after actor cleanup"
        );
        assert!(
            owner.lock().unwrap().reserve(key.clone()).is_err(),
            "slot released while consumer still owns work"
        );
        release.send(()).unwrap();
        let finished = join.await.unwrap();
        assert!(finished.consumer.unwrap().is_ok());
        assert!(finished.actor.unwrap().2.is_some());
        assert!(owner.lock().unwrap().reserve(key).is_ok());
    }

    #[tokio::test]
    async fn combined_setup_failure_still_waits_for_actor_cleanup_before_releasing_slot() {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        let owner = std::sync::Arc::new(std::sync::Mutex::new(NativeVoWiFiSupervisor::default()));
        let key = ReaderSessionKey::new("reader", "8901234567890123456").unwrap();
        let permit = owner.lock().unwrap().reserve(key.clone()).unwrap();
        owner.lock().unwrap().mark_running(&permit).unwrap();
        let cancel = permit.cancellation_sender();
        cancel.send_replace(true);
        let (release, released) = tokio::sync::oneshot::channel();
        let actor = crate::native_ims_actor::Task::spawn(cancel.clone(), async move {
            released.await.unwrap();
            (
                crate::native_ims_actor::Exit {
                    undelivered: Vec::new(),
                    permit,
                    reason: crate::native_ims_actor::StopReason::Requested,
                    cleanup: Ok(()),
                },
                Ok(Ok(())),
                None,
            )
        });
        let mut join = tokio::spawn(super::join_registered_sms_workers(
            actor,
            Err("fixture_setup_rejected"),
            cancel,
            std::sync::Arc::downgrade(&owner),
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut join)
                .await
                .is_err()
        );
        assert!(owner.lock().unwrap().reserve(key.clone()).is_err());
        release.send(()).unwrap();
        let finished = join.await.unwrap();
        assert!(matches!(finished.consumer, Err("fixture_setup_rejected")));
        assert!(finished.actor.unwrap().2.is_some());
        assert!(owner.lock().unwrap().reserve(key).is_ok());
    }

    #[test]
    fn registered_sms_start_distinguishes_wrong_key_stop_and_lost_lease() {
        use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
        let key = ReaderSessionKey::new("reader", "8901234567890123456").unwrap();
        let wrong = ReaderSessionKey::new("other", "8901234567890123456").unwrap();
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(key.clone()).unwrap();
        let (lease, receiver) = tokio::sync::watch::channel(false);
        assert_eq!(
            super::validate_registered_sms_start(&permit, &key, &receiver),
            Ok(())
        );
        assert_eq!(
            super::validate_registered_sms_start(&permit, &wrong, &receiver),
            Err("native_sms_session_mismatch")
        );
        lease.send_replace(true);
        assert_eq!(
            super::validate_registered_sms_start(&permit, &key, &receiver),
            Err("native_sms_lease_lost")
        );
        lease.send_replace(false);
        drop(lease);
        assert_eq!(
            super::validate_registered_sms_start(&permit, &key, &receiver),
            Err("native_sms_lease_lost")
        );
        supervisor.request_stop_all();
        assert_eq!(
            super::validate_registered_sms_start(&permit, &key, &receiver),
            Err("native_sms_session_stopped")
        );
        assert_eq!(
            super::validate_registered_sms_start(&permit, &wrong, &receiver),
            Err("native_sms_session_mismatch")
        );
    }

    use super::*;
    use http_body_util::BodyExt;

    #[test]
    fn request_overrides_are_highest_priority_and_reject_zero_port() {
        let mut overrides = NativeTunnelOverrides {
            epdg: Some("template.example".into()),
            epdg_port: Some(500),
            ..Default::default()
        };
        apply_request_overrides(&mut overrides, " custom.example ".into(), Some(4500)).unwrap();
        assert_eq!(overrides.epdg.as_deref(), Some("custom.example"));
        assert_eq!(overrides.epdg_port, Some(4500));
        assert!(apply_request_overrides(&mut overrides, String::new(), Some(0)).is_err());
    }

    #[test]
    fn session_identity_is_resolved_from_the_generation_bound_lease() {
        let sim = SimIdentity {
            mcc: "234".into(),
            mnc: "33".into(),
            imsi: "234330000000001".into(),
            iccid: "8985200014631959072".into(),
            ..Default::default()
        };
        let carrier = Registry::embedded()
            .unwrap()
            .resolve(&sim)
            .unwrap()
            .unwrap();
        let overrides =
            OverrideSet::parse(b"carrier_overrides:\n  '234/33': {ims_identity_source: isim}\n")
                .unwrap();
        let profile = carrier
            .native_ims_register_profile(&sim, overrides.get("234", "33"))
            .unwrap();
        let lease_identity = crate::reader_session_lease::LeaseIdentity {
            iccid: zeroize::Zeroizing::new(sim.iccid.clone()),
            imsi: zeroize::Zeroizing::new(sim.imsi.clone()),
            mcc: sim.mcc.clone(),
            mnc: sim.mnc.clone(),
            gid1: String::new(),
            gid2: String::new(),
            spn: String::new(),
            private_identity: zeroize::Zeroizing::new("subscriber@ims.example.test".into()),
            public_identity: zeroize::Zeroizing::new("sip:+447700900123@ims.example.test".into()),
            home_domain: Zeroizing::new(String::new()),
            phone_number: Zeroizing::new(String::new()),
            smsc: Zeroizing::new(String::new()),
            smsc_psi: Zeroizing::new(String::new()),
            smsc_target: Zeroizing::new(String::new()),
        };

        let resolved = resolve_session_subscriber_identity(&profile, &lease_identity).unwrap();
        assert_eq!(resolved.private_identity(), "subscriber@ims.example.test");
        assert_eq!(
            resolved.public_identity(),
            "sip:+447700900123@ims.example.test"
        );

        let incomplete_identity = crate::reader_session_lease::LeaseIdentity {
            private_identity: zeroize::Zeroizing::default(),
            ..lease_identity
        };
        assert!(matches!(
            resolve_session_subscriber_identity(&profile, &incomplete_identity),
            Err(NativeImsProfileError::IncompleteIsimIdentity)
        ));
    }

    #[test]
    fn ims_endpoint_selection_uses_only_the_authenticated_tunnel_configuration() {
        let sim = SimIdentity {
            mcc: "234".into(),
            mnc: "33".into(),
            imsi: "234330000000001".into(),
            ..Default::default()
        };
        let carrier = Registry::embedded()
            .unwrap()
            .resolve(&sim)
            .unwrap()
            .unwrap();
        let profile = carrier.native_ims_register_profile(&sim, None).unwrap();
        let tunnel = TunnelConfiguration {
            ipv4_addresses: vec!["10.10.0.2".parse().unwrap()],
            pcscf_servers: vec!["10.10.0.3".parse().unwrap()],
            ..Default::default()
        };
        assert_eq!(
            select_ims_runtime_endpoints(&profile, &tunnel).unwrap(),
            (
                "10.10.0.2".parse().unwrap(),
                "10.10.0.3:5060".parse().unwrap(),
            )
        );

        let no_pcscf = TunnelConfiguration {
            ipv4_addresses: vec!["10.10.0.2".parse().unwrap()],
            ..Default::default()
        };
        assert_eq!(
            select_ims_runtime_endpoints(&profile, &no_pcscf),
            Err(NativeImsEndpointSelectionError::MissingTunnelPcscf)
        );
    }

    #[test]
    fn configured_pcscf_uses_numeric_override_without_requiring_cfg_reply_and_matching_tunnel_address()
     {
        let sim = SimIdentity {
            mcc: "234".into(),
            mnc: "33".into(),
            imsi: "234330000000001".into(),
            ..Default::default()
        };
        let carrier = Registry::embedded()
            .unwrap()
            .resolve(&sim)
            .unwrap()
            .unwrap();
        let overrides =
            OverrideSet::parse(b"carrier_overrides:\n  '234/33': {ims_pcscf: '10.10.0.3:5070'}\n")
                .unwrap();
        let profile = carrier
            .native_ims_register_profile(&sim, overrides.get("234", "33"))
            .unwrap();
        let tunnel = TunnelConfiguration {
            ipv4_addresses: vec!["10.10.0.2".parse().unwrap()],
            pcscf_servers: vec!["10.10.0.3".parse().unwrap()],
            ..Default::default()
        };
        assert_eq!(
            select_ims_runtime_endpoints(&profile, &tunnel).unwrap(),
            (
                "10.10.0.2".parse().unwrap(),
                "10.10.0.3:5070".parse().unwrap(),
            )
        );

        let unadvertised = TunnelConfiguration {
            pcscf_servers: vec!["10.10.0.4".parse().unwrap()],
            ..tunnel.clone()
        };
        assert_eq!(
            select_ims_runtime_endpoints(&profile, &unadvertised),
            Ok((
                "10.10.0.2".parse().unwrap(),
                "10.10.0.3:5070".parse().unwrap()
            ))
        );

        let wrong_family = TunnelConfiguration {
            ipv4_addresses: Vec::new(),
            ipv6_addresses: vec![("2001:db8::2".parse().unwrap(), 64)],
            ..tunnel
        };
        assert_eq!(
            select_ims_runtime_endpoints(&profile, &wrong_family),
            Err(NativeImsEndpointSelectionError::NoMatchingTunnelAddress)
        );
    }

    #[tokio::test]
    async fn actor_stop_is_forwarded_to_the_private_ike_start_channel() {
        let (actor_stop_tx, actor_stop) = tokio::sync::watch::channel(false);
        let (ike_stop_tx, mut ike_stop) = tokio::sync::watch::channel(false);
        let forwarder = forward_startup_stop(actor_stop, ike_stop_tx);

        actor_stop_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), ike_stop.changed())
            .await
            .expect("actor stop should reach the startup-owned IKE channel")
            .unwrap();
        assert!(*ike_stop.borrow());
        forwarder.await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_the_actor_waiter_cannot_stop_ike_before_ims_cleanup() {
        let (ike_stop_tx, mut ike_stop) = tokio::sync::watch::channel(false);
        let (cleanup_entered_tx, cleanup_entered_rx) = tokio::sync::oneshot::channel();
        let (finish_cleanup_tx, finish_cleanup_rx) = tokio::sync::oneshot::channel();
        let cleanup = spawn_ims_shutdown_before_ike(ike_stop_tx, async move {
            cleanup_entered_tx.send(()).unwrap();
            finish_cleanup_rx.await.unwrap();
            Err::<(), _>("simulated IMS cleanup error")
        });

        // The actor may disappear while cleanup is in flight. The independently
        // owned cleanup task must still hold IKE open until IMS cleanup returns.
        drop(cleanup);
        cleanup_entered_rx.await.unwrap();
        assert!(!*ike_stop.borrow());
        finish_cleanup_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), ike_stop.changed())
            .await
            .expect("IKE stop follows completion of IMS cleanup")
            .unwrap();
        assert!(*ike_stop.borrow());
    }

    #[test]
    fn malformed_plan_input_fails_before_any_reader_or_network_work() {
        for request in [
            PlanRequest {
                reader: "Reader\nA".into(),
                proxy: String::new(),
                epdg: String::new(),
                epdg_port: None,
            },
            PlanRequest {
                reader: "Reader A".into(),
                proxy: "proxy\0host:1080".into(),
                epdg: String::new(),
                epdg_port: None,
            },
            PlanRequest {
                reader: "Reader A".into(),
                proxy: String::new(),
                epdg: "https://epdg.example".into(),
                epdg_port: None,
            },
        ] {
            assert!(validate_request(&request).is_err());
        }
        assert!(
            validate_request(&PlanRequest {
                reader: "Reader A".into(),
                proxy: "127.0.0.1:7897".into(),
                epdg: "epdg.epc.mnc033.mcc234.pub.3gppnetwork.org".into(),
                epdg_port: Some(4500),
            })
            .is_ok()
        );
    }

    #[test]
    fn native_reader_readiness_uses_complete_retained_runtime() {
        let readiness = native_session_owner_readiness();
        assert!(readiness.ready());
        assert!(readiness.missing().is_empty());
        let lifecycle = NativeLifecycle::default();
        assert!(lifecycle.handles_public_request(&hyper::Method::POST, "/api/reader/vowifi"));
        assert!(!lifecycle.handles_public_request(&hyper::Method::POST, "/api/vowifi"));
    }

    #[test]
    fn native_owner_requires_every_gate() {
        let ready = NativeOwnerReadiness {
            cross_process_reader_lease: true,
            ble_aka_transport_bridge: true,
            keyed_lifecycle_actor: true,
            protected_ims_runtime: true,
            live_sms_call_runtime: true,
        };
        assert!(ready.ready());

        for readiness in [
            NativeOwnerReadiness {
                cross_process_reader_lease: false,
                ..ready
            },
            NativeOwnerReadiness {
                ble_aka_transport_bridge: false,
                ..ready
            },
            NativeOwnerReadiness {
                keyed_lifecycle_actor: false,
                ..ready
            },
            NativeOwnerReadiness {
                protected_ims_runtime: false,
                ..ready
            },
            NativeOwnerReadiness {
                live_sms_call_runtime: false,
                ..ready
            },
        ] {
            assert!(!readiness.ready());
        }
    }

    #[tokio::test]
    async fn native_owner_not_ready_response_remains_conflict_and_lists_blockers() {
        let response = native_session_owner_not_ready(NativeOwnerReadiness {
            keyed_lifecycle_actor: false,
            protected_ims_runtime: false,
            live_sms_call_runtime: false,
            ..native_session_owner_readiness()
        });
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["code"], "native_session_owner_not_ready");
        assert_eq!(payload["retryable"], false);
        assert_eq!(
            payload["missing"],
            serde_json::json!([
                "keyed_lifecycle_actor",
                "protected_ims_runtime",
                "live_sms_call_runtime",
            ])
        );
    }

    #[test]
    fn reader_failure_codes_preserve_actionability_and_retryability() {
        let cases = [
            (
                PlanError::ReaderIdentity("reader_busy"),
                StatusCode::SERVICE_UNAVAILABLE,
                true,
            ),
            (
                PlanError::ReaderIdentity("reader_card_absent"),
                StatusCode::CONFLICT,
                false,
            ),
            (
                PlanError::Reader(crate::reader_task::ReaderTaskError::Timeout),
                StatusCode::GATEWAY_TIMEOUT,
                true,
            ),
            (
                PlanError::ReaderIdentity("reader_identity_failed"),
                StatusCode::BAD_GATEWAY,
                true,
            ),
            (
                PlanError::Transport(SessionSelectionError::TimedOut),
                StatusCode::GATEWAY_TIMEOUT,
                true,
            ),
        ];
        for (error, status, retryable) in cases {
            assert_eq!(error.status(), status);
            assert_eq!(error.retryable(), retryable);
        }
        assert_eq!(
            PlanError::Transport(SessionSelectionError::TimedOut).code(),
            "transport_selection_timeout"
        );
    }

    #[test]
    fn fallback_config_path_has_expected_filename() {
        // The path selection itself has no I/O and the inherited test process
        // must not mutate its environment. This asserts the stable fallback.
        let path = override_path().expect("configured Switch data path");
        assert!(path.ends_with("carrier-overrides.yaml"));
    }

    #[test]
    fn persistent_identity_failures_stop_before_network_transport() {
        let missing =
            PlanError::DeviceIdentity(NativeImsProfileError::MissingPersistentDeviceIdentity);
        assert_eq!(missing.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(missing.code(), "native_device_identity_missing");

        let unreadable = PlanError::DeviceIdentityConfig(
            crate::reader_device_identity::PersistedDeviceIdentityError::Unreadable,
        );
        assert_eq!(unreadable.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(unreadable.code(), "native_device_identity_unreadable");

        let incomplete_isim =
            PlanError::SubscriberIdentity(NativeImsProfileError::IncompleteIsimIdentity);
        assert_eq!(incomplete_isim.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(incomplete_isim.code(), "native_ims_identity_incomplete");
    }
}
