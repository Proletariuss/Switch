//! Retained module-backed IMS owner. HTTP waiters never own RF or network work.
use super::*;
use crate::module_vowifi_card::ModuleSessionCard;
use crate::ordinary_vowifi_admission::Claim;
use card::OnDemandCard;
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Notify, watch};
#[cfg(test)]
mod persistence_tests;
mod projection;
#[cfg(test)]
mod reconcile_tests;
mod request;
#[path = "module/stop_control.rs"]
mod stop_control;
#[cfg(test)]
mod tests;

const START_ATTEMPT_BUDGET: Duration = Duration::from_secs(90);
const START_WATCHDOG_MARGIN: Duration = Duration::from_secs(1);
// One retry is useful for a transient IMS transport failure, but it needs its
// own preparation/registration budget. Include the bounded backoff without
// allowing an initial request to retry forever.
const START_OPERATION_BUDGET: Duration = Duration::from_secs(196);
const START_RETRY_LIMIT: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PreparationEnd {
    Completed,
    Deadline,
    Stopped,
}

/// A preparation deadline revokes this attempt, not the retained public intent.
/// Keep its future alive until cooperative control-plane cleanup has joined;
/// a second deadline/stop must never drop a CLI's in-progress child.wait().
pub(super) async fn prepare_until<T, F: std::future::Future<Output = T>>(
    deadline: impl std::future::Future<Output = ()>,
    mut stop: watch::Receiver<bool>,
    prepare: impl FnOnce(watch::Receiver<bool>) -> F,
) -> (PreparationEnd, T) {
    let (attempt_stop, attempt_signal) = watch::channel(false);
    let work = prepare(attempt_signal);
    tokio::pin!(work);
    let end = tokio::select! {
        biased;
        _ = async {
            loop {
                if *stop.borrow() {
                    break;
                }
                if stop.changed().await.is_err() {
                    break;
                }
            }
        } => PreparationEnd::Stopped,
        _ = deadline => PreparationEnd::Deadline,
        result = &mut work => return (PreparationEnd::Completed, result),
    };
    attempt_stop.send_replace(true);
    let result = work.await;
    let end = if *stop.borrow() || stop.has_changed().is_err() {
        PreparationEnd::Stopped
    } else {
        end
    };
    (end, result)
}

#[derive(Clone)]
pub(crate) struct Service(Arc<Inner>);
struct Inner {
    lifecycle: Weak<NativeLifecycle>,
    usb: crate::usb_at::Owner,
    modem: crate::modem_actor::ModemActor,
    radio: crate::module_radio::Service,
    reader_gate: Arc<crate::reader_reservation::Gate>,
    admission: Arc<crate::ordinary_vowifi_admission::Gate>,
    transactions: crate::ims_sms_transactions::Service,
    calls: crate::call_facts::CallFacts,
    root: PathBuf,
    epoch: String,
    state: Mutex<State>,
    business: tokio::sync::Mutex<()>,
    #[cfg(test)]
    persistence_hooks: persistence_tests::Hooks,
}
#[derive(Default)]
struct State {
    last_stop: super::stop_control::Retired,
    stopping: bool,
    transport_recovering: bool,
    next: u64,
    record: Option<Arc<Record>>,
    idle_mode: Option<u8>,
    requests: super::operation::RequestLedger,
}
impl State {
    fn check_request(&self, enable: bool) -> Result<(), &'static str> {
        if self.stopping {
            Err("module_vowifi_stopping")
        } else if enable && self.transport_recovering {
            Err("module_vowifi_transport_recovering")
        } else {
            Ok(())
        }
    }
    fn pause_transport(&mut self) {
        self.transport_recovering = true;
        if let Some(record) = &self.record {
            record.stop.send_replace(true);
        }
    }
}
struct Record {
    control: Arc<super::stop_control::Owner>,
    id: u64,
    claim: Arc<Claim>,
    /// Exact connection intent captured when this public operation was
    /// admitted. Internal retries may replace the physical card handle, but
    /// must never drift to a different proxy or ePDG configuration.
    connection: intent::FrozenRequest,
    stop: watch::Sender<bool>,
    state: Mutex<RecordState>,
    completed: Notify,
    join: super::stop_control::TaskJoin,
}
#[derive(Default)]
struct RecordState {
    operation: Option<super::operation::Clock>,
    card: Option<Arc<ModuleSessionCard>>,
    handle: Option<ordinary::Handle>,
    base: Option<Value>,
    operation_done: bool,
    operation_enable: bool,
    cancel_requested: bool,
    operation_deadline: Option<tokio::time::Instant>,
    startup_deadline: Option<tokio::time::Instant>,
    stopping: bool,
    error: Option<String>,
    startup_failure: Option<super::startup_diagnostics::Diagnostic>,
    last_retry_error: Option<String>,
    cleanup_error: Option<String>,
    attempt: u32,
    retrying: bool,
    retired: bool,
    /// No physical/network worker has ever been spawned for this exact claim.
    provisional: bool,
}
impl RecordState {
    /// Called only after the retained worker and RF cleanup have joined and
    /// the ordinary admission claim has been released. An explicit disable
    /// supersedes startup diagnostics, including lease-loss reported while
    /// the card is reacting to that same stop signal.
    fn complete_cleanup(&mut self) {
        self.operation_done = true;
        self.retired = true;
        if !self.operation_enable || self.cancel_requested {
            self.error = None;
            self.startup_failure = None;
            self.last_retry_error = None;
            self.retrying = false;
        }
    }
}

fn initial_retry_allowed(operation_done: bool, completed_retries: u32) -> bool {
    operation_done || completed_retries < START_RETRY_LIMIT
}

fn next_attempt_deadline(
    state: &RecordState,
    now: tokio::time::Instant,
) -> Option<tokio::time::Instant> {
    if state.operation_done {
        return Some(now + START_ATTEMPT_BUDGET);
    }
    state.startup_deadline.and_then(|overall| {
        (overall.saturating_duration_since(now) >= START_ATTEMPT_BUDGET + START_WATCHDOG_MARGIN)
            .then_some(now + START_ATTEMPT_BUDGET)
    })
}

fn retained_failure_message(state: &RecordState, fallback: &str) -> String {
    state
        .startup_failure
        .map(|diagnostic| diagnostic.message.to_owned())
        .or_else(|| state.last_retry_error.clone())
        .unwrap_or_else(|| fallback.to_owned())
}

fn publish_registered_operation(record: &Record, stop: &watch::Receiver<bool>) {
    let mut state = record.state.lock().unwrap_or_else(|e| e.into_inner());
    // A disable can be admitted after the caller observed Registered but before
    // this lock is acquired. Recheck both retained facts while holding the
    // record lock so registration can never overwrite an in-flight cleanup.
    if !state.operation_done && !state.stopping && !*stop.borrow() {
        state.operation_done = true;
        state.error = None;
        state.startup_failure = None;
        state.last_retry_error = None;
        state.retrying = false;
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(record) = &self
            .state
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .record
        {
            record.stop.send_replace(true);
        }
    }
}
impl NativeLifecycle {
    pub(crate) fn module_service(
        self: &Arc<Self>,
        state: &crate::ServerState,
    ) -> Result<Service, String> {
        self.module_service
            .get_or_init(|| {
                Ok(Service(Arc::new(Inner {
                    lifecycle: Arc::downgrade(self),
                    usb: state.usb_at.clone(),
                    modem: state.modem_actor.clone(),
                    radio: state.module_radio.clone(),
                    reader_gate: state.reader_gate.clone(),
                    admission: self.ordinary_admission(),
                    transactions: state.ims_sms_transactions.clone(),
                    calls: state.call_facts.clone(),
                    root: state.multistandby_store.data_root().to_path_buf(),
                    epoch: super::stop_control::new_epoch()?,
                    state: Mutex::new(State {
                        requests: super::operation::RequestLedger::open(
                            state.multistandby_store.data_root(),
                            "module",
                        )?,
                        ..State::default()
                    }),
                    business: tokio::sync::Mutex::new(()),
                    #[cfg(test)]
                    persistence_hooks: persistence_tests::Hooks::default(),
                })))
            })
            .clone()
    }
    pub(crate) fn retained_module_service(&self) -> Option<Service> {
        self.module_service
            .get()
            .and_then(|value| value.as_ref().ok())
            .cloned()
    }
}
impl Service {
    fn stop_target_for(
        &self,
        projection: &super::operation::Projection,
    ) -> Option<super::stop_control::Target> {
        let state = self.0.state.lock().ok()?;
        let record = state.record.as_ref()?;
        let (current, _) = self.operation_observation_in_state(&state, Some(record));
        (current.operation_id == projection.operation_id
            && current.generation == projection.generation)
            .then(|| record.control.target("module", &self.0.epoch, record.id))
    }
    fn lookup_request(
        &self,
        request_id: &str,
        fingerprint: Option<&str>,
    ) -> Result<Option<super::operation::Receipt>, &'static str> {
        self.lookup_request_with_observation(request_id, fingerprint)
            .map(|(receipt, _, _)| receipt)
    }

    fn lookup_request_with_observation(
        &self,
        request_id: &str,
        fingerprint: Option<&str>,
    ) -> Result<
        (
            Option<super::operation::Receipt>,
            super::operation::Projection,
            Value,
        ),
        &'static str,
    > {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        self.lookup_request_locked(&mut state, request_id, fingerprint)
    }

    fn lookup_request_locked(
        &self,
        state: &mut State,
        request_id: &str,
        fingerprint: Option<&str>,
    ) -> Result<
        (
            Option<super::operation::Receipt>,
            super::operation::Projection,
            Value,
        ),
        &'static str,
    > {
        state.requests.lookup(request_id, fingerprint)?;
        // The admission/receipt lock is the observation transaction boundary.
        // A successor record or Clock sequence cannot insert its receipt after
        // this sample and then be retired by an older projection.
        let (current, status) = self.operation_observation_in_state(state, state.record.as_ref());
        state.requests.observe(Some(current.clone()))?;
        let receipt = state.requests.lookup(request_id, fingerprint)?;
        Ok((receipt, current, status))
    }

    fn remember_request(
        &self,
        state: &mut State,
        options: &super::operation::Options,
        fingerprint: &str,
        record: Option<&Arc<Record>>,
    ) -> Result<Option<super::operation::Receipt>, &'static str> {
        let Some(request_id) = options.request_id() else {
            return Ok(None);
        };
        let receipt = if let Some(record) = record {
            let status = record.state.lock().unwrap_or_else(|e| e.into_inner());
            let clock = status
                .operation
                .as_ref()
                .ok_or("vowifi_operation_unavailable")?;
            clock.receipt(request_id, fingerprint, &self.0.epoch, record.id, record.id)
        } else {
            let now = tokio::time::Instant::now();
            let mut clock = super::operation::Clock::start(now, Duration::ZERO);
            clock.stop(now, Duration::ZERO, false)?;
            clock.receipt(request_id, fingerprint, &self.0.epoch, 0, 0)
        };
        #[cfg(test)]
        self.0.persistence_hooks.before_insert(&mut state.requests);
        state.requests.insert(receipt.clone())?;
        Ok(Some(receipt))
    }

    fn remember_staged_stop(
        &self,
        state: &mut State,
        options: Option<&super::operation::Options>,
        fingerprint: &str,
        record: &Record,
        clock: Option<&super::operation::Clock>,
    ) -> Result<Option<super::operation::Receipt>, &'static str> {
        let Some(request_id) = options.and_then(|value| value.request_id()) else {
            return Ok(None);
        };
        let receipt = clock.ok_or("vowifi_operation_unavailable")?.receipt(
            request_id,
            fingerprint,
            &self.0.epoch,
            record.id,
            record.id,
        );
        #[cfg(test)]
        self.0.persistence_hooks.before_insert(&mut state.requests);
        state.requests.insert(receipt.clone())?;
        Ok(Some(receipt))
    }

    /// Only this unpublished claim is releasable without a physical worker.
    /// Keep its Record visible until release has actually been confirmed.
    fn release_provisional_if_any(&self, record: &Record) -> Result<(), &'static str> {
        #[cfg(test)]
        self.0.persistence_hooks.before_provisional_lock();
        let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
        if !status.provisional {
            // Another stop may already have settled this provisional claim.
            // Ordinary started records also no-op here and still owe their
            // normal retained worker/RF join below in stop_before.
            return Ok(());
        }
        if record.join.started() {
            return Err("module_vowifi_cleanup_unconfirmed");
        }
        #[cfg(test)]
        if self.0.persistence_hooks.fail_release() {
            return Err("module_vowifi_cleanup_unconfirmed");
        }
        #[cfg(test)]
        self.0.persistence_hooks.note_actual_release();
        let released = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            record.claim.release_confirmed()
        }));
        if !matches!(released, Ok(Ok(()))) {
            return Err("module_vowifi_cleanup_unconfirmed");
        }
        status.provisional = false;
        status.cleanup_error = None;
        record.join.confirm_unspawned()?;
        status.complete_cleanup();
        drop(status);
        record.completed.notify_waiters();
        Ok(())
    }

    fn reject_provisional(&self, state: &mut State, record: Arc<Record>, error: &'static str) {
        let previous = state.record.replace(record.clone());
        if let Some(previous) = &previous {
            let target = previous
                .control
                .target("module", &self.0.epoch, previous.id);
            super::stop_control::retire(&mut state.last_stop, &previous.control, target);
        }
        {
            let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
            status.error = Some(format!("模块 Wi-Fi Calling: {error}"));
            status.cleanup_error = Some("模块 VoWiFi 未派发；准入释放尚未确认".into());
            status.stopping = true;
        }
        record.stop.send_replace(true);
        if self.release_provisional_if_any(&record).is_ok() {
            state.record = previous;
        }
        // Failed/poisoned release leaves the exact Record/claim visible. A
        // later stop_before may retry this zero-dispatch cleanup; poison is
        // never cleared and cannot be mistaken for a successful retirement.
    }

    /// The service state lock serializes rejection receipts with both owner
    /// admission and reconciliation. A partially admitted receipt always wins.
    fn preadmission_failure(
        &self,
        options: &super::operation::Options,
        fingerprint: &str,
        requested_enable: bool,
        message: &str,
        failure: hyper::Response<Full<Bytes>>,
    ) -> hyper::Response<Full<Bytes>> {
        let Some(request_id) = options.request_id() else {
            return failure;
        };
        let recorded = (|| {
            let mut state = self
                .0
                .state
                .lock()
                .map_err(|_| "module_vowifi_state_unavailable")?;
            if let Some(receipt) = state.requests.lookup(request_id, Some(fingerprint))? {
                let (current, _) =
                    self.operation_observation_in_state(&state, state.record.as_ref());
                return Ok(Some(receipt.resolve(Some(current))));
            }
            state.requests.insert(super::operation::Receipt::rejected(
                request_id,
                fingerprint,
                requested_enable,
                message,
            ))?;
            Ok::<_, &'static str>(None)
        })();
        match recorded {
            Ok(Some(operation)) => response(
                StatusCode::ACCEPTED,
                json!({"accepted":true,"cancelling":false,"operation":operation}),
            ),
            Ok(None) => failure,
            Err(code) => coded(StatusCode::CONFLICT, code, code),
        }
    }

    /// No radio query, retry, stop or physical cleanup is performed here.
    /// Missing means undispatched only while the same durable ledger and all
    /// shared owner/task fences prove a clean process under admission exclusion.
    async fn reconcile_unobserved<B>(
        &self,
        request: hyper::Request<B>,
    ) -> hyper::Response<Full<Bytes>>
    where
        B: hyper::body::Body<Data = Bytes> + Send,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let ingress = std::pin::pin!(request.into_body());
        let bytes = match crate::http_server::collect_body_with_timeout(
            ingress,
            4096,
            Duration::from_secs(2),
        )
        .await
        {
            Ok(body) => body,
            _ => {
                return coded(
                    StatusCode::BAD_REQUEST,
                    "invalid_reconcile_payload",
                    "invalid_reconcile_payload",
                );
            }
        };
        let options = match super::operation::Options::body(&bytes) {
            Ok(options) => options,
            Err(code) => return coded(StatusCode::BAD_REQUEST, code, code),
        };
        // Do not normalize connection fields: even a pre-admission invalid
        // proxy/ePDG payload must retain its exact original identity.
        let enable = match request::requested_enable(&bytes) {
            Ok(enable) => enable,
            Err(_) => {
                return coded(
                    StatusCode::BAD_REQUEST,
                    "invalid_reconcile_payload",
                    "invalid_reconcile_payload",
                );
            }
        };
        let fingerprint = super::operation::request_fingerprint(&bytes);
        let request_id = options
            .request_id()
            .expect("body options require a request ID");
        match self.lookup_request_with_observation(request_id, Some(&fingerprint)) {
            Ok((Some(receipt), current, _)) => {
                return response(
                    StatusCode::OK,
                    json!({"operation":receipt.resolve(Some(current))}),
                );
            }
            Ok((None, _, _)) => {}
            Err(code) => return coded(StatusCode::CONFLICT, code, code),
        }
        let maintenance = match self.0.admission.begin_reader_maintenance(
            "module-vowifi-reconcile",
            0,
            crate::ordinary_vowifi_admission::MaintenancePolicy::RejectAllOrdinary,
        ) {
            Ok(maintenance) => maintenance,
            Err(_) => {
                return coded(
                    StatusCode::CONFLICT,
                    "vowifi_reconcile_owner_unconfirmed",
                    "vowifi_reconcile_owner_unconfirmed",
                );
            }
        };
        let result = (|| {
            let lifecycle = self
                .0
                .lifecycle
                .upgrade()
                .ok_or("vowifi_reconcile_owner_unconfirmed")?;
            // A detached/stale service must never prove absence for the active
            // singleton, including a service retaining another archive cache.
            if !lifecycle
                .retained_module_service()
                .is_some_and(|current| Arc::ptr_eq(&current.0, &self.0))
            {
                return Err("vowifi_reconcile_owner_unconfirmed");
            }
            let lifecycle_state = lifecycle
                .state
                .lock()
                .map_err(|_| "vowifi_reconcile_owner_unconfirmed")?;
            let mut state = self
                .0
                .state
                .lock()
                .map_err(|_| "vowifi_reconcile_owner_unconfirmed")?;
            if !state.requests.durable() {
                return Err("vowifi_request_ledger_unavailable");
            }
            // Revalidate disk completeness at the commit point. Missing/corrupt
            // nonempty archives must not turn earlier dispatch into absence.
            if let Some(receipt) = state.requests.lookup(request_id, Some(&fingerprint))? {
                let (current, _) =
                    self.operation_observation_in_state(&state, state.record.as_ref());
                return Ok(receipt.resolve(Some(current)));
            }
            let owner_clean = !lifecycle_state.stopping
                && !lifecycle_state.finishing
                && lifecycle_state.cleanup_failure.is_none()
                && lifecycle_state.owner.as_ref().is_some_and(|owner| {
                    !owner.stopping
                        && owner.ordinary_tasks.is_empty()
                        && owner.session_tasks.is_empty()
                        && owner.session_task_failure.is_none()
                })
                && state.record.is_none()
                && !state.stopping
                && !state.transport_recovering
                && !self.0.reader_gate.native_reader_session_active();
            if !owner_clean {
                return Err("vowifi_reconcile_owner_unconfirmed");
            }
            let receipt = super::operation::Receipt::rejected(
                request_id,
                &fingerprint,
                enable,
                "原请求在完整回执账本中不存在，Rust 已确认没有模块 owner、任务或待清理资源；未重新提交",
            );
            state.requests.insert(receipt.clone())?;
            Ok(receipt.resolve(None))
        })();
        if maintenance.release_confirmed().is_err() {
            return coded(
                StatusCode::CONFLICT,
                "vowifi_reconcile_owner_unconfirmed",
                "vowifi_reconcile_owner_unconfirmed",
            );
        }
        match result {
            Ok(operation) => response(StatusCode::OK, json!({"operation":operation})),
            Err(code) => coded(StatusCode::CONFLICT, code, code),
        }
    }

    fn record(&self) -> Option<Arc<Record>> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record
            .clone()
    }
    pub(crate) fn owns_session(&self) -> bool {
        self.record()
            .is_some_and(|r| r.claim.try_is_current().unwrap_or(true))
    }
    pub(crate) fn begin_shutdown(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopping = true;
        if let Some(record) = &state.record {
            record.stop.send_replace(true);
        }
    }
    /// Fence both existing and first-ever HTTP starts before discovery retires
    /// its USB producer. Existing cleanup and disable requests remain usable.
    pub(crate) fn pause_transport(&self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pause_transport();
    }
    /// A replacement discovery generation can admit fresh starts, but cannot
    /// revive a stopped attempt or reopen permanent process shutdown admission.
    pub(crate) fn resume_transport(&self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .transport_recovering = false;
    }
    pub(crate) async fn stop_before(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<(), &'static str> {
        let Some(record) = self.record() else {
            return Ok(());
        };
        self.release_provisional_if_any(&record)?;
        {
            let mut state = record.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopping = true;
        }
        record.stop.send_replace(true);
        let joined = tokio::time::timeout_at(deadline, record.join.wait())
            .await
            .map_err(|_| "module_vowifi_cleanup_unconfirmed")??;
        if !joined {
            return Err("module_vowifi_worker_failed");
        }
        if !record.state.lock().map(|s| s.retired).unwrap_or(false)
            || !matches!(record.claim.try_is_current(), Ok(false))
        {
            return Err("module_vowifi_cleanup_unconfirmed");
        }
        Ok(())
    }
    fn request_with_observation(
        &self,
        enable: bool,
        connection: intent::FrozenRequest,
        options: Option<&super::operation::Options>,
        fingerprint: &str,
    ) -> Result<(bool, Option<Arc<Record>>, Option<super::operation::Receipt>), &'static str> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(options) = options
            && let Some(request_id) = options.request_id()
            && let Some(receipt) = state.requests.lookup(request_id, Some(fingerprint))?
        {
            return Ok((false, None, Some(receipt)));
        }
        state.check_request(enable)?;
        if enable && state.record.as_ref().is_some_and(|r| r.control.busy()) {
            return Err("module_vowifi_busy");
        }
        if let Some(options) = options {
            if let Some(record) = &state.record {
                let status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(clock) = &status.operation {
                    clock.validate(options, &self.0.epoch, record.id, record.id)?;
                }
            } else {
                options.validate_identity(&format!("{}-0-1", self.0.epoch), 0)?;
            }
        }
        if let Some(record) = state.record.clone() {
            let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
            if record.claim.try_is_current().unwrap_or(true) {
                if !enable {
                    let cancelling = !status.operation_done && status.operation_enable;
                    if !status.operation_done && !status.operation_enable {
                        return Err("module_vowifi_busy");
                    }
                    let now = tokio::time::Instant::now();
                    let mut staged_clock = status.operation.clone();
                    if let Some(operation) = staged_clock.as_mut() {
                        operation.stop(now, Duration::from_secs(30), cancelling)?;
                    }
                    let record = record.clone();
                    let receipt = self.remember_staged_stop(
                        &mut state,
                        options,
                        fingerprint,
                        &record,
                        staged_clock.as_ref(),
                    )?;
                    if let Some(request_id) = options.and_then(|value| value.request_id()) {
                        #[cfg(test)]
                        self.0
                            .persistence_hooks
                            .before_submitted(&mut state.requests);
                        state.requests.mark_submitted(request_id)?;
                    }
                    // Persistence may have committed without a durability ACK.
                    // Every Err leaves the old live state untouched and the
                    // ledger's existing unknown/failure fence authoritative.
                    status.operation = staged_clock;
                    status.cancel_requested = cancelling;
                    status.operation_enable = cancelling;
                    status.operation_done = false;
                    status.error = None;
                    status.operation_deadline = Some(now + Duration::from_secs(30));
                    status.stopping = true;
                    drop(status);
                    record.stop.send_replace(true);
                    return Ok((cancelling, Some(record), receipt));
                }
                if status.operation_done && !status.stopping {
                    status.error = Some(
                        "模块 Wi-Fi Calling: 已有 VoWiFi 会话正在运行，请先关闭后再切换设备".into(),
                    );
                    let record = record.clone();
                    drop(status);
                    let receipt = options
                        .map(|options| {
                            self.remember_request(&mut state, options, fingerprint, Some(&record))
                        })
                        .transpose()?
                        .flatten();
                    return Ok((false, Some(record), receipt));
                }
                return Err("module_vowifi_busy");
            }
            if !status.retired {
                return Err("module_vowifi_busy");
            }
        }
        if !enable {
            let record = state.record.clone();
            if let Some(record) = &record {
                let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                let now = tokio::time::Instant::now();
                let mut staged_clock = status.operation.clone();
                if let Some(operation) = staged_clock.as_mut() {
                    operation.stop(now, Duration::from_secs(30), false)?;
                }
                let receipt = self.remember_staged_stop(
                    &mut state,
                    options,
                    fingerprint,
                    record,
                    staged_clock.as_ref(),
                )?;
                status.operation = staged_clock;
                status.operation_enable = false;
                status.cancel_requested = false;
                status.complete_cleanup();
                drop(status);
                return Ok((false, Some(record.clone()), receipt));
            }
            let receipt = options
                .map(|options| self.remember_request(&mut state, options, fingerprint, None))
                .transpose()?
                .flatten();
            return Ok((false, None, receipt));
        }
        let id = state.next.checked_add(1).ok_or("module_vowifi_stopping")?;
        let claim = self
            .0
            .admission
            .acquire_native_module(&self.0.epoch, id)
            .map_err(|e| e.code)?;
        // Gate acquisition consumes this sequence even if persistence fails.
        state.next = id;
        let record = Arc::new(Record {
            control: Default::default(),
            id,
            claim,
            connection,
            stop: watch::channel(false).0,
            state: Mutex::new(RecordState {
                operation: Some(super::operation::Clock::start(
                    tokio::time::Instant::now(),
                    START_OPERATION_BUDGET,
                )),
                operation_enable: true,
                provisional: true,
                startup_deadline: Some(tokio::time::Instant::now() + START_OPERATION_BUDGET),
                ..Default::default()
            }),
            completed: Notify::new(),
            join: Default::default(),
        });
        let receipt = match options
            .map(|options| self.remember_request(&mut state, options, fingerprint, Some(&record)))
            .transpose()
        {
            Ok(receipt) => receipt.flatten(),
            Err(error) => {
                self.reject_provisional(&mut state, record, error);
                return Err(error);
            }
        };
        if let Some(request_id) = options.and_then(|value| value.request_id()) {
            #[cfg(test)]
            self.0
                .persistence_hooks
                .before_submitted(&mut state.requests);
            if let Err(error) = state.requests.mark_submitted(request_id) {
                self.reject_provisional(&mut state, record, error);
                return Err(error);
            }
        }
        record
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .provisional = false;
        if let Some(previous) = &state.record {
            let owner = previous.control.clone();
            let target = owner.target("module", &self.0.epoch, previous.id);
            super::stop_control::retire(&mut state.last_stop, &owner, target);
        }
        state.record = Some(record.clone());
        let deadline = record
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .startup_deadline
            .unwrap();
        let weak = Arc::downgrade(&record);
        let mut stopped = record.stop.subscribe();
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    if let Some(record) = weak.upgrade() {
                        let mut state = record.state.lock().unwrap_or_else(|e| e.into_inner());
                        if !state.operation_done && !state.stopping {
                            let message = retained_failure_message(
                                &state,
                                "连接超时，正在释放资源",
                            );
                            state.error = Some(format!("模块 Wi-Fi Calling: {message}"));
                            state.stopping = true;
                            record.stop.send_replace(true);
                        }
                    }
                },
                _ = stopped.changed() => {},
            }
        });
        let resources = self.clone();
        let worker = record.clone();
        record.join.spawn(async move {
            let retained = worker.clone();
            let work = tokio::spawn(async move { resources.run(retained).await });
            if work.await.is_err() {
                worker.state.lock().unwrap_or_else(|e| e.into_inner()).error =
                    Some("模块 Wi-Fi Calling: 会话任务异常，清理尚未确认".into());
                let mut status = worker.state.lock().unwrap_or_else(|e| e.into_inner());
                status.operation_done = true;
                status.stopping = true;
                worker.stop.send_replace(true);
                return;
            }
            // Release the ordinary claim only after the actual retained work
            // (including all network and RF cleanup) has returned and joined.
            if worker.claim.release_confirmed().is_err() {
                return;
            }
            let mut status = worker.state.lock().unwrap_or_else(|e| e.into_inner());
            status.complete_cleanup();
            drop(status);
            worker.completed.notify_waiters();
        })?;
        Ok((false, Some(record), receipt))
    }
    async fn run(&self, record: Arc<Record>) {
        let mut expected = None;
        let mut attempt = 0_u32;
        loop {
            let deadline = {
                let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                let Some(deadline) = next_attempt_deadline(&status, tokio::time::Instant::now())
                else {
                    let message =
                        retained_failure_message(&status, "module_vowifi_retry_budget_exhausted");
                    status.error = Some(format!("模块 Wi-Fi Calling: {message}"));
                    status.retrying = false;
                    return;
                };
                status.attempt = attempt.saturating_add(1);
                status.retrying = attempt > 0;
                deadline
            };
            let card = ModuleSessionCard::begin_prepare_with_serial(
                self.0.usb.clone(),
                self.0.radio.clone(),
                self.0.modem.clone(),
                record.stop.subscribe(),
            );
            {
                let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                status.card = Some(card.clone());
                status.handle = None;
                status.base = None;
            }
            let result = self
                .run_prepared(&record, card.clone(), &mut expected, deadline)
                .await;
            let requested_retry = matches!(&result, Ok(true));
            let retry = if requested_retry {
                let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                if initial_retry_allowed(status.operation_done, attempt) {
                    status.error = None;
                    status.retrying = true;
                    true
                } else {
                    let message = retained_failure_message(&status, "native_vowifi_startup_failed");
                    status.error = Some(format!("模块 Wi-Fi Calling: {message}"));
                    status.retrying = false;
                    false
                }
            } else {
                false
            };
            if let Err(error) = result {
                let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                // A watchdog/cancel path may already carry the authoritative
                // failure. Do not replace it with the timeout observed while
                // the worker is reacting to that stop.
                if status.error.is_none() || !*record.stop.borrow() {
                    let message = retained_failure_message(&status, error.as_str());
                    status.error = Some(format!("模块 Wi-Fi Calling: {message}"));
                }
                status.retrying = false;
            }
            // This path only follows a joined network worker or an unlaunched
            // context. A transport timeout may retain it indefinitely; never claim
            // RF restoration or free the global ordinary slot from a UI timeout.
            loop {
                if ModuleSessionCard::release_with_confirmation(&card)
                    .await
                    .is_ok()
                {
                    break;
                }
                {
                    let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                    status.cleanup_error =
                        Some("模块 Wi-Fi Calling: 无线状态恢复未确认，仍在后台清理".into());
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            record
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cleanup_error = None;
            if !retry || *record.stop.borrow() {
                return;
            }
            attempt = attempt.saturating_add(1);
            let delay = Duration::from_secs((2_u64 << attempt.saturating_sub(1).min(3)).min(16));
            let mut stop = record.stop.subscribe();
            tokio::select! {
                _ = tokio::time::sleep(delay) => {},
                _ = stop.changed() => return,
            }
            if *stop.borrow() {
                return;
            }
        }
    }
    async fn run_prepared(
        &self,
        record: &Arc<Record>,
        card: Arc<ModuleSessionCard>,
        expected: &mut Option<(
            crate::module_vowifi_card::ModulePhysicalIdentity,
            String,
            String,
        )>,
        deadline: tokio::time::Instant,
    ) -> Result<bool, String> {
        self.run_prepared_with(record, card, expected, deadline, |request, card, stop| {
            prepare_card_context_with_stop(request, card, true, Some(stop))
        })
        .await
    }
    async fn run_prepared_with<F>(
        &self,
        record: &Arc<Record>,
        card: Arc<ModuleSessionCard>,
        expected: &mut Option<(
            crate::module_vowifi_card::ModulePhysicalIdentity,
            String,
            String,
        )>,
        deadline: tokio::time::Instant,
        prepare: impl FnOnce(PlanRequest, SessionCard, watch::Receiver<bool>) -> F,
    ) -> Result<bool, String>
    where
        F: std::future::Future<Output = Result<NativeSessionContext, (PlanError, SessionCard)>>,
    {
        match tokio::time::timeout_at(deadline, card.wait_ready()).await {
            Err(_) if !*record.stop.borrow() => {
                let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                if status.last_retry_error.is_none() {
                    status.last_retry_error = Some("module_vowifi_prepare_timeout".into());
                }
                return Ok(true);
            }
            Err(_) => return Err("module_vowifi_prepare_timeout".to_owned()),
            Ok(Err(_)) => {
                return Err(card
                    .status()
                    .code
                    .unwrap_or("module_vowifi_prepare_failed")
                    .to_owned());
            }
            Ok(Ok(())) => {}
        }
        if *record.stop.borrow() {
            return Ok(false);
        }
        let captured = (
            card.physical_identity()
                .ok_or("module_sim_identity_changed")?,
            card.identity().iccid.to_string(),
            card.identity().imsi.to_string(),
        );
        if expected.as_ref().is_some_and(|value| value != &captured) {
            return Err("module_sim_identity_changed".into());
        }
        *expected = Some(captured);
        let (preparation_end, context) = prepare_until(
            tokio::time::sleep_until(deadline),
            record.stop.subscribe(),
            |attempt_stop| {
                prepare(
                    PlanRequest {
                        reader: card.reader().into(),
                        proxy: record.connection.proxy.clone(),
                        epdg: record.connection.epdg.clone(),
                        epdg_port: (record.connection.epdg_port != 0)
                            .then_some(record.connection.epdg_port),
                    },
                    SessionCard::on_demand(card.clone()),
                    attempt_stop,
                )
            },
        )
        .await;
        match preparation_end {
            PreparationEnd::Deadline if !*record.stop.borrow() => {
                let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                if status.last_retry_error.is_none() {
                    let code = match &context {
                        Err((error, _)) if error.code() == "surge_control_join_failed" => {
                            error.code()
                        }
                        _ => "module_vowifi_prepare_timeout",
                    };
                    status.last_retry_error = Some(code.into());
                }
                return Ok(true);
            }
            PreparationEnd::Stopped | PreparationEnd::Deadline => return Ok(false),
            PreparationEnd::Completed => {}
        }
        let context = context.map_err(|(error, _)| error.code().to_owned())?;
        if *record.stop.borrow() {
            return Ok(false);
        }
        let lifecycle = self
            .0
            .lifecycle
            .upgrade()
            .ok_or("module_vowifi_owner_unavailable")?;
        record.state.lock().unwrap_or_else(|e| e.into_inner()).base =
            Some(configuration::live_status(&context));
        let mut handle = lifecycle
            .launch_ordinary(
                context,
                ordinary::Inputs {
                    sms_archive: crate::ims_sms_transactions::NativeSmsArchive::Module,
                    reader_gate: self.0.reader_gate.clone(),
                    settings: service::startup_settings().map_err(str::to_owned)?,
                    data_root: self.0.root.clone(),
                    transactions: self.0.transactions.clone(),
                    call_facts: self.0.calls.clone(),
                },
            )
            .map_err(|_| "module_vowifi_admission_failed".to_owned())?;
        let _stop_guard = service::DriverStopGuard(handle.clone());
        record
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .handle = Some(handle.clone());
        let mut stop = record.stop.subscribe();
        let mut joined = None;
        let mut changes_open = true;
        let mut attempt_timed_out = false;
        loop {
            if *stop.borrow() {
                handle.request_stop();
            }
            let snapshot = handle.snapshot();
            let timed_out =
                !attempt_timed_out && !*stop.borrow() && tokio::time::Instant::now() >= deadline;
            if timed_out {
                attempt_timed_out = true;
                let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                if status.last_retry_error.is_none() {
                    status.last_retry_error = Some("module_vowifi_attempt_timeout".into());
                }
                status.retrying = true;
                handle.request_stop();
            }
            if snapshot.phase == ordinary::Phase::Registered && !*stop.borrow() {
                publish_registered_operation(record, &stop);
            }
            if joined.is_none() {
                joined = lifecycle.take_ordinary_result(handle.generation);
            }
            if joined.is_some()
                && snapshot.phase == ordinary::Phase::Released
                && snapshot.cleanup_confirmed
                && snapshot.lease_released
            {
                return match joined.take().unwrap() {
                    Ok(result) => {
                        let termination = result.termination;
                        {
                            let mut status = record.state.lock().unwrap_or_else(|e| e.into_inner());
                            if status.startup_failure.is_none()
                                && let Some(diagnostic) = termination.startup_failure
                            {
                                status.startup_failure = Some(diagnostic);
                            }
                            if status.last_retry_error.is_none() {
                                status.last_retry_error =
                                    termination.public_error.map(str::to_owned);
                            }
                        }
                        if attempt_timed_out && !*stop.borrow() {
                            Ok(true)
                        } else if !*stop.borrow()
                            && termination.retry == ordinary::status::RetryClass::RetryAfterRelease
                        {
                            Ok(true)
                        } else {
                            termination
                                .public_error
                                .map_or(Ok(false), |code| Err(code.into()))
                        }
                    }
                    Err(_) => Err("module_vowifi_worker_failed".into()),
                };
            }
            // A failed/panicked network owner is quarantined. Its SessionCard
            // must not restore RF until native recovery proves release.
            tokio::select! {
                _ = stop.changed(), if !*stop.borrow() => {},
                changed = handle.changes(), if changes_open => { changes_open = changed.is_ok(); },
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
            }
        }
    }
    pub(crate) fn status(&self) -> Value {
        self.status_for(self.record())
    }
    /// Build a projection from the exact retained record being observed.
    /// An older receipt must never borrow phase/readiness from a successor.
    fn status_for(&self, record: Option<Arc<Record>>) -> Value {
        // Keep the service -> record lock order used by request admission.
        let idle_mode = self
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .idle_mode;
        let Some(record) = record else {
            return self.status_without_record(idle_mode);
        };
        let status = record.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut value = Self::status_for_locked(&record, &status, idle_mode);
        value["stop_target"] = json!(record.control.target("module", &self.0.epoch, record.id));
        value
    }
    fn status_without_record(&self, idle_mode: Option<u8>) -> Value {
        let mut value = Self::default_status();
        let mode = idle_mode.map_or(-1, i32::from);
        value["cfun_mode"] = json!(mode);
        value["airplane_mode"] = json!(mode == 4);
        if !self.0.radio.native_vowifi_route_available() {
            value["state"] = json!("unavailable");
            value["summary"] = json!("模块 Wi-Fi Calling 不可用");
            value["detail"] = json!("DJI 模块未连接");
            value["qmi_error"] = value["detail"].clone();
        }
        value
    }
    fn default_status() -> Value {
        json!({"state":"disabled","summary":"模块 Wi-Fi Calling 未启用",
            "airplane_mode":false,"cfun_mode":-1,"usbnet_mode":-1,"usbnet_ready":false,
            "ims_wifi_calling_enabled":false,"ims_call_mode_set":false,"wifi_calling_mode":0,
            "epdg_auto_detect":true,"ims_registered":false,"vowifi_active":false,"sms_ready":false,
            "call_ready":false,"progress_step":0,"qmi_enabled":false,"qmi_wifi_only":false,
            "qmi_wifi_first":false,"qmi_in_use":false,"qmi_wifi_registered":false})
    }
    /// The caller retains the record guard throughout status and operation
    /// projection. This helper cannot reacquire the service lock in reverse.
    fn status_for_locked(record: &Record, status: &RecordState, idle_mode: Option<u8>) -> Value {
        let mut value = Self::default_status();
        value["epdg_auto_detect"] = json!(record.connection.epdg.is_empty());
        let card_status = status.card.as_ref().map(|v| v.status());
        let mode = if status.retired {
            idle_mode.or_else(|| card_status.and_then(|v| v.cfun_mode))
        } else {
            card_status.and_then(|v| v.cfun_mode)
        }
        .map_or(-1, i32::from);
        value["cfun_mode"] = json!(mode);
        value["airplane_mode"] = json!(mode == 4);
        if let Some(card) = card_status {
            value["cleanup_confirmed"] = json!(card.cleanup_confirmed);
            value["restoration_skipped_reader_lost"] = json!(card.restoration_skipped_reader_lost);
        }
        let owned = record.claim.try_is_current().unwrap_or(true);
        if owned {
            value["state"] = json!("registering");
            value["summary"] = json!("正在通过 Mac 网络建立模块 VoWiFi");
            value["ims_wifi_calling_enabled"] = json!(true);
            value["wifi_calling_mode"] = json!(1);
            value["progress_step"] = json!(if status.handle.is_some() { 2 } else { 1 });
        }
        if let Some(card) = status.card.as_ref().filter(|c| c.is_current()) {
            value["sim_iccid"] = json!(card.identity().iccid.as_str());
        }
        if let Some(base) = &status.base {
            for (to, from) in [
                ("epdg_fqdn", "epdg_fqdn"),
                ("epdg_address", "epdg_fqdn"),
                ("operator_mcc", "mcc"),
                ("operator_mnc", "mnc"),
                ("operator_name", "profile_name"),
                ("transport", "transport"),
                ("proxy_source", "proxy_source"),
                ("ip_stack", "ip_stack"),
                ("ims_transport", "ims_transport"),
            ] {
                if let Some(v) = base.get(from) {
                    value[to] = v.clone();
                }
            }
        }
        if let Some(handle) = &status.handle {
            let snapshot = handle.snapshot();
            let online = owned
                && !status.stopping
                && card_status.is_some_and(|c| c.ready)
                && mode == 4
                && snapshot.registered
                && snapshot.phase == ordinary::Phase::Registered;
            value["ims_registered"] = json!(online);
            value["vowifi_active"] = json!(online && snapshot.tunnel_ready);
            value["sms_ready"] = json!(online && snapshot.sms_ready);
            value["call_ready"] = json!(online && snapshot.call_ready);
            value["ims_call_mode_set"] = json!(owned);
            if online {
                value["state"] = json!("active");
                value["summary"] = json!("模块 VoWiFi 已通过 Mac 网络完成 IMS 注册");
                value["progress_step"] = json!(5);
            }
        }
        let snapshot = status.handle.as_ref().map(|h| h.snapshot());
        let (step, description) =
            projection::progress(snapshot.as_ref(), status.stopping && !status.retired);
        value["progress_step"] = json!(if owned { step } else { 0 });
        if owned {
            value["progress_desc"] = json!(description);
        }
        if let Some(error) = &status.error
            && value["state"] != "active"
        {
            value["state"] = json!("failed");
            value["summary"] = json!("模块 VoWiFi 连接失败");
            value["detail"] = json!(error);
        }
        if let Some(diagnostic) = status.startup_failure {
            value["startup_failure"] = json!(diagnostic);
        }
        if let Some(error) = &status.cleanup_error {
            value["cleanup_error"] = json!(error);
        }
        value["attempt"] = json!(status.attempt);
        value["retrying"] = json!(status.retrying);
        value
    }
    fn operation_projection(&self) -> super::operation::Projection {
        self.operation_observation().0
    }
    fn operation_projection_for(
        &self,
        record: Option<Arc<Record>>,
    ) -> super::operation::Projection {
        self.operation_observation_for(record).0
    }
    fn operation_observation(&self) -> (super::operation::Projection, Value) {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        self.operation_observation_in_state(&state, state.record.as_ref())
    }
    fn operation_observation_for(
        &self,
        record: Option<Arc<Record>>,
    ) -> (super::operation::Projection, Value) {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        self.operation_observation_in_state(&state, record.as_ref())
    }
    fn operation_observation_in_state(
        &self,
        state: &State,
        record: Option<&Arc<Record>>,
    ) -> (super::operation::Projection, Value) {
        let now = tokio::time::Instant::now();
        let current = record.is_some_and(|record| {
            state
                .record
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, record))
        });
        let idle_mode = state.idle_mode;
        let Some(record) = record else {
            let mut clock = super::operation::Clock::start(now, Duration::ZERO);
            let _ = clock.stop(now, Duration::ZERO, false);
            let projection = clock.project(
                now,
                &self.0.epoch,
                0,
                super::operation::Facts {
                    generation: 0,
                    phase: "stopped".into(),
                    ready: false,
                    sms_ready: false,
                    call_ready: false,
                    cleanup_confirmed: true,
                    superseded: false,
                    error: None,
                },
            );
            return (projection, self.status_without_record(idle_mode));
        };
        let status = record.state.lock().unwrap_or_else(|e| e.into_inner());
        self.operation_observation_for_locked(record, &status, current, idle_mode, now)
    }
    fn operation_observation_for_locked(
        &self,
        record: &Record,
        status: &RecordState,
        current: bool,
        idle_mode: Option<u8>,
        now: tokio::time::Instant,
    ) -> (super::operation::Projection, Value) {
        // In particular, complete_cleanup cannot publish retired/clear errors
        // between this Value and the Clock's readiness/cleanup/error facts.
        let value = Self::status_for_locked(record, status, idle_mode);
        let fallback = super::operation::Clock::start(now, START_OPERATION_BUDGET);
        let clock = status.operation.as_ref().unwrap_or(&fallback);
        let projection = clock.project(
            now,
            &self.0.epoch,
            record.id,
            super::operation::Facts {
                generation: record.id,
                phase: if status.stopping && !status.retired {
                    "draining".into()
                } else {
                    value["state"].as_str().unwrap_or("unavailable").into()
                },
                ready: status.operation_done
                    && value["vowifi_active"] == true
                    && value["ims_registered"] == true,
                sms_ready: value["sms_ready"] == true,
                call_ready: value["call_ready"] == true,
                cleanup_confirmed: status.retired && !record.claim.try_is_current().unwrap_or(true),
                // A card is intentionally retired and replaced while the same
                // retained record retries. Only a successor record supersedes
                // this public operation identity.
                superseded: !current,
                error: status
                    .operation_done
                    .then(|| status.error.clone())
                    .flatten(),
            },
        );
        (projection, value)
    }
    fn operation(&self) -> Value {
        let Some(record) = self.record() else {
            return json!({"done":true});
        };
        let mut state = record.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.operation_done
            && state
                .operation_deadline
                .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        {
            state.operation_done = true;
            state.error = Some(
                "模块 Wi-Fi Calling: 隧道资源释放超时；无线状态正在后台恢复，请稍后刷新".into(),
            );
        }
        let mut value = json!({"done":state.operation_done});
        if !state.operation_done {
            return value;
        }
        if let Some(error) = &state.error {
            value["error"] = json!(error);
        }
        drop(state);
        value["status"] = self.status();
        value
    }
    pub(crate) async fn handle<B>(&self, request: hyper::Request<B>) -> hyper::Response<Full<Bytes>>
    where
        B: hyper::body::Body<Data = Bytes> + Send,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let path = request.uri().path().to_owned();
        if path == super::stop_control::MODULE_PATH {
            return self.handle_stop_control(&request);
        }
        if path == "/api/vowifi/reconcile" {
            return if request.method() == hyper::Method::POST {
                self.reconcile_unobserved(request).await
            } else {
                coded(StatusCode::NOT_FOUND, "route_not_found", "route_not_found")
            };
        }
        if path == "/api/vowifi/result" {
            let options = match super::operation::Options::query(request.uri().query()) {
                Ok(options) => options,
                Err(code) => return coded(StatusCode::BAD_REQUEST, code, code),
            };
            // Opted-in observers must not execute the legacy result endpoint's
            // observation-deadline mutation. The retained owner certifies exit.
            let mut value = if options.enabled() {
                json!({"done":false})
            } else {
                self.operation()
            };
            if options.enabled() {
                let (projection, status) = if let Some(request_id) = options.request_id() {
                    match self.lookup_request_with_observation(request_id, None) {
                        Ok((Some(receipt), current, status)) => {
                            let projection = receipt.resolve(Some(current.clone()));
                            // Historical receipts have their own Clock sequence
                            // and must not borrow a successor's live status.
                            let same_basis = projection.operation_id == current.operation_id
                                && projection.generation == current.generation
                                && projection.requested_enable == current.requested_enable;
                            (projection, same_basis.then_some(status))
                        }
                        Ok((None, _, _)) => {
                            return coded(
                                StatusCode::CONFLICT,
                                "vowifi_request_id_unknown",
                                "vowifi_request_id_unknown",
                            );
                        }
                        Err(code) => return coded(StatusCode::CONFLICT, code, code),
                    }
                } else {
                    let (current, status) = self.operation_observation();
                    if let Err(code) = options.validate(&current) {
                        return coded(StatusCode::CONFLICT, code, code);
                    }
                    (current, Some(status))
                };
                value["done"] = json!(projection.terminal);
                value["operation"] = json!(projection);
                if let Some(status) = status {
                    value["status"] = status;
                }
            }
            return response(StatusCode::OK, value);
        }
        if request.method() == hyper::Method::GET {
            let owned = self.owns_session();
            let card = self
                .record()
                .and_then(|record| record.state.lock().ok()?.card.clone());
            let observed = if owned {
                match card {
                    Some(card) => card.observe_radio().await.map(|_| ()).map_err(|_| ()),
                    None => Ok(()),
                }
            } else {
                match tokio::time::timeout(
                    Duration::from_secs(5),
                    self.0.radio.native_vowifi_idle_mode(),
                )
                .await
                {
                    Ok(Ok(mode)) => {
                        self.0
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .idle_mode = Some(mode);
                        Ok(())
                    }
                    _ => Err(()),
                }
            };
            // A concurrent start supersedes an idle observation; never publish
            // its stale CFUN result over the retained native preparation.
            if observed.is_err() && (owned || !self.owns_session()) {
                let mut value = self.status();
                value["state"] = json!("unavailable");
                value["summary"] = json!("模块 Wi-Fi Calling 状态暂不可用");
                for key in ["ims_registered", "vowifi_active", "sms_ready", "call_ready"] {
                    value[key] = json!(false);
                }
                return response(StatusCode::OK, value);
            }
            return response(StatusCode::OK, self.status());
        }
        // Only ingress has a deadline. Once decoded/admitted, the retained
        // module owner continues independently of this HTTP waiter's lifetime.
        let body = std::pin::pin!(request.into_body());
        let bytes = match crate::http_server::collect_body(body, 4096).await {
            Ok(bytes) => bytes,
            Err(crate::http_server::BodyReadError::Timeout) => {
                return crate::http_server::body_timeout_response();
            }
            Err(_) => {
                return coded(
                    StatusCode::BAD_REQUEST,
                    "invalid_vowifi_request",
                    "Wi-Fi Calling 请求格式无效",
                );
            }
        };
        let options = match super::operation::Options::body(&bytes) {
            Ok(options) => options,
            Err(code) => return coded(StatusCode::BAD_REQUEST, code, code),
        };
        let fingerprint = super::operation::request_fingerprint(&bytes);
        // Resolve an already admitted intent before interpreting mutable
        // business fields. Every later payload carrying the same request ID
        // either replays that exact operation or conflicts, even when the
        // altered payload would otherwise fail ordinary request decoding.
        if let Some(request_id) = options.request_id() {
            match self.lookup_request(request_id, Some(&fingerprint)) {
                Ok(Some(receipt)) => {
                    let projection = receipt.resolve(Some(self.operation_projection()));
                    return response(
                        StatusCode::ACCEPTED,
                        json!({"accepted":true,"cancelling":false,"operation":projection}),
                    );
                }
                Ok(None) => {}
                Err(code) => return coded(StatusCode::CONFLICT, code, code),
            }
        }
        let decoded = match request::decode(&bytes) {
            Ok(decoded) => decoded,
            Err(code) => {
                let failure = coded(
                    StatusCode::BAD_REQUEST,
                    code,
                    if code == "module_epdg_ip_unsupported" {
                        "模块 VoWiFi 不接受裸 IP ePDG；请填写运营商 ePDG 域名以保留代理 DNS 与证书校验"
                    } else {
                        code
                    },
                );
                // If even the requested boolean cannot be certified, retain
                // the caller fence instead of inventing a terminal identity.
                return match request::requested_enable(&bytes) {
                    Ok(enable) => {
                        self.preadmission_failure(&options, &fingerprint, enable, code, failure)
                    }
                    Err(_) => failure,
                };
            }
        };
        match self.request_with_observation(
            decoded.enable,
            decoded.connection,
            Some(&options),
            &fingerprint,
        ) {
            Ok((cancelling, record, receipt)) if options.enabled() => {
                let current = self.operation_projection();
                let projection = receipt.map_or_else(
                    || self.operation_projection_for(record),
                    |receipt| receipt.resolve(Some(current)),
                );
                response(
                    StatusCode::ACCEPTED,
                    json!({"accepted":true,"cancelling":cancelling,"stop_target":self.stop_target_for(&projection),"operation":projection}),
                )
            }
            Ok((true, _, _)) => response(
                StatusCode::ACCEPTED,
                json!({"accepted":true,"cancelling":true}),
            ),
            Ok((false, _, _)) => response(StatusCode::ACCEPTED, json!({"accepted":true})),
            Err(code) => self.preadmission_failure(
                &options,
                &fingerprint,
                decoded.enable,
                code,
                coded(
                    StatusCode::CONFLICT,
                    code,
                    "模块 Wi-Fi Calling 操作正在进行",
                ),
            ),
        }
    }
}
impl Service {
    pub(crate) async fn stop_for_profile_switch(&self) -> Result<(), &'static str> {
        let _business = self.0.business.lock().await;
        let Some(record) = self.record() else {
            return Ok(());
        };
        if !record.claim.try_is_current().unwrap_or(true) {
            return Ok(());
        }
        let handle = {
            let state = record.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopping {
                None
            } else {
                let handle = state.handle.as_ref().ok_or("module_vowifi_starting")?;
                if handle.snapshot().phase != ordinary::Phase::Registered {
                    return Err("module_vowifi_starting");
                }
                Some(handle.clone())
            }
        };
        if let Some(handle) = handle {
            let (reply, response) = tokio::sync::oneshot::channel();
            tokio::time::timeout(Duration::from_secs(5), async {
                handle
                    .controls
                    .send(
                        crate::native_registered_session::Control::PrepareProfileSwitch {
                            generation: handle.generation,
                            reply,
                        },
                    )
                    .await
                    .map_err(|_| "module_vowifi_cleanup_unconfirmed")?;
                response
                    .await
                    .map_err(|_| "module_vowifi_cleanup_unconfirmed")?
                    .map_err(|e| e.code)
            })
            .await
            .map_err(|_| "module_vowifi_cleanup_unconfirmed")??;
        }
        self.stop_before(tokio::time::Instant::now() + Duration::from_secs(30))
            .await?;
        if record
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .card
            .as_ref()
            .is_some_and(|card| card.status().restoration_skipped_reader_lost)
        {
            return Err("module_sim_identity_changed");
        }
        Ok(())
    }
    fn binding(&self, generation: u64) -> Result<api::ActiveBinding, api::ApiFailure> {
        let record = self
            .record()
            .ok_or_else(|| api::ApiFailure::new("模块 VoWiFi 尚未启动"))?;
        if *record.stop.borrow() || !matches!(record.claim.try_is_current(), Ok(true)) {
            return Err(api::ApiFailure::new("模块 VoWiFi 已停止或正在清理"));
        }
        let state = record.state.lock().unwrap_or_else(|e| e.into_inner());
        let card = state
            .card
            .as_ref()
            .filter(|v| v.is_current())
            .ok_or_else(|| api::ApiFailure::new("模块 SIM 身份已变化"))?;
        let handle = state
            .handle
            .as_ref()
            .ok_or_else(|| api::ApiFailure::new("模块 VoWiFi 尚未就绪"))?;
        let snapshot = handle.snapshot();
        if (generation != 0 && generation != card.generation())
            || snapshot.phase != ordinary::Phase::Registered
            || !snapshot.registered
            || snapshot.renewal_due
        {
            return Err(api::ApiFailure::new("模块 VoWiFi 会话已变化或尚未就绪"));
        }
        Ok(api::ActiveBinding {
            reader_generation: card.generation(),
            session_generation: handle.generation,
            source: "module".into(),
            controls: handle.controls.clone(),
        })
    }
    pub(crate) async fn validate_business(&self) -> Result<(), &'static str> {
        let before = self
            .binding(0)
            .map_err(|_| "native_module_sms_unavailable")?;
        let card = self
            .record()
            .and_then(|r| r.state.lock().ok()?.card.clone())
            .ok_or("native_module_sms_unavailable")?;
        card.validate_business()
            .await
            .map_err(|_| "module_sim_identity_changed")?;
        let after = self
            .binding(before.reader_generation)
            .map_err(|_| "module_sim_identity_changed")?;
        if after.session_generation != before.session_generation
            || !after.controls.same_channel(&before.controls)
        {
            return Err("module_sim_identity_changed");
        }
        Ok(())
    }
    pub(crate) fn sms_binding(&self) -> Result<NativeSmsBinding, &'static str> {
        let binding = self
            .binding(0)
            .map_err(|_| "native_module_sms_unavailable")?;
        let record = self.record().ok_or("native_module_sms_unavailable")?;
        let state = record
            .state
            .lock()
            .map_err(|_| "native_module_sms_unavailable")?;
        let card = state.card.as_ref().ok_or("native_module_sms_unavailable")?;
        let handle = state
            .handle
            .as_ref()
            .filter(|h| {
                h.generation == binding.session_generation
                    && h.controls.same_channel(&binding.controls)
            })
            .ok_or("native_module_sms_unavailable")?;
        if !handle.snapshot().sms_ready {
            return Err("native_module_sms_unavailable");
        }
        Ok(NativeSmsBinding {
            binding: format!(
                "{}:{}:{}",
                self.0.epoch,
                record.id,
                binding.session_generation.value()
            ),
            subscription_id: native_device_sms_subscription_id(
                &card.identity().iccid,
                &card.identity().imsi,
            )
            .ok_or("native_module_sms_unavailable")?,
            generation: binding.session_generation,
            controls: binding.controls,
        })
    }
    pub(crate) fn resolve_sms_binding(
        &self,
        token: &str,
    ) -> Result<NativeSmsBinding, &'static str> {
        let binding = self.sms_binding()?;
        if binding.binding != token {
            return Err("native_module_sms_binding_expired");
        }
        Ok(binding)
    }
    pub(crate) async fn calls<B>(&self, request: hyper::Request<B>) -> hyper::Response<Full<Bytes>>
    where
        B: hyper::body::Body<Data = Bytes> + Send,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let _business = self.0.business.lock().await;
        api::handle(request, self).await
    }
}
impl api::Service for Service {
    fn resolve(
        &self,
        generation: u64,
        mutation: bool,
    ) -> api::BoxFuture<'_, Result<api::ActiveBinding, api::ApiFailure>> {
        Box::pin(async move {
            if mutation {
                self.validate_business()
                    .await
                    .map_err(api::ApiFailure::new)?;
            }
            self.binding(generation)
        })
    }
    fn revalidate<'a>(
        &'a self,
        binding: &'a api::ActiveBinding,
    ) -> api::BoxFuture<'a, Result<(), api::ApiFailure>> {
        Box::pin(async move {
            let current = self.binding(binding.reader_generation)?;
            if current.session_generation != binding.session_generation
                || !current.controls.same_channel(&binding.controls)
            {
                return Err(api::ApiFailure::new("模块 VoWiFi 会话已变化"));
            }
            Ok(())
        })
    }
    fn history_error(&self) -> Option<String> {
        self.0.calls.history_error()
    }
}
fn response(status: StatusCode, value: Value) -> hyper::Response<Full<Bytes>> {
    hyper::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(value.to_string())))
        .unwrap()
}
fn coded(status: StatusCode, code: &str, message: &str) -> hyper::Response<Full<Bytes>> {
    response(
        status,
        json!({"code":code,"error":message,"recoverable":status == StatusCode::CONFLICT}),
    )
}
