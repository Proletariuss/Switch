//! Native module RF policy, business reservations and scoped VoWiFi intents.
//! Long-lived business ownership never holds a USB/serial physical transaction.
use crate::availability::BackendAvailability;
use crate::module_radio_store::{Preferences, Store};
use crate::module_radio_transport::{
    Action, Binding, Outcome, ProfileNoteCommand, UsbComposition, VoiceConfiguration,
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore, oneshot};
use tokio::task::JoinHandle;

pub(crate) const PREFIX: &str = "/api/internal/module-radio/";
#[cfg(test)]
mod bootstrap_projection_tests;
mod esim_cleanup;
mod native_startup;
mod native_vowifi;
mod reconnect;
pub(crate) use reconnect::CallModeRebindProof;
#[cfg(test)]
#[path = "module_radio/quarantine_tests.rs"]
mod quarantine_tests;
pub(crate) use native_vowifi::{NativePrepareFailure, NativeVoWiFiIntent};
const INTERVAL: Duration = Duration::from_secs(3);
const USBNET_DEADLINE: Duration = Duration::from_secs(25);
const PROFILE_NOTES_BATCH_SIZE: usize = 32;
static NEXT_NATIVE_ESIM_RESERVATION: AtomicU64 = AtomicU64::new(1);
static NEXT_CALL_MODE_READ_OPERATION: AtomicU64 = AtomicU64::new(1);
#[derive(Clone)]
pub(crate) struct Service(Arc<Inner>);
struct Inner {
    data_root: PathBuf,
    store: Store,
    usb: crate::usb_at::Owner,
    modem: crate::modem_actor::ModemActor,
    _availability: BackendAvailability,
    state: Mutex<State>,
    gate: tokio::sync::Mutex<()>,
    shutdown_gate: tokio::sync::Mutex<()>,
    shutdown_receipt: Mutex<Option<Result<Vec<String>, String>>>,
    slots: Arc<Semaphore>,
    wake: Arc<Notify>,
    finished: Notify,
    task: Mutex<Option<JoinHandle<()>>>,
    call_observation_task: Mutex<Option<JoinHandle<()>>>,
    call_observer: Mutex<Option<CallObserver>>,
    call_capture: Mutex<Option<CallCapture>>,
    call_observation_error: Mutex<Option<String>>,
    #[cfg(test)]
    physical_fixture: Mutex<Option<PhysicalFixture>>,
    #[cfg(test)]
    timer_fixture: Mutex<Option<TimerFixture>>,
    #[cfg(test)]
    usbnet_waiters: std::sync::atomic::AtomicUsize,
}
struct State {
    producer: String,
    epoch: u64,
    binding: Option<Binding>,
    bootstrap_receipt: Option<Binding>,
    config_seq: u64,
    config: Option<Configure>,
    preferences: Option<Preferences>,
    preference_error: bool,
    observed: Outcome,
    reservation: Option<Reservation>,
    esim_release_pending: Option<(u64, Identity)>,
    profile_notes_unknown: bool,
    call_mode_backup_delete_in_flight: bool,
    waters: BTreeMap<Purpose, Water>,
    confirmed_call_mode_releases: BTreeSet<(u64, String, String, u64)>,
    confirmed_call_releases: BTreeSet<(u64, String, String, u64)>,
    intent: Option<Intent>,
    intent_water: Water,
    stopping: bool,
    // Terminal physical receipt and all job/pin admissions share this lock.
    // Existing accepted cleanup drains before this irreversible seal.
    finalized: bool,
    retiring: bool,
    running: bool,
    jobs: usize,
    job_deadline: Option<Instant>,
    epoch_cancel: Arc<AtomicBool>,
    mutating_in_flight: bool,
    active_shared_voice_pins: usize,
    executing_operation: Option<Arc<crate::module_recovery::OperationGuard>>,
}
#[derive(Clone, Default)]
struct Water {
    sequence: u64,
    token: String,
    released: bool,
}
#[derive(Clone)]
struct Reservation {
    operation: Arc<crate::module_recovery::OperationGuard>,
    purpose: Purpose,
    identity: Identity,
    attempts: BTreeMap<String, Attempt>,
    unknown: bool,
    operation_lock: Option<Arc<crate::call_mode_backups::OperationFileLock>>,
    active_voice_pins: usize,
    active_reconnect_pins: usize,
    reconnect: Option<crate::usb_at::CallModeUsbAnchor>,
    reconnect_reboot: Option<String>,
    restore_journal_required: bool,
    restore_journal: Option<crate::call_mode_restore_journal::Journal>,
    restore_reboot: Option<crate::call_mode_restore_journal::PreparedWrite>,
    restore_reboot_verified: bool,
}
#[derive(Clone)]
struct Attempt {
    input: RadioAction,
    result: Option<Result<Outcome, Failure>>,
}
#[derive(Clone)]
struct Intent {
    operation: Arc<crate::module_recovery::OperationGuard>,
    identity: Identity,
    active: bool,
    restore_mode: Option<u8>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Identity {
    token: String,
    sequence: u64,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Purpose {
    Esim,
    ModuleProfileNotes,
    DeviceSms,
    Call,
    CallMode,
    Reboot,
    Diagnostic,
}
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
    uncertain: bool,
    submitted: bool,
}
/// Internal receipt only; public errors retain their existing wire shape.
/// Every failure after state.jobs registration is conservatively admitted.
struct SubmissionFailure {
    failure: Failure,
    admitted: bool,
}
impl From<Failure> for SubmissionFailure {
    fn from(failure: Failure) -> Self {
        Self {
            failure,
            admitted: false,
        }
    }
}
pub(crate) struct CallModeBackupDeleteGuard(Arc<Inner>);
/// Proof of the current native exclusive call-mode reservation. The complete
/// guard must follow the real ADB worker through USB cleanup, not its caller.
pub(crate) struct CallModeVoiceGuard {
    inner: Arc<Inner>,
    operation_lock: Arc<crate::call_mode_backups::OperationFileLock>,
    _operation: Arc<crate::module_recovery::OperationGuard>,
    identity: Identity,
    epoch: u64,
    _slot: tokio::sync::OwnedSemaphorePermit,
}
/// An ordinary ADB transition is a native radio job even when its operation
/// file lock is shared. Retirement must wait for its physical USB cleanup.
pub(crate) struct SharedVoiceGuard {
    inner: Arc<Inner>,
    _operation: Option<Arc<crate::module_recovery::OperationGuard>>,
    _right: crate::module_recovery::OperationRight,
    _slot: tokio::sync::OwnedSemaphorePermit,
}
impl Drop for SharedVoiceGuard {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active_shared_voice_pins -= 1;
        state.jobs -= 1;
        drop(state);
        self.inner.finished.notify_waiters();
    }
}
impl Drop for CallModeVoiceGuard {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.epoch == self.epoch {
            if let Some(reservation) = state.reservation.as_mut() {
                if reservation.identity == self.identity
                    && reservation
                        .operation_lock
                        .as_ref()
                        .is_some_and(|lock| Arc::ptr_eq(lock, &self.operation_lock))
                {
                    reservation.active_voice_pins -= 1;
                }
            }
        }
        state.jobs -= 1;
        drop(state);
        self.inner.finished.notify_waiters();
    }
}
pub(crate) struct EsimReservation {
    producer_id: String,
    epoch: u64,
    token: String,
    sequence: u64,
}
pub(crate) struct CallModeReadinessFacts {
    pub(crate) usb: UsbComposition,
    pub(crate) voice: Option<VoiceConfiguration>,
}
struct DeviceSmsReservation {
    producer_id: String,
    token: String,
    sequence: u64,
}
impl Drop for CallModeBackupDeleteGuard {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap()
            .call_mode_backup_delete_in_flight = false;
    }
}
impl Failure {
    pub(crate) fn command_submitted(&self) -> bool {
        self.submitted
    }
    fn new(status: StatusCode, code: &'static str, message: &str) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            uncertain: false,
            submitted: false,
        }
    }
    fn invalid() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_radio_request",
            "射频请求无效",
        )
    }
    fn stale() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "radio_stale_generation",
            "射频所有权已变化，请刷新后重试",
        )
    }
    fn busy() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "module_radio_busy",
            "模块射频正在由其他业务占用",
        )
    }
    fn stopped() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "backend_stopping",
            "Switch 正在退出，未提交新操作",
        )
    }
    fn unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "module_unavailable",
            "DJI 模块未连接",
        )
    }
    fn policy() -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            "flight_mode_state_read_failed",
            "读取模块飞行模式偏好失败",
        )
    }
    pub(crate) fn public_submitted(&self) -> bool {
        self.submitted
    }
    pub(crate) fn public_uncertain(&self) -> bool {
        self.uncertain
    }
    pub(crate) fn public_status(&self) -> StatusCode {
        self.status
    }
    pub(crate) fn public_code(&self) -> &'static str {
        self.code
    }
    pub(crate) fn public_message(&self) -> &str {
        &self.message
    }
    fn response(self) -> Response<Full<Bytes>> {
        crate::json_response(
            self.status,
            &json!({"error":self.message,"code":self.code,"retryable":false,"recoverable":false,"uncertain":self.uncertain,"submitted":self.submitted}),
        )
    }
    pub(crate) fn module_radio_compat_response(self) -> Response<Full<Bytes>> {
        let recoverable = !self.submitted && !self.uncertain;
        crate::json_response(
            self.status,
            &json!({
                "error": self.message,
                "code": self.code,
                "recoverable": recoverable,
            }),
        )
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Failure {}
impl From<crate::usb_at::Failure> for Failure {
    fn from(error: crate::usb_at::Failure) -> Self {
        Self {
            status: error.status,
            code: error.code,
            message: error.message,
            uncertain: error.uncertain,
            submitted: error.uncertain,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Configure {
    producer_id: String,
    config_seq: u64,
    transport: String,
    session_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Producer {
    producer_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReservationRequest {
    producer_id: String,
    action: String,
    purpose: Purpose,
    token: String,
    sequence: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentRequest {
    producer_id: String,
    action: String,
    token: String,
    sequence: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RadioAction {
    producer_id: String,
    purpose: Purpose,
    token: String,
    sequence: u64,
    operation_id: String,
    kind: String,
    #[serde(default)]
    target: Option<u8>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    voice_target: Option<VoiceConfiguration>,
    #[serde(default)]
    usb_expected: Option<UsbComposition>,
    #[serde(default)]
    usb_target: Option<UsbComposition>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetRequest {
    enabled: bool,
    #[serde(default)]
    user_initiated: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct USBNetRequest {
    #[serde(default)]
    mode: Option<i64>,
}
enum ModuleProfileNotesRequest {
    List,
    Save(crate::module_profile_notes::Note),
}
#[derive(Default)]
struct ModulePhonebookProbeResult {
    storage_supported: bool,
    storage_selected: bool,
    read_supported: bool,
    write_supported: bool,
    storage_status: String,
    responses: BTreeMap<String, String>,
    uncertain: bool,
}
impl ModulePhonebookProbeResult {
    fn value(self) -> Value {
        json!({
            "storage":"ME",
            "storage_supported":self.storage_supported,
            "storage_selected":self.storage_selected,
            "read_supported":self.read_supported,
            "write_supported":self.write_supported,
            "storage_status":self.storage_status,
            "responses":self.responses
        })
    }
}
enum Job {
    Query(Producer),
    Configure(Configure),
    ConfigureNativeSerial {
        input: Configure,
        serial_session: u64,
    },
    Bootstrap(Producer),
    BootstrapNativeSerial {
        input: Producer,
        serial_session: u64,
    },
    Refresh(Producer),
    Reservation(ReservationRequest),
    RestoreReservation(ReservationRequest),
    Intent(IntentRequest),
    NativeIntent(native_vowifi::Request),
    NativeIdleObserve(native_vowifi::IdleObservation),
    Action(RadioAction),
    CallModeRebind(call_mode::RebindRequest),
    CaptureRestorePhysicalBinding(CallModeReservation),
    CallModeOpen(Producer),
    CellularCall(call::CallRequest),
    CallPoll,
    PublicGet,
    PublicSet(SetRequest),
    PublicReboot,
    PublicNetworkRecoveryReboot,
    PublicUSBNetMode {
        mode: i64,
        deadline: Instant,
        expected_binding: String,
        read_wait: Option<oneshot::Sender<UsbnetReadWait>>,
    },
    PublicNetworkDiagnostic {
        deadline: Instant,
    },
    PublicAtConsole {
        command: String,
        deadline: Instant,
    },
    PublicModuleProfileNotes(ModuleProfileNotesRequest),
    PublicModulePhonebookProbe,
    Tick,
}
// Not a transport result or dispatch grant. Only a pre-dispatch gate decision
// may mint this marker; the public handler never serializes it.
struct UsbnetReadWait;

impl Job {
    fn producer(&self) -> Option<&str> {
        match self {
            Self::Query(v) | Self::Bootstrap(v) | Self::Refresh(v) => Some(&v.producer_id),
            Self::Configure(v) => Some(&v.producer_id),
            Self::ConfigureNativeSerial { input, .. } => Some(&input.producer_id),
            Self::BootstrapNativeSerial { input, .. } => Some(&input.producer_id),
            Self::Reservation(v) | Self::RestoreReservation(v) => Some(&v.producer_id),
            Self::Intent(v) => Some(&v.producer_id),
            Self::NativeIntent(v) => Some(&v.input.producer_id),
            Self::NativeIdleObserve(v) => Some(&v.producer),
            Self::Action(v) => Some(&v.producer_id),
            Self::CallModeRebind(v) => Some(&v.reservation.producer_id),
            Self::CaptureRestorePhysicalBinding(v) => Some(&v.producer_id),
            Self::CallModeOpen(v) => Some(&v.producer_id),
            Self::CellularCall(v) => Some(&v.lease.producer_id),
            _ => None,
        }
    }
    fn cleanup(&self) -> bool {
        match self {
            Self::Query(_) => true,
            Self::Reservation(v) => v.action == "release",
            Self::Intent(v) => v.action == "restore" || v.action == "observe",
            Self::NativeIntent(v) => v.input.action == "restore" || v.input.action == "observe",
            _ => false,
        }
    }
}
impl Service {
    pub(crate) fn module_recovery(&self) -> &crate::module_recovery::Gate {
        self.0._availability.module_recovery()
    }
    fn require_module_admission(&self) -> Result<(), Failure> {
        if self.module_recovery().is_quarantined() {
            Err(Failure::new(
                StatusCode::CONFLICT,
                crate::module_recovery::CODE,
                crate::module_recovery::MESSAGE,
            ))
        } else {
            Ok(())
        }
    }
    fn admission_failure() -> Failure {
        Failure::new(
            StatusCode::CONFLICT,
            crate::module_recovery::CODE,
            crate::module_recovery::MESSAGE,
        )
    }
    fn require_new_module_admission(&self) -> Result<(), Failure> {
        self.require_module_admission()?;
        if self.module_recovery().is_admission_revoked() {
            Err(Self::admission_failure())
        } else {
            Ok(())
        }
    }
    /// Only the serialized, already admitted actor job can dispatch transport
    /// work. A cached producer/session alone is never an operation right.
    fn current_operation_right(&self) -> Result<crate::module_recovery::OperationRight, Failure> {
        let state = self.0.state.lock().unwrap();
        let operation = state
            .executing_operation
            .as_ref()
            .ok_or_else(Self::admission_failure)?;
        let right = operation.right();
        if !right.is_active_for(self.module_recovery(), &state.producer) {
            return Err(Self::admission_failure());
        }
        Ok(right)
    }
    /// Existing finite owners retain their operation through release/restore.
    /// Request spelling only selects a candidate: the actual retained owner,
    /// identity, producer and (for native leases) epoch/binding prove it.
    fn admit_job(
        &self,
        job: &Job,
        state: &State,
    ) -> Result<(Option<Arc<crate::module_recovery::OperationGuard>>, bool), Failure> {
        let reservation = |purpose, token: &str, sequence| {
            state.reservation.as_ref().filter(|owner| {
                owner.purpose == purpose
                    && owner.identity.token == token
                    && owner.identity.sequence == sequence
            })
        };
        let intent = |input: &IntentRequest| {
            state.intent.as_ref().filter(|owner| {
                owner.identity.token == input.token && owner.identity.sequence == input.sequence
            })
        };
        let operation = match job {
            Job::Reservation(input) if input.action == "release" => {
                if let Some(owner) = reservation(input.purpose, &input.token, input.sequence) {
                    Some(owner.operation.clone())
                } else if state.waters.get(&input.purpose).is_some_and(|water| {
                    water.released && water.token == input.token && water.sequence == input.sequence
                }) {
                    return Ok((None, true));
                } else {
                    None
                }
            }
            Job::Intent(input) => intent(input).map(|owner| owner.operation.clone()),
            Job::NativeIntent(request) => {
                intent(&request.input).map(|owner| owner.operation.clone())
            }
            Job::Action(input) => reservation(input.purpose, &input.token, input.sequence)
                .and_then(|owner| {
                    let replay = owner
                        .attempts
                        .get(&input.operation_id)
                        .is_some_and(|attempt| attempt.input == *input);
                    (replay
                        || input.purpose != Purpose::Call
                            && (input.purpose != Purpose::CallMode
                                || owner.operation_lock.is_some()))
                    .then(|| owner.operation.clone())
                }),
            Job::CallModeRebind(input) => {
                let lease = &input.reservation;
                reservation(lease.purpose, &lease.token, lease.sequence)
                    .filter(|owner| {
                        lease.owned && lease.epoch == state.epoch && owner.operation_lock.is_some()
                    })
                    .map(|owner| owner.operation.clone())
            }
            Job::CaptureRestorePhysicalBinding(lease) => {
                reservation(lease.purpose, &lease.token, lease.sequence)
                    .filter(|owner| {
                        lease.owned
                            && lease.epoch == state.epoch
                            && lease.purpose == Purpose::CallMode
                            && owner.restore_journal_required
                            && owner.operation_lock.is_some()
                    })
                    .map(|owner| owner.operation.clone())
            }
            Job::CellularCall(input)
                if call::call_lease_matches(state, &input.lease)
                    && matches!(
                        &input.action,
                        Action::CellularCallRead
                            | Action::CellularCall(crate::module_radio_transport::CallCommand {
                                action: CallAction::Hangup,
                                ..
                            })
                    ) =>
            {
                state
                    .reservation
                    .as_ref()
                    .map(|owner| owner.operation.clone())
            }
            _ => None,
        };
        if let Some(operation) = operation {
            if operation
                .right()
                .is_active_for(self.module_recovery(), &state.producer)
            {
                return Ok((Some(operation), true));
            }
            return Err(Self::admission_failure());
        }
        let restore = match job {
            Job::Intent(input) => Some(input),
            Job::NativeIntent(request) => Some(&request.input),
            _ => None,
        };
        if restore.is_some_and(|input| {
            input.action == "restore"
                && state.intent.is_none()
                && state.intent_water.released
                && state.intent_water.token == input.token
                && state.intent_water.sequence == input.sequence
        }) {
            return Ok((None, true));
        }
        self.module_recovery()
            .admit_operation(&state.producer)
            .map(|operation| (Some(Arc::new(operation)), false))
            .map_err(|_| Self::admission_failure())
    }
    pub(crate) fn new(
        root: PathBuf,
        usb: crate::usb_at::Owner,
        modem: crate::modem_actor::ModemActor,
        availability: BackendAvailability,
        producer: String,
    ) -> Self {
        Self(Arc::new(Inner {
            data_root: root.clone(),
            store: Store::new(root),
            usb,
            modem,
            _availability: availability,
            state: Mutex::new(State {
                producer,
                epoch: 0,
                binding: None,
                bootstrap_receipt: None,
                config_seq: 0,
                config: None,
                preferences: None,
                preference_error: false,
                observed: Outcome::default(),
                reservation: None,
                esim_release_pending: None,
                profile_notes_unknown: false,
                call_mode_backup_delete_in_flight: false,
                waters: BTreeMap::new(),
                confirmed_call_mode_releases: BTreeSet::new(),
                confirmed_call_releases: BTreeSet::new(),
                intent: None,
                intent_water: Water::default(),
                stopping: false,
                finalized: false,
                retiring: false,
                running: false,
                jobs: 0,
                job_deadline: None,
                epoch_cancel: Arc::new(AtomicBool::new(false)),
                mutating_in_flight: false,
                active_shared_voice_pins: 0,
                executing_operation: None,
            }),
            gate: tokio::sync::Mutex::new(()),
            shutdown_gate: tokio::sync::Mutex::new(()),
            shutdown_receipt: Mutex::new(None),
            slots: Arc::new(Semaphore::new(8)),
            wake: Arc::new(Notify::new()),
            finished: Notify::new(),
            task: Mutex::new(None),
            call_observation_task: Mutex::new(None),
            call_observer: Mutex::new(None),
            call_capture: Mutex::new(None),
            call_observation_error: Mutex::new(None),
            #[cfg(test)]
            physical_fixture: Mutex::new(None),
            #[cfg(test)]
            timer_fixture: Mutex::new(None),
            #[cfg(test)]
            usbnet_waiters: std::sync::atomic::AtomicUsize::new(0),
        }))
    }
    /// True while a module SIM/radio workflow owns or reserves the shared AT
    /// channel. Read-only SMS storage queries must not race those operations.
    pub(crate) fn sim_channel_busy(&self) -> bool {
        let state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.intent.is_some()
            || state.reservation.is_some()
            || state.retiring
            || state.active_shared_voice_pins != 0
    }
    /// Matches the legacy public SMS-refresh conflict: only a VoWiFi intent
    /// owns the SIM security channel, while unrelated radio reservations do not.
    pub(crate) fn vowifi_owns_sim_channel(&self) -> bool {
        self.0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .intent
            .is_some()
    }
    pub(crate) fn uses_usb_transport(&self) -> bool {
        self.usb_transport_session().is_some()
    }
    /// An acknowledged transport configuration is an explicit choice. Missing
    /// configuration is not evidence that the process selected USB.
    pub(crate) fn configured_usb_transport(&self) -> Option<bool> {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopping || state.retiring {
            return None;
        }
        match state
            .config
            .as_ref()
            .map(|config| config.transport.as_str())
        {
            Some("usb") => Some(true),
            Some("serial") => Some(false),
            _ => None,
        }
    }
    /// Capability receipt for the same configured physical USB route. This
    /// records actual bootstrap completion, not eUICC/SIM contents or IMS ready.
    pub(crate) fn usb_esim_route_available(&self) -> bool {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        !state.stopping
            && !state.retiring
            && state.bootstrap_receipt == state.binding
            && matches!(&state.binding, Some(Binding::Usb { producer_id, session_id })
                if self.0.usb.validate_radio_binding(producer_id, session_id).is_ok())
    }
    pub(crate) fn usb_transport_session(&self) -> Option<String> {
        match self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .binding
            .as_ref()
        {
            Some(Binding::Usb { session_id, .. }) => Some(session_id.clone()),
            _ => None,
        }
    }
    /// The configured serial route, including its physical reopen generation.
    /// This is a route observation; SMS readiness still belongs to the actor.
    pub(crate) fn serial_transport_session(&self) -> Option<u64> {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopping
            || state.retiring
            || state
                .config
                .as_ref()
                .is_none_or(|v| v.transport != "serial")
        {
            return None;
        }
        match state.binding.as_ref() {
            Some(Binding::Serial { serial_session }) => Some(*serial_session),
            _ => None,
        }
    }
    /// A synchronous recovery-journal commit may run only while this exact
    /// serial route and its bootstrap receipt remain published. The callback
    /// must not await or invoke actor RPC; it may take the card-binding and
    /// eSIM-cache locks in that order.
    pub(crate) fn with_current_serial_transport<T>(
        &self,
        session: u64,
        commit: impl FnOnce() -> T,
    ) -> Option<T> {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let binding = Some(Binding::Serial {
            serial_session: session,
        });
        let current = session != 0
            && !state.stopping
            && !state.retiring
            && state
                .config
                .as_ref()
                .is_some_and(|config| config.transport == "serial")
            && state.binding == binding
            && (!self.0.modem.native_serial_managed() || state.bootstrap_receipt == binding);
        current.then(commit)
    }
    /// Physical policy receipt, independent of SIM presence or SMS readiness.
    pub(crate) fn native_serial_bootstrapped(&self, session: u64) -> bool {
        let state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        !state.stopping
            && !state.retiring
            && state.binding
                == Some(Binding::Serial {
                    serial_session: session,
                })
            && state.bootstrap_receipt == state.binding
            && self.0.modem.call_binding_current(session)
    }
    pub(crate) fn borrow_call_mode_voice_operation(
        &self,
        producer: &str,
        token: &str,
        sequence: u64,
    ) -> Result<CallModeVoiceGuard, Failure> {
        self.borrow_call_mode_voice_operation_scoped(producer, token, sequence, None, None)
    }

    fn borrow_call_mode_voice_operation_scoped(
        &self,
        producer: &str,
        token: &str,
        sequence: u64,
        expected_epoch: Option<u64>,
        settlement: Option<&crate::module_recovery::OperationRight>,
    ) -> Result<CallModeVoiceGuard, Failure> {
        self.require_module_admission()?;
        valid_token(producer)?;
        let id = identity(token, sequence)?;
        // Share the complete radio job admission boundary: an action can await
        // preferences/observation before it records its physical attempt.
        let _gate = self.0.gate.try_lock().map_err(|_| Failure::busy())?;
        let slot = self
            .0
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::busy())?;
        let mut state = self.0.state.lock().unwrap();
        if state.finalized {
            return Err(Failure::stopped());
        }
        if state.retiring
            || state.producer != producer
            || expected_epoch.is_some_and(|epoch| epoch != state.epoch)
        {
            return Err(Failure::stale());
        }
        if settlement.is_some_and(|right| !right.is_active_for(self.module_recovery(), producer)) {
            return Err(Self::admission_failure());
        }
        let epoch = state.epoch;
        let observed_uncertain = state.observed.uncertain;
        let reservation = state.reservation.as_mut().ok_or_else(Failure::stale)?;
        if reservation.purpose != Purpose::CallMode || reservation.identity != id {
            return Err(Failure::stale());
        }
        // Only the retained voice registry's operation right authorizes its
        // exact Stop settlement through an uncertain radio outcome. Ordinary
        // Start/status borrowing keeps the existing uncertainty fence.
        if (reservation.unknown || observed_uncertain) && settlement.is_none() {
            return Err(Failure::new(
                StatusCode::CONFLICT,
                "radio_outcome_unknown",
                "上次射频操作结果未确认，请先刷新核对",
            ));
        }
        let operation_lock = reservation
            .operation_lock
            .as_ref()
            .ok_or_else(Failure::stale)?
            .clone();
        if reservation.active_voice_pins != 0
            || reservation.active_reconnect_pins != 0
            || reservation
                .attempts
                .values()
                .any(|attempt| attempt.result.is_none())
        {
            return Err(Failure::busy());
        }
        let operation = reservation.operation.clone();
        if !operation
            .right()
            .is_active_for(self.module_recovery(), producer)
        {
            return Err(Self::admission_failure());
        }
        reservation.active_voice_pins += 1;
        state.jobs += 1;
        Ok(CallModeVoiceGuard {
            inner: self.0.clone(),
            operation_lock,
            _operation: operation,
            identity: id,
            epoch,
            _slot: slot,
        })
    }

    /// Pin one ordinary voice transition until ADB and USB cleanup settle.
    /// Cleanup remains admissible during shutdown, while new startup does not.
    pub(crate) fn borrow_shared_voice_operation(
        &self,
        producer: &str,
        cleanup: bool,
    ) -> Result<SharedVoiceGuard, Failure> {
        self.require_module_admission()?;
        let operation = Arc::new(
            self.module_recovery()
                .admit_operation(producer)
                .map_err(|_| Self::admission_failure())?,
        );
        self.borrow_shared_voice_inner(producer, cleanup, operation.right(), Some(operation))
    }

    /// The voice registry owns this right's guard and captured target until
    /// the worker has acknowledged Stop and released USB. The cleanup bool
    /// on the ordinary entry point cannot manufacture this authority.
    pub(crate) fn borrow_shared_voice_settlement(
        &self,
        producer: &str,
        right: &crate::module_recovery::OperationRight,
    ) -> Result<SharedVoiceGuard, Failure> {
        self.borrow_shared_voice_inner(producer, true, right.clone(), None)
    }

    fn borrow_shared_voice_inner(
        &self,
        producer: &str,
        cleanup: bool,
        right: crate::module_recovery::OperationRight,
        operation: Option<Arc<crate::module_recovery::OperationGuard>>,
    ) -> Result<SharedVoiceGuard, Failure> {
        self.require_module_admission()?;
        valid_token(producer)?;
        let _gate = self.0.gate.try_lock().map_err(|_| Failure::busy())?;
        let slot = self
            .0
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::busy())?;
        let mut state = self.0.state.lock().unwrap();
        if state.finalized {
            return Err(Failure::stopped());
        }
        if state.retiring || state.producer != producer {
            return Err(Failure::stale());
        }
        if !right.is_active_for(self.module_recovery(), producer) {
            return Err(Self::admission_failure());
        }
        if state.stopping && !cleanup {
            return Err(Failure::stopped());
        }
        if state.jobs != 0
            || state.active_shared_voice_pins != 0
            || state.reservation.as_ref().is_some_and(|reservation| {
                reservation.operation_lock.is_some()
                    || reservation.active_voice_pins != 0
                    || !cleanup && reservation.purpose == Purpose::Esim
            })
        {
            return Err(Failure::busy());
        }
        state.active_shared_voice_pins += 1;
        state.jobs += 1;
        Ok(SharedVoiceGuard {
            inner: self.0.clone(),
            _operation: operation,
            _right: right,
            _slot: slot,
        })
    }

    pub(crate) async fn borrow_shared_voice_shutdown(
        &self,
        producer: &str,
    ) -> Result<SharedVoiceGuard, Failure> {
        self.require_module_admission()?;
        let operation = Arc::new(
            self.module_recovery()
                .admit_operation(producer)
                .map_err(|_| Self::admission_failure())?,
        );
        self.borrow_shared_voice_shutdown_inner(producer, operation.right(), Some(operation))
            .await
    }

    pub(crate) async fn borrow_shared_voice_shutdown_settlement(
        &self,
        producer: &str,
        right: &crate::module_recovery::OperationRight,
    ) -> Result<SharedVoiceGuard, Failure> {
        self.borrow_shared_voice_shutdown_inner(producer, right.clone(), None)
            .await
    }

    /// Wait without holding the actor gate so accepted workers can finish.
    /// The retained operation right is checked again after each wait.
    async fn borrow_shared_voice_shutdown_inner(
        &self,
        producer: &str,
        right: crate::module_recovery::OperationRight,
        operation: Option<Arc<crate::module_recovery::OperationGuard>>,
    ) -> Result<SharedVoiceGuard, Failure> {
        self.require_module_admission()?;
        valid_token(producer)?;
        loop {
            let finished = self.0.finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            let slot = self
                .0
                .slots
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| Failure::stopped())?;
            let gate = self.0.gate.lock().await;
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.finalized {
                    return Err(Failure::stopped());
                }
                if state.retiring || state.producer != producer {
                    return Err(Failure::stale());
                }
                if !right.is_active_for(self.module_recovery(), producer) {
                    return Err(Self::admission_failure());
                }
                if state.jobs == 0 && state.active_shared_voice_pins == 0 {
                    if state.reservation.as_ref().is_some_and(|reservation| {
                        reservation.operation_lock.is_some() || reservation.active_voice_pins != 0
                    }) {
                        return Err(Failure::busy());
                    }
                    state.active_shared_voice_pins += 1;
                    state.jobs += 1;
                    return Ok(SharedVoiceGuard {
                        inner: self.0.clone(),
                        _operation: operation,
                        _right: right,
                        _slot: slot,
                    });
                }
            }
            drop(slot);
            drop(gate);
            finished.await;
        }
    }

    pub(crate) async fn reserve_esim(&self) -> Result<EsimReservation, Failure> {
        let sequence = NEXT_NATIVE_ESIM_RESERVATION.fetch_add(1, Ordering::Relaxed);
        if sequence == 0 || sequence == u64::MAX {
            return Err(Failure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "module_esim_reservation_exhausted",
                "eSIM 模块事务序号不可用",
            ));
        }
        let (producer_id, epoch) = {
            let state = self.0.state.lock().unwrap();
            (state.producer.clone(), state.epoch)
        };
        let token = format!("rust-esim-{sequence}");
        self.submit(Job::Reservation(ReservationRequest {
            producer_id: producer_id.clone(),
            action: "acquire".into(),
            purpose: Purpose::Esim,
            token: token.clone(),
            sequence,
        }))
        .await?;
        Ok(EsimReservation {
            producer_id,
            epoch,
            token,
            sequence,
        })
    }

    /// Reads USB and IMS/VoLTE call-mode facts under one physical radio
    /// reservation. An existing call/call-mode reservation is borrowed; when
    /// no compatible reservation exists this method owns and releases one.
    pub(crate) async fn call_mode_readiness_facts(
        &self,
    ) -> Result<CallModeReadinessFacts, Failure> {
        let (producer_id, identity, purpose, owned) = {
            let state = self
                .0
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match state.reservation.as_ref() {
                Some(reservation)
                    if matches!(reservation.purpose, Purpose::Call | Purpose::CallMode) =>
                {
                    (
                        state.producer.clone(),
                        reservation.identity.clone(),
                        reservation.purpose,
                        false,
                    )
                }
                Some(_) => return Err(Failure::busy()),
                None => {
                    let sequence = NEXT_NATIVE_ESIM_RESERVATION.fetch_add(1, Ordering::Relaxed);
                    if sequence == 0 || sequence == u64::MAX {
                        return Err(Failure::new(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "module_call_mode_reservation_exhausted",
                            "模块通话模式读取序号不可用",
                        ));
                    }
                    (
                        state.producer.clone(),
                        Identity {
                            token: format!("rust-call-read-{sequence}"),
                            sequence,
                        },
                        Purpose::CallMode,
                        true,
                    )
                }
            }
        };

        if owned {
            self.submit(Job::Reservation(ReservationRequest {
                producer_id: producer_id.clone(),
                action: "acquire".into(),
                purpose,
                token: identity.token.clone(),
                sequence: identity.sequence,
            }))
            .await?;
        }

        let result = async {
            let usb_value = self
                .call_mode_read_action(
                    &producer_id,
                    purpose,
                    &identity,
                    "call_mode_usb_composition_read",
                )
                .await
                .map_err(|error| {
                    Failure::new(
                        error.status,
                        error.code,
                        &format!("读取模块 USB 配置：{}", error.message),
                    )
                })?;
            let usb = parse_call_mode_read_fact::<UsbComposition>(
                &usb_value,
                "usb_composition",
                "call_mode_usb_composition_read",
            )?;
            let usb_target = parse_call_mode_read_fact::<UsbComposition>(
                &usb_value,
                "usb_composition_target",
                "call_mode_usb_composition_read",
            )?;
            if !usb.valid() || !usb_target.valid() {
                return Err(Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "module_radio_outcome_unconfirmed",
                    "Rust USBCFG 查询未返回已验证配置及通话目标",
                ));
            }
            if usb_target.vendor_id != usb.vendor_id
                || usb_target.product_id != usb.product_id
                || usb_target.flags[5] != 1
                || usb_target.flags[6] != 1
                || usb.flags[..5] != usb_target.flags[..5]
            {
                return Err(Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "module_radio_outcome_unconfirmed",
                    "Rust 通话模式目标配置无效或改变了其他 USB 功能位",
                ));
            }

            if usb.flags[5] != 1 || usb.flags[6] != 1 {
                return Ok(CallModeReadinessFacts { usb, voice: None });
            }

            let voice_value = self
                .call_mode_read_action(&producer_id, purpose, &identity, "call_mode_voice_read")
                .await
                .map_err(|error| {
                    Failure::new(
                        error.status,
                        error.code,
                        &format!("读取 IMS/VoLTE 配置：{}", error.message),
                    )
                })?;
            let voice = parse_call_mode_read_fact::<VoiceConfiguration>(
                &voice_value,
                "voice",
                "call_mode_voice_read",
            )?;
            if !voice.valid() {
                return Err(Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "module_radio_outcome_unconfirmed",
                    "Rust IMS/VoLTE 查询未返回已验证状态",
                ));
            }
            Ok(CallModeReadinessFacts {
                usb,
                voice: Some(voice),
            })
        }
        .await;

        if owned {
            let release = self
                .submit(Job::Reservation(ReservationRequest {
                    producer_id,
                    action: "release".into(),
                    purpose,
                    token: identity.token,
                    sequence: identity.sequence,
                }))
                .await;
            if let Err(release_error) = release {
                return match result {
                    Ok(_) => Err(release_error),
                    Err(error) => Err(Failure::new(
                        StatusCode::FAILED_DEPENDENCY,
                        "module_call_mode_read_release_unconfirmed",
                        &format!(
                            "{}；射频读取预约释放未确认：{}",
                            error.message, release_error.message
                        ),
                    )),
                };
            }
        }
        result
    }

    async fn call_mode_read_action(
        &self,
        producer_id: &str,
        purpose: Purpose,
        identity: &Identity,
        kind: &str,
    ) -> Result<Value, Failure> {
        let sequence = NEXT_CALL_MODE_READ_OPERATION.fetch_add(1, Ordering::Relaxed);
        if sequence == 0 || sequence == u64::MAX {
            return Err(Failure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "module_call_mode_operation_exhausted",
                "模块通话模式读取操作序号不可用",
            ));
        }
        self.submit(Job::Action(RadioAction {
            producer_id: producer_id.into(),
            purpose,
            token: identity.token.clone(),
            sequence: identity.sequence,
            operation_id: format!("rust-call-read-{sequence}"),
            kind: kind.into(),
            target: None,
            command: None,
            timeout_ms: None,
            voice_target: None,
            usb_expected: None,
            usb_target: None,
        }))
        .await
    }

    async fn reserve_device_sms(&self) -> Result<DeviceSmsReservation, Failure> {
        let sequence = NEXT_NATIVE_ESIM_RESERVATION.fetch_add(1, Ordering::Relaxed);
        if sequence == 0 || sequence == u64::MAX {
            return Err(Failure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "module_sms_reservation_exhausted",
                "模块短信事务序号不可用",
            ));
        }
        let producer_id = self.0.state.lock().unwrap().producer.clone();
        let token = format!("rust-sms-{sequence}");
        self.submit(Job::Reservation(ReservationRequest {
            producer_id: producer_id.clone(),
            action: "acquire".into(),
            purpose: Purpose::DeviceSms,
            token: token.clone(),
            sequence,
        }))
        .await?;
        Ok(DeviceSmsReservation {
            producer_id,
            token,
            sequence,
        })
    }

    async fn reserve_device_sms_wait(&self) -> Result<DeviceSmsReservation, Failure> {
        let deadline = Instant::now() + Duration::from_secs(150);
        loop {
            match self.reserve_device_sms().await {
                Ok(reservation) => return Ok(reservation),
                Err(error) if error.code == "module_radio_busy" => {
                    let (intent, retiring, reserved) = {
                        let state = self.0.state.lock().unwrap();
                        (
                            state.intent.is_some(),
                            state.retiring,
                            state.reservation.is_some(),
                        )
                    };
                    if intent || retiring || !reserved || Instant::now() >= deadline {
                        return Err(error);
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn release_device_sms(&self, reservation: DeviceSmsReservation) -> Result<(), Failure> {
        self.submit(Job::Reservation(ReservationRequest {
            producer_id: reservation.producer_id,
            action: "release".into(),
            purpose: Purpose::DeviceSms,
            token: reservation.token,
            sequence: reservation.sequence,
        }))
        .await
        .map(|_| ())
    }

    /// Submit the ordinary serial-modem SMS while holding the same Rust
    /// module reservation used by eSIM, radio and diagnostic workflows.
    /// The serial actor binds submission to the actor instance, physical
    /// session and SIM captured when the durable intent was prepared.
    /// Uncertain outcomes are never retried.
    pub(crate) async fn submit_device_sms(
        &self,
        recipient: String,
        message: String,
        expected: crate::modem_actor::SerialSmsBinding,
    ) -> Result<crate::modem_actor::SmsSubmitResult, Failure> {
        // Preserve the previous Go lock's wait-for-owner behavior for an
        // already-running module transaction. A live VoWiFi intent remains a
        // hard rejection. After waiting, both the configured route and the
        // actor's exact physical capability must still be the prepared route.
        let reservation = self.reserve_device_sms_wait().await?;
        let route_current = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            !state.stopping
                && !state.retiring
                && state.intent.is_none()
                && state
                    .config
                    .as_ref()
                    .is_some_and(|v| v.transport == "serial")
                && matches!(state.binding, Some(Binding::Serial { serial_session })
                    if serial_session == expected.serial_session)
        };
        let result = if route_current {
            let modem = self.0.modem.clone();
            tokio::task::spawn_blocking(move || {
                modem.submit_sms_bound(recipient, message, "modem".to_owned(), expected)
            })
            .await
            .map_err(|_| {
                Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "module_sms_actor_failed",
                    "模块短信执行器未确认结果",
                )
            })
            .and_then(|result| {
                result.map_err(|_| {
                    Failure::new(
                        StatusCode::BAD_GATEWAY,
                        "module_sms_actor_failed",
                        "串口执行器未确认短信结果",
                    )
                })
            })
        } else {
            Err(Failure::new(
                StatusCode::CONFLICT,
                "module_sms_route_changed",
                "模块短信通道或 SIM 身份已变化",
            ))
        };
        if self.release_device_sms(reservation).await.is_err() {
            return Err(Failure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "module_sms_reservation_release_unknown",
                "模块短信事务释放状态未确认",
            ));
        }
        result
    }

    /// Submit through the Rust USB AT owner while holding the same module
    /// reservation that excludes VoWiFi/eSIM work. Session and SIM generation
    /// are rechecked after reservation, closing the query-to-write race.
    pub(crate) async fn submit_usb_device_sms(
        &self,
        usb_at: crate::usb_at::Owner,
        expected_session: String,
        recipient: String,
        message: String,
        expected_generation: u64,
    ) -> Result<crate::usb_at::SmsSubmitOutcome, Failure> {
        let reservation = self.reserve_device_sms_wait().await?;
        let result = if self.vowifi_owns_sim_channel()
            || !usb_at.has_active_sim_identity_binding(&expected_session, expected_generation)
        {
            Err(Failure::new(
                StatusCode::CONFLICT,
                "module_sms_route_changed",
                "模块短信通道或 SIM 身份已变化",
            ))
        } else {
            usb_at
                .submit_active_sms(recipient, message, expected_generation, false)
                .await
                .map_err(|error| {
                    Failure::new(
                        error.public_status(),
                        error.public_code(),
                        &error.to_string(),
                    )
                })
        };
        if self.release_device_sms(reservation).await.is_err() {
            return Err(Failure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "module_sms_reservation_release_unknown",
                "模块短信事务释放状态未确认",
            ));
        }
        result
    }

    pub(crate) async fn execute_at_console(&self, command: String) -> Result<Value, Failure> {
        if command.is_empty() || command.len() > 4096 || command.chars().any(char::is_control) {
            return Err(Failure::invalid());
        }
        self.submit(Job::PublicAtConsole {
            command,
            deadline: Instant::now() + Duration::from_secs(24),
        })
        .await
    }
    /// Lightly reinitialize the module radio after an eUICC profile switch.
    /// The caller must retain the same eSIM reservation across the switch and
    /// its reconnect monitor; this keeps VoWiFi and other SIM-channel work out
    /// of the recovery window.
    pub(crate) async fn refresh_esim_profile(
        &self,
        reservation: &EsimReservation,
        operation_id: &str,
    ) -> Result<(), Failure> {
        self.submit(Job::Action(RadioAction {
            producer_id: reservation.producer_id.clone(),
            purpose: Purpose::Esim,
            token: reservation.token.clone(),
            sequence: reservation.sequence,
            operation_id: operation_id.to_owned(),
            kind: "refresh_profile".into(),
            target: None,
            command: None,
            timeout_ms: None,
            voice_target: None,
            usb_expected: None,
            usb_target: None,
        }))
        .await
        .map(|_| ())
    }
    /// Excludes call-mode setup/restore while a backup is being removed.
    /// This shares the same state lock as the Go call-mode radio reservation,
    /// so a reservation cannot start between the busy check and file removal.
    pub(crate) fn try_call_mode_backup_delete(&self) -> Option<CallModeBackupDeleteGuard> {
        let mut state = self.0.state.lock().unwrap();
        if state.call_mode_backup_delete_in_flight
            || state
                .reservation
                .as_ref()
                .is_some_and(|reservation| reservation.purpose == Purpose::CallMode)
        {
            return None;
        }
        state.call_mode_backup_delete_in_flight = true;
        Some(CallModeBackupDeleteGuard(self.0.clone()))
    }
    pub(crate) fn start(&self) {
        if self.module_recovery().is_admission_revoked() {
            return;
        }
        let mut task = self.0.task.lock().unwrap();
        let mut state = self.0.state.lock().unwrap();
        if task.is_some() || state.stopping {
            return;
        }
        state.running = true;
        *task = Some(tokio::spawn(worker(Arc::downgrade(&self.0))));
    }
    pub(crate) fn begin_shutdown(&self) {
        {
            let mut state = self.0.state.lock().unwrap();
            state.stopping = true;
            self.sync_suppression(&state);
        }
        self.0.wake.notify_waiters();
        self.0.finished.notify_waiters();
    }
    pub(crate) async fn finish_shutdown(&self) -> Result<(), Failure> {
        self.begin_shutdown();
        self.wait_idle(true).await;
        let task = self.0.task.lock().unwrap().take();
        if let Some(task) = task {
            task.await.map_err(|_| Failure::stopped())?;
        }
        let pending = {
            let state = self.0.state.lock().unwrap();
            state
                .intent
                .clone()
                .map(|intent| (state.producer.clone(), intent.identity))
        };
        if let Some((producer, id)) = pending {
            self.submit(Job::Intent(IntentRequest {
                producer_id: producer,
                action: "restore".into(),
                token: id.token,
                sequence: id.sequence,
            }))
            .await?;
        }
        self.wait_idle(true).await;
        Ok(())
    }
    /// Final-only physical receipt. A retained intent or unknown mutation is
    /// not restored merely because the scheduler and job queue are idle.
    pub(crate) async fn finish_shutdown_receipt(&self) -> Result<Vec<String>, String> {
        self.begin_shutdown();
        let this = self.clone();
        tokio::spawn(async move {
            let _gate = this.0.shutdown_gate.lock().await;
            {
                let mut receipt = this
                    .0
                    .shutdown_receipt
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(result) = &*receipt {
                    return result.clone();
                }
                *receipt = Some(Err("module radio finalizer unconfirmed".into()));
            }
            // Only one final restore attempt. An ambiguous write retains its
            // original intent/fence and cannot be replayed by coordinator polls.
            let physical = this.finish_shutdown().await;
            let observer = this
                .0
                .call_observation_task
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            let observation = match observer {
                Some(task) => task
                    .await
                    .map_err(|e| format!("module radio observer: {e}")),
                None => Ok(()),
            };
            let result = match (physical, observation) {
                (Err(error), _) => Err(format!("{}: {}", error.code, error.message)),
                (Ok(()), Err(error)) => Err(error),
                (Ok(()), Ok(())) => loop {
                    // Observer join can overlap an already accepted cleanup.
                    // Recheck its real job count, then atomically seal all new
                    // jobs/pins with the final physical-state proof.
                    let finished = this.0.finished.notified();
                    tokio::pin!(finished);
                    finished.as_mut().enable();
                    {
                        let mut state = this.0.state.lock().unwrap_or_else(|e| e.into_inner());
                        if state.jobs == 0 {
                            if state.intent.is_some()
                                || state.reservation.is_some()
                                || state.observed.uncertain
                                || state.mutating_in_flight
                                || state.profile_notes_unknown
                                || state.retiring
                                || state.active_shared_voice_pins != 0
                            {
                                break Err("module radio physical release unconfirmed".into());
                            }
                            state.finalized = true;
                            break Ok(Vec::new());
                        }
                    }
                    finished.await;
                },
            };
            *this
                .0
                .shutdown_receipt
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
            result
        })
        .await
        .map_err(|e| format!("module radio finalizer failed: {e}"))?
    }

    pub(crate) async fn retire_producer(&self, producer: String) -> Result<(), Failure> {
        valid_token(&producer)?;
        // The retained module network/card owner must drain before discovery or
        // compatibility shutdown retires its RF configuration, including the
        // window after actor hold acquisition and before intent.prepare.
        if self.0.modem.native_serial_managed() && self.0.modem.serial_module_held() {
            return Err(Failure::busy());
        }
        let (old, token, old_epoch) = {
            let mut state = self.0.state.lock().unwrap();
            // Serialize this second fact check with intent.prepare/enter. A
            // retain after it can only hold an intent whose epoch is retired;
            // it cannot publish an active RF owner before we fence that epoch.
            if self.0.modem.native_serial_managed() && self.0.modem.serial_module_held() {
                return Err(Failure::busy());
            }
            let old_epoch = state.epoch;
            state.retiring = true;
            state.epoch_cancel.store(true, Ordering::Release);
            state.epoch_cancel = Arc::new(AtomicBool::new(false));
            state.epoch = state.epoch.wrapping_add(1);
            let old = std::mem::replace(&mut state.producer, producer);
            let token = state.intent.as_ref().map(|v| v.identity.token.clone());
            state.binding = None;
            state.bootstrap_receipt = None;
            let unconfirmed = state.intent.as_ref().is_some_and(|intent| intent.active)
                || state.observed.uncertain
                || state.mutating_in_flight;
            state.observed = Outcome::default();
            state.observed.uncertain = unconfirmed;
            if unconfirmed {
                state.observed.error_code = "radio_producer_exit_unconfirmed".into();
            }
            self.sync_suppression(&state);
            (old, token, old_epoch)
        };
        self.0.wake.notify_waiters();
        self.wait_idle(false).await;
        let mut state = self.0.state.lock().unwrap();
        if let Some(token) = token {
            self.0
                .usb
                .set_radio_inhibit(&old, &token, false)
                .map_err(Failure::from)?;
        }
        let retired = state
            .reservation
            .as_ref()
            .filter(|reservation| matches!(reservation.purpose, Purpose::CallMode | Purpose::Call))
            .map(|reservation| {
                (
                    old_epoch,
                    old.clone(),
                    reservation.identity.token.clone(),
                    reservation.identity.sequence,
                )
            });
        if let Some(proof) = retired {
            if state
                .reservation
                .as_ref()
                .is_some_and(|lease| lease.purpose == Purpose::Call)
            {
                state.confirmed_call_releases.insert(proof);
            } else {
                state.confirmed_call_mode_releases.insert(proof);
            }
        }
        for purpose in [Purpose::CallMode, Purpose::Call] {
            if let Some(water) = state.waters.get(&purpose).filter(|water| water.released) {
                let proof = (old_epoch, old.clone(), water.token.clone(), water.sequence);
                if purpose == Purpose::Call {
                    state.confirmed_call_releases.insert(proof);
                } else {
                    state.confirmed_call_mode_releases.insert(proof);
                }
            }
        }
        state.config = None;
        state.config_seq = 0;
        state.reservation = None;
        state.waters.clear();
        state.intent = None;
        state.intent_water = Water::default();
        state.retiring = false;
        drop(state);
        self.0.finished.notify_waiters();
        let observer = self
            .0
            .call_observer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(observer) = observer {
            self.set_call_observer(observer);
        }
        Ok(())
    }
    async fn wait_idle(&self, worker: bool) {
        loop {
            let wait = self.0.finished.notified();
            tokio::pin!(wait);
            wait.as_mut().enable();
            let done = {
                let state = self.0.state.lock().unwrap();
                state.jobs == 0 && (!worker || !state.running)
            };
            if done {
                return;
            }
            wait.await;
        }
    }
    async fn submit(&self, job: Job) -> Result<Value, Failure> {
        self.submit_with_receipt(job)
            .await
            .map_err(|error| error.failure)
    }
    async fn submit_bound(&self, job: Job, binding: &str) -> Result<Value, Failure> {
        self.submit_with_binding(job, Some(binding))
            .await
            .map_err(|e| e.failure)
    }
    pub(crate) fn public_mutation_binding(&self) -> String {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        self.public_binding(&state)
    }
    fn public_binding(&self, state: &State) -> String {
        format!(
            "{}:{}:{}:{:?}",
            state.producer,
            state.epoch,
            state.config_seq,
            (state.binding.clone(), self.0.usb.public_mutation_binding())
        )
    }
    async fn submit_with_receipt(&self, job: Job) -> Result<Value, SubmissionFailure> {
        self.submit_with_binding(job, None).await
    }
    // A terminal seal rejects all new jobs. Only an exact original completed
    // obligation can replay its existing memory ACK; no generic intent handler,
    // high-water update, archive read, USB capture, or new operation is entered.
    fn completed_cleanup_receipt(job: &Job, state: &State) -> Result<Value, Failure> {
        if job.producer() != Some(state.producer.as_str()) {
            return Err(Failure::stale());
        }
        let restored = |input: &IntentRequest| {
            let id = identity(&input.token, input.sequence)?;
            if input.action == "restore"
                && state.intent.is_none()
                && state.intent_water.released
                && state.intent_water.token == id.token
                && state.intent_water.sequence == id.sequence
            {
                Ok(intent_response(state, &id, true))
            } else {
                Err(Failure::stopped())
            }
        };
        match job {
            Job::Intent(input) => restored(input),
            Job::NativeIntent(request) => {
                request.validate_locked(state, state.epoch)?;
                restored(&request.input)
            }
            Job::Reservation(input) if input.action == "release" => {
                let id = identity(&input.token, input.sequence)?;
                if state.reservation.is_none()
                    && state.waters.get(&input.purpose).is_some_and(|water| {
                        water.released && water.token == id.token && water.sequence == id.sequence
                    })
                {
                    Ok(json!({"token":id.token,"sequence":id.sequence,"released":true}))
                } else {
                    Err(Failure::stopped())
                }
            }
            _ => Err(Failure::stopped()),
        }
    }

    async fn submit_with_binding(
        &self,
        job: Job,
        expected_binding: Option<&str>,
    ) -> Result<Value, SubmissionFailure> {
        self.require_module_admission()?;
        if let Some(producer) = job.producer() {
            valid_token(producer)?;
        }
        {
            let state = self.0.state.lock().unwrap();
            if state.finalized {
                if expected_binding.is_some() {
                    return Err(Failure::stopped().into());
                }
                return Self::completed_cleanup_receipt(&job, &state).map_err(Into::into);
            }
        }
        let slot = self
            .0
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::busy())?;
        let (epoch, operation) = {
            let mut state = self.0.state.lock().unwrap();
            if state.finalized {
                if expected_binding.is_some() {
                    return Err(Failure::stopped().into());
                }
                return Self::completed_cleanup_receipt(&job, &state).map_err(Into::into);
            }
            if expected_binding.is_some_and(|expected| self.public_binding(&state) != expected) {
                return Err(Failure::stale().into());
            }
            if state.retiring {
                return Err(Failure::stale().into());
            }
            if job
                .producer()
                .is_some_and(|producer| producer != state.producer)
            {
                return Err(Failure::stale().into());
            }
            if state.active_shared_voice_pins != 0 {
                return Err(Failure::busy().into());
            }
            let (operation, continuation) = self.admit_job(&job, &state)?;
            if state.stopping && !continuation && !job.cleanup() {
                return Err(Failure::stopped().into());
            }
            state.jobs += 1;
            (state.epoch, operation)
        };
        let service = self.clone();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let _done = JobFinished(service.0.clone());
            let _slot = slot;
            let result = service.execute(job, epoch, operation).await;
            let _ = send.send(result);
        });
        receive
            .await
            .map_err(|_| {
                // The actor owned the job before its reply disappeared. It may
                // have reached physical I/O, so a reboot caller must retain an
                // unknown-submission receipt for the durable recovery ledger.
                let mut failure = Failure::stopped();
                failure.message = "射频操作结果未确认".into();
                failure.submitted = true;
                failure.uncertain = true;
                SubmissionFailure {
                    failure,
                    admitted: true,
                }
            })?
            .map_err(|failure| SubmissionFailure {
                failure,
                admitted: true,
            })
    }
    fn fence(&self, epoch: u64) -> Result<(), Failure> {
        self.require_module_admission()?;
        let state = self.0.state.lock().unwrap();
        if state.epoch != epoch || state.retiring {
            Err(Failure::stale())
        } else {
            Ok(())
        }
    }
    async fn load_preferences(&self, epoch: u64) -> Result<Preferences, Failure> {
        let store = self.0.store.clone();
        let loaded = crate::archive_io::run(move || store.load()).await;
        self.fence(epoch)?;
        let mut state = self.0.state.lock().unwrap();
        match loaded {
            Ok(preferences) => {
                state.preferences = Some(preferences);
                state.preference_error = false;
                self.sync_suppression(&state);
                Ok(preferences)
            }
            Err(_) => {
                state.preference_error = true;
                self.sync_suppression(&state);
                Err(Failure::policy())
            }
        }
    }
    async fn save_preferences(&self, preferences: Preferences, epoch: u64) -> Result<(), Failure> {
        if preferences.suppressed() {
            self.0.usb.set_cellular_suppressed(true);
            self.0.modem.set_cellular_suppressed(true);
        }
        let store = self.0.store.clone();
        let result = crate::archive_io::run(move || store.save(preferences)).await;
        self.fence(epoch)?;
        let mut state = self.0.state.lock().unwrap();
        if result.is_err() {
            state.preference_error = true;
            self.sync_suppression(&state);
            return Err(Failure::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "flight_mode_state_write_failed",
                "保存飞行模式恢复状态失败",
            ));
        }
        state.preferences = Some(preferences);
        state.preference_error = false;
        self.sync_suppression(&state);
        Ok(())
    }
    fn sync_suppression(&self, state: &State) {
        let suppressed = suppressed(state);
        self.0.usb.set_cellular_suppressed(suppressed);
        self.0.modem.set_cellular_suppressed(suppressed);
    }
    fn projection(&self) -> Value {
        let state = self.0.state.lock().unwrap();
        status(&state)
    }
    async fn physical(&self, action: Action, epoch: u64) -> Result<Outcome, Failure> {
        self.physical_with_commit(action, epoch, None).await
    }
    async fn physical_with_commit(
        &self,
        action: Action,
        epoch: u64,
        commit: Option<CallModeCommitProof>,
    ) -> Result<Outcome, Failure> {
        self.require_module_admission()?;
        let preserve_rf = matches!(
            action,
            Action::CellularCallRead
                | Action::CellularCall(_)
                | Action::CallModeIdleRead
                | Action::CallModeIdentityRead
                | Action::CallModeVoiceRead
                | Action::CallModeUsbCompositionRead
                | Action::CallModeAdbProbe
        );
        let (binding, inhibit, budget, cancelled) = {
            let state = self.0.state.lock().unwrap();
            if state.epoch != epoch || state.retiring {
                return Err(Failure::stale());
            }
            // Refresh/public observations also reach physical I/O without
            // going through action(); an ADB child pin excludes those too.
            if state.active_shared_voice_pins != 0
                || state
                    .reservation
                    .as_ref()
                    .is_some_and(|reservation| reservation.active_voice_pins != 0)
            {
                return Err(Failure::busy());
            }
            if matches!(action, Action::ProfileNotes(_))
                && state.observed.uncertain
                && !state.profile_notes_unknown
            {
                let mut failure = Failure::new(
                    StatusCode::CONFLICT,
                    "radio_outcome_unknown",
                    "模块上次命令结果未确认，请重新连接模块后再探测",
                );
                failure.uncertain = true;
                failure.submitted = state.observed.submitted;
                return Err(failure);
            }
            (
                state.binding.clone().ok_or_else(Failure::unavailable)?,
                state.intent.as_ref().map(|v| v.identity.token.clone()),
                state
                    .job_deadline
                    .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                    .unwrap_or(Duration::ZERO),
                state.epoch_cancel.clone(),
            )
        };
        let right = self.current_operation_right()?;
        if let Some(proof) = &commit {
            proof.validate_for_binding(&action, &binding)?;
        }
        let producer = self.0.state.lock().unwrap().producer.clone();
        if budget.is_zero() {
            let state = self.0.state.lock().unwrap();
            let mut failure = Failure::new(
                StatusCode::GATEWAY_TIMEOUT,
                "radio_deadline",
                "射频操作整体时限已到，未提交后续命令",
            );
            failure.submitted = state.observed.submitted;
            failure.uncertain = state.observed.uncertain;
            return Err(failure);
        }
        {
            let mut state = self.0.state.lock().unwrap();
            state.mutating_in_flight = action.mutating();
            self.sync_suppression(&state);
        }
        #[cfg(test)]
        let fixture = self.0.physical_fixture.lock().unwrap().clone();
        #[cfg(test)]
        let result = if let Some(fixture) = fixture {
            let (send, receive) = oneshot::channel();
            fixture
                .send((action.clone(), send))
                .map_err(|_| Failure::unavailable())?;
            receive.await.map_err(|_| Failure::unavailable())?
        } else {
            crate::module_radio_transport::execute_with_budget_and_right(
                &self.0.usb,
                &self.0.modem,
                binding,
                action,
                inhibit,
                budget,
                cancelled,
                right,
                producer,
                commit,
            )
            .await
            .map_err(Failure::from)
        };
        #[cfg(not(test))]
        let result = crate::module_radio_transport::execute_with_budget_and_right(
            &self.0.usb,
            &self.0.modem,
            binding,
            action,
            inhibit,
            budget,
            cancelled,
            right,
            producer,
            commit,
        )
        .await
        .map_err(Failure::from);
        self.0.state.lock().unwrap().mutating_in_flight = false;
        if let Err(mut error) = self.fence(epoch) {
            // Retirement can cross a submitted transport error as well as a
            // submitted success. The stale generation must carry either receipt.
            match &result {
                Ok(outcome) => {
                    error.submitted = outcome.submitted;
                    error.uncertain = outcome.uncertain || outcome.submitted;
                }
                Err(physical) => {
                    error.submitted = physical.submitted;
                    error.uncertain = physical.uncertain || physical.submitted;
                }
            }
            return Err(error);
        }
        let mut state = self.0.state.lock().unwrap();
        if preserve_rf {
            if let Ok(outcome) = &result {
                state.observed.uncertain |= outcome.uncertain;
                if outcome.uncertain {
                    state.observed.error_code = outcome.error_code.clone();
                }
            } else if let Err(error) = &result {
                state.observed.uncertain |= error.uncertain;
            }
            self.sync_suppression(&state);
            return result;
        }
        match &result {
            Ok(outcome) => {
                let prior_uncertain = state.observed.uncertain;
                state.observed = outcome.clone();
                if !(outcome.complete && outcome.verified) {
                    state.observed.uncertain |= prior_uncertain;
                }
            }
            Err(error) => {
                let prior_uncertain = state.observed.uncertain;
                state.observed = Outcome::default();
                state.observed.uncertain = prior_uncertain || error.uncertain;
                state.observed.error_code = error.code.into();
            }
        }
        self.sync_suppression(&state);
        result
    }
    async fn execute(
        &self,
        job: Job,
        epoch: u64,
        operation: Option<Arc<crate::module_recovery::OperationGuard>>,
    ) -> Result<Value, Failure> {
        let deadline = match &job {
            Job::PublicNetworkDiagnostic { deadline } => *deadline,
            Job::PublicAtConsole { deadline, .. } => *deadline,
            Job::PublicUSBNetMode { deadline, .. } => *deadline,
            Job::PublicModuleProfileNotes(_) => Instant::now() + Duration::from_secs(55),
            Job::PublicModulePhonebookProbe => Instant::now() + Duration::from_secs(70),
            _ => Instant::now() + Duration::from_secs(30),
        };
        let _gate =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), self.0.gate.lock())
                .await
                .map_err(|_| Failure::busy())?;
        self.fence(epoch)?;
        self.0.state.lock().unwrap().executing_operation = operation;
        let _operation = ExecutingOperation(self.0.clone());
        self.0.state.lock().unwrap().job_deadline = Some(match job {
            Job::PublicNetworkDiagnostic { deadline } => deadline,
            Job::PublicAtConsole { deadline, .. } => deadline,
            Job::PublicUSBNetMode { deadline, .. } => deadline,
            Job::PublicModuleProfileNotes(_) => Instant::now() + Duration::from_secs(55),
            Job::PublicModulePhonebookProbe => Instant::now() + Duration::from_secs(70),
            _ => Instant::now() + Duration::from_secs(55),
        });
        match job {
            Job::NativeIdleObserve(request) => {
                request.validate(self, epoch)?;
                let outcome = self
                    .physical(Action::Observe { cellular: false }, epoch)
                    .await?;
                if !outcome.complete || !outcome.verified {
                    return Err(outcome_failure(&outcome, "flight_mode_read_failed"));
                }
                Ok(json!({"cfun_mode":outcome.cfun.ok_or_else(Failure::unavailable)?}))
            }
            Job::Query(_) => {
                let _ = self.load_preferences(epoch).await;
                let state = self.0.state.lock().unwrap();
                Ok(
                    json!({"preferences":state.preferences.unwrap_or_default(),"suppressed":suppressed(&state),"radio_owned":state.intent.is_some(),"status":status(&state),"error_code":if state.preference_error{"flight_mode_state_read_failed"}else{""}}),
                )
            }
            Job::Configure(input) => self.configure(input, epoch, None).await,
            Job::ConfigureNativeSerial {
                input,
                serial_session,
            } => {
                if !self.0.modem.native_serial_managed() {
                    return Err(Failure::invalid());
                }
                self.configure(input, epoch, Some(serial_session)).await
            }
            Job::Reservation(input) => {
                if matches!(
                    input.action.as_str(),
                    "acquire" | "acquire_call_mode_operation"
                ) {
                    self.load_preferences(epoch).await?;
                }
                self.reservation(input)
            }
            Job::RestoreReservation(input) => {
                self.load_preferences(epoch).await?;
                self.reservation_with_restore(input, true)
            }
            Job::Intent(input) => self.intent(input, epoch).await,
            Job::NativeIntent(request) => {
                request.validate(self, epoch)?;
                self.intent(request.input, epoch).await
            }
            Job::Action(input) => self.action(input, epoch).await,
            Job::CallModeRebind(input) => self.call_mode_rebind_inner(input, epoch).await,
            Job::CaptureRestorePhysicalBinding(lease) => {
                self.capture_restore_physical_binding_inner(lease, epoch)
                    .await
            }
            Job::CallModeOpen(input) => self.call_mode_open_inner(input, epoch).await,
            Job::CellularCall(input) => self.cellular_call_inner(input, epoch).await,
            Job::CallPoll => self.poll_call_capture(epoch).await,
            job @ (Job::Bootstrap(_) | Job::BootstrapNativeSerial { .. } | Job::Tick) => {
                if let Job::BootstrapNativeSerial { serial_session, .. } = &job {
                    if !self.0.modem.native_serial_managed()
                        || self.serial_transport_session() != Some(*serial_session)
                        || !self.0.modem.call_binding_current(*serial_session)
                    {
                        return Err(Failure::stale());
                    }
                }
                let bootstrap = !matches!(job, Job::Tick);
                let preferences = self.load_preferences(epoch).await?;
                let blocked = {
                    let state = self.0.state.lock().unwrap();
                    if state.binding.is_none() {
                        Some(Failure::unavailable())
                    } else if state.reservation.is_some() || state.intent.is_some() {
                        Some(Failure::busy())
                    } else if state.observed.uncertain {
                        let mut error = Failure::new(
                            StatusCode::CONFLICT,
                            "radio_outcome_unknown",
                            "上次射频操作结果未确认，请先刷新核对",
                        );
                        error.uncertain = true;
                        error.submitted = state.observed.submitted;
                        Some(error)
                    } else {
                        None
                    }
                };
                // A background tick may defer work; startup must acknowledge an
                // actual policy barrier before Go begins dependent SIM work.
                if let Some(error) = blocked {
                    return if bootstrap {
                        Err(error)
                    } else {
                        Ok(self.projection())
                    };
                }
                if preferences.suppressed() {
                    self.set(
                        SetRequest {
                            enabled: true,
                            user_initiated: false,
                        },
                        epoch,
                        false,
                    )
                    .await?;
                } else if bootstrap && !preferences.suppressed() {
                    let outcome = self
                        .physical(Action::Observe { cellular: false }, epoch)
                        .await?;
                    if !(outcome.complete && outcome.verified) {
                        return Err(outcome_failure(&outcome, "flight_mode_read_failed"));
                    }
                }
                if bootstrap {
                    self.fence(epoch)?;
                    let mut state = self.0.state.lock().unwrap();
                    state.bootstrap_receipt = state.binding.clone();
                }
                Ok(self.projection())
            }
            Job::Refresh(_) | Job::PublicGet => {
                let prefs = self.load_preferences(epoch).await;
                let (bound, allowed) = {
                    let state = self.0.state.lock().unwrap();
                    (
                        state.binding.is_some(),
                        prefs.as_ref().is_ok_and(|prefs| !prefs.suppressed())
                            && state.intent.is_none(),
                    )
                };
                if prefs.is_ok() && bound {
                    let result = self
                        .physical(Action::Observe { cellular: allowed }, epoch)
                        .await;
                    if let Ok(outcome) = result {
                        if outcome.complete && outcome.verified {
                            if let Some(reservation) =
                                self.0.state.lock().unwrap().reservation.as_mut()
                            {
                                reservation.unknown = false;
                            }
                        }
                    }
                }
                Ok(self.projection())
            }
            Job::PublicSet(input) => self.set(input, epoch, true).await,
            Job::PublicReboot => self.public_reboot(epoch).await,
            Job::PublicNetworkRecoveryReboot => self.public_reboot_inner(epoch, true).await,
            Job::PublicUSBNetMode {
                mode,
                deadline,
                expected_binding,
                read_wait,
            } => {
                {
                    let state = self.0.state.lock().unwrap();
                    if state.stopping {
                        return Err(Failure::stopped());
                    }
                    if self.public_binding(&state) != expected_binding {
                        return Err(Failure::stale());
                    }
                    if usbnet_temporary_read_owner(&state) {
                        if let Some(reply) = read_wait {
                            let _ = reply.send(UsbnetReadWait);
                        }
                        return Err(Failure::busy());
                    }
                }
                if Instant::now() >= deadline {
                    return Err(usbnet_wait_deadline());
                }
                self.public_usbnet_mode(mode, epoch).await
            }
            Job::PublicNetworkDiagnostic { deadline } => {
                self.public_network_diagnostic(deadline, epoch).await
            }
            Job::PublicAtConsole { command, deadline } => {
                self.public_at_console(command, deadline, epoch).await
            }
            Job::PublicModuleProfileNotes(request) => {
                self.public_module_profile_notes(request, epoch).await
            }
            Job::PublicModulePhonebookProbe => self.public_module_phonebook_probe(epoch).await,
        }
    }
    async fn public_module_profile_notes(
        &self,
        request: ModuleProfileNotesRequest,
        epoch: u64,
    ) -> Result<Value, Failure> {
        let (producer_id, sequence) = {
            let state = self.0.state.lock().unwrap();
            let sequence = state
                .waters
                .get(&Purpose::ModuleProfileNotes)
                .map_or(0, |water| water.sequence)
                .checked_add(1)
                .ok_or_else(|| {
                    Failure::new(
                        StatusCode::CONFLICT,
                        "radio_sequence_exhausted",
                        "射频请求序号已耗尽",
                    )
                })?;
            (state.producer.clone(), sequence)
        };
        let token = format!("rust-profile-notes-{}-{sequence}", std::process::id());
        let reservation = |action: &str| ReservationRequest {
            producer_id: producer_id.clone(),
            action: action.into(),
            purpose: Purpose::ModuleProfileNotes,
            token: token.clone(),
            sequence,
        };
        self.reservation(reservation("acquire"))?;
        let result = self
            .public_module_profile_notes_reserved(request, epoch)
            .await;
        let release = self.reservation(reservation("release"));
        match (result, release) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(_)) => Ok(value),
        }
    }
    async fn public_module_profile_notes_reserved(
        &self,
        request: ModuleProfileNotesRequest,
        epoch: u64,
    ) -> Result<Value, Failure> {
        {
            let state = self.0.state.lock().unwrap();
            if state.observed.uncertain && !state.profile_notes_unknown {
                return Err(Failure::new(
                    StatusCode::CONFLICT,
                    "radio_outcome_unknown",
                    "上次模块射频操作结果未确认，请先刷新核对",
                ));
            }
            if matches!(&request, ModuleProfileNotesRequest::Save(_)) && state.profile_notes_unknown
            {
                return Err(Failure::new(
                    StatusCode::CONFLICT,
                    "module_profile_notes_outcome_unknown",
                    "上次模块资料写入结果未知，请先读取资料核对后再操作",
                ));
            }
        }
        let (notes, occupied, used, total) = self.read_module_profile_notes(epoch).await?;
        match request {
            ModuleProfileNotesRequest::List => {
                let mut state = self.0.state.lock().unwrap();
                if state.profile_notes_unknown {
                    state.profile_notes_unknown = false;
                    state.observed.uncertain = false;
                    state.observed.submitted = false;
                    state.observed.error_code.clear();
                    self.sync_suppression(&state);
                }
                Ok(json!({"notes":notes,"used":used,"total":total}))
            }
            ModuleProfileNotesRequest::Save(note) => {
                let key = note.iccid.trim();
                let current = notes.get(key);
                if note.label.trim().is_empty()
                    && note.phone.trim().is_empty()
                    && note.tags.trim().is_empty()
                {
                    let Some(current) = current else {
                        return Ok(json!({"message":"模块资料库中没有此记录"}));
                    };
                    self.module_profile_notes_command(
                        ProfileNoteCommand::Delete {
                            index: current.index,
                        },
                        epoch,
                    )
                    .await
                    .map_err(|error| {
                        module_profile_notes_command_failure("删除模块资料失败", error)
                    })?;
                    return Ok(json!({"message":"模块资料已删除"}));
                }
                let encoded =
                    crate::module_profile_notes::encode_note(note.clone()).map_err(|message| {
                        Failure::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_module_profile_note",
                            message,
                        )
                    })?;
                let mut index = current.map_or(0, |current| current.index);
                if index == 0 {
                    index = (1..=total)
                        .find(|candidate| !occupied.contains(candidate))
                        .unwrap_or_default();
                }
                if index == 0 {
                    return Err(Failure::new(
                        StatusCode::CONFLICT,
                        "module_profile_notes_full",
                        "模块通讯录已满",
                    ));
                }
                self.module_profile_notes_command(
                    ProfileNoteCommand::Write {
                        index,
                        record: encoded,
                    },
                    epoch,
                )
                .await
                .map_err(|error| module_profile_notes_command_failure("写入模块资料失败", error))?;
                Ok(json!({"message":"模块资料已保存","index":index}))
            }
        }
    }
    async fn public_module_phonebook_probe(&self, epoch: u64) -> Result<Value, Failure> {
        let mut result = ModulePhonebookProbeResult::default();
        if self.0.state.lock().unwrap().binding.is_none() {
            result
                .responses
                .insert("AT+CPBS=?".into(), "DJI 模块未连接".into());
            return Ok(result.value());
        }
        let (producer_id, sequence) = {
            let state = self.0.state.lock().unwrap();
            let sequence = state
                .waters
                .get(&Purpose::ModuleProfileNotes)
                .map_or(0, |water| water.sequence)
                .checked_add(1)
                .ok_or_else(|| {
                    Failure::new(
                        StatusCode::CONFLICT,
                        "radio_sequence_exhausted",
                        "射频请求序号已耗尽",
                    )
                })?;
            (state.producer.clone(), sequence)
        };
        let token = format!("rust-phonebook-probe-{}-{sequence}", std::process::id());
        let reservation = |action: &str| ReservationRequest {
            producer_id: producer_id.clone(),
            action: action.into(),
            purpose: Purpose::ModuleProfileNotes,
            token: token.clone(),
            sequence,
        };
        self.reservation(reservation("acquire"))?;
        let operation = self
            .public_module_phonebook_probe_reserved(&mut result, epoch)
            .await;
        let release = self.reservation(reservation("release"));
        match (operation, release) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(()), Ok(_)) => Ok(result.value()),
        }
    }
    async fn public_module_phonebook_probe_reserved(
        &self,
        result: &mut ModulePhonebookProbeResult,
        epoch: u64,
    ) -> Result<(), Failure> {
        const TEST_STORAGE: &str = "AT+CPBS=?";
        if !self
            .module_phonebook_probe_command(
                TEST_STORAGE,
                ProfileNoteCommand::TestStorage,
                result,
                epoch,
            )
            .await
        {
            return Ok(());
        }
        result.storage_supported = phonebook_storage_supported(
            result
                .responses
                .get(TEST_STORAGE)
                .map(String::as_str)
                .unwrap_or_default(),
            "ME",
        );
        if !result.storage_supported {
            return Ok(());
        }
        const SELECT_STORAGE: &str = r#"AT+CPBS="ME""#;
        result.storage_selected = self
            .module_phonebook_probe_command(
                SELECT_STORAGE,
                ProfileNoteCommand::SelectME,
                result,
                epoch,
            )
            .await;
        if !result.storage_selected {
            return Ok(());
        }
        const STORAGE_STATUS: &str = "AT+CPBS?";
        if self
            .module_phonebook_probe_command(
                STORAGE_STATUS,
                ProfileNoteCommand::Status,
                result,
                epoch,
            )
            .await
        {
            result.storage_status = result
                .responses
                .get(STORAGE_STATUS)
                .cloned()
                .unwrap_or_default();
        }
        if result.uncertain {
            return Ok(());
        }
        result.read_supported = self
            .module_phonebook_probe_command(
                "AT+CPBR=?",
                ProfileNoteCommand::ReadSupport,
                result,
                epoch,
            )
            .await;
        if result.uncertain {
            return Ok(());
        }
        result.write_supported = self
            .module_phonebook_probe_command(
                "AT+CPBW=?",
                ProfileNoteCommand::WriteSupport,
                result,
                epoch,
            )
            .await;
        Ok(())
    }
    async fn module_phonebook_probe_command(
        &self,
        command_text: &str,
        command: ProfileNoteCommand,
        result: &mut ModulePhonebookProbeResult,
        epoch: u64,
    ) -> bool {
        match self.module_profile_notes_outcome(command, epoch).await {
            Ok(outcome) if outcome.uncertain => {
                result.uncertain = true;
                result.responses.insert(
                    command_text.into(),
                    if !outcome.response.trim().is_empty() {
                        outcome.response.trim().into()
                    } else {
                        outcome.error_code
                    },
                );
                false
            }
            Ok(outcome) if !outcome.response.trim().is_empty() => {
                let response = outcome.response.trim().to_owned();
                let succeeded = at_command_succeeded(&response);
                result.responses.insert(command_text.into(), response);
                succeeded
            }
            Ok(outcome) => {
                result.responses.insert(
                    command_text.into(),
                    if outcome.error_code.is_empty() {
                        "模块命令未返回响应".into()
                    } else {
                        outcome.error_code
                    },
                );
                false
            }
            Err(error) => {
                result.uncertain |= error.uncertain;
                result
                    .responses
                    .insert(command_text.into(), error.to_string());
                false
            }
        }
    }
    async fn read_module_profile_notes(
        &self,
        epoch: u64,
    ) -> Result<
        (
            BTreeMap<String, crate::module_profile_notes::Note>,
            BTreeSet<usize>,
            usize,
            usize,
        ),
        Failure,
    > {
        let selected = self
            .module_profile_notes_command(ProfileNoteCommand::SelectME, epoch)
            .await
            .map_err(|error| module_profile_notes_command_failure("读取模块资料库失败", error))?;
        let _ = selected;
        let status = self
            .module_profile_notes_command(ProfileNoteCommand::Status, epoch)
            .await
            .map_err(|error| module_profile_notes_command_failure("读取模块资料库失败", error))?;
        let (used, total) =
            crate::module_profile_notes::parse_phonebook_status(&status).map_err(|message| {
                Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "module_profile_notes_status_invalid",
                    &format!("读取模块资料库失败: {message}"),
                )
            })?;
        if total > 65_535 {
            return Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "module_profile_notes_capacity_unsupported",
                "读取模块资料库失败: ME 通讯录容量超出支持范围",
            ));
        }
        let mut notes = BTreeMap::new();
        let mut occupied = BTreeSet::new();
        if used == 0 || total == 0 {
            return Ok((notes, occupied, used, total));
        }
        let mut first = 1usize;
        while first <= total {
            let last = first
                .saturating_add(PROFILE_NOTES_BATCH_SIZE - 1)
                .min(total);
            let response = self
                .module_profile_notes_command(ProfileNoteCommand::Read { first, last }, epoch)
                .await
                .map_err(|error| {
                    module_profile_notes_command_failure("读取模块资料库失败", error)
                })?;
            let (batch_notes, batch_occupied) =
                crate::module_profile_notes::decode_phonebook(&response);
            for (iccid, note) in batch_notes {
                notes.insert(iccid, note);
            }
            occupied.extend(batch_occupied);
            first = last.saturating_add(1);
        }
        Ok((notes, occupied, used, total))
    }
    async fn module_profile_notes_command(
        &self,
        command: ProfileNoteCommand,
        epoch: u64,
    ) -> Result<String, Failure> {
        let outcome = self.module_profile_notes_outcome(command, epoch).await?;
        if !outcome.complete {
            let detail = if outcome.uncertain {
                "模块 AT 命令状态未知，请先读取资料核对"
            } else {
                "模块 AT 命令未成功"
            };
            let mut error = Failure::new(
                StatusCode::BAD_GATEWAY,
                "module_profile_notes_command_failed",
                detail,
            );
            error.uncertain = outcome.uncertain;
            error.submitted = outcome.submitted;
            return Err(error);
        }
        Ok(outcome.response)
    }
    async fn module_profile_notes_outcome(
        &self,
        command: ProfileNoteCommand,
        epoch: u64,
    ) -> Result<Outcome, Failure> {
        let prior = self.0.state.lock().unwrap().observed.clone();
        let result = self.physical(Action::ProfileNotes(command), epoch).await;
        let (uncertain, submitted, error_code) = match &result {
            Ok(outcome) => (
                outcome.uncertain,
                outcome.submitted,
                outcome.error_code.clone(),
            ),
            Err(error) => (error.uncertain, error.submitted, error.code.to_owned()),
        };
        {
            let mut state = self.0.state.lock().unwrap();
            if state.epoch == epoch && !state.retiring {
                let previous_operation = std::mem::replace(&mut state.observed, prior);
                if previous_operation.uncertain {
                    state.observed.uncertain = true;
                    state.observed.submitted |= previous_operation.submitted;
                    state.observed.error_code = previous_operation.error_code;
                }
                if uncertain {
                    state.observed.uncertain = true;
                    state.observed.submitted |= submitted;
                    state.observed.error_code = error_code;
                    state.profile_notes_unknown = true;
                }
                self.sync_suppression(&state);
            }
        }
        result
    }
    async fn public_at_console(
        &self,
        command: String,
        deadline: Instant,
        epoch: u64,
    ) -> Result<Value, Failure> {
        let sequence = {
            let state = self.0.state.lock().unwrap();
            if state.binding.is_none() {
                return Err(Failure::unavailable());
            }
            state
                .waters
                .get(&Purpose::Diagnostic)
                .map_or(0, |water| water.sequence)
                .checked_add(1)
                .ok_or_else(|| {
                    Failure::new(
                        StatusCode::CONFLICT,
                        "radio_sequence_exhausted",
                        "射频请求序号已耗尽",
                    )
                })?
        };
        let producer_id = self.0.state.lock().unwrap().producer.clone();
        let token = format!("rust-at-console-{}-{sequence}", std::process::id());
        let reservation = |action: &str| ReservationRequest {
            producer_id: producer_id.clone(),
            action: action.into(),
            purpose: Purpose::Diagnostic,
            token: token.clone(),
            sequence,
        };
        self.reservation(reservation("acquire"))?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = self.reservation(reservation("release"));
            return Err(Failure::new(
                StatusCode::GATEWAY_TIMEOUT,
                "at_transport_failed",
                "AT 命令整体时限已到，未提交命令",
            ));
        }
        let operation = RadioAction {
            producer_id: producer_id.clone(),
            purpose: Purpose::Diagnostic,
            token: token.clone(),
            sequence,
            operation_id: format!("rust-at-console-{}-{sequence}", std::process::id()),
            kind: "diagnostic".into(),
            target: None,
            command: Some(command),
            timeout_ms: Some(remaining.as_millis().min(20_000).max(1) as u64),
            voice_target: None,
            usb_expected: None,
            usb_target: None,
        };
        let action_result = self.action(operation, epoch).await;
        let release_result = self.reservation(reservation("release"));
        match action_result {
            Ok(outcome) => {
                release_result?;
                Ok(
                    json!({"response":outcome.get("response").and_then(Value::as_str).unwrap_or_default()}),
                )
            }
            // Preserve the established operator-controlled console behavior:
            // return an unknown result without replay, then let the user
            // inspect the modem before deciding what to do next.
            Err(error) => Err(error),
        }
    }

    async fn public_network_diagnostic(
        &self,
        deadline: Instant,
        epoch: u64,
    ) -> Result<Value, Failure> {
        const COMMANDS: [(&str, &str); 5] = [
            ("usbnet", r#"AT+QCFG="usbnet""#),
            ("usbcfg", r#"AT+QCFG="usbcfg""#),
            ("cgdcont", "AT+CGDCONT?"),
            ("cgact", "AT+CGACT?"),
            ("cgpaddr", "AT+CGPADDR=1"),
        ];
        let mut raw: BTreeMap<String, String> = BTreeMap::new();
        let mut errors: BTreeMap<String, String> = BTreeMap::new();
        let mut completed = 0usize;
        let sequence = {
            let state = self.0.state.lock().unwrap();
            state
                .waters
                .get(&Purpose::Diagnostic)
                .map_or(0, |water| water.sequence)
                .checked_add(1)
                .ok_or_else(|| {
                    Failure::new(
                        StatusCode::CONFLICT,
                        "radio_sequence_exhausted",
                        "射频请求序号已耗尽",
                    )
                })?
        };
        let producer_id = self.0.state.lock().unwrap().producer.clone();
        let token = format!("rust-netdiag-{}-{sequence}", std::process::id());
        let reservation = |action: &str| ReservationRequest {
            producer_id: producer_id.clone(),
            action: action.into(),
            purpose: Purpose::Diagnostic,
            token: token.clone(),
            sequence,
        };
        if let Err(error) = self.reservation(reservation("acquire")) {
            let message = diagnostic_error_text(&error);
            for (key, _) in COMMANDS {
                errors.insert(key.to_owned(), message.clone());
            }
            return Ok(json!({"raw":raw,"errors":errors,"completed_commands":completed}));
        }
        for (index, (key, command)) in COMMANDS.into_iter().enumerate() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                errors.insert(key.into(), "context deadline exceeded".into());
                continue;
            }
            let timeout_ms = remaining.as_millis().min(8_000).max(1) as u64;
            let operation = RadioAction {
                producer_id: producer_id.clone(),
                purpose: Purpose::Diagnostic,
                token: token.clone(),
                sequence,
                operation_id: format!("rust-netdiag-{}-{sequence}-{index}", std::process::id()),
                kind: "diagnostic".into(),
                target: None,
                command: Some(command.into()),
                timeout_ms: Some(timeout_ms),
                voice_target: None,
                usb_expected: None,
                usb_target: None,
            };
            match self.action(operation, epoch).await {
                Ok(value) => {
                    if let Some(response) = value.get("response").and_then(Value::as_str) {
                        raw.insert(key.into(), response.to_owned());
                    }
                    completed += 1;
                }
                Err(error) => {
                    errors.insert(key.into(), diagnostic_error_text(&error));
                    completed += 1;
                }
            }
        }
        if let Err(error) = self.reservation(reservation("release")) {
            errors
                .entry("cgpaddr".into())
                .or_insert_with(|| diagnostic_error_text(&error));
        }
        self.fence(epoch)?;
        Ok(json!({"raw":raw,"errors":errors,"completed_commands":completed}))
    }
    /// Retry only a gate-confirmed, never-dispatched collision with a temporary
    /// read. Waiting holds neither an actor slot nor the actor gate, so the
    /// original owner's release can always enter. All attempts share one bound.
    async fn submit_usbnet_mode_bound(&self, mode: i64, binding: &str) -> Result<Value, Failure> {
        self.submit_usbnet_mode_until(mode, binding, Instant::now() + USBNET_DEADLINE)
            .await
    }

    async fn submit_usbnet_mode_until(
        &self,
        mode: i64,
        binding: &str,
        deadline: Instant,
    ) -> Result<Value, Failure> {
        let epoch = self.0.state.lock().unwrap().epoch;
        loop {
            self.require_new_module_admission()?;
            {
                let state = self.0.state.lock().unwrap();
                if state.stopping {
                    return Err(Failure::stopped());
                }
                if state.retiring || state.epoch != epoch || self.public_binding(&state) != binding
                {
                    return Err(Failure::stale());
                }
            }
            if Instant::now() >= deadline {
                return Err(usbnet_wait_deadline());
            }
            let (send, mut waiting) = oneshot::channel();
            let result = self
                .submit_bound(
                    Job::PublicUSBNetMode {
                        mode,
                        deadline,
                        expected_binding: binding.to_owned(),
                        read_wait: Some(send),
                    },
                    binding,
                )
                .await;
            if !matches!(waiting.try_recv(), Ok(UsbnetReadWait)) {
                return result;
            }
            // The marker is only issued before any reservation/action. Never
            // reinterpret an actual submitted/uncertain transport result.
            if !matches!(&result, Err(error) if !error.submitted && !error.uncertain) {
                return result;
            }
            #[cfg(test)]
            let _waiting = UsbnetWaiter::new(self.0.clone());
            loop {
                let changed = self.0.finished.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                self.require_new_module_admission()?;
                {
                    let state = self.0.state.lock().unwrap();
                    if state.stopping {
                        return Err(Failure::stopped());
                    }
                    if state.retiring
                        || state.epoch != epoch
                        || self.public_binding(&state) != binding
                    {
                        return Err(Failure::stale());
                    }
                    // Ignore notifications from our own rejected job or
                    // unrelated reads. Only resubmit when the owner changed.
                    if !usbnet_temporary_read_owner(&state) {
                        break;
                    }
                }
                tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), changed)
                    .await
                    .map_err(|_| usbnet_wait_deadline())?;
            }
        }
    }

    async fn public_usbnet_mode(&self, mode: i64, epoch: u64) -> Result<Value, Failure> {
        let sequence = {
            let state = self.0.state.lock().unwrap();
            state
                .waters
                .get(&Purpose::Diagnostic)
                .map_or(0, |water| water.sequence)
                .checked_add(1)
                .ok_or_else(|| {
                    Failure::new(
                        StatusCode::CONFLICT,
                        "radio_sequence_exhausted",
                        "射频请求序号已耗尽",
                    )
                })?
        };
        let producer_id = self.0.state.lock().unwrap().producer.clone();
        let token = format!("rust-usbnet-{}-{sequence}", std::process::id());
        let reservation = |action: &str| ReservationRequest {
            producer_id: producer_id.clone(),
            action: action.into(),
            purpose: Purpose::Diagnostic,
            token: token.clone(),
            sequence,
        };
        self.reservation(reservation("acquire"))?;

        let operation = RadioAction {
            producer_id: producer_id.clone(),
            purpose: Purpose::Diagnostic,
            token: token.clone(),
            sequence,
            operation_id: format!("rust-usbnet-{}-{sequence}-submit", std::process::id()),
            kind: "diagnostic".into(),
            target: None,
            command: Some(format!(r#"AT+QCFG="usbnet",{mode}"#)),
            timeout_ms: Some(8_000),
            voice_target: None,
            usb_expected: None,
            usb_target: None,
        };
        let action_result = self.action(operation, epoch).await;
        let release_result = self.reservation(reservation("release"));
        match action_result {
            Ok(outcome) => {
                release_result?;
                Ok(json!({
                    "mode": mode,
                    "response": outcome.get("response").and_then(Value::as_str).unwrap_or_default(),
                    "needs_reboot": true,
                }))
            }
            Err(error) => Err(error),
        }
    }
    async fn public_reboot(&self, epoch: u64) -> Result<Value, Failure> {
        self.public_reboot_inner(epoch, false).await
    }
    /// A retained recovery worker enters the existing actor gate. USB mode
    /// observation and the call-safe reboot share one exact reservation.
    pub(crate) async fn reboot_module(&self, binding: &str) -> Result<(), Failure> {
        self.submit_bound(Job::PublicReboot, binding)
            .await
            .map(|_| ())
    }

    /// Network-recovery uses the same physical actor but retains its stricter
    /// USB-network admission checks. Both entry points are fenced by the
    /// durable network-recovery ledger before this dispatcher is reached.
    pub(crate) async fn recover_network_reboot(&self, binding: &str) -> Result<(), Failure> {
        self.submit_bound(Job::PublicNetworkRecoveryReboot, binding)
            .await
            .map(|_| ())
    }
    async fn public_reboot_inner(
        &self,
        epoch: u64,
        network_recovery: bool,
    ) -> Result<Value, Failure> {
        let preferences = self.load_preferences(epoch).await?;
        if preferences.enabled {
            return Err(Failure::new(
                StatusCode::CONFLICT,
                "module_flight_mode_enabled",
                "模块飞行模式已开启；请先关闭飞行模式后再重启模块",
            ));
        }
        let (producer_id, sequence) = {
            let state = self.0.state.lock().unwrap();
            if state.binding.is_none() {
                return Err(Failure::unavailable());
            }
            let sequence = state
                .waters
                .get(&Purpose::Reboot)
                .map_or(0, |water| water.sequence)
                .checked_add(1)
                .ok_or_else(|| {
                    Failure::new(
                        StatusCode::CONFLICT,
                        "radio_sequence_exhausted",
                        "模块射频请求序号已耗尽",
                    )
                })?;
            (state.producer.clone(), sequence)
        };
        let token = format!("rust-reboot-{}-{sequence}", std::process::id());
        let reservation = |action: &str| ReservationRequest {
            producer_id: producer_id.clone(),
            action: action.into(),
            purpose: Purpose::Reboot,
            token: token.clone(),
            sequence,
        };
        self.reservation(reservation("acquire"))?;

        if network_recovery {
            let observed = self
                .physical(
                    Action::Diagnostic {
                        command: r#"AT+QCFG="usbnet""#.into(),
                        timeout_ms: 8_000,
                    },
                    epoch,
                )
                .await;
            let checked = match observed {
                Ok(outcome)
                    if outcome.complete
                        && outcome.verified
                        && crate::network_diagnostic::parse_usbnet_mode(&outcome.response)
                            == "1" =>
                {
                    Ok(())
                }
                Ok(_) => Err(Failure::new(
                    StatusCode::CONFLICT,
                    "network_usbnet_not_enabled",
                    "USB 网卡模式尚未确认开启",
                )),
                Err(mut error) => {
                    // These are observations, not a submitted reboot.
                    error.submitted = false;
                    Err(error)
                }
            };
            if let Err(error) = checked {
                // No reboot has been submitted. The observation cannot leave
                // a temporary reservation behind after a rejected request.
                self.reservation(reservation("release"))?;
                return Err(error);
            }
            let calls = self.physical(Action::CellularCallRead, epoch).await;
            let checked = match calls {
                Ok(outcome)
                    if outcome.complete
                        && outcome.verified
                        && call_snapshot(outcome.response.clone()).safe_idle =>
                {
                    Ok(())
                }
                Ok(_) => Err(Failure::new(
                    StatusCode::CONFLICT,
                    "radio_call_busy",
                    "通话状态尚未确认空闲，未提交网络恢复重启",
                )),
                Err(mut error) => {
                    error.submitted = false;
                    Err(error)
                }
            };
            if let Err(error) = checked {
                self.reservation(reservation("release"))?;
                return Err(error);
            }
        }

        let operation = RadioAction {
            producer_id: producer_id.clone(),
            purpose: Purpose::Reboot,
            token: token.clone(),
            sequence,
            operation_id: format!("rust-reboot-{}-{sequence}-submit", std::process::id()),
            kind: "reboot".into(),
            target: None,
            command: None,
            timeout_ms: None,
            voice_target: None,
            usb_expected: None,
            usb_target: None,
        };
        let action_result = self.action(operation, epoch).await;
        let release_result = self.reservation(reservation("release"));
        match action_result {
            Ok(outcome) => {
                release_result?;
                Ok(json!({
                    "response": outcome.get("response").and_then(Value::as_str).unwrap_or_default()
                }))
            }
            Err(error) => Err(error),
        }
    }
    async fn configure(
        &self,
        input: Configure,
        epoch: u64,
        expected_serial_session: Option<u64>,
    ) -> Result<Value, Failure> {
        if input.config_seq == 0
            || !matches!(input.transport.as_str(), "usb" | "serial" | "none")
            || (input.transport != "usb" && !input.session_id.is_empty())
        {
            return Err(Failure::invalid());
        }
        if let Some(expected) = expected_serial_session
            && (input.transport != "serial" || !self.0.modem.call_binding_current(expected))
        {
            return Err(Failure::stale());
        }
        {
            let state = self.0.state.lock().unwrap();
            if state.active_shared_voice_pins != 0 {
                return Err(Failure::busy());
            }
            if input.config_seq < state.config_seq {
                return Err(Failure::stale());
            }
            if input.config_seq == state.config_seq {
                return if state.config.as_ref() == Some(&input)
                    && expected_serial_session.is_none_or(|expected| {
                        state.binding
                            == Some(Binding::Serial {
                                serial_session: expected,
                            })
                    }) {
                    Ok(json!({"configured":true,"config_seq":input.config_seq}))
                } else {
                    Err(Failure::stale())
                };
            }
        }
        let binding = match input.transport.as_str() {
            "usb" => {
                valid_token(&input.session_id)?;
                #[cfg(test)]
                let fixture = self.0.physical_fixture.lock().unwrap().is_some();
                #[cfg(not(test))]
                let fixture = false;
                if !fixture {
                    self.0
                        .usb
                        .validate_radio_binding(&input.producer_id, &input.session_id)
                        .map_err(Failure::from)?;
                }
                Some(Binding::Usb {
                    producer_id: input.producer_id.clone(),
                    session_id: input.session_id.clone(),
                })
            }
            "serial" => Some(
                crate::module_radio_transport::capture_serial_binding(&self.0.modem)
                    .await
                    .map_err(Failure::from)?,
            ),
            _ => None,
        };
        if let Some(expected) = expected_serial_session {
            if binding
                != Some(Binding::Serial {
                    serial_session: expected,
                })
                || !self.0.modem.call_binding_current(expected)
            {
                return Err(Failure::stale());
            }
        }
        self.fence(epoch)?;
        let mut state = self.0.state.lock().unwrap();
        if state.active_shared_voice_pins != 0 {
            return Err(Failure::busy());
        }
        if state.binding != binding && (state.reservation.is_some() || state.intent.is_some()) {
            return Err(Failure::busy());
        }
        if state.binding != binding {
            state.observed = Outcome::default();
            state.bootstrap_receipt = None;
        }
        state.binding = binding;
        state.config_seq = input.config_seq;
        state.config = Some(input.clone());
        self.sync_suppression(&state);
        drop(state);
        self.flush_call_capture();
        self.0.wake.notify_one();
        Ok(json!({"configured":true,"config_seq":input.config_seq}))
    }
    fn reservation(&self, input: ReservationRequest) -> Result<Value, Failure> {
        self.reservation_with_restore(input, false)
    }
    fn reservation_with_restore(
        &self,
        input: ReservationRequest,
        restore_required: bool,
    ) -> Result<Value, Failure> {
        let id = identity(&input.token, input.sequence)?;
        let mut state = self.0.state.lock().unwrap();
        match input.action.as_str() {
            "acquire" | "acquire_call_mode_operation" => {
                let operation = input.action == "acquire_call_mode_operation";
                if restore_required && !operation {
                    return Err(Failure::invalid());
                }
                if operation && input.purpose != Purpose::CallMode {
                    return Err(Failure::invalid());
                }
                if input.purpose == Purpose::CallMode && state.call_mode_backup_delete_in_flight {
                    return Err(Failure::busy());
                }
                if state.intent.is_some() || state.binding.is_none() {
                    return Err(if state.intent.is_some() {
                        if input.purpose == Purpose::Diagnostic {
                            Failure::new(
                                StatusCode::CONFLICT,
                                "module_vowifi_owns_sim",
                                "模块 VoWiFi 正在管理射频",
                            )
                        } else {
                            Failure::busy()
                        }
                    } else {
                        Failure::unavailable()
                    });
                }
                if matches!(input.purpose, Purpose::Call | Purpose::Reboot) && suppressed(&state) {
                    return Err(blocked());
                }
                if let Some(current) = &state.reservation {
                    return if current.purpose == input.purpose
                        && current.identity == id
                        && current.operation_lock.is_some() == operation
                        && current.restore_journal_required == restore_required
                    {
                        Ok(json!({"token":id.token,"sequence":id.sequence,"released":false}))
                    } else {
                        Err(Failure::busy())
                    };
                }
                if state
                    .waters
                    .get(&input.purpose)
                    .is_some_and(|previous| id.sequence <= previous.sequence)
                {
                    return Err(Failure::stale());
                }
                let operation_lock = if operation {
                    Some(Arc::new(
                        crate::call_mode_backups::try_call_mode_operation_file_lock(
                            &self.0.data_root,
                        )
                        .map_err(|error| match error {
                            crate::call_mode_backups::OperationFileLockError::Busy => {
                                Failure::busy()
                            }
                            crate::call_mode_backups::OperationFileLockError::Io(_) => {
                                Failure::new(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "call_mode_operation_lock_unavailable",
                                    "无法取得通话模式操作锁",
                                )
                            }
                        })?,
                    ))
                } else {
                    None
                };
                let operation = state
                    .executing_operation
                    .as_ref()
                    .ok_or_else(Self::admission_failure)?
                    .clone();
                let previous = state.waters.entry(input.purpose).or_default();
                *previous = Water {
                    sequence: id.sequence,
                    token: id.token.clone(),
                    released: false,
                };
                state.reservation = Some(Reservation {
                    operation,
                    purpose: input.purpose,
                    identity: id.clone(),
                    attempts: BTreeMap::new(),
                    unknown: false,
                    operation_lock,
                    active_voice_pins: 0,
                    active_reconnect_pins: 0,
                    reconnect: None,
                    reconnect_reboot: None,
                    restore_journal_required: restore_required,
                    restore_journal: None,
                    restore_reboot: None,
                    restore_reboot_verified: false,
                });
                Ok(json!({"token":id.token,"sequence":id.sequence,"released":false}))
            }
            "release" => {
                if let Some(current) = &state.reservation {
                    if current.purpose == input.purpose && current.identity == id {
                        if current.active_voice_pins != 0 || current.active_reconnect_pins != 0 {
                            return Err(Failure::busy());
                        }
                        state.reservation = None;
                    } else if current.purpose == input.purpose {
                        return Err(Failure::stale());
                    }
                }
                let water = state.waters.entry(input.purpose).or_default();
                if id.sequence < water.sequence
                    || (id.sequence == water.sequence
                        && !water.token.is_empty()
                        && water.token != id.token)
                {
                    return Err(Failure::stale());
                }
                *water = Water {
                    sequence: id.sequence,
                    token: id.token.clone(),
                    released: true,
                };
                self.0.wake.notify_one();
                Ok(json!({"token":id.token,"sequence":id.sequence,"released":true}))
            }
            _ => Err(Failure::invalid()),
        }
    }
    async fn set(&self, input: SetRequest, epoch: u64, public: bool) -> Result<Value, Failure> {
        let previous = self.load_preferences(epoch).await?;
        {
            let state = self.0.state.lock().unwrap();
            if state.binding.is_none() {
                return Err(Failure::unavailable());
            }
            if state.intent.is_some() || state.reservation.is_some() {
                return Err(Failure::busy());
            }
        }
        if !input.user_initiated && !previous.suppressed() {
            return Ok(self.projection());
        }
        let before = self
            .physical(Action::Observe { cellular: false }, epoch)
            .await
            .map_err(|error| at_stage(error, "flight_mode_read_failed"))?;
        let mode = before
            .cfun
            .filter(|_| before.complete && before.verified)
            .ok_or_else(|| outcome_failure(&before, "flight_mode_read_failed"))?;
        if input.enabled {
            let restore = if mode == 4 {
                if previous.enabled {
                    previous.restore_cfun
                } else {
                    1
                }
            } else {
                i32::from(mode)
            };
            let preferences = Preferences {
                enabled: true,
                restore_cfun: restore,
                user_configured: if mode == 4 && previous.enabled {
                    previous.user_configured || input.user_initiated
                } else {
                    input.user_initiated
                },
            };
            if preferences != previous {
                self.save_preferences(preferences, epoch).await?;
            }
            if mode != 4 {
                let applied = match self.physical(Action::SetVerify { target: 4 }, epoch).await {
                    Ok(outcome) => require_mode(&outcome, 4, "flight_mode_enable_failed"),
                    Err(error) => Err(at_stage(error, "flight_mode_enable_failed")),
                };
                if let Err(error) = applied {
                    return Err(self
                        .recover_enable(error, mode, input.user_initiated, epoch)
                        .await);
                }
                // Preserve the old independent status re-read before reporting success.
                let verified = match self
                    .physical(Action::Observe { cellular: false }, epoch)
                    .await
                {
                    Ok(outcome) => require_mode(&outcome, 4, "flight_mode_snapshot_unverified"),
                    Err(error) => Err(at_stage(error, "flight_mode_snapshot_unverified")),
                };
                if let Err(error) = verified {
                    return Err(self
                        .recover_enable(error, mode, input.user_initiated, epoch)
                        .await);
                }
            } else if public {
                let confirmed = self
                    .physical(Action::Observe { cellular: false }, epoch)
                    .await
                    .map_err(|error| at_stage(error, "flight_mode_cfun_unverified"))?;
                require_mode(&confirmed, 4, "flight_mode_cfun_unverified")?;
                let snapshot = self
                    .physical(Action::Observe { cellular: false }, epoch)
                    .await
                    .map_err(|error| at_stage(error, "flight_mode_snapshot_unverified"))?;
                require_mode(&snapshot, 4, "flight_mode_snapshot_unverified")?;
            }
        } else {
            let target = if mode == 4 && previous.enabled {
                u8::try_from(previous.restore_cfun).map_err(|_| Failure::policy())?
            } else if mode == 4 {
                1
            } else {
                mode
            };
            if mode == 4 {
                let outcome = self
                    .physical(Action::SetVerify { target }, epoch)
                    .await
                    .map_err(|error| at_stage(error, "flight_mode_restore_failed"))?;
                require_mode(&outcome, target, "flight_mode_restore_failed")?;
            }
            let preferences = Preferences {
                enabled: false,
                restore_cfun: 1,
                user_configured: input.user_initiated,
            };
            if preferences != previous {
                self.save_preferences(preferences, epoch).await?;
            }
        }
        Ok(self.projection())
    }
    async fn recover_enable(
        &self,
        mut error: Failure,
        mode: u8,
        user_configured: bool,
        epoch: u64,
    ) -> Failure {
        // A partial/transport failure retires its physical stream. Never reopen
        // or issue a compensating write into an unconfirmed transaction.
        if error.uncertain {
            return error;
        }
        match self
            .physical(Action::SetVerify { target: mode }, epoch)
            .await
        {
            Ok(outcome) if require_mode(&outcome, mode, "flight_mode_restore_failed").is_ok() => {
                if self
                    .save_preferences(
                        Preferences {
                            enabled: false,
                            restore_cfun: i32::from(mode),
                            user_configured,
                        },
                        epoch,
                    )
                    .await
                    .is_err()
                {
                    error.code = "flight_mode_state_write_failed";
                }
            }
            Ok(outcome) => {
                error.uncertain |= outcome.uncertain;
                error.submitted |= outcome.submitted;
            }
            Err(restore) => {
                error.uncertain |= restore.uncertain;
                error.submitted |= restore.submitted;
            }
        }
        error
    }
    fn set_intent_inhibit(&self, state: &State, token: &str, enabled: bool) -> Result<(), Failure> {
        match state.binding.as_ref() {
            Some(Binding::Serial { serial_session }) if self.0.modem.native_serial_managed() => {
                // The actor's retained capability already excludes foreign RF,
                // card and lifecycle work. This is a nonblocking fact check;
                // never wait on the actor while holding the radio state mutex.
                if self
                    .0
                    .modem
                    .serial_module_hold_matches(*serial_session, token)
                {
                    Ok(())
                } else {
                    Err(Failure::stale())
                }
            }
            _ => self
                .0
                .usb
                .set_radio_inhibit(&state.producer, token, enabled)
                .map_err(Failure::from),
        }
    }
    async fn intent(&self, input: IntentRequest, epoch: u64) -> Result<Value, Failure> {
        let id = identity(&input.token, input.sequence)?;
        if input.action == "prepare" {
            self.load_preferences(epoch).await?;
            let mut state = self.0.state.lock().unwrap();
            if state.reservation.is_some() || state.binding.is_none() {
                return Err(Failure::busy());
            }
            if let Some(current) = &state.intent {
                return if current.identity == id {
                    Ok(intent_response(&state, &id, false))
                } else {
                    Err(Failure::busy())
                };
            }
            if id.sequence <= state.intent_water.sequence {
                return Err(Failure::stale());
            }
            let operation = state
                .executing_operation
                .as_ref()
                .ok_or_else(Self::admission_failure)?
                .clone();
            self.set_intent_inhibit(&state, &id.token, true)?;
            state.intent_water = Water {
                sequence: id.sequence,
                token: id.token.clone(),
                released: false,
            };
            state.intent = Some(Intent {
                operation,
                identity: id.clone(),
                active: false,
                restore_mode: None,
            });
            self.sync_suppression(&state);
            return Ok(intent_response(&state, &id, false));
        }
        {
            let mut state = self.0.state.lock().unwrap();
            if input.action == "restore" && state.intent.is_none() {
                if id.sequence < state.intent_water.sequence
                    || (id.sequence == state.intent_water.sequence
                        && state.intent_water.token != id.token)
                {
                    return Err(Failure::stale());
                }
                state.intent_water = Water {
                    sequence: id.sequence,
                    token: id.token.clone(),
                    released: true,
                };
                return Ok(intent_response(&state, &id, true));
            }
            if state
                .intent
                .as_ref()
                .is_none_or(|intent| intent.identity != id)
            {
                return Err(Failure::stale());
            }
        }
        match input.action.as_str() {
            "enter" => {
                let active = self.0.state.lock().unwrap().intent.as_ref().unwrap().active;
                if !active {
                    let initial = self
                        .physical(Action::Observe { cellular: false }, epoch)
                        .await?;
                    let mode = initial
                        .cfun
                        .filter(|_| initial.complete && initial.verified)
                        .ok_or_else(|| outcome_failure(&initial, "flight_mode_read_failed"))?;
                    let mut state = self.0.state.lock().unwrap();
                    let intent = state.intent.as_mut().ok_or_else(Failure::stale)?;
                    intent.restore_mode = Some(mode);
                    intent.active = true;
                } else {
                    let observed = self
                        .physical(Action::Observe { cellular: false }, epoch)
                        .await?;
                    require_mode(&observed, 4, "flight_mode_cfun_unverified")?;
                    return Ok(intent_response(&self.0.state.lock().unwrap(), &id, false));
                }
                let outcome = self
                    .physical(Action::SetVerify { target: 4 }, epoch)
                    .await?;
                require_mode(&outcome, 4, "flight_mode_cfun_unverified")?;
                Ok(intent_response(&self.0.state.lock().unwrap(), &id, false))
            }
            "observe" => {
                self.physical(Action::Observe { cellular: false }, epoch)
                    .await?;
                Ok(intent_response(&self.0.state.lock().unwrap(), &id, false))
            }
            "restore" => {
                let preferences = self.load_preferences(epoch).await;
                let intent = self
                    .0
                    .state
                    .lock()
                    .unwrap()
                    .intent
                    .clone()
                    .ok_or_else(Failure::stale)?;
                if intent.active {
                    let target = match preferences {
                        Ok(prefs) if !prefs.suppressed() => intent.restore_mode.unwrap_or(4),
                        _ => 4,
                    };
                    let observed = self
                        .physical(Action::Observe { cellular: false }, epoch)
                        .await?;
                    if !(observed.complete && observed.verified) {
                        return Err(outcome_failure(&observed, "flight_mode_restore_failed"));
                    }
                    if observed.cfun != Some(target) {
                        let outcome = self.physical(Action::SetVerify { target }, epoch).await?;
                        require_mode(&outcome, target, "flight_mode_restore_failed")?;
                    }
                }
                let mut state = self.0.state.lock().unwrap();
                self.set_intent_inhibit(&state, &id.token, false)?;
                state.intent = None;
                state.intent_water.released = true;
                self.sync_suppression(&state);
                self.0.wake.notify_one();
                Ok(intent_response(&state, &id, true))
            }
            _ => Err(Failure::invalid()),
        }
    }
    async fn action(&self, input: RadioAction, epoch: u64) -> Result<Value, Failure> {
        valid_token(&input.operation_id)?;
        let id = identity(&input.token, input.sequence)?;
        let usb_write = input.kind == "call_mode_usb_composition_write";
        if usb_write != (input.usb_expected.is_some() && input.usb_target.is_some())
            || !usb_write && (input.usb_expected.is_some() || input.usb_target.is_some())
        {
            return Err(Failure::invalid());
        }
        {
            let state = self.0.state.lock().unwrap();
            let reservation = state.reservation.as_ref().ok_or_else(Failure::stale)?;
            if reservation.purpose != input.purpose || reservation.identity != id {
                return Err(Failure::stale());
            }
            if reservation.active_voice_pins != 0 || reservation.active_reconnect_pins != 0 {
                return Err(Failure::busy());
            }
            if let Some(previous) = reservation.attempts.get(&input.operation_id) {
                if previous.input != input {
                    return Err(Failure::stale());
                }
                return previous
                    .result
                    .clone()
                    .ok_or_else(Failure::busy)?
                    .map(|outcome| json!(outcome));
            }
        }
        {
            let state = self.0.state.lock().unwrap();
            let reservation = state.reservation.as_ref().ok_or_else(Failure::stale)?;
            if reservation.unknown || state.observed.uncertain {
                return Err(Failure::new(
                    StatusCode::CONFLICT,
                    "radio_outcome_unknown",
                    "上次射频操作结果未确认，请先刷新核对",
                ));
            }
            if reservation.attempts.len()
                >= if reservation.operation_lock.is_some() {
                    512
                } else {
                    64
                }
            {
                return Err(Failure::busy());
            }
        }
        let preferences = if matches!(
            input.kind.as_str(),
            "call_mode_voice"
                | "call_mode_identity_read"
                | "call_mode_idle_read"
                | "call_mode_voice_read"
                | "call_mode_usb_composition_read"
                | "call_mode_usb_composition_write"
                | "call_mode_adb_probe"
                | "call_mode_adb_authorize"
        ) {
            None
        } else {
            Some(self.load_preferences(epoch).await?)
        };
        let mut cellular_check = false;
        let action = match input.kind.as_str() {
            "set_mode"
                if input.purpose == Purpose::Diagnostic
                    && input.command.is_none()
                    && input.timeout_ms.is_none()
                    && input.voice_target.is_none() =>
            {
                let target = input
                    .target
                    .filter(|mode| *mode <= 4)
                    .ok_or_else(Failure::invalid)?;
                if preferences.is_some_and(Preferences::suppressed) && target != 4 {
                    return Err(blocked());
                }
                cellular_check = target != 4;
                Action::SetVerify { target }
            }
            "reboot"
                if matches!(
                    input.purpose,
                    Purpose::Reboot | Purpose::CallMode | Purpose::Diagnostic
                ) && input.target.is_none()
                    && input.command.is_none()
                    && input.timeout_ms.is_none()
                    && input.voice_target.is_none() =>
            {
                if input.purpose == Purpose::Reboot
                    && preferences.is_some_and(|preferences| preferences.enabled)
                {
                    return Err(Failure::new(
                        StatusCode::CONFLICT,
                        "module_flight_mode_enabled",
                        "模块飞行模式已开启；请先关闭飞行模式后再重启模块",
                    ));
                }
                if preferences.is_some_and(Preferences::suppressed) {
                    return Err(blocked());
                }
                cellular_check = true;
                Action::Reboot
            }
            "refresh_profile"
                if input.purpose == Purpose::Esim
                    && input.target.is_none()
                    && input.command.is_none()
                    && input.timeout_ms.is_none()
                    && input.voice_target.is_none() =>
            {
                Action::RefreshAfterProfileSwitch {
                    target: if preferences.is_some_and(Preferences::suppressed) {
                        4
                    } else {
                        1
                    },
                }
            }
            "diagnostic"
                if input.purpose == Purpose::Diagnostic
                    && input.target.is_none()
                    && input.voice_target.is_none() =>
            {
                let command = input.command.clone().ok_or_else(Failure::invalid)?;
                let timeout_ms = input
                    .timeout_ms
                    .filter(|value| (1..=20_000).contains(value))
                    .ok_or_else(Failure::invalid)?;
                let is_blocked = diagnostic::blocked(&command)?;
                if preferences.is_some_and(Preferences::suppressed) && is_blocked {
                    return Err(blocked());
                }
                cellular_check = is_blocked;
                Action::Diagnostic {
                    command,
                    timeout_ms,
                }
            }
            "call_mode_voice"
                if input.purpose == Purpose::CallMode
                    && input.target.is_none()
                    && input.command.is_none()
                    && input.timeout_ms.is_none() =>
            {
                let target = input.voice_target.ok_or_else(Failure::invalid)?;
                if !target.valid() {
                    return Err(Failure::invalid());
                }
                Action::CallModeVoice { target }
            }
            "call_mode_idle_read"
                if matches!(input.purpose, Purpose::CallMode | Purpose::Call)
                    && input.target.is_none()
                    && input.command.is_none()
                    && input.timeout_ms.is_none()
                    && input.voice_target.is_none() =>
            {
                Action::CallModeIdleRead
            }
            "call_mode_identity_read"
                if matches!(input.purpose, Purpose::CallMode | Purpose::Call)
                    && input.target.is_none()
                    && input.command.is_none()
                    && input.timeout_ms.is_none()
                    && input.voice_target.is_none() =>
            {
                Action::CallModeIdentityRead
            }
            "call_mode_voice_read"
                if matches!(input.purpose, Purpose::CallMode | Purpose::Call)
                    && input.target.is_none()
                    && input.command.is_none()
                    && input.timeout_ms.is_none()
                    && input.voice_target.is_none() =>
            {
                Action::CallModeVoiceRead
            }
            "call_mode_usb_composition_read"
                if matches!(input.purpose, Purpose::CallMode | Purpose::Call)
                    && input.target.is_none()
                    && input.command.is_none()
                    && input.timeout_ms.is_none()
                    && input.voice_target.is_none() =>
            {
                Action::CallModeUsbCompositionRead
            }
            "call_mode_usb_composition_write"
                if input.purpose == Purpose::CallMode
                    && input.target.is_none()
                    && input.command.is_none()
                    && input.timeout_ms.is_none()
                    && input.voice_target.is_none() =>
            {
                let expected = input.usb_expected.ok_or_else(Failure::invalid)?;
                let target = input.usb_target.ok_or_else(Failure::invalid)?;
                let action = Action::CallModeUsbCompositionWrite { expected, target };
                action.validate()?;
                action
            }
            "call_mode_adb_probe"
                if matches!(input.purpose, Purpose::CallMode | Purpose::Call)
                    && input.target.is_none()
                    && input.command.is_none()
                    && input.timeout_ms.is_none()
                    && input.voice_target.is_none() =>
            {
                Action::CallModeAdbProbe
            }
            "call_mode_adb_authorize"
                if input.purpose == Purpose::CallMode
                    && input.target.is_none()
                    && input.command.is_none()
                    && input.timeout_ms.is_none()
                    && input.voice_target.is_none() =>
            {
                Action::CallModeAdbAuthorize
            }
            _ => return Err(Failure::invalid()),
        };
        if cellular_check {
            let observed = self
                .physical(Action::Observe { cellular: false }, epoch)
                .await?;
            if !(observed.complete && observed.verified) {
                return Err(outcome_failure(&observed, "flight_mode_read_failed"));
            }
            if observed.cfun == Some(4) {
                return Err(blocked());
            }
        }
        {
            let mut state = self.0.state.lock().unwrap();
            let reservation = state.reservation.as_mut().ok_or_else(Failure::stale)?;
            if reservation.purpose != input.purpose || reservation.identity != id {
                return Err(Failure::stale());
            }
            if let Some(previous) = reservation.attempts.get(&input.operation_id) {
                if previous.input != input {
                    return Err(Failure::stale());
                }
                return previous
                    .result
                    .clone()
                    .ok_or_else(Failure::busy)?
                    .map(|outcome| json!(outcome));
            }
            if reservation.unknown {
                return Err(Failure::new(
                    StatusCode::CONFLICT,
                    "radio_outcome_unknown",
                    "上次射频操作结果未确认，请先刷新核对",
                ));
            }
            if reservation.attempts.len()
                >= if reservation.operation_lock.is_some() {
                    512
                } else {
                    64
                }
            {
                return Err(Failure::busy());
            }
            reservation.attempts.insert(
                input.operation_id.clone(),
                Attempt {
                    input: input.clone(),
                    result: None,
                },
            );
        }
        let commit = if input.purpose == Purpose::CallMode && action.mutating() {
            let prepared = match self.validate_restore_attachment() {
                Ok(()) => {
                    self.prepare_call_mode_reconnect(&input, &action, epoch)
                        .await
                }
                Err(error) => Err(error),
            };
            match prepared {
                Ok(()) => Some(self.mint_call_mode_commit(&input, &action, epoch)),
                Err(error) => Some(Err(error)),
            }
        } else {
            None
        };
        // A failed mint is still this registered attempt's final result. Never
        // strand an in-flight attempt or clear previous unknown ownership.
        let result = match commit {
            Some(Ok(proof)) => self.physical_with_commit(action, epoch, Some(proof)).await,
            Some(Err(error)) => Err(error),
            None => self.physical(action, epoch).await,
        };
        if let Err(mut stale) = self.fence(epoch) {
            // A producer may retire while the physical command is awaiting its
            // reply. Keep the submission fact across this second actor fence.
            match &result {
                Ok(outcome) => {
                    stale.submitted = outcome.submitted;
                    stale.uncertain = outcome.uncertain || outcome.submitted;
                }
                Err(physical) => {
                    stale.submitted = physical.submitted;
                    stale.uncertain = physical.uncertain || physical.submitted;
                }
            }
            return Err(stale);
        }
        {
            let mut state = self.0.state.lock().unwrap();
            if input.kind == "diagnostic" {
                state.observed.cfun = None;
                state.observed.verified = false;
                state.observed.cereg = None;
                state.observed.creg = None;
                state.observed.signal = None;
                state.observed.response.clear();
                self.sync_suppression(&state);
            }

            let reservation = state.reservation.as_mut().ok_or_else(Failure::stale)?;
            if input.purpose == Purpose::CallMode
                && input.kind == "reboot"
                && match &result {
                    Ok(outcome) => {
                        outcome.submitted && outcome.error_code != "radio_reboot_rejected"
                    }
                    Err(error) => error.submitted && error.code != "radio_reboot_rejected",
                }
            {
                reservation.reconnect_reboot = Some(input.operation_id.clone());
            }
            reservation.unknown = match &result {
                Ok(outcome) => outcome.uncertain,
                Err(error) => error.uncertain,
            };
            reservation
                .attempts
                .get_mut(&input.operation_id)
                .ok_or_else(Failure::stale)?
                .result = Some(result.clone());
        }
        result.map(|outcome| json!(outcome))
    }
}
#[cfg(test)]
struct UsbnetWaiter(Arc<Inner>);
#[cfg(test)]
impl UsbnetWaiter {
    fn new(inner: Arc<Inner>) -> Self {
        inner.usbnet_waiters.fetch_add(1, Ordering::SeqCst);
        Self(inner)
    }
}
#[cfg(test)]
impl Drop for UsbnetWaiter {
    fn drop(&mut self) {
        self.0.usbnet_waiters.fetch_sub(1, Ordering::SeqCst);
    }
}

fn usbnet_temporary_read_owner(state: &State) -> bool {
    state.intent.is_none()
        && !state.observed.uncertain
        && !state.mutating_in_flight
        && state.active_shared_voice_pins == 0
        && state.reservation.as_ref().is_some_and(|owner| {
            owner.purpose == Purpose::CallMode
                && owner.operation_lock.is_none()
                && !owner.restore_journal_required
                && owner.restore_journal.is_none()
                && owner.restore_reboot.is_none()
                && !owner.restore_reboot_verified
                && !owner.unknown
                && owner.active_voice_pins == 0
                && owner.active_reconnect_pins == 0
                && owner.reconnect.is_none()
                && owner.reconnect_reboot.is_none()
                && owner.attempts.values().all(|attempt| {
                    matches!(
                        attempt.input.kind.as_str(),
                        "call_mode_idle_read"
                            | "call_mode_identity_read"
                            | "call_mode_voice_read"
                            | "call_mode_usb_composition_read"
                            | "call_mode_adb_probe"
                    ) && attempt.result.as_ref().is_some_and(|result| match result {
                        Ok(outcome) => !outcome.uncertain && !outcome.submitted,
                        Err(error) => !error.uncertain && !error.submitted,
                    })
                })
        })
}

fn usbnet_wait_deadline() -> Failure {
    Failure::new(
        StatusCode::CONFLICT,
        "module_radio_busy",
        "模块只读状态检查尚未结束，未提交网卡配置，请稍后重试",
    )
}

struct ExecutingOperation(Arc<Inner>);
impl Drop for ExecutingOperation {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .executing_operation = None;
    }
}
struct JobFinished(Arc<Inner>);
impl Drop for JobFinished {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().jobs -= 1;
        self.0.finished.notify_waiters();
    }
}
async fn worker(weak: Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let wake = inner.wake.clone();
        let stopping = inner.state.lock().unwrap().stopping;
        if stopping {
            inner.state.lock().unwrap().running = false;
            inner.finished.notify_waiters();
            return;
        }
        let runnable = {
            let state = inner.state.lock().unwrap();
            !state.retiring
                && state.binding.is_some()
                && state.intent.is_none()
                && state.reservation.is_none()
                && !state.observed.uncertain
        };
        if runnable {
            let _ = Service(inner.clone()).submit(Job::Tick).await;
        }
        if inner.state.lock().unwrap().stopping {
            inner.state.lock().unwrap().running = false;
            inner.finished.notify_waiters();
            return;
        }
        #[cfg(test)]
        let timer = inner.timer_fixture.lock().unwrap().clone();
        drop(inner);
        #[cfg(test)]
        if let Some(timer) = timer {
            let (send, receive) = oneshot::channel();
            if timer.send((INTERVAL, send)).is_ok() {
                tokio::select! {_=receive=>{},_=wake.notified()=>{}}
                continue;
            }
        }
        tokio::select! {_=tokio::time::sleep(INTERVAL)=>{},_=wake.notified()=>{}}
    }
}
fn identity(token: &str, sequence: u64) -> Result<Identity, Failure> {
    valid_token(token)?;
    if sequence == 0 {
        return Err(Failure::invalid());
    }
    Ok(Identity {
        token: token.into(),
        sequence,
    })
}
fn valid_token(value: &str) -> Result<(), Failure> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    {
        Err(Failure::invalid())
    } else {
        Ok(())
    }
}

fn parse_call_mode_read_fact<T: serde::de::DeserializeOwned>(
    value: &Value,
    field: &str,
    kind: &str,
) -> Result<T, Failure> {
    let complete = value.get("complete").and_then(Value::as_bool) == Some(true);
    let verified = value.get("verified").and_then(Value::as_bool) == Some(true);
    let uncertain = value.get("uncertain").and_then(Value::as_bool) == Some(true);
    let error_code = value
        .get("error_code")
        .and_then(Value::as_str)
        .unwrap_or("");
    let response = value
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !complete || !verified || uncertain || !error_code.is_empty() {
        let cause = if error_code.is_empty() {
            "模块射频操作未确认 (module_radio_outcome_unconfirmed)".to_owned()
        } else {
            format!("模块射频操作未确认 ({error_code})")
        };
        let detail = if response.is_empty() {
            &cause
        } else {
            response
        };
        let message = match (kind, error_code) {
            ("call_mode_usb_composition_read", "call_mode_usb_parse_failed") => {
                format!("无法识别模块 USB 配置：{detail}: {cause}")
            }
            ("call_mode_usb_composition_read", "call_mode_usb_query_rejected") => {
                format!("模块拒绝读取 USB 配置：{detail}: {cause}")
            }
            ("call_mode_usb_composition_read", _) => {
                format!("模块 USB 配置事务未确认：{detail}")
            }
            ("call_mode_voice_read", "call_mode_ims_read_failed") => {
                format!("读取 IMS 配置：{detail}")
            }
            ("call_mode_voice_read", "call_mode_volte_read_failed") => {
                format!("读取 VoLTE 开关：{detail}")
            }
            ("call_mode_voice_read", _) => {
                format!("Rust IMS/VoLTE 配置事务未确认：{detail}")
            }
            _ => "模块通话模式读取操作无效".into(),
        };
        return Err(Failure::new(
            StatusCode::BAD_GATEWAY,
            "module_radio_outcome_unconfirmed",
            &message,
        ));
    }
    let Some(fact) = value.get(field).cloned() else {
        let message = if field == "voice" {
            "Rust IMS/VoLTE 查询未返回已验证状态"
        } else {
            "Rust USBCFG 查询未返回已验证配置及通话目标"
        };
        return Err(Failure::new(
            StatusCode::BAD_GATEWAY,
            "module_radio_outcome_unconfirmed",
            message,
        ));
    };
    serde_json::from_value(fact).map_err(|_| {
        Failure::new(
            StatusCode::BAD_GATEWAY,
            "module_radio_outcome_unconfirmed",
            if field == "voice" {
                "Rust IMS/VoLTE 查询未返回已验证状态"
            } else {
                "Rust USBCFG 查询未返回已验证配置及通话目标"
            },
        )
    })
}
fn suppressed(state: &State) -> bool {
    state.stopping
        || state.retiring
        || state.mutating_in_flight
        || state.preference_error
        || state.preferences.is_none_or(Preferences::suppressed)
        || state.intent.is_some()
        || !(state.observed.complete
            && state.observed.verified
            && !state.observed.uncertain
            && state.observed.cfun.is_some_and(|mode| mode != 4))
}

fn restore_journal_failure() -> Failure {
    Failure::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "call_mode_restore_journal_unavailable",
        "还原写入日志尚未耐久确认，未提交后续配置",
    )
}
fn blocked() -> Failure {
    Failure::new(
        StatusCode::CONFLICT,
        "module_flight_mode_blocks_cellular_probe",
        "模块飞行模式已开启，已阻止蜂窝射频操作",
    )
}
fn at_stage(mut error: Failure, code: &'static str) -> Failure {
    error.status = StatusCode::BAD_GATEWAY;
    error.code = code;
    error.message = "模块射频状态未通过核验".into();
    error
}
fn outcome_failure(outcome: &Outcome, code: &'static str) -> Failure {
    let mut error = Failure::new(StatusCode::BAD_GATEWAY, code, "模块射频状态未通过核验");
    error.uncertain = outcome.uncertain;
    error.submitted = outcome.submitted;
    error
}
fn require_mode(outcome: &Outcome, target: u8, code: &'static str) -> Result<(), Failure> {
    if outcome.complete && outcome.verified && outcome.cfun == Some(target) {
        Ok(())
    } else {
        Err(outcome_failure(outcome, code))
    }
}
fn intent_response(state: &State, id: &Identity, restored: bool) -> Value {
    json!({"token":id.token,"sequence":id.sequence,"active":state.intent.as_ref().is_some_and(|intent|intent.active),"restored":restored,"cfun_mode":state.observed.cfun.map(i32::from).unwrap_or(-1),"verified":state.observed.verified})
}
fn status(state: &State) -> Value {
    let reason = state
        .reservation
        .as_ref()
        .map(|reservation| match reservation.purpose {
            Purpose::Esim => "eSIM Profile 操作进行中",
            Purpose::ModuleProfileNotes => "模块 Profile 资料正在操作",
            Purpose::DeviceSms => "模块短信正在发送",
            Purpose::Call => "通话进行中",
            Purpose::CallMode => "模块通话模式正在变更",
            Purpose::Reboot => "模块正在重启",
            Purpose::Diagnostic => "模块诊断进行中",
        })
        .unwrap_or("");
    status_with_block_reason(state, reason)
}
fn status_with_block_reason(state: &State, block_reason: &str) -> Value {
    let mut value = json!({"available":false,"enabled":false,"verified":false,"user_configured":false,"cfun_mode":-1,"owner":"none","can_toggle":false,"registration_known":false,"signal_known":false,"summary":"模块未连接"});
    if state.binding.is_none() {
        return value;
    }
    value["available"] = json!(true);
    if state.preference_error {
        value["summary"] = json!("无法读取模块飞行模式偏好");
        value["detail"] = json!("读取模块飞行模式偏好失败");
        return value;
    }
    let prefs = state.preferences.unwrap_or_default();
    value["user_configured"] = json!(prefs.user_configured);
    let Some(mode) = state
        .observed
        .cfun
        .filter(|_| state.observed.verified && !state.observed.uncertain)
    else {
        value["summary"] = json!("无法读取模块射频状态");
        value["detail"] = json!("模块射频状态未通过核验");
        return value;
    };
    let enabled = mode == 4;
    value["cfun_mode"] = json!(mode);
    value["enabled"] = json!(enabled);
    value["summary"] = json!(if enabled {
        "蜂窝射频已关闭，正在核验"
    } else {
        "蜂窝射频已开启"
    });
    if state.intent.is_some() {
        value["owner"] = json!("vowifi");
        value["summary"] = json!("蜂窝射频由 Wi-Fi Calling 管理");
        value["detail"] = json!("当前由模块 VoWiFi 管理蜂窝射频；关闭 VoWiFi 后可独立切换。");
        value["verified"] = json!(enabled);
        return value;
    }
    value["owner"] = json!(if enabled && prefs.enabled {
        if prefs.user_configured {
            "manual"
        } else {
            "default"
        }
    } else if enabled {
        "external"
    } else {
        "none"
    });
    if enabled {
        value["verified"] = json!(true);
        value["summary"] = json!("飞行模式已开启 · 蜂窝射频关闭");
        value["detail"] =
            json!("模块已确认 CFUN=4，蜂窝射频收发已关闭；飞行模式期间不会扫描或注册蜂窝基站。");
    } else if !prefs.suppressed() {
        if let (Some(cereg), Some(creg)) = (state.observed.cereg, state.observed.creg) {
            let registered = matches!(cereg, 1 | 5) || matches!(creg, 1 | 5);
            value["registration_known"] = json!(true);
            value["cereg_state"] = json!(cereg);
            value["creg_state"] = json!(creg);
            value["cellular_registered"] = json!(registered);
            value["detail"] = json!(if registered {
                "蜂窝网络已注册。"
            } else {
                "当前未注册蜂窝网络。"
            });
        }
        if let Some(signal) = state.observed.signal {
            value["signal_known"] = json!(true);
            value["signal_level"] = json!(signal);
            value["signal_detected"] = json!(signal != 99);
        }
    } else {
        value["summary"] = json!("正在关闭蜂窝射频");
        value["detail"] =
            json!("飞行模式已启用；射频关闭确认前也不会查询信号、注册状态或扫描基站。");
    }
    value["can_toggle"] = json!(!state.stopping && !state.retiring && block_reason.is_empty());
    if value.get("detail").is_none() && !block_reason.is_empty() {
        value["detail"] = json!(block_reason);
    }
    value
}
async fn body(request: Request<Incoming>, limit: usize) -> Result<Vec<u8>, Failure> {
    crate::http_server::collect_body_with_timeout(
        request.into_body(),
        limit,
        Duration::from_secs(5),
    )
    .await
    .map(|bytes| bytes.to_vec())
    .map_err(|error| match error {
        crate::http_server::BodyReadError::TooLarge => Failure::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            "请求过大",
        ),
        crate::http_server::BodyReadError::Invalid => Failure::invalid(),
        crate::http_server::BodyReadError::Timeout => Failure::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_timeout",
            "读取请求超时",
        ),
    })
}

fn parse_public_at_command(data: &[u8]) -> Result<String, String> {
    let mut decoder = serde_json::Deserializer::from_slice(data);
    let value = Value::deserialize(&mut decoder).map_err(|error| {
        if error.is_eof() {
            "EOF".to_owned()
        } else {
            error.to_string()
        }
    })?;
    if decoder.end().is_err() {
        return Err("请求只能包含一个 JSON 对象".into());
    }
    let Some(fields) = value.as_object() else {
        return Err("请求必须是 JSON 对象".into());
    };
    for key in fields.keys() {
        if key != "command" {
            return Err(format!("json: unknown field \"{key}\""));
        }
    }
    match fields.get("command") {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(command)) => Ok(command.clone()),
        Some(value) => Err(format!(
            "json: cannot unmarshal {} into Go struct field .command of type string",
            match value {
                Value::Bool(_) => "bool",
                Value::Number(_) => "number",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
                Value::Null | Value::String(_) => unreachable!(),
            }
        )),
    }
}
fn at_console_coded_error(
    status: StatusCode,
    code: &'static str,
    message: &str,
    recoverable: bool,
) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":message.trim(),"code":code,"recoverable":recoverable}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AtConsoleExecutionResult {
    response: String,
    #[serde(default)]
    error: Option<String>,
}

fn at_console_adapter_result(result: AtConsoleExecutionResult) -> Response<Full<Bytes>> {
    if result.response.len() > 64 * 1024
        || result
            .error
            .as_ref()
            .is_some_and(|error| error.len() > 8 * 1024)
    {
        return at_console_coded_error(
            StatusCode::BAD_GATEWAY,
            "at_transport_failed",
            "AT 通道返回了无法识别的结果。请先核对设备状态，再决定是否重新执行。",
            false,
        );
    }
    if let Some(error) = result.error.filter(|error| !error.is_empty()) {
        return match error.as_str() {
            "module flight mode blocks AT command" => at_console_coded_error(
                StatusCode::CONFLICT,
                "module_flight_mode_blocks_cellular_probe",
                "模块飞行模式已开启，已阻止基站扫描、蜂窝信号查询及蜂窝注册/附着命令",
                true,
            ),
            "module VoWiFi owns AT channel" => at_console_coded_error(
                StatusCode::CONFLICT,
                "module_vowifi_owns_sim",
                "模块 VoWiFi 正在占用 AT/SIM 安全通道，暂不接受手动 AT 命令",
                true,
            ),
            _ => at_console_coded_error(
                StatusCode::BAD_GATEWAY,
                "at_transport_failed",
                &format!(
                    "AT 通道未能确认执行结果：{error}。请先核对设备状态，再决定是否重新执行。"
                ),
                false,
            ),
        };
    }
    crate::json_response(StatusCode::OK, &diagnostic::result(&result.response))
}

fn proven_read_only_at(command: &str) -> bool {
    matches!(
        command.trim().to_ascii_uppercase().as_str(),
        "AT" | "ATI"
            | "AT+CGMI"
            | "AT+CGMM"
            | "AT+CGMR"
            | "AT+CGSN"
            | "AT+CSQ"
            | "AT+CESQ"
            | "AT+COPS?"
            | "AT+CPIN?"
            | "AT+CNUM"
            | "AT+CFUN?"
            | "AT+CREG?"
            | "AT+CGREG?"
            | "AT+CEREG?"
            | "AT+QCFG=\"USBNET\""
            | "AT+QCFG=\"USBCFG\""
    )
}
pub(crate) async fn handle_at_console_public(
    request: Request<Incoming>,
    service: Service,
    stopping: bool,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST || request.uri().path() != "/api/at" {
        return crate::json_response(StatusCode::NOT_FOUND, &json!({"error":"not found"}));
    }
    if stopping {
        return Failure::stopped().response();
    }
    let data = match body(request, 1 << 20).await {
        Ok(data) => data,
        Err(error) => {
            return crate::json_response(error.status, &json!({"error":error.message}));
        }
    };
    let (request_id, stripped) = match crate::public_mutation_receipt::split(&data, false) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let command = match parse_public_at_command(&stripped) {
        Ok(command) => command,
        Err(message) => {
            return crate::json_response(StatusCode::BAD_REQUEST, &json!({"error":message}));
        }
    };
    let command = match diagnostic::parse_console_command(&command) {
        Ok(command) => command,
        Err(message) => {
            return at_console_coded_error(
                StatusCode::BAD_REQUEST,
                "at_command_invalid",
                message,
                false,
            );
        }
    };
    let binding = service.public_mutation_binding();
    if proven_read_only_at(&command) {
        return execute_public_at(&service, command, &binding).await;
    }
    let Some(id) = request_id else {
        return crate::public_mutation_receipt::error(
            StatusCode::BAD_REQUEST,
            "client_request_id_required",
        );
    };
    crate::public_mutation_receipt::run(
        &service.0.data_root,
        "/api/at",
        &id,
        &data,
        binding.clone(),
        || execute_public_at(&service, command, &binding),
    )
    .await
}
async fn execute_public_at(
    service: &Service,
    command: String,
    binding: &str,
) -> Response<Full<Bytes>> {
    if service.vowifi_owns_sim_channel() {
        return at_console_coded_error(
            StatusCode::CONFLICT,
            "module_vowifi_owns_sim",
            "模块 VoWiFi 正在占用 AT/SIM 安全通道，暂不接受手动 AT 命令",
            true,
        );
    }
    match service
        .submit_bound(
            Job::PublicAtConsole {
                command,
                deadline: Instant::now() + Duration::from_secs(24),
            },
            binding,
        )
        .await
    {
        Ok(value) => match serde_json::from_value::<AtConsoleExecutionResult>(value) {
            Ok(result) => at_console_adapter_result(result),
            Err(_) => at_console_coded_error(
                StatusCode::BAD_GATEWAY,
                "at_transport_failed",
                "AT 通道返回了无法识别的结果。请先核对设备状态，再决定是否重新执行。",
                false,
            ),
        },
        Err(error) if error.public_code() == "module_flight_mode_blocks_cellular_probe" => {
            at_console_coded_error(
                StatusCode::CONFLICT,
                "module_flight_mode_blocks_cellular_probe",
                "模块飞行模式已开启，已阻止基站扫描、蜂窝信号查询及蜂窝注册/附着命令",
                true,
            )
        }
        Err(error) if error.public_code() == "module_vowifi_owns_sim" => at_console_coded_error(
            StatusCode::CONFLICT,
            "module_vowifi_owns_sim",
            "模块 VoWiFi 正在占用 AT/SIM 安全通道，暂不接受手动 AT 命令",
            true,
        ),
        Err(error) => at_console_coded_error(
            StatusCode::BAD_GATEWAY,
            "at_transport_failed",
            &format!(
                "AT 通道未能确认执行结果：{}。请先核对设备状态，再决定是否重新执行。",
                error
            ),
            false,
        ),
    }
}
fn decode<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, Failure> {
    serde_json::from_slice(data).map_err(|_| Failure::invalid())
}
pub(crate) async fn handle_public(
    request: Request<Incoming>,
    service: Service,
    stopping: bool,
) -> Response<Full<Bytes>> {
    if request.uri().path() == "/api/esim/module-notes" && request.method() == Method::PUT {
        if stopping {
            return Failure::stopped().response();
        }
        let data = match body(request, 64 * 1024).await {
            Ok(v) => v,
            Err(e) => return e.response(),
        };
        let (id, stripped) = match crate::public_mutation_receipt::split(&data, true) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let note: crate::module_profile_notes::Note = match decode(&stripped) {
            Ok(v) => v,
            Err(e) => return e.response(),
        };
        let binding = service.public_mutation_binding();
        return crate::public_mutation_receipt::run(
            &service.0.data_root,
            "/api/esim/module-notes",
            id.as_deref().unwrap(),
            &data,
            binding.clone(),
            || async {
                match service
                    .submit_bound(
                        Job::PublicModuleProfileNotes(ModuleProfileNotesRequest::Save(note)),
                        &binding,
                    )
                    .await
                {
                    Ok(value) => crate::json_response(StatusCode::OK, &value),
                    Err(error) if error.code == "module_radio_busy" => {
                        module_profile_notes_busy_response()
                    }
                    Err(error) => error.response(),
                }
            },
        )
        .await;
    }
    if request.uri().path() == "/api/esim/module-notes" {
        if stopping {
            return crate::json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error":"Switch 正在退出，未提交新操作"}),
            );
        }
        let operation = match (request.method(), request.uri().path()) {
            (&Method::GET, "/api/esim/module-notes") => Ok(ModuleProfileNotesRequest::List),
            _ => Err(Failure::invalid()),
        };
        return match operation {
            Ok(operation) => match service
                .submit(Job::PublicModuleProfileNotes(operation))
                .await
            {
                Ok(value) => crate::json_response(StatusCode::OK, &value),
                Err(error) if error.code == "module_radio_busy" => {
                    module_profile_notes_busy_response()
                }
                Err(error) => crate::json_response(error.status, &json!({"error":error.message})),
            },
            Err(error) => crate::json_response(error.status, &json!({"error":error.message})),
        };
    }
    if request.uri().path() == "/api/esim/phonebook/probe" {
        if stopping {
            return crate::json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error":"Switch 正在退出，未提交新操作"}),
            );
        }
        if request.method() != Method::POST {
            return Failure::invalid().response();
        }
        return match service.submit(Job::PublicModulePhonebookProbe).await {
            Ok(value) => crate::json_response(StatusCode::OK, &value),
            Err(error) if error.code == "module_radio_busy" => module_profile_notes_busy_response(),
            Err(error) => crate::json_response(error.status, &json!({"error":error.message})),
        };
    }
    if request.method() == Method::GET && request.uri().path() == "/api/network" {
        if stopping {
            return Failure::stopped().response();
        }
        let started = Instant::now();
        let captured_at_ms = crate::network_diagnostic::now_ms();
        let span = crate::safe_events::begin_network_diagnostic();
        let host = crate::network_diagnostic::capture_host().await;
        if let Some(span) = &span {
            span.record("waiting_lock", "started", "pending");
            span.record("submit", "started", "pending");
        }
        let deadline = started + Duration::from_secs(20);
        let (raw, errors, completed_commands) = match service
            .submit(Job::PublicNetworkDiagnostic { deadline })
            .await
        {
            Ok(value) => (
                value
                    .get("raw")
                    .and_then(Value::as_object)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|(key, value)| {
                                value.as_str().map(|value| (key.clone(), value.to_owned()))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                value
                    .get("errors")
                    .and_then(Value::as_object)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|(key, value)| {
                                value.as_str().map(|value| (key.clone(), value.to_owned()))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                value
                    .get("completed_commands")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as usize,
            ),
            Err(error) => {
                let message = diagnostic_error_text(&error);
                let errors: BTreeMap<String, String> =
                    ["usbnet", "usbcfg", "cgdcont", "cgact", "cgpaddr"]
                        .into_iter()
                        .map(|key| (key.to_owned(), message.clone()))
                        .collect();
                (BTreeMap::new(), errors, 0)
            }
        };
        if let Some(span) = &span {
            if errors.is_empty() {
                span.record("complete", "success", "succeeded");
            } else {
                span.record("complete", "transport_failed", "failed");
            }
        }
        let response = crate::network_diagnostic::project(
            host,
            raw,
            errors,
            completed_commands,
            captured_at_ms,
            crate::network_diagnostic::elapsed_ms(started),
        );
        return crate::json_response(StatusCode::OK, &response);
    }
    if request.method() == Method::POST && request.uri().path() == "/api/network/usbnet" {
        if stopping {
            let error = Failure::stopped();
            return crate::json_response(error.status, &json!({"error":error.message}));
        }
        let data = match body(request, 1024 * 1024).await {
            Ok(data) => data,
            Err(error) if error.status == StatusCode::PAYLOAD_TOO_LARGE => {
                return crate::json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":"http: request body too large"}),
                );
            }
            Err(error) => {
                return crate::json_response(error.status, &json!({"error":error.message}));
            }
        };
        let (id, stripped) = match crate::public_mutation_receipt::split(&data, true) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let mode = match decode_usbnet_mode(&stripped) {
            Ok(mode) => mode,
            Err(error) => {
                return crate::json_response(error.status, &json!({"error":error.message}));
            }
        };
        if !(0..=1).contains(&mode) {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"only usbnet mode 0 or 1 is allowed"}),
            );
        }
        let binding = service.public_mutation_binding();
        return crate::public_mutation_receipt::run(
            &service.0.data_root,
            "/api/network/usbnet",
            id.as_deref().unwrap(),
            &data,
            binding.clone(),
            || async {
                match service.submit_usbnet_mode_bound(mode, &binding).await {
                    Ok(value) => crate::json_response(StatusCode::OK, &value),
                    Err(error) => error.response(),
                }
            },
        )
        .await;
    }
    if request.method() == Method::POST && request.uri().path() == "/api/module/flight-mode" {
        if stopping {
            return Failure::stopped().response();
        }
        let data = match body(request, 4 * 1024).await {
            Ok(v) => v,
            Err(e) => return e.response(),
        };
        let (id, stripped) = match crate::public_mutation_receipt::split(&data, true) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let input: SetRequest = match decode(&stripped) {
            Ok(v) => v,
            Err(e) => return e.response(),
        };
        let binding = service.public_mutation_binding();
        return crate::public_mutation_receipt::run(
            &service.0.data_root,
            "/api/module/flight-mode",
            id.as_deref().unwrap(),
            &data,
            binding.clone(),
            || async {
                match service.submit_bound(Job::PublicSet(input), &binding).await {
                    Ok(value) => crate::json_response(StatusCode::OK, &value),
                    Err(error) => error.response(),
                }
            },
        )
        .await;
    }
    let job = match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/module/flight-mode") => Ok(Job::PublicGet),
        (&Method::POST, "/api/network/reboot-module") if !stopping => Ok(Job::PublicReboot),
        _ => Err(if stopping {
            Failure::stopped()
        } else {
            Failure::invalid()
        }),
    };
    match job {
        Ok(job) => {
            let reboot = matches!(job, Job::PublicReboot);
            match service.submit(job).await {
                Ok(value) if reboot => crate::json_response(
                    StatusCode::ACCEPTED,
                    &json!({"accepted":true,"response":value["response"]}),
                ),
                Ok(value) => crate::json_response(StatusCode::OK, &value),
                Err(error) if reboot => error.module_radio_compat_response(),
                Err(error) => error.response(),
            }
        }
        Err(error) => error.response(),
    }
}
fn decode_usbnet_mode(data: &[u8]) -> Result<i64, Failure> {
    let value: Value = serde_json::from_slice(data).map_err(|error| {
        Failure::new(
            StatusCode::BAD_REQUEST,
            "invalid_radio_request",
            &error.to_string(),
        )
    })?;
    if value.is_null() {
        return Ok(0);
    }
    if !value.is_object() {
        return Err(Failure::new(
            StatusCode::BAD_REQUEST,
            "invalid_radio_request",
            "invalid request body",
        ));
    }
    let request: USBNetRequest = serde_json::from_value(value).map_err(|error| {
        Failure::new(
            StatusCode::BAD_REQUEST,
            "invalid_radio_request",
            &error.to_string(),
        )
    })?;
    Ok(request.mode.unwrap_or_default())
}

fn module_profile_notes_command_failure(context: &str, error: Failure) -> Failure {
    let mut wrapped = Failure::new(
        StatusCode::BAD_GATEWAY,
        "module_profile_notes_command_failed",
        &format!("{context}: {}", error.message),
    );
    wrapped.uncertain = error.uncertain;
    wrapped.submitted = error.submitted;
    wrapped
}

fn at_command_succeeded(response: &str) -> bool {
    let normalized = response.trim().replace("\r\n", "\n");
    normalized == "OK" || normalized.ends_with("\nOK")
}

fn phonebook_storage_supported(response: &str, storage: &str) -> bool {
    response
        .to_ascii_uppercase()
        .contains(&format!("\"{}\"", storage.to_ascii_uppercase()))
}

fn module_profile_notes_busy_response() -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::CONFLICT,
        &json!({
            "error":"已有 eSIM 操作正在进行，请等待完成后重试",
            "code":"esim_operation_busy",
            "recoverable":true
        }),
    )
}

fn diagnostic_error_text(error: &Failure) -> String {
    format!("模块射频操作未确认 ({})", error.code)
}
pub(crate) async fn handle_internal(
    request: Request<Incoming>,
    service: Service,
    stopping: bool,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST {
        return crate::json_response(StatusCode::NOT_FOUND, &json!({"error":"not found"}));
    }
    let path = request.uri().path().to_owned();
    let result = match body(request, 32 * 1024).await {
        Ok(data) => match path.strip_prefix(PREFIX) {
            Some("query") => decode(&data).map(Job::Query),
            Some("configure") => decode(&data).map(Job::Configure),
            Some("bootstrap") => decode(&data).map(Job::Bootstrap),
            Some("refresh") => decode(&data).map(Job::Refresh),
            Some("reservation") => decode(&data).map(Job::Reservation),
            Some("vowifi") => decode(&data).map(Job::Intent),
            Some("action") => decode(&data).map(Job::Action),
            _ => Err(Failure::invalid()),
        },
        Err(error) => Err(error),
    };
    match result {
        Ok(job) if stopping && !job.cleanup() => Failure::stopped().response(),
        Ok(job) => match service.submit(job).await {
            Ok(value) => crate::json_response(StatusCode::OK, &value),
            Err(error) => error.response(),
        },
        Err(error) => error.response(),
    }
}
#[path = "module_radio/call.rs"]
mod call;
pub(crate) use call::call_snapshot;
pub(crate) use call::{
    CallAction, CallAdmission, CallCapture, CallEvent, CallObserver, CallReservation,
    CallStateSnapshot,
};
#[path = "module_radio/call_mode.rs"]
mod call_mode;
pub(crate) use call_mode::CallModeReservation;
#[path = "module_radio/call_mode_commit.rs"]
mod call_mode_commit;
pub(crate) use call_mode_commit::CallModeCommitProof;
#[path = "module_radio/diagnostic.rs"]
mod diagnostic;
#[cfg(test)]
type PhysicalFixture =
    tokio::sync::mpsc::UnboundedSender<(Action, oneshot::Sender<Result<Outcome, Failure>>)>;
#[cfg(test)]
type TimerFixture = tokio::sync::mpsc::UnboundedSender<(Duration, oneshot::Sender<()>)>;
#[cfg(test)]
pub(crate) mod tests {
    include!("module_radio/tests.rs");
}
