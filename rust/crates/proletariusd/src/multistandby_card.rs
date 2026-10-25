//! Retained short physical workflows for N-wait. Network sessions retain only
//! frozen identity; the existing business/FIFO transaction authorizes this work.
use crate::reader_pcsc::{Binding, Owner};
use crate::reader_reservation::{CardTransaction, Gate};
use hyper::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
mod native;
#[cfg(test)]
pub(crate) mod production;
#[cfg(test)]
mod tests;
mod workflow;
pub(crate) use native::{NativeAction, NativeRequest, NativeResult};

#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
}
impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message: message.into(),
        }
    }
    fn invalid() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message: "无效的卡片工作请求".into(),
        }
    }
    fn physical(e: crate::reader_pcsc::Failure) -> Self {
        Self {
            status: e.status,
            code: e.code,
            message: e.message,
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    #[serde(skip)]
    Inventory,
    Identity,
    Prepare,
    Authenticate,
}
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Submit {
    operation_id: String,
    action: Action,
    reader: String,
    reader_generation: u64,
    transaction: CardTransaction,
    #[serde(default)]
    pcsc: Option<Binding>,
    eid: String,
    #[serde(default)]
    aid: String,
    #[serde(default)]
    iccid: String,
    #[serde(default)]
    expected_imsi: String,
    #[serde(default)]
    application: String,
    #[serde(default)]
    rand: Option<[u8; 16]>,
    #[serde(default)]
    autn: Option<[u8; 16]>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
struct Query {
    operation_id: String,
    transaction: CardTransaction,
}
struct Entry {
    request: Option<Submit>,
    transaction: CardTransaction,
    cancelled: Arc<AtomicBool>,
    receipt: Value,
    completed: bool,
}
#[derive(Default)]
struct Registry {
    entries: HashMap<String, Entry>,
    stopping: bool,
}
struct Inner {
    owner: Owner,
    gate: Arc<Gate>,
    ble_socket: Option<PathBuf>,
    app_token: Option<String>,
    registry: Mutex<Registry>,
    pool: crate::blocking_pool::BlockingPool,
    changed: tokio::sync::Notify,
}
#[derive(Clone)]
pub(crate) struct Service(Arc<Inner>);
impl Service {
    pub(crate) fn new(
        owner: Owner,
        gate: Arc<Gate>,
        ble_socket: Option<PathBuf>,
        app_token: Option<String>,
    ) -> Self {
        Self(Arc::new(Inner {
            owner,
            gate,
            ble_socket,
            app_token,
            registry: Mutex::new(Registry::default()),
            pool: crate::blocking_pool::BlockingPool::new(4),
            changed: tokio::sync::Notify::new(),
        }))
    }
    fn reap_retired_receipts(&self, registry: &mut Registry) {
        if registry.entries.len() < 2048 {
            return;
        }
        registry.entries.retain(|_, entry| {
            if !entry.completed {
                return true;
            }
            let CardTransaction::Operation { binding } = &entry.transaction else {
                return true;
            };
            // Replays can only execute after fresh admission. Once this exact
            // capability has retired, forgetting its completed receipt cannot
            // replay physical work; a lost waiter can still cancel to fence it.
            self.0
                .gate
                .validate_card_workflow(
                    &entry.transaction,
                    &binding.reader,
                    binding.reader_generation,
                )
                .is_ok()
        });
    }
    #[cfg(test)]
    pub(crate) async fn handle(&self, action: &str, value: Value) -> Result<Value, Failure> {
        match action {
            "submit" => self.submit(serde_json::from_value(value).map_err(|_| Failure::invalid())?),
            "status" | "cancel" => {
                let q: Query = serde_json::from_value(value).map_err(|_| Failure::invalid())?;
                let mut registry = self.0.registry.lock().unwrap_or_else(|e| e.into_inner());
                if q.operation_id.is_empty()
                    || q.operation_id.len() > 128
                    || !matches!(q.transaction, CardTransaction::Operation { .. })
                {
                    return Err(Failure::invalid());
                }
                if action == "cancel" && !registry.entries.contains_key(&q.operation_id) {
                    self.reap_retired_receipts(&mut registry);
                    if registry.entries.len() >= 4096 {
                        return Err(Failure::new("reader_busy", "卡片任务容量已满"));
                    }
                    let receipt = json!({"operation_id":q.operation_id,"state":"completed","error":{"code":"multistandby_cancelled","message":"卡片任务已取消"},"worker_finished":true});
                    registry.entries.insert(
                        q.operation_id.clone(),
                        Entry {
                            request: None,
                            transaction: q.transaction.clone(),
                            cancelled: Arc::new(AtomicBool::new(true)),
                            receipt,
                            completed: true,
                        },
                    );
                }
                let entry = registry.entries.get_mut(&q.operation_id).ok_or_else(|| {
                    Failure::new(
                        "multistandby_operation_unknown",
                        "卡片任务尚未确认，请确认或取消原任务；不得重放切卡",
                    )
                })?;
                if entry.transaction != q.transaction {
                    return Err(Failure::new("owner_mismatch", "卡片任务归属不匹配"));
                }
                if action == "cancel" {
                    entry.cancelled.store(true, Ordering::Release);
                }
                Ok(entry.receipt.clone())
            }
            _ => Err(Failure::invalid()),
        }
    }
    pub(crate) fn begin_shutdown(&self) {
        let mut registry = self.0.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.stopping = true;
        for entry in registry.entries.values() {
            entry.cancelled.store(true, Ordering::Release);
        }
    }
    pub(crate) async fn finish_shutdown(&self) {
        self.shutdown().await;
    }
    pub(crate) async fn shutdown(&self) {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut registry = self.0.registry.lock().unwrap_or_else(|e| e.into_inner());
                registry.stopping = true;
                for entry in registry.entries.values() {
                    entry.cancelled.store(true, Ordering::Release);
                }
                if registry.entries.values().all(|entry| entry.completed) {
                    return;
                }
            }
            changed.await;
        }
    }
    #[cfg(test)]
    fn submit(&self, request: Submit) -> Result<Value, Failure> {
        self.submit_cancelled(request, None, None)
    }
    fn submit_cancelled(
        &self,
        request: Submit,
        cancel: Option<Arc<AtomicBool>>,
        native_reply: Option<tokio::sync::oneshot::Sender<Result<Value, Failure>>>,
    ) -> Result<Value, Failure> {
        request.validate()?;
        let mut registry = self.0.registry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = registry.entries.get(&request.operation_id) {
            return if entry.transaction == request.transaction
                && entry.request.as_ref().is_none_or(|old| old == &request)
            {
                Ok(entry.receipt.clone())
            } else {
                Err(Failure::new(
                    "operation_conflict",
                    "同一任务标识不能更换卡片请求",
                ))
            };
        }
        if registry.stopping {
            return Err(Failure::new("backend_stopping", "服务正在退出"));
        }
        // Only retired exact capabilities may lose completed receipts.
        self.reap_retired_receipts(&mut registry);
        if registry.entries.len() >= 4096
            || registry.entries.values().filter(|e| !e.completed).count() >= 16
        {
            return Err(Failure::new("reader_busy", "卡片任务容量已满"));
        }
        self.0
            .gate
            .validate_card_workflow(
                &request.transaction,
                &request.reader,
                request.reader_generation,
            )
            .map_err(|e| Failure::new(e, "卡片任务归属已失效"))?;
        let CardTransaction::Operation { binding } = &request.transaction else {
            return Err(Failure::invalid());
        };
        let permit = self
            .0
            .gate
            .track_card_io(&request.transaction, &binding.producer_id, &request.reader)
            .map_err(|e| Failure::new(e, "卡片事务不可用"))?;
        if let Some(pcsc) = &request.pcsc {
            self.0
                .gate
                .register_card_pcsc(&request.transaction, pcsc)
                .map_err(|e| Failure::new(e, "物理卡片事务不可用"))?;
        }
        let cancelled = cancel.unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        let receipt =
            json!({"operation_id":request.operation_id,"state":"running","worker_finished":false});
        registry.entries.insert(
            request.operation_id.clone(),
            Entry {
                request: Some(request.clone()),
                transaction: request.transaction.clone(),
                cancelled: cancelled.clone(),
                receipt: receipt.clone(),
                completed: false,
            },
        );
        let service = self.clone();
        tokio::spawn(async move {
            let worker_service = service.clone();
            let id = request.operation_id.clone();
            // The spawned task outlives every HTTP waiter. Only actual worker
            // completion drops the physical/Gate guards and publishes terminal.
            // worker_finished proves this workflow's APDU/recovery work joined;
            // the Go caller still releases the original PC/SC lease and waits
            // for its separate physical-release receipt before releasing Gate.
            let result = service
                .0
                .pool
                .run(move || {
                    let _permit = permit;
                    workflow::execute(&worker_service.0, &request, cancelled)
                })
                .await;
            let result = result.unwrap_or_else(|_| {
                Err(Failure::new(
                    "multistandby_worker_failed",
                    "卡片工作线程异常结束",
                ))
            });
            if let Some(reply) = native_reply {
                let _ = reply.send(result.clone());
            }
            let mut registry = service.0.registry.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = registry.entries.get_mut(&id) {
                entry.completed = true;
                entry.receipt = match result {
                    Ok(result) => {
                        json!({"operation_id":id,"state":"completed","result":result,"worker_finished":true})
                    }
                    Err(error) => {
                        json!({"operation_id":id,"state":"completed","error":{"code":error.code,"message":error.message},"worker_finished":true})
                    }
                };
            }
            drop(registry);
            service.0.changed.notify_waiters();
        });
        Ok(receipt)
    }
}
impl Submit {
    fn validate(&self) -> Result<(), Failure> {
        if self.operation_id.is_empty()
            || self.operation_id.len() > 128
            || self.reader.is_empty()
            || self.reader_generation == 0
            || (self.action != Action::Inventory
                && (self.eid.len() != 32 || !self.eid.bytes().all(|b| b.is_ascii_hexdigit())))
        {
            return Err(Failure::invalid());
        }
        let CardTransaction::Operation { binding } = &self.transaction else {
            return Err(Failure::invalid());
        };
        if binding.reader != self.reader
            || binding.reader_generation != self.reader_generation
            || binding.purpose != "card"
        {
            return Err(Failure::invalid());
        }
        if !self.aid.is_empty()
            && (self.aid.len() < 10
                || self.aid.len() > 32
                || self.aid.len() % 2 != 0
                || !self.aid.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(Failure::invalid());
        }
        if self.pcsc.is_none() && !crate::reader_ble_apdu::valid_ble_reader(&self.reader) {
            return Err(Failure::invalid());
        }
        if !matches!(self.action, Action::Identity | Action::Inventory)
            && (!(10..=20).contains(&normalize_iccid(&self.iccid).len())
                || !normalize_iccid(&self.iccid)
                    .bytes()
                    .all(|b| b.is_ascii_digit()))
        {
            return Err(Failure::invalid());
        }
        if self.action == Action::Authenticate
            && (self.expected_imsi.is_empty()
                || !matches!(self.application.as_str(), "usim" | "isim")
                || self.rand.is_none()
                || self.autn.is_none())
        {
            return Err(Failure::invalid());
        }
        Ok(())
    }
}
fn normalize_iccid(value: &str) -> String {
    value.trim().trim_end_matches(['F', 'f']).to_owned()
}

#[cfg(test)]
pub(crate) fn owns(path: &str) -> bool {
    matches!(
        path,
        "/api/internal/multistandby/card/submit"
            | "/api/internal/multistandby/card/status"
            | "/api/internal/multistandby/card/cancel"
    )
}

#[cfg(test)]
pub(crate) async fn handle_http(
    request: hyper::Request<hyper::body::Incoming>,
    service: Service,
    stopping: bool,
) -> hyper::Response<http_body_util::Full<bytes::Bytes>> {
    use http_body_util::{BodyExt, Limited};
    let action = request
        .uri()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_owned();
    if request.method() != hyper::Method::POST || !owns(request.uri().path()) {
        return crate::json_response(
            StatusCode::NOT_FOUND,
            &json!({"code":"route_not_found","error":"Unknown card workflow route"}),
        );
    }
    if stopping {
        service.begin_shutdown();
    }
    let body = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        Limited::new(request.into_body(), 16 * 1024).collect(),
    )
    .await
    {
        Ok(Ok(body)) => body.to_bytes(),
        _ => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"code":"invalid_request","error":"Invalid card workflow body"}),
            );
        }
    };
    let result = match serde_json::from_slice(&body) {
        Ok(value) => service.handle(&action, value).await,
        Err(_) => Err(Failure::invalid()),
    };
    match result {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(error) => crate::json_response(
            error.status,
            &json!({"code":error.code,"error":error.message}),
        ),
    }
}
