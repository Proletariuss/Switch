//! Public device-SMS submission owner. Only retained Rust physical/session
//! capabilities authorize submission; retired process bindings expire.
use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Body};
use proletarius_store::{ArchiveStore, StoreError};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Visitor},
};
use serde_json::{Value, json};
#[cfg(test)]
use std::path::PathBuf;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

#[cfg(test)]
const BRIDGE_PATH: &str = "/api/internal/device-sms-route";
const RUST_USB_BINDING_PREFIX: &str = "rust_usb_at:";
const RUST_USB_SESSION_BINDING_PREFIX: &str = "rust_usb_at_session:";
const RUST_MODEM_BINDING_PREFIX: &str = "rust_modem:";
const RUST_MODEM_GENERATION_BINDING_PREFIX: &str = "rust_modem_sms_generation:";
const RUST_MODEM_SESSION_BINDING_PREFIX: &str = "rust_modem_session_v2:";
const RUST_READER_IMS_BINDING_PREFIX: &str = "rust_reader_ims:";
const RUST_MODULE_IMS_BINDING_PREFIX: &str = "rust_module_ims:";
const RPC_TIMEOUT: Duration = Duration::from_secs(8);
const EXECUTE_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_BODY: usize = 1024 * 1024;

mod native_module;
mod native_reader;
mod native_serial;
mod native_usb;
pub(crate) mod owner;
use native_module::RuntimeModuleSms;
use native_reader::{ReaderSms, RuntimeReaderSms};
use native_serial::{serial_binding_namespace, serial_session_binding};

#[derive(Clone)]
struct Service {
    job: Option<owner::Job>,
    #[cfg(test)]
    shutdown_hooks: Arc<ShutdownTestHooks>,
    #[cfg(test)]
    archive_executor: Option<Arc<crate::archive_io::IsolatedTestExecutor>>,
    // Explicit fake bridge for durable-ledger race tests; absent from production.
    #[cfg(test)]
    compatibility_fixture: bool,
    store: crate::device_send_store::Store,
    archive: ArchiveStore,
    #[cfg(test)]
    socket: PathBuf,
    #[cfg(test)]
    app_token: Option<String>,
    #[cfg(test)]
    internal_token: String,
    usb_at: crate::usb_at::Owner,
    modem_actor: crate::modem_actor::ModemActor,
    module_radio: crate::module_radio::Service,
    #[cfg(test)]
    availability: crate::availability::BackendAvailability,
    native_sms: Arc<dyn ReaderSms>,
    native_module_sms: Arc<dyn ReaderSms>,
    io_timeout: Duration,
    claims: Arc<Semaphore>,
}

#[cfg(test)]
#[derive(Default)]
struct ShutdownTestHooks {
    ledger_worker: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    before_finish: std::sync::Mutex<Option<Arc<ShutdownTestGate>>>,
    ims_poll: std::sync::Mutex<Option<Arc<ShutdownTestGate>>>,
}
#[cfg(test)]
#[derive(Default)]
struct ShutdownTestGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[cfg(test)]
impl ShutdownTestGate {
    async fn checkpoint(slot: &std::sync::Mutex<Option<Arc<Self>>>) {
        let gate = slot.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }
}

struct ClaimJob {
    result: tokio::sync::oneshot::Receiver<Result<Record>>,
    accepted: tokio::sync::oneshot::Sender<()>,
    _finished: tokio::task::JoinHandle<owner::Output<()>>,
}

#[derive(Default)]
struct SendRequest {
    identity_version: u8,
    intent_nonce: String,
    client_request_id: String,
    phone: String,
    message: String,
    device: String,
    expected_subscription_id: String,
}

// Go encoding/json matches string field names case-insensitively and treats
// null string fields as unchanged. Preserve that public wire behavior while
// rejecting unknown fields and retaining the original message whitespace.
impl<'de> Deserialize<'de> for SendRequest {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct RequestVisitor;
        impl<'de> Visitor<'de> for RequestVisitor {
            type Value = SendRequest;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("one JSON object")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(SendRequest::default())
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut request = SendRequest::default();
                while let Some(key) = map.next_key::<String>()? {
                    if key.eq_ignore_ascii_case("identity_version") {
                        let version = map.next_value::<u8>()?;
                        if !matches!(version, 1 | 2) {
                            return Err(serde::de::Error::custom("unsupported SMS identity version"));
                        }
                        request.identity_version = version;
                        continue;
                    }
                    let field = match key.to_ascii_lowercase().as_str() {
                        "intent_nonce" => &mut request.intent_nonce,
                        "client_request_id" => &mut request.client_request_id,
                        "phone" => &mut request.phone,
                        "message" => &mut request.message,
                        "device" => &mut request.device,
                        "expected_subscription_id" => &mut request.expected_subscription_id,
                        _ => return Err(serde::de::Error::custom("unknown SMS request field")),
                    };
                    if let Some(value) = map.next_value::<Option<String>>()? {
                        *field = value;
                    }
                }
                Ok(request)
            }
        }
        de.deserialize_any(RequestVisitor)
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct Record {
    #[serde(default = "legacy_identity_version")]
    identity_version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    intent_nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_subscription_id: Option<String>,
    client_request_id: String,
    phone: String,
    message: String,
    device: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    subscription_id: String,
    state: String,
    segments: usize,
    archive_confirmed: bool,
    can_submit: bool,
    created_at_ms: u64,
    updated_at_ms: u64,
    binding: String,
    attempt: String,
}

fn legacy_identity_version() -> u8 { 1 }

#[derive(Deserialize)]
struct Page {
    intents: Vec<Record>,
    next_cursor: Option<String>,
}
#[derive(Deserialize)]
struct Binding {
    binding: String,
    device: String,
    #[serde(default)]
    subscription_id: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    expected_iccid: String,
}

#[derive(Debug)]
struct Failure(StatusCode, Value);
type Result<T, E = Failure> = std::result::Result<T, E>;
fn coded(status: StatusCode, code: &str, error: &str) -> Failure {
    // Match Go codedErrorPayload: recoverable=false is omitted, and the
    // public SMS API never used the unrelated gateway retryable field.
    Failure(status, json!({"error":error,"code":code}))
}
fn unavailable() -> Failure {
    coded(
        StatusCode::SERVICE_UNAVAILABLE,
        "sms_intent_store_unavailable",
        "短信发送记录未确认，未授权新提交；请先查询记录",
    )
}
fn conflict() -> Failure {
    coded(
        StatusCode::CONFLICT,
        "sms_intent_conflict",
        "发送记录与当前目标或内容不一致",
    )
}
fn unknown() -> Failure {
    coded(
        StatusCode::BAD_GATEWAY,
        "sms_send_outcome_unknown",
        "短信结果未确认，请查询原发送记录；再次发送可能重复",
    )
}
fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
fn ledger_result<T: serde::de::DeserializeOwned>(
    result: std::result::Result<Value, StoreError>,
) -> Result<T> {
    let value = result.map_err(|error| match &error {
        StoreError::Configuration(code) if code == "send_intent_not_found" => coded(
            StatusCode::NOT_FOUND,
            "sms_intent_not_found",
            "未找到短信发送记录，请确认后重新准备",
        ),
        StoreError::Configuration(code) if code == "send_intent_conflict" => conflict(),
        StoreError::Configuration(code) if matches!(code.as_str(),
            "mutation_identity_retired" | "mutation_identity_not_issued"
            | "mutation_identity_upgrade_required" | "mutation_authority_mismatch"
            | "mutation_sequence_exhausted" | "ledger_full" | "invalid_request") => {
            Failure(if code == "mutation_identity_retired" { StatusCode::GONE }
                else if code == "invalid_request" { StatusCode::BAD_REQUEST }
                else { StatusCode::CONFLICT },
                json!({"code":code,"error":code,"retryable":false,"request_dispatched":false}))
        }
        _ => unavailable(),
    })?;
    serde_json::from_value(value).map_err(|_| unavailable())
}
impl SendRequest {
    fn normalize(&mut self) -> bool {
        self.phone = self.phone.trim().to_owned();
        self.device = self.device.trim().to_lowercase();
        !self.phone.is_empty()
            && self.phone.len() <= 128
            && !self.message.trim().is_empty()
            && self.message.len() <= 64 * 1024
            && (self.client_request_id.is_empty() || valid_token(&self.client_request_id))
            && (self.expected_subscription_id.is_empty()
                || valid_token(&self.expected_subscription_id))
    }
    fn matches(&self, r: &Record) -> bool {
        if self.identity_version.max(1) != r.identity_version
            || self.phone != r.phone || self.message != r.message {
            return false;
        }
        if r.identity_version == 2 {
            self.device == r.device
                && r.intent_nonce.as_deref() == Some(self.intent_nonce.as_str())
                && r.expected_subscription_id.as_deref() == Some(self.expected_subscription_id.as_str())
        } else {
            (self.device.is_empty() || self.device == r.device)
                && (self.expected_subscription_id.is_empty() || self.expected_subscription_id == r.subscription_id)
        }
    }
}

impl Service {
    fn spawn_owned<T, F>(&self, work: F) -> Result<tokio::task::JoinHandle<owner::Output<T>>>
    where
        T: Send + 'static,
        F: std::future::Future<Output = T> + Send + 'static,
    {
        let job = match &self.job {
            Some(job) => job.clone(),
            None => self.store.send_owner().admit().map_err(owner_failure)?,
        };
        Ok(job.spawn(work))
    }

    #[cfg(test)]
    fn compatibility_write_ready(&self) -> Result<()> {
        if self.availability.snapshot().phase != crate::BackendPhase::Ready
            || !crate::socket_is_live(&self.socket)
        {
            return Err(Failure(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error":"Go 后端正在恢复；为避免重复执行，写入请求未自动重放","code":"go_backend_restarting_write_not_replayed","retryable":false}),
            ));
        }
        Ok(())
    }
    fn input_failure(&self, failure: Failure) -> Failure {
        #[cfg(test)]
        if self.compatibility_fixture {
            return self.compatibility_write_ready().err().unwrap_or(failure);
        }
        failure
    }

    async fn ledger<T: serde::de::DeserializeOwned>(
        &self,
        action: &'static str,
        body: Value,
    ) -> Result<T> {
        let store = self.store.clone();
        #[cfg(test)]
        let hooks = self.shutdown_hooks.clone();
        let operation = move || {
            #[cfg(test)]
            if let Some(hook) = hooks.ledger_worker.lock().unwrap().take() {
                hook();
            }
            ledger_storage_result(store.apply(action, body))
        };
        #[cfg(test)]
        let io: std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = std::result::Result<
                            std::result::Result<Value, StoreError>,
                            StoreError,
                        >,
                    > + Send,
            >,
        > = match &self.archive_executor {
            Some(executor) => Box::pin(executor.run_durable(operation, self.job.clone())),
            None => Box::pin(crate::archive_io::run_durable(operation, self.job.clone())),
        };
        #[cfg(not(test))]
        let io = crate::archive_io::run_durable(operation, self.job.clone());
        let result = tokio::time::timeout(self.io_timeout, io)
            .await
            .map_err(|_| unavailable())?;
        ledger_result(result.and_then(|business| business))
    }

    fn start_claim(&self, record: &Record, attempt: String) -> Result<ClaimJob> {
        // A timed-out claim may still commit. Keep its actual result and only
        // release the route if this attempt won but nobody accepted permission.
        // This separate budget lasts through the late write and cleanup RPC.
        let permit = self
            .claims
            .clone()
            .try_acquire_owned()
            .map_err(|_| unavailable())?;
        let service = self.clone();
        let store = self.store.clone();
        let binding = record.binding.clone();
        let body = json!({"identity_version":record.identity_version,"intent_nonce":record.intent_nonce,"client_request_id":record.client_request_id,"binding":binding,"attempt":attempt});
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let finished = self.spawn_owned(async move {
            let _permit = permit;
            let result: Result<Record> =
                ledger_result(crate::archive_io::run(move || store.apply("claim", body)).await);
            let owns_permission = result
                .as_ref()
                .is_ok_and(|r| r.can_submit && r.attempt == attempt);
            let delivered = result_tx.send(result).is_ok();
            if (!delivered || accepted_rx.await.is_err()) && owns_permission {
                service.release(&binding).await;
            }
        })?;
        Ok(ClaimJob {
            result: result_rx,
            accepted: accepted_tx,
            _finished: finished,
        })
    }
    async fn claim(&self, record: &Record, attempt: String) -> Result<Record> {
        let job = self.start_claim(record, attempt)?;
        let result = tokio::time::timeout(self.io_timeout, job.result)
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
        // No await between accepting the grant and returning it to submit.
        job.accepted.send(()).map_err(|_| unavailable())?;
        result
    }
    #[cfg(test)]
    async fn rpc(&self, body: Value, duration: Duration) -> Result<Value> {
        if matches!(body["action"].as_str(), Some("capture" | "execute")) {
            self.compatibility_write_ready()?;
        }
        let (status, value) = crate::upstream::post_internal_json(
            &self.socket,
            self.app_token.as_deref(),
            &self.internal_token,
            BRIDGE_PATH,
            &body,
            duration,
        )
        .await
        .map_err(|_| unavailable())?;
        if !status.is_success() {
            // Only explicit coded application errors cross this boundary.
            if value["code"].is_string()
                && value["error"].is_string()
                && value.get("recoverable").is_none_or(Value::is_boolean)
                && value.get("retryable").is_none_or(Value::is_boolean)
            {
                return Err(Failure(status, value));
            }
            return Err(unavailable());
        }
        Ok(value)
    }
    async fn release(&self, binding: &str) {
        if binding.starts_with(RUST_READER_IMS_BINDING_PREFIX)
            || binding.starts_with(RUST_MODULE_IMS_BINDING_PREFIX)
            || usb_route_session_generation(binding).is_some()
            || binding.starts_with(RUST_MODEM_SESSION_BINDING_PREFIX)
            || binding.starts_with(RUST_MODEM_GENERATION_BINDING_PREFIX)
        {
            return;
        }
        #[cfg(test)]
        if self.compatibility_fixture {
            let binding = go_binding(binding);
            let _ = self
                .rpc(json!({"action":"release","binding":binding}), RPC_TIMEOUT)
                .await;
        }
    }

    async fn valid(&self, bindings: Vec<String>) -> BTreeMap<String, bool> {
        let mut result = BTreeMap::new();
        #[cfg(test)]
        let mut compatibility_bindings = Vec::new();
        let vowifi_owns_channel = self.module_radio.vowifi_owns_sim_channel();
        for binding in bindings {
            if let Some(token) = binding.strip_prefix(RUST_READER_IMS_BINDING_PREFIX) {
                let valid = self.native_sms.resolve(token).is_ok();
                result.insert(binding, valid);
            } else if let Some(token) = binding.strip_prefix(RUST_MODULE_IMS_BINDING_PREFIX) {
                result.insert(
                    binding.clone(),
                    self.native_module_sms.resolve(token).is_ok(),
                );
            } else if let Some((session, generation)) = usb_route_session_generation(&binding) {
                let valid = self.current_usb_binding(session, generation) && !vowifi_owns_channel;
                result.insert(binding, valid);
            } else if serial_binding_namespace(&binding) {
                let valid = match serial_session_binding(&binding) {
                    Some(expected) => self.current_serial_binding(expected).await,
                    None => false, // Legacy generation-only and malformed capabilities expire.
                };
                result.insert(binding, valid);
            } else {
                #[cfg(test)]
                if self.compatibility_fixture {
                    compatibility_bindings.push(binding);
                    continue;
                }
                result.insert(binding, false);
            }
        }
        #[cfg(test)]
        if !compatibility_bindings.is_empty() {
            return self
                .valid_compatibility_fixture(result, compatibility_bindings)
                .await;
        }
        result
    }
    #[cfg(test)]
    async fn valid_compatibility_fixture(
        &self,
        mut result: BTreeMap<String, bool>,
        compatibility_bindings: Vec<String>,
    ) -> BTreeMap<String, bool> {
        let bridge_bindings = compatibility_bindings
            .iter()
            .map(|binding| go_binding(binding).to_owned())
            .collect::<Vec<_>>();
        match self
            .rpc(
                json!({"action":"validate","bindings":bridge_bindings}),
                RPC_TIMEOUT,
            )
            .await
        {
            Ok(value) => {
                let raw: BTreeMap<String, bool> =
                    serde_json::from_value(value["valid"].clone()).unwrap_or_default();
                compatibility_bindings
                    .into_iter()
                    .map(|binding| {
                        let accepted = raw.get(go_binding(&binding)).copied().unwrap_or(false);
                        (binding, accepted)
                    })
                    .for_each(|(binding, accepted)| {
                        result.insert(binding, accepted);
                    });
                result
            }
            Err(_) => {
                for binding in compatibility_bindings {
                    result.insert(binding, false);
                }
                result
            }
        }
    }
    fn superseded_binding(&self, record: &Record) -> bool {
        if record.device == "dji"
            && (self.modem_actor.native_serial_managed()
                || self.module_radio.configured_usb_transport() == Some(false))
            && !record
                .binding
                .starts_with(RUST_MODEM_SESSION_BINDING_PREFIX)
            && !record.binding.starts_with(RUST_MODULE_IMS_BINDING_PREFIX)
        {
            return true;
        }
        (record.device == "reader"
            && self.native_sms.has_intent()
            && !record.binding.starts_with(RUST_READER_IMS_BINDING_PREFIX))
            || (record.device == "dji"
                && self.native_module_sms.has_intent()
                && !record.binding.starts_with(RUST_MODULE_IMS_BINDING_PREFIX))
    }
    async fn native_dji_capture(&self, body: &SendRequest) -> Option<Binding> {
        // The configured compatibility USB route retains its existing capture.
        // Serial capture is authoritative (including not-ready) in native_serial.
        if body.device != "dji"
            || !body.expected_subscription_id.is_empty()
            || self.module_radio.vowifi_owns_sim_channel()
            || self.native_module_sms.has_intent()
        {
            return None;
        }
        let session = self.module_radio.usb_transport_session()?;
        let (session, generation) = matching_usb_transport_binding(
            Some(&session),
            self.usb_at.active_sim_identity_binding_current(),
        )?;
        Some(Binding {
            binding: format!("{RUST_USB_SESSION_BINDING_PREFIX}{session}:{generation}"),
            device: "dji".into(),
            subscription_id: String::new(),
            owner: "rust_usb_at".into(),
            expected_iccid: String::new(),
        })
    }
    async fn capture(&self, body: &SendRequest) -> Result<Binding> {
        if self.module_radio.module_recovery().is_admission_revoked()
            && (body.device == "dji" || (body.device.is_empty() && !self.native_sms.has_intent()))
        {
            return Err(coded(
                StatusCode::CONFLICT,
                crate::module_recovery::CODE,
                crate::module_recovery::MESSAGE,
            ));
        }
        // The module owns DJI routing from startup through actual cleanup,
        // including not-ready states. Never select AT or Go as a fallback.
        if self.native_module_sms.has_intent() && (body.device.is_empty() || body.device == "dji") {
            if self.native_sms.has_intent() {
                return Err(conflict());
            }
            return self.capture_native_module(body);
        }
        if self.native_sms.has_intent() {
            if body.device.is_empty() {
                return Err(Failure(
                    StatusCode::CONFLICT,
                    json!({"error":"原生读卡器 VoWiFi 已占用短信通道，请明确选择 reader 或 dji","code":"sms_device_selection_required","recoverable":true}),
                ));
            }
            if body.device == "reader" {
                let route = self
                    .native_sms
                    .capture()
                    .map_err(|_| native_reader::expired())?;
                if !body.expected_subscription_id.is_empty()
                    && body.expected_subscription_id != route.subscription_id
                {
                    return Err(conflict());
                }
                let binding = format!("{RUST_READER_IMS_BINDING_PREFIX}{}", route.binding);
                if !valid_token(&binding)
                    || (!route.subscription_id.is_empty() && !valid_token(&route.subscription_id))
                {
                    return Err(unavailable());
                }
                return Ok(Binding {
                    binding,
                    device: "reader".into(),
                    subscription_id: route.subscription_id,
                    owner: "rust_reader_ims".into(),
                    expected_iccid: String::new(),
                });
            }
        }
        if self.native_serial_selected(body) {
            return self.capture_native_serial(body).await;
        }
        if self.native_usb_selected(body) {
            return self.capture_native_usb(body).await;
        }
        if let Some(binding) = self.native_dji_capture(body).await {
            return Ok(binding);
        }
        #[cfg(test)]
        if self.compatibility_fixture {
            return self.capture_compatibility_fixture(body).await;
        }
        Err(match body.device.as_str() {
            "" => Failure(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"code":"sms_device_unavailable","error":"当前没有可用的短信发送设备","recoverable":true}),
            ),
            "dji" | "reader" => Failure(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"code":"sms_device_not_ready","error":"短信发送设备尚未就绪","recoverable":true}),
            ),
            _ => coded(
                StatusCode::BAD_REQUEST,
                "invalid_sms_device",
                "无效的短信发送设备",
            ),
        })
    }
    #[cfg(test)]
    async fn capture_compatibility_fixture(&self, body: &SendRequest) -> Result<Binding> {
        let observation = self.rpc(json!({"action":"capture","phone":body.phone,"message":body.message,"device":body.device,"expected_subscription_id":body.expected_subscription_id}),RPC_TIMEOUT).await;
        // Transport configuration may complete while a compatibility request
        // is in flight. Native serial now owns even errors/not-ready facts.
        if self.native_serial_selected(body) {
            if let Ok(value) = &observation {
                if let Some(token) = value["binding"].as_str().filter(|token| valid_token(token)) {
                    self.release(token).await;
                }
            }
            return self.capture_native_serial(body).await;
        }
        let binding: Binding = serde_json::from_value(observation?).map_err(|_| unavailable())?;
        if (binding.device == "reader" && self.native_sms.has_intent())
            || (binding.device == "dji" && self.native_module_sms.has_intent())
        {
            if valid_token(&binding.binding) {
                self.release(&binding.binding).await;
            }
            return Err(native_reader::expired());
        }
        if !valid_token(&binding.binding)
            || !matches!(binding.device.as_str(), "reader" | "dji")
            || !matches!(binding.owner.as_str(), "" | "rust_usb_at" | "rust_modem")
            || binding.owner == "rust_usb_at"
                && (!(10..=20).contains(&binding.expected_iccid.len())
                    || !binding
                        .expected_iccid
                        .bytes()
                        .all(|byte| byte.is_ascii_digit()))
            || binding.owner != "rust_usb_at" && !binding.expected_iccid.is_empty()
            || (!binding.subscription_id.is_empty() && !valid_token(&binding.subscription_id))
        {
            if valid_token(&binding.binding) {
                self.release(&binding.binding).await;
            }
            return Err(unavailable());
        }
        if binding.owner == "rust_modem" {
            // A Go capability contains no exact serial session. Never upgrade
            // its generation onto whichever actor session happens to be live.
            self.release(&binding.binding).await;
            return Err(native_reader::expired());
        }
        if binding.owner == "rust_usb_at" {
            if self.module_radio.vowifi_owns_sim_channel() {
                self.release(&binding.binding).await;
                return Err(coded(
                    StatusCode::CONFLICT,
                    "sms_intent_binding_expired",
                    "原发送通道已变化，请核对后重新准备；没有重放这条短信",
                ));
            }
            let Some((session, generation)) = self
                .usb_at
                .active_sim_identity_binding(&binding.expected_iccid)
            else {
                self.release(&binding.binding).await;
                return Err(unavailable());
            };
            // Rust now fences a USB route by its own physical session and SIM
            // identity generation. The Go bridge supplied the initial device
            // observation only; no process-local route token survives capture.
            self.release(&binding.binding).await;
            return Ok(Binding {
                binding: format!("{RUST_USB_SESSION_BINDING_PREFIX}{session}:{generation}"),
                expected_iccid: String::new(),
                ..binding
            });
        }
        Ok(binding)
    }
    async fn prepare(
        &self,
        body: &SendRequest,
        cancel: &AtomicBool,
        captured: Option<Binding>,
    ) -> Result<Record> {
        let existing: Result<Record> = self
            .ledger("get", json!({"client_request_id":body.client_request_id}))
            .await;
        match existing {
            Ok(record) if record.state == "reserved" => {
                if !body.matches(&record) {
                    if let Some(binding) = captured { self.release(&binding.binding).await; }
                    return Err(conflict());
                }
            }
            Ok(record) => {
                if let Some(binding) = captured {
                    self.release(&binding.binding).await;
                }
                return if body.matches(&record) {
                    Ok(record)
                } else {
                    Err(conflict())
                };
            }
            Err(failure) if failure.0 != StatusCode::NOT_FOUND => {
                if let Some(binding) = captured {
                    self.release(&binding.binding).await;
                }
                return Err(failure);
            }
            Err(_) => {
                if self.store.is_v2() {
                    if let Some(binding) = captured { self.release(&binding.binding).await; }
                    return Err(Failure(StatusCode::CONFLICT, json!({
                        "code":if body.identity_version == 2 { "mutation_identity_not_issued" } else { "mutation_identity_upgrade_required" },"error":"新请求需要持久签发身份",
                        "retryable":false,"request_dispatched":false
                    })));
                }
            }
        }
        if cancel.load(Ordering::Acquire) {
            if let Some(binding) = captured {
                self.release(&binding.binding).await;
            }
            return Err(unavailable());
        }
        let binding = match captured {
            Some(binding) => binding,
            None => self.capture(body).await?,
        };
        if cancel.load(Ordering::Acquire) {
            self.release(&binding.binding).await;
            return Err(unavailable());
        }
        let stored_binding = match binding.owner.as_str() {
            "rust_module_ims" => {
                let token = binding
                    .binding
                    .strip_prefix(RUST_MODULE_IMS_BINDING_PREFIX)
                    .ok_or_else(native_reader::expired)?;
                self.native_module_sms
                    .resolve(token)
                    .map_err(|_| native_reader::expired())?;
                binding.binding.clone()
            }
            "rust_reader_ims" => {
                let token = binding
                    .binding
                    .strip_prefix(RUST_READER_IMS_BINDING_PREFIX)
                    .ok_or_else(native_reader::expired)?;
                self.native_sms
                    .resolve(token)
                    .map_err(|_| native_reader::expired())?;
                binding.binding.clone()
            }
            "rust_usb_at" => {
                if let Some((session, generation)) = usb_route_session_generation(&binding.binding)
                {
                    if !self.current_usb_binding(session, generation) {
                        return Err(unavailable());
                    }
                    binding.binding.clone()
                } else {
                    #[cfg(test)]
                    if self.compatibility_fixture {
                        // Read old durable intents created before the Rust session
                        // binding existed. Their Go capability still expires on
                        // process restart and remains non-replayable.
                        let Some(generation) = self
                            .usb_at
                            .active_sim_identity_generation(&binding.expected_iccid)
                        else {
                            self.release(&binding.binding).await;
                            return Err(unavailable());
                        };
                        format!("{RUST_USB_BINDING_PREFIX}{generation}:{}", binding.binding)
                    } else {
                        return Err(native_reader::expired());
                    }
                    #[cfg(not(test))]
                    {
                        return Err(native_reader::expired());
                    }
                }
            }
            "rust_modem" => {
                let expected =
                    serial_session_binding(&binding.binding).ok_or_else(native_reader::expired)?;
                if !self.current_serial_binding(expected).await {
                    return Err(native_reader::expired());
                }
                binding.binding.clone()
            }
            _ => {
                #[cfg(test)]
                if self.compatibility_fixture {
                    binding.binding.clone()
                } else {
                    return Err(native_reader::expired());
                }
                #[cfg(not(test))]
                {
                    return Err(native_reader::expired());
                }
            }
        };
        let result: Result<Record> = self.ledger("prepare",json!({"identity_version":body.identity_version.max(1),"intent_nonce":body.intent_nonce,"client_request_id":body.client_request_id,"phone":body.phone,"message":body.message,"device":binding.device,"expected_subscription_id":body.expected_subscription_id,"subscription_id":binding.subscription_id,"binding":stored_binding})).await;
        if !result.as_ref().is_ok_and(|r| r.binding == stored_binding) {
            self.release(&binding.binding).await;
        }
        result
    }
    async fn project(&self, records: Vec<Record>) -> Vec<Value> {
        let bindings = records
            .iter()
            .filter(|r| r.state == "prepared" && (r.identity_version == 2 || !self.store.is_v2()) && !self.superseded_binding(r))
            .map(|r| r.binding.clone())
            .collect::<Vec<_>>();
        let valid = if bindings.is_empty() {
            BTreeMap::new()
        } else {
            self.valid(bindings).await
        };
        records
            .into_iter()
            .map(|mut record| {
                record.can_submit = record.state == "prepared"
                    && (record.identity_version == 2 || !self.store.is_v2())
                    && valid.get(&record.binding) == Some(&true)
                    && !self.superseded_binding(&record);
                if record.state == "submitting" {
                    record.state = "unknown".into();
                }
                let mut value = serde_json::to_value(record).expect("SMS record serialization");
                value.as_object_mut().unwrap().remove("binding");
                value.as_object_mut().unwrap().remove("attempt");
                value
            })
            .collect()
    }
    async fn reconcile_archive(&self, record: Record) -> Result<Value> {
        if record.archive_confirmed {
            return outcome(record);
        }
        let item = archive_item(&record).ok_or_else(unknown)?;
        let restored = tokio::time::timeout(
            self.io_timeout,
            crate::message_archive::reconcile_device_send_retained(
                self.archive.clone(), self.store.clone(), item,
                record.attempt.clone(), self.job.clone(),
            ),
        ).await;
        if !matches!(restored, Ok(Ok(_))) {
            return outcome(record);
        }
        let confirmed = self.ledger("get", json!({"client_request_id":record.client_request_id})).await?;
        outcome(confirmed)
    }

    async fn submit(&self, record: Record, cancel: &AtomicBool) -> Result<Value> {
        // This branch is archive-only: it cannot capture, validate, claim,
        // release, or execute a physical route, including after daemon restart.
        if record.state == "accepted" {
            return self.reconcile_archive(record).await;
        }
        // Existing accepted/unknown outcomes are never rewritten by quarantine.
        // A prepared module intent must be rejected before its durable claim.
        if record.state == "prepared"
            && record.device == "dji"
            && self.module_radio.module_recovery().is_admission_revoked()
        {
            return Err(coded(
                StatusCode::CONFLICT,
                crate::module_recovery::CODE,
                crate::module_recovery::MESSAGE,
            ));
        }
        if record.state == "prepared" && self.superseded_binding(&record) {
            if record.binding.starts_with(RUST_MODEM_BINDING_PREFIX) {
                self.release(&record.binding).await;
            }
            return Err(native_reader::expired());
        }
        #[cfg(test)]
        if self.compatibility_fixture && compatibility_binding(&record.binding) {
            // Reject compatibility writes before consuming their durable
            // permission. RPC execute checks again at the submit boundary.
            self.compatibility_write_ready()?;
        }
        if record.state != "prepared" {
            return outcome(record);
        }
        if self
            .valid(vec![record.binding.clone()])
            .await
            .get(&record.binding)
            != Some(&true)
        {
            if record.binding.starts_with(RUST_MODEM_BINDING_PREFIX) {
                self.release(&record.binding).await;
            }
            return Err(coded(
                StatusCode::CONFLICT,
                "sms_intent_binding_expired",
                "原发送通道已变化，请核对后重新准备；没有重放这条短信",
            ));
        }
        if cancel.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let attempt = crate::multistandby_store::random_token().map_err(|_| unavailable())?;
        // Permission is consumed atomically before the only hardware execute.
        // A failed/uncertain claim never authorizes I/O, even if a write landed.
        let claimed = self.claim(&record, attempt.clone()).await?;
        if !claimed.can_submit {
            return outcome(claimed);
        }
        if record.binding.starts_with(RUST_READER_IMS_BINDING_PREFIX)
            || record.binding.starts_with(RUST_MODULE_IMS_BINDING_PREFIX)
        {
            if cancel.load(Ordering::Acquire) {
                return self
                    .finish_claimed_send(record, attempt, Err(unknown()))
                    .await;
            }
            let service = self.clone();
            let task = self.spawn_owned(async move {
                let executed = service.execute_native_ims(&record, &attempt).await;
                service.finish_claimed_send(record, attempt, executed).await
            })?;
            return await_supervisor(task, EXECUTE_TIMEOUT).await;
        }
        if let Some(expected_binding) = serial_session_binding(&record.binding) {
            if cancel.load(Ordering::Acquire) {
                return self
                    .finish_claimed_send(record, attempt, Err(unknown()))
                    .await;
            }
            // Preserve the public response budget while keeping the serial
            // transaction and radio reservation alive after the waiter times
            // out. This detached supervisor alone finalizes the durable claim
            // and records the final outcome only after actor settlement.
            let service = self.clone();
            let task_record = record.clone();
            let task_attempt = attempt.clone();
            let task = self.spawn_owned(async move {
                let executed = match service
                    .module_radio
                    .submit_device_sms(
                        task_record.phone.clone(),
                        task_record.message.clone(),
                        expected_binding,
                    )
                    .await
                {
                    Ok(outcome) => Ok(json!({
                        "accepted": outcome.complete,
                        // The existing Go modem route records one accepted
                        // submission, not the actor's PDU segment count.
                        "segments": if outcome.complete { 1 } else { 0 },
                    })),
                    Err(_) => Err(unknown()),
                };
                service
                    .finish_claimed_send(task_record, task_attempt, executed)
                    .await
            })?;
            return await_supervisor(task, EXECUTE_TIMEOUT).await;
        }
        let executed = if cancel.load(Ordering::Acquire) {
            Err(unknown())
        } else if let Some(expected_generation) = usb_route_generation(&record.binding) {
            let Some(expected_session) = usb_route_session_generation(&record.binding)
                .map(|(session, _)| session.to_owned())
                .or_else(|| self.usb_at.active_sim_identity_session(expected_generation))
            else {
                return self
                    .finish_claimed_send(record, attempt, Err(unknown()))
                    .await;
            };
            // USB AT transport calls run in a blocking owner worker and remain
            // active if this request future is dropped. Keep the paired module
            // reservation and durable claim alive in the same detached owner
            // task until that physical worker settles; timing out only the
            // HTTP waiter must not strand the radio reservation or admit an
            // overlapping eSIM/VoWiFi operation.
            let service = self.clone();
            let task_record = record.clone();
            let task_attempt = attempt.clone();
            let usb_at = self.usb_at.clone();
            let module_radio = self.module_radio.clone();
            let task = self.spawn_owned(async move {
                let executed =
                    if usb_at.owns_native_sms_binding(&expected_session, expected_generation) {
                        service
                            .execute_native_usb(
                                expected_session,
                                expected_generation,
                                task_record.phone.clone(),
                                task_record.message.clone(),
                            )
                            .await
                    } else {
                        match module_radio
                            .submit_usb_device_sms(
                                usb_at,
                                expected_session,
                                task_record.phone.clone(),
                                task_record.message.clone(),
                                expected_generation,
                            )
                            .await
                        {
                            Ok(outcome) => Ok(json!({
                                "accepted": outcome.complete,
                                "segments": outcome.accepted_segments,
                            })),
                            Err(_) => Err(unknown()),
                        }
                    };
                service
                    .finish_claimed_send(task_record, task_attempt, executed)
                    .await
            })?;
            return await_supervisor(task, EXECUTE_TIMEOUT).await;
        } else {
            #[cfg(test)]
            if self.compatibility_fixture {
                self.rpc(
                    json!({"action":"execute","binding":record.binding}),
                    EXECUTE_TIMEOUT,
                )
                .await
            } else {
                Err(native_reader::expired())
            }
            #[cfg(not(test))]
            {
                Err(native_reader::expired())
            }
        };
        self.finish_claimed_send(record, attempt, executed).await
    }

    async fn finish_claimed_send(
        &self,
        record: Record,
        attempt: String,
        executed: Result<Value>,
    ) -> Result<Value> {
        #[cfg(test)]
        ShutdownTestGate::checkpoint(&self.shutdown_hooks.before_finish).await;
        let (accepted, segments) = match executed {
            Ok(value) => match (value["accepted"].as_bool(), value["segments"].as_u64()) {
                (Some(accepted), Some(segments))
                    if segments <= 1024 && (!accepted || segments > 0) =>
                {
                    (accepted, segments)
                }
                _ => (false, 0),
            },
            Err(_) => (false, 0),
        };
        let archived = if accepted {
            if let Some(item) = archive_item(&record) {
                matches!(
                    tokio::time::timeout(
                        self.io_timeout,
                        crate::message_archive::ingest_messages_to_retained(
                            self.archive.clone(), vec![item], self.job.clone(),
                        ),
                    ).await,
                    Ok(Ok(_))
                )
            } else { false }
        } else { false };
        // This supervisor survives the HTTP waiter. Finishing only writes a
        // result; it does not grant another send after lost network/archive ACKs.
        let finished = self.ledger("finish",json!({"client_request_id":record.client_request_id,"attempt":attempt,"state":if accepted {"accepted"} else {"unknown"},"segments":segments,"archive_confirmed":archived})).await;
        self.release(&record.binding).await;
        outcome(finished.map_err(|_| unknown())?)
    }
    async fn post(
        &self,
        mut body: SendRequest,
        prepare_only: bool,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        if !valid_token(&body.client_request_id) {
            return Err(self.input_failure(coded(
                StatusCode::BAD_REQUEST,
                "sms_client_request_id_required",
                "短信发送必须携带有效的请求标识",
            )));
        }
        if !body.normalize() {
            return Err(self.input_failure(coded(
                StatusCode::BAD_REQUEST,
                "sms_fields_required",
                "请填写收件人号码和短信内容",
            )));
        }
        if cancel.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        if prepare_only {
            let record = self.prepare(&body, cancel, None).await?;
            #[cfg(test)]
            if self.compatibility_fixture && compatibility_binding(&record.binding) {
                self.compatibility_write_ready()?;
            }
            return Ok(self.project(vec![record]).await.remove(0));
        }
        let record = match self
            .ledger("get", json!({"client_request_id":body.client_request_id}))
            .await
        {
            Ok(record) => record,
            Err(error) => {
                #[cfg(test)]
                if self.compatibility_fixture
                    && error.0 == StatusCode::NOT_FOUND
                    && !(body.device == "reader" && self.native_sms.has_intent())
                    && !((body.device.is_empty() || body.device == "dji")
                        && self.native_module_sms.has_intent())
                    && !self.native_serial_selected(&body)
                    && !self.native_usb_selected(&body)
                    && self.native_dji_capture(&body).await.is_none()
                {
                    self.compatibility_write_ready()?;
                }
                return Err(error);
            }
        };
        if !body.matches(&record) {
            return Err(conflict());
        }
        self.submit(record, cancel).await
    }
    async fn get(&self, query: &str) -> Result<Value> {
        let (id, cursor) = query_fields(query)?;
        if !id.is_empty() {
            let record = self.ledger("get", json!({"client_request_id":id})).await?;
            return Ok(self.project(vec![record]).await.remove(0));
        }
        let page: Page = self.ledger("list", json!({"cursor":cursor})).await?;
        Ok(json!({"intents":self.project(page.intents).await,"next_cursor":page.next_cursor}))
    }
}

pub(crate) fn compatibility_binding(binding: &str) -> bool {
    !binding.starts_with(RUST_READER_IMS_BINDING_PREFIX)
        && !binding.starts_with(RUST_MODULE_IMS_BINDING_PREFIX)
        && usb_route_session_generation(binding).is_none()
        && !serial_binding_namespace(binding)
}

/// A retired Go-wrapped serial capability cannot authorize a new send, but
/// an old in-flight record still loses its compatibility owner on Go exit.
/// Keep recovery classification separate from new-send admission.
pub(crate) fn go_recovery_binding(binding: &str) -> bool {
    binding.starts_with(RUST_MODEM_BINDING_PREFIX) || compatibility_binding(binding)
}

#[cfg(test)]
fn go_binding(binding: &str) -> &str {
    [RUST_USB_BINDING_PREFIX, RUST_MODEM_BINDING_PREFIX]
        .iter()
        .find_map(|prefix| {
            binding
                .strip_prefix(prefix)
                .and_then(|rest| rest.split_once(':').map(|(_, token)| token))
        })
        .unwrap_or(binding)
}

async fn await_supervisor(
    task: tokio::task::JoinHandle<owner::Output<Result<Value>>>,
    timeout: Duration,
) -> Result<Value> {
    // Dropping a JoinHandle detaches the owner task; timing out the HTTP
    // waiter must not cancel a submitted physical write or its finalizer.
    match tokio::time::timeout(timeout, task).await {
        Ok(Ok(result)) => result.into_inner(),
        _ => Err(unknown()),
    }
}

fn usb_route_session_generation(binding: &str) -> Option<(&str, u64)> {
    let value = binding.strip_prefix(RUST_USB_SESSION_BINDING_PREFIX)?;
    let (session, generation) = value.split_once(':')?;
    (valid_token(session) && !session.contains(':'))
        .then(|| generation.parse::<u64>().ok().filter(|value| *value > 0))
        .flatten()
        .map(|generation| (session, generation))
}

fn usb_route_generation(binding: &str) -> Option<u64> {
    if let Some((_, generation)) = usb_route_session_generation(binding) {
        return Some(generation);
    }
    binding
        .strip_prefix(RUST_USB_BINDING_PREFIX)?
        .split_once(':')?
        .0
        .parse()
        .ok()
}

fn matching_usb_transport_binding(
    configured_session: Option<&str>,
    active: Option<(String, u64)>,
) -> Option<(String, u64)> {
    let expected = configured_session?;
    let binding = active?;
    (binding.0 == expected && binding.1 > 0).then_some(binding)
}

fn archive_item(record: &Record) -> Option<Value> {
    let timestamp = time::OffsetDateTime::from_unix_timestamp_nanos(
        i128::from(record.created_at_ms) * 1_000_000,
    ).ok()?.format(&time::format_description::well_known::Rfc3339).ok()?;
    let mut item = json!({"sender":record.phone,"content":record.message,"timestamp":timestamp,
        "direction":"out","device":record.device,"send_request_id":record.client_request_id});
    if !record.subscription_id.is_empty() {
        item["subscription_id"] = json!(record.subscription_id);
    }
    Some(item)
}

fn outcome(record: Record) -> Result<Value> {
    if record.state == "accepted" && record.archive_confirmed {
        return Ok(
            json!({"sent":true,"segments":record.segments,"device":record.device,"client_request_id":record.client_request_id}),
        );
    }
    if record.state == "accepted" {
        return Err(coded(
            StatusCode::BAD_GATEWAY,
            "sms_send_archive_outcome_unknown",
            "发送通道已接受请求，但本机记录未确认。再次发送可能重复，请先核对。",
        ));
    }
    Err(unknown())
}

fn query_fields(query: &str) -> Result<(String, String)> {
    let invalid = || coded(StatusCode::BAD_REQUEST, "invalid_request", "查询参数无效");
    let mut fields = BTreeMap::new();
    for field in query
        .split('&')
        .filter(|part| !part.is_empty() && !part.contains(';'))
    {
        let (key, value) = field.split_once('=').unwrap_or((field, ""));
        let (Some(key), Some(value)) = (unescape(key), unescape(value)) else {
            continue;
        };
        if !matches!(key.as_str(), "client_request_id" | "cursor")
            || fields.insert(key, value).is_some()
        {
            return Err(invalid());
        }
    }
    let id = fields.remove("client_request_id").unwrap_or_default();
    let cursor = fields.remove("cursor").unwrap_or_default();
    if (!id.is_empty() && (!valid_token(&id) || !cursor.is_empty()))
        || (!cursor.is_empty() && !valid_token(&cursor))
    {
        return Err(invalid());
    }
    Ok((id, cursor))
}
fn unescape(text: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut input = text.bytes();
    while let Some(b) = input.next() {
        bytes.push(match b {
            b'+' => b' ',
            b'%' => {
                let h = (input.next()? as char).to_digit(16)?;
                let l = (input.next()? as char).to_digit(16)?;
                (h * 16 + l) as u8
            }
            b => b,
        });
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(crate) async fn handle<B>(
    request: Request<B>,
    state: &crate::ServerState,
) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if let Some(response) = crate::sms_store_health::rejection(state.device_send_store.status()) {
        return response;
    }
    static CLAIMS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let service = Service {
        job: None,
        #[cfg(test)]
        shutdown_hooks: Default::default(),
        #[cfg(test)]
        archive_executor: None,
        #[cfg(test)]
        compatibility_fixture: false,
        store: state.device_send_store.clone(),
        archive: ArchiveStore::new(
            state
                .multistandby_store
                .data_root()
                .join("sms-archive.json"),
        ),
        #[cfg(test)]
        socket: state.go_socket_path.clone(),
        #[cfg(test)]
        app_token: state.app_token.clone(),
        #[cfg(test)]
        internal_token: state.internal_token.clone(),
        usb_at: state.usb_at.clone(),
        modem_actor: state.modem_actor.clone(),
        module_radio: state.module_radio.clone(),
        #[cfg(test)]
        availability: state.availability.clone(),
        native_sms: Arc::new(RuntimeReaderSms(state.native_vowifi_lifecycle.clone())),
        native_module_sms: Arc::new(RuntimeModuleSms(state.native_vowifi_lifecycle.clone())),
        io_timeout: RPC_TIMEOUT,
        claims: CLAIMS.get_or_init(|| Arc::new(Semaphore::new(16))).clone(),
    };
    let query = request.uri().query().unwrap_or("").to_owned();
    let result = if request.method() == Method::GET {
        if query == "authority=2" {
            service.ledger("authority", json!({})).await
        } else {
            service.get(&query).await
        }
    } else {
        if !query.is_empty() && query != "cancel=2" {
            return respond(Err(coded(StatusCode::BAD_REQUEST, "invalid_request", "查询参数无效")));
        }
        let prepare_only = request.uri().path() == "/api/sms/send-intents";
        let bytes = match crate::http_server::collect_body(request.into_body(), MAX_BODY).await {
            Ok(body) => body,
            Err(crate::http_server::BodyReadError::Timeout) => {
                return crate::http_server::body_timeout_response();
            }
            Err(_) => {
                return respond(Err(service.input_failure(Failure(
                    StatusCode::BAD_REQUEST,
                    json!({"error":"请求正文无效或超过大小限制"}),
                ))));
            }
        };
        let inspected: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return respond(Err(coded(StatusCode::BAD_REQUEST, "invalid_request", "请求格式无效"))),
        };
        if inspected["identity_version"] == 2 {
            if crate::device_send_store::Store::strict_v2_body(&bytes).is_err() {
                return respond(Err(coded(StatusCode::BAD_REQUEST, "invalid_request", "请求字段重复或无效")));
            }
        }
        if query == "cancel=2" || inspected.get("payload").is_some() {
            if !prepare_only {
                return respond(Err(coded(StatusCode::BAD_REQUEST, "invalid_request", "签发必须使用准备接口")));
            }
            let body = match crate::device_send_store::Store::reserve_body(&bytes) {
                Ok(body) => body,
                Err(_) => return respond(Err(coded(StatusCode::BAD_REQUEST, "invalid_request", "签发请求无效"))),
            };
            return respond(supervise_reservation(service, body, query == "cancel=2").await);
        }
        let mut decoder = serde_json::Deserializer::from_slice(&bytes);
        let body = match SendRequest::deserialize(&mut decoder) {
            Ok(body) => body,
            Err(_) => {
                return respond(Err(service.input_failure(Failure(
                    StatusCode::BAD_REQUEST,
                    json!({"error":"短信请求必须是字段有效的 JSON 对象"}),
                ))));
            }
        };
        if body.identity_version == 2 {
            let strict = crate::device_send_store::Store::strict_v2_body(&bytes);
            let exact = strict.as_ref().ok().and_then(Value::as_object).is_some_and(|fields| {
                fields.get("identity_version") == Some(&json!(2))
                    && fields.keys().all(|key| matches!(key.as_str(), "identity_version" | "intent_nonce"
                        | "client_request_id" | "phone" | "message" | "device" | "expected_subscription_id"))
            });
            if !exact {
                return respond(Err(coded(StatusCode::BAD_REQUEST, "invalid_request", "v2请求字段重复或大小写无效")));
            }
        }
        if decoder.end().is_err() {
            return respond(Err(service.input_failure(Failure(
                StatusCode::BAD_REQUEST,
                json!({"error":"请求只能包含一个 JSON 对象"}),
            ))));
        }
        supervise(service, body, prepare_only).await
    };
    respond(result)
}

/// These exact Store::apply refusals return before write(items, records) and
/// before ArchiveStore commits active bytes. Everything else, including unknown
/// Configuration and real I/O, remains an executor failure when orphaned.
fn ledger_storage_result(
    result: std::result::Result<Value, StoreError>,
) -> std::result::Result<std::result::Result<Value, StoreError>, StoreError> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(StoreError::Configuration(code))
            if matches!(
                code.as_str(),
                "invalid_request"
                    | "send_intent_not_found"
                    | "send_intent_conflict"
                    | "ledger_full"
                    | "mutation_identity_retired"
                    | "mutation_identity_not_issued"
                    | "mutation_identity_upgrade_required"
                    | "mutation_authority_mismatch"
                    | "mutation_sequence_exhausted"
            ) =>
        {
            Ok(Err(StoreError::Configuration(code)))
        }
        Err(error) => Err(error),
    }
}

fn owner_failure(code: &'static str) -> Failure {
    coded(
        StatusCode::SERVICE_UNAVAILABLE,
        code,
        "设备短信正在退出或受理队列不可用；未提交新操作",
    )
}

async fn supervise(mut service: Service, body: SendRequest, prepare_only: bool) -> Result<Value> {
    // Root registration precedes spawn. All child tasks inherit this same
    // bounded admission even after the HTTP waiter or a timed wait disappears.
    let job = service.store.send_owner().admit().map_err(owner_failure)?;
    service.job = Some(job.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let _guard = CancelOnDrop(cancel.clone());
    job.spawn(async move { service.post(body, prepare_only, &cancel).await })
        .await
        .map(|output| output.into_inner())
        .unwrap_or_else(|_| Err(unknown()))
}

async fn supervise_reservation(mut service: Service, body: Value, cancel: bool) -> Result<Value> {
    let job = service.store.send_owner().admit().map_err(owner_failure)?;
    service.job = Some(job.clone());
    job.spawn(async move {
        let record: Record = service.ledger(if cancel { "cancel" } else { "reserve" }, body).await?;
        Ok(service.project(vec![record]).await.remove(0))
    }).await.map(|output| output.into_inner()).unwrap_or_else(|_| Err(unavailable()))
}

fn respond(result: Result<Value>) -> Response<Full<Bytes>> {
    match result {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(Failure(status, value)) => crate::json_response(status, &value),
    }
}

#[cfg(test)]
mod tests {
    include!("device_sms_send/tests.rs");
}
