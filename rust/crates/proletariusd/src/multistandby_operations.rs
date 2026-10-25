//! Durable admission and status owner for N-wait start/stop/retry requests.
//!
//! The native manager serializes execution of a durably claimed operation. An operation whose execution outcome is ambiguous is quarantined
//! and is never automatically dispatched again after daemon restart.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use proletarius_sms::stable_digest_hex;
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(test)]
pub(crate) const INTERNAL_TRANSITION_PATH: &str = "/api/internal/multistandby/operation-transition";
#[cfg(test)]
pub(crate) const INTERNAL_CLAIM_PATH: &str = "/api/internal/multistandby/operation-claim";
const MAX_BODY: usize = 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024;
const RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const OPERATION_LIMIT: usize = 10_000;
const STOP_RESERVE: usize = 500;
const PENDING_LIMIT: usize = 128;
const PENDING_STOP_RESERVE: usize = 32;

#[derive(Clone)]
pub(crate) struct Store(
    crate::sms_store_health::GuardedArchive,
    std::sync::Arc<Dispatches>,
);

#[derive(Default)]
struct Dispatches {
    active: std::sync::atomic::AtomicUsize,
    changed: tokio::sync::Notify,
    #[cfg(test)]
    terminal_attempts: std::sync::atomic::AtomicUsize,
}
struct DispatchGuard(std::sync::Arc<Dispatches>);
impl Drop for DispatchGuard {
    fn drop(&mut self) {
        self.0
            .active
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct Snapshot {
    #[serde(default = "schema_version")]
    schema_version: u16,
    #[serde(default)]
    operations: BTreeMap<String, Operation>,
    #[serde(default)]
    requests: BTreeMap<String, RequestIndex>,
    #[serde(default)]
    transitions: BTreeMap<String, String>,
    #[serde(default)]
    intents: BTreeMap<String, ControlIntent>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            schema_version: schema_version(),
            operations: BTreeMap::new(),
            requests: BTreeMap::new(),
            transitions: BTreeMap::new(),
            intents: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct ControlIntent {
    request_id: String,
    created_at_ms: u64,
}

fn schema_version() -> u16 {
    1
}

#[derive(Clone, Deserialize, Serialize)]
struct Operation {
    operation_id: String,
    state: String,
    detail: String,
    #[serde(default)]
    #[serde(skip_serializing_if = "is_zero")]
    created_at_ms: u64,
    #[serde(default)]
    #[serde(skip_serializing_if = "is_zero")]
    finished_at_ms: u64,
    request_id: String,
    fingerprint: String,
    action: String,
    card_id: String,
    profile_ids: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct RequestIndex {
    operation_id: String,
    fingerprint: String,
}

#[derive(Serialize)]
struct PublicOperation<'a> {
    operation_id: &'a str,
    state: &'a str,
    detail: &'a str,
    #[serde(skip_serializing_if = "is_zero")]
    created_at_ms: u64,
    #[serde(skip_serializing_if = "is_zero")]
    finished_at_ms: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Command {
    card_id: String,
    #[serde(default)]
    profile_ids: Option<Vec<String>>,
    #[serde(default)]
    all: bool,
    client_request_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ControlIntentCommand {
    action: String,
    card_id: String,
    #[serde(default)]
    profile_ids: Option<Vec<String>>,
    #[serde(default)]
    all: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlIntentAcknowledgement {
    action: String,
    card_id: String,
    #[serde(default)]
    profile_ids: Option<Vec<String>>,
    #[serde(default)]
    all: bool,
    client_request_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Transition {
    operation_id: String,
    transition_id: String,
    state: String,
    detail: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimRequest {
    operation_id: String,
    action: String,
    card_id: String,
    profile_ids: Vec<String>,
}

#[derive(Clone)]
struct Admission {
    operation_id: String,
    created: bool,
    action: String,
    card_id: String,
    profile_ids: Vec<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn failure(code: &'static str) -> StoreError {
    StoreError::Configuration(code.to_owned())
}

fn read_snapshot(items: &[Value]) -> Result<Snapshot, StoreError> {
    if items.is_empty() {
        return Ok(Snapshot::default());
    }
    if items.len() != 1 {
        return Err(failure("invalid_store_schema"));
    }
    let snapshot: Snapshot = serde_json::from_value(items[0].clone()).map_err(StoreError::Json)?;
    if snapshot.schema_version != 1 {
        return Err(failure("invalid_store_schema"));
    }
    Ok(snapshot)
}

fn persist(items: &mut Vec<Value>, snapshot: Snapshot) -> Result<(), StoreError> {
    *items = vec![serde_json::to_value(snapshot).map_err(StoreError::Json)?];
    Ok(())
}

fn finished(state: &str) -> bool {
    matches!(
        state,
        "accepted" | "completed" | "failed" | "cancelled" | "partial" | "stopped" | "unknown"
    )
}

fn prune(snapshot: &mut Snapshot, now: u64) {
    let cutoff = now.saturating_sub(RETENTION.as_millis().min(u128::from(u64::MAX)) as u64);
    let expired = snapshot
        .operations
        .iter()
        .filter_map(|(id, operation)| {
            (finished(&operation.state)
                && operation.finished_at_ms > 0
                && operation.finished_at_ms <= cutoff)
                .then_some(id.clone())
        })
        .collect::<Vec<_>>();
    for id in expired {
        if let Some(operation) = snapshot.operations.remove(&id) {
            if snapshot
                .requests
                .get(&operation.request_id)
                .is_some_and(|request| request.operation_id == id)
            {
                snapshot.requests.remove(&operation.request_id);
            }
            snapshot
                .transitions
                .retain(|key, _| !key.starts_with(&format!("{id}:")));
        }
    }
    snapshot.intents.retain(|_, intent| {
        snapshot.requests.contains_key(&intent.request_id) || intent.created_at_ms > cutoff
    });
}

fn normalized_intent(
    action: &str,
    card_id: &str,
    profile_ids: Option<&[String]>,
    all: bool,
) -> Result<(String, Vec<String>, bool), StoreError> {
    let mut profiles = profile_ids.unwrap_or_default().to_vec();
    profiles.sort();
    profiles.dedup();
    if !matches!(action, "start" | "stop" | "retry")
        || !valid_text(card_id, 512)
        || all == !profiles.is_empty()
        || profiles.iter().any(|id| !valid_text(id, 512))
    {
        return Err(failure("invalid_request"));
    }
    let value = json!({
        "action": action,
        "card_id": card_id,
        "profile_ids": if all { Value::Null } else { json!(profiles) },
        "all": all,
    });
    let key = stable_digest_hex(&serde_json::to_vec(&value).map_err(StoreError::Json)?);
    Ok((key, profiles, all))
}

fn admission(
    snapshot: &mut Snapshot,
    request_id: &str,
    fingerprint: &str,
    action: &str,
    card_id: &str,
    profile_ids: &[String],
    operation_id: String,
    now: u64,
    operation_limit: usize,
    pending_limit: usize,
    stop_reserve: usize,
    pending_stop_reserve: usize,
) -> Result<Admission, StoreError> {
    prune(snapshot, now);
    if let Some(prior) = snapshot.requests.get(request_id) {
        if prior.fingerprint != fingerprint {
            return Err(failure("request_id_conflict"));
        }
        return Ok(Admission {
            operation_id: prior.operation_id.clone(),
            created: false,
            action: action.to_owned(),
            card_id: card_id.to_owned(),
            profile_ids: profile_ids.to_vec(),
        });
    }
    let (total_limit, queue_limit) = if action == "stop" {
        (operation_limit, pending_limit)
    } else {
        (
            operation_limit.saturating_sub(stop_reserve),
            pending_limit.saturating_sub(pending_stop_reserve),
        )
    };
    if snapshot.operations.len() >= total_limit {
        return Err(failure("operation_capacity"));
    }
    if snapshot
        .operations
        .values()
        .filter(|operation| !finished(&operation.state))
        .count()
        >= queue_limit
    {
        return Err(failure("operation_queue_busy"));
    }
    let record = Operation {
        operation_id: operation_id.clone(),
        state: "pending".to_owned(),
        detail: "等待卡片调度".to_owned(),
        created_at_ms: now,
        finished_at_ms: 0,
        request_id: request_id.to_owned(),
        fingerprint: fingerprint.to_owned(),
        action: action.to_owned(),
        card_id: card_id.to_owned(),
        profile_ids: profile_ids.to_vec(),
    };
    snapshot.operations.insert(operation_id.clone(), record);
    snapshot.requests.insert(
        request_id.to_owned(),
        RequestIndex {
            operation_id: operation_id.clone(),
            fingerprint: fingerprint.to_owned(),
        },
    );
    Ok(Admission {
        operation_id,
        created: true,
        action: action.to_owned(),
        card_id: card_id.to_owned(),
        profile_ids: profile_ids.to_vec(),
    })
}

impl Store {
    pub(crate) fn open(root: &Path) -> Result<Self, StoreError> {
        Ok(Self(
            ArchiveStore::with_limits(
                root.join("multistandby").join("operations-v1.json"),
                MAX_ARCHIVE_BYTES,
                1,
            )?
            .into(),
            Default::default(),
        ))
    }

    fn spawn_dispatch(&self, work: impl std::future::Future<Output = ()> + Send + 'static) {
        // Register before spawn, so a task not yet polled is still joined.
        self.1
            .active
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let guard = DispatchGuard(self.1.clone());
        tokio::spawn(async move {
            let _guard = guard;
            work.await;
        });
    }

    /// Call after public admission has stopped and native execution has joined.
    /// Includes terminal persistence retries, even if the HTTP waiter was dropped.
    pub(crate) async fn finish_dispatches(&self) {
        loop {
            let changed = self.1.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.1.active.load(std::sync::atomic::Ordering::Acquire) == 0 {
                return;
            }
            changed.await;
        }
    }

    async fn finish_transition(&self, transition: Transition) {
        // Only the idempotent journal write retries; execution is never replayed.
        for attempt in 0..3 {
            #[cfg(test)]
            self.1
                .terminal_attempts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let store = self.clone();
            let transition = transition.clone();
            if crate::archive_io::run(move || store.transition(transition))
                .await
                .is_ok()
            {
                return;
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis((attempt + 1) * 200)).await;
            }
        }
        // Durable running/pending remains quarantined as unknown on recovery.
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> proletarius_api_contract::SmsStoreStatus {
        self.0.status()
    }

    /// Any process that died while an operation could have reached hardware
    /// leaves an ambiguous outcome. Quarantine it; never redispatch it.
    pub(crate) fn recover(&self) -> Result<(), StoreError> {
        self.0.mutate(|items| {
            let mut snapshot = read_snapshot(items)?;
            let mut changed = false;
            let now = now_ms();
            for operation in snapshot.operations.values_mut() {
                if matches!(operation.state.as_str(), "pending" | "running") {
                    operation.state = "unknown".to_owned();
                    operation.detail =
                        "后端重启前操作结果未确认；为避免重复执行，请刷新线路状态".to_owned();
                    operation.finished_at_ms = now;
                    changed = true;
                }
            }
            prune(&mut snapshot, now);
            // ArchiveStore's first mutation creates an on-disk archive even
            // when the logical snapshot is empty. Persist the empty versioned
            // registry now so the next guarded read cannot confuse that
            // zero-byte bootstrap file with corruption.
            if changed || items.is_empty() {
                persist(items, snapshot)?;
            }
            Ok(())
        })
    }

    fn lookup(&self, request_id: &str, fingerprint: &str) -> Result<Option<String>, StoreError> {
        self.0.read(|items| {
            let mut snapshot = read_snapshot(items)?;
            prune(&mut snapshot, now_ms());
            let Some(prior) = snapshot.requests.get(request_id) else {
                return Ok(None);
            };
            if prior.fingerprint != fingerprint {
                return Err(failure("request_id_conflict"));
            }
            Ok(Some(prior.operation_id.clone()))
        })
    }

    /// Rust creates and durably retains the UI control identity before the
    /// physical command is submitted. A lost HTTP response or GUI restart can
    /// therefore recover the same request id without replaying hardware work.
    fn prepare_intent(&self, key: &str) -> Result<String, StoreError> {
        let request_id = crate::multistandby_store::random_token()?;
        self.0.mutate(|items| {
            let mut snapshot = read_snapshot(items)?;
            prune(&mut snapshot, now_ms());
            if let Some(intent) = snapshot.intents.get(key) {
                return Ok(intent.request_id.clone());
            }
            if snapshot.intents.len() >= PENDING_LIMIT {
                return Err(failure("operation_queue_busy"));
            }
            snapshot.intents.insert(
                key.to_owned(),
                ControlIntent {
                    request_id: request_id.clone(),
                    created_at_ms: now_ms(),
                },
            );
            persist(items, snapshot)?;
            Ok(request_id.clone())
        })
    }

    fn acknowledge_intent(&self, key: &str, request_id: &str) -> Result<bool, StoreError> {
        self.0.mutate(|items| {
            let mut snapshot = read_snapshot(items)?;
            prune(&mut snapshot, now_ms());
            let Some(intent) = snapshot.intents.get(key) else {
                return Ok(false);
            };
            if intent.request_id != request_id {
                return Err(failure("request_id_conflict"));
            }
            snapshot.intents.remove(key);
            persist(items, snapshot)?;
            Ok(true)
        })
    }

    fn admit(
        &self,
        request_id: &str,
        fingerprint: &str,
        action: &str,
        card_id: &str,
        profile_ids: &[String],
    ) -> Result<Admission, StoreError> {
        let operation_id = crate::multistandby_store::random_token()?;
        self.0.mutate(|items| {
            let mut snapshot = read_snapshot(items)?;
            let result = admission(
                &mut snapshot,
                request_id,
                fingerprint,
                action,
                card_id,
                profile_ids,
                operation_id,
                now_ms(),
                OPERATION_LIMIT,
                PENDING_LIMIT,
                STOP_RESERVE,
                PENDING_STOP_RESERVE,
            )?;
            if result.created {
                persist(items, snapshot)?;
            }
            Ok(result)
        })
    }

    fn transition(&self, transition: Transition) -> Result<(), StoreError> {
        if transition.transition_id.is_empty()
            || transition.transition_id.len() > 160
            || transition.detail.len() > 2048
            || !matches!(
                transition.state.as_str(),
                "running" | "accepted" | "completed" | "failed" | "unknown"
            )
        {
            return Err(failure("invalid_request"));
        }
        self.0.mutate(|items| {
            let mut snapshot = read_snapshot(items)?;
            let current_state = snapshot
                .operations
                .get(&transition.operation_id)
                .map(|operation| operation.state.clone())
                .ok_or_else(|| failure("operation_not_found"))?;
            let transition_hash =
                stable_digest_hex(&serde_json::to_vec(&transition).map_err(StoreError::Json)?);
            let event_key = format!("{}:{}", transition.operation_id, transition.transition_id);
            if let Some(existing) = snapshot.transitions.get(&event_key) {
                if existing != &transition_hash {
                    return Err(failure("transition_conflict"));
                }
                return Ok(());
            }
            let allowed = match (current_state.as_str(), transition.state.as_str()) {
                ("pending", "running" | "failed" | "unknown") => true,
                ("running", "accepted" | "completed" | "failed" | "unknown") => true,
                (state, next) => state == next && state != "unknown",
            };
            if !allowed {
                return Err(failure("transition_conflict"));
            }
            if let Some(operation) = snapshot.operations.get_mut(&transition.operation_id) {
                operation.state = transition.state.clone();
                operation.detail = transition.detail.clone();
                if finished(&operation.state) {
                    operation.finished_at_ms = now_ms();
                }
            }
            snapshot.transitions.insert(event_key, transition_hash);
            prune(&mut snapshot, now_ms());
            persist(items, snapshot)
        })
    }

    /// Persist the only execution claim before the manager may touch a reader or start a
    /// session. Duplicate or late private callbacks cannot re-run an operation.
    fn claim(&self, claim: &ClaimRequest) -> Result<bool, StoreError> {
        if !valid_text(&claim.operation_id, 128)
            || !matches!(claim.action.as_str(), "start" | "stop" | "retry")
            || !valid_text(&claim.card_id, 512)
            || claim.profile_ids.is_empty()
            || claim.profile_ids.len() > 10_000
            || claim.profile_ids.iter().any(|id| !valid_text(id, 512))
        {
            return Err(failure("invalid_request"));
        }
        let mut requested_profiles = claim.profile_ids.clone();
        requested_profiles.sort();
        if requested_profiles.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(failure("invalid_request"));
        }
        self.0.mutate(|items| {
            let mut snapshot = read_snapshot(items)?;
            let operation = snapshot
                .operations
                .get(&claim.operation_id)
                .ok_or_else(|| failure("operation_not_found"))?;
            let mut admitted_profiles = operation.profile_ids.clone();
            admitted_profiles.sort();
            if operation.action != claim.action
                || operation.card_id != claim.card_id
                || admitted_profiles != requested_profiles
            {
                return Err(failure("operation_claim_conflict"));
            }
            if operation.state != "pending" {
                return Ok(false);
            }
            let transition = Transition {
                operation_id: claim.operation_id.clone(),
                transition_id: format!("{}-running", claim.operation_id),
                state: "running".to_owned(),
                detail: "正在执行".to_owned(),
            };
            let transition_hash =
                stable_digest_hex(&serde_json::to_vec(&transition).map_err(StoreError::Json)?);
            let event_key = format!("{}:{}", transition.operation_id, transition.transition_id);
            if let Some(existing) = snapshot.transitions.get(&event_key) {
                if existing != &transition_hash {
                    return Err(failure("transition_conflict"));
                }
                return Ok(false);
            }
            let operation = snapshot
                .operations
                .get_mut(&claim.operation_id)
                .expect("operation validated above");
            operation.state = "running".to_owned();
            operation.detail = "正在执行".to_owned();
            snapshot.transitions.insert(event_key, transition_hash);
            prune(&mut snapshot, now_ms());
            persist(items, snapshot)?;
            Ok(true)
        })
    }

    fn get(&self, operation_id: &str) -> Result<Option<Operation>, StoreError> {
        self.0.read(|items| {
            let snapshot = read_snapshot(items)?;
            let Some(operation) = snapshot.operations.get(operation_id) else {
                return Ok(None);
            };
            if finished(&operation.state)
                && operation.finished_at_ms > 0
                && operation.finished_at_ms <= now_ms().saturating_sub(RETENTION.as_millis() as u64)
            {
                return Ok(None);
            }
            Ok(Some(operation.clone()))
        })
    }
}

fn map_store_error(error: &StoreError) -> (StatusCode, &'static str, &'static str) {
    match error {
        StoreError::Configuration(code) if code == "invalid_request" => {
            (StatusCode::BAD_REQUEST, "无效请求", "invalid_request")
        }
        StoreError::Configuration(code) if code == "request_id_conflict" => (
            StatusCode::CONFLICT,
            "请求标识已用于其他操作",
            "request_id_conflict",
        ),
        StoreError::Configuration(code) if code == "operation_capacity" => (
            StatusCode::TOO_MANY_REQUESTS,
            "最近24小时内操作过多，请稍后重试",
            "operation_capacity",
        ),
        StoreError::Configuration(code) if code == "operation_queue_busy" => (
            StatusCode::TOO_MANY_REQUESTS,
            "操作队列繁忙，请等待已有操作结束",
            "operation_queue_busy",
        ),
        StoreError::Configuration(code) if code == "operation_not_found" => (
            StatusCode::NOT_FOUND,
            "operation not found",
            "operation_not_found",
        ),
        StoreError::Configuration(code) if code == "operation_claim_conflict" => (
            StatusCode::CONFLICT,
            "operation claim does not match the accepted command",
            "operation_claim_conflict",
        ),
        StoreError::Configuration(code) if code == "transition_conflict" => (
            StatusCode::CONFLICT,
            "operation transition conflict",
            "transition_conflict",
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "多待操作记录暂不可用",
            "operation_store_unavailable",
        ),
    }
}

fn error_response(status: StatusCode, error: &str, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &json!({"error":error,"code":code}))
}

fn public_path(path: &str) -> bool {
    matches!(
        path,
        "/api/multistandby/start"
            | "/api/multistandby/stop"
            | "/api/multistandby/retry"
            | "/api/multistandby/control-intents"
            | "/api/multistandby/control-intents/ack"
    ) || (path.starts_with("/api/multistandby/operations/")
        && !path["/api/multistandby/operations/".len()..].contains('/'))
}

pub(crate) fn owns_public(method: &Method, path: &str) -> bool {
    if !public_path(path) {
        return false;
    }
    let operation_status = path.starts_with("/api/multistandby/operations/");
    (matches!(*method, Method::GET | Method::HEAD) && operation_status)
        || (*method == Method::POST
            && (action_for_path(path).is_some()
                || matches!(
                    path,
                    "/api/multistandby/control-intents" | "/api/multistandby/control-intents/ack"
                )))
}

#[cfg(test)]
pub(crate) fn owns_internal(path: &str) -> bool {
    matches!(path, INTERNAL_TRANSITION_PATH | INTERNAL_CLAIM_PATH)
}

#[cfg(test)]
pub(crate) fn shutdown_cleanup_allowed(path: &str) -> bool {
    path == INTERNAL_TRANSITION_PATH
}

fn action_for_path(path: &str) -> Option<&'static str> {
    match path {
        "/api/multistandby/start" => Some("start"),
        "/api/multistandby/stop" => Some("stop"),
        "/api/multistandby/retry" => Some("retry"),
        _ => None,
    }
}

async fn parse_json<T: for<'de> Deserialize<'de>>(
    request: Request<Incoming>,
) -> Result<T, Response<Full<Bytes>>> {
    let invalid = || error_response(StatusCode::BAD_REQUEST, "无效请求", "invalid_request");
    let body = crate::http_server::collect_body(request.into_body(), MAX_BODY)
        .await
        .map_err(|error| match error {
            crate::http_server::BodyReadError::Timeout => {
                crate::http_server::body_timeout_response()
            }
            _ => invalid(),
        })?;
    serde_json::from_slice(&body).map_err(|_| invalid())
}

fn valid_text(value: &str, max: usize) -> bool {
    // Match the former Go decoder's byte-length and empty checks. IDs are
    // opaque contract values; trimming or normalizing them would change
    // idempotency for a request the existing API accepted.
    !value.is_empty() && value.len() <= max
}

async fn operation_status(raw_id: &str, store: Store) -> Response<Full<Bytes>> {
    let Some(operation_id) = crate::public_http::path_value(raw_id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "operation not found",
            "operation_not_found",
        );
    };
    if !valid_text(&operation_id, 128) {
        return error_response(
            StatusCode::NOT_FOUND,
            "operation not found",
            "operation_not_found",
        );
    }
    let store = store.clone();
    return match crate::archive_io::run(move || store.get(&operation_id)).await {
        Ok(Some(operation)) => crate::json_response(
            StatusCode::OK,
            &PublicOperation {
                operation_id: &operation.operation_id,
                state: &operation.state,
                detail: &operation.detail,
                created_at_ms: operation.created_at_ms,
                finished_at_ms: operation.finished_at_ms,
            },
        ),
        Ok(None) => error_response(
            StatusCode::NOT_FOUND,
            "operation not found",
            "operation_not_found",
        ),
        Err(error) => {
            let (status, message, code) = map_store_error(&error);
            error_response(status, message, code)
        }
    };
}

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    store: Store,
    runtime_service: std::sync::Arc<crate::multistandby_native_service::Service>,
) -> Response<Full<Bytes>> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    if method == Method::GET && path.starts_with("/api/multistandby/operations/") {
        return operation_status(&path["/api/multistandby/operations/".len()..], store).await;
    }

    if method == Method::POST && path == "/api/multistandby/control-intents" {
        let body: ControlIntentCommand = match parse_json(request).await {
            Ok(body) => body,
            Err(response) => return response,
        };
        let (key, _, _) = match normalized_intent(
            &body.action,
            &body.card_id,
            body.profile_ids.as_deref(),
            body.all,
        ) {
            Ok(value) => value,
            Err(error) => {
                let (status, message, code) = map_store_error(&error);
                return error_response(status, message, code);
            }
        };
        let intent_store = store.clone();
        return match crate::archive_io::run(move || intent_store.prepare_intent(&key)).await {
            Ok(request_id) => {
                crate::json_response(StatusCode::OK, &json!({"client_request_id":request_id}))
            }
            Err(error) => {
                let (status, message, code) = map_store_error(&error);
                error_response(status, message, code)
            }
        };
    }

    if method == Method::POST && path == "/api/multistandby/control-intents/ack" {
        let body: ControlIntentAcknowledgement = match parse_json(request).await {
            Ok(body) => body,
            Err(response) => return response,
        };
        if !valid_text(&body.client_request_id, 128) {
            return error_response(StatusCode::BAD_REQUEST, "无效请求", "invalid_request");
        }
        let (key, _, _) = match normalized_intent(
            &body.action,
            &body.card_id,
            body.profile_ids.as_deref(),
            body.all,
        ) {
            Ok(value) => value,
            Err(error) => {
                let (status, message, code) = map_store_error(&error);
                return error_response(status, message, code);
            }
        };
        let request_id = body.client_request_id;
        let intent_store = store.clone();
        return match crate::archive_io::run(move || {
            intent_store.acknowledge_intent(&key, &request_id)
        })
        .await
        {
            Ok(acknowledged) => {
                crate::json_response(StatusCode::OK, &json!({"acknowledged":acknowledged}))
            }
            Err(error) => {
                let (status, message, code) = map_store_error(&error);
                error_response(status, message, code)
            }
        };
    }

    let Some(action) = action_for_path(&path) else {
        return error_response(StatusCode::NOT_FOUND, "route not found", "route_not_found");
    };
    if method != Method::POST {
        return error_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
            "method_not_allowed",
        );
    }
    let body: Command = match parse_json(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let mut profile_ids = body.profile_ids.clone().unwrap_or_default();
    if !valid_text(&body.card_id, 512)
        || !valid_text(&body.client_request_id, 128)
        || body.all == !profile_ids.is_empty()
        || profile_ids.iter().any(|id| !valid_text(id, 512))
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "须指定card_id、client_request_id及profile_ids或all",
            "invalid_request",
        );
    }
    profile_ids.sort();
    let normalized = Command {
        card_id: body.card_id.clone(),
        profile_ids: body.profile_ids.as_ref().map(|ids| {
            let mut ids = ids.clone();
            ids.sort();
            ids
        }),
        all: body.all,
        client_request_id: body.client_request_id.clone(),
    };
    let fingerprint = format!(
        "{path}{}",
        serde_json::to_string(&normalized).unwrap_or_default()
    );

    // Preserve Go's replay-before-profile-validation contract and avoid
    // touching the runtime adapter for an already durably accepted request.
    let lookup_store = store.clone();
    let lookup_request = body.client_request_id.clone();
    let lookup_fingerprint = fingerprint.clone();
    match crate::archive_io::run(move || lookup_store.lookup(&lookup_request, &lookup_fingerprint))
        .await
    {
        Ok(Some(operation_id)) => {
            return crate::json_response(
                StatusCode::ACCEPTED,
                &json!({"operation_id":operation_id,"accepted":true}),
            );
        }
        Ok(None) => {}
        Err(error) => {
            let (status, message, code) = map_store_error(&error);
            return error_response(status, message, code);
        }
    }

    let runtime = match runtime_service.snapshot(&body.card_id, false, false).await {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "多待运行状态暂不可读取",
                "runtime_snapshot_unavailable",
            );
        }
    };
    let profiles = runtime["profiles"].as_array().cloned().unwrap_or_default();
    let mut available = profiles
        .into_iter()
        .filter(|profile| profile["card_id"].as_str() == Some(body.card_id.as_str()))
        .filter_map(|profile| profile["profile_id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    available.sort();
    available.dedup();
    if body.all {
        profile_ids = available;
    } else {
        let requested = profile_ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if requested.is_empty()
            || requested
                .iter()
                .any(|id| available.binary_search(id).is_err())
        {
            return error_response(
                StatusCode::CONFLICT,
                "所选Profile不属于当前卡片或已不可用",
                "profile_unavailable",
            );
        }
        profile_ids = requested.into_iter().collect();
    }
    if profile_ids.is_empty() {
        return error_response(
            StatusCode::CONFLICT,
            "所选Profile不属于当前卡片或已不可用",
            "profile_unavailable",
        );
    }

    let admit_store = store.clone();
    let request_id = body.client_request_id.clone();
    let card_id = body.card_id.clone();
    let fingerprint_for_admit = fingerprint.clone();
    let selected_ids = profile_ids.clone();
    let result = crate::archive_io::run(move || {
        admit_store.admit(
            &request_id,
            &fingerprint_for_admit,
            action,
            &card_id,
            &selected_ids,
        )
    })
    .await;
    let admission = match result {
        Ok(admission) => admission,
        Err(error) => {
            let (status, message, code) = map_store_error(&error);
            return error_response(status, message, code);
        }
    };
    let response_operation_id = admission.operation_id.clone();
    if admission.created {
        let dispatch_store = store.clone();
        store.spawn_dispatch(async move {
            let claim = ClaimRequest {
                operation_id: admission.operation_id.clone(),
                action: admission.action.clone(),
                card_id: admission.card_id.clone(),
                profile_ids: admission.profile_ids.clone(),
            };
            let claim_store = dispatch_store.clone();
            let claimed = crate::archive_io::run(move || claim_store.claim(&claim)).await;
            match claimed {
                Ok(true) => {}
                Ok(false) => return,
                Err(_) => return, // Durable pending claim remains unresolved; never run or replay it.
            }
            let action = match admission.action.as_str() {
                "start" => crate::multistandby_native_runtime::Action::Start,
                "stop" => crate::multistandby_native_runtime::Action::Stop,
                "retry" => crate::multistandby_native_runtime::Action::Retry,
                _ => return,
            };
            let result = runtime_service
                .execute(action, admission.card_id, admission.profile_ids)
                .await;
            let (state, detail) = match result {
                Ok(_) if admission.action == "start" => (
                    "accepted",
                    "启动请求已接受，正在逐号连接；短信就绪状态请查看线路".to_owned(),
                ),
                Ok(_) if admission.action == "retry" => (
                    "accepted",
                    "重试请求已接受，等待旧会话退出并串行重建".to_owned(),
                ),
                Ok(_) => ("completed", "操作完成".to_owned()),
                Err(error) => native_failure_outcome(error),
            };
            let transition = Transition {
                operation_id: admission.operation_id.clone(),
                transition_id: format!("{}-finished", admission.operation_id),
                state: state.into(),
                detail,
            };
            dispatch_store.finish_transition(transition).await;
        });
    }

    crate::json_response(
        StatusCode::ACCEPTED,
        &json!({"operation_id":response_operation_id,"accepted":true}),
    )
}

fn native_failure_outcome(
    error: crate::multistandby_native_service::Failure,
) -> (&'static str, String) {
    let state = if matches!(
        error.code,
        "native_multi_command_unconfirmed" | "native_multi_cleanup_unconfirmed"
    ) {
        "unknown"
    } else {
        "failed"
    };
    (state, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retained_terminal_writer_survives_caller_drop_and_waits_actual_archive_commit() {
        let root = root();
        let store = Store::open(&root).unwrap();
        let admitted = store
            .admit("retained", "hash", "stop", "card", &["p".into()])
            .unwrap();
        assert!(
            store
                .claim(&ClaimRequest {
                    operation_id: admitted.operation_id.clone(),
                    action: admitted.action,
                    card_id: admitted.card_id,
                    profile_ids: admitted.profile_ids
                })
                .unwrap()
        );
        // Hold the actual archive lock, not a fake completion or timed stand-in.
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("multistandby/operations-v1.json.lock"))
            .unwrap();
        lock.lock().unwrap();
        let transition = Transition {
            operation_id: admitted.operation_id.clone(),
            transition_id: "done".into(),
            state: "completed".into(),
            detail: "finished".into(),
        };
        let dispatch_store = store.clone();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(async move {
            let writer = dispatch_store.clone();
            dispatch_store.spawn_dispatch(async move {
                writer.finish_transition(transition).await;
            });
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        waiting.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let join_store = store.clone();
        let join = tokio::spawn(async move { join_store.finish_dispatches().await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while store
                .1
                .terminal_attempts
                .load(std::sync::atomic::Ordering::Relaxed)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(store.1.active.load(std::sync::atomic::Ordering::Acquire), 1);
        assert!(!join.is_finished());
        lock.unlock().unwrap();
        join.await.unwrap();
        assert_eq!(
            Store::open(&root)
                .unwrap()
                .get(&admitted.operation_id)
                .unwrap()
                .unwrap()
                .state,
            "completed"
        );
        assert_eq!(store.1.active.load(std::sync::atomic::Ordering::Acquire), 0);
        assert_eq!(
            store
                .1
                .terminal_attempts
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retained_failed_writes_join_three_attempts_and_restart_quarantines_without_replay() {
        let root = root();
        let store = Store::open(&root).unwrap();
        let admitted = store
            .admit("failed-writer", "hash", "stop", "card", &["p".into()])
            .unwrap();
        let claim = ClaimRequest {
            operation_id: admitted.operation_id.clone(),
            action: admitted.action,
            card_id: admitted.card_id,
            profile_ids: admitted.profile_ids,
        };
        assert!(store.claim(&claim).unwrap());
        let path = root.join("multistandby/operations-v1.json");
        let saved = root.join("multistandby/saved.json");
        fs::rename(&path, &saved).unwrap();
        fs::create_dir(&path).unwrap(); // Actual filesystem refuses archive access.
        let writer = store.clone();
        let transition = Transition {
            operation_id: admitted.operation_id.clone(),
            transition_id: "done".into(),
            state: "completed".into(),
            detail: "finished".into(),
        };
        store.spawn_dispatch(async move {
            writer.finish_transition(transition).await;
        });
        store.finish_dispatches().await;
        assert_eq!(
            store
                .1
                .terminal_attempts
                .load(std::sync::atomic::Ordering::Relaxed),
            3
        );
        assert_eq!(store.1.active.load(std::sync::atomic::Ordering::Acquire), 0);
        fs::remove_dir(&path).unwrap();
        fs::rename(saved, path).unwrap();
        let restarted = Store::open(&root).unwrap();
        restarted.recover().unwrap();
        assert_eq!(
            restarted
                .get(&admitted.operation_id)
                .unwrap()
                .unwrap()
                .state,
            "unknown"
        );
        assert!(!restarted.claim(&claim).unwrap());
        assert!(
            !restarted
                .admit("failed-writer", "hash", "stop", "card", &["p".into()])
                .unwrap()
                .created
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_unconfirmed_result_stays_unknown_without_retry_permission() {
        for code in [
            "native_multi_command_unconfirmed",
            "native_multi_cleanup_unconfirmed",
        ] {
            let (state, _) = native_failure_outcome(crate::multistandby_native_service::Failure {
                code,
                message: "unconfirmed".into(),
            });
            assert_eq!(state, "unknown");
        }
        let (state, _) = native_failure_outcome(crate::multistandby_native_service::Failure {
            code: "profile_not_found",
            message: "missing".into(),
        });
        assert_eq!(state, "failed");
    }

    use std::fs;

    fn root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "switch-nwait-ops-{}",
            crate::multistandby_store::random_token().unwrap()
        ))
    }

    fn new_admission(
        snapshot: &mut Snapshot,
        request: &str,
        fingerprint: &str,
        op: &str,
        action: &str,
        now: u64,
    ) -> Result<Admission, StoreError> {
        admission(
            snapshot,
            request,
            fingerprint,
            action,
            "card",
            &["profile".into()],
            op.into(),
            now,
            10,
            4,
            2,
            1,
        )
    }

    #[test]
    fn opaque_identifiers_keep_legacy_empty_and_byte_length_validation() {
        assert!(valid_text(" request id ", 128));
        assert!(valid_text("card\n", 512));
        assert!(!valid_text("", 128));
        assert!(!valid_text(&"x".repeat(129), 128));
        assert!(owns_public(
            &Method::POST,
            "/api/multistandby/control-intents"
        ));
        assert!(owns_public(
            &Method::POST,
            "/api/multistandby/control-intents/ack"
        ));
        assert!(!owns_public(
            &Method::GET,
            "/api/multistandby/control-intents"
        ));
        assert!(!owns_public(
            &Method::GET,
            "/api/multistandby/control-intents/ack"
        ));
        assert!(shutdown_cleanup_allowed(INTERNAL_TRANSITION_PATH));
        assert!(!shutdown_cleanup_allowed(INTERNAL_CLAIM_PATH));
    }

    #[tokio::test]
    async fn operation_status_uses_go_path_value_once_after_escaped_segment_routing() {
        // The retired Go registration indexes operations[r.PathValue("id")].
        // Seed opaque synthetic IDs so a missing-ID 404 cannot hide raw lookup.
        let root = root();
        let store = Store::open(&root).unwrap();
        store
            .0
            .mutate(|items| {
                let mut snapshot = Snapshot::default();
                for (index, id) in ["plain", "op/a", "%61", "a+b", "中"]
                    .into_iter()
                    .enumerate()
                {
                    admission(
                        &mut snapshot,
                        &format!("request-{index}"),
                        "hash",
                        "stop",
                        "card",
                        &["profile".into()],
                        id.into(),
                        now_ms(),
                        20,
                        20,
                        0,
                        0,
                    )?;
                }
                persist(items, snapshot)
            })
            .unwrap();
        for (raw, id) in [
            ("%70lain", "plain"),
            ("op%2Fa", "op/a"),
            ("%2561", "%61"),
            ("a+b", "a+b"),
            ("%E4%B8%AD", "中"),
        ] {
            let path = format!("/api/multistandby/operations/{raw}");
            assert!(owns_public(&Method::GET, &path));
            let response = operation_status(raw, store.clone()).await;
            assert_eq!(response.status(), StatusCode::OK, "{raw}");
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(body["operation_id"], id);
        }
        assert!(!owns_public(
            &Method::GET,
            "/api/multistandby/operations/op/a"
        ));
        for raw in ["%2570lain", "%FF", "%GG"] {
            assert_eq!(
                operation_status(raw, store.clone()).await.status(),
                StatusCode::NOT_FOUND,
                "{raw}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn durable_idempotency_and_request_conflict_survive_reopen() {
        let root = root();
        fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root).unwrap();
        let first = store
            .admit("request-1", "start{}", "start", "card", &["p".into()])
            .unwrap();
        drop(store);
        let reopened = Store::open(&root).unwrap();
        assert_eq!(
            reopened.lookup("request-1", "start{}").unwrap(),
            Some(first.operation_id.clone())
        );
        assert!(
            matches!(reopened.lookup("request-1", "stop{}").unwrap_err(), StoreError::Configuration(code) if code == "request_id_conflict")
        );
        assert!(
            !reopened
                .admit("request-1", "start{}", "start", "card", &["p".into()])
                .unwrap()
                .created
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rust_control_intent_survives_gui_restart_until_exact_acknowledgement() {
        let root = root();
        fs::create_dir_all(&root).unwrap();
        let key = normalized_intent("retry", "card", Some(&["p".into()]), false)
            .unwrap()
            .0;
        let first = Store::open(&root).unwrap().prepare_intent(&key).unwrap();
        let restarted = Store::open(&root).unwrap();
        assert_eq!(restarted.prepare_intent(&key).unwrap(), first);
        assert!(matches!(
            restarted.acknowledge_intent(&key, "different").unwrap_err(),
            StoreError::Configuration(code) if code == "request_id_conflict"
        ));
        assert_eq!(restarted.prepare_intent(&key).unwrap(), first);
        assert!(restarted.acknowledge_intent(&key, &first).unwrap());
        let next = restarted.prepare_intent(&key).unwrap();
        assert_ne!(
            next, first,
            "a confirmed UI receipt must allow a fresh intent"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn intent_identity_is_rust_normalized_and_payload_bound() {
        let a = normalized_intent("start", "card", Some(&["b".into(), "a".into()]), false)
            .unwrap()
            .0;
        let b = normalized_intent("start", "card", Some(&["a".into(), "b".into()]), false)
            .unwrap()
            .0;
        assert_eq!(a, b);
        assert_ne!(
            a,
            normalized_intent("stop", "card", Some(&["a".into(), "b".into()]), false)
                .unwrap()
                .0
        );
        assert!(normalized_intent("retry", "card", None, true).is_ok());
        let invalid = normalized_intent("retry", "card", Some(&[]), false).unwrap_err();
        assert_eq!(
            map_store_error(&invalid),
            (StatusCode::BAD_REQUEST, "无效请求", "invalid_request")
        );
        for invalid in [
            normalized_intent("invalid", "card", None, true),
            normalized_intent("start", "", None, true),
            normalized_intent("start", "card", Some(&["p".into()]), true),
        ] {
            assert_eq!(
                map_store_error(&invalid.unwrap_err()).0,
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn first_recovery_persists_empty_registry_before_guarded_reads() {
        let root = root();
        fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root).unwrap();
        store.recover().unwrap();
        assert_eq!(store.status().state, "ready");
        assert_eq!(store.lookup("missing", "hash").unwrap(), None);
        let created = store
            .admit("request", "hash", "stop", "card", &["p".into()])
            .unwrap();
        assert!(created.created);
        assert_eq!(store.status().state, "ready");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_quarantines_pending_and_running_without_requeue() {
        let root = root();
        fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root).unwrap();
        let pending = store
            .admit("pending-request", "pending", "start", "card", &["p".into()])
            .unwrap();
        let running = store
            .admit("running-request", "running", "stop", "card", &["p".into()])
            .unwrap();
        store
            .transition(Transition {
                operation_id: running.operation_id.clone(),
                transition_id: "running-event".into(),
                state: "running".into(),
                detail: "执行中".into(),
            })
            .unwrap();
        drop(store);
        let restarted = Store::open(&root).unwrap();
        restarted.recover().unwrap();
        for id in [&pending.operation_id, &running.operation_id] {
            assert_eq!(restarted.get(id).unwrap().unwrap().state, "unknown");
        }
        // Retained request IDs replay the same quarantined record; no dispatch token is returned.
        assert_eq!(
            restarted.lookup("pending-request", "pending").unwrap(),
            Some(pending.operation_id.clone())
        );
        let replay = restarted
            .admit("pending-request", "pending", "start", "card", &["p".into()])
            .unwrap();
        assert!(!replay.created, "recovered operation must never redispatch");
        assert_eq!(
            restarted.get(&pending.operation_id).unwrap().unwrap().state,
            "unknown"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn callback_transitions_are_idempotent_monotonic_and_terminal() {
        let root = root();
        fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root).unwrap();
        let created = store
            .admit("request", "hash", "start", "card", &["p".into()])
            .unwrap();
        let running = Transition {
            operation_id: created.operation_id.clone(),
            transition_id: "run".into(),
            state: "running".into(),
            detail: "执行中".into(),
        };
        store.transition(running.clone()).unwrap();
        store.transition(running.clone()).unwrap();
        let terminal = Transition {
            operation_id: created.operation_id.clone(),
            transition_id: "done".into(),
            state: "accepted".into(),
            detail: "已接受".into(),
        };
        store.transition(terminal).unwrap();
        assert!(
            matches!(store.transition(Transition { operation_id: created.operation_id.clone(), transition_id: "bad".into(), state: "running".into(), detail: "late".into() }).unwrap_err(), StoreError::Configuration(code) if code == "transition_conflict")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn durable_claim_has_one_winner_and_restart_never_releases_it() {
        let root = root();
        fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root).unwrap();
        let operation = store
            .admit("request", "hash", "stop", "card", &["profile".into()])
            .unwrap();
        let mut mismatched_claim = ClaimRequest {
            operation_id: operation.operation_id.clone(),
            action: operation.action.clone(),
            card_id: operation.card_id.clone(),
            profile_ids: operation.profile_ids.clone(),
        };
        mismatched_claim.action = "start".to_owned();
        assert!(matches!(
            store.claim(&mismatched_claim).unwrap_err(),
            StoreError::Configuration(code) if code == "operation_claim_conflict"
        ));
        assert_eq!(
            store.get(&operation.operation_id).unwrap().unwrap().state,
            "pending",
            "mismatched execution parameters must not consume the claim"
        );
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let workers = (0..8)
            .map(|_| {
                let store = store.clone();
                let barrier = barrier.clone();
                let claim = ClaimRequest {
                    operation_id: operation.operation_id.clone(),
                    action: operation.action.clone(),
                    card_id: operation.card_id.clone(),
                    profile_ids: operation.profile_ids.clone(),
                };
                std::thread::spawn(move || {
                    barrier.wait();
                    store.claim(&claim).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1, "only one executor may win the durable claim");
        assert_eq!(
            store.get(&operation.operation_id).unwrap().unwrap().state,
            "running"
        );

        drop(store);
        let restarted = Store::open(&root).unwrap();
        restarted.recover().unwrap();
        assert_eq!(
            restarted
                .get(&operation.operation_id)
                .unwrap()
                .unwrap()
                .state,
            "unknown"
        );
        let claim = ClaimRequest {
            operation_id: operation.operation_id.clone(),
            action: operation.action,
            card_id: operation.card_id,
            profile_ids: operation.profile_ids,
        };
        assert!(!restarted.claim(&claim).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn capacity_reserves_stop_slots_and_keeps_pending_bounded() {
        let mut snapshot = Snapshot::default();
        for index in 0..8 {
            let result = new_admission(
                &mut snapshot,
                &format!("r{index}"),
                &format!("h{index}"),
                &format!("o{index}"),
                "start",
                1,
            );
            if index < 3 {
                assert!(result.is_ok());
            } else {
                assert!(result.is_err());
            }
        }
        // Stops can use the reserved fourth pending slot.
        assert!(new_admission(&mut snapshot, "stop", "hs", "os", "stop", 1).is_ok());
        assert_eq!(snapshot.operations.len(), 4);
    }

    #[test]
    fn corrupt_registry_fails_closed_instead_of_becoming_empty() {
        let root = root();
        fs::create_dir_all(root.join("multistandby")).unwrap();
        fs::write(root.join("multistandby/operations-v1.json"), b"not-json").unwrap();
        let store = Store::open(&root).unwrap();
        assert!(store.lookup("request", "hash").is_err());
        assert_eq!(store.status().state, "unavailable");
        fs::remove_dir_all(root).unwrap();
    }
}
