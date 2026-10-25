//! Rust owner for the authenticated SMS-forwarding configuration, queue and
//! provider API. The queue format and destination bindings remain compatible
//! with the former Go owner so a process restart does not migrate or replay
//! accepted notifications.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Body;
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use proletarius_forwarding::{
    BarkEncryptionAlgorithm, BarkSender, DestinationBinding, EnqueueOptions, ForwardingChannel,
    ForwardingMessage, ForwardingRecentChannelSummary, ForwardingWorker, ProxyMode, QueueStore,
    StateKey, StateLimits, SystemForwardingClock, TelegramSender,
};
use proletarius_sms::stable_digest_hex;
use proletarius_store::StoreError;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::{Mutex, Notify, watch};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

mod queue_io;
mod test_intent;

const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_QUEUE_ACTION_BODY_BYTES: usize = 16 * 1024;
const MAX_BATCH: usize = 200;
const TASK_LIMIT: usize = 100;
const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(crate) struct Service(Arc<Inner>);

struct Inner {
    state_path: PathBuf,
    test_intents: test_intent::Store,
    #[cfg(test)]
    test_provider_result: StdMutex<Option<Result<(), proletarius_forwarding::DeliveryFailure>>>,
    #[cfg(test)]
    test_dispatches: std::sync::atomic::AtomicUsize,
    configuration_intents: crate::sms_forwarding_config_intent::Store,
    state: Mutex<ServiceState>,
    wake: Arc<Notify>,
    handoffs: Option<crate::forwarding_handoff_store::Store>,
    device_admission: Option<crate::device_forwarding_store::Store>,
    #[cfg(test)]
    admission_gate: StdMutex<Option<Arc<retirement_tests::AdmissionGate>>>,
    #[cfg(test)]
    runner_entries: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Default)]
struct ServiceState {
    active: Option<Configured>,
    admission: Option<AdmissionCommit>,
    retirement_error: Option<&'static str>,
    admission_failure: Option<&'static str>,
    configuration_failure: Option<&'static str>,
}

struct AdmissionCommit {
    configuration_id: Option<String>,
    task: Option<JoinHandle<Result<(), ()>>>,
    outcome: Option<Result<(), &'static str>>,
}

struct Configured {
    settings: Settings,
    key: Zeroizing<Vec<u8>>,
    id: String,
    worker: Arc<ForwardingWorker<SystemForwardingClock>>,
    queue_io: Arc<queue_io::Owner>,
    telegram_binding: DestinationBinding,
    bark_binding: DestinationBinding,
    shutdown: watch::Sender<bool>,
    runner: Option<JoinHandle<()>>,
    start: watch::Sender<bool>,
    admitted: bool,
    retiring: bool,
    runner_result: Option<Result<(), &'static str>>,
    stop_result: Option<Result<(), &'static str>>,
    last_error: Arc<StdMutex<String>>,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
struct Settings {
    telegram: TelegramSettings,
    bark: BarkSettings,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
struct TelegramSettings {
    enabled: bool,
    #[serde(default)]
    bot_token: Zeroizing<String>,
    #[serde(default)]
    chat_id: Zeroizing<String>,
    #[serde(default)]
    proxy: ProxySettings,
}

#[derive(Clone, Default, Deserialize, PartialEq, Eq)]
struct ProxySettings {
    #[serde(default)]
    mode: String,
    #[serde(default)]
    url: Zeroizing<String>,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
struct BarkSettings {
    enabled: bool,
    #[serde(default)]
    server_url: String,
    #[serde(default)]
    device_key: Zeroizing<String>,
    #[serde(default)]
    icon_url: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    encrypted: bool,
    #[serde(default)]
    encryption_algorithm: String,
    #[serde(default)]
    encryption_key: Zeroizing<String>,
}

#[derive(Deserialize)]
struct ConfigureRequest {
    client_request_id: String,
    client_sequence: u64,
    telegram: TelegramSettings,
    bark: BarkSettings,
    #[serde(default)]
    state_key: Zeroizing<String>,
}

#[derive(Deserialize)]
struct EnqueueRequest {
    messages: Vec<IncomingMessage>,
    expected_configuration_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeviceSnapshotRequest {
    #[serde(default)]
    expected_configuration_id: Option<String>,
    current_notification_ids: Vec<String>,
    messages: Vec<DeviceSnapshotMessage>,
    #[serde(default)]
    baseline_only: bool,
    #[serde(default)]
    include_current_snapshot: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeviceSnapshotMessage {
    pub(crate) notification_id: String,
    pub(crate) id: String,
    pub(crate) direction: String,
    pub(crate) sender: String,
    pub(crate) content: String,
    pub(crate) timestamp: String,
    pub(crate) device: String,
}

#[derive(Deserialize)]
struct IncomingMessage {
    id: String,
    #[serde(default)]
    direction: String,
    #[serde(default)]
    sender: String,
    #[serde(default)]
    content: String,
    timestamp: String,
    #[serde(default)]
    device: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderRequest {
    client_request_id: String,
    provider: String,
    expected_configuration_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueActionRequest {
    id: String,
    provider: String,
    action: String,
    expected_configuration_id: String,
    expected_attempts: i64,
    expected_state: String,
    expected_destination_revision: String,
}

#[derive(Serialize)]
struct APIError<'a> {
    error: &'a str,
    code: &'a str,
    retryable: bool,
}

impl Service {
    pub(crate) fn new(data_root: &Path) -> Self {
        Self(Arc::new(Inner {
            state_path: data_root.join("sms-forwarding").join("state.enc"),
            test_intents: test_intent::Store::new(data_root),
            #[cfg(test)]
            test_provider_result: StdMutex::new(None),
            #[cfg(test)]
            test_dispatches: std::sync::atomic::AtomicUsize::new(0),
            configuration_intents: crate::sms_forwarding_config_intent::Store::new(data_root),
            state: Mutex::new(ServiceState::default()),
            wake: Arc::new(Notify::new()),
            handoffs: None,
            device_admission: None,
            #[cfg(test)]
            admission_gate: StdMutex::new(None),
            #[cfg(test)]
            runner_entries: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }))
    }

    pub(crate) fn new_with_admission(
        data_root: &Path,
        handoffs: crate::forwarding_handoff_store::Store,
        device_admission: crate::device_forwarding_store::Store,
    ) -> Self {
        Self(Arc::new(Inner {
            state_path: data_root.join("sms-forwarding").join("state.enc"),
            test_intents: test_intent::Store::new(data_root),
            #[cfg(test)]
            test_provider_result: StdMutex::new(None),
            #[cfg(test)]
            test_dispatches: std::sync::atomic::AtomicUsize::new(0),
            configuration_intents: crate::sms_forwarding_config_intent::Store::new(data_root),
            state: Mutex::new(ServiceState::default()),
            wake: Arc::new(Notify::new()),
            handoffs: Some(handoffs),
            device_admission: Some(device_admission),
            #[cfg(test)]
            admission_gate: StdMutex::new(None),
            #[cfg(test)]
            runner_entries: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }))
    }

    pub(crate) async fn handle<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
    where
        B: Body<Data = Bytes> + Send + Unpin,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        match (request.method(), request.uri().path()) {
            (&Method::GET, "/api/sms/forwarding/status") => self.status().await,
            (&Method::POST, "/api/sms/forwarding/configure") => self.configure(request).await,
            (&Method::POST, "/api/sms/forwarding/enqueue") => self.enqueue(request).await,
            (&Method::POST, "/api/sms/forwarding/device-snapshot") => {
                self.device_snapshot(request).await
            }
            (&Method::POST, "/api/sms/forwarding/test") => self.test(request).await,
            (&Method::POST, "/api/sms/forwarding/queue-action") => self.queue_action(request).await,
            _ => error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed", false),
        }
    }

    /// Reports whether at least one channel is enabled in the currently
    /// confirmed configuration. This is used by the Rust-owned event producer;
    /// it never exposes provider credentials or configuration contents.
    /// Pending/failed retirement is Err, not a disabled high-water decision.
    pub(crate) async fn admission_enabled(&self) -> Result<bool, &'static str> {
        let mut state = self.0.state.lock().await;
        if let Some(code) = state.admission_error().await {
            return Err(code);
        }
        // `active == None` has two meanings: an explicitly acknowledged
        // disabled configuration, or the process-start window before the
        // GUI has restored its Keychain-backed configuration. Only the first
        // may advance durable SMS high-water marks or establish a baseline.
        if state.admission.is_none() {
            return Err("sms_forwarding_configuration_unconfirmed");
        }
        Ok(state
            .active
            .as_ref()
            .is_some_and(|active| active.settings.telegram.enabled || active.settings.bark.enabled))
    }

    /// Commits a snapshot discovered from Rust-owned archive/USB inbox state.
    /// Unlike the compatibility HTTP route, this producer is independent of
    /// Go-child generation and its volatile duplicate snapshot.
    pub(crate) async fn submit_device_snapshot_from_rust(
        &self,
        current_notification_ids: Vec<String>,
        messages: Vec<DeviceSnapshotMessage>,
        baseline_only: bool,
        include_current_snapshot: bool,
    ) -> Result<String, &'static str> {
        self.submit_device_snapshot_inner(
            current_notification_ids,
            messages,
            baseline_only,
            include_current_snapshot,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn submit_device_snapshot(
        &self,
        current_notification_ids: Vec<String>,
        messages: Vec<DeviceSnapshotMessage>,
        baseline_only: bool,
        include_current_snapshot: bool,
    ) -> Result<String, &'static str> {
        self.submit_device_snapshot_inner(
            current_notification_ids,
            messages,
            baseline_only,
            include_current_snapshot,
        )
        .await
    }

    async fn submit_device_snapshot_inner(
        &self,
        current_notification_ids: Vec<String>,
        messages: Vec<DeviceSnapshotMessage>,
        baseline_only: bool,
        include_current_snapshot: bool,
    ) -> Result<String, &'static str> {
        let expected_configuration_id = self
            .0
            .state
            .lock()
            .await
            .active
            .as_ref()
            .map(|active| active.id.clone());
        let input = DeviceSnapshotRequest {
            expected_configuration_id,
            current_notification_ids,
            messages,
            baseline_only,
            include_current_snapshot,
        };
        let body = serde_json::to_vec(&input).map_err(|_| "invalid_device_sms_snapshot")?;
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/sms/forwarding/device-snapshot")
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|_| "invalid_device_sms_snapshot")?;
        let response = self.device_snapshot(request).await;
        if !response.status().is_success() {
            return Err(match response.status() {
                StatusCode::CONFLICT => "forwarding_configuration_changed",
                StatusCode::SERVICE_UNAVAILABLE => "device_forwarding_admission_unavailable",
                _ => "invalid_device_sms_snapshot",
            });
        }
        #[derive(Deserialize)]
        struct Accepted {
            accepted: bool,
            disposition: String,
        }
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| "device_forwarding_admission_unavailable")?
            .to_bytes();
        let accepted: Accepted = serde_json::from_slice(&bytes)
            .map_err(|_| "device_forwarding_admission_unavailable")?;
        if !accepted.accepted
            || !matches!(
                accepted.disposition.as_str(),
                "accepted" | "baseline" | "disabled" | "resume_baseline"
            )
        {
            return Err("device_forwarding_admission_unavailable");
        }
        Ok(accepted.disposition)
    }

    /// Durably admits messages discovered by an in-process Rust producer. The
    /// queue commit and configuration check share the service lock, preventing
    /// a concurrent settings change from accepting against a stale target.
    /// `Ok(false)` means forwarding was disabled; `Err(())` means the producer
    /// must retain its cursor and retry the same stable message IDs.
    pub(crate) async fn enqueue_admitted(
        &self,
        messages: &[ForwardingMessage],
    ) -> Result<bool, ()> {
        if messages.is_empty() {
            return Ok(true);
        }
        let mut state = self.0.state.lock().await;
        if state.admission_error().await.is_some() {
            return Err(());
        }
        let Some(active) = state.active.as_mut() else {
            return Ok(false);
        };
        if !active.settings.telegram.enabled && !active.settings.bark.enabled {
            return Ok(false);
        }
        let options = EnqueueOptions::new(
            active.settings.telegram.enabled,
            &active.telegram_binding,
            active.settings.bark.enabled,
            &active.bark_binding,
        );
        let now = proletarius_forwarding::ForwardingClock::now_rfc3339(&SystemForwardingClock)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
        let queue = active.worker.queue().clone();
        let io = active.queue_io.clone();
        let worker = active.worker.clone();
        let wake = self.0.wake.clone();
        let messages = messages.to_vec();
        queue_io::run(state, queue, io, move |queue| {
            let summary = queue
                .enqueue_and_persist(&messages, &now, &options)
                .map_err(|_| {
                    proletarius_store::StoreError::Configuration(
                        "forwarding_queue_unavailable".to_owned(),
                    )
                })?;
            // The retained queue command, rather than its cancellable producer,
            // owns the wake after durable admission.
            if summary.queued > 0 {
                worker.notify_new_work();
                wake.notify_one();
            }
            Ok(summary)
        })
        .await
        .map_err(|_| ())?;
        Ok(true)
    }

    async fn configure<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
    where
        B: Body<Data = Bytes> + Send + Unpin,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let body: ConfigureRequest = match read_json(request).await {
            Ok(body) => body,
            Err(response) => return response,
        };
        let fingerprint = configuration_fingerprint(&body);
        let client_request_id = body.client_request_id.clone();
        let client_sequence = body.client_sequence;
        let settings = Settings {
            telegram: body.telegram,
            bark: body.bark,
        };
        let state_key = body.state_key;

        // The durable intent decision and active worker replacement share one
        // lock. If the newer UI snapshot enters first, a late older snapshot
        // is rejected before it can retire that worker. If the older snapshot
        // enters first, the newer one necessarily commits last.
        let mut state = self.0.state.lock().await;
        let intent_store = self.0.configuration_intents.clone();
        let intent_request_id = client_request_id.clone();
        let intent_fingerprint = fingerprint.clone();
        let intent = match crate::archive_io::run(move || {
            intent_store.admit(&intent_request_id, client_sequence, &intent_fingerprint)
        })
        .await
        {
            Ok(intent) => intent,
            Err(error) => return configuration_intent_error(error),
        };
        if let Err((code, _message)) = validate_settings(&settings) {
            return error(StatusCode::BAD_REQUEST, code, false);
        }
        let supplied_key = if state_key.is_empty() {
            None
        } else {
            let decoded = match STANDARD.decode(state_key.as_bytes()) {
                Ok(key) if key.len() == 32 => Zeroizing::new(key),
                _ => {
                    return error(
                        StatusCode::BAD_REQUEST,
                        "state_key_must_be_a_32_byte_base64_value",
                        false,
                    );
                }
            };
            Some(decoded)
        };

        if let Err(code) = state.observe_finished().await {
            return retirement_error(code);
        }
        let old_settings = state.active.as_ref().map(|active| active.settings.clone());
        let old_key = state
            .active
            .as_ref()
            .map(|active| Zeroizing::new(active.key.to_vec()));
        let key = supplied_key.or_else(|| {
            (!settings.telegram.enabled && !settings.bark.enabled)
                .then(|| old_key.as_ref().map(|key| Zeroizing::new(key.to_vec())))
                .flatten()
        });
        if (settings.telegram.enabled || settings.bark.enabled) && key.is_none() {
            return error(
                StatusCode::BAD_REQUEST,
                "state_key_must_be_a_32_byte_base64_value",
                false,
            );
        }
        let Some(key) = key else {
            if let Err(code) = state.retire_active().await {
                return retirement_error(code);
            }
            if let Err(code) = self.commit_mode(&mut state, false).await {
                return retirement_error(code);
            }
            state.active = None;
            state.configuration_failure = None;
            return configuration_response(
                false,
                None,
                &client_request_id,
                client_sequence,
                intent.revision,
            );
        };
        if key.len() != 32 {
            return error(
                StatusCode::BAD_REQUEST,
                "state_key_must_be_a_32_byte_base64_value",
                false,
            );
        }
        if let Some(active) = state.active.as_ref()
            && active.is_running()
            && !state.mode_pending()
            && active.settings == settings
            && bool::from(active.key.as_slice().ct_eq(key.as_slice()))
        {
            let id = active.id.clone();
            if let Err(code) = self
                .commit_mode(
                    &mut state,
                    settings.telegram.enabled || settings.bark.enabled,
                )
                .await
            {
                if let Err(stop) = state.retire_active().await {
                    return retirement_error(stop);
                }
                return retirement_error(code);
            }
            state.configuration_failure = None;
            return configuration_response(
                settings.telegram.enabled || settings.bark.enabled,
                Some(&id),
                &client_request_id,
                client_sequence,
                intent.revision,
            );
        }

        if let Err(code) = state.retire_active().await {
            return retirement_error(code);
        }
        match self.build(settings.clone(), key).await {
            Ok(active) => {
                let id = active.id.clone();
                // Install before the first admission I/O await. Cancellation
                // retains this exact runner, queue and key in ServiceState.
                if let Err(code) = self.install(&mut state, active).await {
                    if let Err(stop) = state.retire_active().await {
                        return retirement_error(stop);
                    }
                    if let Err(restore) = self.restore(&mut state, old_settings, old_key).await {
                        if state.active.is_none() {
                            state.configuration_failure = Some(restore);
                        }
                        return retirement_error(restore);
                    }
                    return retirement_error(code);
                }
                configuration_response(
                    settings.telegram.enabled || settings.bark.enabled,
                    Some(&id),
                    &client_request_id,
                    client_sequence,
                    intent.revision,
                )
            }
            Err(code) => {
                if let Err(restore) = self.restore(&mut state, old_settings, old_key).await {
                    if state.active.is_none() {
                        state.configuration_failure = Some(restore);
                    }
                    return retirement_error(restore);
                }
                error(StatusCode::BAD_REQUEST, code, false)
            }
        }
    }

    async fn install(
        &self,
        state: &mut ServiceState,
        active: Configured,
    ) -> Result<(), &'static str> {
        let enabled = active.settings.telegram.enabled || active.settings.bark.enabled;
        // build has no suspension point after spawning; no runner remains in
        // a local variable across this method's first possible suspension.
        state.active = Some(active);
        self.commit_mode(state, enabled).await?;
        let active = state.active.as_mut().expect("installed runner");
        active.admitted = true;
        active.start.send_replace(true);
        state.configuration_failure = None;
        Ok(())
    }

    async fn commit_mode(
        &self,
        state: &mut ServiceState,
        enabled: bool,
    ) -> Result<(), &'static str> {
        // This includes the no-config disabled write and same-config path;
        // neither may leave a late detached writer after cancellation.
        if let Some(previous) = state.admission.as_mut() {
            previous.finish().await?;
        }
        let service = self.clone();
        state.admission = Some(AdmissionCommit {
            configuration_id: state.active.as_ref().map(|active| active.id.clone()),
            task: Some(tokio::spawn(async move {
                service.note_device_admission_mode(enabled).await
            })),
            outcome: None,
        });
        state
            .admission
            .as_mut()
            .expect("installed mode writer")
            .finish()
            .await
    }

    async fn restore(
        &self,
        state: &mut ServiceState,
        settings: Option<Settings>,
        key: Option<Zeroizing<Vec<u8>>>,
    ) -> Result<(), &'static str> {
        if let (Some(settings), Some(key)) = (settings, key) {
            let active = self
                .build(settings, key)
                .await
                .map_err(|_| "sms_forwarding_unavailable")?;
            if let Err(code) = self.install(state, active).await {
                state.retire_active().await?;
                return Err(code);
            }
        }
        Ok(())
    }

    async fn note_device_admission_mode(&self, enabled: bool) -> Result<(), ()> {
        #[cfg(test)]
        {
            let gate = self.0.admission_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.wait().await;
            }
        }
        let Some(store) = self.0.device_admission.clone() else {
            return Ok(());
        };
        crate::archive_io::run(move || store.note_configuration_enabled(enabled))
            .await
            .map_err(|_| ())
    }

    async fn build(
        &self,
        settings: Settings,
        key_bytes: Zeroizing<Vec<u8>>,
    ) -> Result<Configured, &'static str> {
        let state_key = StateKey::from_slice(&key_bytes).map_err(|_| "invalid_state_key")?;
        let telegram_binding = DestinationBinding::telegram(
            &state_key,
            &settings.telegram.bot_token,
            &settings.telegram.chat_id,
        );
        let bark_server = if settings.bark.server_url.trim().is_empty() {
            "https://api.day.app"
        } else {
            settings.bark.server_url.trim()
        };
        let bark_binding =
            DestinationBinding::bark(&state_key, bark_server, &settings.bark.device_key)
                .map_err(|_| "invalid_bark_server_url")?;
        let telegram = if settings.telegram.enabled {
            let proxy = match settings
                .telegram
                .proxy
                .mode
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "system" => ProxyMode::System,
                "direct" => ProxyMode::Direct,
                "manual" => ProxyMode::Manual(settings.telegram.proxy.url.to_string()),
                _ => return Err("telegram_proxy_mode_invalid"),
            };
            Some(
                TelegramSender::new(
                    settings.telegram.bot_token.to_string(),
                    settings.telegram.chat_id.to_string(),
                    proxy,
                )
                .map_err(|failure| provider_code(&failure.code))?,
            )
        } else {
            None
        };
        let bark = if settings.bark.enabled {
            let encryption = if settings.bark.encrypted {
                let algorithm =
                    BarkEncryptionAlgorithm::from_name(&settings.bark.encryption_algorithm)
                        .map_err(|_| "bark_encryption_algorithm_invalid")?;
                Some((algorithm, settings.bark.encryption_key.to_string()))
            } else {
                None
            };
            Some(
                BarkSender::new(
                    bark_server,
                    settings.bark.device_key.to_string(),
                    nonempty(&settings.bark.icon_url),
                    nonempty(&settings.bark.group),
                    encryption,
                )
                .map_err(|failure| provider_code(&failure.code))?,
            )
        } else {
            None
        };

        let parent = self.0.state_path.parent().ok_or("invalid_state_path")?;
        prepare_private_state_directory(parent)?;
        let queue = QueueStore::open(&self.0.state_path, state_key, StateLimits::default())
            .map_err(|_| "sms_forwarding_queue_unavailable")?;
        let worker = Arc::new(
            ForwardingWorker::system_clock(
                queue,
                telegram,
                settings
                    .telegram
                    .enabled
                    .then_some(telegram_binding.clone()),
                bark,
                settings.bark.enabled.then_some(bark_binding.clone()),
            )
            .map_err(|_| "sms_forwarding_worker_configuration_invalid")?,
        );
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| "system_entropy_unavailable")?;
        let id = hex(&random);
        let (shutdown, mut receiver) = watch::channel(false);
        let (start, mut admission) = watch::channel(false);
        let runner_worker = worker.clone();
        let wake = self.0.wake.clone();
        let status_telegram_binding = telegram_binding.clone();
        let status_bark_binding = bark_binding.clone();
        let telegram_enabled = settings.telegram.enabled;
        let bark_enabled = settings.bark.enabled;
        let last_error = Arc::new(StdMutex::new(String::new()));
        let runner_error = last_error.clone();
        #[cfg(test)]
        let runner_entries = self.0.runner_entries.clone();
        let runner = tokio::spawn(async move {
            // Configuration is installed before admission persistence yields.
            // No queued provider attempt may start before its real mode ACK.
            loop {
                tokio::select! {
                    biased;
                    changed = receiver.changed() => {
                        if changed.is_err() || *receiver.borrow() { return; }
                    }
                    changed = admission.changed() => {
                        if changed.is_err() { return; }
                        if *admission.borrow() { break; }
                    }
                }
            }
            loop {
                #[cfg(test)]
                runner_entries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let flush = tokio::select! {
                    biased;
                    changed = receiver.changed() => {
                        if changed.is_err() || *receiver.borrow() { break; }
                        continue;
                    }
                    result = runner_worker.flush_pending() => result,
                };
                match flush {
                    Ok(summary) => {
                        let now = proletarius_forwarding::ForwardingClock::now_rfc3339(
                            &SystemForwardingClock,
                        )
                        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
                        let paused = match runner_worker
                            .inspect_for_bindings(
                                Some(status_telegram_binding.clone()),
                                Some(status_bark_binding.clone()),
                            )
                            .await
                        {
                            Ok(queue) => queue.tasks.iter().any(|task| {
                                pending_channel(&task.telegram, telegram_enabled, &now, true)
                                    .state
                                    .starts_with("paused_")
                                    || pending_channel(&task.bark, bark_enabled, &now, false)
                                        .state
                                        .starts_with("paused_")
                            }),
                            Err(_) => {
                                if let Ok(mut value) = runner_error.lock() {
                                    *value = "queue_persistence_failed".to_owned();
                                }
                                continue;
                            }
                        };
                        if let Ok(mut value) = runner_error.lock() {
                            *value = if paused {
                                "pending_channels_paused".to_owned()
                            } else if summary.failed > 0 || summary.uncertain > 0 {
                                "provider_delivery_pending".to_owned()
                            } else {
                                String::new()
                            };
                        }
                    }
                    Err(_) => {
                        if let Ok(mut value) = runner_error.lock() {
                            *value = "queue_persistence_failed".to_owned();
                        }
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(POLL_INTERVAL) => {},
                    _ = wake.notified() => {},
                    changed = receiver.changed() => {
                        if changed.is_err() || *receiver.borrow() { break; }
                    }
                }
            }
        });
        Ok(Configured {
            settings,
            key: key_bytes,
            id,
            worker,
            queue_io: Arc::new(queue_io::Owner::default()),
            telegram_binding,
            bark_binding,
            shutdown,
            runner: Some(runner),
            start,
            admitted: false,
            retiring: false,
            runner_result: None,
            stop_result: None,
            last_error,
        })
    }

    async fn enqueue<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
    where
        B: Body<Data = Bytes> + Send + Unpin,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let body: EnqueueRequest = match read_json(request).await {
            Ok(body) => body,
            Err(response) => return response,
        };
        if body.expected_configuration_id.len() != 32 {
            return error(
                StatusCode::CONFLICT,
                "forwarding_configuration_confirmation_required",
                false,
            );
        }
        if body.messages.len() > MAX_BATCH {
            return error(StatusCode::BAD_REQUEST, "batch_exceeds_200_messages", false);
        }
        let mut messages = Vec::with_capacity(body.messages.len());
        for message in body.messages {
            if message.id.len() > 4096
                || message.sender.len() > 1024
                || message.device.len() > 256
                || message.content.len() > 65536
            {
                return error(
                    StatusCode::BAD_REQUEST,
                    "message_field_exceeds_limit",
                    false,
                );
            }
            match ForwardingMessage::new(
                message.id,
                message.direction,
                message.sender,
                message.content,
                message.timestamp,
                message.device,
            ) {
                Ok(message) => messages.push(message),
                Err(_) => {
                    return error(StatusCode::BAD_REQUEST, "invalid_forwarding_message", false);
                }
            }
        }
        let mut state = self.0.state.lock().await;
        if let Some(code) = state.admission_error().await {
            return retirement_error(code);
        }
        let Some(active) = state.active.as_mut() else {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "sms_forwarding_disabled",
                false,
            );
        };
        if !active.settings.telegram.enabled && !active.settings.bark.enabled {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "sms_forwarding_disabled",
                false,
            );
        }
        if active.id != body.expected_configuration_id {
            return error(
                StatusCode::CONFLICT,
                "forwarding_configuration_changed",
                false,
            );
        }
        let options = EnqueueOptions::new(
            active.settings.telegram.enabled,
            &active.telegram_binding,
            active.settings.bark.enabled,
            &active.bark_binding,
        );
        let now = proletarius_forwarding::ForwardingClock::now_rfc3339(&SystemForwardingClock)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
        let queue = active.worker.queue().clone();
        let io = active.queue_io.clone();
        let worker = active.worker.clone();
        let wake = self.0.wake.clone();
        // The configured owner retains the queue guard through the actual disk
        // transaction even when this HTTP observer is cancelled. Replacement
        // and shutdown already drain this same owner before releasing its flock.
        let result = queue_io::run(state, queue, io, move |queue| {
            let summary = queue
                .enqueue_and_persist(&messages, &now, &options)
                .map_err(|_| {
                    StoreError::Configuration("forwarding_queue_unavailable".to_owned())
                })?;
            // Wake belongs to the successful durable job, not its HTTP waiter.
            // Both captures retire with the named command before completion.
            if summary.queued > 0 {
                worker.notify_new_work();
                wake.notify_one();
            }
            Ok(summary)
        })
        .await;
        match result {
            Ok(summary) => json(
                StatusCode::ACCEPTED,
                serde_json::json!({"queued":summary.queued,"pending":summary.pending}),
            ),
            Err(_) => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "sms_forwarding_queue_unavailable",
                true,
            ),
        }
    }

    async fn device_snapshot<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
    where
        B: Body<Data = Bytes> + Send + Unpin,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let body: DeviceSnapshotRequest = match read_json(request).await {
            Ok(body) => body,
            Err(response) => return response,
        };
        if body.current_notification_ids.len() > 100_000
            || body.messages.len() > MAX_BATCH
            || (!body.include_current_snapshot && !body.current_notification_ids.is_empty())
            || (body.baseline_only && (!body.messages.is_empty() || !body.include_current_snapshot))
            || body
                .current_notification_ids
                .iter()
                .any(|id| !crate::device_forwarding_store::is_digest(id))
        {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_device_sms_snapshot",
                false,
            );
        }
        let current: std::collections::BTreeSet<_> =
            body.current_notification_ids.iter().cloned().collect();
        if current.len() != body.current_notification_ids.len() {
            return error(StatusCode::BAD_REQUEST, "duplicate_notification_id", false);
        }
        let mut message_ids = std::collections::BTreeSet::new();
        let mut snapshot_message_ids = std::collections::BTreeSet::new();
        let mut messages = Vec::with_capacity(body.messages.len());
        let mut notification_ids = Vec::with_capacity(body.messages.len());
        let include_current_snapshot = body.include_current_snapshot;
        for message in body.messages {
            if !crate::device_forwarding_store::is_digest(&message.notification_id)
                || (include_current_snapshot && !current.contains(&message.notification_id))
                || !snapshot_message_ids.insert(message.notification_id.clone())
                || !message_ids.insert(message.id.clone())
                || message.direction != "in"
                || message.id.len() > 4096
                || message.sender.len() > 1024
                || message.device.len() > 256
                || message.content.len() > 65_536
            {
                return error(StatusCode::BAD_REQUEST, "invalid_device_sms_message", false);
            }
            match ForwardingMessage::new(
                message.id,
                message.direction,
                message.sender,
                message.content,
                message.timestamp,
                message.device,
            ) {
                Ok(value) => {
                    notification_ids.push(message.notification_id);
                    messages.push(value);
                }
                Err(_) => {
                    return error(StatusCode::BAD_REQUEST, "invalid_forwarding_message", false);
                }
            }
        }

        let (Some(handoffs), Some(admission)) =
            (self.0.handoffs.clone(), self.0.device_admission.clone())
        else {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "device_forwarding_admission_unavailable",
                true,
            );
        };
        let progress = match crate::archive_io::run(move || {
            let handoff = handoffs.current()?.ok_or_else(|| {
                proletarius_store::StoreError::Configuration(
                    "forwarding_handoff_required".to_owned(),
                )
            })?;
            admission.import_handoff(&handoff)
        })
        .await
        {
            Ok(progress) => progress,
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "device_forwarding_admission_unavailable",
                    true,
                );
            }
        };

        let mut state = self.0.state.lock().await;
        if let Some(code) = state.admission_error().await {
            return retirement_error(code);
        }
        let Some(active) = state.active.as_mut() else {
            let admission = self
                .0
                .device_admission
                .clone()
                .expect("admission store has ledger");
            let current_ids = body.current_notification_ids;
            let Some(handoff_id) = progress.handoff_id.clone() else {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "forwarding_handoff_required",
                    false,
                );
            };
            let stored = crate::archive_io::run(move || {
                admission.skip_snapshot(&handoff_id, &current_ids, false)
            })
            .await;
            return match stored {
                Ok(_) => json(
                    StatusCode::OK,
                    serde_json::json!({"accepted":true,"disposition":"disabled"}),
                ),
                Err(_) => error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "device_forwarding_admission_unavailable",
                    true,
                ),
            };
        };
        let enabled = active.settings.telegram.enabled || active.settings.bark.enabled;
        if enabled && body.expected_configuration_id.as_deref() != Some(active.id.as_str()) {
            return error(
                StatusCode::CONFLICT,
                "forwarding_configuration_changed",
                false,
            );
        }
        if enabled && progress.skip_next_snapshot && !include_current_snapshot {
            return error(StatusCode::CONFLICT, "forwarding_snapshot_required", true);
        }

        let admission = self
            .0
            .device_admission
            .clone()
            .expect("admission store has ledger");
        let current_ids = body.current_notification_ids;
        let Some(handoff_id) = progress.handoff_id.clone() else {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "forwarding_handoff_required",
                false,
            );
        };
        if !enabled || progress.skip_next_snapshot || body.baseline_only {
            let disposition = if !enabled {
                "disabled"
            } else if body.baseline_only {
                "baseline"
            } else {
                "resume_baseline"
            };
            let stored = crate::archive_io::run(move || {
                admission.skip_snapshot(&handoff_id, &current_ids, enabled)
            })
            .await;
            return match stored {
                Ok(_) => json(
                    StatusCode::OK,
                    serde_json::json!({"accepted":true,"disposition":disposition}),
                ),
                Err(_) => error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "device_forwarding_admission_unavailable",
                    true,
                ),
            };
        }

        let mut accepted_ids = Vec::new();
        let mut pending_messages = Vec::new();
        for (notification_id, message) in notification_ids.into_iter().zip(messages) {
            if !progress
                .accepted_notification_ids
                .contains(&notification_id)
            {
                accepted_ids.push(notification_id);
                pending_messages.push(message);
            }
        }
        let options = EnqueueOptions::new(
            active.settings.telegram.enabled,
            &active.telegram_binding,
            active.settings.bark.enabled,
            &active.bark_binding,
        );
        let now = proletarius_forwarding::ForwardingClock::now_rfc3339(&SystemForwardingClock)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
        let queue = active.worker.queue().clone();
        let io = active.queue_io.clone();
        let worker = active.worker.clone();
        #[cfg(test)]
        let checkpoint = io.clone();
        let notify = self.0.wake.clone();
        let queue_messages = pending_messages;
        let queue_store = move |queue: &mut QueueStore| {
            let summary = queue
                .enqueue_and_persist(&queue_messages, &now, &options)
                .map_err(|_| {
                    proletarius_store::StoreError::Configuration(
                        "forwarding_queue_unavailable".to_owned(),
                    )
                })?;
            // The queue is still locked until the sidecar admission finishes.
            // Wake the committed work here so observer cancellation or a known
            // sidecar write failure cannot defer it until the periodic poll.
            if summary.queued > 0 {
                worker.notify_new_work();
                notify.notify_one();
            }
            #[cfg(test)]
            checkpoint.checkpoint(queue_io::Stage::QueueCommitted)?;
            if include_current_snapshot {
                admission.accept_batch(&handoff_id, &accepted_ids, &current_ids)?;
            } else {
                admission.accept_batch_preserving(&handoff_id, &accepted_ids)?;
            }
            Ok::<_, proletarius_store::StoreError>(summary)
        };
        let result = queue_io::run(state, queue, io, queue_store).await;
        match result {
            Ok(summary) => json(
                StatusCode::ACCEPTED,
                serde_json::json!({"accepted":true,"disposition":"accepted","queued":summary.queued}),
            ),
            Err(_) => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "forwarding_queue_unavailable",
                true,
            ),
        }
    }

    async fn test<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
    where
        B: Body<Data = Bytes> + Send + Unpin,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let body: ProviderRequest = match read_json(request).await {
            Ok(body) => body,
            Err(response) => return response,
        };
        if !test_intent::valid_id(&body.client_request_id)
            || !test_intent::valid_id(&body.expected_configuration_id)
        {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_test_request_identity",
                false,
            );
        }
        let provider = body.provider.trim().to_ascii_lowercase();
        if !matches!(provider.as_str(), "telegram" | "bark") {
            return error(StatusCode::BAD_REQUEST, "unknown_provider", false);
        }
        let store = self.0.test_intents.clone();
        let id = body.client_request_id;
        let configuration = body.expected_configuration_id;
        // Replay is independent of the active configuration/daemon restore.
        // A changed configuration never invalidates an existing exact receipt.
        let lookup_store = store.clone();
        let lookup_id = id.clone();
        let lookup_provider = provider.clone();
        let lookup_configuration = configuration.clone();
        match crate::archive_io::run(move || {
            lookup_store.lookup(&lookup_id, &lookup_provider, &lookup_configuration)
        })
        .await
        {
            Ok(Some(receipt)) => return json(StatusCode::OK, receipt),
            Ok(None) => {}
            Err(error) => return test_intent_error(error),
        }
        let mut state = self.0.state.lock().await;
        let rejection = if let Some(code) = state.admission_error().await {
            Some(code)
        } else if let Some(active) = state.active.as_ref() {
            if active.id != configuration {
                Some("sms_forwarding_configuration_changed")
            } else {
                None
            }
        } else {
            Some("sms_forwarding_disabled")
        };
        let claim_store = store.clone();
        let claim_id = id.clone();
        let claim_provider = provider.clone();
        let claim_configuration = configuration.clone();
        let (receipt, dispatch) = match crate::archive_io::run(move || {
            claim_store.claim(&claim_id, &claim_provider, &claim_configuration, rejection)
        })
        .await
        {
            Ok(value) => value,
            Err(error) => return test_intent_error(error),
        };
        if !dispatch {
            return json(StatusCode::OK, receipt);
        }
        let active = state
            .active
            .as_ref()
            .expect("admitted active test configuration");
        #[cfg(test)]
        self.0
            .test_dispatches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        #[cfg(test)]
        let injected = self.0.test_provider_result.lock().unwrap().clone();
        #[cfg(not(test))]
        let injected: Option<Result<(), proletarius_forwarding::DeliveryFailure>> = None;
        let result = match injected {
            Some(result) => result,
            None => active.worker.test_provider(&provider).await,
        };
        let outcome = result.map_err(|failure| {
            (
                provider_test_code(&failure.code),
                failure.disposition == proletarius_forwarding::DeliveryDisposition::Unknown,
            )
        });
        match crate::archive_io::run(move || store.finish(&id, outcome)).await {
            Ok(receipt) => json(StatusCode::OK, receipt),
            Err(error) => test_intent_error(error),
        }
    }

    async fn queue_action<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
    where
        B: Body<Data = Bytes> + Send + Unpin,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let raw = match crate::http_server::collect_body(
            request.into_body(),
            MAX_QUEUE_ACTION_BODY_BYTES,
        )
        .await
        {
            Err(crate::http_server::BodyReadError::Timeout) => {
                return crate::http_server::body_timeout_response();
            }
            Ok(body) => body,
            Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_request_body", false),
        };
        let (request_id, stripped) = match crate::public_mutation_receipt::split(&raw, true) {
            Ok((Some(id), body)) => (id, body),
            Ok((None, _)) => unreachable!("required mutation identity"),
            Err(response) => return response,
        };
        let body: QueueActionRequest = match serde_json::from_slice(&stripped) {
            Ok(body) => body,
            Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_request_body", false),
        };
        if body.id.len() != 64 || !body.id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return error(StatusCode::BAD_REQUEST, "invalid_forwarding_task", false);
        }
        let channel = match body.provider.as_str() {
            "telegram" => ForwardingChannel::Telegram,
            "bark" => ForwardingChannel::Bark,
            _ => return error(StatusCode::BAD_REQUEST, "invalid_forwarding_task", false),
        };
        if !matches!(body.action.as_str(), "cancel" | "retry" | "bind_legacy") {
            return error(StatusCode::BAD_REQUEST, "unknown_queue_action", false);
        }
        if body.expected_configuration_id.len() != 32
            || !body
                .expected_configuration_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || body.expected_attempts < 0
            || body.expected_state.is_empty()
            || body.expected_state.len() > 64
            || body.expected_destination_revision.len() != 64
            || !body
                .expected_destination_revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_forwarding_queue_cas",
                false,
            );
        }
        let receipt_root = self
            .0
            .state_path
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let binding = format!(
            "forwarding-queue-action-v1:{}",
            stable_digest_hex(&stripped)
        );
        crate::public_mutation_receipt::run(
            &receipt_root,
            "/api/sms/forwarding/queue-action",
            &request_id,
            &raw,
            binding,
            || async { self.execute_queue_action(body, channel).await },
        )
        .await
    }

    async fn execute_queue_action(
        &self,
        body: QueueActionRequest,
        channel: ForwardingChannel,
    ) -> Response<Full<Bytes>> {
        let mut state = self.0.state.lock().await;
        if let Some(code) = state.admission_error().await {
            return retirement_error(code);
        }
        let Some(active) = state.active.as_mut() else {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "sms_forwarding_disabled",
                false,
            );
        };
        let configuration_id = active.id.clone();
        let telegram_enabled = active.settings.telegram.enabled;
        let bark_enabled = active.settings.bark.enabled;
        let telegram_binding = active.telegram_binding.clone();
        let bark_binding = active.bark_binding.clone();
        let queue = active.worker.queue().clone();
        let io = active.queue_io.clone();
        let worker = active.worker.clone();
        let wake = self.0.wake.clone();
        let now = proletarius_forwarding::ForwardingClock::now_rfc3339(&SystemForwardingClock)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
        // One bounded command owns the queue guard, CAS and durable mutation.
        // A cancelled HTTP receipt cannot release this configuration or wake
        // a provider before the actual replacement has completed.
        let result = queue_io::run(state, queue, io, move |queue| {
            let summary =
                match queue.inspect_for_bindings(Some(&telegram_binding), Some(&bark_binding)) {
                    Ok(summary) => summary,
                    Err(_) => {
                        return Ok(error(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "sms_forwarding_queue_unavailable",
                            true,
                        ));
                    }
                };
            if configuration_id != body.expected_configuration_id {
                return Ok(error(
                    StatusCode::CONFLICT,
                    "forwarding_configuration_changed",
                    false,
                ));
            }
            let Some(task) = summary.tasks.iter().find(|task| task.id == body.id) else {
                return Ok(error(
                    StatusCode::CONFLICT,
                    "forwarding_task_changed",
                    false,
                ));
            };
            let (destination, enabled, binding) = match channel {
                ForwardingChannel::Telegram => {
                    (&task.telegram, telegram_enabled, &telegram_binding)
                }
                ForwardingChannel::Bark => (&task.bark, bark_enabled, &bark_binding),
            };
            let status = pending_channel(
                destination,
                enabled,
                &now,
                channel == ForwardingChannel::Telegram,
            );
            let revision = destination_revision(
                &configuration_id,
                &task.id,
                body.provider.as_str(),
                &task.updated_at,
                &status,
            );
            if destination.attempts != body.expected_attempts
                || status.state != body.expected_state
                || revision != body.expected_destination_revision
            {
                return Ok(error(
                    StatusCode::CONFLICT,
                    "forwarding_destination_changed",
                    false,
                ));
            }
            let updated_at = mutation_timestamp(&now, &task.updated_at);
            let result = match body.action.as_str() {
                "cancel" => queue.cancel_destination(&body.id, channel, &updated_at),
                "retry" if enabled => {
                    queue.retry_destination(&body.id, channel, true, binding, &updated_at)
                }
                "bind_legacy" if enabled => {
                    queue.bind_legacy_destination(&body.id, channel, true, binding, &updated_at)
                }
                "retry" | "bind_legacy" => {
                    return Ok(error(
                        StatusCode::CONFLICT,
                        "forwarding_destination_paused",
                        false,
                    ));
                }
                _ => unreachable!(),
            };
            Ok(match result {
                Ok(()) => {
                    // Wake remains with the command even if its HTTP observer
                    // disappears during the synchronous disk transaction.
                    worker.notify_new_work();
                    wake.notify_one();
                    json(StatusCode::OK, serde_json::json!({"accepted":true}))
                }
                Err(proletarius_forwarding::QueueMutationError::PersistenceUncertain) => json(
                    StatusCode::SERVICE_UNAVAILABLE,
                    serde_json::json!({
                        "error":"sms_forwarding_queue_persistence_uncertain",
                        "code":"sms_forwarding_queue_persistence_uncertain",
                        "retryable":false,
                        "uncertain":true
                    }),
                ),
                Err(proletarius_forwarding::QueueMutationError::ReopenRequired) => error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "sms_forwarding_queue_unavailable",
                    true,
                ),
                Err(failure) => error(StatusCode::CONFLICT, &failure.to_string(), false),
            })
        })
        .await;
        match result {
            Ok(response) => response,
            Err(_) => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "sms_forwarding_queue_unavailable",
                true,
            ),
        }
    }

    async fn status(&self) -> Response<Full<Bytes>> {
        let mut state = self.0.state.lock().await;
        if let Some(code) = state.admission_error().await {
            return retirement_error(code);
        }
        let Some(active) = state.active.as_ref() else {
            return json(
                StatusCode::OK,
                serde_json::json!({
                    "configured":false,"telegram_enabled":false,"bark_enabled":false,
                    "pending":0,"telegram":empty_channel_summary(),"bark":empty_channel_summary(),
                    "tasks":[],"tasks_truncated":false
                }),
            );
        };
        let summary = match active
            .worker
            .inspect_for_bindings(
                Some(active.telegram_binding.clone()),
                Some(active.bark_binding.clone()),
            )
            .await
        {
            Ok(summary) => summary,
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "sms_forwarding_queue_unavailable",
                    true,
                );
            }
        };
        let now = proletarius_forwarding::ForwardingClock::now_rfc3339(&SystemForwardingClock)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
        let mut telegram_counts = Counts::default();
        let mut bark_counts = Counts::default();
        let mut tasks = Vec::with_capacity(TASK_LIMIT);
        for task in &summary.tasks {
            let mut tg =
                pending_channel(&task.telegram, active.settings.telegram.enabled, &now, true);
            let mut bk = pending_channel(&task.bark, active.settings.bark.enabled, &now, false);
            tg.revision =
                destination_revision(&active.id, &task.id, "telegram", &task.updated_at, &tg);
            bk.revision = destination_revision(&active.id, &task.id, "bark", &task.updated_at, &bk);
            telegram_counts.add(&tg.state);
            bark_counts.add(&bk.state);
            if tasks.len() < TASK_LIMIT {
                tasks.push(serde_json::json!({"id":task.id,"updated_at":task.updated_at,"telegram":tg,"bark":bk}));
            }
        }
        for task in &summary.recent {
            telegram_counts.add(&task.telegram.state);
            bark_counts.add(&task.bark.state);
            if tasks.len() < TASK_LIMIT {
                tasks.push(serde_json::json!({
                    "id":task.id,
                    "updated_at":task.updated_at,
                    "telegram":recent_channel(&task.telegram),
                    "bark":recent_channel(&task.bark)
                }));
            }
        }
        let last_error = active
            .last_error
            .lock()
            .map(|error| error.clone())
            .unwrap_or_default();
        json(
            StatusCode::OK,
            serde_json::json!({
                "configuration_id":active.id,
                "configured":active.settings.telegram.enabled || active.settings.bark.enabled,
                "telegram_enabled":active.settings.telegram.enabled,
                "bark_enabled":active.settings.bark.enabled,
                "telegram_proxy":active.settings.telegram.proxy.mode.trim().to_ascii_lowercase(),
                "pending":summary.tasks.len(),
                "last_error":if last_error.is_empty() { None } else { Some(last_error) },
                "telegram":telegram_counts,
                "bark":bark_counts,
                "tasks":tasks,
                "tasks_truncated":summary.tasks.len()+summary.recent_diagnostics > TASK_LIMIT
            }),
        )
    }

    pub(crate) async fn shutdown(&self) -> Result<(), &'static str> {
        let mut state = self.0.state.lock().await;
        state.retire_active().await?;
        state
            .admission_failure
            .or(state.configuration_failure)
            .map_or(Ok(()), Err)
    }
}

impl Default for Service {
    fn default() -> Self {
        Self::new(&std::env::temp_dir().join("switch-rust-unconfigured-test"))
    }
}

impl ServiceState {
    fn mode_pending(&self) -> bool {
        self.admission
            .as_ref()
            .is_some_and(|mode| mode.outcome.is_none())
    }

    async fn observe_finished(&mut self) -> Result<(), &'static str> {
        if let Some(code) = self.retirement_error {
            return Err(code);
        }
        if self.active.as_ref().is_some_and(|active| {
            active.stop_result.is_some()
                || active.runner_result.is_some()
                || active.runner.as_ref().is_some_and(JoinHandle::is_finished)
        }) {
            self.retire_active().await?;
        }
        Ok(())
    }

    async fn admission_error(&mut self) -> Option<&'static str> {
        if let Err(code) = self.observe_finished().await {
            return Some(code);
        }
        if let Some(code) = self.admission_failure.or(self.configuration_failure) {
            return Some(code);
        }
        if let Some(mode) = self.admission.as_ref() {
            if let Some(Err(code)) = mode.outcome {
                return Some(code);
            }
        }
        if self.mode_pending() {
            return Some("sms_forwarding_configuration_pending");
        }
        self.active.as_ref().and_then(|active| {
            if active.retiring {
                Some("sms_forwarding_stopping")
            } else if !active.admitted {
                Some("sms_forwarding_configuration_pending")
            } else {
                None
            }
        })
    }

    async fn retire_active(&mut self) -> Result<(), &'static str> {
        if let Some(code) = self.retirement_error {
            return Err(code);
        }
        if let Some(active) = self.active.as_mut() {
            active.retiring = true;
            active.queue_io.close();
            active.worker.close_admission();
            let _ = active.shutdown.send(true);
        }
        // Retain both the generation and mode writer while either await can
        // be cancelled. A later writer cannot overtake this actual task.
        let current_id = self.active.as_ref().map(|active| active.id.clone());
        let admission = match self.admission.as_mut() {
            Some(mode) => {
                let bound = mode.configuration_id == current_id;
                let result = mode.finish().await;
                if bound {
                    result
                } else {
                    Err("sms_forwarding_admission_unconfirmed")
                }
            }
            None => Ok(()),
        };
        let runner = match self.active.as_mut() {
            Some(active) => active.stop().await,
            None => Ok(()),
        };
        if let Err(code) = admission {
            // The task may outlive its cancelled HTTP waiter. Keep its real
            // failure observable at shutdown even after both owners retire.
            self.admission_failure.get_or_insert(code);
        }
        let result = runner.and(match admission {
            // A returned disk failure is not a successful configuration, but
            // the real task has ended and no longer owns a detached write.
            Err("device_forwarding_admission_unavailable") | Ok(()) => Ok(()),
            Err(code) => Err(code),
        });
        if let Err(code) = result {
            self.retirement_error = Some(code);
            return Err(code);
        }
        // No await between observed real completions and removing ownership.
        self.active = None;
        self.admission = None;
        Ok(())
    }
}

impl AdmissionCommit {
    async fn finish(&mut self) -> Result<(), &'static str> {
        if let Some(result) = self.outcome {
            return result;
        }
        let result = match self.task.as_mut() {
            Some(task) => match task.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(())) => Err("device_forwarding_admission_unavailable"),
                Err(error) if error.is_panic() => Err("sms_forwarding_admission_panicked"),
                Err(_) => Err("sms_forwarding_admission_aborted"),
            },
            None => Err("sms_forwarding_admission_unconfirmed"),
        };
        self.outcome = Some(result);
        self.task = None;
        result
    }
}

impl Configured {
    fn is_running(&self) -> bool {
        self.admitted
            && !self.retiring
            && self.runner_result.is_none()
            && self.stop_result.is_none()
            && self
                .runner
                .as_ref()
                .is_some_and(|runner| !runner.is_finished())
    }

    async fn stop(&mut self) -> Result<(), &'static str> {
        if let Some(result) = self.stop_result {
            return result;
        }
        self.retiring = true;
        self.queue_io.close();
        self.worker.close_admission();
        let _ = self.shutdown.send(true);
        if self.runner_result.is_none() {
            let result = match self.runner.as_mut() {
                Some(runner) => runner.await.map_err(|error| {
                    if error.is_panic() {
                        "sms_forwarding_runner_panicked"
                    } else {
                        "sms_forwarding_runner_aborted"
                    }
                }),
                None => Err("sms_forwarding_runner_unconfirmed"),
            };
            // No await between observing the join and retaining its outcome.
            self.runner = None;
            self.runner_result = Some(result);
        }
        // A joined runner may have synchronously registered a deferred Drop
        // release. Keep this generation and its Store/flock through the real
        // drain; cancelling this waiter leaves the observed join for next stop.
        // No queue guard is held while waiting for release completion.
        let io = self.queue_io.finish().await;
        let releases = self
            .worker
            .finish_releases()
            .await
            .map_err(|_| "sms_forwarding_release_unconfirmed");
        // Observe every owner even if an earlier result is an error.
        let result = self.runner_result.unwrap().and(io).and(releases);
        self.stop_result = Some(result);
        result
    }
}

fn retirement_error(code: &'static str) -> Response<Full<Bytes>> {
    error(StatusCode::SERVICE_UNAVAILABLE, code, false)
}

fn configuration_intent_error(failure: StoreError) -> Response<Full<Bytes>> {
    match failure {
        StoreError::Configuration(code) if code == "invalid_configuration_intent" => {
            error(StatusCode::BAD_REQUEST, &code, false)
        }
        StoreError::Configuration(code)
            if matches!(
                code.as_str(),
                "configuration_intent_stale"
                    | "configuration_intent_request_id_conflict"
                    | "configuration_intent_revision_exhausted"
            ) =>
        {
            error(StatusCode::CONFLICT, &code, false)
        }
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "configuration_intent_store_unavailable",
            false,
        ),
    }
}

fn configuration_response(
    configured: bool,
    configuration_id: Option<&str>,
    client_request_id: &str,
    client_sequence: u64,
    intent_revision: u64,
) -> Response<Full<Bytes>> {
    let mut body = serde_json::json!({
        "configured": configured,
        "client_request_id": client_request_id,
        "client_sequence": client_sequence,
        "configuration_intent_revision": intent_revision,
    });
    if let Some(configuration_id) = configuration_id {
        body["configuration_id"] = serde_json::json!(configuration_id);
    }
    json(StatusCode::OK, body)
}

fn configuration_fingerprint(request: &ConfigureRequest) -> String {
    fn text(output: &mut Vec<u8>, value: &str) {
        output.extend_from_slice(&(value.len() as u64).to_be_bytes());
        output.extend_from_slice(value.as_bytes());
    }
    fn flag(output: &mut Vec<u8>, value: bool) {
        output.push(u8::from(value));
    }

    // Fixed field order plus length prefixes gives one unambiguous encoding.
    // Zeroizing prevents this temporary credential-bearing buffer from
    // lingering after only its one-way digest is retained by the ledger.
    let mut bytes = Zeroizing::new(Vec::new());
    flag(&mut bytes, request.telegram.enabled);
    text(&mut bytes, &request.telegram.bot_token);
    text(&mut bytes, &request.telegram.chat_id);
    text(&mut bytes, &request.telegram.proxy.mode);
    text(&mut bytes, &request.telegram.proxy.url);
    flag(&mut bytes, request.bark.enabled);
    text(&mut bytes, &request.bark.server_url);
    text(&mut bytes, &request.bark.device_key);
    text(&mut bytes, &request.bark.icon_url);
    text(&mut bytes, &request.bark.group);
    flag(&mut bytes, request.bark.encrypted);
    text(&mut bytes, &request.bark.encryption_algorithm);
    text(&mut bytes, &request.bark.encryption_key);
    text(&mut bytes, &request.state_key);
    stable_digest_hex(&bytes)
}

impl Drop for Configured {
    fn drop(&mut self) {
        // Last-owner fallback only: this signal is not a join receipt. HTTP
        // cancellation retains the generation in ServiceState instead. The
        // pre-submit SWF1 unknown marker is never cleared by dropping a waiter.
        self.queue_io.close();
        self.worker.close_admission();
        let _ = self.shutdown.send(true);
    }
}

#[derive(Default, Serialize)]
struct Counts {
    pending: usize,
    queued: usize,
    in_flight: usize,
    retrying: usize,
    paused: usize,
    delivered: usize,
    cancelled: usize,
    skipped: usize,
}

impl Counts {
    fn add(&mut self, state: &str) {
        match state {
            "delivered" => self.delivered += 1,
            "cancelled" => self.cancelled += 1,
            "skipped" => self.skipped += 1,
            "submitting" => {
                self.pending += 1;
                self.in_flight += 1;
            }
            "retry_wait" => {
                self.pending += 1;
                self.retrying += 1;
            }
            "queued" => {
                self.pending += 1;
                self.queued += 1;
            }
            _ => {
                self.pending += 1;
                self.paused += 1;
            }
        }
    }
}

fn empty_channel_summary() -> Counts {
    Counts::default()
}

#[derive(Serialize)]
struct ChannelStatus {
    state: String,
    attempts: i64,
    revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_attempt_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_attempt_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confirmed_parts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_parts: Option<usize>,
}

fn pending_channel(
    channel: &proletarius_forwarding::ForwardingDestinationSummary,
    enabled: bool,
    now: &str,
    telegram: bool,
) -> ChannelStatus {
    let state = if channel.not_requested {
        "skipped"
    } else if channel.cancelled {
        "cancelled"
    } else if channel.completed {
        "delivered"
    } else if channel.in_flight {
        "submitting"
    } else if !enabled {
        "paused_disabled"
    } else if !channel.destination_bound {
        "paused_destination_unverified"
    } else if !channel.destination_matches_current {
        "paused_destination_changed"
    } else if telegram && channel.error_code.as_deref() == Some("telegram_chunk_progress_invalid") {
        "paused_payload_mismatch"
    } else if channel.permanent_failure {
        "paused_configuration_error"
    } else if channel.result_unknown {
        "paused_result_unknown"
    } else if channel
        .next_attempt_at
        .as_deref()
        .is_some_and(|next| timestamp_is_future(next, now))
    {
        "retry_wait"
    } else {
        "queued"
    };
    let (confirmed_parts, total_parts) = channel
        .telegram_checkpoint
        .as_ref()
        .map(|checkpoint| {
            (
                Some(checkpoint.confirmed_parts),
                Some(checkpoint.total_parts),
            )
        })
        .unwrap_or((None, None));
    ChannelStatus {
        state: state.to_owned(),
        attempts: channel.attempts,
        revision: String::new(),
        error_code: channel.error_code.clone(),
        next_attempt_at: channel.next_attempt_at.clone(),
        last_attempt_at: channel.last_attempt_at.clone(),
        confirmed_parts,
        total_parts,
    }
}

fn timestamp_is_future(next: &str, now: &str) -> bool {
    match (
        OffsetDateTime::parse(next, &Rfc3339),
        OffsetDateTime::parse(now, &Rfc3339),
    ) {
        (Ok(next), Ok(now)) => next > now,
        _ => false,
    }
}

fn recent_channel(channel: &ForwardingRecentChannelSummary) -> ChannelStatus {
    ChannelStatus {
        state: channel.state.clone(),
        attempts: channel.attempts,
        revision: String::new(),
        error_code: channel.error_code.clone(),
        next_attempt_at: channel.next_attempt_at.clone(),
        last_attempt_at: channel.last_attempt_at.clone(),
        confirmed_parts: channel.confirmed_parts,
        total_parts: channel.total_parts,
    }
}

fn destination_revision(
    configuration_id: &str,
    task_id: &str,
    provider: &str,
    task_updated_at: &str,
    status: &ChannelStatus,
) -> String {
    let material = serde_json::to_vec(&serde_json::json!({
        "configuration_id":configuration_id,
        "task_id":task_id,
        "provider":provider,
        "task_updated_at":task_updated_at,
        "state":status.state,
        "attempts":status.attempts,
        "error_code":status.error_code,
        "next_attempt_at":status.next_attempt_at,
        "last_attempt_at":status.last_attempt_at,
        "confirmed_parts":status.confirmed_parts,
        "total_parts":status.total_parts
    }))
    .expect("forwarding destination revision");
    stable_digest_hex(&material)
}

fn mutation_timestamp(now: &str, previous: &str) -> String {
    if now != previous {
        return now.to_owned();
    }
    OffsetDateTime::parse(previous, &Rfc3339)
        .ok()
        .and_then(|value| value.checked_add(time::Duration::nanoseconds(1)))
        .and_then(|value| value.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000000001Z".to_owned())
}

fn validate_settings(settings: &Settings) -> Result<(), (&'static str, &'static str)> {
    if settings.telegram.enabled {
        if settings.telegram.bot_token.trim().is_empty()
            || settings.telegram.chat_id.trim().is_empty()
        {
            return Err((
                "telegram_configuration_invalid",
                "telegram_configuration_invalid",
            ));
        }
        match settings
            .telegram
            .proxy
            .mode
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "system" | "direct" => {}
            "manual" if !settings.telegram.proxy.url.trim().is_empty() => {}
            "manual" => return Err(("telegram_proxy_url_invalid", "telegram_proxy_url_invalid")),
            _ => return Err(("telegram_proxy_mode_invalid", "telegram_proxy_mode_invalid")),
        }
    }
    if settings.bark.enabled {
        if settings.bark.device_key.trim().is_empty() {
            return Err(("bark_configuration_invalid", "bark_configuration_invalid"));
        }
        let server = settings.bark.server_url.trim();
        if !(server.starts_with("https://")
            || server.starts_with("http://localhost")
            || server.starts_with("http://127.0.0.1")
            || server.starts_with("http://[::1]"))
        {
            return Err(("invalid_bark_server_url", "invalid_bark_server_url"));
        }
        if !settings.bark.icon_url.trim().is_empty()
            && !settings.bark.icon_url.trim().starts_with("https://")
        {
            return Err(("bark_icon_url_invalid", "bark_icon_url_invalid"));
        }
        if settings.bark.encrypted {
            let expected = match settings.bark.encryption_algorithm.as_str() {
                "AES-128-CBC" => 16,
                "AES-256-CBC" => 32,
                _ => {
                    return Err((
                        "bark_encryption_algorithm_invalid",
                        "bark_encryption_algorithm_invalid",
                    ));
                }
            };
            if settings.bark.encryption_key.as_bytes().len() != expected {
                return Err(("bark_encryption_key_invalid", "bark_encryption_key_invalid"));
            }
        }
    }
    Ok(())
}

fn nonempty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_owned())
}

fn provider_code(code: &str) -> &'static str {
    match code {
        "telegram_config_invalid" => "telegram_configuration_invalid",
        "bark_config_invalid" => "bark_configuration_invalid",
        "telegram_proxy_invalid" => "telegram_proxy_url_invalid",
        "system_proxy_not_found" => "telegram_system_proxy_not_found",
        "system_proxy_resolution_failed" => "telegram_system_proxy_resolution_failed",
        _ => "provider_configuration_invalid",
    }
}

/// Provider test errors are already stable, redacted error codes. Keep the
/// actionable reason instead of converting API rejections and transport
/// failures into the misleading `provider_configuration_invalid` message.
fn provider_test_code(code: &str) -> String {
    match code {
        "telegram_config_invalid" => "telegram_configuration_invalid".to_owned(),
        "bark_config_invalid" => "bark_configuration_invalid".to_owned(),
        "telegram_proxy_invalid" => "telegram_proxy_url_invalid".to_owned(),
        "system_proxy_not_found" => "telegram_system_proxy_not_found".to_owned(),
        "system_proxy_resolution_failed" => "telegram_system_proxy_resolution_failed".to_owned(),
        "telegram_not_enabled"
        | "bark_not_enabled"
        | "unknown_provider"
        | "telegram_request_invalid"
        | "telegram_connect_failed"
        | "telegram_proxy_auth_required"
        | "telegram_proxy_policy_rejected"
        | "telegram_proxy_configuration_rejected"
        | "telegram_proxy_unavailable"
        | "telegram_network_error"
        | "telegram_response_invalid"
        | "telegram_response_too_large"
        | "telegram_response_read_failed"
        | "telegram_api_rejected"
        | "bark_request_invalid"
        | "bark_network_error"
        | "bark_payload_encode_failed"
        | "bark_encryption_failed"
        | "bark_response_invalid"
        | "bark_response_too_large"
        | "bark_response_read_failed"
        | "http_client_unavailable" => code.to_owned(),
        _ if safe_provider_status_code(code, "telegram_http_")
            || safe_provider_status_code(code, "bark_http_")
            || safe_provider_status_code(code, "bark_api_") =>
        {
            code.to_owned()
        }
        _ => "provider_configuration_invalid".to_owned(),
    }
}

fn safe_provider_status_code(code: &str, prefix: &str) -> bool {
    code.strip_prefix(prefix)
        .and_then(|value| value.parse::<u16>().ok())
        .is_some_and(|status| (100..=599).contains(&status))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn prepare_private_state_directory(path: &Path) -> Result<(), &'static str> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
                return Err("sms_forwarding_queue_unavailable");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| "sms_forwarding_queue_unavailable")?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|_| "sms_forwarding_queue_unavailable")?;
        }
        Err(_) => return Err("sms_forwarding_queue_unavailable"),
    }
    Ok(())
}

async fn read_json<B, T>(request: Request<B>) -> Result<T, Response<Full<Bytes>>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    T: for<'de> Deserialize<'de>,
{
    let body = crate::http_server::collect_body(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|failure| match failure {
            crate::http_server::BodyReadError::Timeout => {
                crate::http_server::body_timeout_response()
            }
            _ => error(StatusCode::BAD_REQUEST, "invalid_request_body", false),
        })?;
    serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_request_body", false))
}

fn json<T: Serialize>(status: StatusCode, value: T) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = Response::new(Full::new(Bytes::from(bytes)));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

fn test_intent_error(failure: StoreError) -> Response<Full<Bytes>> {
    match failure {
        StoreError::Configuration(code) if code == "sms_forwarding_test_request_id_conflict" => {
            error(StatusCode::CONFLICT, &code, false)
        }
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "sms_forwarding_test_store_unavailable",
            false,
        ),
    }
}

fn error(status: StatusCode, code: &str, retryable: bool) -> Response<Full<Bytes>> {
    json(
        status,
        APIError {
            error: code,
            code,
            retryable,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{Service, provider_test_code, timestamp_is_future};
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{Method, Request, StatusCode};
    use serde_json::Value;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    pub(super) fn test_root() -> std::path::PathBuf {
        let base = std::path::PathBuf::from(
            std::env::var_os("SWITCH_BUILD_DIR").expect("workspace-env build directory"),
        )
        .join("tmp/sms-forwarding-tests");
        fs::create_dir_all(&base).unwrap();
        let mut entropy = [0_u8; 16];
        getrandom::fill(&mut entropy).unwrap();
        let name = entropy
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let root = base.join(format!("sms-forwarding-test-{name}"));
        fs::create_dir(&root).unwrap();
        root
    }

    fn request(method: Method, path: &str, mut body: Value) -> Request<Full<Bytes>> {
        static CONFIGURATION_SEQUENCE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        if method == Method::POST && path == "/api/sms/forwarding/configure" {
            let sequence =
                CONFIGURATION_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let object = body.as_object_mut().expect("configuration object");
            object
                .entry("client_request_id")
                .or_insert_with(|| serde_json::json!(format!("test-request-{sequence}")));
            object
                .entry("client_sequence")
                .or_insert_with(|| serde_json::json!(sequence));
        }
        Request::builder()
            .method(method)
            .uri(path)
            .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
            .unwrap()
    }

    #[tokio::test]
    async fn queue_action_requires_caller_identity_and_complete_cas_body() {
        let root = test_root();
        let service = Service::new(&root);
        let missing = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/queue-action",
                serde_json::json!({"id":"a".repeat(64),"provider":"telegram","action":"retry"}),
            ))
            .await;
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(missing).await["code"],
            "client_request_id_required"
        );
        let incomplete = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/queue-action",
                serde_json::json!({
                    "client_request_id":"queue-action-incomplete",
                    "id":"a".repeat(64),"provider":"telegram","action":"retry"
                }),
            ))
            .await;
        assert_eq!(incomplete.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(incomplete).await["code"], "invalid_request_body");
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    async fn body_json(response: hyper::Response<Full<Bytes>>) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn durable_test_lost_http_receipt_replays_exactly_after_reopen_without_provider_send() {
        use std::sync::atomic::Ordering;
        let root = test_root();
        let service = Service::new(&root);
        *service.0.test_provider_result.lock().unwrap() = Some(Ok(()));
        let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [9_u8; 32]);
        let configured = body_json(service.handle(request(Method::POST,
            "/api/sms/forwarding/configure", serde_json::json!({
                "telegram":{"enabled":false}, "bark":{"enabled":true,
                "server_url":"https://example.invalid", "device_key":"mock-key"}, "state_key":key
            }))).await).await;
        let payload = serde_json::json!({"client_request_id":"test-lost-response",
            "provider":"  BaRk  ", "expected_configuration_id":configured["configuration_id"]});
        let response = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/test",
                payload.clone(),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        // Throw away the HTTP response after the physical mock dispatch.
        drop(response);
        let receipt = body_json(
            service
                .handle(request(
                    Method::POST,
                    "/api/sms/forwarding/test",
                    payload.clone(),
                ))
                .await,
        )
        .await;
        assert_eq!(receipt["state"], "succeeded");
        assert_eq!(receipt["terminal"], true);
        assert_eq!(receipt["dispatch_count"], 1);
        assert_eq!(service.0.test_dispatches.load(Ordering::SeqCst), 1);
        let mut changed = payload.clone();
        changed["provider"] = serde_json::json!("telegram");
        assert_eq!(
            service
                .handle(request(Method::POST, "/api/sms/forwarding/test", changed))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        let mut changed = payload.clone();
        changed["expected_configuration_id"] = serde_json::json!("new-generation");
        assert_eq!(
            service
                .handle(request(Method::POST, "/api/sms/forwarding/test", changed))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        service.shutdown().await.unwrap();
        drop(service);
        let reopened = Service::new(&root); // no active config, no provider
        let replay = body_json(
            reopened
                .handle(request(Method::POST, "/api/sms/forwarding/test", payload))
                .await,
        )
        .await;
        assert_eq!(replay, receipt);
        assert_eq!(reopened.0.test_dispatches.load(Ordering::SeqCst), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn durable_test_interrupted_dispatch_reopen_returns_unknown_receipt_without_active_provider()
     {
        let root = test_root();
        let service = Service::new(&root);
        let (_, dispatch) = service
            .0
            .test_intents
            .claim("interrupted", "bark", "old-configuration", None)
            .unwrap();
        assert!(dispatch);
        drop(service); // process loss after durable dispatch claim, before receipt
        let reopened = Service::new(&root);
        let payload = serde_json::json!({"client_request_id":"interrupted", "provider":"bark",
            "expected_configuration_id":"old-configuration"});
        let receipt = body_json(
            reopened
                .handle(request(
                    Method::POST,
                    "/api/sms/forwarding/test",
                    payload.clone(),
                ))
                .await,
        )
        .await;
        assert_eq!(receipt["state"], "unknown_no_replay");
        assert_eq!(receipt["terminal"], false);
        assert_eq!(receipt["dispatch_count"], 1);
        assert_eq!(
            body_json(
                reopened
                    .handle(request(Method::POST, "/api/sms/forwarding/test", payload))
                    .await
            )
            .await,
            receipt
        );
        assert_eq!(
            reopened
                .0
                .test_dispatches
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn durable_test_unknown_provider_result_keeps_nonterminal_fence_and_dispatches_once() {
        let root = test_root();
        let service = Service::new(&root);
        *service.0.test_provider_result.lock().unwrap() =
            Some(Err(proletarius_forwarding::DeliveryFailure {
                code: "bark_network_error".into(),
                disposition: proletarius_forwarding::DeliveryDisposition::Unknown,
                retry_after: None,
            }));
        let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [8_u8; 32]);
        let configured = body_json(service.handle(request(Method::POST,
            "/api/sms/forwarding/configure", serde_json::json!({
                "telegram":{"enabled":false}, "bark":{"enabled":true,
                "server_url":"https://example.invalid", "device_key":"mock-key"}, "state_key":key
            }))).await).await;
        let payload = serde_json::json!({"client_request_id":"unknown-test", "provider":"bark",
            "expected_configuration_id":configured["configuration_id"]});
        let receipt = body_json(
            service
                .handle(request(
                    Method::POST,
                    "/api/sms/forwarding/test",
                    payload.clone(),
                ))
                .await,
        )
        .await;
        assert_eq!(receipt["state"], "unknown_no_replay");
        assert_eq!(receipt["terminal"], false);
        for _ in 0..3 {
            let replay = body_json(
                service
                    .handle(request(
                        Method::POST,
                        "/api/sms/forwarding/test",
                        payload.clone(),
                    ))
                    .await,
            )
            .await;
            assert_eq!(replay, receipt);
        }
        assert_eq!(
            service
                .0
                .test_dispatches
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        service.shutdown().await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn configure_status_and_replay_are_idempotent_without_exposing_key() {
        let root = test_root();
        let service = Service::new(&root);
        let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [7_u8; 32]);
        let configuration = serde_json::json!({
            "telegram":{"enabled":false,"bot_token":"private-telegram","chat_id":"private-chat","proxy":{"mode":"system","url":""}},
            "bark":{"enabled":false,"server_url":"https://api.day.app","device_key":"private-bark","icon_url":"","group":"Switch-SMS","encrypted":false,"encryption_algorithm":"AES-128-CBC","encryption_key":"private-encryption"},
            "state_key":key
        });
        let first = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/configure",
                configuration.clone(),
            ))
            .await;
        assert_eq!(first.status(), StatusCode::OK);
        let first = body_json(first).await;
        let configuration_id = first["configuration_id"].as_str().unwrap().to_owned();
        assert_eq!(configuration_id.len(), 32);

        let second = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/configure",
                configuration,
            ))
            .await;
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(
            body_json(second).await["configuration_id"],
            configuration_id
        );

        let status = service
            .handle(request(
                Method::GET,
                "/api/sms/forwarding/status",
                serde_json::json!({}),
            ))
            .await;
        assert_eq!(status.status(), StatusCode::OK);
        let status = body_json(status).await;
        assert_eq!(status["configured"], false);
        assert_eq!(status["tasks"], serde_json::json!([]));
        let serialized = status.to_string();
        assert!(!serialized.contains("private-telegram"));
        assert!(!serialized.contains("private-chat"));
        assert!(!serialized.contains("private-bark"));
        assert!(!serialized.contains("private-encryption"));

        service.shutdown().await.unwrap();
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn durable_configuration_order_restart_conflict_and_test_target_are_enforced() {
        let root = test_root();
        let service = Service::new(&root);
        let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [5_u8; 32]);
        let newer = serde_json::json!({
            "client_request_id":"newer-request",
            "client_sequence":20,
            "telegram":{"enabled":false,"bot_token":"","chat_id":"","proxy":{"mode":"system","url":""}},
            "bark":{"enabled":true,"server_url":"http://127.0.0.1:1","device_key":"new-target","icon_url":"","group":"Switch-SMS","encrypted":false,"encryption_algorithm":"AES-128-CBC","encryption_key":""},
            "state_key":key
        });
        let configured = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/configure",
                newer.clone(),
            ))
            .await;
        assert_eq!(configured.status(), StatusCode::OK);
        let configured = body_json(configured).await;
        let active_id = configured["configuration_id"].as_str().unwrap().to_owned();
        assert_eq!(configured["client_request_id"], "newer-request");
        assert_eq!(configured["client_sequence"], 20);

        let mut older = newer.clone();
        older["client_request_id"] = serde_json::json!("older-request");
        older["client_sequence"] = serde_json::json!(19);
        older["bark"]["device_key"] = serde_json::json!("old-target");
        let stale = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/configure",
                older,
            ))
            .await;
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(stale).await["code"], "configuration_intent_stale");
        let status = body_json(
            service
                .handle(request(
                    Method::GET,
                    "/api/sms/forwarding/status",
                    serde_json::json!({}),
                ))
                .await,
        )
        .await;
        assert_eq!(status["configuration_id"], active_id);

        let mut conflicting = newer.clone();
        conflicting["bark"]["device_key"] = serde_json::json!("different-payload");
        let conflict = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/configure",
                conflicting,
            ))
            .await;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(conflict).await["code"],
            "configuration_intent_request_id_conflict"
        );

        // A stale configuration yields a durable terminal rejection with no
        // dispatch. The provider mock counter proves no send was attempted.
        let stale_test = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/test",
                serde_json::json!({
                    "client_request_id":"stale-test-request",
                    "provider":"bark",
                    "expected_configuration_id":"00000000000000000000000000000000"
                }),
            ))
            .await;
        assert_eq!(stale_test.status(), StatusCode::OK);
        assert_eq!(
            service
                .0
                .test_dispatches
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            body_json(stale_test).await["code"],
            "sms_forwarding_configuration_changed"
        );

        service.shutdown().await.unwrap();
        drop(service);
        let restarted = Service::new(&root);
        let replay = restarted
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/configure",
                newer,
            ))
            .await;
        assert_eq!(replay.status(), StatusCode::OK);
        let replay = body_json(replay).await;
        assert_eq!(replay["client_request_id"], "newer-request");
        assert_eq!(replay["client_sequence"], 20);
        assert_eq!(replay["configuration_intent_revision"], 1);
        restarted.shutdown().await.unwrap();
        drop(restarted);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn device_snapshot_owns_disabled_baseline_reenable_and_queue_admission() {
        let root = test_root();
        let handoffs = crate::forwarding_handoff_store::Store::open(&root).unwrap();
        let handoff = serde_json::json!({
            "schema_version": 1,
            "handoff_id": "123e4567-e89b-12d3-a456-426614174000",
            "device_sms": {"accepted_notification_ids": [], "was_enabled": null},
            "nwait_sms": {"cursor": null, "seeded": false, "was_enabled": null}
        });
        let handoff_response = {
            let _fixture = crate::forwarding_handoff_store::REQUEST_TEST_LOCK
                .lock()
                .await;
            crate::forwarding_handoff_store::handle(
                request(Method::POST, "/api/v2/sms/forwarding/handoff", handoff),
                handoffs.clone(),
            )
            .await
        };
        assert_eq!(handoff_response.status(), StatusCode::OK);
        let ledger = crate::device_forwarding_store::Store::open(&root).unwrap();
        let service = Service::new_with_admission(&root, handoffs, ledger.clone());
        let state_key =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [9_u8; 32]);
        let disabled_configuration = serde_json::json!({
            "telegram":{"enabled":false,"bot_token":"","chat_id":"","proxy":{"mode":"system","url":""}},
            "bark":{"enabled":false,"server_url":"https://api.day.app","device_key":"","icon_url":"","group":"Switch-SMS","encrypted":false,"encryption_algorithm":"AES-128-CBC","encryption_key":""},
            "state_key":state_key
        });
        assert_eq!(
            service
                .handle(request(
                    Method::POST,
                    "/api/sms/forwarding/configure",
                    disabled_configuration
                ))
                .await
                .status(),
            StatusCode::OK
        );

        let first_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let disabled = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/device-snapshot",
                serde_json::json!({"current_notification_ids":[first_id],"messages":[],"include_current_snapshot":true}),
            ))
            .await;
        assert_eq!(disabled.status(), StatusCode::OK);
        assert_eq!(body_json(disabled).await["disposition"], "disabled");

        let enabled = service.handle(request(
            Method::POST,
            "/api/sms/forwarding/configure",
            serde_json::json!({
                "telegram":{"enabled":false,"bot_token":"","chat_id":"","proxy":{"mode":"system","url":""}},
                "bark":{"enabled":true,"server_url":"http://127.0.0.1:1","device_key":"local-test-key","icon_url":"","group":"Switch-SMS","encrypted":false,"encryption_algorithm":"AES-128-CBC","encryption_key":""},
                "state_key":state_key
            }),
        )).await;
        assert_eq!(enabled.status(), StatusCode::OK);
        let configuration_id = body_json(enabled).await["configuration_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let second_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let missing_baseline = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/device-snapshot",
                serde_json::json!({
                    "expected_configuration_id":configuration_id,
                    "current_notification_ids":[],"messages":[],"include_current_snapshot":false
                }),
            ))
            .await;
        assert_eq!(missing_baseline.status(), StatusCode::CONFLICT);

        let baseline = service.handle(request(
            Method::POST,
            "/api/sms/forwarding/device-snapshot",
            serde_json::json!({
                "expected_configuration_id":configuration_id,
                "current_notification_ids":[first_id,second_id],
                "include_current_snapshot":true,
                "messages":[{"notification_id":second_id,"id":"device:2","direction":"in","sender":"65075","content":"historical","timestamp":"2026-10-04T00:00:00Z","device":"reader"}]
            }),
        )).await;
        assert_eq!(baseline.status(), StatusCode::OK);
        assert_eq!(body_json(baseline).await["disposition"], "resume_baseline");

        let third_id = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let accepted = service.handle(request(
            Method::POST,
            "/api/sms/forwarding/device-snapshot",
            serde_json::json!({
                "expected_configuration_id":configuration_id,
                "current_notification_ids":[first_id,second_id,third_id],
                "include_current_snapshot":true,
                "messages":[{"notification_id":third_id,"id":"device:3","direction":"in","sender":"65075","content":"new","timestamp":"2026-10-04T00:00:01Z","device":"reader"}]
            }),
        )).await;
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        assert_eq!(body_json(accepted).await["disposition"], "accepted");

        let fourth_id = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
        let continuation = service.handle(request(
            Method::POST,
            "/api/sms/forwarding/device-snapshot",
            serde_json::json!({
                "expected_configuration_id":configuration_id,
                "current_notification_ids":[],"include_current_snapshot":false,
                "messages":[{"notification_id":fourth_id,"id":"device:4","direction":"in","sender":"65075","content":"next batch","timestamp":"2026-10-04T00:00:02Z","device":"reader"}]
            }),
        )).await;
        assert_eq!(continuation.status(), StatusCode::ACCEPTED);
        let fifth_id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let internal_disposition = service
            .submit_device_snapshot(
                vec![
                    first_id.to_owned(),
                    second_id.to_owned(),
                    third_id.to_owned(),
                    fourth_id.to_owned(),
                    fifth_id.to_owned(),
                ],
                vec![super::DeviceSnapshotMessage {
                    notification_id: fifth_id.to_owned(),
                    id: "device:5".to_owned(),
                    direction: "in".to_owned(),
                    sender: "65075".to_owned(),
                    content: "internal producer".to_owned(),
                    timestamp: "2026-10-04T00:00:03Z".to_owned(),
                    device: "reader".to_owned(),
                }],
                false,
                true,
            )
            .await
            .unwrap();
        assert_eq!(internal_disposition, "accepted");
        let progress = ledger.current().unwrap().unwrap();
        assert_eq!(progress.was_enabled, Some(true));
        assert_eq!(progress.accepted_notification_ids.len(), 5);

        let sixth_id = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        assert_eq!(
            service
                .submit_device_snapshot_from_rust(
                    Vec::new(),
                    vec![super::DeviceSnapshotMessage {
                        notification_id: sixth_id.to_owned(),
                        id: "device:6".into(),
                        direction: "in".into(),
                        sender: "65075".into(),
                        content: "native source after Go retirement".into(),
                        timestamp: "2026-10-04T00:00:04Z".into(),
                        device: "reader".into(),
                    }],
                    false,
                    false,
                )
                .await
                .unwrap(),
            "accepted"
        );
        assert_eq!(
            ledger
                .current()
                .unwrap()
                .unwrap()
                .accepted_notification_ids
                .len(),
            6
        );

        service.shutdown().await.unwrap();
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn enabled_configuration_requires_state_key_and_rejects_bad_body() {
        let root = test_root();
        let service = Service::new(&root);
        let response = service
            .handle(request(
                Method::POST,
                "/api/sms/forwarding/configure",
                serde_json::json!({
                    "telegram":{"enabled":true,"bot_token":"token","chat_id":"chat","proxy":{"mode":"system","url":""}},
                    "bark":{"enabled":false,"server_url":"https://api.day.app","device_key":"","icon_url":"","group":"","encrypted":false,"encryption_algorithm":"AES-128-CBC","encryption_key":""},
                    "state_key":""
                }),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = body_json(response).await;
        assert_eq!(response["code"], "state_key_must_be_a_32_byte_base64_value");

        let bad_body = Request::builder()
            .method(Method::POST)
            .uri("/api/sms/forwarding/configure")
            .body(Full::new(Bytes::from_static(b"{")))
            .unwrap();
        assert_eq!(
            service.handle(bad_body).await.status(),
            StatusCode::BAD_REQUEST
        );
        service.shutdown().await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retry_timestamps_compare_instants_not_rfc3339_strings() {
        assert!(timestamp_is_future(
            "2026-10-04T13:00:00+02:00",
            "2026-10-04T10:00:00Z"
        ));
        assert!(!timestamp_is_future(
            "2026-10-04T10:30:00+02:00",
            "2026-10-04T10:00:00Z"
        ));
    }

    #[test]
    fn telegram_provider_test_keeps_redacted_actionable_failure_codes() {
        assert_eq!(
            provider_test_code("telegram_not_enabled"),
            "telegram_not_enabled"
        );
        assert_eq!(provider_test_code("telegram_http_401"), "telegram_http_401");
        assert_eq!(provider_test_code("telegram_http_403"), "telegram_http_403");
        assert_eq!(
            provider_test_code("telegram_network_error"),
            "telegram_network_error"
        );
        for code in [
            "telegram_connect_failed",
            "telegram_proxy_auth_required",
            "telegram_proxy_policy_rejected",
            "telegram_proxy_configuration_rejected",
            "telegram_proxy_unavailable",
        ] {
            assert_eq!(provider_test_code(code), code);
        }
        assert_eq!(
            provider_test_code("system_proxy_resolution_failed"),
            "telegram_system_proxy_resolution_failed"
        );
        assert_eq!(
            provider_test_code("telegram_http_999"),
            "provider_configuration_invalid"
        );
        assert_eq!(
            provider_test_code("token=must-never-be-echoed"),
            "provider_configuration_invalid"
        );
    }

    #[test]
    fn state_directory_symlink_is_rejected_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let root = test_root();
        let target = root.join("outside-target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.join("sms-forwarding");
        symlink(&target, &link).unwrap();
        assert_eq!(
            super::prepare_private_state_directory(&link),
            Err("sms_forwarding_queue_unavailable")
        );
        assert_eq!(
            fs::symlink_metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
#[path = "sms_forwarding/retirement_tests.rs"]
mod retirement_tests;

#[cfg(test)]
#[path = "sms_forwarding/attempt_ownership_tests.rs"]
mod attempt_ownership_tests;

#[cfg(test)]
#[path = "sms_forwarding/queue_io_integration_tests.rs"]
mod queue_io_integration_tests;
