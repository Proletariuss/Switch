//! Native USB SMS scheduling, projection and persistence. The same Rust USB
//! owner performs the fixed CMGF/CPMS/CMGL transaction without a Go callback.
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use proletarius_store::ArchiveStore;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::availability::BackendAvailability;
use crate::message_archive::InboxState;
use crate::usb_at::{SmsObservationToken, SmsPollBinding};

#[path = "usb_sms_storage_service.rs"]
mod storage_service;
pub(crate) use storage_service::FrozenDeletionSelection;
pub(crate) use storage_service::PublicAction;

pub(crate) const INTERNAL_PREFIX: &str = "/api/internal/usb-sms/";
const FIRST_POLL_DELAY: Duration = Duration::from_millis(1200);
const DEFAULT_INTERVAL: Duration = Duration::from_secs(8);
const MAX_REFRESH_WAITERS: usize = 32;
const MAX_STORAGE_BYTES: usize = 240 * 1024;

#[derive(Clone)]
pub(crate) struct Service {
    inner: Arc<Inner>,
}
struct Inner {
    archive: ArchiveStore,
    inbox: InboxState,
    source: SnapshotSource,
    #[cfg(test)]
    availability: BackendAvailability,
    state: Mutex<State>,
    wake: Arc<Notify>,
    finished: Notify,
    task: Mutex<Option<JoinHandle<()>>>,
    lifecycle: tokio::sync::Mutex<()>,
    mutation_slots: Arc<tokio::sync::Semaphore>,
    #[cfg(test)]
    archive_started: Notify,
    #[cfg(test)]
    storage_fixture: Mutex<Option<storage_service::StorageFixture>>,
}
enum SnapshotSource {
    Native(crate::usb_at::Owner),
    #[cfg(test)]
    Fixture(tokio::sync::mpsc::UnboundedSender<FixtureSnapshotReply>),
}
#[cfg(test)]
type FixtureSnapshotReply = oneshot::Sender<(StatusCode, Value)>;
#[derive(Clone, Debug, Eq, PartialEq)]
enum PollAuthority {
    Native(SmsPollBinding),
    #[cfg(test)]
    Fixture,
}
impl SnapshotSource {
    fn binding(&self, producer: &str) -> Result<Option<PollAuthority>, Failure> {
        match self {
            Self::Native(owner) => owner
                .sms_poll_binding(producer)
                .map(|binding| binding.map(PollAuthority::Native))
                .map_err(|_| Failure::unavailable()),
            #[cfg(test)]
            Self::Fixture(_) => Ok(Some(PollAuthority::Fixture)),
        }
    }
    fn with_current<R>(
        &self,
        token: Option<&SmsObservationToken>,
        publish: impl FnOnce() -> R,
    ) -> Result<R, Failure> {
        match self {
            Self::Native(owner) => owner
                .with_current_sms_observation(token.ok_or_else(Failure::stale)?, publish)
                .map_err(|_| Failure::stale()),
            #[cfg(test)]
            Self::Fixture(_) => Ok(publish()),
        }
    }
    fn current(&self, token: Option<&SmsObservationToken>) -> bool {
        self.with_current(token, || ()).is_ok()
    }
    async fn snapshot(
        &self,
        authority: &PollAuthority,
    ) -> Result<(StatusCode, Value, Option<SmsObservationToken>), Failure> {
        match (self, authority) {
            (Self::Native(owner), PollAuthority::Native(binding)) => owner
                .snapshot_bound(binding)
                .await
                .map(|value| (StatusCode::OK, value.value, Some(value.token)))
                .map_err(|error| Failure::new(error.status, error.code, error.message, true)),
            #[cfg(test)]
            (Self::Fixture(send), PollAuthority::Fixture) => {
                let (reply, wait) = oneshot::channel();
                send.send(reply).map_err(|_| Failure::unavailable())?;
                wait.await
                    .map(|(status, value)| (status, value, None))
                    .map_err(|_| Failure::unavailable())
            }
            #[cfg(test)]
            _ => Err(Failure::stale()),
        }
    }
}

struct State {
    producer_id: String,
    epoch: u64,
    preview_revision: u64,
    stopping: bool,
    started: bool,
    running: bool,
    polling: bool,
    interval: Duration,
    last_poll: String,
    last_poll_error: String,
    waiters: Vec<oneshot::Sender<Result<usize, Failure>>>,
    mutations: usize,
    proofs: storage_service::ProofRegistry,
}
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    status: StatusCode,
    code: &'static str,
    message: String,
    retryable: bool,
}
impl Failure {
    fn new(
        status: StatusCode,
        code: &'static str,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retryable,
        }
    }
    fn unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "usb_sms_unavailable",
            "USB 短信收件服务暂不可用",
            true,
        )
    }
    fn stale() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "usb_sms_stale_generation",
            "设备服务已重新启动，请重试",
            true,
        )
    }
    fn invalid() -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            "usb_sms_invalid_snapshot",
            "USB 短信快照无效",
            true,
        )
    }
    fn response(self) -> Response<Full<Bytes>> {
        crate::json_response(
            self.status,
            &json!({"error":self.message,"code":self.code,"retryable":self.retryable,"recoverable":self.retryable}),
        )
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Failure {}

impl Service {
    pub(crate) fn new(
        archive: ArchiveStore,
        inbox: InboxState,
        owner: crate::usb_at::Owner,
        availability: BackendAvailability,
        producer_id: String,
    ) -> Self {
        #[cfg(not(test))]
        let _ = availability;
        Self {
            inner: Arc::new(Inner {
                archive,
                inbox,
                source: SnapshotSource::Native(owner),
                #[cfg(test)]
                availability,
                state: Mutex::new(State {
                    producer_id,
                    epoch: 0,
                    preview_revision: 0,
                    stopping: false,
                    started: false,
                    running: false,
                    polling: true,
                    interval: DEFAULT_INTERVAL,
                    last_poll: "0001-01-01T00:00:00Z".into(),
                    last_poll_error: String::new(),
                    waiters: Vec::new(),
                    mutations: 0,
                    proofs: storage_service::ProofRegistry::default(),
                }),
                wake: Arc::new(Notify::new()),
                finished: Notify::new(),
                task: Mutex::new(None),
                lifecycle: tokio::sync::Mutex::new(()),
                mutation_slots: Arc::new(tokio::sync::Semaphore::new(8)),
                #[cfg(test)]
                archive_started: Notify::new(),
                #[cfg(test)]
                storage_fixture: Mutex::new(None),
            }),
        }
    }
    pub(crate) async fn storage_usage_response(
        &self,
        module_sim_busy: bool,
    ) -> Response<Full<Bytes>> {
        let archive = self.inner.archive.clone();
        let archived = match crate::archive_io::run(move || {
            archive
                .load()
                .map(|items| items.len().min(u32::MAX as usize) as u32)
        })
        .await
        {
            Ok(count) => count,
            Err(_) => {
                return crate::json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &json!({"error":"读取本机短信归档失败","code":"sms_archive_read_failed","recoverable":false}),
                );
            }
        };
        let mut usage = serde_json::Map::new();
        usage.insert("LOCAL".into(), json!({"used":archived,"total":1000}));
        if module_sim_busy {
            return crate::json_response(
                StatusCode::OK,
                &json!({"usage":usage,"module_sim_busy":true}),
            );
        }

        let (producer, stopping) = {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (state.producer_id.clone(), state.stopping)
        };
        if stopping {
            return crate::json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error":"USB 短信服务正在退出","code":"usb_sms_unavailable","recoverable":true}),
            );
        }
        let (outcome, observation) = match &self.inner.source {
            SnapshotSource::Native(owner) => {
                let binding = match owner.sms_poll_binding(&producer) {
                    Ok(Some(binding)) => binding,
                    // The archive is available without a bootstrapped card.
                    // Report only LOCAL and never probe hardware for this read.
                    Ok(None) => {
                        return crate::json_response(StatusCode::OK, &json!({"usage":usage}));
                    }
                    Err(_) => return Failure::unavailable().response(),
                };
                match owner.storage_usage_observed(&producer, Some(binding)).await {
                    Ok((outcome, Some(token))) => (outcome, token),
                    Ok(_) => return Failure::stale().response(),
                    Err(error)
                        if matches!(
                            error.code,
                            "usb_at_not_found" | "usb_at_disconnected" | "usb_at_no_device"
                        ) =>
                    {
                        return crate::json_response(StatusCode::OK, &json!({"usage":usage}));
                    }
                    Err(error) if error.code == "usb_at_busy" => {
                        return crate::json_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            &json!({"error":"短信设备正在处理其他事务，请稍后重试","code":"usb_sms_storage_busy","recoverable":true}),
                        );
                    }
                    Err(_) => {
                        return crate::json_response(
                            StatusCode::BAD_GATEWAY,
                            &json!({"error":"短信存储查询未能完成","code":"usb_sms_storage_unconfirmed","recoverable":false,"complete":false}),
                        );
                    }
                }
            }
            #[cfg(test)]
            SnapshotSource::Fixture(_) => {
                return crate::json_response(StatusCode::OK, &json!({"usage":usage}));
            }
        };
        self.inner
            .source
            .with_current(Some(&observation), || {
                for (storage, value) in outcome.usage {
                    usage.insert(storage.as_str().into(), json!(value));
                }
                if !outcome.complete {
                    let error_code = if outcome.error_code.is_empty() {
                        "usb_sms_storage_unconfirmed"
                    } else {
                        &outcome.error_code
                    };
                    return crate::json_response(
                        StatusCode::BAD_GATEWAY,
                        &json!({
                            "error":"短信存储查询未全部确认，请刷新核对",
                            "code":"usb_sms_storage_unconfirmed",
                            "recoverable":false,
                            "complete":false,
                            "uncertain":outcome.uncertain,
                            "storage_restored":outcome.storage_restored,
                            "usage":usage,
                            "details":outcome.details,
                            "deleted_ids":outcome.deleted_tokens,
                            "error_code":error_code
                        }),
                    );
                }
                crate::json_response(StatusCode::OK, &json!({"usage":usage}))
            })
            .unwrap_or_else(Failure::response)
    }

    pub(crate) async fn public_refresh_response(&self) -> Response<Full<Bytes>> {
        let result = tokio::time::timeout(Duration::from_secs(135), self.refresh()).await;
        match result {
            Ok(Ok(count)) => crate::json_response(
                StatusCode::ACCEPTED,
                &json!({"accepted":true,"count":count}),
            ),
            Ok(Err(_)) | Err(_) => crate::json_response(
                StatusCode::BAD_GATEWAY,
                &json!({
                    "error":"USB 短信刷新尚未获得服务确认",
                    "code":"usb_sms_refresh_unconfirmed",
                    "retryable":true,
                    "recoverable":true
                }),
            ),
        }
    }
    pub(crate) fn start(&self) {
        let mut task = self.inner.task.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.started || state.stopping {
            return;
        }
        state.started = true;
        state.running = true;
        *task = Some(tokio::spawn(run_worker(Arc::downgrade(&self.inner))));
    }
    /// Revoke old replies immediately; this never waits for a USB transaction or
    /// fsync. A transaction already committed remains a historical observation.
    pub(crate) fn invalidate_backend(&self, producer_id: String) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.epoch = state.epoch.wrapping_add(1);
        state.producer_id = producer_id;
        state.interval = DEFAULT_INTERVAL;
        state.polling = true;
        state.last_poll = "0001-01-01T00:00:00Z".into();
        state.last_poll_error.clear();
        state.proofs = storage_service::ProofRegistry::default();
        let _ = self.inner.inbox.clear_usb();
        for waiter in state.waiters.drain(..) {
            let _ = waiter.send(Err(Failure::stale()));
        }
        self.inner.wake.notify_one();
    }
    pub(crate) async fn shutdown(&self) -> Result<(), Failure> {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopping = true;
            for waiter in state.waiters.drain(..) {
                let _ = waiter.send(Err(Failure::unavailable()));
            }
        }
        self.inner.wake.notify_one();
        loop {
            let done = self.inner.finished.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            let finished = {
                let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                !state.running && state.mutations == 0
            };
            if finished {
                break;
            }
            done.await;
        }
        let task = self
            .inner
            .task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(task) = task {
            task.await.map_err(|_| Failure::unavailable())?;
        }
        Ok(())
    }
    async fn refresh(&self) -> Result<usize, Failure> {
        let (send, receive) = oneshot::channel();
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopping
                || !state.started
                || self.inner.source.binding(&state.producer_id)?.is_none()
            {
                return Err(Failure::unavailable());
            }
            state.waiters.retain(|waiter| !waiter.is_closed());
            if state.waiters.len() >= MAX_REFRESH_WAITERS {
                return Err(Failure::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "usb_sms_refresh_busy",
                    "短信刷新请求过多",
                    true,
                ));
            }
            state.waiters.push(send);
        }
        self.inner.wake.notify_one();
        receive.await.map_err(|_| Failure::unavailable())?
    }
    pub(crate) fn inbox_snapshot(&self) -> Result<Value, Failure> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let observe = || {
            let messages = self
                .inner
                .inbox
                .usb_snapshot()
                .map_err(|_| Failure::unavailable())?;
            let deletion_senders = if !state.stopping {
                state.proofs.deletion_senders(&state.producer_id)
            } else {
                Vec::new()
            };
            Ok(
                json!({"messages":messages,"deletion_senders":deletion_senders,"polling":state.polling,"poll_interval_s":state.interval.as_secs(),"last_poll":state.last_poll,"last_poll_error":state.last_poll_error}),
            )
        };
        if let Ok(value) = self
            .inner
            .source
            .with_current(state.proofs.observation.as_ref(), observe)
        {
            return value;
        }
        state.proofs = storage_service::ProofRegistry::default();
        self.inner
            .inbox
            .clear_usb_deletion_tokens()
            .map_err(|_| Failure::unavailable())?;
        let messages = self
            .inner
            .inbox
            .usb_snapshot()
            .map_err(|_| Failure::unavailable())?;
        Ok(
            json!({"messages":messages,"deletion_senders":[],"polling":state.polling,"poll_interval_s":state.interval.as_secs(),"last_poll":state.last_poll,"last_poll_error":state.last_poll_error}),
        )
    }

    pub(crate) fn polling_status(&self, configured: Option<(bool, u64)>) -> Value {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let (polling, interval_s) = configured.map_or(
            (state.polling, state.interval.as_secs()),
            |(serial, interval_ms)| (!serial, interval_ms / 1000),
        );
        json!({
            "polling": polling,
            "poll_interval_s": interval_s,
            "last_poll": state.last_poll,
            "last_poll_error": state.last_poll_error,
        })
    }

    fn prune(&self, ids: &[String]) -> Result<usize, Failure> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let deleted = self
            .inner
            .inbox
            .prune_usb(ids)
            .map_err(|_| Failure::unavailable())?;
        if deleted > 0 {
            state.preview_revision = state.preview_revision.wrapping_add(1);
            state.proofs.prune_ids(ids);
        }
        Ok(deleted)
    }

    pub(crate) fn prune_public_ids(&self, ids: &[String]) -> Result<usize, ()> {
        self.prune(ids).map_err(|_| ())
    }

    pub(crate) fn frozen_sender_deletion(
        &self,
        sender: &str,
    ) -> Result<FrozenDeletionSelection, Response<Full<Bytes>>> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        self.inner
            .source
            .with_current(state.proofs.observation.as_ref(), || {
                state
                    .proofs
                    .frozen_sender_selection(&state.producer_id, sender)
            })
            .map_err(Failure::response)?
            .map_err(Failure::response)
    }

    pub(crate) fn frozen_storage_deletion(
        &self,
        storages: &[crate::usb_at_storage::Storage],
    ) -> Result<FrozenDeletionSelection, Response<Full<Bytes>>> {
        let selected: std::collections::BTreeSet<_> = storages.iter().copied().collect();
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        self.inner
            .source
            .with_current(state.proofs.observation.as_ref(), || {
                state
                    .proofs
                    .frozen_storage_selection(&state.producer_id, &selected)
            })
            .map_err(Failure::response)?
            .map_err(Failure::response)
    }

    pub(crate) async fn execute_public_storage_operation(
        &self,
        action: storage_service::PublicAction,
    ) -> Result<Value, Response<Full<Bytes>>> {
        self.execute_public_storage_unleased(action)
            .await
            .map_err(Failure::response)
    }
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    service: Service,
    stopping: bool,
) -> Response<Full<Bytes>> {
    match (request.method(), request.uri().path()) {
        (&Method::POST, "/api/internal/usb-sms/storage-operation") => {
            storage_service::handle(request, service, stopping).await
        }
        (&Method::GET, "/api/internal/usb-sms/inbox") => match service.inbox_snapshot() {
            Ok(value) => crate::json_response(StatusCode::OK, &value),
            Err(error) => error.response(),
        },
        (&Method::POST, "/api/internal/usb-sms/refresh") => {
            if stopping {
                return Failure::unavailable().response();
            }
            match service.refresh().await {
                Ok(count) => crate::json_response(
                    StatusCode::ACCEPTED,
                    &json!({"accepted":true,"count":count}),
                ),
                Err(error) => error.response(),
            }
        }
        (&Method::POST, "/api/internal/usb-sms/prune") => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Prune {
                ids: Vec<String>,
            }
            let bytes = match crate::http_server::collect_body_with_timeout(
                request.into_body(),
                40 * 1024,
                Duration::from_secs(5),
            )
            .await
            {
                Ok(bytes) => bytes,
                Err(crate::http_server::BodyReadError::Timeout) => {
                    return Failure::new(
                        StatusCode::REQUEST_TIMEOUT,
                        "request_timeout",
                        "读取请求超时",
                        true,
                    )
                    .response();
                }
                Err(crate::http_server::BodyReadError::TooLarge) => {
                    return Failure::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "request_too_large",
                        "请求过大",
                        false,
                    )
                    .response();
                }
                Err(crate::http_server::BodyReadError::Invalid) => {
                    return Failure::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        "请求格式错误",
                        false,
                    )
                    .response();
                }
            };
            let input: Prune = match serde_json::from_slice(&bytes) {
                Ok(input) => input,
                Err(_) => {
                    return Failure::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        "请求格式错误",
                        false,
                    )
                    .response();
                }
            };
            if input.ids.len() > 500
                || input.ids.iter().any(|id| {
                    id.len() != 64
                        || !id
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            {
                return Failure::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "短信标识无效",
                    false,
                )
                .response();
            }
            match service.prune(&input.ids) {
                Ok(count) => crate::json_response(
                    StatusCode::OK,
                    &json!({"accepted":true,"deleted_count":count}),
                ),
                Err(error) => error.response(),
            }
        }
        _ => crate::json_response(
            StatusCode::NOT_FOUND,
            &json!({"error":"USB SMS route not found","code":"route_not_found","retryable":false}),
        ),
    }
}

async fn run_worker(weak: Weak<Inner>) {
    let _finished = WorkerFinished(weak.clone());
    let mut generation = None;
    let mut deadline = Instant::now() + FIRST_POLL_DELAY;
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let wake = inner.wake.clone();
        match next_step(&inner, &mut generation, &mut deadline) {
            Step::Stop => return,
            Step::Ready => {
                drop(inner);
                tokio::select! { _ = wake.notified() => {}, _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
            }
            Step::Delay => {
                drop(inner);
                tokio::select! { _ = wake.notified() => {}, _ = tokio::time::sleep_until(deadline) => {} }
            }
            Step::Collect {
                identity,
                producer_id,
                waiters,
            } => {
                // The task owns the transaction, not any one HTTP refresh
                // caller. Dropping a receiver cannot cancel a read or fsync.
                let result = collect_once(&inner, identity, &producer_id).await;
                deadline = Instant::now()
                    + inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .interval;
                for waiter in waiters {
                    let _ = waiter.send(result.clone());
                }
            }
        }
    }
}

enum Step {
    Stop,
    Ready,
    Delay,
    Collect {
        identity: u64,
        producer_id: String,
        waiters: Vec<oneshot::Sender<Result<usize, Failure>>>,
    },
}
fn next_step(
    inner: &Inner,
    generation: &mut Option<(u64, PollAuthority)>,
    deadline: &mut Instant,
) -> Step {
    let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.stopping {
        return Step::Stop;
    }
    let Some(authority) = inner.source.binding(&state.producer_id).ok().flatten() else {
        *generation = None;
        for waiter in state.waiters.drain(..) {
            let _ = waiter.send(Err(Failure::unavailable()));
        }
        return Step::Ready;
    };
    let identity = state.epoch;
    if generation.as_ref() != Some(&(identity, authority.clone())) {
        *generation = Some((identity, authority));
        *deadline = Instant::now() + FIRST_POLL_DELAY;
    }
    if state.waiters.is_empty() && Instant::now() < *deadline {
        return Step::Delay;
    }
    Step::Collect {
        identity,
        producer_id: state.producer_id.clone(),
        waiters: std::mem::take(&mut state.waiters),
    }
}
struct WorkerFinished(Weak<Inner>);
impl Drop for WorkerFinished {
    fn drop(&mut self) {
        if let Some(inner) = self.0.upgrade() {
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.running = false;
            for waiter in state.waiters.drain(..) {
                let _ = waiter.send(Err(Failure::unavailable()));
            }
            inner.finished.notify_waiters();
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSnapshot {
    producer_id: String,
    mode: String,
    adopt: bool,
    poll_interval_ms: u64,
    captured_at: String,
    session_id: String,
    #[serde(default)]
    card_identity: String,
    storages: Vec<RawStorage>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStorage {
    storage: String,
    #[serde(default)]
    response: String,
    #[serde(default)]
    error_code: String,
}
fn current(inner: &Inner, identity: u64, producer_id: &str) -> bool {
    let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    state.epoch == identity && state.producer_id == producer_id
}

async fn collect_once(inner: &Inner, identity: u64, producer_id: &str) -> Result<usize, Failure> {
    // Held through the actual durable commit, not only USB I/O. A storage
    // mutation cannot finish and then be undone by a previously read scan.
    let _lifecycle = inner.lifecycle.lock().await;
    if !current(inner, identity, producer_id) {
        return Err(Failure::stale());
    }
    let preview_revision = inner
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .preview_revision;
    let authority = inner
        .source
        .binding(producer_id)?
        .ok_or_else(Failure::unavailable)?;
    let response = inner.source.snapshot(&authority).await;
    if !current(inner, identity, producer_id) {
        return Err(Failure::stale());
    }
    let mut publication_token = match &authority {
        PollAuthority::Native(binding) => Some(binding.observation_token()),
        #[cfg(test)]
        PollAuthority::Fixture => None,
    };
    let result = async {
        let (status, value, observation) = response?;
        publication_token = observation.clone();
        if !inner.source.current(observation.as_ref()) {
            return Err(Failure::stale());
        }
        if !status.is_success() {
            return Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "usb_sms_snapshot_failed",
                "USB 短信读取失败",
                true,
            ));
        }
        let snapshot: RawSnapshot =
            serde_json::from_value(value).map_err(|_| Failure::invalid())?;
        validate_snapshot(&snapshot)?;
        if snapshot.producer_id != producer_id {
            return Err(Failure::stale());
        }
        {
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.epoch != identity || state.producer_id != producer_id {
                return Err(Failure::stale());
            }
            inner.source.with_current(observation.as_ref(), || {
                state.interval = Duration::from_millis(snapshot.poll_interval_ms);
                state.polling = snapshot.mode != "serial";
            })?;
        }
        if snapshot.mode == "busy" {
            return Err(Failure::new(
                StatusCode::CONFLICT,
                "usb_sms_busy",
                "模块正在占用 USB 短信通道，请稍后刷新",
                true,
            ));
        }
        if snapshot.mode == "serial" {
            let mut state = inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            inner.source.with_current(observation.as_ref(), || {
                state.proofs = storage_service::ProofRegistry::default();
                inner
                    .inbox
                    .clear_usb_deletion_tokens()
                    .map_err(|_| Failure::unavailable())
            })??;
            return Ok(inner
                .inbox
                .usb_snapshot()
                .map_err(|_| Failure::unavailable())?
                .len());
        }
        let projection = storage_service::project_with_proofs(&snapshot)?;
        let mut messages = projection.messages.clone();
        if !current(inner, identity, producer_id) || !inner.source.current(observation.as_ref()) {
            return Err(Failure::stale());
        }
        {
            let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.preview_revision != preview_revision {
                // An explicit deletion raced this read. Keep the current inbox;
                // the next fresh scan can add arrivals absent from that view.
                return Ok(inner
                    .inbox
                    .usb_snapshot()
                    .map_err(|_| Failure::unavailable())?
                    .len());
            }
        }
        if snapshot.adopt && !messages.is_empty() {
            let commit = {
                let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.epoch != identity || state.producer_id != producer_id {
                    return Err(Failure::stale());
                }
                if state.preview_revision != preview_revision {
                    return Ok(inner
                        .inbox
                        .usb_snapshot()
                        .map_err(|_| Failure::unavailable())?
                        .len());
                }
                // Admission and revocation are ordered by the same owner lock.
                // Once admitted, this task owns the historical write through
                // its actual completion; no owner lock spans archive I/O.
                inner.source.with_current(observation.as_ref(), || {
                    tokio::spawn(crate::message_archive::ingest_messages_to(
                        inner.archive.clone(),
                        messages.clone(),
                    ))
                })?
            };
            #[cfg(test)]
            inner.archive_started.notify_one();
            commit
                .await
                .map_err(|_| Failure::unavailable())?
                .map_err(|_| {
                    Failure::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "sms_archive_unavailable",
                        "短信本地保存失败",
                        true,
                    )
                })?;
        }
        for message in &mut messages {
            if let Some(content) = message["content"].as_str() {
                let code = extract_code(content);
                if !code.is_empty() {
                    message["code"] = json!(code);
                }
            }
        }
        let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.epoch != identity || state.producer_id != producer_id {
            return Err(Failure::stale());
        }
        inner.source.with_current(observation.as_ref(), || {
            // A write that started before a concurrent prune/backend revocation is
            // an accepted historical observation and is not rolled back. It must
            // not repopulate a newer volatile view from an older read, however.
            let count = if state.preview_revision == preview_revision {
                if state.proofs.observation != observation {
                    // An opaque deletion capability cannot revive after a
                    // configuration or physical-identity generation changes,
                    // even when the same PDU occupies the same storage slot.
                    state.proofs = storage_service::ProofRegistry::default();
                }
                state.proofs.publish(&snapshot, projection, &mut messages);
                state.proofs.observation = observation.clone();
                inner
                    .inbox
                    .merge_usb(messages)
                    .map_err(|_| Failure::unavailable())?
            } else {
                inner
                    .inbox
                    .usb_snapshot()
                    .map_err(|_| Failure::unavailable())?
                    .len()
            };
            state.last_poll =
                normalized_time(&snapshot.captured_at).ok_or_else(Failure::invalid)?;
            state.last_poll_error.clear();
            Ok(count)
        })?
    }
    .await;
    if let Err(error) = &result {
        // Skipped/busy scans did not inspect storage and must not pretend that
        // a new poll completed. A revoked process cannot update current status.
        if error.code != "usb_sms_busy" && error.code != "usb_sms_stale_generation" {
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.epoch == identity && state.producer_id == producer_id {
                let _ = inner.source.with_current(publication_token.as_ref(), || {
                    state.last_poll = OffsetDateTime::now_utc()
                        .format(&Rfc3339)
                        .unwrap_or_default();
                    state.last_poll_error = error.message.clone();
                });
            }
        }
    }
    result
}

fn validate_snapshot(snapshot: &RawSnapshot) -> Result<(), Failure> {
    if snapshot.producer_id.is_empty()
        || snapshot.producer_id.len() > 128
        || !(1_000..=86_400_000).contains(&snapshot.poll_interval_ms)
        || normalized_time(&snapshot.captured_at).is_none()
        || snapshot.session_id.len() > 128
        || (!snapshot.card_identity.is_empty()
            && (!(10..=22).contains(&snapshot.card_identity.len())
                || !snapshot
                    .card_identity
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())))
    {
        return Err(Failure::invalid());
    }
    match snapshot.mode.as_str() {
        "usb" => {
            if snapshot.session_id.is_empty() || snapshot.storages.len() != 2 {
                return Err(Failure::invalid());
            }
            for (storage, expected) in snapshot.storages.iter().zip(["SM", "ME"]) {
                if storage.storage != expected
                    || storage.response.len() > MAX_STORAGE_BYTES
                    || storage.response.is_empty() == storage.error_code.is_empty()
                {
                    return Err(Failure::invalid());
                }
                if !storage.error_code.is_empty()
                    && !matches!(
                        storage.error_code.as_str(),
                        "select_failed"
                            | "read_failed"
                            | "incomplete_response"
                            | "response_too_large"
                    )
                {
                    return Err(Failure::invalid());
                }
                if storage.error_code.is_empty()
                    && storage
                        .response
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .next_back()
                        != Some("OK")
                {
                    return Err(Failure::invalid());
                }
            }
        }
        "serial" | "absent" | "busy" if snapshot.storages.is_empty() => {}
        _ => return Err(Failure::invalid()),
    }
    Ok(())
}
fn project_snapshot(snapshot: &RawSnapshot) -> Result<Vec<Value>, Failure> {
    storage_service::project_with_proofs(snapshot).map(|projection| projection.messages)
}
/* Projection without physical authority remains available to legacy tests.
All source references are retained by the private storage service. */
fn project_storage(response: &str, storage: &str, fallback: &str) -> Result<Vec<Value>, Failure> {
    storage_service::project_storage_with_proofs(response, storage, fallback)
        .map(|projection| projection.messages)
}
fn normalized_time(value: &str) -> Option<String> {
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()?
        .format(&Rfc3339)
        .ok()
}
fn extract_code(content: &str) -> String {
    static PATTERNS: OnceLock<[regex::Regex; 2]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| [
        regex::Regex::new(r"(?i)(?:验证码|校验码|动态码|验证代码|verification[\t\n\f\r ]*code|security[\t\n\f\r ]*code|one[-\t\n\f\r ]?time[\t\n\f\r ]*(?:password|code)|otp|passcode|login[\t\n\f\r ]*code|code)[^0-9]{0,12}([0-9]{4,8})").expect("fixed SMS code pattern"),
        regex::Regex::new(r"(?i)([0-9]{4,8})[^0-9]{0,12}(?:验证码|校验码|动态码|verification[\t\n\f\r ]*code|security[\t\n\f\r ]*code|one[-\t\n\f\r ]?time[\t\n\f\r ]*(?:password|code)|otp|passcode|login[\t\n\f\r ]*code)").expect("fixed SMS code pattern"),
    ]);
    patterns
        .iter()
        .find_map(|pattern| {
            pattern
                .captures(content)
                .and_then(|m| m.get(1).map(|m| m.as_str().to_owned()))
        })
        .unwrap_or_default()
}
fn sms_timestamp(decoded: &Value, fallback: &str) -> Option<String> {
    let Some(value) = decoded
        .get("service_center_time")
        .filter(|value| !value.is_null())
    else {
        return Some(fallback.to_owned());
    };
    let year = i32::try_from(value.get("year")?.as_i64()?).ok()?;
    let month = u8::try_from(value.get("month")?.as_u64()?).ok()?;
    let day = u8::try_from(value.get("day")?.as_u64()?).ok()?;
    let hour = u8::try_from(value.get("hour")?.as_u64()?).ok()?;
    let minute = u8::try_from(value.get("minute")?.as_u64()?).ok()?;
    let second = u8::try_from(value.get("second")?.as_u64()?).ok()?;
    let offset_minutes = i16::try_from(value.get("offset_minutes")?.as_i64()?).ok()?;
    if !(1900..=2099).contains(&year) || !(-840..=840).contains(&offset_minutes) {
        return None;
    }
    let date =
        time::Date::from_calendar_date(year, time::Month::try_from(month).ok()?, day).ok()?;
    let clock = time::Time::from_hms(hour, minute, second).ok()?;
    let offset = time::UtcOffset::from_whole_seconds(i32::from(offset_minutes) * 60).ok()?;
    time::PrimitiveDateTime::new(date, clock)
        .assume_offset(offset)
        .format(&Rfc3339)
        .ok()
}

#[cfg(test)]
mod tests {
    include!("usb_sms_receive_tests.rs");
}
