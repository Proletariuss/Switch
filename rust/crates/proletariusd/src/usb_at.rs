//! The only USB AT handle owner. RPCs and native SMS collection reserve the
//! same transaction admission; a dropped HTTP waiter never releases live I/O.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
#[cfg(test)]
use proletarius_sim::ApduTransport;
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::{Notify, oneshot};
use zeroize::{Zeroize, Zeroizing};

use crate::module_esim_channel::AtCommandTransport;
use crate::module_sim_identity::*;
use crate::usb_at_transport::{self, CloseDisposition, Transport, TransportError};

pub(crate) const PREFIX: &str = "/api/internal/usb-at/";
mod device_observation;
mod esim_health;
mod esim_snapshot;
mod native_module;
mod native_sms_route;
mod native_startup;
mod reconnect;
#[cfg(test)]
pub(crate) mod reconnect_tests;
pub(crate) use reconnect::CallModeUsbAnchor;
mod sms_observation;
pub(crate) use device_observation::DeviceObservation;
pub(crate) use esim_snapshot::EsimRefreshBinding;
pub(crate) use esim_snapshot::EsimSnapshotKey;
pub(crate) use native_module::NativeModuleIdentity;
pub(crate) use sms_observation::{SmsObservationToken, SmsPollBinding};
pub(crate) const MAX_BYTES: usize = 1024 * 1024;
const MAX_STORAGE_BYTES: usize = 240 * 1024;
const MAX_WAIT: Duration = Duration::from_secs(60);
const MAX_CLOSE_FAILURES: usize = 64;
const DEVICE_STATUS_BUDGET: Duration = Duration::from_secs(55);
const SMS_STORAGE_BUDGET: Duration = Duration::from_secs(120);
const SMS_STORAGE_CLEANUP_RESERVE: Duration = Duration::from_secs(12);
const OPEN_BACKOFF: Duration = Duration::from_secs(2);
type Factory = Arc<dyn Fn() -> Result<Box<dyn Transport>, TransportError> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct Owner {
    inner: Arc<Inner>,
    profile_notes: Option<crate::profile_notes::Store>,
    hardware_observer: crate::module_discovery::Observer,
}

/// A read-only capture of this owner's current connection. Fields cannot be
/// supplied by an HTTP caller. Acquisition checks the same owner, producer,
/// epoch, identity generation and session while installing the voice lease.
/// No target means no AT handle was open, not proof of a serial/ADB binding.
#[derive(Clone)]
pub(crate) struct CapturedVoiceTarget {
    owner: Arc<Inner>,
    producer: String,
    epoch: u64,
    identity_generation: u64,
    session: String,
    target: Option<usb_at_transport::PhysicalTarget>,
}
impl CapturedVoiceTarget {
    pub(crate) fn physical_target(&self) -> Option<&usb_at_transport::PhysicalTarget> {
        self.target.as_ref()
    }
    fn matches(&self, inner: &Arc<Inner>, state: &State) -> bool {
        Arc::ptr_eq(&self.owner, inner)
            && self.producer == state.producer
            && self.epoch == state.epoch
            && self.identity_generation == state.identity_generation
            && self.session == state.session
    }
}
impl std::fmt::Debug for CapturedVoiceTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedVoiceTarget")
            .field("native_connection_captured", &self.target.is_some())
            .finish_non_exhaustive()
    }
}

/// A generation-bound reservation that prevents SIM/AT work while the module
/// voice helper changes the USB composition. It is owned by the Rust USB AT
/// actor so the caller cannot release a different producer's lease.
#[must_use = "the voice transition reservation must remain alive until the helper operation finishes"]
pub(crate) struct VoiceTransitionLease {
    owner: Owner,
    producer: String,
    session: String,
    lease_id: String,
    sequence: u64,
    released: bool,
}

impl VoiceTransitionLease {
    /// Native operation owners await physical I/O and transport close, rather
    /// than an RPC observation deadline. A returned error is a settled result
    /// and must not start a second, unobserved cleanup from Drop.
    pub(crate) async fn release_confirmed(mut self) -> Result<(), Failure> {
        let result = self
            .owner
            .release_with_deadline(
                ReleaseRequest {
                    producer_id: self.producer.clone(),
                    session_id: self.session.clone(),
                    lease_id: self.lease_id.clone(),
                    lease_seq: self.sequence,
                },
                None,
            )
            .await
            .map(|_| ());
        self.released = true;
        result
    }

    pub(crate) async fn release(mut self) -> Result<(), Failure> {
        let result = self
            .owner
            .release(ReleaseRequest {
                producer_id: self.producer.clone(),
                session_id: self.session.clone(),
                lease_id: self.lease_id.clone(),
                lease_seq: self.sequence,
            })
            .await
            .map(|_| ());
        self.released = result.is_ok();
        result
    }
}

impl Drop for VoiceTransitionLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let owner = self.owner.clone();
        let producer = self.producer.clone();
        let session = self.session.clone();
        let lease_id = self.lease_id.clone();
        let sequence = self.sequence;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = owner
                    .release(ReleaseRequest {
                        producer_id: producer,
                        session_id: session,
                        lease_id,
                        lease_seq: sequence,
                    })
                    .await
                {
                    crate::diagnostics::write_line(format_args!(
                        "switchd-rs: voice USB lease cleanup unconfirmed: {}",
                        error.code
                    ));
                }
            });
        }
    }
}

struct Inner {
    state: Mutex<State>,
    adoption_update: Mutex<()>,
    esim_snapshot_gate: tokio::sync::Mutex<()>,
    port: Mutex<Option<Box<dyn Transport>>>,
    factory: Factory,
    #[cfg(test)]
    reconnect_opener: Mutex<Option<reconnect::TestOpener>>,
    call_capture: Arc<Mutex<Option<crate::module_radio::CallCapture>>>,
    changed: Notify,
    #[cfg(test)]
    spawned_io: std::sync::atomic::AtomicU64,
}
struct State {
    producer: String,
    module_recovery: crate::module_recovery::Gate,
    epoch: u64,
    identity_generation: u64,
    module_identity: Option<ModuleSimIdentityCache>,
    esim_snapshot: esim_snapshot::Cache,
    device_observation: Option<device_observation::DeviceObservation>,
    native_sms_identity: Option<(String, u64)>,
    native_managed: bool,
    native_sms_ready_session: Option<String>,
    sms_observation_generation: u64,
    session: String,
    description: String,
    last_closed: String,
    // Identity is installed with the actual port, independent of logical revoke.
    port_identity: Option<PortIdentity>,
    close_failures: Vec<(PortIdentity, Failure)>,
    last_close_result: Option<(PortIdentity, Result<CloseDisposition, Failure>)>,
    port_sequence: u64,
    reconnect: Option<reconnect::Connection>,
    native_module_hold: Option<native_module::Hold>,
    lease: Option<Lease>,
    lease_high_water: u64,
    busy: bool,
    busy_lease: String,
    revoking: bool,
    stopping: bool,
    next_open: Option<Instant>,
    sms: Option<SmsConfig>,
    adopt: bool,
    config_high_water: u64,
    inhibits: HashMap<String, u64>,
    inhibit_high_water: u64,
    radio_inhibit: Option<String>,
    cellular_suppressed: bool,
}
#[derive(Clone)]
struct PortIdentity {
    producer: String,
    session: String,
    epoch: u64,
    connection: u64,
    lease_id: String,
    lease_sequence: u64,
    // An attempt's finite reconnect owner, never reconstructed from wire data.
    reconnect: Option<std::sync::Weak<reconnect::Anchor>>,
}
impl PortIdentity {
    fn current(state: &State) -> Self {
        Self {
            producer: state.producer.clone(),
            session: state.session.clone(),
            epoch: state.epoch,
            connection: state.port_sequence,
            lease_id: state
                .lease
                .as_ref()
                .map_or_else(String::new, |lease| lease.id.clone()),
            lease_sequence: state.lease.as_ref().map_or(0, |lease| lease.sequence),
            reconnect: None,
        }
    }
    fn same_connection(&self, other: &Self) -> bool {
        self.producer == other.producer
            && self.epoch == other.epoch
            && self.connection == other.connection
    }
}
fn close_failure_for_lease(
    state: &State,
    producer: &str,
    id: &str,
    sequence: u64,
) -> Option<Failure> {
    state
        .close_failures
        .iter()
        .rev()
        .find(|(identity, _)| {
            identity.producer == producer
                && identity.lease_id == id
                && identity.lease_sequence == sequence
        })
        .map(|(_, error)| error.clone())
}
fn close_failure_for(state: &State, producer: &str, session: &str) -> Option<Failure> {
    if session.is_empty() {
        return None;
    }
    state
        .close_failures
        .iter()
        .rev()
        .find(|(identity, _)| identity.producer == producer && identity.session == session)
        .map(|(_, error)| error.clone())
}
struct ModuleSimIdentityCache {
    generation: u64,
    iccid: Zeroizing<String>,
    imsi: Zeroizing<String>,
}
#[derive(Clone)]
struct Lease {
    id: String,
    sequence: u64,
    session: String,
    purpose: Purpose,
    releasing: bool,
    connection: Option<PortIdentity>,
    _authority: IoAuthority,
}

#[derive(Clone)]
enum IoAuthority {
    Admitted {
        _guard: Arc<crate::module_recovery::OperationGuard>,
    },
    Retained {
        _right: crate::module_recovery::OperationRight,
    },
    Drain,
}

fn new_io_authority(state: &State, producer: &str) -> Result<IoAuthority, Failure> {
    state
        .module_recovery
        .admit_operation(producer)
        .map(|guard| IoAuthority::Admitted {
            _guard: Arc::new(guard),
        })
        .map_err(|_| {
            Failure::new(
                StatusCode::CONFLICT,
                crate::module_recovery::CODE,
                crate::module_recovery::MESSAGE,
            )
        })
}
fn retained_io_authority(
    state: &State,
    producer: &str,
    right: &crate::module_recovery::OperationRight,
) -> Result<IoAuthority, Failure> {
    if right.is_active_for(&state.module_recovery, producer) {
        Ok(IoAuthority::Retained {
            _right: right.clone(),
        })
    } else {
        Err(Failure::stale())
    }
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Purpose {
    Sim,
    Voice,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SmsConfig {
    serial: bool,
    adopt: bool,
    interval_ms: u64,
}

#[derive(Clone)]
pub(crate) struct Failure {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) response: String,
    pub(crate) uncertain: bool,
}
impl Failure {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            response: String::new(),
            uncertain: false,
        }
    }
    fn invalid() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "usb_at_invalid_request",
            "USB AT request is invalid",
        )
    }
    fn stale() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "usb_at_stale_session",
            "USB AT producer or session changed",
        )
    }
    fn stopped() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "usb_at_stopping",
            "USB AT owner is stopping",
        )
    }
    fn busy() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "usb_at_busy",
            "USB AT owner is busy",
        )
    }
    fn timeout() -> Self {
        Self::new(
            StatusCode::REQUEST_TIMEOUT,
            "usb_at_wait_timeout",
            "USB AT admission deadline expired",
        )
    }
    pub(crate) fn response(self) -> Response<Full<Bytes>> {
        crate::json_response(
            self.status,
            &json!({
                "code":self.code,"error":self.message,"response":self.response,
                "uncertain":self.uncertain,"retryable":false
            }),
        )
    }
    pub(crate) fn public_status(&self) -> StatusCode {
        self.status
    }
    pub(crate) fn public_code(&self) -> &'static str {
        self.code
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Failure")
            .field("code", &self.code)
            .field("message", &self.message)
            .field("response_bytes", &self.response.len())
            .field("uncertain", &self.uncertain)
            .finish()
    }
}
impl std::error::Error for Failure {}
impl From<TransportError> for Failure {
    fn from(error: TransportError) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: error.code,
            message: error.message,
            response: error.response,
            uncertain: error.uncertain,
        }
    }
}

struct EsimCommandAdapter<'a> {
    inner: &'a Inner,
    port: &'a mut dyn Transport,
    epoch: u64,
    producer: String,
    session: String,
    transport_failed: bool,
    allow_sim_identity: bool,
    radio_permit: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SimIdentityRequest {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SimProfileCheckRequest {
    session_generation: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SimAkaRequest {
    application: String,
    session_generation: u64,
    rand: String,
    autn: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SmsSubmitRequest {
    producer_id: String,
    session_id: String,
    lease_id: String,
    recipient: String,
    message: String,
    expected_iccid: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct SmsSubmitOutcome {
    pub(crate) accepted_segments: usize,
    pub(crate) complete: bool,
}

impl Drop for SimAkaRequest {
    fn drop(&mut self) {
        self.rand.zeroize();
        self.autn.zeroize();
    }
}

fn decode_module_challenge(value: &str) -> Option<Zeroizing<[u8; 16]>> {
    if value.len() != 32 {
        return None;
    }
    let mut decoded = Zeroizing::new([0; 16]);
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        decoded[index] = (high << 4) | low;
    }
    Some(decoded)
}

fn encode_module_secret(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02X}");
    }
    encoded
}

impl AtCommandTransport for EsimCommandAdapter<'_> {
    type Error = Failure;

    fn command(
        &mut self,
        command: &str,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<String, Self::Error> {
        if !(valid_euicc_at_command(command)
            || self.allow_sim_identity && valid_module_identity_at_command(command))
            || !(1..=60_000).contains(&timeout.as_millis())
            || max_response_bytes == 0
            || max_response_bytes > MAX_BYTES
        {
            return Err(Failure::invalid());
        }
        check_radio_fence(
            self.inner,
            self.epoch,
            &self.producer,
            &self.session,
            self.radio_permit.as_deref(),
        )?;
        check_cellular_command(self.inner, command)?;
        let mut response = match self
            .port
            .command(command, None, timeout, max_response_bytes)
        {
            Ok(response) => response,
            Err(error) => {
                self.transport_failed |= error.uncertain
                    || error.code == "usb_at_disconnected"
                    || error.code == "usb_at_timeout";
                return Err(Failure::from(error));
            }
        };
        if response.len() > max_response_bytes {
            response.zeroize();
            return Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "usb_at_response_too_large",
                "USB AT 响应超过 eSIM APDU 事务上限",
            ));
        }
        if let Err(mut error) = check_radio_fence(
            self.inner,
            self.epoch,
            &self.producer,
            &self.session,
            self.radio_permit.as_deref(),
        ) {
            error.uncertain = true;
            response.zeroize();
            return Err(error);
        }
        Ok(response)
    }
}

pub(crate) fn valid_euicc_at_command(command: &str) -> bool {
    if command.len() > 131_200 || command.chars().any(char::is_control) {
        return false;
    }
    if let Some(aid) = command
        .strip_prefix("AT+CCHO=\"")
        .and_then(|value| value.strip_suffix('"'))
    {
        return valid_upper_hex(aid, 2, 64);
    }
    if let Some(channel) = command.strip_prefix("AT+CCHC=") {
        return channel
            .parse::<u8>()
            .is_ok_and(|value| (1..=19).contains(&value));
    }
    let Some(arguments) = command.strip_prefix("AT+CGLA=") else {
        return false;
    };
    let Some((channel_length, encoded)) = arguments.split_once(",\"") else {
        return false;
    };
    let Some(encoded) = encoded.strip_suffix('"') else {
        return false;
    };
    let Some((channel, length)) = channel_length.split_once(',') else {
        return false;
    };
    channel
        .parse::<u8>()
        .is_ok_and(|value| (1..=19).contains(&value))
        && length
            .parse::<usize>()
            .is_ok_and(|value| value == encoded.len())
        && valid_upper_hex(encoded, 2, 131_070)
}

fn valid_module_identity_at_command(command: &str) -> bool {
    if matches!(command, "AT+CNUM" | "AT+CSCA?" | "AT+QCCID" | "AT+CIMI") {
        return true;
    }
    valid_module_csim_command(command)
}

fn valid_upper_hex(value: &str, minimum: usize, maximum: usize) -> bool {
    value.len() >= minimum
        && value.len() <= maximum
        && value.len().is_multiple_of(2)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    producer_id: String,
    #[serde(default)]
    lease_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityRequest {
    producer_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseRequest {
    producer_id: String,
    session_id: String,
    lease_id: String,
    lease_seq: u64,
    purpose: Purpose,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRequest {
    producer_id: String,
    session_id: String,
    lease_id: String,
    lease_seq: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseRequest {
    producer_id: String,
    session_id: String,
    #[serde(default)]
    lease_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandRequest {
    producer_id: String,
    session_id: String,
    #[serde(default)]
    lease_id: String,
    command: String,
    #[serde(default)]
    follow_up_base64: Option<String>,
    timeout_ms: u64,
    #[serde(default)]
    max_bytes: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceStatusRequest {
    producer_id: String,
    session_id: String,
    lease_id: String,
    suppress_cellular: bool,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum ConfigureRequest {
    Sms {
        producer_id: String,
        config_seq: u64,
        serial: bool,
        poll_interval_ms: u64,
    },
    Inhibit {
        producer_id: String,
        inhibit_seq: u64,
        token: String,
        enabled: bool,
    },
}

impl Owner {
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub(crate) fn new(producer: String) -> Self {
        Self::new_with_adopt(producer, false)
    }
    pub(crate) fn new_with_adopt(producer: String, adopt: bool) -> Self {
        Self::with_factory_and_adopt(producer, Arc::new(usb_at_transport::open), adopt)
    }
    pub(crate) fn new_with_adopt_and_recovery(
        producer: String,
        adopt: bool,
        gate: crate::module_recovery::Gate,
    ) -> Self {
        Self::with_factory_adopt_recovery(producer, Arc::new(usb_at_transport::open), adopt, gate)
    }
    pub(crate) fn with_profile_notes(mut self, store: crate::profile_notes::Store) -> Self {
        self.profile_notes = Some(store);
        self
    }

    /// Revoke any previous USB AT handle and reserve the current physical
    /// owner for the full voice-route operation. No SIM command can reopen a
    /// replacement handle until the returned lease is released.
    pub(crate) async fn acquire_voice_transition(
        &self,
        expected_producer: &str,
    ) -> Result<VoiceTransitionLease, Failure> {
        self.acquire_voice_transition_with_capture(expected_producer, None, None)
            .await
    }

    /// Read only the already opened AT transport. Do not open a replacement or
    /// issue an AT command to manufacture identity for this capture. Blocking
    /// handle locking stays off the async executor, including on cancellation.
    pub(crate) async fn capture_voice_target(
        &self,
        expected_producer: &str,
    ) -> Result<CapturedVoiceTarget, Failure> {
        validate_token(expected_producer)?;
        let owner = self.clone();
        let producer = expected_producer.to_owned();
        tokio::task::spawn_blocking(move || {
            let port = owner.inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let state = owner.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            // Capturing a reference is not admission to execute or close I/O.
            // Read-only Status and inactive Stop historically need no USB
            // transition lease, including while the owner is stopping. The
            // captured acquire below still checks stopping/busy/lease state.
            if state.module_recovery.is_quarantined() {
                return Err(Failure::new(
                    StatusCode::CONFLICT,
                    crate::module_recovery::CODE,
                    crate::module_recovery::MESSAGE,
                ));
            }
            if state.producer != producer {
                return Err(Failure::stale());
            }
            if state.revoking {
                return Err(Failure::busy());
            }
            if state.session.is_empty() {
                if let Some((_, Err(error))) = &state.last_close_result {
                    return Err(error.clone());
                }
            }
            let target = match port.as_deref() {
                Some(port) if !state.session.is_empty() => {
                    Some(port.physical_target().map_err(Failure::from)?)
                }
                None if state.session.is_empty() => None,
                _ => return Err(Failure::stale()),
            };
            Ok(CapturedVoiceTarget {
                owner: owner.inner.clone(),
                producer,
                epoch: state.epoch,
                identity_generation: state.identity_generation,
                session: state.session.clone(),
                target,
            })
        })
        .await
        .map_err(|_| Failure::stopped())?
    }

    pub(crate) async fn acquire_voice_transition_captured(
        &self,
        expected_producer: &str,
        captured: CapturedVoiceTarget,
    ) -> Result<VoiceTransitionLease, Failure> {
        self.acquire_voice_transition_with_capture(expected_producer, Some(captured), None)
            .await
    }

    pub(crate) async fn acquire_voice_transition_captured_with_right(
        &self,
        expected_producer: &str,
        captured: CapturedVoiceTarget,
        right: &crate::module_recovery::OperationRight,
    ) -> Result<VoiceTransitionLease, Failure> {
        self.acquire_voice_transition_with_capture(
            expected_producer,
            Some(captured),
            Some(right.clone()),
        )
        .await
    }

    pub(crate) async fn validate_voice_cleanup_capture(
        &self,
        expected_producer: &str,
        original: &CapturedVoiceTarget,
    ) -> Result<CapturedVoiceTarget, Failure> {
        if !Arc::ptr_eq(&self.inner, &original.owner) || original.producer != expected_producer {
            return Err(Failure::stale());
        }
        {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(error) = close_failure_for(&state, &original.producer, &original.session) {
                return Err(error);
            }
        }
        let current = self.capture_voice_target(expected_producer).await?;
        match (&original.target, &current.target) {
            (_, None) => Ok(current),
            (Some(original), Some(now)) if original.same_connection(now) => Ok(current),
            _ => Err(Failure::stale()),
        }
    }

    async fn acquire_voice_transition_with_capture(
        &self,
        expected_producer: &str,
        captured: Option<CapturedVoiceTarget>,
        right: Option<crate::module_recovery::OperationRight>,
    ) -> Result<VoiceTransitionLease, Failure> {
        // The physical close can outlive the HTTP request. Keep acquisition in
        // an owner task so cancellation cannot strand the newly-installed
        // voice lease after `finish_revoke` completes.
        let owner = self.clone();
        let expected_producer = expected_producer.to_owned();
        let (sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let result = owner
                .acquire_voice_transition_inner(&expected_producer, captured, right)
                .await;
            if let Err(result) = sender.send(result)
                && let Ok(lease) = result
                && let Err(error) = lease.release().await
            {
                crate::diagnostics::write_line(format_args!(
                    "switchd-rs: canceled voice USB lease acquisition cleanup unconfirmed: {}",
                    error.code
                ));
            }
        });
        receiver.await.unwrap_or_else(|_| Err(Failure::stopped()))
    }

    async fn acquire_voice_transition_inner(
        &self,
        expected_producer: &str,
        captured: Option<CapturedVoiceTarget>,
        right: Option<crate::module_recovery::OperationRight>,
    ) -> Result<VoiceTransitionLease, Failure> {
        let (producer, session, sequence) = {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            check(&state, expected_producer)?;
            if state.stopping {
                return Err(Failure::stopped());
            }
            let sequence = state
                .lease_high_water
                .checked_add(1)
                .ok_or_else(Failure::invalid)?;
            (state.producer.clone(), state.session.clone(), sequence)
        };
        let lease_id = crate::multistandby_store::random_token().map_err(|_| {
            Failure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "usb_at_entropy_unavailable",
                "无法为模块语音路由分配安全租约",
            )
        })?;
        self.acquire_with_capture(
            LeaseRequest {
                producer_id: producer.clone(),
                session_id: session.clone(),
                lease_id: lease_id.clone(),
                lease_seq: sequence,
                purpose: Purpose::Voice,
            },
            captured.as_ref(),
            right.as_ref(),
        )
        .await?;
        Ok(VoiceTransitionLease {
            owner: self.clone(),
            producer,
            session,
            lease_id,
            sequence,
            released: false,
        })
    }
    #[cfg(test)]
    pub(crate) fn unavailable_for_tests(producer: String) -> Self {
        Self::with_factory(
            producer,
            Arc::new(|| {
                Err(usb_at_transport::TransportError {
                    code: "usb_at_not_found",
                    message: "test USB AT device is absent".into(),
                    response: String::new(),
                    uncertain: false,
                })
            }),
        )
    }
    pub(crate) fn with_factory(producer: String, factory: Factory) -> Self {
        Self::with_factory_and_adopt(producer, factory, false)
    }

    #[cfg(test)]
    pub(crate) fn spawned_io_for_tests(&self) -> u64 {
        self.inner
            .spawned_io
            .load(std::sync::atomic::Ordering::Acquire)
    }
    #[cfg(test)]
    pub(crate) fn with_recovery_gate_for_tests(self, gate: crate::module_recovery::Gate) -> Self {
        {
            let mut state = self.inner.state.lock().unwrap();
            assert!(!state.busy && !state.revoking && state.lease.is_none());
            state.module_recovery = gate;
        }
        self
    }

    /// Return the active handle's product description without making a second
    /// device query. This mirrors the identity source used by the legacy
    /// health projection while keeping callers inside the single USB owner.
    pub(crate) fn active_description(&self) -> Option<String> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (!state.session.is_empty()).then(|| state.description.clone())
    }

    /// Read-only hardware discovery is fenced to the physical owner. An ioreg
    /// result captured across close/replacement must never describe a new card.
    pub(crate) async fn native_hardware_observation(
        &self,
    ) -> Result<crate::module_discovery::UsbFallback, Failure> {
        let (epoch, identity_generation, _authority) = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check(&state, &state.producer)?;
            (
                state.epoch,
                state.identity_generation,
                new_io_authority(&state, &state.producer)?,
            )
        };
        let value = self
            .hardware_observer
            .capture_usb_fallback(self, None, identity_generation)
            .await;
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check_epoch(&state, epoch)?;
        if state.identity_generation != identity_generation {
            return Err(Failure::stale());
        }
        Ok(value)
    }

    /// Synchronous observation only; does not acquire a SIM lease or issue AT.
    /// Public durable mutations bind to the originally observed card generation.
    pub(crate) fn public_mutation_binding(&self) -> String {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        format!(
            "{}:{}:{}:{}",
            state.producer, state.session, state.epoch, state.identity_generation
        )
    }
    pub(crate) fn hardware_observation_current(&self, identity_generation: u64) -> bool {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        !state.stopping && !state.revoking && state.identity_generation == identity_generation
    }

    #[cfg(test)]
    pub(crate) fn with_hardware_observer(
        mut self,
        observer: crate::module_discovery::Observer,
    ) -> Self {
        self.hardware_observer = observer;
        self
    }

    /// Execute an entire eSIM logical-channel transaction under one USB AT
    /// admission. This prevents SMS, VoWiFi AKA, diagnostics, or another
    /// profile operation from interleaving CCHO/APDU/CCHC commands.
    pub(crate) async fn esim_transaction<T, F>(&self, operation: F) -> Result<T, Failure>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a mut dyn AtCommandTransport<Error = Failure>) -> Result<T, Failure>
            + Send
            + 'static,
    {
        self.esim_transaction_mode(operation, true).await
    }

    async fn esim_transaction_mode<T, F>(&self, operation: F, mutating: bool) -> Result<T, Failure>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a mut dyn AtCommandTransport<Error = Failure>) -> Result<T, Failure>
            + Send
            + 'static,
    {
        let (producer, session) = {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (
                state.producer.clone(),
                (!state.session.is_empty()).then(|| state.session.clone()),
            )
        };
        let deadline = Instant::now() + MAX_WAIT;
        let permit = self
            .admit(&producer, session.as_deref(), "", deadline)
            .await?;
        self.run(permit, move |inner, epoch| {
            ensure_open(inner, epoch)?;
            if mutating {
                inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .esim_snapshot
                    .invalidate(false);
            }
            let (producer, session) = {
                let state = inner
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                check_epoch(&state, epoch)?;
                (state.producer.clone(), state.session.clone())
            };
            if session.is_empty() {
                return Err(Failure::stale());
            }
            let mut port = inner.port.lock().unwrap_or_else(|error| error.into_inner());
            check_epoch(
                &inner
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
                epoch,
            )?;
            let transport = port.as_mut().ok_or_else(Failure::stale)?;
            let mut adapter = EsimCommandAdapter {
                inner,
                port: transport.as_mut(),
                epoch,
                producer,
                session,
                transport_failed: false,
                allow_sim_identity: false,
                radio_permit: None,
            };
            let result = operation(&mut adapter);
            let transport_failed = adapter.transport_failed;
            drop(adapter);
            if transport_failed {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            if let Err(mut error) = check_epoch(
                &inner
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
                epoch,
            ) {
                error.uncertain = true;
                return Err(error);
            }
            if transport_failed && result.is_ok() {
                let mut error = Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "usb_at_uncertain_transaction",
                    "USB AT 连接在 eSIM 卡片读取期间中断",
                );
                error.uncertain = true;
                return Err(error);
            }
            result
        })
        .await
    }

    /// Execute a fixed Rust-owned module SIM identity read sequence. The
    /// adapter admits only CSIM, CNUM, and CSCA? commands for this transaction;
    /// caller-provided raw AT commands are never accepted by the identity RPC.
    pub(crate) async fn sim_identity_transaction<T, F>(&self, operation: F) -> Result<T, Failure>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(
                &'a mut dyn AtCommandTransport<Error = Failure>,
                u64,
            ) -> Result<T, Failure>
            + Send
            + 'static,
    {
        self.sim_identity_transaction_with_radio(operation, None)
            .await
    }
    async fn sim_identity_transaction_with_radio<T, F>(
        &self,
        operation: F,
        radio_permit: Option<String>,
    ) -> Result<T, Failure>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(
                &'a mut dyn AtCommandTransport<Error = Failure>,
                u64,
            ) -> Result<T, Failure>
            + Send
            + 'static,
    {
        let (producer, session) = {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (
                state.producer.clone(),
                (!state.session.is_empty()).then(|| state.session.clone()),
            )
        };
        let deadline = Instant::now() + MAX_WAIT;
        let permit = self
            .admit_waiting(&producer, session.as_deref(), deadline)
            .await?;
        self.run(permit, move |inner, epoch| {
            ensure_open(inner, epoch)?;
            let (producer, session, identity_generation) = {
                let state = inner
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                check_epoch(&state, epoch)?;
                (
                    state.producer.clone(),
                    state.session.clone(),
                    state.identity_generation,
                )
            };
            if session.is_empty() || identity_generation == 0 {
                return Err(Failure::stale());
            }
            let mut port = inner.port.lock().unwrap_or_else(|error| error.into_inner());
            check_epoch(
                &inner
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
                epoch,
            )?;
            let transport = port.as_mut().ok_or_else(Failure::stale)?;
            let mut adapter = EsimCommandAdapter {
                inner,
                port: transport.as_mut(),
                epoch,
                producer,
                session,
                transport_failed: false,
                allow_sim_identity: true,
                radio_permit,
            };
            let result = operation(&mut adapter, identity_generation);
            let transport_failed = adapter.transport_failed;
            drop(adapter);
            if transport_failed {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            if let Err(mut error) = check_epoch(
                &inner
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
                epoch,
            ) {
                error.uncertain = true;
                return Err(error);
            }
            if transport_failed && result.is_ok() {
                let mut error = Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "usb_at_uncertain_identity_transaction",
                    "USB AT 连接在 SIM 身份读取期间中断",
                );
                error.uncertain = true;
                return Err(error);
            }
            result
        })
        .await
    }

    async fn sim_identity(&self, stopping: bool) -> Result<Value, Failure> {
        self.sim_identity_bound(stopping, None, None).await
    }
    async fn sim_identity_bound(
        &self,
        stopping: bool,
        binding: Option<(String, String)>,
        radio_permit: Option<String>,
    ) -> Result<Value, Failure> {
        if stopping {
            return Err(Failure::stopped());
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let owner = self.clone();
        let (response, iccid, imsi, observed_generation) = self
            .sim_identity_transaction_with_radio(move |command, session_generation| {
            if let Some((producer, session)) = &binding {
                owner.native_module_session(producer, session)?;
            }
            let crate::module_sim_identity::IdentityRead { metadata, phone_number, iccid, imsi } =
                crate::module_sim_identity::read_identity(command, deadline)?;
            Ok((
                json!({"metadata":metadata,"phone_number":phone_number,"session_generation":session_generation}),
                iccid,
                imsi,
                session_generation,
            ))
        }, radio_permit)
            .await?;
        self.commit_module_identity(observed_generation, response, iccid, imsi)
    }

    fn commit_module_identity(
        &self,
        observed_generation: u64,
        mut response: Value,
        iccid: Zeroizing<String>,
        imsi: Zeroizing<String>,
    ) -> Result<Value, Failure> {
        if !valid_module_sim_identifier(&iccid, 10, 22)
            || !valid_module_sim_identifier(&imsi, 5, 20)
        {
            return Err(Failure::invalid());
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.identity_generation != observed_generation
            || state.session.is_empty()
            || state.revoking
            || state.stopping
        {
            return Err(Failure::stale());
        }
        let session_generation = state
            .identity_generation
            .checked_add(1)
            .ok_or_else(Failure::stopped)?;
        state.esim_snapshot.identity_observed(&iccid);
        state.identity_generation = session_generation;
        response["session_generation"] = json!(session_generation);
        state.module_identity = Some(ModuleSimIdentityCache {
            generation: session_generation,
            iccid,
            imsi,
        });
        Ok(response)
    }

    fn module_identity_for_generation(
        &self,
        expected_generation: u64,
    ) -> Result<(Zeroizing<String>, Zeroizing<String>), Failure> {
        if expected_generation == 0 {
            return Err(Failure::invalid());
        }
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.identity_generation != expected_generation
            || state.session.is_empty()
            || state.revoking
            || state.stopping
        {
            return Err(Failure::stale());
        }
        let identity = state
            .module_identity
            .as_ref()
            .filter(|identity| identity.generation == expected_generation)
            .ok_or_else(Failure::stale)?;
        Ok((
            Zeroizing::new(identity.iccid.to_string()),
            Zeroizing::new(identity.imsi.to_string()),
        ))
    }

    async fn sim_profile_check(
        &self,
        input: SimProfileCheckRequest,
        stopping: bool,
    ) -> Result<Value, Failure> {
        self.sim_profile_check_bound(input, stopping, None).await
    }
    async fn sim_profile_check_bound(
        &self,
        input: SimProfileCheckRequest,
        stopping: bool,
        binding: Option<NativeModuleIdentity>,
    ) -> Result<Value, Failure> {
        if stopping {
            return Err(Failure::stopped());
        }
        let expected_generation = input.session_generation;
        drop(input);
        let (expected_iccid, expected_imsi) =
            self.module_identity_for_generation(expected_generation)?;
        let deadline = Instant::now() + Duration::from_secs(12);
        let radio_permit = binding.as_ref().map(|v| v.radio_permit.clone());
        let owner = self.clone();
        self.sim_identity_transaction_with_radio(
            move |command, session_generation| {
                if binding
                    .as_ref()
                    .is_some_and(|binding| !owner.native_module_current(binding))
                {
                    return Err(Failure::stale());
                }
                if session_generation != expected_generation {
                    return Err(Failure::stale());
                }
                crate::module_sim_identity::profile_check(
                    command,
                    &expected_iccid,
                    &expected_imsi,
                    deadline,
                )
            },
            radio_permit,
        )
        .await
    }

    async fn sim_aka(&self, input: SimAkaRequest, stopping: bool) -> Result<Value, Failure> {
        if stopping {
            return Err(Failure::stopped());
        }
        let application = match input.application.as_str() {
            "usim" => proletarius_reader::AkaApplication::Usim,
            "isim" => proletarius_reader::AkaApplication::Isim,
            _ => return Err(Failure::invalid()),
        };
        let rand = decode_module_challenge(&input.rand).ok_or_else(Failure::invalid)?;
        let autn = decode_module_challenge(&input.autn).ok_or_else(Failure::invalid)?;
        let expected_generation = input.session_generation;
        if expected_generation == 0 {
            return Err(Failure::invalid());
        }
        drop(input);
        let (iccid, imsi) = self.module_identity_for_generation(expected_generation)?;
        self.sim_identity_transaction(move |command, session_generation| {
            if session_generation != expected_generation {
                return Err(Failure::stale());
            }
            let mut transport = ModuleSimApduTransport {
                command,
                deadline: Instant::now() + Duration::from_secs(60),
            };
            match proletarius_reader::authenticate_module_bound_with_transport(
                &mut transport,
                application,
                &iccid,
                &imsi,
                &rand[..],
                &autn[..],
            ) {
                Ok(proletarius_sim::AkaResult::Success { res, ck, ik }) => Ok(json!({
                    "outcome":"success",
                    "res":encode_module_secret(&res),
                    "ck":encode_module_secret(&ck),
                    "ik":encode_module_secret(&ik)
                })),
                Ok(proletarius_sim::AkaResult::SynchronizationFailure { auts }) => Ok(json!({
                    "outcome":"synchronization_failure",
                    "auts":encode_module_secret(&auts)
                })),
                Ok(proletarius_sim::AkaResult::AuthenticationReject) => {
                    Ok(json!({"outcome":"authentication_reject"}))
                }
                Err(proletarius_reader::AuthenticationError::IdentityChanged) => {
                    Ok(json!({"outcome":"identity_changed"}))
                }
                Err(proletarius_reader::AuthenticationError::ApplicationUnavailable) => Ok(json!({
                    "outcome":if application == proletarius_reader::AkaApplication::Isim {
                        "isim_unavailable"
                    } else {
                        "usim_unavailable"
                    }
                })),
                Err(_) => Err(Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "module_sim_aka_failed",
                    "模块 SIM 鉴权失败",
                )),
            }
        })
        .await
    }

    pub(crate) fn with_legacy_current_seed(
        producer: String,
        adopt: bool,
        gate: crate::module_recovery::Gate,
        seed: crate::startup_recovery::legacy_current::Seed,
    ) -> Self {
        let opener_gate = gate.clone();
        let seed = Arc::new(seed);
        Self::with_factory_adopt_recovery(
            producer,
            Arc::new(move || seed.open(&opener_gate)),
            adopt,
            gate,
        )
    }

    fn with_factory_and_adopt(producer: String, factory: Factory, adopt: bool) -> Self {
        Self::with_factory_adopt_recovery(producer, factory, adopt, Default::default())
    }
    fn with_factory_adopt_recovery(
        producer: String,
        factory: Factory,
        adopt: bool,
        module_recovery: crate::module_recovery::Gate,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    producer,
                    module_recovery,
                    epoch: 0,
                    identity_generation: 0,
                    module_identity: None,
                    esim_snapshot: Default::default(),
                    device_observation: None,
                    native_sms_identity: None,
                    native_managed: false,
                    native_sms_ready_session: None,
                    sms_observation_generation: 0,
                    session: String::new(),
                    description: String::new(),
                    last_closed: String::new(),
                    port_identity: None,
                    close_failures: Vec::new(),
                    last_close_result: None,
                    port_sequence: 0,
                    reconnect: None,
                    native_module_hold: None,
                    lease: None,
                    lease_high_water: 0,
                    busy: false,
                    busy_lease: String::new(),
                    revoking: false,
                    stopping: false,
                    next_open: None,
                    sms: None,
                    adopt,
                    config_high_water: 0,
                    inhibits: HashMap::new(),
                    inhibit_high_water: 0,
                    radio_inhibit: None,
                    cellular_suppressed: true,
                }),
                adoption_update: Mutex::new(()),
                esim_snapshot_gate: tokio::sync::Mutex::new(()),
                port: Mutex::new(None),
                factory,
                #[cfg(test)]
                reconnect_opener: Mutex::new(None),
                call_capture: Arc::new(Mutex::new(None)),
                changed: Notify::new(),
                #[cfg(test)]
                spawned_io: std::sync::atomic::AtomicU64::new(0),
            }),
            profile_notes: None,
            #[cfg(not(test))]
            hardware_observer: crate::module_discovery::Observer::new(),
            #[cfg(test)]
            hardware_observer: crate::module_discovery::Observer::absent_for_tests(),
        }
    }

    pub(crate) async fn adoption_enabled(&self) -> Result<bool, ()> {
        let owner = self.clone();
        tokio::task::spawn_blocking(move || {
            let _gate = owner
                .inner
                .adoption_update
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            owner
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .adopt
        })
        .await
        .map_err(|_| ())
    }

    /// Current SMS transport mode and interval come from the same serialized
    /// configuration that drives the native receiver. This lets public status
    /// remain correct before the receiver's first hardware snapshot.
    pub(crate) fn sms_status_config(&self) -> Option<(bool, u64)> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.sms.map(|config| (config.serial, config.interval_ms))
    }

    pub(crate) async fn set_adoption(
        &self,
        store: crate::sms_adopt::Store,
        enabled: bool,
    ) -> Result<(), crate::sms_adopt::StoreError> {
        let owner = self.clone();
        tokio::task::spawn_blocking(move || {
            let _gate = owner
                .inner
                .adoption_update
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut state = owner
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.adopt != enabled {
                let Some(next) = state.sms_observation_generation.checked_add(1) else {
                    state.stopping = true;
                    owner.inner.changed.notify_waiters();
                    return Err(crate::sms_adopt::StoreError::Worker);
                };
                state.sms_observation_generation = next;
            }
            state.adopt = enabled;
            if let Some(config) = state.sms.as_mut() {
                config.adopt = enabled;
            }
            owner.inner.changed.notify_waiters();
            drop(state);
            // The legacy Go handler updated the live preference and returned
            // success even when its best-effort file write failed. Keep that
            // API behavior while retaining Rust's atomic, symlink-safe store.
            if store.save(enabled).is_err() {
                crate::diagnostics::write_line(format_args!(
                    "switchd-rs: SMS adoption preference could not be persisted"
                ));
            }
            Ok(())
        })
        .await
        .map_err(|_| crate::sms_adopt::StoreError::Worker)?
    }

    fn status(&self, producer: &str) -> Result<Value, Failure> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check(&state, producer)?;
        Ok(
            json!({"open":!state.session.is_empty(),"session_id":state.session,
            "description":state.description,"busy":state.busy,"leased":state.lease.is_some()}),
        )
    }

    async fn open(&self, input: OpenRequest, stopping: bool) -> Result<Value, Failure> {
        validate_token(&input.producer_id)?;
        validate_optional_token(&input.lease_id)?;
        if stopping {
            let value = self.status(&input.producer_id)?;
            return if value["open"] == true {
                Ok(value)
            } else {
                Err(Failure::stopped())
            };
        }
        let deadline = Instant::now() + MAX_WAIT;
        let permit = self
            .admit(&input.producer_id, None, &input.lease_id, deadline)
            .await?;
        self.run(permit, move |inner, epoch| {
            ensure_open(inner, epoch)?;
            let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check_epoch(&state, epoch)?;
            Ok(json!({"open":true,"session_id":state.session,"description":state.description}))
        })
        .await
    }

    async fn acquire(&self, input: LeaseRequest) -> Result<Value, Failure> {
        self.acquire_with_capture(input, None, None).await
    }

    async fn acquire_with_capture(
        &self,
        input: LeaseRequest,
        captured: Option<&CapturedVoiceTarget>,
        right: Option<&crate::module_recovery::OperationRight>,
    ) -> Result<Value, Failure> {
        validate_token(&input.producer_id)?;
        validate_token(&input.lease_id)?;
        validate_optional_token(&input.session_id)?;
        if input.lease_seq == 0 {
            return Err(Failure::invalid());
        }
        let closing_epoch = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check(&state, &input.producer_id)?;
            if let Some(error) = close_failure_for(&state, &input.producer_id, &input.session_id)
                .or_else(|| {
                    close_failure_for_lease(
                        &state,
                        &input.producer_id,
                        &input.lease_id,
                        input.lease_seq,
                    )
                })
            {
                return Err(error);
            }
            if captured.is_some_and(|captured| !captured.matches(&self.inner, &state)) {
                return Err(Failure::stale());
            }
            if let Some(lease) = &state.lease
                && lease.id == input.lease_id
            {
                if lease.sequence != input.lease_seq
                    || lease.purpose != input.purpose
                    || lease.releasing
                    || (input.session_id != lease.session && input.session_id != state.session)
                {
                    return Err(Failure::stale());
                }
                return Ok(json!({"accepted":true}));
            }
            if input.lease_seq <= state.lease_high_water || input.session_id != state.session {
                return Err(Failure::stale());
            }
            // Never occupy an internal HTTP connection while waiting for another
            // holder. Its command/release must remain able to enter that listener.
            if state.lease.is_some() || state.busy {
                return Err(Failure::busy());
            }
            let authority = match right {
                Some(right) => retained_io_authority(&state, &input.producer_id, right)?,
                None => new_io_authority(&state, &input.producer_id)?,
            };
            // Validate both generation increments before installing a lease.
            // If they are exhausted no owner task receives a returned lease
            // to settle, so failure must leave admission entirely unchanged.
            if input.purpose == Purpose::Voice {
                revoke(&mut state)?;
            }
            let connection = state.port_identity.as_mut().map(|identity| {
                identity.lease_id = input.lease_id.clone();
                identity.lease_sequence = input.lease_seq;
                identity.clone()
            });
            state.lease_high_water = input.lease_seq;
            state.lease = Some(Lease {
                id: input.lease_id,
                sequence: input.lease_seq,
                session: input.session_id,
                purpose: input.purpose,
                releasing: false,
                connection,
                _authority: authority,
            });
            if input.purpose == Purpose::Voice {
                // Voice helpers can re-enumerate the physical USB composition.
                // They must not begin while this process still owns the old
                // claimed handle. Same-token retries see busy until Drop ends.
                Some(state.epoch)
            } else {
                None
            }
        };
        if let Some(epoch) = closing_epoch {
            self.finish_revoke(epoch).await?;
        }
        Ok(json!({"accepted":true}))
    }

    async fn release(&self, input: ReleaseRequest) -> Result<Value, Failure> {
        self.release_with_deadline(input, Some(Instant::now() + MAX_WAIT))
            .await
    }

    async fn release_with_deadline(
        &self,
        input: ReleaseRequest,
        deadline: Option<Instant>,
    ) -> Result<Value, Failure> {
        validate_token(&input.producer_id)?;
        validate_token(&input.lease_id)?;
        validate_optional_token(&input.session_id)?;
        if input.lease_seq == 0 {
            return Err(Failure::invalid());
        }
        loop {
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(error) =
                    close_failure_for(&state, &input.producer_id, &input.session_id).or_else(|| {
                        close_failure_for_lease(
                            &state,
                            &input.producer_id,
                            &input.lease_id,
                            input.lease_seq,
                        )
                    })
                {
                    return Err(error);
                }
                // Producer retirement/shutdown clears the logical lease before
                // its old command and transport Drop finish. Native owners
                // cannot treat that state change as physical completion.
                let draining = deadline.is_none() && (state.busy || state.revoking);
                if state.producer != input.producer_id {
                    if !draining {
                        return Err(Failure::stale());
                    }
                } else if deadline.is_none() && state.stopping {
                    if !draining {
                        return Err(Failure::stopped());
                    }
                } else {
                    // A release can arrive before its acquire. The monotonically
                    // increasing producer-local sequence prevents that late
                    // request from resurrecting a reservation, without tombstones.
                    state.lease_high_water = state.lease_high_water.max(input.lease_seq);
                    let matching = state.lease.as_ref().is_some_and(|lease| {
                        lease.id == input.lease_id && lease.sequence == input.lease_seq
                    });
                    if !matching {
                        if !draining {
                            return Ok(json!({"accepted":true}));
                        }
                    } else {
                        let lease = state.lease.as_ref().unwrap();
                        if input.session_id != lease.session
                            && input.session_id != state.session
                            && input.session_id != state.last_closed
                        {
                            return Err(Failure::stale());
                        }
                        if state.revoking
                            || (state.busy
                                && (deadline.is_none() || state.busy_lease == input.lease_id))
                        {
                            // Cancellation of this wait leaves the intent recorded;
                            // the I/O guard completes the release after real I/O.
                            state.lease.as_mut().unwrap().releasing = true;
                        } else {
                            state.lease = None;
                            self.inner.changed.notify_waiters();
                            return Ok(json!({"accepted":true}));
                        }
                    }
                }
            }
            match deadline {
                Some(deadline) => wait_until(notified, deadline).await?,
                None => notified.await,
            }
        }
    }

    fn configure(&self, input: ConfigureRequest, stopping: bool) -> Result<Value, Failure> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        match input {
            ConfigureRequest::Sms {
                producer_id,
                config_seq,
                serial,
                poll_interval_ms,
            } => {
                validate_token(&producer_id)?;
                check(&state, &producer_id)?;
                if stopping {
                    return Err(Failure::stopped());
                }
                if config_seq == 0 || !(1000..=86_400_000).contains(&poll_interval_ms) {
                    return Err(Failure::invalid());
                }
                let next = SmsConfig {
                    serial,
                    adopt: state.adopt,
                    interval_ms: poll_interval_ms,
                };
                if config_seq < state.config_high_water
                    || (config_seq == state.config_high_water && state.sms != Some(next))
                {
                    return Err(Failure::stale());
                }
                if state.config_high_water != config_seq || state.sms != Some(next) {
                    state.sms_observation_generation = state
                        .sms_observation_generation
                        .checked_add(1)
                        .ok_or_else(Failure::stopped)?;
                }
                state.config_high_water = config_seq;
                state.sms = Some(next);
            }
            ConfigureRequest::Inhibit {
                producer_id,
                inhibit_seq,
                token,
                enabled,
            } => {
                validate_token(&producer_id)?;
                validate_token(&token)?;
                check(&state, &producer_id)?;
                if inhibit_seq == 0 {
                    return Err(Failure::invalid());
                }
                if enabled {
                    if stopping {
                        return Err(Failure::stopped());
                    }
                    if let Some(active_seq) = state.inhibits.get(&token) {
                        // Another token's later cleanup must not invalidate an
                        // already active, exactly matching observation owner.
                        return if *active_seq == inhibit_seq {
                            Ok(json!({"accepted":true}))
                        } else {
                            Err(Failure::stale())
                        };
                    }
                    if inhibit_seq <= state.inhibit_high_water {
                        return Err(Failure::stale());
                    }
                    if state.inhibits.len() >= 64 {
                        return Err(Failure::busy());
                    }
                    state.inhibit_high_water = inhibit_seq;
                    state.inhibits.insert(token, inhibit_seq);
                } else {
                    // Cleanup may arrive before a timed-out enable request.
                    // Fence late enables without retaining unbounded tokens.
                    state.inhibit_high_water = state.inhibit_high_water.max(inhibit_seq);
                    if state.inhibits.get(&token) == Some(&inhibit_seq) {
                        state.inhibits.remove(&token);
                    }
                }
            }
        }
        self.inner.changed.notify_waiters();
        Ok(json!({"accepted":true}))
    }

    async fn command(&self, input: CommandRequest, stopping: bool) -> Result<Value, Failure> {
        validate_token(&input.producer_id)?;
        validate_token(&input.session_id)?;
        validate_optional_token(&input.lease_id)?;
        if input.command.len() > MAX_BYTES
            || input.command.chars().any(char::is_control)
            || !(1..=60_000).contains(&input.timeout_ms)
            || input.max_bytes > MAX_BYTES
        {
            return Err(Failure::invalid());
        }
        if stopping
            && (!shutdown_command_allowed(&input.command) || input.follow_up_base64.is_some())
        {
            return Err(Failure::stopped());
        }
        if crate::module_radio_transport::is_radio_write(&input.command)
            || (self
                .inner
                .call_capture
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
                && crate::module_radio_transport::is_cellular_call_write(&input.command))
        {
            return Err(crate::module_radio_transport::failure(
                "radio_authorization_required",
            ));
        }
        check_cellular_command(&self.inner, &input.command)?;
        let payload = match input.follow_up_base64.as_deref() {
            Some(encoded) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| Failure::invalid())?;
                if bytes.len() < 2
                    || bytes.len() > MAX_BYTES
                    || bytes.last() != Some(&0x1a)
                    || !bytes[..bytes.len() - 1].iter().all(u8::is_ascii_hexdigit)
                {
                    return Err(Failure::invalid());
                }
                Some(bytes)
            }
            None => None,
        };
        let deadline = Instant::now() + Duration::from_millis(input.timeout_ms);
        let permit = self
            .admit(
                &input.producer_id,
                Some(&input.session_id),
                &input.lease_id,
                deadline,
            )
            .await?;
        self.run(permit, move |inner, epoch| {
            let remaining = remaining(deadline)?;
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            check_epoch(
                &inner.state.lock().unwrap_or_else(|e| e.into_inner()),
                epoch,
            )?;
            check_cellular_command(inner, &input.command)?;
            if input.command.starts_with("AT+CSIM=") || input.command.starts_with("AT+CGLA=") {
                inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .esim_snapshot
                    .invalidate(false);
            }
            let result = port.as_mut().ok_or_else(Failure::stale)?.command(
                &input.command,
                payload.as_deref(),
                remaining,
                input.max_bytes,
            );
            if let Err(error) = &result
                && (error.code == "usb_at_disconnected"
                    || (error.code == "usb_at_timeout" && payload.is_none()))
            {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            let response = result.map_err(Failure::from)?;
            // The operation may have completed on a retired handle. Never
            // associate that response with its replacement's session.
            if let Err(mut error) = check_epoch(
                &inner.state.lock().unwrap_or_else(|e| e.into_inner()),
                epoch,
            ) {
                error.uncertain = true;
                return Err(error);
            }
            Ok(json!({"response":response,"session_id":input.session_id}))
        })
        .await
    }

    async fn close(&self, input: CloseRequest) -> Result<Value, Failure> {
        validate_token(&input.producer_id)?;
        validate_token(&input.session_id)?;
        validate_optional_token(&input.lease_id)?;
        let epoch = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.producer != input.producer_id {
                return Err(Failure::stale());
            }
            if let Some(error) = close_failure_for(&state, &input.producer_id, &input.session_id) {
                return Err(error);
            }
            if state.session.is_empty() && state.last_closed == input.session_id && !state.revoking
            {
                return Ok(json!({"accepted":true}));
            }
            if state.session != input.session_id || state.revoking {
                return Err(Failure::stale());
            }
            if state
                .native_module_hold
                .as_ref()
                .is_some_and(|hold| hold.session == input.session_id)
            {
                return Err(Failure::busy());
            }
            match &state.lease {
                Some(lease) if lease.id != input.lease_id || lease.releasing => {
                    return Err(Failure::stale());
                }
                None if !input.lease_id.is_empty() => return Err(Failure::stale()),
                _ => {}
            }
            revoke(&mut state)?;
            state.epoch
        };
        self.inner.changed.notify_waiters();
        self.finish_revoke(epoch).await?;
        Ok(json!({"accepted":true}))
    }

    async fn device_status(
        &self,
        input: DeviceStatusRequest,
        stopping: bool,
    ) -> Result<Value, Failure> {
        if stopping {
            return Err(Failure::stopped());
        }
        validate_token(&input.producer_id)?;
        validate_token(&input.session_id)?;
        validate_token(&input.lease_id)?;
        let deadline = Instant::now() + DEVICE_STATUS_BUDGET;
        let permit = self
            .admit(
                &input.producer_id,
                Some(&input.session_id),
                &input.lease_id,
                deadline,
            )
            .await?;
        let profile_notes = self.profile_notes.clone();
        let mut response = self
            .run(permit, move |inner, epoch| {
                check_device_status_fence(inner, epoch, &input.session_id, &input.lease_id)?;
                let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
                let mut transport_failed = false;
                let suppressed = input.suppress_cellular
                    || inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .cellular_suppressed;
                let result =
                    crate::usb_at_status::collect(suppressed, |command, timeout, maximum| {
                        check_device_status_fence(
                            inner,
                            epoch,
                            &input.session_id,
                            &input.lease_id,
                        )?;
                        check_cellular_command(inner, command)?;
                        let timeout = remaining(deadline)?
                            .min(timeout)
                            .min(Duration::from_secs(4));
                        let result = port.as_mut().ok_or_else(Failure::stale)?.command(
                            command,
                            None,
                            timeout,
                            maximum.min(MAX_BYTES),
                        );
                        // Every Err here is a failed physical transaction rather
                        // than a modem's textual ERROR reply. Stop the collection
                        // and retire that stream before another operation uses it.
                        transport_failed |= result.is_err();
                        check_device_status_fence(
                            inner,
                            epoch,
                            &input.session_id,
                            &input.lease_id,
                        )?;
                        result.map_err(Failure::from)
                    });
                if transport_failed {
                    let _ = native_module::close_port(inner, &mut port);
                    detached(inner, epoch);
                }
                let status = result?;
                if !suppressed
                    && inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .cellular_suppressed
                {
                    return Err(crate::module_radio_transport::failure(
                        "module_flight_mode_blocks_cellular_probe",
                    ));
                }
                check_device_status_fence(inner, epoch, &input.session_id, &input.lease_id)?;
                remaining(deadline)?;
                Ok(json!({"session_id":input.session_id,"status":status}))
            })
            .await?;
        if let Some(status) = response.get_mut("status").and_then(Value::as_object_mut) {
            let phone = status
                .get("phone_number")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if crate::profile_notes::is_missing_phone(phone) {
                let iccid = status
                    .get("iccid")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let fallback = profile_notes
                    .as_ref()
                    .map(|store| store.phone_for_iccid_or_empty(iccid))
                    .unwrap_or_default();
                Self::apply_saved_phone_fallback(status, &fallback);
            }
        }
        Ok(response)
    }

    fn apply_saved_phone_fallback(status: &mut serde_json::Map<String, Value>, fallback: &str) {
        let current = status
            .get("phone_number")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if crate::profile_notes::is_missing_phone(current) {
            status.insert("phone_number".into(), json!(fallback.trim()));
        }
    }

    /// A complete manual storage operation uses an already acquired, fixed SIM
    /// lease. It never opens a handle, retries a delete, or releases ownership
    /// when its HTTP waiter disappears. Physical results remain typed even when
    /// a later step fails, so callers cannot mistake partial deletion for no-op.
    pub(crate) async fn storage(
        &self,
        producer: String,
        session: String,
        lease_id: String,
        operation: crate::usb_at_storage::StorageOperation,
    ) -> Result<crate::usb_at_storage::Outcome, Failure> {
        self.storage_observed(producer, session, lease_id, operation, None)
            .await
    }

    pub(crate) async fn storage_observed(
        &self,
        producer: String,
        session: String,
        lease_id: String,
        operation: crate::usb_at_storage::StorageOperation,
        observation: Option<SmsObservationToken>,
    ) -> Result<crate::usb_at_storage::Outcome, Failure> {
        validate_token(&producer)?;
        validate_token(&session)?;
        validate_token(&lease_id)?;
        operation.validate()?;
        let deadline = Instant::now() + SMS_STORAGE_BUDGET;
        let active_deadline = deadline - SMS_STORAGE_CLEANUP_RESERVE;
        let permit = self
            .admit(&producer, Some(&session), &lease_id, active_deadline)
            .await?;
        self.run(permit, move |inner, epoch| {
            use crate::usb_at_storage::Phase;
            sms_observation::check_active(inner, observation.as_ref(), Phase::Active)?;
            check_storage_fence(inner, epoch, &producer, &session, &lease_id, Phase::Active)?;
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let mut transport_failed = false;
            let mut execution =
                crate::usb_at_storage::execute(&operation, |phase, command, budget, maximum| {
                    check_storage_fence(inner, epoch, &producer, &session, &lease_id, phase)?;
                    sms_observation::check_active(inner, observation.as_ref(), phase)?;
                    if transport_failed {
                        return Err(Failure::new(
                            StatusCode::BAD_GATEWAY,
                            "usb_sms_storage_transport_unavailable",
                            "USB SMS storage transport is unavailable",
                        ));
                    }
                    let timeout = remaining(if phase == Phase::Cleanup {
                        deadline
                    } else {
                        active_deadline
                    })?
                    .min(budget);
                    let result = port.as_mut().ok_or_else(Failure::stale)?.command(
                        command,
                        None,
                        timeout,
                        maximum.min(MAX_BYTES),
                    );
                    transport_failed |= result.is_err();
                    let response = result.map_err(Failure::from)?;
                    if let Err(mut error) =
                        check_storage_fence(inner, epoch, &producer, &session, &lease_id, phase)
                            .and_then(|()| {
                                sms_observation::check_active(inner, observation.as_ref(), phase)
                            })
                    {
                        // This command was already submitted. An ownership change
                        // cannot turn a potentially completed delete into a no-op.
                        error.uncertain = command.starts_with("AT+CMGD=");
                        return Err(error);
                    }
                    Ok(response)
                });
            if transport_failed || execution.retire_transport {
                // A failed read/write or unconfirmed restore invalidates this
                // stream. Wait for its actual Drop before publishing outcome.
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
                execution.outcome.complete = false;
                execution.outcome.storage_restored = false;
            } else if let Err(error) =
                check_storage_fence(inner, epoch, &producer, &session, &lease_id, Phase::Active)
            {
                execution.outcome.complete = false;
                if check_storage_fence(inner, epoch, &producer, &session, &lease_id, Phase::Cleanup)
                    .is_err()
                {
                    execution.outcome.storage_restored = false;
                }
                if execution.outcome.error_code.is_empty() {
                    execution.outcome.error_code = error.code.into();
                }
            }
            Ok(execution.outcome)
        })
        .await
    }

    /// Read modem SIM/ME storage usage as one bounded owner transaction. This
    /// public read does not borrow a Go lease: admission is atomic with the
    /// same USB owner and refuses any active SIM lease, RF inhibit, or I/O.
    pub(crate) async fn storage_usage(
        &self,
        producer: &str,
    ) -> Result<crate::usb_at_storage::Outcome, Failure> {
        self.storage_usage_observed(producer, None)
            .await
            .map(|(outcome, _)| outcome)
    }

    pub(crate) async fn storage_usage_observed(
        &self,
        producer: &str,
        binding: Option<SmsPollBinding>,
    ) -> Result<(crate::usb_at_storage::Outcome, Option<SmsObservationToken>), Failure> {
        validate_token(producer)?;
        let permit = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check(&state, producer)?;
            if let Some(binding) = &binding {
                binding.check(&state)?;
            }
            if state.busy
                || state.lease.is_some()
                || !state.inhibits.is_empty()
                || state.radio_inhibit.is_some()
            {
                return Err(Failure::busy());
            }
            let authority = new_io_authority(&state, producer)?;
            state.busy = true;
            state.busy_lease.clear();
            IoPermit {
                inner: self.inner.clone(),
                epoch: state.epoch,
                _authority: authority,
            }
        };
        let producer = producer.to_owned();
        self.run(permit, move |inner, epoch| {
            ensure_open(inner, epoch)?;
            let session = inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .session
                .clone();
            let observation = binding
                .as_ref()
                .map(|binding| sms_observation::capture(inner, binding))
                .transpose()?;
            let deadline = Instant::now() + SMS_STORAGE_BUDGET;
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let mut transport_failed = false;
            let execution = crate::usb_at_storage::execute(
                &crate::usb_at_storage::StorageOperation::Usage,
                |phase, command, budget, maximum| {
                    check_unleased_storage_fence(inner, epoch, &producer, &session, phase)?;
                    sms_observation::check_active(inner, observation.as_ref(), phase)?;
                    if transport_failed {
                        return Err(Failure::new(
                            StatusCode::BAD_GATEWAY,
                            "usb_sms_storage_transport_unavailable",
                            "USB SMS storage transport is unavailable",
                        ));
                    }
                    let timeout = remaining(deadline)?.min(budget);
                    let result = port.as_mut().ok_or_else(Failure::stale)?.command(
                        command,
                        None,
                        timeout,
                        maximum.min(MAX_BYTES),
                    );
                    transport_failed |= result.is_err();
                    let response = result.map_err(Failure::from)?;
                    check_unleased_storage_fence(inner, epoch, &producer, &session, phase)?;
                    sms_observation::check_active(inner, observation.as_ref(), phase)?;
                    Ok(response)
                },
            );
            if transport_failed || execution.retire_transport {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            Ok((execution.outcome, observation))
        })
        .await
    }

    /// Submit PDU-mode SMS through the already-bound USB AT SIM lease. The
    /// caller cannot select a new card or retry an uncertain segment; the
    /// expected ICCID is checked on this same transport before CMGF/CMGS.
    async fn submit_sms(
        &self,
        input: SmsSubmitRequest,
        stopping: bool,
    ) -> Result<SmsSubmitOutcome, Failure> {
        validate_token(&input.producer_id)?;
        validate_token(&input.session_id)?;
        validate_token(&input.lease_id)?;
        if stopping {
            return Err(Failure::stopped());
        }
        let recipient = input.recipient.trim().to_owned();
        let message = input.message.clone();
        let expected_iccid = input.expected_iccid.trim().to_owned();
        if recipient.is_empty()
            || recipient.len() > 128
            || message.trim().is_empty()
            || message.len() > 64 * 1024
            || !(10..=20).contains(&expected_iccid.len())
            || !expected_iccid.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(Failure::invalid());
        }
        let mut reference = [0_u8; 1];
        getrandom::fill(&mut reference).map_err(|_| {
            Failure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "usb_sms_submit_entropy_unavailable",
                "短信提交暂不可用",
            )
        })?;
        let tpdus = proletarius_sms::submit::prepare_with_policy(
            &recipient,
            &message,
            reference[0],
            proletarius_sms::submit::EncodingPolicy::Modem,
            false,
        )
        .map_err(|_| Failure::invalid())?;
        if tpdus.is_empty()
            || tpdus.len() > 255
            || tpdus
                .iter()
                .any(|tpdu| !(8..=232).contains(&tpdu.len()) || tpdu[0] & 0x03 != 0x01)
        {
            return Err(Failure::invalid());
        }
        let producer = input.producer_id;
        let session = input.session_id;
        let lease_id = input.lease_id;
        let deadline = Instant::now() + Duration::from_secs(180);
        let permit = self
            .admit(&producer, Some(&session), &lease_id, deadline)
            .await?;
        self.run(permit, move |inner, epoch| {
            use crate::usb_at_storage::Phase;
            let mut port = inner.port.lock().unwrap_or_else(|error| error.into_inner());
            let mut transport_failed = false;
            let mut run = |command: &str,
                           follow_up: Option<&[u8]>,
                           timeout: Duration|
             -> Result<String, Failure> {
                check_storage_fence(inner, epoch, &producer, &session, &lease_id, Phase::Active)?;
                let result = port.as_mut().ok_or_else(Failure::stale)?.command(
                    command,
                    follow_up,
                    remaining(deadline)?.min(timeout),
                    MAX_STORAGE_BYTES,
                );
                transport_failed |= result.is_err();
                let response = result.map_err(Failure::from)?;
                check_storage_fence(inner, epoch, &producer, &session, &lease_id, Phase::Active)?;
                Ok(response)
            };

            let identity = match run("AT+QCCID", None, Duration::from_secs(3)) {
                Ok(response) => crate::usb_at_storage::card_identity(&response).ok_or_else(|| {
                    Failure::new(
                        StatusCode::BAD_GATEWAY,
                        "usb_sms_identity_unconfirmed",
                        "当前 SIM 身份无法确认",
                    )
                }),
                Err(error) => Err(error),
            };
            if !identity
                .as_ref()
                .is_ok_and(|observed| observed == &expected_iccid)
            {
                let failure = match identity {
                    Err(error) => error,
                    Ok(_) => Failure::new(
                        StatusCode::CONFLICT,
                        "usb_sms_card_changed",
                        "短信目标卡已变化，请重新确认发送通道",
                    ),
                };
                drop(run);
                if transport_failed {
                    let _ = native_module::close_port(inner, &mut port);
                    detached(inner, epoch);
                }
                return Err(failure);
            }

            let pdu_mode = run("AT+CMGF=0", None, Duration::from_secs(5));
            if !pdu_mode
                .as_ref()
                .is_ok_and(|response| complete_ok(response))
            {
                let failure = pdu_mode.err().unwrap_or_else(|| {
                    Failure::new(
                        StatusCode::BAD_GATEWAY,
                        "usb_sms_pdu_mode_unconfirmed",
                        "短信 PDU 模式未确认",
                    )
                });
                drop(run);
                if transport_failed {
                    let _ = native_module::close_port(inner, &mut port);
                    detached(inner, epoch);
                }
                return Err(failure);
            }

            let mut accepted_segments = 0;
            let mut complete = true;
            for (index, tpdu) in tpdus.iter().enumerate() {
                if let Err(error) =
                    check_storage_fence(inner, epoch, &producer, &session, &lease_id, Phase::Active)
                {
                    if accepted_segments == 0 {
                        if transport_failed {
                            let _ = native_module::close_port(inner, &mut port);
                            detached(inner, epoch);
                        }
                        return Err(error);
                    }
                    complete = false;
                    break;
                }
                let mut encoded = String::with_capacity(tpdu.len() * 2 + 2);
                encoded.push_str("00");
                for byte in tpdu {
                    use std::fmt::Write as _;
                    let _ = write!(&mut encoded, "{byte:02X}");
                }
                let mut payload = encoded.into_bytes();
                payload.push(0x1a);
                let command = format!("AT+CMGS={}", tpdu.len());
                let response = run(&command, Some(&payload), Duration::from_secs(45));
                match response {
                    Ok(response) if complete_ok(&response) && response.contains("+CMGS:") => {
                        accepted_segments += 1;
                        if index + 1 < tpdus.len() {
                            std::thread::sleep(Duration::from_millis(500));
                        }
                    }
                    _ => {
                        complete = false;
                        break;
                    }
                }
            }
            drop(run);
            if transport_failed {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            Ok(SmsSubmitOutcome {
                accepted_segments,
                complete: complete && accepted_segments == tpdus.len(),
            })
        })
        .await
    }

    /// Submit an SMS using the currently admitted SIM lease. The public
    /// device-SMS owner uses this after its durable claim, so a plain USB AT
    /// route does not have to round-trip through Go merely to recover the
    /// lease and card identity that this owner already holds.
    pub(crate) async fn submit_active_sms(
        &self,
        recipient: String,
        message: String,
        expected_generation: u64,
        stopping: bool,
    ) -> Result<SmsSubmitOutcome, Failure> {
        let (producer_id, session_id, lease_id, expected_iccid) = {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(lease) = state.lease.as_ref().filter(|lease| {
                lease.purpose == Purpose::Sim && !lease.releasing && lease.session == state.session
            }) else {
                return Err(Failure::stale());
            };
            let Some(identity) = state.module_identity.as_ref().filter(|identity| {
                identity.generation == state.identity_generation
                    && state.identity_generation == expected_generation
            }) else {
                return Err(Failure::new(
                    StatusCode::CONFLICT,
                    "usb_sms_identity_unconfirmed",
                    "当前 SIM 身份无法确认",
                ));
            };
            (
                state.producer.clone(),
                state.session.clone(),
                lease.id.clone(),
                identity.iccid.to_string(),
            )
        };
        self.submit_sms(
            SmsSubmitRequest {
                producer_id,
                session_id,
                lease_id,
                recipient,
                message,
                expected_iccid,
            },
            stopping,
        )
        .await
    }

    /// Bind a captured public SMS route to the current Rust-owned card
    /// generation without persisting the ICCID in the send ledger.
    pub(crate) fn active_sim_identity_generation(&self, expected_iccid: &str) -> Option<u64> {
        self.active_sim_identity_binding(expected_iccid)
            .map(|(_, generation)| generation)
    }

    /// Return an opaque owner session plus identity generation for a bound
    /// active SIM route. The session prevents a persisted send intent from
    /// becoming valid again after daemon restart or transport replacement.
    pub(crate) fn active_sim_identity_binding(
        &self,
        expected_iccid: &str,
    ) -> Option<(String, u64)> {
        self.active_sim_identity_binding_matching(Some(expected_iccid))
    }

    /// Return the current physical USB session and identity generation for an
    /// already-admitted SIM lease. The later submit still confirms ICCID on
    /// the same transport before sending; this helper avoids asking the Go
    /// route bridge to rediscover a capability already owned here.
    pub(crate) fn active_sim_identity_binding_current(&self) -> Option<(String, u64)> {
        self.active_sim_identity_binding_matching(None)
    }

    fn active_sim_identity_binding_matching(
        &self,
        expected_iccid: Option<&str>,
    ) -> Option<(String, u64)> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.stopping || state.revoking || state.cellular_suppressed {
            return None;
        }
        if !state.lease.as_ref().is_some_and(|lease| {
            lease.purpose == Purpose::Sim && !lease.releasing && lease.session == state.session
        }) {
            return None;
        }
        let identity = state.module_identity.as_ref().filter(|identity| {
            identity.generation == state.identity_generation
                && expected_iccid.is_none_or(|expected| identity.iccid.as_str() == expected.trim())
        })?;
        Some((state.session.clone(), identity.generation))
    }

    pub(crate) fn has_active_sim_identity_binding(
        &self,
        expected_session: &str,
        expected_generation: u64,
    ) -> bool {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        !state.stopping
            && !state.revoking
            && !state.cellular_suppressed
            && state.session == expected_session
            && state.identity_generation == expected_generation
            && state.module_identity.as_ref().is_some_and(|identity| {
                identity.generation == expected_generation
                    && state.lease.as_ref().is_some_and(|lease| {
                        lease.purpose == Purpose::Sim
                            && !lease.releasing
                            && lease.session == state.session
                    })
            })
    }

    pub(crate) fn active_sim_identity_session(&self, expected_generation: u64) -> Option<String> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (!state.stopping
            && !state.revoking
            && !state.cellular_suppressed
            && state.identity_generation == expected_generation
            && state.module_identity.as_ref().is_some_and(|identity| {
                identity.generation == expected_generation
                    && state.lease.as_ref().is_some_and(|lease| {
                        lease.purpose == Purpose::Sim
                            && !lease.releasing
                            && lease.session == state.session
                    })
            }))
        .then(|| state.session.clone())
    }

    /// Execute a Rust-admitted SIM/ME storage mutation without borrowing a
    /// Go-created lease. Admission and the entire physical transaction remain
    /// inside this owner; any existing SIM/voice lease or RF inhibit wins.
    pub(crate) async fn storage_exclusive(
        &self,
        producer: &str,
        expected_session: &str,
        operation: crate::usb_at_storage::StorageOperation,
    ) -> Result<crate::usb_at_storage::Outcome, Failure> {
        self.storage_exclusive_observed(producer, expected_session, operation, None)
            .await
    }

    pub(crate) async fn storage_exclusive_observed(
        &self,
        producer: &str,
        expected_session: &str,
        operation: crate::usb_at_storage::StorageOperation,
        observation: Option<SmsObservationToken>,
    ) -> Result<crate::usb_at_storage::Outcome, Failure> {
        validate_token(producer)?;
        validate_token(expected_session)?;
        operation.validate()?;
        let permit = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check(&state, producer)?;
            if state.session != expected_session {
                return Err(Failure::stale());
            }
            if state.busy
                || state.lease.is_some()
                || state.revoking
                || !state.inhibits.is_empty()
                || state.radio_inhibit.is_some()
            {
                return Err(Failure::busy());
            }
            let authority = new_io_authority(&state, producer)?;
            state.busy = true;
            state.busy_lease.clear();
            IoPermit {
                inner: self.inner.clone(),
                epoch: state.epoch,
                _authority: authority,
            }
        };
        let producer = producer.to_owned();
        let session = expected_session.to_owned();
        self.run(permit, move |inner, epoch| {
            use crate::usb_at_storage::Phase;
            sms_observation::check_active(inner, observation.as_ref(), Phase::Active)?;
            let deadline = Instant::now() + SMS_STORAGE_BUDGET;
            let active_deadline = deadline - SMS_STORAGE_CLEANUP_RESERVE;
            ensure_open(inner, epoch)?;
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let mut transport_failed = false;
            let mut execution =
                crate::usb_at_storage::execute(&operation, |phase, command, budget, maximum| {
                    check_unleased_storage_fence(inner, epoch, &producer, &session, phase)?;
                    sms_observation::check_active(inner, observation.as_ref(), phase)?;
                    if transport_failed {
                        return Err(Failure::new(
                            StatusCode::BAD_GATEWAY,
                            "usb_sms_storage_transport_unavailable",
                            "USB SMS storage transport is unavailable",
                        ));
                    }
                    let timeout = remaining(if phase == Phase::Cleanup {
                        deadline
                    } else {
                        active_deadline
                    })?
                    .min(budget);
                    let result = port.as_mut().ok_or_else(Failure::stale)?.command(
                        command,
                        None,
                        timeout,
                        maximum.min(MAX_BYTES),
                    );
                    transport_failed |= result.is_err();
                    let response = result.map_err(Failure::from)?;
                    if let Err(mut error) =
                        check_unleased_storage_fence(inner, epoch, &producer, &session, phase)
                            .and_then(|()| {
                                sms_observation::check_active(inner, observation.as_ref(), phase)
                            })
                    {
                        error.uncertain = command.starts_with("AT+CMGD=");
                        return Err(error);
                    }
                    Ok(response)
                });
            if transport_failed || execution.retire_transport {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
                execution.outcome.complete = false;
                execution.outcome.storage_restored = false;
            }
            Ok(execution.outcome)
        })
        .await
    }

    pub(crate) fn set_cellular_suppressed(&self, suppressed: bool) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cellular_suppressed = suppressed;
    }

    /// Retire the old stream and discover within the real USB owner worker.
    pub(crate) async fn call_mode_radio_reconnect(
        &self,
        producer: &str,
        old_session: &str,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<String, Failure> {
        self.call_mode_radio_reconnect_authorized(producer, old_session, deadline, cancelled, None)
            .await
    }
    pub(crate) async fn call_mode_radio_reconnect_with_right(
        &self,
        producer: &str,
        old_session: &str,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        right: crate::module_recovery::OperationRight,
    ) -> Result<String, Failure> {
        self.call_mode_radio_reconnect_authorized(
            producer,
            old_session,
            deadline,
            cancelled,
            Some(right),
        )
        .await
    }
    async fn call_mode_radio_reconnect_authorized(
        &self,
        producer: &str,
        old_session: &str,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        right: Option<crate::module_recovery::OperationRight>,
    ) -> Result<String, Failure> {
        validate_token(producer)?;
        let owner = self.clone();
        let producer = producer.to_owned();
        let old_session = old_session.to_owned();
        if cancelled.load(std::sync::atomic::Ordering::Acquire) || Instant::now() >= deadline {
            return Err(crate::module_radio_transport::failure(
                "radio_owner_revoked",
            ));
        }
        let permit = owner
            .admit_with_right(
                &producer,
                None,
                "",
                deadline.min(Instant::now() + MAX_WAIT),
                right.as_ref(),
            )
            .await?;
        owner
            .run(permit, move |inner, epoch| {
                if cancelled.load(std::sync::atomic::Ordering::Acquire)
                    || Instant::now() >= deadline
                {
                    return Err(crate::module_radio_transport::failure(
                        "radio_owner_revoked",
                    ));
                }
                let current = {
                    let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    check(&state, &producer)?;
                    check_epoch(&state, epoch)?;
                    state.session.clone()
                };
                if !current.is_empty() && current == old_session {
                    let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = native_module::close_port(inner, &mut port);
                    detached(inner, epoch);
                }
                ensure_open(inner, epoch)?;
                if cancelled.load(std::sync::atomic::Ordering::Acquire)
                    || Instant::now() >= deadline
                {
                    let now_session = inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .session
                        .clone();
                    // Close only the candidate opened by this worker. A newer
                    // stream adopted from another owner is never closed here.
                    if now_session != current && (current.is_empty() || current == old_session) {
                        let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
                        let _ = native_module::close_port(inner, &mut port);
                        detached(inner, epoch);
                    }
                    return Err(crate::module_radio_transport::failure(
                        "radio_owner_revoked",
                    ));
                }
                let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                check_epoch(&state, epoch)?;
                check(&state, &producer)?;
                Ok(state.session.clone())
            })
            .await
    }

    pub(crate) fn validate_radio_binding(
        &self,
        producer: &str,
        session: &str,
    ) -> Result<(), Failure> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check(&state, producer)?;
        if session.is_empty() || state.session != session {
            return Err(Failure::stale());
        }
        Ok(())
    }

    /// Only the native policy service mutates this synchronous intent. Unlike
    /// transport leases it leaves AKA commands available for the online tunnel.
    pub(crate) fn set_radio_inhibit(
        &self,
        producer: &str,
        token: &str,
        enabled: bool,
    ) -> Result<(), Failure> {
        validate_token(token)?;
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check(&state, producer)?;
        if enabled {
            if state
                .native_module_hold
                .as_ref()
                .is_some_and(|hold| hold.token != token)
            {
                return Err(Failure::busy());
            }
            if state
                .radio_inhibit
                .as_deref()
                .is_some_and(|active| active != token)
            {
                return Err(Failure::busy());
            }
            state.radio_inhibit = Some(token.into());
        } else if state.radio_inhibit.as_deref() == Some(token) {
            state.radio_inhibit = None;
            state.cellular_suppressed = true;
        }
        self.inner.changed.notify_waiters();
        Ok(())
    }

    /// Entire RF plans own one actual handle until all readbacks finish, even
    /// if their asynchronous waiter is canceled. No step opens or replays it.
    #[cfg(test)]
    pub(crate) async fn radio_execute(
        &self,
        producer: String,
        session: String,
        action: crate::module_radio_transport::Action,
        permitted_inhibit: Option<String>,
    ) -> Result<crate::module_radio_transport::Outcome, Failure> {
        self.radio_execute_with_budget(
            producer,
            session,
            action,
            permitted_inhibit,
            crate::module_radio_transport::PLAN_BUDGET,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
    }
    pub(crate) async fn radio_execute_with_budget(
        &self,
        producer: String,
        session: String,
        action: crate::module_radio_transport::Action,
        permitted_inhibit: Option<String>,
        budget: Duration,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<crate::module_radio_transport::Outcome, Failure> {
        self.radio_execute_authorized(
            producer,
            session,
            action,
            permitted_inhibit,
            budget,
            cancelled,
            None,
            None,
        )
        .await
    }
    pub(crate) async fn radio_execute_with_budget_and_right(
        &self,
        producer: String,
        session: String,
        action: crate::module_radio_transport::Action,
        permitted_inhibit: Option<String>,
        budget: Duration,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        right: crate::module_recovery::OperationRight,
        commit: Option<crate::module_radio::CallModeCommitProof>,
    ) -> Result<crate::module_radio_transport::Outcome, Failure> {
        self.radio_execute_authorized(
            producer,
            session,
            action,
            permitted_inhibit,
            budget,
            cancelled,
            Some(right),
            commit,
        )
        .await
    }
    async fn radio_execute_authorized(
        &self,
        producer: String,
        session: String,
        action: crate::module_radio_transport::Action,
        permitted_inhibit: Option<String>,
        budget: Duration,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        right: Option<crate::module_recovery::OperationRight>,
        commit: Option<crate::module_radio::CallModeCommitProof>,
    ) -> Result<crate::module_radio_transport::Outcome, Failure> {
        use crate::module_radio_transport::{PLAN_BUDGET, Plan, RESPONSE_LIMIT, Reply};
        validate_token(&producer)?;
        validate_token(&session)?;
        action.validate()?;
        if let Some(proof) = &commit {
            proof
                .validate_for_usb_owner(self)
                .map_err(|error| Failure {
                    status: error.status,
                    code: error.code,
                    message: error.message,
                    response: String::new(),
                    uncertain: false,
                })?;
        }
        if budget.is_zero() {
            return Err(crate::module_radio_transport::failure("radio_plan_timeout"));
        }
        let deadline = Instant::now() + budget.min(PLAN_BUDGET);
        let permit = self
            .admit_with_right(&producer, Some(&session), "", deadline, right.as_ref())
            .await?;
        let fence_owner = self.inner.clone();
        self.run(permit, move |inner, epoch| {
            if let Some(proof) = &commit {
                let gate = inner
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .module_recovery
                    .clone();
                proof.validate_for_gate(&gate).map_err(|error| Failure {
                    status: error.status,
                    code: error.code,
                    message: error.message,
                    response: String::new(),
                    uncertain: false,
                })?;
            }
            check_radio_fence(
                inner,
                epoch,
                &producer,
                &session,
                permitted_inhibit.as_deref(),
            )?;
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let mut plan = Plan::new_with_cancelled(
                action,
                deadline.saturating_duration_since(Instant::now()),
                cancelled,
            );
            plan.set_call_mode_commit_proof(commit);
            while !plan.done() {
                if let Err(error) = check_radio_fence(
                    inner,
                    epoch,
                    &producer,
                    &session,
                    permitted_inhibit.as_deref(),
                ) {
                    plan.fail(error.code, plan.unconfirmed_write());
                    break;
                }
                let Some(mut step) = plan.next() else {
                    if !plan.done() {
                        std::thread::sleep(
                            plan.ready_at()
                                .saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(25)),
                        );
                    }
                    continue;
                };
                let binding = crate::module_radio_transport::Binding::Usb {
                    producer_id: producer.clone(),
                    session_id: session.clone(),
                };
                if step.write {
                    if let Err(code) = plan.prepare_commit_before_write(&step, &binding) {
                        if step.sensitive {
                            step.command.zeroize();
                        }
                        plan.fail(code, plan.unconfirmed_write());
                        break;
                    }
                }
                let transport = port.as_mut().ok_or_else(Failure::stale)?;
                let plan_guard = plan.admission_guard_for_step(&step, binding);
                let fence_owner = fence_owner.clone();
                let fence_producer = producer.clone();
                let fence_session = session.clone();
                let fence_inhibit = permitted_inhibit.clone();
                if step.write {
                    plan.note_write_started();
                }
                let mut result = transport.command_with_guard(
                    &step.command,
                    None,
                    step.timeout,
                    RESPONSE_LIMIT,
                    Arc::new(move || {
                        plan_guard()
                            && check_radio_fence(
                                &fence_owner,
                                epoch,
                                &fence_producer,
                                &fence_session,
                                fence_inhibit.as_deref(),
                            )
                            .is_ok()
                    }),
                );
                if step.sensitive {
                    step.command.zeroize();
                }
                if step.write {
                    if let Err(error) = &mut result {
                        if !error.uncertain {
                            if step.sensitive {
                                error.response.zeroize();
                            }
                            // No bytes from this step were submitted. Earlier
                            // accepted writes still require reconciliation.
                            let code = plan
                                .record_write_not_submitted()
                                .err()
                                .unwrap_or(error.code);
                            plan.fail(code, plan.unconfirmed_write());
                            break;
                        }
                    }
                    plan.mark_submitted();
                }
                let reply = match result {
                    Ok(text) => Reply::response(text),
                    Err(mut error) => {
                        if step.sensitive {
                            error.response.zeroize();
                        }
                        Reply::error(&Failure::from(error))
                    }
                };
                plan.accept(reply);
                if let Err(error) = check_radio_fence(
                    inner,
                    epoch,
                    &producer,
                    &session,
                    permitted_inhibit.as_deref(),
                ) {
                    plan.fail(error.code, step.write || plan.unconfirmed_write());
                    break;
                }
            }
            if plan.retire_transport() {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            Ok(plan.finish())
        })
        .await
    }

    /// Producer fencing is synchronous before the first await. Cleanup itself
    /// belongs to a detached task, so canceling a supervisor waiter cannot
    /// leave a stale transport eligible for reuse.
    pub(crate) async fn replace_producer(&self, producer: String) -> Result<(), Failure> {
        validate_token(&producer)?;
        let epoch = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopping {
                return Err(Failure::stopped());
            }
            revoke(&mut state)?;
            state.producer = producer;
            state.lease = None;
            state.lease_high_water = 0;
            state.inhibits.clear();
            state.radio_inhibit = None;
            state.inhibit_high_water = 0;
            state.sms = None;
            state.config_high_water = 0;
            state.next_open = None;
            state.epoch
        };
        self.inner.changed.notify_waiters();
        self.finish_revoke(epoch).await
    }

    pub(crate) async fn shutdown(&self) -> Result<(), Failure> {
        if self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .module_recovery
            .is_quarantined()
        {
            // A startup-quarantined owner has never admitted I/O. Prove that
            // locally before draining; do not infer historical module recovery.
            let port = self.inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if port.is_some()
                || state.busy
                || state.native_managed
                || !state.session.is_empty()
                || state.lease.is_some()
                || state.native_module_hold.is_some()
                || state.revoking
                || !state.close_failures.is_empty()
            {
                return Err(Failure::new(
                    StatusCode::CONFLICT,
                    "module_quarantine_release_unconfirmed",
                    "隔离模块存在未排空的本次物理工作",
                ));
            }
            state.stopping = true;
            drop(state);
            drop(port);
            self.inner.changed.notify_waiters();
            return Ok(());
        }
        let epoch = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            revoke(&mut state)?;
            state.stopping = true;
            state.lease = None;
            state.epoch
        };
        self.inner.changed.notify_waiters();
        self.finish_revoke(epoch).await?;
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, error)) = state.close_failures.first() {
            return Err(error.clone());
        }
        Ok(())
    }

    async fn finish_revoke(&self, epoch: u64) -> Result<(), Failure> {
        let owner = self.clone();
        tokio::spawn(async move {
            loop {
                let notified = owner.inner.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let permit = {
                    let mut state = owner.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.epoch != epoch {
                        return Err(Failure::stale());
                    }
                    if !state.busy {
                        state.busy = true;
                        state.busy_lease = state
                            .lease
                            .as_ref()
                            .map_or_else(String::new, |lease| lease.id.clone());
                        Some(IoPermit {
                            inner: owner.inner.clone(),
                            epoch,
                            _authority: IoAuthority::Drain,
                        })
                    } else {
                        None
                    }
                };
                if let Some(permit) = permit {
                    return owner
                        .run(permit, move |inner, expected| {
                            let closed = native_module::close_port(
                                inner,
                                &mut inner.port.lock().unwrap_or_else(|e| e.into_inner()),
                            );
                            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                            if state.epoch == expected {
                                state.revoking = false;
                            }
                            closed
                        })
                        .await;
                }
                notified.await;
            }
        })
        .await
        .map_err(|_| Failure::stopped())?
    }

    async fn admit(
        &self,
        producer: &str,
        session: Option<&str>,
        lease_id: &str,
        deadline: Instant,
    ) -> Result<IoPermit, Failure> {
        self.admit_with_right(producer, session, lease_id, deadline, None)
            .await
    }
    async fn admit_with_right(
        &self,
        producer: &str,
        session: Option<&str>,
        lease_id: &str,
        deadline: Instant,
        right: Option<&crate::module_recovery::OperationRight>,
    ) -> Result<IoPermit, Failure> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check(&state, producer)?;
        if let Some(session) = session
            && (session.is_empty() || state.session != session)
        {
            return Err(Failure::stale());
        }
        let allowed = match &state.lease {
            Some(lease) => {
                lease.id == lease_id && lease.purpose == Purpose::Sim && !lease.releasing
            }
            None => {
                if !lease_id.is_empty() {
                    return Err(Failure::stale());
                }
                true
            }
        };
        if !allowed || state.busy {
            return Err(Failure::busy());
        }
        let authority = if let Some(right) = right {
            retained_io_authority(&state, producer, right)?
        } else {
            // A compatibility SIM lease can outlive many HTTP requests. It
            // is not permission for new commands after live revocation.
            // Native finite transactions keep their original IoPermit; only
            // typed settlement paths may provide a retained operation right.
            new_io_authority(&state, producer)?
        };
        remaining(deadline)?;
        state.busy = true;
        state.busy_lease = lease_id.into();
        Ok(IoPermit {
            inner: self.inner.clone(),
            epoch: state.epoch,
            _authority: authority,
        })
    }

    /// Queue SIM identity transactions behind an existing USB AT lease or
    /// operation. The legacy caller waited on its process-local SIM mutex;
    /// keeping that ordering in this owner prevents identity reads from
    /// bypassing an active card workflow while removing the Go-only lock.
    async fn admit_waiting(
        &self,
        producer: &str,
        session: Option<&str>,
        deadline: Instant,
    ) -> Result<IoPermit, Failure> {
        loop {
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                check(&state, producer)?;
                if let Some(session) = session
                    && (session.is_empty() || state.session != session)
                {
                    return Err(Failure::stale());
                }
                if state.lease.is_none() && !state.busy {
                    remaining(deadline)?;
                    let authority = new_io_authority(&state, producer)?;
                    state.busy = true;
                    state.busy_lease.clear();
                    return Ok(IoPermit {
                        inner: self.inner.clone(),
                        epoch: state.epoch,
                        _authority: authority,
                    });
                }
            }
            wait_until(notified, deadline).await?;
        }
    }

    async fn run<T, F>(&self, permit: IoPermit, operation: F) -> Result<T, Failure>
    where
        T: Send + 'static,
        F: FnOnce(&Inner, u64) -> Result<T, Failure> + Send + 'static,
    {
        // Spawn before yielding. Once admitted, even a canceled HTTP future
        // leaves this owner alive until the transport call actually returns.
        let worker = tokio::task::spawn_blocking(move || {
            let before = permit
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .close_failures
                .len();
            let result = operation(&permit.inner, permit.epoch);
            // Retirement paths may preserve their command failure, but checked
            // physical cleanup failure takes precedence at the actor boundary.
            let close_error = {
                let state = permit.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                (state.close_failures.len() > before)
                    .then(|| state.close_failures.last().unwrap().1.clone())
            };
            drop(permit);
            match close_error {
                Some(error) => Err(error),
                None => result,
            }
        });
        // Observation only: publish after the actual blocking job has a
        // JoinHandle, including when all blocking workers are occupied.
        #[cfg(test)]
        self.inner
            .spawned_io
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        worker.await.map_err(|_| {
            Failure::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "usb_at_worker_failed",
                "USB AT worker failed",
            )
        })?
    }

    pub(crate) async fn snapshot(&self, producer: &str) -> Result<Value, Failure> {
        match self.sms_poll_binding(producer)? {
            Some(binding) => self
                .snapshot_bound(&binding)
                .await
                .map(|observation| observation.value),
            None => Ok(raw_snapshot(producer, "busy", None, "", vec![])),
        }
    }
}

struct IoPermit {
    inner: Arc<Inner>,
    epoch: u64,
    _authority: IoAuthority,
}
impl Drop for IoPermit {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.lease.as_ref().is_some_and(|lease| {
            lease.releasing
                && lease.id == state.busy_lease
                && !state.close_failures.iter().any(|(identity, _)| {
                    lease
                        .connection
                        .as_ref()
                        .is_some_and(|connection| identity.same_connection(connection))
                })
        }) {
            state.lease = None;
        }
        state.busy = false;
        state.busy_lease.clear();
        self.inner.changed.notify_waiters();
    }
}

fn check(state: &State, producer: &str) -> Result<(), Failure> {
    if state.producer != producer {
        return Err(Failure::stale());
    }
    if state.module_recovery.is_quarantined() {
        return Err(Failure::new(
            StatusCode::CONFLICT,
            crate::module_recovery::CODE,
            crate::module_recovery::MESSAGE,
        ));
    }
    if state.stopping {
        return Err(Failure::stopped());
    }
    if state.revoking {
        return Err(Failure::busy());
    }
    Ok(())
}
fn check_epoch(state: &State, epoch: u64) -> Result<(), Failure> {
    if state.module_recovery.is_quarantined() {
        return Err(Failure::new(
            StatusCode::CONFLICT,
            crate::module_recovery::CODE,
            crate::module_recovery::MESSAGE,
        ));
    }
    if state.epoch != epoch || state.revoking {
        Err(Failure::stale())
    } else if state.stopping {
        Err(Failure::stopped())
    } else {
        Ok(())
    }
}
fn revoke(state: &mut State) -> Result<(), Failure> {
    let next_epoch = state.epoch.checked_add(1).ok_or_else(Failure::stopped)?;
    let next_identity_generation = state
        .identity_generation
        .checked_add(1)
        .ok_or_else(Failure::stopped)?;
    state.epoch = next_epoch;
    state.identity_generation = next_identity_generation;
    state.module_identity = None;
    state.native_sms_identity = None;
    state.device_observation = None;
    if !state.session.is_empty() {
        state.last_closed = std::mem::take(&mut state.session);
    }
    state.description.clear();
    state.revoking = true;
    Ok(())
}
fn detached(inner: &Inner, epoch: u64) {
    let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.epoch == epoch {
        let Some(next_generation) = state.identity_generation.checked_add(1) else {
            state.stopping = true;
            inner.changed.notify_waiters();
            return;
        };
        state.identity_generation = next_generation;
        state.module_identity = None;
        state.native_sms_identity = None;
        state.device_observation = None;
        let closed_session = state.session.clone();
        let generation = state.identity_generation;
        if let Some(slot) = state.reconnect.as_mut() {
            slot.note_closed(&closed_session, generation);
        }
        state.last_closed = std::mem::take(&mut state.session);
        state.description.clear();
        state.next_open = Some(Instant::now() + OPEN_BACKOFF);
        inner.changed.notify_waiters();
    }
}
fn check_cellular_command(inner: &Inner, command: &str) -> Result<(), Failure> {
    if inner
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .cellular_suppressed
        && crate::module_radio_transport::is_cellular_probe(command)
    {
        return Err(crate::module_radio_transport::failure(
            "module_flight_mode_blocks_cellular_probe",
        ));
    }
    Ok(())
}
fn check_radio_fence(
    inner: &Inner,
    epoch: u64,
    producer: &str,
    session: &str,
    permit: Option<&str>,
) -> Result<(), Failure> {
    let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    check_epoch(&state, epoch)?;
    if state.producer != producer || state.session != session || state.lease.is_some() {
        return Err(Failure::stale());
    }
    if state
        .inhibits
        .keys()
        .any(|token| Some(token.as_str()) != permit)
        || state
            .radio_inhibit
            .as_deref()
            .is_some_and(|token| Some(token) != permit)
    {
        return Err(Failure::busy());
    }
    Ok(())
}
fn check_device_status_fence(
    inner: &Inner,
    epoch: u64,
    session: &str,
    lease_id: &str,
) -> Result<(), Failure> {
    let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    check_epoch(&state, epoch)?;
    if state.session != session
        || !state.lease.as_ref().is_some_and(|lease| {
            lease.id == lease_id && lease.purpose == Purpose::Sim && !lease.releasing
        })
    {
        return Err(Failure::stale());
    }
    if !state.inhibits.is_empty() || state.radio_inhibit.is_some() {
        return Err(Failure::busy());
    }
    Ok(())
}
fn check_storage_fence(
    inner: &Inner,
    epoch: u64,
    producer: &str,
    session: &str,
    lease_id: &str,
    phase: crate::usb_at_storage::Phase,
) -> Result<(), Failure> {
    let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    check_epoch(&state, epoch)?;
    if state.producer != producer
        || state.session != session
        || !state
            .lease
            .as_ref()
            .is_some_and(|lease| lease.id == lease_id && lease.purpose == Purpose::Sim)
    {
        return Err(Failure::stale());
    }
    if phase == crate::usb_at_storage::Phase::Active
        && ((!state.inhibits.is_empty() || state.radio_inhibit.is_some())
            || state.lease.as_ref().is_some_and(|lease| lease.releasing))
    {
        return Err(Failure::busy());
    }
    Ok(())
}
fn check_unleased_storage_fence(
    inner: &Inner,
    epoch: u64,
    producer: &str,
    session: &str,
    phase: crate::usb_at_storage::Phase,
) -> Result<(), Failure> {
    let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    check_epoch(&state, epoch)?;
    if state.producer != producer
        || state.session != session
        || !state.busy
        || !state.busy_lease.is_empty()
        || state.lease.is_some()
    {
        return Err(Failure::stale());
    }
    if phase == crate::usb_at_storage::Phase::Active
        && (!state.inhibits.is_empty() || state.radio_inhibit.is_some())
    {
        return Err(Failure::busy());
    }
    Ok(())
}
fn begin_port_attempt(inner: &Inner, epoch: u64) -> Result<PortIdentity, Failure> {
    let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    check_epoch(&state, epoch)?;
    if let Some(lease) = &state.lease {
        if let Some(error) =
            close_failure_for_lease(&state, &state.producer, &lease.id, lease.sequence)
        {
            return Err(error);
        }
    }
    // Keep every unknown close for its original finite owner, never eviction.
    if state.close_failures.len() >= MAX_CLOSE_FAILURES {
        return Err(Failure::from(usb_at_transport::close_unconfirmed()));
    }
    state.port_sequence = state
        .port_sequence
        .checked_add(1)
        .ok_or_else(Failure::stopped)?;
    let mut identity = PortIdentity::current(&state);
    identity.session.clear(); // a real opening attempt, not a fabricated session
    if let Some(lease) = &mut state.lease {
        lease.connection = Some(identity.clone());
    }
    Ok(identity)
}

/// Own a candidate through every fallible pre-publication check. Its Drop is
/// cleanup, not success evidence: it invokes checked close and records failures.
struct PendingPort<'a> {
    inner: &'a Inner,
    identity: PortIdentity,
    port: Option<Box<dyn Transport>>,
}
impl PendingPort<'_> {
    fn commit(mut self, session: String) -> (Box<dyn Transport>, PortIdentity) {
        self.identity.session = session;
        (
            self.port.take().expect("pending port"),
            self.identity.clone(),
        )
    }
}
impl std::ops::Deref for PendingPort<'_> {
    type Target = Box<dyn Transport>;
    fn deref(&self) -> &Self::Target {
        self.port.as_ref().expect("pending port")
    }
}
impl std::ops::DerefMut for PendingPort<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.port.as_mut().expect("pending port")
    }
}
impl Drop for PendingPort<'_> {
    fn drop(&mut self) {
        if let Some(port) = self.port.take() {
            let _ = native_module::retire_port(self.inner, self.identity.clone(), port);
        }
    }
}
fn pending_port<'a>(
    inner: &'a Inner,
    identity: PortIdentity,
    open: impl FnOnce() -> Result<Box<dyn Transport>, TransportError>,
) -> Result<PendingPort<'a>, Failure> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(open))
        .unwrap_or_else(|_| Err(usb_at_transport::close_unconfirmed()));
    match result {
        Ok(port) => Ok(PendingPort {
            inner,
            identity,
            port: Some(port),
        }),
        Err(error) => {
            let close_failed = error.code == "usb_at_close_unconfirmed";
            let error = Failure::from(error);
            if close_failed {
                native_module::record_close(inner, identity, Err(error.clone()));
            }
            Err(error)
        }
    }
}
fn ensure_open(inner: &Inner, epoch: u64) -> Result<(), Failure> {
    let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
    {
        let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check_epoch(&state, epoch)?;
        // Native startup must never attach to, or reopen around, an
        // interface whose checked close remains uncertain. Generic USB AT
        // sessions retain their separate per-connection failure contract.
        if state.native_managed {
            if let Some((_, error)) = state.close_failures.first() {
                return Err(error.clone());
            }
        }
        if port.is_some() {
            return Ok(());
        }
        if state
            .next_open
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return Err(Failure::busy());
        }
    }
    let identity = begin_port_attempt(inner, epoch)?;
    let mut opened = pending_port(inner, identity, || (inner.factory)())?;
    let description = opened.description();
    let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    check_epoch(&state, epoch)?;
    let generation = state
        .identity_generation
        .checked_add(1)
        .ok_or_else(Failure::stopped)?;
    let session = crate::snapshot::new_instance_id();
    install_call_capture(inner, opened.as_mut(), &state.producer, &session);
    let (opened, identity) = opened.commit(session.clone());
    state.identity_generation = generation;
    state.module_identity = None;
    state.session = session;
    state.native_sms_identity = None;
    state.device_observation = None;
    state.description = description;
    state.next_open = None;
    state.port_identity = Some(identity);
    *port = Some(opened);
    Ok(())
}

fn validate_token(value: &str) -> Result<(), Failure> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        Err(Failure::invalid())
    } else {
        Ok(())
    }
}
fn validate_optional_token(value: &str) -> Result<(), Failure> {
    if value.is_empty() {
        Ok(())
    } else {
        validate_token(value)
    }
}
fn remaining(deadline: Instant) -> Result<Duration, Failure> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining < Duration::from_millis(1) {
        Err(Failure::timeout())
    } else {
        Ok(remaining)
    }
}
async fn wait_until<F: std::future::Future<Output = ()>>(
    notification: F,
    deadline: Instant,
) -> Result<(), Failure> {
    tokio::time::timeout(remaining(deadline)?, notification)
        .await
        .map_err(|_| Failure::timeout())
}
fn shutdown_command_allowed(command: &str) -> bool {
    let command = command.trim().to_ascii_uppercase();
    command == "AT+CFUN?"
        || command == "AT+CFUN=?"
        || matches!(
            command.as_str(),
            "AT+CFUN=0" | "AT+CFUN=1" | "AT+CFUN=2" | "AT+CFUN=3" | "AT+CFUN=4"
        )
}

fn raw_snapshot(
    producer: &str,
    mode: &str,
    config: Option<SmsConfig>,
    session: &str,
    storages: Vec<Value>,
) -> Value {
    json!({"producer_id":producer,"mode":mode,"adopt":config.is_some_and(|c| c.adopt),
        "poll_interval_ms":config.map_or(8000, |c| c.interval_ms),
        "captured_at":OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_default(),
        "session_id":session,"card_identity":"","storages":storages})
}
fn complete_ok(response: &str) -> bool {
    response
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .next_back()
        == Some("OK")
}
fn complete_error(response: &str) -> bool {
    response
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .next_back()
        .is_some_and(|line| {
            line == "ERROR" || line.starts_with("+CMS ERROR:") || line.starts_with("+CME ERROR:")
        })
}
fn read_snapshot(
    inner: &Inner,
    epoch: u64,
    observation: &SmsObservationToken,
    port: &mut dyn Transport,
    deadline: Instant,
) -> Result<(Vec<Value>, bool, String), Failure> {
    let mut lost: Option<Failure> = None;
    let mut run = |command: &str, budget: Duration, limit: usize| -> Result<String, Failure> {
        {
            let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check_epoch(&state, epoch)?;
            observation.check(&state)?;
            if (!state.inhibits.is_empty() || state.radio_inhibit.is_some())
                || state.sms.is_some_and(|sms| sms.serial)
            {
                return Err(Failure::busy());
            }
        }
        if let Some(error) = &lost {
            return Err(error.clone());
        }
        let result = port
            .command(command, None, remaining(deadline)?.min(budget), limit)
            .map_err(Failure::from)
            .and_then(|response| {
                if complete_ok(&response) || complete_error(&response) {
                    Ok(response)
                } else {
                    // A normal transport command can return partial data at
                    // its deadline. A quiet drain cannot prove that the old
                    // command has finished, so never continue this snapshot
                    // or grant deletion authority on the same stream.
                    Err(Failure::new(
                        StatusCode::BAD_GATEWAY,
                        "usb_sms_snapshot_incomplete_response",
                        "USB SMS snapshot response is incomplete",
                    ))
                }
            });
        check_epoch(
            &inner.state.lock().unwrap_or_else(|e| e.into_inner()),
            epoch,
        )?;
        if let Err(error) = &result {
            // Preserve any storage already read, but never send another AT
            // command after a physical transport failure. In particular an
            // overflow/drain/read failure cannot grant a new deletion proof
            // using a potentially misaligned later QCCID response.
            lost = Some(error.clone());
        }
        result
    };
    let identity_before = match run("AT+QCCID", Duration::from_secs(3), 4096) {
        Ok(response) => crate::usb_at_storage::card_identity(&response),
        Err(error)
            if matches!(
                error.code,
                "usb_at_stale_session" | "usb_at_stopping" | "usb_at_busy" | "usb_at_wait_timeout"
            ) =>
        {
            return Err(error);
        }
        Err(_) => None,
    };
    let pdu = run("AT+CMGF=0", Duration::from_secs(3), 4096)?;
    if !complete_ok(&pdu) {
        return Err(Failure::new(
            StatusCode::BAD_GATEWAY,
            "usb_sms_snapshot_failed",
            "USB SMS PDU mode not confirmed",
        ));
    }
    let mut storages = Vec::new();
    for storage in ["SM", "ME"] {
        let select = run(
            &format!("AT+CPMS=\"{storage}\",\"{storage}\",\"{storage}\""),
            Duration::from_secs(5),
            4096,
        );
        match select {
            Err(error)
                if matches!(
                    error.code,
                    "usb_at_stale_session"
                        | "usb_at_stopping"
                        | "usb_at_busy"
                        | "usb_at_wait_timeout"
                ) =>
            {
                return Err(error);
            }
            Err(_) => {
                storages
                    .push(json!({"storage":storage,"response":"","error_code":"select_failed"}));
                continue;
            }
            Ok(response) if !complete_ok(&response) => {
                storages
                    .push(json!({"storage":storage,"response":"","error_code":"select_failed"}));
                continue;
            }
            Ok(_) => {}
        }
        let read = run("AT+CMGL=4", Duration::from_secs(15), MAX_STORAGE_BYTES);
        match read {
            Err(error) if matches!(error.code, "usb_at_stale_session" | "usb_at_stopping" | "usb_at_busy" | "usb_at_wait_timeout") => return Err(error),
            Err(error) => storages.push(json!({"storage":storage,"response":"","error_code":match error.code { "usb_at_response_too_large" => "response_too_large", "usb_sms_snapshot_incomplete_response" => "incomplete_response", _ => "read_failed"}})),
            Ok(response) if !complete_ok(&response) => storages.push(json!({"storage":storage,"response":"","error_code":"incomplete_response"})),
            Ok(response) => storages.push(json!({"storage":storage,"response":response,"error_code":""})),
        }
    }
    let identity_after = match run("AT+QCCID", Duration::from_secs(3), 4096) {
        Ok(response) => crate::usb_at_storage::card_identity(&response),
        Err(error)
            if matches!(
                error.code,
                "usb_at_stale_session" | "usb_at_stopping" | "usb_at_busy" | "usb_at_wait_timeout"
            ) =>
        {
            return Err(error);
        }
        Err(_) => None,
    };
    let identity = match (identity_before, identity_after) {
        (Some(before), Some(after)) if before == after => before,
        _ => String::new(),
    };
    Ok((storages, lost.is_some(), identity))
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    owner: Owner,
    stopping: bool,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST {
        return crate::json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            &json!({"code":"method_not_allowed"}),
        );
    }
    let path = request.uri().path().to_owned();
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        MAX_BYTES,
        Duration::from_secs(5),
    )
    .await
    {
        Ok(body) => body,
        _ => return Failure::invalid().response(),
    };
    let result = dispatch(&path, &body, &owner, stopping).await;
    match result {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(error) => error.response(),
    }
}
async fn dispatch(
    path: &str,
    body: &[u8],
    owner: &Owner,
    stopping: bool,
) -> Result<Value, Failure> {
    fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Failure> {
        serde_json::from_slice(body).map_err(|_| Failure::invalid())
    }
    match path.strip_prefix(PREFIX).unwrap_or("") {
        "open" => owner.open(decode(body)?, stopping).await,
        "status" => {
            let input: IdentityRequest = decode(body)?;
            validate_token(&input.producer_id)?;
            owner.status(&input.producer_id)
        }
        "acquire" => owner.acquire(decode(body)?).await,
        "release" => owner.release(decode(body)?).await,
        "close" => owner.close(decode(body)?).await,
        "configure" => owner.configure(decode(body)?, stopping),
        "command" => owner.command(decode(body)?, stopping).await,
        "device-status" => owner.device_status(decode(body)?, stopping).await,
        "sim-identity" => {
            let _: SimIdentityRequest = decode(body)?;
            owner.sim_identity(stopping).await
        }
        "sim-profile-check" => owner.sim_profile_check(decode(body)?, stopping).await,
        "sim-aka" => owner.sim_aka(decode(body)?, stopping).await,
        "submit-sms" => {
            let outcome = owner.submit_sms(decode(body)?, stopping).await?;
            serde_json::to_value(outcome).map_err(|_| Failure::invalid())
        }
        _ => Err(Failure::new(
            StatusCode::NOT_FOUND,
            "route_not_found",
            "USB AT route not found",
        )),
    }
}

#[cfg(test)]
#[path = "usb_at/quarantine_tests.rs"]
mod quarantine_tests;
#[cfg(test)]
#[path = "usb_at/tests.rs"]
mod tests;

#[cfg(test)]
mod call_mode_radio_reconnect_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct FakeTransport(Arc<AtomicUsize>);
    impl Transport for FakeTransport {
        fn checked_close(
            self: Box<Self>,
        ) -> Result<
            crate::usb_at_transport::CloseDisposition,
            crate::usb_at_transport::TransportError,
        > {
            // Explicit fixture release; Drop barriers must finish before success.
            drop(self);
            Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
        }

        fn command(
            &mut self,
            _: &str,
            _: Option<&[u8]>,
            _: Duration,
            _: usize,
        ) -> Result<String, TransportError> {
            panic!("reconnect bridge must not send AT configuration")
        }
        fn description(&self) -> String {
            "offline call mode module".into()
        }
    }
    impl Drop for FakeTransport {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[tokio::test]
    async fn native_usb_reconnect_keeps_already_rebound_session_and_retires_only_old_handle() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let counter = dropped.clone();
        let owner = Owner::with_factory(
            "native-usb-reconnect".into(),
            Arc::new(move || Ok(Box::new(FakeTransport(counter.clone())))),
        );
        let cancellation = Arc::new(AtomicBool::new(false));
        let first = owner
            .call_mode_radio_reconnect(
                "native-usb-reconnect",
                "",
                Instant::now() + Duration::from_secs(2),
                cancellation.clone(),
            )
            .await
            .unwrap();
        let adopted = owner
            .call_mode_radio_reconnect(
                "native-usb-reconnect",
                "old-other-stream",
                Instant::now() + Duration::from_secs(2),
                cancellation.clone(),
            )
            .await
            .unwrap();
        assert_eq!(adopted, first);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        assert!(
            owner
                .call_mode_radio_reconnect(
                    "native-usb-reconnect",
                    &first,
                    Instant::now() + Duration::from_secs(2),
                    cancellation.clone()
                )
                .await
                .is_err()
        );
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(owner.status("native-usb-reconnect").unwrap()["open"], false);
        owner.inner.state.lock().unwrap().next_open = None;
        let rebound = owner
            .call_mode_radio_reconnect(
                "native-usb-reconnect",
                &first,
                Instant::now() + Duration::from_secs(2),
                cancellation,
            )
            .await
            .unwrap();
        assert_ne!(rebound, first);
        owner.shutdown().await.unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn native_usb_reconnect_cancellation_after_factory_waits_cleanup_and_drops_candidate() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicUsize::new(0));
        let counter = dropped.clone();
        let (entered, started) = std::sync::mpsc::sync_channel(1);
        let (unblock, finish) = std::sync::mpsc::sync_channel(1);
        let finish = Mutex::new(finish);
        let owner = Owner::with_factory(
            "native-usb-cancel".into(),
            Arc::new(move || {
                entered.send(()).unwrap();
                finish.lock().unwrap().recv().unwrap();
                Ok(Box::new(FakeTransport(counter.clone())))
            }),
        );
        let worker = owner.clone();
        let flag = cancelled.clone();
        let reconnect = tokio::spawn(async move {
            worker
                .call_mode_radio_reconnect(
                    "native-usb-cancel",
                    "",
                    Instant::now() + Duration::from_secs(5),
                    flag,
                )
                .await
        });
        tokio::task::spawn_blocking(move || started.recv().unwrap())
            .await
            .unwrap();
        assert_eq!(owner.status("native-usb-cancel").unwrap()["busy"], true);
        cancelled.store(true, Ordering::Release);
        assert!(!reconnect.is_finished());
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        unblock.send(()).unwrap();
        assert_eq!(
            reconnect.await.unwrap().unwrap_err().code,
            "radio_owner_revoked"
        );
        let status = owner.status("native-usb-cancel").unwrap();
        assert_eq!(status["open"], false);
        assert_eq!(status["busy"], false);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        owner.shutdown().await.unwrap();
    }
}

fn install_call_capture(inner: &Inner, port: &mut dyn Transport, producer: &str, session: &str) {
    let slot = inner.call_capture.clone();
    let binding = crate::module_radio_transport::Binding::Usb {
        producer_id: producer.into(),
        session_id: session.into(),
    };
    port.set_line_capture(Arc::new(move |line| {
        let capture = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(capture) = capture {
            capture(binding.clone(), line);
        }
    }));
}
impl Owner {
    pub(crate) fn call_binding_current(&self, producer: &str, session: &str) -> bool {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        !state.stopping
            && !state.revoking
            && state.producer == producer
            && state.session == session
            && !session.is_empty()
    }
    pub(crate) fn set_call_capture(&self, capture: crate::module_radio::CallCapture) {
        *self
            .inner
            .call_capture
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(capture);
    }
    pub(crate) async fn poll_call_capture(
        &self,
        producer: String,
        session: String,
    ) -> Result<(), Failure> {
        let deadline = Instant::now() + Duration::from_millis(100);
        let permit = self.admit(&producer, Some(&session), "", deadline).await?;
        self.run(permit, move |inner, epoch| {
            check_radio_fence(inner, epoch, &producer, &session, None)?;
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let result = port
                .as_mut()
                .ok_or_else(Failure::stale)?
                .poll_call_capture(Duration::from_millis(40))
                .map_err(Failure::from);
            if result.is_err() {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            result
        })
        .await
    }
}
