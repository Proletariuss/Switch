//! Rust owner for the optional, control-only QMI enhancement path.
//!
//! The AT backend remains the default. This owner is opened only when the
//! native daemon explicitly selects `-backend=qmi`. The authenticated private
//! Unix socket retains typed owner operations for internal clients. It exposes
//! typed IMS/NAS/UIM operations rather than arbitrary QMI frames.
//!
//! The control-owner recovery policy is adapted from Celmux's MIT QMI manager
//! error classification and recovery lifecycle, narrowed to this single-owner
//! IMS/NAS/UIM path. Mutating requests are never replayed after an uncertain result.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use proletarius_qmi_core::ClientError;
use proletarius_qmi_services::{
    ImsClient, ImsSettingsUpdate, NasClient, QmiServiceClientError, UimClient,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[path = "qmi_native_startup.rs"]
mod native_startup;
pub(crate) use native_startup::{NativeQmiConfig, NativeQmiLifecycle, NativeQmiRetryLifecycle};

pub(crate) const PREFIX: &str = "/api/internal/qmi/";
const MAX_BODY_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub(crate) struct QmiEnhancement {
    state: Arc<Mutex<QmiState>>,
    factory: Arc<dyn QmiSessionFactory>,
    module_recovery: crate::module_recovery::Gate,
}

impl Default for QmiEnhancement {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(QmiState::default())),
            factory: Arc::new(PlatformQmiSessionFactory),
            module_recovery: crate::module_recovery::Gate::default(),
        }
    }
}

#[derive(Default)]
struct QmiState {
    enabled: bool,
    /// Only a private close disables automatic USB attach/reconnect. This is
    /// separate from `enabled`, which selects the QMI backend for status.
    explicitly_closed: bool,
    /// Fences an automatic attempt that straddles a private close/open.
    manual_epoch: u64,
    /// A private open may select an exact VID:PID; retry must not silently
    /// switch that choice back to the automatic priority list after loss.
    retry_control_path: String,
    control_path: Option<String>,
    session: Option<Box<dyn QmiSessionOps>>,
    last_open_error: Option<String>,
    timeouts: TimeoutWindows,
    open_backoff: OpenBackoff,
    cleanup_failure: Option<String>,
}

trait QmiSessionFactory: Send + Sync {
    fn open(&self, control_path: &str) -> Result<Box<dyn QmiSessionOps>, QmiOpenFailure>;
}

/// A failed optional startup is harmless only when its acquired resources
/// have a confirmed cleanup receipt. Keep that distinction out of display text.
struct QmiOpenFailure {
    message: String,
    cleanup_failure: Option<String>,
}

impl From<String> for QmiOpenFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            cleanup_failure: None,
        }
    }
}

impl QmiOpenFailure {
    fn after_cleanup(message: String, cleanup: Result<(), String>) -> Self {
        Self {
            message,
            cleanup_failure: cleanup.err(),
        }
    }
}

trait QmiSessionOps: Send {
    fn probe(&mut self) -> Result<Value, QmiReadFailure>;
    fn set_ims(&mut self, update: ImsSettingsUpdate) -> Result<(), QmiServiceClientError>;
    fn operator(&mut self) -> Result<Value, QmiReadFailure>;
    fn close(&mut self) -> Result<(), String>;
    /// Stop a failed session and confirm its physical transport is released
    /// before any replacement can be opened. A failed release is terminal for
    /// this owner because the previous USB claim may still be live.
    fn abort(&mut self) -> Result<(), String>;
    /// Cache-only liveness of the retained reader. No QMI request is sent.
    fn transport_closed(&self) -> bool {
        false
    }
}

struct QmiReadFailure {
    scope: &'static str,
    message: &'static str,
    source: QmiServiceClientError,
    completed_scopes: Vec<&'static str>,
}

impl QmiReadFailure {
    fn new(scope: &'static str, message: &'static str, source: QmiServiceClientError) -> Self {
        Self {
            scope,
            message,
            source,
            completed_scopes: Vec::new(),
        }
    }

    fn after_completed(mut self, scopes: &[&'static str]) -> Self {
        self.completed_scopes.extend_from_slice(scopes);
        self
    }
}

impl std::fmt::Display for QmiReadFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.message, self.source)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryClass {
    TransportLost,
    ServiceHandleLost,
    Timeout,
    Permanent,
}

const QMI_SERVICE_TIMEOUT_LIMIT: u8 = 3;
const QMI_SERVICE_TIMEOUT_WINDOW: Duration = Duration::from_secs(60);
const QMI_RECOVERY_INITIAL_DELAY: Duration = Duration::from_millis(500);
const QMI_RECOVERY_MAX_DELAY: Duration = Duration::from_secs(30);
const QMI_ERROR_DEVICE_NOT_READY: u16 = 0x0005;
const QMI_ERROR_CLIENT_IDS_EXHAUSTED: u16 = 0x001f;
const QMI_ERROR_INVALID_ID: u16 = 0x0029;

#[derive(Default)]
struct TimeoutWindows {
    windows: HashMap<&'static str, TimeoutWindow>,
}

struct TimeoutWindow {
    started: Instant,
    failures: u8,
}

#[derive(Default)]
struct OpenBackoff {
    failures: u8,
    retry_at: Option<Instant>,
}

impl OpenBackoff {
    fn remaining(&self, now: Instant) -> Option<Duration> {
        self.retry_at
            .filter(|retry_at| *retry_at > now)
            .map(|retry_at| retry_at.saturating_duration_since(now))
    }

    fn failed(&mut self, now: Instant) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let exponent = u32::from(self.failures.saturating_sub(1).min(6));
        let multiplier = 1_u32 << exponent;
        let delay = QMI_RECOVERY_INITIAL_DELAY
            .saturating_mul(multiplier)
            .min(QMI_RECOVERY_MAX_DELAY);
        self.retry_at = now.checked_add(delay);
        delay
    }

    fn succeeded(&mut self) {
        *self = Self::default();
    }
}

fn classify_qmi_failure(error: &QmiServiceClientError) -> RecoveryClass {
    let QmiServiceClientError::Client(error) = error else {
        return RecoveryClass::Permanent;
    };
    if error.is_transport_lost() {
        return RecoveryClass::TransportLost;
    }
    match error {
        ClientError::Timeout => RecoveryClass::Timeout,
        ClientError::Qmi { error, .. }
            if matches!(
                *error,
                QMI_ERROR_DEVICE_NOT_READY | QMI_ERROR_CLIENT_IDS_EXHAUSTED | QMI_ERROR_INVALID_ID
            ) =>
        {
            RecoveryClass::ServiceHandleLost
        }
        _ => RecoveryClass::Permanent,
    }
}

struct PlatformQmiSessionFactory;

struct PlatformQmiSession {
    #[cfg(target_os = "macos")]
    usb: proletarius_qmi_libusb::QmiUsbClient,
    #[cfg(target_os = "macos")]
    ims: ImsClient,
    #[cfg(target_os = "macos")]
    nas: NasClient,
    #[cfg(target_os = "macos")]
    uim: UimClient,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: &'static str,
    error: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    #[serde(default)]
    control_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetImsRequest {
    #[serde(default)]
    voice_over_lte: Option<bool>,
    #[serde(default)]
    video_telephony: Option<bool>,
    #[serde(default)]
    voice_wifi: Option<bool>,
    #[serde(default)]
    call_mode: Option<u8>,
    #[serde(default)]
    ims: Option<bool>,
    #[serde(default)]
    ut: Option<bool>,
    #[serde(default)]
    sms: Option<bool>,
    #[serde(default)]
    ussd: Option<bool>,
    #[serde(default)]
    presence: Option<bool>,
    #[serde(default)]
    autoconfig: Option<bool>,
    #[serde(default)]
    xdm_client: Option<bool>,
    #[serde(default)]
    rcs: Option<bool>,
    #[serde(default)]
    carrier_config: Option<bool>,
}

impl SetImsRequest {
    fn is_empty(&self) -> bool {
        self.voice_over_lte.is_none()
            && self.video_telephony.is_none()
            && self.voice_wifi.is_none()
            && self.call_mode.is_none()
            && self.ims.is_none()
            && self.ut.is_none()
            && self.sms.is_none()
            && self.ussd.is_none()
            && self.presence.is_none()
            && self.autoconfig.is_none()
            && self.xdm_client.is_none()
            && self.rcs.is_none()
            && self.carrier_config.is_none()
    }
}

impl From<SetImsRequest> for ImsSettingsUpdate {
    fn from(value: SetImsRequest) -> Self {
        Self {
            voice_over_lte: value.voice_over_lte,
            video_telephony: value.video_telephony,
            voice_wifi: value.voice_wifi,
            call_mode: value.call_mode,
            ims: value.ims,
            ut: value.ut,
            sms: value.sms,
            ussd: value.ussd,
            presence: value.presence,
            autoconfig: value.autoconfig,
            xdm_client: value.xdm_client,
            rcs: value.rcs,
            carrier_config: value.carrier_config,
        }
    }
}

impl QmiEnhancement {
    /// Inject the daemon's shared authority before cloning this owner.
    pub(crate) fn with_module_recovery(mut self, gate: crate::module_recovery::Gate) -> Self {
        self.module_recovery = gate;
        self
    }

    fn admit_operation(&self) -> Result<crate::module_recovery::OperationGuard, RpcError> {
        self.module_recovery
            .admit_operation("rust-native-qmi")
            .map_err(|()| RpcError {
                code: crate::module_recovery::CODE,
                error: crate::module_recovery::MESSAGE.to_owned(),
            })
    }

    fn confirm_startup_admission(&self, epoch: u64) -> Result<(), RpcError> {
        let _operation = self.admit_operation()?;
        let state = self.lock_state()?;
        if state.explicitly_closed || state.manual_epoch != epoch {
            return Err(unavailable(
                "QMI automatic startup was superseded by a private request",
            ));
        }
        Ok(())
    }

    pub(crate) fn enable(&self) -> Result<(u64, String), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "QMI owner lock is poisoned".to_owned())?;
        state.enabled = true;
        // Selecting the backend must never undo a private close.
        Ok((state.manual_epoch, state.retry_control_path.clone()))
    }

    fn auto_reconnect_desired(&self) -> Result<bool, String> {
        self.state
            .lock()
            .map(|state| !state.explicitly_closed)
            .map_err(|_| "QMI owner lock is poisoned".to_owned())
    }

    fn auto_epoch_current(&self, epoch: u64) -> Result<bool, String> {
        self.state
            .lock()
            .map(|state| !state.explicitly_closed && state.manual_epoch == epoch)
            .map_err(|_| "QMI owner lock is poisoned".to_owned())
    }

    fn open_auto(&self, control_path: String, epoch: u64) -> Result<Value, RpcError> {
        let mut state = self.lock_state()?;
        let _operation = self.admit_operation()?;
        if state.explicitly_closed || state.manual_epoch != epoch {
            return Err(unavailable(
                "QMI automatic startup was superseded by a private request",
            ));
        }
        state.open(&control_path, self.factory.as_ref())?;
        Ok(json!({ "opened": true }))
    }

    fn close_auto_epoch(&self, epoch: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.manual_epoch != epoch {
            // The private operation owns any new session. An earlier failed
            // release remains a hard fence; never reinterpret it as success.
            return state.cleanup_failure.clone().map_or(Ok(()), Err);
        }
        state.close()
    }

    fn close_for_shutdown(&self) -> Result<(), String> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .close()
    }

    /// Cache-only projection for GET /api/status. Initialization means the
    /// retained client passed CTL_SYNC and IMS/NAS/UIM client setup; it does
    /// not probe current USB link health or claim SIM/radio/IMS readiness.
    pub(crate) fn transport_status(&self) -> Option<proletarius_api_contract::UpstreamQmiStatus> {
        let state = self.state.lock().ok()?;
        if !state.enabled {
            return None;
        }
        let reader_lost = state
            .session
            .as_ref()
            .is_some_and(|session| session.transport_closed());
        Some(proletarius_api_contract::UpstreamQmiStatus {
            enabled: state.enabled,
            session_initialized: state.session.is_some() && !reader_lost,
            initialization_error: if let Some(error) = &state.cleanup_failure {
                Some(bounded_diagnostic(error, 240))
            } else if state.explicitly_closed && state.session.is_none() {
                Some("QMI enhancement closed by private request".to_owned())
            } else if reader_lost {
                Some("QMI USB transport disconnected; reconnecting".to_owned())
            } else if state.session.is_none() {
                state.last_open_error.clone()
            } else {
                None
            },
        })
    }

    /// Observe a terminal reader exit without polling the module or SIM.
    /// The retry owner still owes a confirmed close before any new open.
    fn observed_transport_closed(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let closed = state
            .session
            .as_ref()
            .is_some_and(|session| session.transport_closed());
        if closed {
            state.last_open_error = Some("QMI USB transport disconnected; reconnecting".to_owned());
        }
        closed
    }

    fn record_retry_failure(&self, error: &str) {
        if let Ok(mut state) = self.state.lock()
            && state.session.is_none()
        {
            state.last_open_error = Some(bounded_diagnostic(error, 240));
        }
    }

    fn open(&self, control_path: String) -> Result<Value, RpcError> {
        if control_path.len() > 256 || control_path.contains('\0') {
            return Err(invalid_request("QMI control path is invalid"));
        }
        let mut state = self.lock_state()?;
        let _operation = self.admit_operation()?;
        let result = state.open(&control_path, self.factory.as_ref());
        // A valid private open reauthorizes retry even if the selected device
        // is temporarily absent. Rejected path conflicts and unconfirmed
        // cleanup cannot clear an explicit-close fence.
        if result.is_ok()
            || (result
                .as_ref()
                .err()
                .is_some_and(|error| error.code == "qmi_unavailable")
                && state.control_path.as_deref() == Some(control_path.as_str())
                && state.cleanup_failure.is_none())
        {
            state.explicitly_closed = false;
            state.manual_epoch = state.manual_epoch.wrapping_add(1);
            state.retry_control_path = control_path;
        }
        result?;
        Ok(json!({ "opened": true }))
    }

    fn close(&self) -> Result<Value, RpcError> {
        let mut state = self.lock_state()?;
        state.explicitly_closed = true;
        state.manual_epoch = state.manual_epoch.wrapping_add(1);
        state.retry_control_path.clear();
        let result = state.close();
        state.last_open_error = Some(match &result {
            Ok(()) => "QMI enhancement closed by private request".to_owned(),
            Err(error) => bounded_diagnostic(&format!("QMI close failed: {error}"), 240),
        });
        result.map_err(|error| unavailable(format!("QMI close failed: {error}")))?;
        Ok(json!({ "closed": true }))
    }

    fn probe(&self) -> Result<Value, RpcError> {
        self.run_read(&["ims-services", "imsa-registration"], |session| {
            session.probe()
        })
    }

    fn set_ims(&self, update: ImsSettingsUpdate) -> Result<Value, RpcError> {
        let mut state = self.lock_state()?;
        let _operation = self.admit_operation()?;
        state.ensure_open(self.factory.as_ref())?;
        match state
            .session
            .as_mut()
            .expect("ensure_open installs a QMI session")
            .set_ims(update)
        {
            Ok(()) => {
                state.timeouts.clear(&["ims-set-services"]);
                Ok(json!({ "updated": true }))
            }
            Err(error) => {
                let original = format!("QMI IMS update failed: {error}");
                let class = classify_qmi_failure(&error);
                if state
                    .timeouts
                    .recovery_due("ims-set-services", class, Instant::now())
                    && let Err(recovery_error) =
                        state.reopen(self.factory.as_ref(), &self.module_recovery)
                {
                    return Err(unavailable(format!(
                        "{original}; QMI owner recovery failed: {recovery_error}"
                    )));
                }
                Err(unavailable(original))
            }
        }
    }

    fn operator(&self) -> Result<Value, RpcError> {
        self.run_read(&["uim-home-network"], |session| session.operator())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, QmiState>, RpcError> {
        self.state
            .lock()
            .map_err(|_| unavailable("QMI owner lock is poisoned"))
    }

    fn run_read(
        &self,
        successful_scopes: &[&'static str],
        mut query: impl FnMut(&mut dyn QmiSessionOps) -> Result<Value, QmiReadFailure>,
    ) -> Result<Value, RpcError> {
        let mut state = self.lock_state()?;
        let _operation = self.admit_operation()?;
        state.ensure_open(self.factory.as_ref())?;
        match query(
            state
                .session
                .as_mut()
                .expect("ensure_open installs a QMI session")
                .as_mut(),
        ) {
            Ok(value) => {
                state.timeouts.clear(successful_scopes);
                Ok(value)
            }
            Err(failure) => {
                state.timeouts.clear(&failure.completed_scopes);
                let recovery = state.timeouts.recovery_due(
                    failure.scope,
                    classify_qmi_failure(&failure.source),
                    Instant::now(),
                );
                if !recovery {
                    return Err(unavailable(failure.to_string()));
                }
                let original = failure.to_string();
                if let Err(error) = state.reopen(self.factory.as_ref(), &self.module_recovery) {
                    return Err(unavailable(format!(
                        "{original}; QMI owner recovery failed: {error}"
                    )));
                }
                match query(
                    state
                        .session
                        .as_mut()
                        .expect("reopen installs a QMI session")
                        .as_mut(),
                ) {
                    Ok(value) => {
                        state.timeouts.clear(successful_scopes);
                        Ok(value)
                    }
                    Err(retry_failure) => Err(unavailable(format!(
                        "{original}; one QMI recovery attempt also failed: {retry_failure}"
                    ))),
                }
            }
        }
    }
}

impl QmiState {
    fn open(
        &mut self,
        control_path: &str,
        factory: &dyn QmiSessionFactory,
    ) -> Result<(), RpcError> {
        if self.session.is_some() {
            if self.control_path.as_deref() == Some(control_path) {
                return Ok(());
            }
            return Err(conflict("QMI owner already holds another control path"));
        }
        if let Some(current) = self.control_path.as_deref() {
            if current != control_path {
                return Err(conflict(
                    "QMI owner recovery is bound to another control path; close it first",
                ));
            }
            return self.ensure_open(factory);
        }
        self.control_path = Some(control_path.to_owned());
        self.timeouts = TimeoutWindows::default();
        self.ensure_open(factory)
    }

    fn ensure_open(&mut self, factory: &dyn QmiSessionFactory) -> Result<(), RpcError> {
        if self.session.is_some() {
            return Ok(());
        }
        let path = self
            .control_path
            .clone()
            .ok_or_else(|| unavailable("QMI enhancement is not open"))?;
        self.open_configured(&path, factory, Instant::now())
            .map_err(unavailable)
    }

    fn reopen(
        &mut self,
        factory: &dyn QmiSessionFactory,
        gate: &crate::module_recovery::Gate,
    ) -> Result<(), String> {
        // The previous typed request has failed. Opening another session is a
        // new bounded recovery attempt; an old session cannot authorize it.
        let _operation = gate
            .admit_operation("rust-native-qmi")
            .map_err(|()| crate::module_recovery::MESSAGE.to_owned())?;
        let path = self
            .control_path
            .clone()
            .ok_or_else(|| "QMI owner has no configured control path".to_owned())?;
        if let Some(mut session) = self.session.take() {
            self.cleanup_failure =
                Some("QMI recovery abort did not reach a confirmed result".to_owned());
            let result = session.abort();
            self.cleanup_failure = result.as_ref().err().cloned();
            result.map_err(|error| format!("QMI prior cleanup remains unconfirmed: {error}"))?;
        }
        self.open_configured(&path, factory, Instant::now())
    }

    fn open_configured(
        &mut self,
        path: &str,
        factory: &dyn QmiSessionFactory,
        now: Instant,
    ) -> Result<(), String> {
        if let Some(error) = &self.cleanup_failure {
            return Err(format!("QMI prior cleanup remains unconfirmed: {error}"));
        }
        if let Some(remaining) = self.open_backoff.remaining(now) {
            return Err(format!(
                "QMI recovery backoff active for {} ms",
                remaining.as_millis()
            ));
        }
        self.cleanup_failure =
            Some("QMI startup did not reach a confirmed ownership result".to_owned());
        match factory.open(path) {
            Ok(session) => {
                self.session = Some(session);
                self.cleanup_failure = None;
                self.last_open_error = None;
                self.timeouts = TimeoutWindows::default();
                self.open_backoff.succeeded();
                Ok(())
            }
            Err(error) => {
                self.cleanup_failure = error.cleanup_failure;
                let message = error.message;
                let delay = self.open_backoff.failed(now);
                self.last_open_error = Some(bounded_diagnostic(&message, 240));
                Err(format!(
                    "QMI session open failed: {message}; next attempt after {} ms",
                    delay.as_millis()
                ))
            }
        }
    }

    fn close(&mut self) -> Result<(), String> {
        self.control_path = None;
        self.timeouts = TimeoutWindows::default();
        self.open_backoff.succeeded();
        if let Some(error) = &self.cleanup_failure {
            return Err(error.clone());
        }
        if self.session.is_none() {
            return Ok(());
        }
        self.cleanup_failure =
            Some("QMI session close did not reach a confirmed result".to_owned());
        let mut session = self.session.take().expect("checked QMI session");
        let result = session.close();
        self.cleanup_failure = result.as_ref().err().cloned();
        result
    }
}

fn bounded_diagnostic(message: &str, maximum_chars: usize) -> String {
    let mut visible = message.chars().filter(|character| !character.is_control());
    let mut value = visible.by_ref().take(maximum_chars).collect::<String>();
    if visible.next().is_some() {
        value.push('…');
    }
    value
}

impl TimeoutWindows {
    fn recovery_due(&mut self, scope: &'static str, class: RecoveryClass, now: Instant) -> bool {
        match class {
            RecoveryClass::TransportLost | RecoveryClass::ServiceHandleLost => true,
            RecoveryClass::Timeout => self.timeout_recovery_due(scope, now),
            RecoveryClass::Permanent => false,
        }
    }

    fn timeout_recovery_due(&mut self, scope: &'static str, now: Instant) -> bool {
        let window = self.windows.entry(scope).or_insert(TimeoutWindow {
            started: now,
            failures: 0,
        });
        if now.saturating_duration_since(window.started) > QMI_SERVICE_TIMEOUT_WINDOW {
            window.started = now;
            window.failures = 0;
        }
        window.failures = window.failures.saturating_add(1);
        if window.failures >= QMI_SERVICE_TIMEOUT_LIMIT {
            self.windows.remove(scope);
            true
        } else {
            false
        }
    }

    fn clear(&mut self, scopes: &[&'static str]) {
        for scope in scopes {
            self.windows.remove(scope);
        }
    }
}

impl QmiSessionFactory for PlatformQmiSessionFactory {
    fn open(&self, control_path: &str) -> Result<Box<dyn QmiSessionOps>, QmiOpenFailure> {
        #[cfg(target_os = "macos")]
        {
            let usb = proletarius_qmi_libusb::open_qmi_client(
                control_path,
                proletarius_qmi_core::ClientOptions::default(),
            )
            .map_err(|error| {
                let cleanup_failure = error
                    .get_ref()
                    .and_then(|source| {
                        source.downcast_ref::<proletarius_qmi_libusb::ProbeCleanupFailure>()
                    })
                    .map(|failure| failure.cleanup_failure.clone());
                QmiOpenFailure {
                    message: format!("QMI USB open failed: {error}"),
                    cleanup_failure,
                }
            })?;
            let ims = match ImsClient::open(&usb.client) {
                Ok(client) => client,
                Err(error) => {
                    return Err(QmiOpenFailure::after_cleanup(
                        format!("QMI IMS service open failed: {error}"),
                        usb.client.close().map_err(|error| error.to_string()),
                    ));
                }
            };
            let nas = match NasClient::open(&usb.client) {
                Ok(client) => client,
                Err(error) => {
                    let failure = QmiOpenFailure::after_cleanup(
                        format!("QMI NAS service open failed: {error}"),
                        usb.client.close().map_err(|error| error.to_string()),
                    );
                    drop(ims);
                    return Err(failure);
                }
            };
            let uim = match UimClient::open(&usb.client) {
                Ok(client) => client,
                Err(error) => {
                    let failure = QmiOpenFailure::after_cleanup(
                        format!("QMI UIM service open failed: {error}"),
                        usb.client.close().map_err(|error| error.to_string()),
                    );
                    drop((ims, nas));
                    return Err(failure);
                }
            };
            return Ok(Box::new(PlatformQmiSession { usb, ims, nas, uim }));
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = control_path;
            Err("Switch QMI USB owner is available only on macOS"
                .to_owned()
                .into())
        }
    }
}

#[cfg(target_os = "macos")]
impl QmiSessionOps for PlatformQmiSession {
    fn transport_closed(&self) -> bool {
        self.usb.client.is_closed()
    }
    fn probe(&mut self) -> Result<Value, QmiReadFailure> {
        let settings = self
            .ims
            .get_services()
            .map_err(|error| QmiReadFailure::new("ims-services", "QMI IMS query failed", error))?;
        let registration = self.ims.get_registration_status().map_err(|error| {
            QmiReadFailure::new("imsa-registration", "QMI IMSA query failed", error)
                .after_completed(&["ims-services"])
        })?;
        Ok(json!({
            "settings": {
                "voice_over_lte_enabled": settings.voice_over_lte,
                "video_telephony_enabled": settings.video_telephony,
                "voice_wifi_enabled": settings.voice_wifi,
                "call_mode_preference": settings.call_mode,
                "ims_service_enabled": settings.ims,
                "ut_service_enabled": settings.ut,
                "sms_service_enabled": settings.sms,
                "ussd_service_enabled": settings.ussd,
                "presence_enabled": settings.presence,
                "autoconfig_enabled": settings.autoconfig,
                "xdm_client_enabled": settings.xdm_client,
                "rcs_enabled": settings.rcs,
                "carrier_config_enabled": settings.carrier_config
            },
            "registration": {
                "status": registration.state,
                "error_code": registration.error_code,
                "error_message": registration.error_message,
                "technology": registration.technology
            }
        }))
    }

    fn set_ims(&mut self, update: ImsSettingsUpdate) -> Result<(), QmiServiceClientError> {
        self.ims.set_services(update)
    }

    fn operator(&mut self) -> Result<Value, QmiReadFailure> {
        let home = self.uim.get_native_mcc_mnc().map_err(|error| {
            QmiReadFailure::new(
                "uim-home-network",
                "QMI UIM home network query failed",
                error,
            )
        })?;
        let name = self
            .nas
            .get_operator_name()
            .map(|name| name.preferred_name().to_owned())
            .unwrap_or_default();
        Ok(json!({ "mcc": home.mcc(), "mnc": home.mnc(), "name": name }))
    }

    fn close(&mut self) -> Result<(), String> {
        let results = [
            self.uim.close(),
            self.nas.close(),
            self.ims.close(),
            self.usb.client.close(),
        ];
        let failures = results
            .into_iter()
            .filter_map(Result::err)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn abort(&mut self) -> Result<(), String> {
        // A failed typed request may mean the service clients are already
        // unreachable. Releasing their IDs over that transport is not a
        // prerequisite for recovery, but the claimed USB reader is.
        self.usb.client.close().map_err(|error| error.to_string())
    }
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    qmi: QmiEnhancement,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST {
        return error_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "QMI owner endpoints require POST",
        );
    }
    let action = request
        .uri()
        .path()
        .strip_prefix(PREFIX)
        .unwrap_or("")
        .to_owned();
    let body = match crate::http_server::collect_body(request.into_body(), MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "QMI owner request exceeds its byte limit",
            );
        }
    };
    let input: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                "invalid JSON request",
            );
        }
    };
    let result = tokio::task::spawn_blocking(move || match action.as_str() {
        "open" => match serde_json::from_value::<OpenRequest>(input) {
            Ok(input) => qmi.open(input.control_path),
            Err(_) => Err(invalid_request("invalid QMI open request")),
        },
        "close" => match empty_object(input) {
            Ok(()) => qmi.close(),
            Err(error) => Err(error),
        },
        "probe" => match empty_object(input) {
            Ok(()) => qmi.probe(),
            Err(error) => Err(error),
        },
        "set-ims" => match serde_json::from_value::<SetImsRequest>(input) {
            Ok(input) if input.is_empty() => {
                Err(invalid_request("IMS settings update must include a field"))
            }
            Ok(input) => qmi.set_ims(input.into()),
            Err(_) => Err(invalid_request("invalid IMS settings request")),
        },
        "operator" => match empty_object(input) {
            Ok(()) => qmi.operator(),
            Err(error) => Err(error),
        },
        _ => Err(RpcError {
            code: "route_not_found",
            error: "QMI owner action not found".to_owned(),
        }),
    })
    .await;
    match result {
        Ok(Ok(value)) => json_response(StatusCode::OK, &value),
        Ok(Err(error)) => {
            let status = match error.code {
                "invalid_request" => StatusCode::BAD_REQUEST,
                "qmi_owner_conflict" => StatusCode::CONFLICT,
                "route_not_found" => StatusCode::NOT_FOUND,
                _ => StatusCode::SERVICE_UNAVAILABLE,
            };
            json_response(status, &error)
        }
        Err(_) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "qmi_owner_stopped",
            "QMI owner task failed",
        ),
    }
}

fn empty_object(value: Value) -> Result<(), RpcError> {
    if value.as_object().is_some_and(serde_json::Map::is_empty) {
        Ok(())
    } else {
        Err(invalid_request("expected an empty JSON object"))
    }
}

fn invalid_request(error: impl Into<String>) -> RpcError {
    RpcError {
        code: "invalid_request",
        error: error.into(),
    }
}

fn unavailable(error: impl Into<String>) -> RpcError {
    RpcError {
        code: "qmi_unavailable",
        error: error.into(),
    }
}

fn conflict(error: impl Into<String>) -> RpcError {
    RpcError {
        code: "qmi_owner_conflict",
        error: error.into(),
    }
}

fn error_response(
    status: StatusCode,
    code: &'static str,
    error: &'static str,
) -> Response<Full<Bytes>> {
    json_response(
        status,
        &RpcError {
            code,
            error: error.to_owned(),
        },
    )
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(bytes)))
        .expect("valid QMI response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Clone, Copy)]
    enum FakeOutcome {
        Success,
        TransportLost,
        StaleHandle,
        Timeout,
        Permanent,
    }

    #[derive(Default)]
    struct FakeRuntime {
        opens: usize,
        open_paths: Vec<String>,
        probes: usize,
        operators: usize,
        updates: usize,
        closes: usize,
        aborts: usize,
        abort_failure: bool,
        close_failure: bool,
        transport_closed: bool,
        open_events: Option<std::sync::mpsc::Sender<usize>>,
        close_events: Option<std::sync::mpsc::Sender<usize>>,
        open_failures: VecDeque<bool>,
        probe_outcomes: VecDeque<FakeOutcome>,
        operator_outcomes: VecDeque<FakeOutcome>,
        update_outcomes: VecDeque<FakeOutcome>,
        revoke_on_open: Option<crate::module_recovery::Gate>,
        revoke_on_probe: Option<crate::module_recovery::Gate>,
        revoke_on_update: Option<crate::module_recovery::Gate>,
    }

    #[derive(Clone)]
    struct FakeFactory(Arc<Mutex<FakeRuntime>>);

    struct FakeSession(Arc<Mutex<FakeRuntime>>);

    impl QmiSessionFactory for FakeFactory {
        fn open(&self, control_path: &str) -> Result<Box<dyn QmiSessionOps>, QmiOpenFailure> {
            let mut runtime = self.0.lock().unwrap();
            runtime.opens += 1;
            runtime.open_paths.push(control_path.to_owned());
            if let Some(events) = &runtime.open_events {
                let _ = events.send(runtime.opens);
            }
            if let Some(gate) = runtime.revoke_on_open.take() {
                gate.revoke_for_tests();
            }
            if runtime.open_failures.pop_front().unwrap_or(false) {
                return Err("simulated open failure".to_owned().into());
            }
            Ok(Box::new(FakeSession(self.0.clone())))
        }
    }

    impl QmiSessionOps for FakeSession {
        fn probe(&mut self) -> Result<Value, QmiReadFailure> {
            let mut runtime = self.0.lock().unwrap();
            runtime.probes += 1;
            if let Some(gate) = runtime.revoke_on_probe.take() {
                gate.revoke_for_tests();
            }
            let outcome = runtime
                .probe_outcomes
                .pop_front()
                .unwrap_or(FakeOutcome::Success);
            drop(runtime);
            outcome.read("fake-probe", "QMI fake probe failed")
        }

        fn set_ims(&mut self, _update: ImsSettingsUpdate) -> Result<(), QmiServiceClientError> {
            let mut runtime = self.0.lock().unwrap();
            runtime.updates += 1;
            if let Some(gate) = runtime.revoke_on_update.take() {
                gate.revoke_for_tests();
            }
            runtime
                .update_outcomes
                .pop_front()
                .unwrap_or(FakeOutcome::Success)
                .client_error()
                .map_or(Ok(()), Err)
        }

        fn operator(&mut self) -> Result<Value, QmiReadFailure> {
            let mut runtime = self.0.lock().unwrap();
            runtime.operators += 1;
            let outcome = runtime
                .operator_outcomes
                .pop_front()
                .unwrap_or(FakeOutcome::Success);
            drop(runtime);
            outcome.read("fake-operator", "QMI fake operator failed")
        }

        fn close(&mut self) -> Result<(), String> {
            let mut runtime = self.0.lock().unwrap();
            runtime.closes += 1;
            if let Some(events) = &runtime.close_events {
                let _ = events.send(runtime.closes);
            }
            if runtime.close_failure {
                Err("simulated close failure".to_owned())
            } else {
                Ok(())
            }
        }

        fn abort(&mut self) -> Result<(), String> {
            let mut runtime = self.0.lock().unwrap();
            runtime.aborts += 1;
            if runtime.abort_failure {
                Err("simulated abort USB release unconfirmed".to_owned())
            } else {
                Ok(())
            }
        }

        fn transport_closed(&self) -> bool {
            self.0.lock().unwrap().transport_closed
        }
    }

    impl FakeOutcome {
        fn client_error(self) -> Option<QmiServiceClientError> {
            match self {
                Self::Success => None,
                Self::TransportLost => Some(QmiServiceClientError::Client(ClientError::Closed)),
                Self::StaleHandle => Some(QmiServiceClientError::Client(ClientError::Qmi {
                    service: 0x12,
                    message: 0x0090,
                    result: 1,
                    error: QMI_ERROR_INVALID_ID,
                })),
                Self::Timeout => Some(QmiServiceClientError::Client(ClientError::Timeout)),
                Self::Permanent => Some(QmiServiceClientError::Codec(
                    proletarius_qmi_services::CodecError::MalformedField("fake permanent error"),
                )),
            }
        }

        fn read(self, scope: &'static str, message: &'static str) -> Result<Value, QmiReadFailure> {
            match self.client_error() {
                Some(error) => Err(QmiReadFailure::new(scope, message, error)),
                None => Ok(json!({ "ok": true })),
            }
        }
    }

    fn fake_owner() -> (QmiEnhancement, Arc<Mutex<FakeRuntime>>) {
        let runtime = Arc::new(Mutex::new(FakeRuntime::default()));
        let owner = QmiEnhancement {
            module_recovery: crate::module_recovery::Gate::default(),
            state: Arc::new(Mutex::new(QmiState::default())),
            factory: Arc::new(FakeFactory(runtime.clone())),
        };
        (owner, runtime)
    }

    #[test]
    fn qmi_status_diagnostic_removes_controls_and_has_a_strict_display_bound() {
        assert_eq!(super::bounded_diagnostic("interface\n4", 32), "interface4");
        assert_eq!(super::bounded_diagnostic("abcdefghij", 4), "abcd…");
    }

    #[test]
    fn native_default_at_never_opens_or_closes_qmi() {
        let (owner, runtime) = fake_owner();
        assert_eq!(owner.transport_status(), None);
        let mut lifecycle = NativeQmiLifecycle::start(NativeQmiConfig::At, owner.clone());
        assert_eq!(lifecycle.startup_error(), None);
        lifecycle.shutdown().unwrap();
        drop(lifecycle);
        let runtime = runtime.lock().unwrap();
        assert_eq!(runtime.opens, 0);
        assert_eq!(runtime.closes, 0);
        assert!(owner.state.lock().unwrap().control_path.is_none());
    }

    #[test]
    fn native_qmi_uses_auto_selector_and_shutdown_is_idempotent() {
        let (owner, runtime) = fake_owner();
        assert_eq!(owner.transport_status(), None);
        let mut lifecycle = NativeQmiLifecycle::start(NativeQmiConfig::Qmi, owner.clone());
        assert_eq!(lifecycle.startup_error(), None);
        assert_eq!(runtime.lock().unwrap().open_paths, [""]);
        assert_eq!(
            owner.transport_status(),
            Some(proletarius_api_contract::UpstreamQmiStatus {
                enabled: true,
                session_initialized: true,
                initialization_error: None,
            })
        );
        assert!(owner.probe().is_ok());
        lifecycle.shutdown().unwrap();
        lifecycle.shutdown().unwrap();
        drop(lifecycle);
        assert_eq!(
            owner.transport_status(),
            Some(proletarius_api_contract::UpstreamQmiStatus {
                enabled: true,
                session_initialized: false,
                initialization_error: None,
            })
        );
        assert_eq!(runtime.lock().unwrap().closes, 1);
        assert_eq!(owner.probe().unwrap_err().code, "qmi_unavailable");
        assert_eq!(runtime.lock().unwrap().opens, 1);
    }

    #[test]
    fn native_qmi_startup_failure_is_optional_and_clears_recovery_path() {
        let (owner, runtime) = fake_owner();
        runtime.lock().unwrap().open_failures.push_back(true);
        let mut lifecycle = NativeQmiLifecycle::start(NativeQmiConfig::Qmi, owner.clone());
        assert!(
            lifecycle
                .startup_error()
                .unwrap()
                .contains("simulated open failure")
        );
        let state = owner.state.lock().unwrap();
        assert!(state.session.is_none());
        assert!(state.control_path.is_none());
        assert!(state.open_backoff.retry_at.is_none());
        assert_eq!(
            state.last_open_error.as_deref(),
            Some("simulated open failure")
        );
        drop(state);
        assert_eq!(
            owner.transport_status(),
            Some(proletarius_api_contract::UpstreamQmiStatus {
                enabled: true,
                session_initialized: false,
                initialization_error: Some("simulated open failure".to_owned()),
            })
        );
        assert!(owner.probe().is_err());
        assert_eq!(runtime.lock().unwrap().opens, 1);
        lifecycle.shutdown().unwrap();
    }

    fn wait_for_fake_status(owner: &QmiEnhancement, initialized: bool, error: bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(status) = owner.transport_status()
                && status.session_initialized == initialized
                && status.initialization_error.is_some() == error
            {
                return;
            }
            std::thread::yield_now();
        }
        panic!("QMI fake status did not reach initialized={initialized}, error={error}");
    }

    #[test]
    fn native_qmi_retry_handles_late_attach_reader_disconnect_and_shutdown() {
        use std::sync::mpsc;
        let (owner, runtime) = fake_owner();
        let (opened, open_events) = mpsc::channel();
        let (closed, close_events) = mpsc::channel();
        {
            let mut fake = runtime.lock().unwrap();
            fake.open_failures.push_back(true);
            fake.open_events = Some(opened);
            fake.close_events = Some(closed);
        }
        let mut retry = NativeQmiRetryLifecycle::start_with_timing(
            NativeQmiConfig::Qmi,
            owner.clone(),
            Duration::from_millis(100),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        )
        .unwrap();
        assert_eq!(open_events.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        wait_for_fake_status(&owner, false, true);
        retry.recheck_for_tests();
        assert_eq!(open_events.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        wait_for_fake_status(&owner, true, false);
        runtime.lock().unwrap().transport_closed = true;
        retry.recheck_for_tests();
        assert_eq!(
            close_events.recv_timeout(Duration::from_secs(2)).unwrap(),
            1
        );
        wait_for_fake_status(&owner, false, true);
        runtime.lock().unwrap().transport_closed = false;
        retry.recheck_for_tests();
        assert_eq!(open_events.recv_timeout(Duration::from_secs(2)).unwrap(), 3);
        wait_for_fake_status(&owner, true, false);
        retry.shutdown().unwrap();
        assert_eq!(
            close_events.recv_timeout(Duration::from_secs(2)).unwrap(),
            2
        );
        assert_eq!(runtime.lock().unwrap().opens, 3);
        assert_eq!(runtime.lock().unwrap().closes, 2);
    }

    #[test]
    fn native_qmi_retry_private_close_stays_closed_until_private_open() {
        use std::sync::mpsc;
        let (owner, runtime) = fake_owner();
        let (opened, open_events) = mpsc::channel();
        let (closed, close_events) = mpsc::channel();
        {
            let mut fake = runtime.lock().unwrap();
            fake.open_events = Some(opened);
            fake.close_events = Some(closed);
        }
        let mut retry = NativeQmiRetryLifecycle::start_with_timing(
            NativeQmiConfig::Qmi,
            owner.clone(),
            Duration::from_millis(100),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        )
        .unwrap();
        assert_eq!(open_events.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        retry.wait_for_connected_for_tests(1);
        owner.close().unwrap();
        assert_eq!(
            close_events.recv_timeout(Duration::from_secs(2)).unwrap(),
            1
        );
        assert_eq!(
            owner
                .transport_status()
                .unwrap()
                .initialization_error
                .as_deref(),
            Some("QMI enhancement closed by private request")
        );
        retry.recheck_for_tests();
        retry.wait_for_paused_for_tests(1);
        retry.recheck_for_tests();
        retry.wait_for_paused_for_tests(2);
        assert_eq!(runtime.lock().unwrap().opens, 1);
        assert!(!owner.transport_status().unwrap().session_initialized);

        owner.open(String::new()).unwrap();
        assert_eq!(open_events.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        retry.recheck_for_tests();
        retry.wait_for_connected_for_tests(2);
        assert_eq!(runtime.lock().unwrap().opens, 2);
        assert_eq!(owner.transport_status().unwrap().initialization_error, None);
        retry.shutdown().unwrap();
        assert_eq!(
            close_events.recv_timeout(Duration::from_secs(2)).unwrap(),
            2
        );
    }

    #[test]
    fn native_qmi_retry_private_open_failure_reauthorizes_late_attach() {
        let (owner, runtime) = fake_owner();
        let mut retry = NativeQmiRetryLifecycle::start_with_timing(
            NativeQmiConfig::Qmi,
            owner.clone(),
            Duration::from_millis(100),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        )
        .unwrap();
        retry.wait_for_connected_for_tests(1);
        owner.close().unwrap();
        retry.recheck_for_tests();
        retry.wait_for_paused_for_tests(1);
        runtime.lock().unwrap().open_failures.push_back(true);
        assert!(owner.open(String::new()).is_err());
        assert!(owner.auto_reconnect_desired().unwrap());
        // Advance only the fake owner's application backoff, not wall time.
        owner.state.lock().unwrap().open_backoff.retry_at = None;
        retry.recheck_for_tests();
        retry.wait_for_connected_for_tests(2);
        assert_eq!(runtime.lock().unwrap().opens, 3);
        retry.shutdown().unwrap();
    }

    #[test]
    fn native_qmi_retry_keeps_private_exact_selector_after_reader_loss() {
        use std::sync::mpsc;
        let (owner, runtime) = fake_owner();
        let (opened, open_events) = mpsc::channel();
        let (closed, close_events) = mpsc::channel();
        {
            let mut fake = runtime.lock().unwrap();
            fake.open_events = Some(opened);
            fake.close_events = Some(closed);
        }
        let mut retry = NativeQmiRetryLifecycle::start_with_timing(
            NativeQmiConfig::Qmi,
            owner.clone(),
            Duration::from_millis(100),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        )
        .unwrap();
        assert_eq!(open_events.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        retry.wait_for_connected_for_tests(1);
        owner.close().unwrap();
        assert_eq!(
            close_events.recv_timeout(Duration::from_secs(2)).unwrap(),
            1
        );
        retry.recheck_for_tests();
        retry.wait_for_paused_for_tests(1);
        owner.open("2ca3:4006".to_owned()).unwrap();
        assert_eq!(open_events.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        retry.recheck_for_tests();
        retry.wait_for_connected_for_tests(2);

        runtime.lock().unwrap().transport_closed = true;
        retry.recheck_for_tests();
        assert_eq!(
            close_events.recv_timeout(Duration::from_secs(2)).unwrap(),
            2
        );
        runtime.lock().unwrap().transport_closed = false;
        retry.recheck_for_tests();
        assert_eq!(open_events.recv_timeout(Duration::from_secs(2)).unwrap(), 3);
        retry.wait_for_connected_for_tests(3);
        assert_eq!(
            runtime.lock().unwrap().open_paths,
            vec!["", "2ca3:4006", "2ca3:4006"]
        );
        retry.shutdown().unwrap();
    }

    #[test]
    fn native_qmi_retry_private_open_cannot_clear_unconfirmed_close() {
        let (owner, runtime) = fake_owner();
        let mut retry = NativeQmiRetryLifecycle::start_with_timing(
            NativeQmiConfig::Qmi,
            owner.clone(),
            Duration::from_millis(100),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        )
        .unwrap();
        retry.wait_for_connected_for_tests(1);
        runtime.lock().unwrap().close_failure = true;
        assert!(
            owner
                .close()
                .unwrap_err()
                .error
                .contains("simulated close failure")
        );
        assert!(owner.open(String::new()).is_err());
        assert!(!owner.auto_reconnect_desired().unwrap());
        assert_eq!(runtime.lock().unwrap().opens, 1);
        assert!(
            retry
                .shutdown()
                .unwrap_err()
                .contains("simulated close failure")
        );
    }

    #[test]
    fn native_qmi_retry_private_close_during_open_fences_stale_handoff() {
        use std::sync::mpsc;
        struct DelayedFactory {
            entered: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
            inner: FakeFactory,
        }
        impl QmiSessionFactory for DelayedFactory {
            fn open(&self, path: &str) -> Result<Box<dyn QmiSessionOps>, QmiOpenFailure> {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                self.inner.open(path)
            }
        }
        let (entered, attempt) = mpsc::channel();
        let (release, unblock) = mpsc::channel();
        let (closing, close_started) = mpsc::channel();
        let runtime = Arc::new(Mutex::new(FakeRuntime::default()));
        let owner = QmiEnhancement {
            module_recovery: crate::module_recovery::Gate::default(),
            state: Arc::new(Mutex::new(QmiState::default())),
            factory: Arc::new(DelayedFactory {
                entered,
                release: Mutex::new(unblock),
                inner: FakeFactory(runtime.clone()),
            }),
        };
        let mut retry = NativeQmiRetryLifecycle::start_with_timing(
            NativeQmiConfig::Qmi,
            owner.clone(),
            Duration::from_secs(2),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        )
        .unwrap();
        attempt.recv_timeout(Duration::from_secs(2)).unwrap();
        let closer = std::thread::spawn({
            let owner = owner.clone();
            move || {
                closing.send(()).unwrap();
                owner.close()
            }
        });
        close_started.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
        closer.join().unwrap().unwrap();
        retry.recheck_for_tests();
        retry.wait_for_paused_for_tests(1);
        assert_eq!(runtime.lock().unwrap().opens, 1);
        assert_eq!(runtime.lock().unwrap().closes, 1);
        retry.shutdown().unwrap();
        assert_eq!(runtime.lock().unwrap().opens, 1);
    }

    #[test]
    fn native_qmi_retry_stop_during_late_open_closes_once_and_never_reopens() {
        use std::sync::mpsc;
        struct DelayedFactory {
            entered: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
            inner: FakeFactory,
        }
        impl QmiSessionFactory for DelayedFactory {
            fn open(&self, path: &str) -> Result<Box<dyn QmiSessionOps>, QmiOpenFailure> {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                self.inner.open(path)
            }
        }
        let (entered, attempt) = mpsc::channel();
        let (release, unblock) = mpsc::channel();
        let runtime = Arc::new(Mutex::new(FakeRuntime::default()));
        let owner = QmiEnhancement {
            module_recovery: crate::module_recovery::Gate::default(),
            state: Arc::new(Mutex::new(QmiState::default())),
            factory: Arc::new(DelayedFactory {
                entered,
                release: Mutex::new(unblock),
                inner: FakeFactory(runtime.clone()),
            }),
        };
        let retry = NativeQmiRetryLifecycle::start_with_timing(
            NativeQmiConfig::Qmi,
            owner.clone(),
            Duration::from_millis(5),
            Duration::from_millis(2),
            Duration::from_millis(8),
            Duration::from_millis(2),
        )
        .unwrap();
        attempt.recv_timeout(Duration::from_secs(2)).unwrap();
        let shutdown = std::thread::spawn(move || {
            let mut retry = retry;
            retry.shutdown()
        });
        release.send(()).unwrap();
        shutdown.join().unwrap().unwrap();
        assert_eq!(runtime.lock().unwrap().opens, 1);
        assert_eq!(runtime.lock().unwrap().closes, 1);
        assert!(!owner.transport_status().unwrap().session_initialized);
    }

    #[test]
    fn native_qmi_retry_never_reopens_after_unconfirmed_probe_cleanup() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };
        struct FailedCleanupFactory {
            calls: Arc<AtomicUsize>,
            entered: mpsc::Sender<()>,
        }
        impl QmiSessionFactory for FailedCleanupFactory {
            fn open(&self, _: &str) -> Result<Box<dyn QmiSessionOps>, QmiOpenFailure> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.entered.send(()).unwrap();
                Err(QmiOpenFailure::after_cleanup(
                    "injected CTL_SYNC failure".to_owned(),
                    Err("injected USB release unconfirmed".to_owned()),
                ))
            }
        }
        let (entered, attempt) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = QmiEnhancement {
            module_recovery: crate::module_recovery::Gate::default(),
            state: Arc::new(Mutex::new(QmiState::default())),
            factory: Arc::new(FailedCleanupFactory {
                calls: calls.clone(),
                entered,
            }),
        };
        let mut retry = NativeQmiRetryLifecycle::start_with_timing(
            NativeQmiConfig::Qmi,
            owner.clone(),
            Duration::from_millis(100),
            Duration::from_millis(1),
            Duration::from_millis(2),
            Duration::from_millis(1),
        )
        .unwrap();
        attempt.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            retry
                .shutdown()
                .unwrap_err()
                .contains("injected USB release unconfirmed")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            owner
                .transport_status()
                .unwrap()
                .initialization_error
                .unwrap()
                .contains("injected USB release unconfirmed")
        );
    }

    #[test]
    fn failed_service_startup_keeps_unconfirmed_cleanup_in_final_receipt() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct FailedServiceFactory {
            service: &'static str,
            fail_cleanup: bool,
            opens: Arc<AtomicUsize>,
        }
        impl QmiSessionFactory for FailedServiceFactory {
            fn open(&self, _: &str) -> Result<Box<dyn QmiSessionOps>, QmiOpenFailure> {
                self.opens.fetch_add(1, Ordering::SeqCst);
                Err(QmiOpenFailure::after_cleanup(
                    format!(
                        "QMI {} service open failed: injected allocation failure",
                        self.service
                    ),
                    if self.fail_cleanup {
                        Err("injected USB cleanup unconfirmed".to_owned())
                    } else {
                        Ok(())
                    },
                ))
            }
        }
        for service in ["IMS", "NAS", "UIM"] {
            for fail_cleanup in [false, true] {
                let opens = Arc::new(AtomicUsize::new(0));
                let owner = QmiEnhancement {
                    module_recovery: crate::module_recovery::Gate::default(),
                    state: Arc::new(Mutex::new(QmiState::default())),
                    factory: Arc::new(FailedServiceFactory {
                        service,
                        fail_cleanup,
                        opens: opens.clone(),
                    }),
                };
                let mut lifecycle = NativeQmiLifecycle::start(NativeQmiConfig::Qmi, owner.clone());
                assert!(
                    lifecycle
                        .startup_error()
                        .unwrap()
                        .contains("injected allocation failure")
                );
                let receipt = lifecycle.shutdown();
                assert_eq!(receipt.is_err(), fail_cleanup);
                assert_eq!(lifecycle.shutdown(), receipt);
                assert_eq!(owner.state.lock().unwrap().close().is_err(), fail_cleanup);
                if fail_cleanup {
                    assert!(
                        receipt
                            .unwrap_err()
                            .contains("injected USB cleanup unconfirmed")
                    );
                    assert!(
                        owner
                            .open(String::new())
                            .unwrap_err()
                            .error
                            .contains("prior cleanup remains unconfirmed")
                    );
                    assert_eq!(
                        opens.load(Ordering::SeqCst),
                        1,
                        "unknown startup cleanup must fence reopening"
                    );
                }
            }
        }
    }

    #[test]
    fn native_qmi_guard_drop_releases_session_even_without_explicit_shutdown() {
        let (owner, runtime) = fake_owner();
        let lifecycle = NativeQmiLifecycle::start(NativeQmiConfig::Qmi, owner.clone());
        assert!(lifecycle.startup_error().is_none());
        drop(lifecycle);
        assert_eq!(runtime.lock().unwrap().closes, 1);
        assert!(owner.state.lock().unwrap().control_path.is_none());
    }

    #[test]
    fn native_qmi_shutdown_failure_still_forgets_owner_and_never_replays_close() {
        let (owner, runtime) = fake_owner();
        let mut lifecycle = NativeQmiLifecycle::start(NativeQmiConfig::Qmi, owner.clone());
        runtime.lock().unwrap().close_failure = true;
        let first_error = lifecycle.shutdown().unwrap_err();
        assert!(first_error.contains("simulated close failure"));
        assert_eq!(lifecycle.shutdown().unwrap_err(), first_error);
        assert!(owner.state.lock().unwrap().session.is_none());
        assert!(owner.state.lock().unwrap().control_path.is_none());
        assert_eq!(runtime.lock().unwrap().closes, 1);
    }

    #[test]
    fn native_qmi_late_open_after_deadline_is_closed_and_joined() {
        use std::sync::mpsc;

        struct DelayedFactory {
            release: Mutex<mpsc::Receiver<()>>,
            inner: FakeFactory,
        }
        impl QmiSessionFactory for DelayedFactory {
            fn open(&self, path: &str) -> Result<Box<dyn QmiSessionOps>, QmiOpenFailure> {
                self.release.lock().unwrap().recv().unwrap();
                self.inner.open(path)
            }
        }
        for close_failure in [false, true] {
            let (release, wait) = mpsc::channel();
            let runtime = Arc::new(Mutex::new(FakeRuntime::default()));
            runtime.lock().unwrap().close_failure = close_failure;
            let owner = QmiEnhancement {
                module_recovery: crate::module_recovery::Gate::default(),
                state: Arc::new(Mutex::new(QmiState::default())),
                factory: Arc::new(DelayedFactory {
                    release: Mutex::new(wait),
                    inner: FakeFactory(runtime.clone()),
                }),
            };
            let mut lifecycle = NativeQmiLifecycle::start_with_timeout(
                NativeQmiConfig::Qmi,
                owner.clone(),
                Duration::from_millis(5),
            );
            assert!(lifecycle.startup_error().unwrap().contains("timed out"));
            release.send(()).unwrap();
            if close_failure {
                let error = lifecycle.shutdown().unwrap_err();
                assert!(error.contains("simulated close failure"));
                assert_eq!(lifecycle.shutdown().unwrap_err(), error);
            } else {
                lifecycle.shutdown().unwrap();
            }
            assert_eq!(runtime.lock().unwrap().opens, 1);
            assert_eq!(runtime.lock().unwrap().closes, 1);
            assert!(owner.state.lock().unwrap().session.is_none());
            assert!(owner.state.lock().unwrap().control_path.is_none());
        }
    }

    #[test]
    fn native_qmi_startup_panic_clears_poisoned_owner_configuration() {
        struct PanicFactory;
        impl QmiSessionFactory for PanicFactory {
            fn open(&self, _path: &str) -> Result<Box<dyn QmiSessionOps>, QmiOpenFailure> {
                panic!("simulated startup panic");
            }
        }
        let owner = QmiEnhancement {
            module_recovery: crate::module_recovery::Gate::default(),
            state: Arc::new(Mutex::new(QmiState::default())),
            factory: Arc::new(PanicFactory),
        };
        let mut lifecycle = NativeQmiLifecycle::start(NativeQmiConfig::Qmi, owner.clone());
        assert_eq!(
            lifecycle.startup_error(),
            Some("QMI startup worker stopped")
        );
        assert!(lifecycle.shutdown().unwrap_err().contains("panicked"));
        let state = owner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(state.control_path.is_none());
        assert!(state.session.is_none());
    }

    #[test]
    fn native_qmi_shutdown_releases_existing_session_after_owner_poison() {
        let (owner, runtime) = fake_owner();
        let mut lifecycle = NativeQmiLifecycle::start(NativeQmiConfig::Qmi, owner.clone());
        let poisoned_owner = owner.clone();
        let _ = std::thread::spawn(move || {
            let _locked = poisoned_owner.state.lock().unwrap();
            panic!("simulated owner operation panic");
        })
        .join();
        lifecycle.shutdown().unwrap();
        assert_eq!(runtime.lock().unwrap().closes, 1);
        let state = owner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(state.control_path.is_none());
        assert!(state.session.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_qmi_guard_dropped_on_async_runtime_retains_blocking_cleanup() {
        let (owner, runtime) = fake_owner();
        let open_owner = owner.clone();
        let lifecycle = tokio::task::spawn_blocking(move || {
            NativeQmiLifecycle::start(NativeQmiConfig::Qmi, open_owner)
        })
        .await
        .unwrap();
        drop(lifecycle);
        tokio::time::timeout(Duration::from_secs(2), async {
            while runtime.lock().unwrap().closes != 1 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(owner.state.lock().unwrap().session.is_none());
        assert!(owner.state.lock().unwrap().control_path.is_none());
    }

    #[test]
    fn update_dto_preserves_explicit_false_and_partial_updates() {
        let request: SetImsRequest =
            serde_json::from_str(r#"{"voice_wifi":false,"sms":true,"call_mode":2}"#).unwrap();
        let update: ImsSettingsUpdate = request.into();
        assert_eq!(update.voice_wifi, Some(false));
        assert_eq!(update.sms, Some(true));
        assert_eq!(update.call_mode, Some(2));
        assert_eq!(update.ims, None);
    }

    #[test]
    fn empty_ims_update_is_rejected_before_device_access() {
        let request: SetImsRequest = serde_json::from_str("{}").unwrap();
        assert!(request.is_empty());
        let (owner, runtime) = fake_owner();
        assert!(owner.state.lock().unwrap().session.is_none());
        assert_eq!(runtime.lock().unwrap().opens, 0);
    }

    #[test]
    fn private_actions_reject_unknown_fields_and_nonempty_empty_action_bodies() {
        assert!(
            serde_json::from_str::<SetImsRequest>(r#"{"voice_wifi":false,"raw_frame":"00"}"#)
                .is_err()
        );
        assert!(empty_object(json!({"control_path":"/dev/anything"})).is_err());
        assert!(empty_object(json!({})).is_ok());
    }

    #[test]
    fn open_rejects_invalid_path_before_usb_discovery() {
        let (owner, runtime) = fake_owner();
        let error = owner.open("invalid\0path".to_owned()).unwrap_err();
        assert_eq!(error.code, "invalid_request");
        assert!(owner.state.lock().unwrap().session.is_none());
        assert_eq!(runtime.lock().unwrap().opens, 0);
    }

    #[test]
    fn read_transport_loss_reopens_owner_and_retries_once() {
        let (owner, runtime) = fake_owner();
        runtime
            .lock()
            .unwrap()
            .probe_outcomes
            .extend([FakeOutcome::TransportLost, FakeOutcome::Success]);
        owner.open("auto".to_owned()).unwrap();

        assert_eq!(owner.probe().unwrap(), json!({ "ok": true }));
        let runtime = runtime.lock().unwrap();
        assert_eq!(runtime.opens, 2);
        assert_eq!(runtime.probes, 2);
        assert_eq!(runtime.aborts, 1);
    }

    #[test]
    fn read_recovery_never_reopens_after_unconfirmed_usb_abort() {
        let (owner, runtime) = fake_owner();
        {
            let mut runtime = runtime.lock().unwrap();
            runtime.probe_outcomes.push_back(FakeOutcome::TransportLost);
            runtime.abort_failure = true;
        }
        owner.open("auto".to_owned()).unwrap();

        let failure = owner.probe().unwrap_err();
        assert_eq!(failure.code, "qmi_unavailable");
        assert!(
            failure
                .error
                .contains("simulated abort USB release unconfirmed")
        );
        assert!(
            owner
                .probe()
                .unwrap_err()
                .error
                .contains("prior cleanup remains unconfirmed")
        );
        assert!(owner.open("auto".to_owned()).is_err());
        let runtime = runtime.lock().unwrap();
        assert_eq!((runtime.opens, runtime.probes, runtime.aborts), (1, 1, 1));
        let state = owner.state.lock().unwrap();
        assert!(state.session.is_none());
        assert_eq!(state.control_path.as_deref(), Some("auto"));
        assert!(
            state
                .cleanup_failure
                .as_deref()
                .unwrap()
                .contains("simulated abort USB release unconfirmed")
        );
    }

    #[test]
    fn active_qmi_retry_preserves_abort_failure_in_status_and_shutdown() {
        let (owner, runtime) = fake_owner();
        let mut retry = NativeQmiRetryLifecycle::start_with_timing(
            NativeQmiConfig::Qmi,
            owner.clone(),
            Duration::from_millis(100),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        )
        .unwrap();
        retry.wait_for_connected_for_tests(1);
        let status = owner.transport_status().unwrap();
        assert!(status.enabled && status.session_initialized);
        assert_eq!(status.initialization_error, None);
        {
            let mut fake = runtime.lock().unwrap();
            fake.probe_outcomes.push_back(FakeOutcome::TransportLost);
            fake.abort_failure = true;
        }

        let failure = owner.probe().unwrap_err();
        let cleanup_error = "simulated abort USB release unconfirmed";
        assert!(failure.error.contains(cleanup_error));
        let status = owner.transport_status().unwrap();
        assert!(status.enabled && !status.session_initialized);
        assert!(
            status
                .initialization_error
                .as_deref()
                .unwrap()
                .contains(cleanup_error)
        );
        {
            let state = owner.state.lock().unwrap();
            assert_eq!(state.control_path.as_deref(), Some(""));
            assert_eq!(state.last_open_error, None);
        }

        // Wake the retained worker rather than waiting for its hour-long
        // reader-check timer. Its terminal reporting is a durable receipt
        // that it processed the debt before this test requests shutdown.
        retry.recheck_for_tests();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = owner.state.lock().unwrap();
            let reported = state.control_path.is_none()
                && state
                    .last_open_error
                    .as_deref()
                    .is_some_and(|error| error.contains(cleanup_error));
            drop(state);
            if reported {
                break;
            }
            assert!(Instant::now() < deadline, "retry did not report abort debt");
            std::thread::yield_now();
        }

        assert!(owner.close().unwrap_err().error.contains(cleanup_error));
        assert!(
            owner
                .open(String::new())
                .unwrap_err()
                .error
                .contains(cleanup_error)
        );
        let status = owner.transport_status().unwrap();
        assert!(status.enabled && !status.session_initialized);
        assert!(status.initialization_error.unwrap().contains(cleanup_error));
        let shutdown_error = retry.shutdown().unwrap_err();
        assert!(shutdown_error.contains(cleanup_error));
        assert_eq!(retry.shutdown().unwrap_err(), shutdown_error);
        drop(retry);
        {
            let fake = runtime.lock().unwrap();
            assert_eq!(
                (fake.opens, fake.probes, fake.aborts, fake.closes),
                (1, 1, 1, 0)
            );
        }
        let state = owner.state.lock().unwrap();
        assert!(state.session.is_none());
        assert!(state.explicitly_closed);
        assert!(
            state
                .cleanup_failure
                .as_deref()
                .unwrap()
                .contains(cleanup_error)
        );
    }

    #[test]
    fn service_timeouts_recover_only_after_three_in_sixty_seconds() {
        let (owner, runtime) = fake_owner();
        runtime.lock().unwrap().probe_outcomes.extend([
            FakeOutcome::Timeout,
            FakeOutcome::Timeout,
            FakeOutcome::Timeout,
            FakeOutcome::Success,
        ]);
        owner.open("auto".to_owned()).unwrap();

        assert_eq!(owner.probe().unwrap_err().code, "qmi_unavailable");
        assert_eq!(owner.probe().unwrap_err().code, "qmi_unavailable");
        assert_eq!(runtime.lock().unwrap().opens, 1);
        assert_eq!(owner.probe().unwrap(), json!({ "ok": true }));

        let runtime = runtime.lock().unwrap();
        assert_eq!(runtime.opens, 2);
        assert_eq!(runtime.probes, 4);
        assert_eq!(runtime.aborts, 1);
    }

    #[test]
    fn permanent_read_errors_do_not_reopen_the_owner() {
        let (owner, runtime) = fake_owner();
        runtime
            .lock()
            .unwrap()
            .probe_outcomes
            .push_back(FakeOutcome::Permanent);
        owner.open("auto".to_owned()).unwrap();

        let error = owner.probe().unwrap_err();
        assert!(error.error.contains("QMI fake probe failed"));
        let runtime = runtime.lock().unwrap();
        assert_eq!(runtime.opens, 1);
        assert_eq!(runtime.probes, 1);
        assert_eq!(runtime.aborts, 0);
    }

    #[test]
    fn uncertain_ims_write_is_never_replayed_during_recovery() {
        let (owner, runtime) = fake_owner();
        runtime
            .lock()
            .unwrap()
            .update_outcomes
            .extend([FakeOutcome::TransportLost, FakeOutcome::Success]);
        owner.open("auto".to_owned()).unwrap();

        let update = ImsSettingsUpdate {
            voice_wifi: Some(false),
            ..ImsSettingsUpdate::default()
        };
        assert_eq!(owner.set_ims(update).unwrap_err().code, "qmi_unavailable");
        {
            let runtime = runtime.lock().unwrap();
            assert_eq!(runtime.opens, 2);
            assert_eq!(runtime.updates, 1);
            assert_eq!(runtime.aborts, 1);
        }
        owner.set_ims(update).unwrap();
        assert_eq!(runtime.lock().unwrap().updates, 2);
    }

    #[test]
    fn uncertain_ims_write_with_unconfirmed_abort_is_not_replayed_or_reopened() {
        let (owner, runtime) = fake_owner();
        {
            let mut runtime = runtime.lock().unwrap();
            runtime
                .update_outcomes
                .push_back(FakeOutcome::TransportLost);
            runtime.abort_failure = true;
        }
        owner.open("auto".to_owned()).unwrap();

        let update = ImsSettingsUpdate {
            voice_wifi: Some(false),
            ..ImsSettingsUpdate::default()
        };
        let failure = owner.set_ims(update).unwrap_err();
        assert_eq!(failure.code, "qmi_unavailable");
        assert!(
            failure
                .error
                .contains("simulated abort USB release unconfirmed")
        );
        assert!(
            owner
                .set_ims(update)
                .unwrap_err()
                .error
                .contains("prior cleanup remains unconfirmed")
        );
        let runtime = runtime.lock().unwrap();
        assert_eq!((runtime.opens, runtime.updates, runtime.aborts), (1, 1, 1));
        let state = owner.state.lock().unwrap();
        assert!(state.session.is_none());
        assert!(state.cleanup_failure.is_some());
    }

    #[test]
    fn failed_reopen_keeps_the_selected_path_for_a_later_read() {
        let (owner, runtime) = fake_owner();
        {
            let mut runtime = runtime.lock().unwrap();
            runtime.open_failures.extend([false, true, false]);
            runtime
                .probe_outcomes
                .extend([FakeOutcome::TransportLost, FakeOutcome::Success]);
        }
        owner.open("auto".to_owned()).unwrap();

        let failed = owner.probe().unwrap_err();
        assert!(failed.error.contains("QMI owner recovery failed"));
        {
            let mut state = owner.state.lock().unwrap();
            assert!(state.session.is_none());
            assert_eq!(state.control_path.as_deref(), Some("auto"));
            assert!(
                state
                    .open_backoff
                    .remaining(Instant::now())
                    .is_some_and(|remaining| !remaining.is_zero())
            );
            state.open_backoff.retry_at = Some(Instant::now() - Duration::from_millis(1));
        }
        assert_eq!(owner.probe().unwrap(), json!({ "ok": true }));
        assert_eq!(
            owner.state.lock().unwrap().control_path.as_deref(),
            Some("auto")
        );
        assert_eq!(runtime.lock().unwrap().opens, 3);
    }

    #[test]
    fn failed_initial_open_keeps_path_and_backs_off_before_retry() {
        let (owner, runtime) = fake_owner();
        runtime.lock().unwrap().open_failures.push_back(true);

        let failed = owner.open("auto".to_owned()).unwrap_err();
        assert!(failed.error.contains("next attempt after 500 ms"));
        {
            let state = owner.state.lock().unwrap();
            assert!(state.session.is_none());
            assert_eq!(state.control_path.as_deref(), Some("auto"));
            assert!(state.open_backoff.remaining(Instant::now()).is_some());
        }
        assert!(owner.probe().unwrap_err().error.contains("backoff active"));
        assert_eq!(runtime.lock().unwrap().opens, 1);

        {
            let mut state = owner.state.lock().unwrap();
            state.open_backoff.retry_at = Some(Instant::now() - Duration::from_millis(1));
        }
        assert_eq!(owner.probe().unwrap(), json!({ "ok": true }));
        assert_eq!(runtime.lock().unwrap().opens, 2);
    }

    #[test]
    fn open_backoff_grows_exponentially_and_caps_at_thirty_seconds() {
        let start = Instant::now();
        let mut backoff = OpenBackoff::default();
        assert_eq!(backoff.failed(start), Duration::from_millis(500));
        assert_eq!(backoff.remaining(start), Some(Duration::from_millis(500)));
        assert_eq!(
            backoff.failed(start + Duration::from_millis(500)),
            Duration::from_secs(1)
        );
        assert_eq!(
            backoff.failed(start + Duration::from_secs(2)),
            Duration::from_secs(2)
        );
        for step in 0..6 {
            backoff.failed(start + Duration::from_secs(4 + step));
        }
        assert_eq!(
            backoff.failed(start + Duration::from_secs(20)),
            QMI_RECOVERY_MAX_DELAY
        );
        backoff.succeeded();
        assert_eq!(backoff.remaining(start + Duration::from_secs(21)), None);
    }

    #[test]
    fn explicit_close_forgets_path_and_does_not_reopen_implicitly() {
        let (owner, runtime) = fake_owner();
        owner.open("auto".to_owned()).unwrap();
        owner.close().unwrap();
        assert_eq!(runtime.lock().unwrap().closes, 1);

        assert_eq!(owner.probe().unwrap_err().code, "qmi_unavailable");
        assert_eq!(runtime.lock().unwrap().opens, 1);
    }

    #[test]
    fn qmi_recovery_classifier_only_selects_transport_handle_or_repeated_timeout() {
        assert_eq!(
            classify_qmi_failure(&QmiServiceClientError::Client(ClientError::Closed)),
            RecoveryClass::TransportLost
        );
        assert_eq!(
            classify_qmi_failure(&FakeOutcome::StaleHandle.client_error().unwrap()),
            RecoveryClass::ServiceHandleLost
        );
        assert_eq!(
            classify_qmi_failure(&QmiServiceClientError::Client(ClientError::Timeout)),
            RecoveryClass::Timeout
        );
        assert_eq!(
            classify_qmi_failure(&FakeOutcome::Permanent.client_error().unwrap()),
            RecoveryClass::Permanent
        );
    }

    #[test]
    fn timeout_window_resets_after_sixty_seconds_and_success_clears_scope() {
        let start = Instant::now();
        let mut windows = TimeoutWindows::default();
        assert!(!windows.timeout_recovery_due("uim", start));
        assert!(!windows.timeout_recovery_due("uim", start + Duration::from_secs(30)));
        assert!(!windows.timeout_recovery_due("uim", start + Duration::from_secs(61)));
        assert!(!windows.timeout_recovery_due("uim", start + Duration::from_secs(62)));
        assert!(windows.timeout_recovery_due("uim", start + Duration::from_secs(63)));
        assert!(!windows.timeout_recovery_due("uim", start + Duration::from_secs(64)));
        windows.clear(&["uim"]);
        assert!(!windows.timeout_recovery_due("uim", start + Duration::from_secs(65)));
    }

    #[test]
    fn revoked_qmi_owner_rejects_every_typed_request_before_factory_or_session() {
        let (owner, runtime) = fake_owner();
        let gate = crate::module_recovery::Gate::default();
        let owner = owner.with_module_recovery(gate.clone());
        gate.revoke_for_tests();
        for error in [
            owner.open("auto".into()).unwrap_err(),
            owner.probe().unwrap_err(),
            owner.operator().unwrap_err(),
            owner.set_ims(ImsSettingsUpdate::default()).unwrap_err(),
        ] {
            assert_eq!(error.code, crate::module_recovery::CODE);
        }
        owner.close().unwrap();
        let runtime = runtime.lock().unwrap();
        assert_eq!(
            (
                runtime.opens,
                runtime.probes,
                runtime.operators,
                runtime.updates,
                runtime.closes
            ),
            (0, 0, 0, 0, 0)
        );
    }

    #[test]
    fn existing_qmi_session_cannot_admit_new_typed_work_after_revoke_and_close_remains_idempotent()
    {
        let (owner, runtime) = fake_owner();
        let gate = crate::module_recovery::Gate::default();
        let owner = owner.with_module_recovery(gate.clone());
        owner.open("auto".into()).unwrap();
        gate.revoke_for_tests();
        assert_eq!(
            owner.open("auto".into()).unwrap_err().code,
            crate::module_recovery::CODE
        );
        assert_eq!(
            owner.probe().unwrap_err().code,
            crate::module_recovery::CODE
        );
        assert_eq!(
            owner.operator().unwrap_err().code,
            crate::module_recovery::CODE
        );
        assert_eq!(
            owner
                .set_ims(ImsSettingsUpdate::default())
                .unwrap_err()
                .code,
            crate::module_recovery::CODE
        );
        owner.close().unwrap();
        owner.close().unwrap();
        let runtime = runtime.lock().unwrap();
        assert_eq!(
            (
                runtime.opens,
                runtime.probes,
                runtime.operators,
                runtime.updates,
                runtime.closes
            ),
            (1, 0, 0, 0, 1)
        );
        assert!(owner.state.lock().unwrap().control_path.is_none());
    }

    #[test]
    fn accepted_qmi_probe_keeps_its_result_but_revoke_blocks_following_work() {
        let (owner, runtime) = fake_owner();
        let gate = crate::module_recovery::Gate::default();
        let owner = owner.with_module_recovery(gate.clone());
        owner.open("auto".into()).unwrap();
        runtime.lock().unwrap().revoke_on_probe = Some(gate);
        assert_eq!(owner.probe().unwrap(), json!({ "ok": true }));
        assert_eq!(
            owner.probe().unwrap_err().code,
            crate::module_recovery::CODE
        );
        owner.close().unwrap();
        let runtime = runtime.lock().unwrap();
        assert_eq!((runtime.opens, runtime.probes, runtime.closes), (1, 1, 1));
    }

    #[test]
    fn revoke_during_failed_qmi_request_prevents_recovery_open_and_mutation_replay() {
        for mutation in [false, true] {
            let (owner, runtime) = fake_owner();
            let gate = crate::module_recovery::Gate::default();
            let owner = owner.with_module_recovery(gate.clone());
            owner.open("auto".into()).unwrap();
            {
                let mut runtime = runtime.lock().unwrap();
                if mutation {
                    runtime.revoke_on_update = Some(gate);
                    runtime
                        .update_outcomes
                        .push_back(FakeOutcome::TransportLost);
                } else {
                    runtime.revoke_on_probe = Some(gate);
                    runtime.probe_outcomes.push_back(FakeOutcome::TransportLost);
                }
            }
            let error = if mutation {
                owner.set_ims(ImsSettingsUpdate::default())
            } else {
                owner.probe()
            }
            .unwrap_err();
            assert!(error.error.contains(crate::module_recovery::MESSAGE));
            owner.close().unwrap();
            let runtime = runtime.lock().unwrap();
            assert_eq!(
                runtime.opens, 1,
                "revoked owner must never automatically reopen"
            );
            assert_eq!(runtime.aborts, 0, "new recovery was not accepted");
            assert_eq!(
                runtime.probes + runtime.updates,
                1,
                "no replay after revocation"
            );
            assert_eq!(
                runtime.closes, 1,
                "original session still has a real close receipt"
            );
        }
    }

    #[test]
    fn qmi_waiting_for_serialized_owner_checks_admission_after_lock() {
        let (owner, runtime) = fake_owner();
        let gate = crate::module_recovery::Gate::default();
        let owner = owner.with_module_recovery(gate.clone());
        owner.open("auto".into()).unwrap();
        let locked = owner.state.lock().unwrap();
        let waiting = owner.clone();
        let (entered, receiver) = std::sync::mpsc::sync_channel(1);
        let task = std::thread::spawn(move || {
            entered.send(()).unwrap();
            waiting.probe()
        });
        receiver.recv().unwrap();
        gate.revoke_for_tests();
        drop(locked);
        assert_eq!(
            task.join().unwrap().unwrap_err().code,
            crate::module_recovery::CODE
        );
        owner.close().unwrap();
        assert_eq!(runtime.lock().unwrap().probes, 0);
    }

    #[test]
    fn native_qmi_revoke_during_open_closes_before_handoff_and_keeps_unconfirmed_cleanup() {
        for close_failure in [false, true] {
            let (owner, runtime) = fake_owner();
            let gate = crate::module_recovery::Gate::default();
            let owner = owner.with_module_recovery(gate.clone());
            {
                let mut runtime = runtime.lock().unwrap();
                runtime.revoke_on_open = Some(gate);
                runtime.close_failure = close_failure;
            }
            let mut lifecycle = NativeQmiLifecycle::start(NativeQmiConfig::Qmi, owner.clone());
            assert!(
                lifecycle
                    .startup_error()
                    .unwrap()
                    .contains(crate::module_recovery::MESSAGE)
            );
            let first = lifecycle.shutdown();
            assert_eq!(first.is_err(), close_failure);
            assert_eq!(lifecycle.shutdown(), first);
            assert_eq!(owner.close().is_err(), close_failure);
            assert_eq!(
                owner.probe().unwrap_err().code,
                crate::module_recovery::CODE
            );
            let runtime = runtime.lock().unwrap();
            assert_eq!((runtime.opens, runtime.probes, runtime.closes), (1, 0, 1));
            let state = owner.state.lock().unwrap();
            assert!(state.session.is_none());
            assert!(state.control_path.is_none());
            assert_eq!(state.cleanup_failure.is_some(), close_failure);
        }
    }
}
