//! Daemon-owned ordinary intent and its real reader/tunnel/IMS adapter.
//! UI waiters never own startup, the reader lease, or the cleanup task.
use super::{api, intent, ordinary, *};
use crate::reader_session_lease::NativeReaderSessionLease;
use intent::Driver as _;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::watch;

mod legacy_stop;
mod sessions;
mod sms;
mod status_projection;
mod stop_control;
pub(crate) use sms::NativeSmsBinding;

trait IdentitySource: Send + Sync {
    fn capture(&self) -> Result<intent::ExpectedIdentity, String>;
    fn snapshot(&self) -> Option<crate::reader_lifecycle::Snapshot> {
        None
    }
}
impl IdentitySource for crate::reader_lifecycle::Service {
    fn capture(&self) -> Result<intent::ExpectedIdentity, String> {
        identity_from_snapshot(self.snapshot())
    }
    fn snapshot(&self) -> Option<crate::reader_lifecycle::Snapshot> {
        Some(crate::reader_lifecycle::Service::snapshot(self))
    }
}

#[derive(Clone)]
enum StartFailure {
    Busy,
    NoSim,
    InvalidRequest(String),
    Internal(String),
}
impl From<String> for StartFailure {
    fn from(message: String) -> Self {
        Self::Internal(message)
    }
}
impl From<&str> for StartFailure {
    fn from(message: &str) -> Self {
        Self::Internal(message.into())
    }
}
impl std::fmt::Display for StartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Busy => "读卡器 VoWiFi 操作正在进行，请稍后重试",
            Self::NoSim => "读卡器未连接或无 SIM 卡",
            Self::InvalidRequest(message) => message,
            Self::Internal(message) => message,
        })
    }
}
impl StartFailure {
    fn response(self) -> Response<Full<Bytes>> {
        let (status, message) = match self {
            Self::Busy => (StatusCode::CONFLICT, Self::Busy.to_string()),
            Self::NoSim => (StatusCode::SERVICE_UNAVAILABLE, Self::NoSim.to_string()),
            Self::InvalidRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Internal(message) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("启动 VoWiFi 失败: {message}"),
            ),
        };
        crate::json_response(status, &json!({"error":message}))
    }
}
fn identity_from_snapshot(
    snapshot: crate::reader_lifecycle::Snapshot,
) -> Result<intent::ExpectedIdentity, String> {
    let selection = snapshot.selection;
    if selection.pending || selection.reader.is_empty() || selection.generation == 0 {
        return Err("reader_identity_unavailable".into());
    }
    let info = snapshot.usim_info.ok_or("reader_identity_unavailable")?;
    let value = |key| {
        info.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let identity = intent::ExpectedIdentity {
        reader: selection.reader,
        reader_generation: selection.generation,
        iccid: value("iccid"),
        imsi: value("imsi"),
    };
    if identity.iccid.is_empty() || identity.imsi.is_empty() {
        return Err("reader_identity_incomplete".into());
    }
    Ok(identity)
}

#[derive(Clone)]
struct Resources {
    legacy_module_claim: Arc<dyn Fn() -> bool + Send + Sync>,
    ordinary_admission: Arc<crate::ordinary_vowifi_admission::Gate>,
    identity: Arc<dyn IdentitySource>,
    lifecycle: Weak<NativeLifecycle>,
    gate: Arc<crate::reader_reservation::Gate>,
    pcsc: crate::reader_pcsc::Owner,
    socket: PathBuf,
    token: Option<Zeroizing<String>>,
    data_root: PathBuf,
    transactions: crate::ims_sms_transactions::Service,
    calls: crate::call_facts::CallFacts,
}
struct Record {
    control: Arc<super::stop_control::Owner>,
    id: u64,
    expected: intent::ExpectedIdentity,
    driver: Arc<LiveDriver>,
    stop: watch::Sender<bool>,
    progress: watch::Receiver<intent::Snapshot>,
    task: tokio::sync::Mutex<intent::Handle>,
    claim: Arc<crate::ordinary_vowifi_admission::Claim>,
    observer: Mutex<Option<tokio::task::JoinHandle<()>>>,
    operation: Mutex<super::operation::Clock>,
}
impl Record {
    fn observe_terminal(self: &Arc<Self>) {
        let record = self.clone();
        let observer = tokio::spawn(async move {
            let mut progress = record.progress.clone();
            loop {
                let terminal = matches!(
                    progress.borrow().phase,
                    intent::Phase::Stopped | intent::Phase::Failed | intent::Phase::ReaderChanged
                );
                if terminal || progress.changed().await.is_err() {
                    break;
                }
            }
            loop {
                // A terminal/closed watch is only a reason to observe. Never
                // hold the task lock while its real worker is still running.
                let joined = if let Ok(mut task) = record.task.try_lock() {
                    if task.is_finished() {
                        Some(task.finish_cancel_safe().await)
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(joined) = joined {
                    if joined.is_ok_and(|exit| exit.snapshot.cleanup_confirmed) {
                        let _ = record.claim.release_confirmed();
                    }
                    // Panic or unconfirmed cleanup leaves the exact claim
                    // quarantined. Claim Drop deliberately cannot release it.
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        *self
            .observer
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(observer);
    }
    fn request_stop(&self) {
        self.stop.send_replace(true);
    }
    async fn finish_before(&self, deadline: tokio::time::Instant) -> Result<(), &'static str> {
        let mut task = tokio::time::timeout_at(deadline, self.task.lock())
            .await
            .map_err(|_| "native_intent_shutdown_unconfirmed")?;
        let exit = tokio::time::timeout_at(deadline, task.finish_cancel_safe())
            .await
            .map_err(|_| "native_intent_shutdown_unconfirmed")?
            .map_err(|_| "native_intent_task_failed")?;
        if !exit.snapshot.cleanup_confirmed {
            return Err("native_intent_cleanup_unconfirmed");
        }
        self.claim
            .release_confirmed()
            .map_err(|_| "native_intent_claim_release_unconfirmed")?;
        Ok(())
    }
}
#[derive(Default)]
struct State {
    last_stop: super::stop_control::Retired,
    stopping: bool,
    next: u64,
    record: Option<Arc<Record>>,
    requests: super::operation::RequestLedger,
    #[cfg(test)]
    before_stop_insert: Option<Box<dyn FnOnce(&super::operation::RequestLedger) + Send>>,
    #[cfg(test)]
    before_stop_submitted: Option<Box<dyn FnOnce(&super::operation::RequestLedger) + Send>>,
}
struct Inner {
    sms_epoch: String,
    resources: Resources,
    state: Mutex<State>,
    mutation: tokio::sync::Mutex<()>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        // The observer retains Record so it can harvest real cleanup. Losing
        // the last service owner must still stop retry/start work; it cannot
        // release the shared claim or abort an in-flight physical worker.
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(record) = &state.record {
            record.request_stop();
        }
    }
}
#[derive(Clone)]
pub(crate) struct Service(Arc<Inner>);
impl NativeLifecycle {
    pub(crate) fn service(self: &Arc<Self>, state: &crate::ServerState) -> Result<Service, String> {
        // Shutdown revokes new admission, but the retained owner still serves
        // observations and exact stop/control errors until its actual join.
        if let Some(service) = self.ordinary_service.get() {
            return service.clone();
        }
        let lifecycle = self
            .state
            .lock()
            .map_err(|_| "native_lifecycle_unavailable")?;
        if lifecycle.stopping
            || lifecycle.finishing
            || lifecycle.owner.is_none()
            || lifecycle.cleanup_failure.is_some()
        {
            return Err("native_lifecycle_not_accepting".into());
        }
        self.ordinary_service
            .get_or_init(|| {
                let reader = state
                    .reader_lifecycle()
                    .map_err(|_| "reader_lifecycle_unavailable".to_owned())?;
                Ok(Service(Arc::new(Inner {
                    sms_epoch: sms::new_epoch()?,
                    resources: Resources {
                        ordinary_admission: self.ordinary_admission(),
                        legacy_module_claim: {
                            let radio = state.module_radio.clone();
                            Arc::new(move || radio.vowifi_owns_sim_channel())
                        },
                        identity: Arc::new(reader),
                        lifecycle: Arc::downgrade(self),
                        gate: state.reader_gate.clone(),
                        pcsc: state.reader_pcsc.clone(),
                        socket: state.go_socket_path.clone(),
                        token: state.app_token.clone().map(Zeroizing::new),
                        data_root: state.multistandby_store.data_root().to_path_buf(),
                        transactions: state.ims_sms_transactions.clone(),
                        calls: state.call_facts.clone(),
                    },
                    state: Mutex::new(State {
                        requests: super::operation::RequestLedger::open(
                            state.multistandby_store.data_root(),
                            "reader",
                        )?,
                        ..State::default()
                    }),
                    mutation: tokio::sync::Mutex::new(()),
                })))
            })
            .clone()
    }
    pub(crate) fn ordinary_service(&self) -> Option<Service> {
        self.ordinary_service
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
        let current = self
            .operation_projection_in_state(&state, Some(record))
            .ok()?;
        (current.operation_id == projection.operation_id
            && current.generation == projection.generation)
            .then(|| {
                record.control.target(
                    "reader",
                    &self.0.sms_epoch,
                    record.expected.reader_generation,
                )
            })
    }
    fn record(&self) -> Result<Option<Arc<Record>>, &'static str> {
        self.0
            .state
            .lock()
            .map(|state| state.record.clone())
            .map_err(|_| "native_intent_state_unavailable")
    }

    fn lookup_request(
        &self,
        request_id: &str,
        fingerprint: Option<&str>,
    ) -> Result<Option<super::operation::Receipt>, &'static str> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| "native_intent_state_unavailable")?;
        self.lookup_request_locked(&mut state, request_id, fingerprint)
    }

    fn lookup_request_locked(
        &self,
        state: &mut State,
        request_id: &str,
        fingerprint: Option<&str>,
    ) -> Result<Option<super::operation::Receipt>, &'static str> {
        // Check fingerprint first: altered payload never reaches any owner.
        state.requests.lookup(request_id, fingerprint)?;
        // Keep admission and receipt publication fenced until this exact
        // current-record/Clock sample has been applied to the ledger.
        let current = self
            .operation_projection_in_state(state, state.record.as_ref())
            .ok();
        state.requests.observe(current)?;
        state.requests.lookup(request_id, fingerprint)
    }

    fn remember_request(
        &self,
        options: &super::operation::Options,
        fingerprint: &str,
        projection: &super::operation::Projection,
    ) -> Result<Option<super::operation::Receipt>, &'static str> {
        let Some(request_id) = options.request_id() else {
            return Ok(None);
        };
        let receipt = super::operation::Receipt::capture(request_id, fingerprint, projection);
        self.0
            .state
            .lock()
            .map_err(|_| "native_intent_state_unavailable")?
            .requests
            .insert(receipt.clone())?;
        Ok(Some(receipt))
    }

    fn reject_unadmitted(
        &self,
        options: &super::operation::Options,
        fingerprint: &str,
        requested_enable: bool,
        message: &str,
        response: Response<Full<Bytes>>,
    ) -> Response<Full<Bytes>> {
        let Some(request_id) = options.request_id() else {
            return response;
        };
        // The mutation lock serializes this check with admission. A start may
        // have already recorded its ID before failing later; in that case its
        // admitted/unknown receipt remains the only authority.
        let recorded = self
            .0
            .state
            .lock()
            .map_err(|_| "native_intent_state_unavailable")
            .and_then(
                |mut state| match state.requests.lookup(request_id, Some(fingerprint))? {
                    Some(_) => Ok(()),
                    None => state.requests.insert(super::operation::Receipt::rejected(
                        request_id,
                        fingerprint,
                        requested_enable,
                        message,
                    )),
                },
            );
        match recorded {
            Ok(()) => response,
            Err(code) => {
                crate::json_response(StatusCode::CONFLICT, &json!({"error":code,"code":code}))
            }
        }
    }
    pub(crate) fn has_intent(&self) -> bool {
        self.record().map_or(true, |record| {
            record.is_some_and(|record| record.claim.is_current())
        })
    }
    /// A released native record is retained for diagnostics. It cannot hide a
    /// subsequent Go owner which acquired the same global admission slot.
    pub(crate) fn retains_route_owner(&self) -> bool {
        self.reader_observation().is_some()
    }
    pub(crate) fn reader_observation(&self) -> Option<super::ReaderObservation> {
        let record = match self.record() {
            Ok(None) => return None,
            Err(_) => return Some(super::ReaderObservation::unavailable()),
            Ok(Some(record)) => record,
        };
        let mut status = self.status();
        let binding = self.sms_binding().ok();
        let identity_valid =
            self.0.resources.identity.capture().ok().as_ref() == Some(&record.expected);
        // Both capture and binding resolution can race with publication. Never
        // combine an old record/status with a new record's control capability.
        if !self
            .record()
            .ok()
            .flatten()
            .is_some_and(|current| Arc::ptr_eq(&current, &record))
        {
            return Some(super::ReaderObservation::unavailable());
        }
        let session_active = match record.claim.try_is_current() {
            Ok(current) => current,
            Err(_) => return Some(super::ReaderObservation::unavailable()),
        };
        if !session_active {
            match self.0.resources.ordinary_admission.current_go_claim() {
                Ok(Some(_)) => return None,
                Err(_) => return Some(super::ReaderObservation::unavailable()),
                Ok(None) => {}
            }
        }
        let sms_ready = session_active && identity_valid && binding.is_some();
        if !identity_valid {
            status = super::ReaderObservation::unavailable().status;
            status["message"] = json!("读卡器或号卡已变化，请重新连接 VoWiFi");
        }
        status["sms_ready"] = json!(sms_ready);
        status["sms_state"] = json!(if sms_ready { "ready" } else { "unavailable" });
        Some(super::ReaderObservation {
            status,
            reader: record.expected.reader.clone(),
            reader_generation: record.expected.reader_generation,
            sim_id: if record.expected.iccid.is_empty() {
                record.expected.imsi.clone()
            } else {
                record.expected.iccid.clone()
            },
            session_active,
            identity_valid,
            sms_ready,
            subscription_id: if session_active && identity_valid {
                native_device_sms_subscription_id(&record.expected.iccid, &record.expected.imsi)
            } else {
                None
            },
        })
    }
    /// Startup publishes its retained task before returning. No card/network
    /// future is executed in the HTTP caller that can be cancelled independently.
    pub(crate) async fn start(&self, request: intent::FrozenRequest) -> Result<u64, String> {
        self.start_checked(request)
            .await
            .map_err(|error| error.to_string())
    }
    async fn start_checked(&self, request: intent::FrozenRequest) -> Result<u64, StartFailure> {
        self.start_checked_observed(request, None).await
    }
    async fn start_checked_observed(
        &self,
        request: intent::FrozenRequest,
        observed: Option<(&super::operation::Options, &str)>,
    ) -> Result<u64, StartFailure> {
        if (self.0.resources.legacy_module_claim)() {
            return Err("已有模块 VoWiFi 会话正在运行或释放，请先关闭后再切换设备".into());
        }
        let expected = self.capture_start_identity()?;
        let previous = self.record().map_err(str::to_owned)?;
        if let Some(previous) = &previous {
            if !previous
                .task
                .try_lock()
                .is_ok_and(|task| task.is_finished())
            {
                return Err("VoWiFi 会话已在运行或正在释放资源".into());
            }
            previous
                .finish_before(tokio::time::Instant::now() + Duration::from_secs(1))
                .await
                .map_err(str::to_owned)?;
        }
        let lifecycle = self
            .0
            .resources
            .lifecycle
            .upgrade()
            .ok_or("native_lifecycle_unavailable")?;
        // Global admission precedes the service lock everywhere, including shutdown.
        let admission = lifecycle
            .state
            .lock()
            .map_err(|_| "native_lifecycle_unavailable")?;
        if admission.stopping
            || admission.finishing
            || admission.cleanup_failure.is_some()
            || admission.owner.is_none()
        {
            return Err("native_lifecycle_not_accepting".into());
        }
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| "native_intent_state_unavailable")?;
        if state.stopping {
            return Err("Switch 正在退出，未启动 VoWiFi".into());
        }
        if state.record.as_ref().is_some_and(|r| r.control.busy()) {
            return Err(StartFailure::Busy);
        }
        let same_record = match (&state.record, &previous) {
            (None, None) => true,
            (Some(current), Some(previous)) => Arc::ptr_eq(current, previous),
            _ => false,
        };
        if !same_record {
            return Err("VoWiFi 会话已变化，请重试".into());
        }
        if self.capture_start_identity()? != expected {
            return Err("reader_identity_changed".into());
        }
        let id = state
            .next
            .checked_add(1)
            .ok_or("native_intent_generation_exhausted")?;
        // Persist the final owner identity before acquiring/dispatching work.
        // On a crash this exact ID is retained as unknown and cannot respawn.
        let clock =
            super::operation::Clock::start(tokio::time::Instant::now(), Duration::from_secs(90));
        if let Some((options, fingerprint)) = observed
            && let Some(request_id) = options.request_id()
        {
            state
                .requests
                .insert(clock.receipt(
                    request_id,
                    fingerprint,
                    &self.0.sms_epoch,
                    id,
                    expected.reader_generation,
                ))
                .map_err(str::to_owned)?;
        }
        let claim = self
            .0
            .resources
            .ordinary_admission
            .acquire_native_reader(
                &self.0.sms_epoch,
                id,
                &expected.reader,
                expected.reader_generation,
            )
            .map_err(|error| {
                if error.status == StatusCode::CONFLICT {
                    StartFailure::Busy
                } else {
                    StartFailure::Internal(error.to_string())
                }
            })?;
        // This sequence is consumed once admission exists, even if validation
        // rejects spawn before dispatch. A released claim cannot be resurrected.
        state.next = id;
        let driver = Arc::new(LiveDriver {
            ordinary_claim: Some(claim.clone()),
            resources: self.0.resources.clone(),
            current: Mutex::new(AttemptState::default()),
        });
        if let Some((options, _)) = observed
            && let Some(request_id) = options.request_id()
            && let Err(error) = state.requests.mark_submitted(request_id)
        {
            let _ = claim.release_confirmed();
            return Err(error.into());
        }
        let task = match intent::spawn(
            driver.clone(),
            id,
            expected.clone(),
            request,
            intent::Policy::default(),
        ) {
            Ok(task) => task,
            Err(error) => {
                // spawn rejected before dispatch; no worker owns this claim.
                claim
                    .release_confirmed()
                    .map_err(|error| error.to_string())?;
                return Err(error.into());
            }
        };
        let record = Arc::new(Record {
            control: Default::default(),
            id,
            expected,
            driver,
            stop: task.stop_sender(),
            progress: task.subscribe(),
            task: tokio::sync::Mutex::new(task),
            claim,
            observer: Mutex::new(None),
            operation: Mutex::new(clock),
        });
        record.observe_terminal();
        if let Some(previous) = &state.record {
            let owner = previous.control.clone();
            let target = owner.target(
                "reader",
                &self.0.sms_epoch,
                previous.expected.reader_generation,
            );
            super::stop_control::retire(&mut state.last_stop, &owner, target);
        }
        state.record = Some(record);
        Ok(id)
    }
    pub(crate) fn begin_shutdown(&self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.stopping = true;
        if let Some(record) = &state.record {
            record.request_stop();
        }
    }
    pub(crate) async fn finish_before(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<(), &'static str> {
        if let Some(record) = self.record()? {
            record.finish_before(deadline).await?;
        }
        Ok(())
    }
    pub(crate) async fn stop(
        &self,
        reader: Option<(&str, u64)>,
        deadline: tokio::time::Instant,
    ) -> Result<(), &'static str> {
        let Some(record) = self.record()? else {
            return Ok(());
        };
        if reader.is_some_and(|(reader, generation)| {
            reader != record.expected.reader || generation != record.expected.reader_generation
        }) {
            return Ok(());
        }
        record.request_stop();
        record.finish_before(deadline).await
    }
    pub(crate) async fn stop_for_maintenance(
        &self,
        maintenance: &Arc<crate::ordinary_vowifi_admission::Maintenance>,
        deadline: tokio::time::Instant,
    ) -> Result<(), String> {
        if !maintenance.is_current() {
            return Err("reader_maintenance_stale".into());
        }
        let Some(claim) = maintenance.native_claim() else {
            return Ok(());
        };
        if !claim.is_current() {
            return Ok(());
        }
        let record = self
            .record()
            .map_err(str::to_owned)?
            .ok_or("native_intent_cleanup_unconfirmed")?;
        if !Arc::ptr_eq(&record.claim, &claim)
            || record.expected.reader != maintenance.reader()
            || record.expected.reader_generation != maintenance.reader_generation()
        {
            return Err("reader_maintenance_binding_changed".into());
        }
        record.request_stop();
        record
            .finish_before(deadline)
            .await
            .map_err(str::to_owned)?;
        if claim.is_current() {
            return Err("native_intent_cleanup_unconfirmed".into());
        }
        Ok(())
    }
    pub(crate) fn busy(&self, reader: &str, generation: u64) -> bool {
        self.record().map_or(true, |record| {
            record.is_some_and(|record| {
                record.claim.is_current()
                    && record.expected.reader == reader
                    && record.expected.reader_generation == generation
            })
        })
    }
    fn current_binding(&self, generation: u64) -> Result<api::ActiveBinding, api::ApiFailure> {
        if (self.0.resources.legacy_module_claim)() {
            return Err(api::ApiFailure::new(
                "VoWiFi 通话会话归属冲突，请等待资源释放",
            ));
        }
        let record = self
            .record()
            .map_err(api::ApiFailure::new)?
            .ok_or_else(|| api::ApiFailure::new("读卡器 VoWiFi 尚未连接"))?;
        if !matches!(record.claim.try_is_current(), Ok(true))
            || *record.stop.borrow()
            || (generation != 0 && generation != record.expected.reader_generation)
            || self.0.resources.identity.capture().ok().as_ref() != Some(&record.expected)
        {
            return Err(api::ApiFailure::new(
                "读卡器或号卡已变化，请重新连接 VoWiFi",
            ));
        }
        let handle = record
            .driver
            .handle()
            .ok_or_else(|| api::ApiFailure::new("IMS 尚未注册"))?;
        if record.progress.borrow().phase != intent::Phase::Registered
            || record.progress.borrow().generation != Some(handle.generation)
            || handle.snapshot().phase != ordinary::Phase::Registered
        {
            return Err(api::ApiFailure::new("IMS 尚未注册或正在恢复"));
        }
        Ok(api::ActiveBinding {
            reader_generation: record.expected.reader_generation,
            session_generation: handle.generation,
            source: "reader".into(),
            controls: handle.controls,
        })
    }
    pub(crate) async fn handle_control<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
    where
        B: hyper::body::Body<Data = Bytes> + Send,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        match api::handle_call_if_owned(
            request,
            self,
            self.retains_route_owner(),
            (self.0.resources.legacy_module_claim)(),
        )
        .await
        {
            Ok(response) => response,
            Err(request) => api::handle(request, self).await,
        }
    }
}
impl api::Service for Service {
    fn resolve(
        &self,
        generation: u64,
        _: bool,
    ) -> api::BoxFuture<'_, Result<api::ActiveBinding, api::ApiFailure>> {
        Box::pin(async move { self.current_binding(generation) })
    }
    fn revalidate<'a>(
        &'a self,
        binding: &'a api::ActiveBinding,
    ) -> api::BoxFuture<'a, Result<(), api::ApiFailure>> {
        Box::pin(async move {
            let current = self.current_binding(binding.reader_generation)?;
            if current.session_generation != binding.session_generation
                || !current.controls.same_channel(&binding.controls)
            {
                return Err(api::ApiFailure::new(
                    "VoWiFi 会话已变化，原结果不再属于当前号卡",
                ));
            }
            Ok(())
        })
    }
    fn history_error(&self) -> Option<String> {
        self.0.resources.calls.history_error()
    }
}

#[derive(Default)]
struct AttemptState {
    base_status: Option<serde_json::Value>,
    startup_failure: Option<StartFailure>,
    key: Option<(u64, u32)>,
    handle: Option<ordinary::Handle>,
    unlaunched_lease: Option<SessionCard>,
    result: Option<ordinary::SessionResult>,
    acquire_unconfirmed: bool,
    acquisition_cleanup: Option<crate::reader_session_lease::AcquisitionCleanup>,
}
/// The driver owns cancellation until the original worker has retired. UI
/// handles deliberately do not have this responsibility.
pub(super) struct DriverStopGuard(pub(super) ordinary::Handle);
impl Drop for DriverStopGuard {
    fn drop(&mut self) {
        if !released(&self.0.snapshot()) {
            self.0.request_shutdown();
        }
    }
}
struct LiveDriver {
    ordinary_claim: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
    resources: Resources,
    current: Mutex<AttemptState>,
}
impl LiveDriver {
    fn handle(&self) -> Option<ordinary::Handle> {
        self.current.lock().ok()?.handle.clone()
    }
    fn collect_cleanup_result(
        &self,
        attempt: &intent::Attempt,
        handle: &ordinary::Handle,
        take_result: impl FnOnce() -> Result<ordinary::SessionResult, String>,
    ) -> Result<(), String> {
        let matches = |current: &AttemptState| {
            current.key == Some((attempt.intent_id, attempt.number))
                && current
                    .handle
                    .as_ref()
                    .is_some_and(|captured| captured.generation == handle.generation)
        };
        {
            let current = self
                .current
                .lock()
                .map_err(|_| "native_attempt_state_unavailable")?;
            if !matches(&current) {
                return Err("native_attempt_cleanup_identity_mismatch".into());
            }
            if current.result.is_some() {
                return Ok(());
            }
        }
        // Never hold driver.current while entering NativeLifecycle. Shutdown
        // holds lifecycle.state before fencing the service's admission State;
        // observations hold that State before sampling driver.current.
        let result = take_result();
        let mut current = self
            .current
            .lock()
            .map_err(|_| "native_attempt_state_unavailable")?;
        if !matches(&current) {
            // A different attempt cannot acknowledge this captured cleanup,
            // even when the old ordinary worker returned a joined result.
            return Err("native_attempt_cleanup_identity_mismatch".into());
        }
        if current.result.is_none() {
            current.result = Some(result?);
        }
        Ok(())
    }
    async fn release_unlaunched(
        &self,
        lease: impl Into<SessionCard>,
        reason: intent::EndReason,
    ) -> intent::AttemptExit {
        let lease = lease.into();
        self.current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .unlaunched_lease = Some(lease.clone());
        let cleanup_confirmed = lease.release_with_confirmation().await.is_ok();
        intent::AttemptExit {
            reason,
            cleanup_confirmed,
        }
    }
    async fn run_attempt(
        &self,
        attempt: intent::Attempt,
        mut stop: watch::Receiver<bool>,
        progress: watch::Sender<intent::AttemptProgress>,
    ) -> intent::AttemptExit {
        let failed = |code: &str, cleanup_confirmed| intent::AttemptExit {
            reason: intent::EndReason::StartupFailed { error: code.into() },
            cleanup_confirmed,
        };
        if self.check_identity(&attempt.expected).is_err() {
            return intent::AttemptExit {
                reason: intent::EndReason::ReaderChanged,
                cleanup_confirmed: true,
            };
        }
        if *stop.borrow() || stop.has_changed().is_err() {
            return intent::AttemptExit {
                reason: intent::EndReason::Stopped,
                cleanup_confirmed: true,
            };
        }
        let acquisition_cleanup = self.ordinary_claim.as_ref().map(|_| {
            crate::reader_session_lease::AcquisitionCleanup::new(self.resources.gate.clone())
        });
        {
            let mut current = self
                .current
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            *current = AttemptState {
                key: Some((attempt.intent_id, attempt.number)),
                acquire_unconfirmed: true,
                acquisition_cleanup: acquisition_cleanup.clone(),
                ..Default::default()
            };
        }
        let acquisition = if let Some(claim) = &self.ordinary_claim {
            NativeReaderSessionLease::acquire_for_ordinary(
                &self.resources.socket,
                self.resources.token.as_ref().map(|token| token.as_str()),
                &attempt.expected.reader,
                self.resources.gate.clone(),
                self.resources.pcsc.clone(),
                claim.clone(),
                acquisition_cleanup
                    .clone()
                    .expect("ordinary acquisition receipt"),
            )
            .await
        } else {
            NativeReaderSessionLease::acquire(
                &self.resources.socket,
                self.resources.token.as_ref().map(|token| token.as_str()),
                &attempt.expected.reader,
                self.resources.gate.clone(),
                self.resources.pcsc.clone(),
            )
            .await
        };
        let lease = match acquisition {
            Ok(lease) => lease,
            Err(error) => {
                let pending = acquisition_cleanup.as_ref().map_or_else(
                    || self.resources.gate.native_reader_session_active(),
                    |receipt| !receipt.confirmed(),
                );
                let mut current = self
                    .current
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                current.acquire_unconfirmed = pending;
                current.startup_failure = Some(match error {
                    proletarius_domain::BackendFailure::ReaderAdmissionRejected { .. } => {
                        StartFailure::Busy
                    }
                    _ => StartFailure::Internal("reader_lease_acquisition_failed".into()),
                });
                return failed("reader_lease_acquisition_failed", !pending);
            }
        };
        let identity_matches = lease.reader == attempt.expected.reader
            && lease.reader_generation == attempt.expected.reader_generation
            && lease.identity.iccid.as_str() == attempt.expected.iccid
            && lease.identity.imsi.as_str() == attempt.expected.imsi;
        if !identity_matches || self.check_identity(&attempt.expected).is_err() {
            return self
                .release_unlaunched(Arc::new(lease), intent::EndReason::ReaderChanged)
                .await;
        }
        if *stop.borrow() || stop.has_changed().is_err() {
            return self
                .release_unlaunched(Arc::new(lease), intent::EndReason::Stopped)
                .await;
        }
        let request = PlanRequest {
            reader: attempt.expected.reader.clone(),
            proxy: attempt.request.proxy.clone(),
            epdg: attempt.request.epdg.clone(),
            epdg_port: (attempt.request.epdg_port != 0).then_some(attempt.request.epdg_port),
        };
        let context = match prepare(request, lease, stop.clone()).await {
            Ok(context) => context,
            Err((error, lease)) => {
                return self
                    .release_unlaunched(
                        lease,
                        intent::EndReason::StartupFailed {
                            error: error.code().into(),
                        },
                    )
                    .await;
            }
        };
        if self.check_identity(&attempt.expected).is_err() {
            return self
                .release_unlaunched(context.card, intent::EndReason::ReaderChanged)
                .await;
        }
        if *stop.borrow() || stop.has_changed().is_err() {
            return self
                .release_unlaunched(context.card, intent::EndReason::Stopped)
                .await;
        }
        let Some(lifecycle) = self.resources.lifecycle.upgrade() else {
            return self
                .release_unlaunched(context.card, intent::EndReason::Stopped)
                .await;
        };
        self.current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .base_status = Some(configuration::live_status(&context));
        let settings = match startup_settings() {
            Ok(settings) => settings,
            Err(code) => {
                return self
                    .release_unlaunched(
                        context.card,
                        intent::EndReason::StartupFailed { error: code.into() },
                    )
                    .await;
            }
        };
        let mut handle = match lifecycle.launch_ordinary(
            context,
            ordinary::Inputs {
                sms_archive: Default::default(),
                settings,
                reader_gate: self.resources.gate.clone(),
                data_root: self.resources.data_root.clone(),
                transactions: self.resources.transactions.clone(),
                call_facts: self.resources.calls.clone(),
            },
        ) {
            Ok(handle) => handle,
            Err((_, context)) => {
                return self
                    .release_unlaunched(
                        context.card,
                        intent::EndReason::StartupFailed {
                            error: "native_session_admission_failed".into(),
                        },
                    )
                    .await;
            }
        };
        let _stop_on_driver_exit = DriverStopGuard(handle.clone());
        self.current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .handle = Some(handle.clone());
        let mut stopped = false;
        let mut observer_open = true;
        loop {
            if *stop.borrow() || stop.has_changed().is_err() {
                stopped = true;
                handle.request_stop();
            }
            let snapshot = handle.snapshot();
            if snapshot.phase == ordinary::Phase::Registered {
                progress.send_replace(intent::AttemptProgress::Registered(handle.generation));
            } else if matches!(
                snapshot.phase,
                ordinary::Phase::Draining
                    | ordinary::Phase::CleanupUnconfirmed
                    | ordinary::Phase::Released
            ) {
                progress.send_replace(intent::AttemptProgress::Draining);
            }
            if let Some(result) = lifecycle.take_ordinary_result(handle.generation) {
                match result {
                    Ok(result) => {
                        let reason = if stopped {
                            intent::EndReason::Stopped
                        } else {
                            end_reason(&result.termination, attempt.number)
                        };
                        self.current
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .result = Some(result);
                        return intent::AttemptExit {
                            reason,
                            cleanup_confirmed: released(&handle.snapshot()),
                        };
                    }
                    Err(_) => return failed("native_ordinary_task_failed", false),
                }
            }
            tokio::select! {
                _ = stop.changed(), if !stopped => {},
                change = handle.changes(), if observer_open => { observer_open = change.is_ok(); },
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
            }
        }
    }
}
impl intent::Driver for LiveDriver {
    fn check_identity(&self, expected: &intent::ExpectedIdentity) -> Result<(), String> {
        if self.resources.identity.capture()? == *expected {
            Ok(())
        } else {
            Err("reader_identity_changed".into())
        }
    }
    fn run(
        self: Arc<Self>,
        attempt: intent::Attempt,
        stop: watch::Receiver<bool>,
        progress: watch::Sender<intent::AttemptProgress>,
    ) -> intent::DriverFuture<intent::AttemptExit> {
        Box::pin(async move { self.run_attempt(attempt, stop, progress).await })
    }
    fn await_cleanup(
        self: Arc<Self>,
        attempt: intent::Attempt,
    ) -> intent::DriverFuture<Result<(), String>> {
        Box::pin(async move {
            let (lease, handle, acquisition, acquisition_cleanup) = {
                let current = self
                    .current
                    .lock()
                    .map_err(|_| "native_attempt_state_unavailable")?;
                if current.key != Some((attempt.intent_id, attempt.number)) {
                    return Err("native_attempt_cleanup_identity_mismatch".into());
                }
                (
                    current.unlaunched_lease.clone(),
                    current.handle.clone(),
                    current.acquire_unconfirmed,
                    current.acquisition_cleanup.clone(),
                )
            };
            if let Some(lease) = lease {
                return lease
                    .release_with_confirmation()
                    .await
                    .map_err(|_| "native_lease_release_unconfirmed".into());
            }
            if let Some(handle) = handle {
                if !released(&handle.snapshot()) {
                    return Err("native_ordinary_cleanup_unconfirmed".into());
                }
                // Lease acknowledgement precedes SessionResult publication.
                // The original task must also have actually returned/joined;
                // a released watch alone cannot retire the intent's owner.
                return self.collect_cleanup_result(&attempt, &handle, || {
                    let lifecycle = self
                        .resources
                        .lifecycle
                        .upgrade()
                        .ok_or("native_ordinary_owner_unavailable")?;
                    lifecycle
                        .take_ordinary_result(handle.generation)
                        .ok_or("native_ordinary_worker_join_unconfirmed")?
                        .map_err(|_| "native_ordinary_worker_join_failed".into())
                });
            }
            if let Some(receipt) = acquisition_cleanup {
                return if receipt.confirmed() {
                    Ok(())
                } else {
                    Err("native_attempt_cleanup_unconfirmed".into())
                };
            }
            if acquisition && !self.resources.gate.native_reader_session_active() {
                return Ok(());
            }
            Err("native_attempt_cleanup_unconfirmed".into())
        })
    }
}
fn released(snapshot: &ordinary::status::Snapshot) -> bool {
    snapshot.phase == ordinary::Phase::Released
        && snapshot.cleanup_confirmed
        && snapshot.lease_released
}
fn end_reason(termination: &ordinary::status::Termination, _attempt: u32) -> intent::EndReason {
    intent_end_reason(
        termination.reason,
        termination.retry,
        termination.public_error,
    )
}
fn intent_end_reason(
    reason: ordinary::status::EndReason,
    retry: ordinary::status::RetryClass,
    error: Option<&'static str>,
) -> intent::EndReason {
    use ordinary::status::EndReason as End;
    match reason {
        End::UserStop => intent::EndReason::Stopped,
        End::ReaderChanged => intent::EndReason::ReaderChanged,
        End::RegistrationRenewal => intent::EndReason::RegistrationRenewal,
        End::PlannedReauthentication => intent::EndReason::PlannedReauthentication,
        // Only the ordinary owner's typed startup classifier may authorize a
        // rebuild. Setup failures remain permanent; a transport failure during
        // reconstruction still owes exact cleanup/lease release before retry.
        End::StartupFailure if retry == ordinary::status::RetryClass::RetryAfterRelease => {
            intent::EndReason::StartupTransient {
                error: error.unwrap_or("native_ordinary_session_failed").into(),
            }
        }
        End::StartupFailure | End::SetupFailure => intent::EndReason::StartupFailed {
            error: error.unwrap_or("native_ordinary_session_failed").into(),
        },
        _ => intent::EndReason::RuntimeLost {
            error: error.unwrap_or("native_ordinary_session_failed").into(),
            retryable: retry == ordinary::status::RetryClass::RetryAfterRelease,
        },
    }
}
pub(super) fn startup_settings() -> Result<NativeRegisteredStartupSettings, &'static str> {
    let mut random = [0; 12];
    getrandom::fill(&mut random).map_err(|_| "native_ims_entropy_unavailable")?;
    let spi_c = u32::from_ne_bytes(random[..4].try_into().unwrap()).max(256);
    let mut spi_s = u32::from_ne_bytes(random[4..8].try_into().unwrap()).max(256);
    if spi_s == spi_c {
        spi_s = spi_c.wrapping_add(1).max(256);
    }
    let port_c = 20000 + u16::from_ne_bytes(random[8..10].try_into().unwrap()) % 20000;
    let port_s = 40000 + u16::from_ne_bytes(random[10..12].try_into().unwrap()) % 20000;
    Ok(NativeRegisteredStartupSettings {
        endpoints: NativeSecurityEndpoints {
            spi_c,
            spi_s,
            port_c,
            port_s,
        },
        deadline: tokio::time::Instant::now() + Duration::from_secs(120),
        probe_interval: Duration::from_secs(20),
        retry_interval: Duration::from_secs(2),
        attempts: 3,
    })
}

pub(crate) fn public_route(method: &hyper::Method, path: &str) -> bool {
    api::call_route_owner(method, path, true, false).is_some()
        || matches!(
            (method.as_str(), path),
            ("GET" | "HEAD" | "POST", "/api/reader/vowifi")
                | ("POST", "/api/reader/vowifi/reconcile")
                | ("GET" | "POST", "/api/reader/vowifi/stop-control")
                | ("GET" | "HEAD", "/api/reader/vowifi/sessions")
                | ("GET", "/api/reader/vowifi/calls/status")
                | (
                    "POST",
                    "/api/reader/vowifi/calls/dial"
                        | "/api/reader/vowifi/calls/answer"
                        | "/api/reader/vowifi/calls/reject"
                        | "/api/reader/vowifi/calls/hangup"
                        | "/api/reader/vowifi/calls/dtmf"
                        | "/api/reader/vowifi/ussd"
                )
        )
}
impl Service {
    pub(crate) fn status(&self) -> serde_json::Value {
        self.status_for(self.record().ok().flatten())
    }

    fn status_for(&self, record: Option<Arc<Record>>) -> serde_json::Value {
        let target = record.as_ref().map(|r| {
            r.control
                .target("reader", &self.0.sms_epoch, r.expected.reader_generation)
        });
        let mut value = self.status_details_for(record);
        if let Some(target) = target {
            value["stop_target"] = json!(target);
        }
        value
    }

    fn status_details_for(&self, record: Option<Arc<Record>>) -> serde_json::Value {
        let unavailable = || json!({"available":false,"tunnel_state":"disconnected","ims_state":"idle","incoming_listener_started":false,"incoming_accept_count":0,"incoming_reachability":"unknown","progress_step":0});
        let Some(record) = record else {
            return unavailable();
        };
        if self.0.resources.identity.capture().ok().as_ref() != Some(&record.expected) {
            let mut value = unavailable();
            value["message"] = json!("读卡器或号卡已变化，请重新连接 VoWiFi");
            return value;
        }
        let state = record.progress.borrow().clone();
        let (base, session, terminal) = match record.driver.current.lock() {
            Ok(current) => (
                current.base_status.clone(),
                current.handle.as_ref().map(ordinary::Handle::snapshot),
                current.result.as_ref().map(|result| {
                    (
                        result.termination.public_error,
                        result.termination.startup_failure,
                    )
                }),
            ),
            Err(_) => {
                let mut value = unavailable();
                value["message"] = json!("原生会话状态暂不可用");
                return value;
            }
        };
        let mut value = base.unwrap_or_else(unavailable);
        value["available"] = json!(true);
        value["reader_id"] = json!(record.expected.reader);
        value["reader_generation"] = json!(record.expected.reader_generation);
        value["sim_id"] = json!(record.expected.iccid);
        let connection =
            status_projection::project(&mut value, &state, session.as_ref(), *record.stop.borrow());
        value["incoming_listener_started"] = json!(false);
        value["incoming_accept_count"] = json!(0);
        value["incoming_reachability"] = json!("unknown");
        if let Some(session) = session {
            project_incoming(&mut value, session.incoming);
            if !connection.tunnel_connected {
                value["incoming_listener_started"] = json!(false);
            }
            if connection.tunnel_connected {
                if let Some(ip) = session.local_ipv4.first() {
                    value["local_ipv4"] = json!(ip.to_string());
                }
                if let Some(ip) = session.local_ipv6.first() {
                    value["local_ipv6"] = json!(ip.to_string());
                }
                value["pcscf"] = json!(
                    session
                        .pcscf
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                );
            }
            if connection.ims_registered {
                if let Some(number) = session.registered_msisdn {
                    value["registered_msisdn"] = json!(number);
                }
                if let Some(code) = session.registration_identity_error {
                    // Persistence is best-effort, as in the compatibility
                    // backend. Expose its safe diagnostic without replacing
                    // the live registration state or leaking SIM identities.
                    value["registration_identity_warning"] = json!(code);
                }
                if let Some(transport) = session.ims_transport {
                    value["ims_transport"] = json!(transport);
                }
                value["ims_security_mode"] = json!(if session.ims_secured {
                    "ipsec"
                } else {
                    "plain"
                });
            }
            if let Some(code) = session.cleanup_error {
                value["message"] = json!(code);
            } else if let Some(termination) = session.termination {
                status_projection::failure(
                    &mut value,
                    termination.public_error,
                    termination.startup_failure,
                );
            }
        }
        if let Some((code, diagnostic)) = terminal {
            status_projection::failure(&mut value, code, diagnostic);
        }
        if value.get("message").is_none() {
            if let Some(error) = state.last_error {
                value["message"] = json!(error);
            }
        }
        value
    }
    async fn await_initial_preparation(&self, expected_intent: u64) -> Result<(), StartFailure> {
        let record = self
            .record()
            .map_err(str::to_owned)?
            .ok_or("native_intent_missing")?;
        if record.id != expected_intent {
            return Err("VoWiFi 会话已变化，启动结果不属于当前请求".into());
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
        loop {
            if self
                .record()
                .map_err(str::to_owned)?
                .is_none_or(|current| current.id != expected_intent)
            {
                return Err("VoWiFi 会话已变化，启动结果不属于当前请求".into());
            }
            if *record.stop.borrow() {
                return Err("VoWiFi 启动已取消".into());
            }
            if record.driver.handle().is_some() {
                return Ok(());
            }
            let status = record.progress.borrow().clone();
            if matches!(
                status.phase,
                intent::Phase::Failed
                    | intent::Phase::ReaderChanged
                    | intent::Phase::Stopped
                    | intent::Phase::CleanupUnconfirmed
            ) {
                if let Some(error) = record
                    .driver
                    .current
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .startup_failure
                    .clone()
                {
                    return Err(error);
                }
                return Err(status
                    .last_error
                    .unwrap_or("VoWiFi 启动未完成".into())
                    .into());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("VoWiFi 启动结果未确认，原任务仍在处理中".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub(crate) fn retained_status(&self) -> Option<serde_json::Value> {
        self.reader_observation()
            .map(|observation| observation.status)
    }
    pub(crate) async fn public_status(&self) -> serde_json::Value {
        if let Some(status) = self.retained_status() {
            return status;
        }
        let Some(snapshot) = self.0.resources.identity.snapshot() else {
            return self.status();
        };
        let expected_reader = snapshot.selection.reader.clone();
        let expected_generation = snapshot.selection.generation;
        let expected_metadata = snapshot.usim_info.clone();
        let data_root = self.0.resources.data_root.clone();
        let preview =
            tokio::task::spawn_blocking(move || preview_reader_status(&snapshot, &data_root)).await;
        // A start may have published while local configuration was read.
        if let Some(status) = self.retained_status() {
            return status;
        }
        if self.0.resources.identity.snapshot().is_none_or(|current| {
            current.selection.pending
                || current.selection.reader != expected_reader
                || current.selection.generation != expected_generation
                || current.usim_info != expected_metadata
        }) {
            let mut value = self.status();
            value["message"] = json!("读卡器或 SIM 身份已变更，已忽略过期 VoWiFi 会话状态");
            return value;
        }
        let projection = match preview {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => error.status_projection(),
            Err(_) => {
                let mut value = self.status();
                value["message"] = json!("VoWiFi 运营商配置不可用");
                return value;
            }
        };
        // Configuration is an observation, not the entire HTTP status DTO.
        // Keep required Swift fields even with no SIM or a failed preview.
        let mut value = self.status();
        value["incoming_reachability"] = json!("unverified");
        if let (Some(target), Some(fields)) = (value.as_object_mut(), projection.as_object()) {
            target.extend(fields.clone());
        }
        if value.get("startup_failure").is_some() {
            return value;
        }
        value["message"] = json!(if value["available"] != true {
            "读卡器未连接或无 SIM 卡".into()
        } else if let Some(error) = value
            .get("carrier_config_error")
            .and_then(serde_json::Value::as_str)
            .filter(|error| !error.is_empty())
        {
            format!("VoWiFi 运营商配置不可用: {error}")
        } else {
            "VoWiFi 未启动".into()
        });
        value
    }
    fn preflight_start(&self) -> Result<(), StartFailure> {
        if self
            .0
            .resources
            .ordinary_admission
            .reader_maintenance_active()
        {
            return Err(StartFailure::Busy);
        }
        if let Some(snapshot) = self.0.resources.identity.snapshot() {
            // Match the old business-admission error priority before no SIM.
            if snapshot.selection.pending
                || self
                    .0
                    .resources
                    .gate
                    .reader_work_active(&snapshot.selection.reader)
            {
                return Err(StartFailure::Busy);
            }
            if snapshot.selection.reader.is_empty()
                || snapshot.selection.generation == 0
                || snapshot.usim_info.is_none()
            {
                return Err(StartFailure::NoSim);
            }
        }
        Ok(())
    }
    fn capture_start_identity(&self) -> Result<intent::ExpectedIdentity, StartFailure> {
        if let Some(snapshot) = self.0.resources.identity.snapshot() {
            if snapshot.selection.pending {
                return Err(StartFailure::Busy);
            }
            if snapshot.selection.reader.is_empty()
                || snapshot.selection.generation == 0
                || snapshot.usim_info.is_none()
            {
                return Err(StartFailure::NoSim);
            }
            return identity_from_snapshot(snapshot).map_err(StartFailure::Internal);
        }
        self.0
            .resources
            .identity
            .capture()
            .map_err(StartFailure::Internal)
    }
    fn operation_projection(&self) -> Result<super::operation::Projection, &'static str> {
        let state = self
            .0
            .state
            .lock()
            .map_err(|_| "native_intent_state_unavailable")?;
        self.operation_projection_in_state(&state, state.record.as_ref())
    }
    fn operation_projection_for(
        &self,
        record: Option<Arc<Record>>,
    ) -> Result<super::operation::Projection, &'static str> {
        let state = self
            .0
            .state
            .lock()
            .map_err(|_| "native_intent_state_unavailable")?;
        self.operation_projection_in_state(&state, record.as_ref())
    }
    fn operation_projection_in_state(
        &self,
        state: &State,
        record: Option<&Arc<Record>>,
    ) -> Result<super::operation::Projection, &'static str> {
        let now = tokio::time::Instant::now();
        let Some(record) = record else {
            let mut clock = super::operation::Clock::start(now, Duration::ZERO);
            clock.stop(now, Duration::ZERO, false)?;
            return Ok(clock.project(
                now,
                &self.0.sms_epoch,
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
            ));
        };
        let progress = record.progress.borrow().clone();
        // Derive all companion status from this exact retained record. An old
        // request receipt must never borrow a successor's ready phase.
        let status = self.status_for(Some(record.clone()));
        let identity_matches =
            self.0.resources.identity.capture().ok().as_ref() == Some(&record.expected);
        let current = state
            .record
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, record));
        let facts = super::operation::Facts {
            generation: record.expected.reader_generation,
            phase: format!("{:?}", progress.phase).to_lowercase(),
            ready: identity_matches
                && current
                && !*record.stop.borrow()
                && progress.phase == intent::Phase::Registered
                && status["tunnel_state"] == "connected"
                && status["ims_state"] == "registered",
            sms_ready: status["sms_ready"] == true,
            call_ready: status["call_ready"] == true,
            cleanup_confirmed: progress.cleanup_confirmed
                && !record.claim.try_is_current().unwrap_or(true),
            superseded: !identity_matches
                || !current
                || progress.phase == intent::Phase::ReaderChanged,
            error: matches!(
                progress.phase,
                intent::Phase::Failed | intent::Phase::CleanupUnconfirmed
            )
            .then(|| progress.last_error.clone())
            .flatten(),
        };
        let clock = record.operation.lock().unwrap_or_else(|e| e.into_inner());
        Ok(clock.project(now, &self.0.sms_epoch, record.id, facts))
    }
    fn request_observed_stop(
        &self,
        options: &super::operation::Options,
        fingerprint: &str,
    ) -> Result<super::operation::Projection, &'static str> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| "native_vowifi_state_poisoned")?;
        let target = state.record.clone();
        if let Some(record) = &target {
            let mut clock = record.operation.lock().unwrap_or_else(|e| e.into_inner());
            let mut staged = clock.clone();
            staged.validate(
                options,
                &self.0.sms_epoch,
                record.id,
                record.expected.reader_generation,
            )?;
            let cancelled = record.progress.borrow().phase != intent::Phase::Registered;
            staged.stop(
                tokio::time::Instant::now(),
                Duration::from_secs(180),
                cancelled,
            )?;
            if let Some(request_id) = options.request_id() {
                #[cfg(test)]
                if let Some(hook) = state.before_stop_insert.take() {
                    hook(&state.requests);
                }
                state.requests.insert(staged.receipt(
                    request_id,
                    fingerprint,
                    &self.0.sms_epoch,
                    record.id,
                    record.expected.reader_generation,
                ))?;
            }
            if let Some(request_id) = options.request_id() {
                #[cfg(test)]
                if let Some(hook) = state.before_stop_submitted.take() {
                    hook(&state.requests);
                }
                state.requests.mark_submitted(request_id)?;
            }
            // A failed/uncertain persistence must not publish a stopping Clock
            // while the exact original owner has not received its stop signal.
            *clock = staged;
            record.request_stop();
        } else {
            options.validate_identity(&format!("{}-0-1", self.0.sms_epoch), 0)?;
            if let Some(request_id) = options.request_id() {
                let mut clock =
                    super::operation::Clock::start(tokio::time::Instant::now(), Duration::ZERO);
                clock.stop(tokio::time::Instant::now(), Duration::ZERO, false)?;
                state.requests.insert(clock.receipt(
                    request_id,
                    fingerprint,
                    &self.0.sms_epoch,
                    0,
                    0,
                ))?;
            }
        }
        drop(state);
        if let Some(record) = target.clone() {
            // The detached waiter refers to exactly the accepted owner. It
            // must never resolve a newer record and stop its successor.
            tokio::spawn(async move {
                let _ = record
                    .finish_before(tokio::time::Instant::now() + Duration::from_secs(180))
                    .await;
            });
        }
        self.operation_projection_for(target)
    }

    async fn reconcile_unobserved<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
    where
        B: hyper::body::Body<Data = Bytes> + Send,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let ingress = std::pin::pin!(request.into_body());
        let body = match crate::http_server::collect_body_with_timeout(
            ingress,
            REQUEST_LIMIT,
            Duration::from_secs(2),
        )
        .await
        {
            Ok(body) => body,
            _ => {
                return crate::json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":"请求格式错误"}),
                );
            }
        };
        let options = match super::operation::Options::body(&body) {
            Ok(options) => options,
            Err(code) => {
                return crate::json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":code,"code":code}),
                );
            }
        };
        let enable = match requested_enable(&body) {
            Ok(enable) => enable,
            Err(_) => {
                return crate::json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":"invalid_reconcile_payload","code":"invalid_reconcile_payload"}),
                );
            }
        };
        let fingerprint = super::operation::request_fingerprint(&body);
        let request_id = options
            .request_id()
            .expect("body options require request ID");
        let _mutation = self.0.mutation.lock().await;
        let conflict = |code: &'static str| {
            crate::json_response(StatusCode::CONFLICT, &json!({"error":code,"code":code}))
        };
        // An exact admitted ID is authoritative even when its owner is still
        // active. Reconciliation then simply returns that same receipt.
        match self.lookup_request(request_id, Some(&fingerprint)) {
            Ok(Some(receipt)) => {
                return crate::json_response(
                    StatusCode::OK,
                    &json!({"operation":receipt.resolve(self.operation_projection().ok())}),
                );
            }
            Ok(None) => {}
            Err(code) => return conflict(code),
        }
        // RejectAllOrdinary reserves the shared owner gate without touching a
        // card. It prevents another ordinary/module owner from appearing
        // between the clean-state proof and the durable tombstone.
        let maintenance = match self
            .0
            .resources
            .ordinary_admission
            .begin_reader_maintenance(
                "vowifi-reconcile",
                0,
                crate::ordinary_vowifi_admission::MaintenancePolicy::RejectAllOrdinary,
            ) {
            Ok(maintenance) => maintenance,
            Err(_) => return conflict("vowifi_reconcile_owner_unconfirmed"),
        };
        let result = (|| -> Result<super::operation::Projection, &'static str> {
            // Maintain the established lifecycle -> service lock order.
            let lifecycle = self
                .0
                .resources
                .lifecycle
                .upgrade()
                .ok_or("vowifi_reconcile_owner_unconfirmed")?;
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
            // A second lookup proves the same durable archive is healthy while
            // the shared owner gate and this service's mutation lock are held.
            if let Some(receipt) = state.requests.lookup(request_id, Some(&fingerprint))? {
                return Ok(receipt.resolve(
                    self.operation_projection_in_state(&state, state.record.as_ref())
                        .ok(),
                ));
            }
            let owner_clean = !lifecycle_state.stopping
                && !lifecycle_state.finishing
                && lifecycle_state.cleanup_failure.is_none()
                && lifecycle_state.owner.as_ref().is_some_and(|owner| {
                    owner.ordinary_tasks.is_empty()
                        && owner.session_tasks.is_empty()
                        && owner.session_task_failure.is_none()
                })
                && state.record.is_none()
                && !state.stopping
                // Sample the other reader/module fences at the commit point,
                // under stable lifecycle and service state. The shared
                // maintenance claim excludes new ordinary VoWiFi owners.
                && !self.0.resources.gate.native_reader_session_active()
                && !(self.0.resources.legacy_module_claim)();
            if !owner_clean {
                return Err("vowifi_reconcile_owner_unconfirmed");
            }
            let receipt = super::operation::Receipt::rejected(
                request_id,
                &fingerprint,
                enable,
                "原请求在健康回执账本中不存在，且 Rust 已确认没有待清理的读卡器 owner；未重新提交",
            );
            state.requests.insert(receipt.clone())?;
            Ok(receipt.resolve(None))
        })();
        if maintenance.release_confirmed().is_err() {
            return conflict("vowifi_reconcile_owner_unconfirmed");
        }
        match result {
            Ok(projection) => {
                crate::json_response(StatusCode::OK, &json!({"operation":projection}))
            }
            Err(code) => conflict(code),
        }
    }
    pub(crate) async fn handle_public<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
    where
        B: hyper::body::Body<Data = Bytes> + Send,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        if request.uri().path() == super::stop_control::READER_PATH {
            // Entire validation and signal publication precede any await.
            return self.handle_stop_control(&request);
        }
        if request.method() == hyper::Method::POST
            && request.uri().path() == "/api/reader/vowifi/reconcile"
        {
            return self.reconcile_unobserved(request).await;
        }
        if matches!(*request.method(), hyper::Method::GET | hyper::Method::HEAD)
            && request.uri().path() == "/api/reader/vowifi/sessions"
        {
            return self.public_sessions().await;
        }
        if request.uri().path() != "/api/reader/vowifi" {
            return self.handle_control(request).await;
        }
        if matches!(*request.method(), hyper::Method::GET | hyper::Method::HEAD) {
            let options = match super::operation::Options::query(request.uri().query()) {
                Ok(options) => options,
                Err(code) => {
                    return crate::json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({"error":code,"code":code}),
                    );
                }
            };
            let mut value = self.public_status().await;
            if options.enabled() {
                let projection = if let Some(request_id) = options.request_id() {
                    match self.lookup_request(request_id, None) {
                        Ok(Some(receipt)) => receipt.resolve(self.operation_projection().ok()),
                        Ok(None) => {
                            return crate::json_response(
                                StatusCode::CONFLICT,
                                &json!({"error":"vowifi_request_id_unknown","code":"vowifi_request_id_unknown"}),
                            );
                        }
                        Err(code) => {
                            return crate::json_response(
                                StatusCode::CONFLICT,
                                &json!({"error":code,"code":code}),
                            );
                        }
                    }
                } else {
                    let projection = match self.operation_projection() {
                        Ok(projection) => projection,
                        Err(code) => {
                            return crate::json_response(
                                StatusCode::CONFLICT,
                                &json!({"error":code,"code":code}),
                            );
                        }
                    };
                    if let Err(code) = options.validate(&projection) {
                        return crate::json_response(
                            StatusCode::CONFLICT,
                            &json!({"error":code,"code":code}),
                        );
                    }
                    projection
                };
                value["operation"] = json!(projection);
            }
            return crate::json_response(StatusCode::OK, &value);
        }
        let ingress = std::pin::pin!(request.into_body());
        let body = match crate::http_server::collect_body_with_timeout(
            ingress,
            REQUEST_LIMIT,
            Duration::from_secs(2),
        )
        .await
        {
            Ok(body) => body,
            _ => {
                return crate::json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":"请求格式错误"}),
                );
            }
        };
        let options = match super::operation::Options::body(&body) {
            Ok(options) => options,
            Err(code) => {
                return crate::json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":code,"code":code}),
                );
            }
        };
        let fingerprint = super::operation::request_fingerprint(&body);
        // Serialize admission with durable-request recording. A lost HTTP
        // response can retry the same ID without dispatching a second owner.
        let _mutation = self.0.mutation.lock().await;
        if let Some(request_id) = options.request_id() {
            match self.lookup_request(request_id, Some(&fingerprint)) {
                Ok(Some(receipt)) => {
                    let projection = receipt.resolve(self.operation_projection().ok());
                    return crate::json_response(
                        StatusCode::ACCEPTED,
                        &json!({"accepted":true,"stop_target":self.stop_target_for(&projection),"operation":projection}),
                    );
                }
                Ok(None) => {}
                Err(code) => {
                    return crate::json_response(
                        StatusCode::CONFLICT,
                        &json!({"error":code,"code":code}),
                    );
                }
            }
        }
        // An admitted request ID owns its fingerprint before mutable business
        // fields are decoded. A later body using the same ID must therefore
        // replay or conflict; it cannot fall through as a fresh malformed
        // mutation and obscure the idempotency decision.
        let (enable, request) = match decode_start(&body) {
            Ok(value) => value,
            Err(error) => {
                let response =
                    crate::json_response(StatusCode::BAD_REQUEST, &json!({"error":error}));
                if let Ok(enable) = requested_enable(&body) {
                    return self.reject_unadmitted(&options, &fingerprint, enable, error, response);
                }
                return response;
            }
        };
        if options.enabled() {
            if let Err(code) = self
                .operation_projection()
                .and_then(|projection| options.validate(&projection))
            {
                return self.reject_unadmitted(
                    &options,
                    &fingerprint,
                    enable,
                    code,
                    crate::json_response(StatusCode::CONFLICT, &json!({"error":code,"code":code})),
                );
            }
        }
        if !enable {
            if options.enabled() {
                if self
                    .0
                    .resources
                    .ordinary_admission
                    .current_go_claim()
                    .map_or(true, |claim| claim.is_some())
                {
                    return self.reject_unadmitted(
                        &options,
                        &fingerprint,
                        false,
                        "legacy_stop_unconfirmed",
                        crate::json_response(
                            StatusCode::CONFLICT,
                            &json!({"error":"legacy_stop_unconfirmed","code":"legacy_stop_unconfirmed"}),
                        ),
                    );
                }
                return match self.request_observed_stop(&options, &fingerprint) {
                    Ok(projection) => {
                        match self.remember_request(&options, &fingerprint, &projection) {
                            Ok(receipt) => {
                                let projection = receipt.map_or(projection.clone(), |receipt| {
                                    receipt.resolve(Some(projection))
                                });
                                crate::json_response(
                                    StatusCode::ACCEPTED,
                                    &json!({"accepted":true,"stop_target":self.stop_target_for(&projection),"operation":projection}),
                                )
                            }
                            Err(code) => crate::json_response(
                                StatusCode::CONFLICT,
                                &json!({"error":code,"code":code}),
                            ),
                        }
                    }
                    Err(code) => self.reject_unadmitted(
                        &options,
                        &fingerprint,
                        false,
                        code,
                        crate::json_response(
                            StatusCode::CONFLICT,
                            &json!({"error":code,"code":code}),
                        ),
                    ),
                };
            }
            return match self
                .stop_global(tokio::time::Instant::now() + Duration::from_secs(180))
                .await
            {
                Ok(()) => crate::json_response(
                    StatusCode::OK,
                    &json!({"accepted":true,"tunnel_state":"disconnected","message":"Wi-Fi Calling 已关闭"}),
                ),
                Err(_) => crate::json_response(
                    StatusCode::CONFLICT,
                    &json!({"error":"VoWiFi 正在释放资源，尚未确认关闭"}),
                ),
            };
        }
        if let Err(message) = validate_start_proxy(&request.proxy) {
            return self.reject_unadmitted(
                &options,
                &fingerprint,
                true,
                message,
                crate::json_response(StatusCode::BAD_REQUEST, &json!({"error":message})),
            );
        }
        if let Err(error) = self.preflight_start() {
            let message = error.to_string();
            return self.reject_unadmitted(
                &options,
                &fingerprint,
                true,
                &message,
                error.response(),
            );
        }
        let request = match request.freeze() {
            Ok(request) => request,
            Err(error) => {
                let message = error.to_string();
                return self.reject_unadmitted(
                    &options,
                    &fingerprint,
                    true,
                    &message,
                    error.response(),
                );
            }
        };
        let intent_id = match self
            .start_checked_observed(request, Some((&options, &fingerprint)))
            .await
        {
            Ok(id) => id,
            Err(error) => {
                let message = error.to_string();
                return self.reject_unadmitted(
                    &options,
                    &fingerprint,
                    true,
                    &message,
                    error.response(),
                );
            }
        };
        if options.enabled() {
            return match self.operation_projection() {
                Ok(projection) => {
                    match self.remember_request(&options, &fingerprint, &projection) {
                        Ok(receipt) => {
                            let projection = receipt.map_or(projection.clone(), |receipt| {
                                receipt.resolve(Some(projection))
                            });
                            crate::json_response(
                                StatusCode::ACCEPTED,
                                &json!({"accepted":true,"stop_target":self.stop_target_for(&projection),"operation":projection}),
                            )
                        }
                        Err(code) => crate::json_response(
                            StatusCode::CONFLICT,
                            &json!({"error":code,"code":code}),
                        ),
                    }
                }
                Err(code) => {
                    crate::json_response(StatusCode::CONFLICT, &json!({"error":code,"code":code}))
                }
            };
        }
        match self.await_initial_preparation(intent_id).await {
            Ok(()) => crate::json_response(
                StatusCode::OK,
                &json!({"accepted":true,"tunnel_state":"connecting","message":"VoWiFi 正在启动..."}),
            ),
            Err(error) => error.response(),
        }
    }
}
fn project_incoming(
    value: &mut serde_json::Value,
    facts: proletarius_vowifi::registration_coordinator::IncomingDiagnostics,
) {
    value["incoming_listener_started"] = json!(facts.listener_started);
    value["incoming_accept_count"] = json!(facts.accept_count);
    value["incoming_reachability"] = json!(facts.reachability.as_str());
    if let Some(object) = value.as_object_mut() {
        object.remove("incoming_last_sip_at");
        object.remove("incoming_last_response_write_error");
    }
    if let Some(at) = facts
        .last_sip_at
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|at| i128::try_from(at.as_nanos()).ok())
        .and_then(|at| time::OffsetDateTime::from_unix_timestamp_nanos(at).ok())
        .and_then(|at| {
            at.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
    {
        value["incoming_last_sip_at"] = json!(at);
    }
    if let Some(error) = facts.last_response_write_error {
        value["incoming_last_response_write_error"] = json!(error.as_str());
    }
}

struct StartRequest {
    proxy: String,
    proxy_host: Option<String>,
    proxy_port: Option<String>,
    epdg: String,
    epdg_port: i64,
    epdg_port_text: Option<String>,
}
impl StartRequest {
    fn freeze(self) -> Result<intent::FrozenRequest, StartFailure> {
        let proxy = crate::vowifi_input::normalize_proxy(
            &self.proxy,
            self.proxy_host.as_deref(),
            self.proxy_port.as_deref(),
        )
        .map_err(|message| StartFailure::InvalidRequest(message.into()))?;
        let epdg = crate::vowifi_input::normalize_epdg(&self.epdg)
            .map_err(|message| StartFailure::InvalidRequest(message.into()))?;
        let epdg_port = crate::vowifi_input::normalize_epdg_port(
            self.epdg_port,
            self.epdg_port_text.as_deref(),
        )
        .map_err(|message| StartFailure::InvalidRequest(message.into()))?;
        // Go's int accepts non-positive input as the carrier/default port.
        // A positive out-of-range port fails in configuration, not decoding.
        let epdg_port = if epdg_port <= 0 {
            0
        } else {
            u16::try_from(epdg_port)
                .map_err(|_| StartFailure::Internal(format!("ePDG 端口无效: {epdg_port}")))?
        };
        Ok(intent::FrozenRequest {
            proxy,
            epdg,
            epdg_port,
        })
    }
}
fn validate_start_proxy(proxy: &str) -> Result<(), &'static str> {
    let proxy = proxy.trim();
    let proxy = proxy.strip_prefix("socks5://").unwrap_or(proxy);
    let proxy = proxy.strip_prefix("socks5h://").unwrap_or(proxy);
    if proxy.is_empty() {
        return Ok(());
    }
    let (host, port) = if let Some(rest) = proxy.strip_prefix('[') {
        let Some((host, suffix)) = rest.split_once(']') else {
            return Err("前置代理地址无效，请手动填写有效的 IP:端口");
        };
        let Some(port) = suffix.strip_prefix(':') else {
            return Err("前置代理地址无效，请手动填写有效的 IP:端口");
        };
        if host.contains(['[', ']']) || port.contains([':', '[', ']']) {
            return Err("前置代理地址无效，请手动填写有效的 IP:端口");
        }
        (host, port)
    } else {
        let Some((host, port)) = proxy.split_once(':') else {
            return Err("前置代理地址无效，请手动填写有效的 IP:端口");
        };
        if host.contains(['[', ']']) || port.contains([':', '[', ']']) {
            return Err("前置代理地址无效，请手动填写有效的 IP:端口");
        }
        (host, port)
    };
    if host.trim().is_empty() {
        return Err("前置代理地址无效，请手动填写有效的 IP:端口");
    }
    match port.parse::<i64>() {
        Ok(1..=65535) => Ok(()),
        _ => Err("前置代理端口无效"),
    }
}
// Reader mutation decoding preserves ordered, case-folded fields and skips
// null values. Receipt direction must use that same contract, including the
// default false; Module's null-clearing contract is intentionally separate.
fn start_fields(body: &[u8]) -> Result<api::Fields, &'static str> {
    let mut decoder = serde_json::Deserializer::from_slice(body);
    Option::<api::Fields>::deserialize(&mut decoder)
        .map_err(|_| "请求格式错误")
        .map(|fields| fields.unwrap_or(api::Fields(Vec::new())))
}

fn requested_enable_from_fields(fields: &api::Fields) -> Result<bool, &'static str> {
    let mut enable = false;
    for (key, value) in &fields.0 {
        if api::field_name(key) == "enable" && !value.is_null() {
            // An invalid earlier duplicate remains invalid even when followed
            // by a valid boolean. Do not collapse ordered fields into Value.
            enable = value.as_bool().ok_or("请求格式错误")?;
        }
    }
    Ok(enable)
}

fn requested_enable(body: &[u8]) -> Result<bool, &'static str> {
    requested_enable_from_fields(&start_fields(body)?)
}

fn decode_start(body: &[u8]) -> Result<(bool, StartRequest), &'static str> {
    let fields = start_fields(body)?;
    let enable = requested_enable_from_fields(&fields)?;
    let mut request = StartRequest {
        proxy: String::new(),
        proxy_host: None,
        proxy_port: None,
        epdg: String::new(),
        epdg_port: 0,
        epdg_port_text: None,
    };
    for (key, value) in fields.0 {
        if value.is_null() {
            continue;
        }
        match api::field_name(&key).as_str() {
            "enable" => {} // Already validated with the shared receipt decoder.
            "proxy" => request.proxy = value.as_str().ok_or("请求格式错误")?.trim().to_owned(),
            "proxy_host" => {
                request.proxy_host = Some(value.as_str().ok_or("请求格式错误")?.to_owned())
            }
            "proxy_port" => {
                request.proxy_port = Some(value.as_str().ok_or("请求格式错误")?.to_owned())
            }
            "epdg" => request.epdg = value.as_str().ok_or("请求格式错误")?.trim().to_owned(),
            "epdg_port" => request.epdg_port = value.as_i64().ok_or("请求格式错误")?,
            "epdg_port_text" => {
                request.epdg_port_text = Some(value.as_str().ok_or("请求格式错误")?.to_owned())
            }
            _ => {}
        }
    }
    Ok((enable, request))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cleanup_tests;

#[cfg(test)]
mod startup_http_tests;

#[cfg(test)]
mod http_tests {
    use super::*;
    #[test]
    fn startup_decode_matches_go_case_duplicate_null_and_first_object_contract() {
        let (enabled, request) = decode_start(br#"{"Enable":true,"PROXY":"first:1","proxy":null,"proxy":"second:2","EPDG":"custom.example","epdg_port":500,"epdg_port":null,"unknown":7} {}"#).unwrap();
        assert!(enabled);
        assert_eq!(request.proxy, "second:2");
        assert_eq!(request.epdg, "custom.example");
        assert_eq!(request.epdg_port, 500);
        assert!(!decode_start(b"null").unwrap().0);
        assert!(!decode_start(br#"{"enabled":true}"#).unwrap().0);
        for body in [
            br#"{"enable":1}"#.as_slice(),
            br#"{"epdg_port":9223372036854775808}"#,
            br#"{"epdg_port":1.5}"#,
            br#"{"proxy":false}"#,
        ] {
            assert!(decode_start(body).is_err());
        }
        assert_eq!(decode_start(br#"{"enable":true}"#).unwrap().1.epdg_port, 0);
    }
    #[test]
    fn raw_ui_proxy_and_epdg_fields_are_normalized_only_by_rust() {
        let (_, request) = decode_start(
            br#"{"enable":true,"proxy_host":"2001:db8::1","proxy_port":"1080","epdg":" wlan.example.test ","epdg_port_text":"4500"}"#,
        )
        .unwrap();
        let frozen = match request.freeze() {
            Ok(frozen) => frozen,
            Err(error) => panic!("unexpected failure: {error}"),
        };
        assert_eq!(frozen.proxy, "socks5://[2001:db8::1]:1080");
        assert_eq!(frozen.epdg, "wlan.example.test");
        assert_eq!(frozen.epdg_port, 4500);

        for body in [
            br#"{"proxy_host":"proxy.example","proxy_port":"0"}"#.as_slice(),
            br#"{"proxy_host":"socks5://proxy.example","proxy_port":"1080"}"#,
            br#"{"epdg":"https://wlan.example.test"}"#,
            br#"{"epdg_port_text":"65536"}"#,
        ] {
            assert!(decode_start(body).unwrap().1.freeze().is_err());
        }
    }
    #[test]
    fn incoming_projection_uses_observed_facts_and_clears_previous_session_diagnostics() {
        use proletarius_vowifi::registration_coordinator::{
            IncomingDiagnostics, IncomingReachability, IncomingWriteError,
        };
        let mut value = json!({});
        let observed = IncomingDiagnostics {
            listener_started: true,
            accept_count: 2,
            last_sip_at: Some(std::time::UNIX_EPOCH + Duration::from_secs(42)),
            last_response_write_error: Some(IncomingWriteError::ConnectionClosed),
            reachability: IncomingReachability::Observed,
            ..Default::default()
        };
        project_incoming(&mut value, observed);
        assert_eq!(value["incoming_listener_started"], true);
        assert_eq!(value["incoming_accept_count"], 2);
        assert_eq!(value["incoming_reachability"], "observed");
        assert_eq!(value["incoming_last_sip_at"], "1970-01-01T00:00:42Z");
        assert_eq!(
            value["incoming_last_response_write_error"],
            "connection_closed"
        );
        project_incoming(&mut value, IncomingDiagnostics::default());
        assert_eq!(value["incoming_reachability"], "unverified");
        assert_eq!(value["incoming_accept_count"], 0);
        assert_eq!(value["incoming_listener_started"], false);
        assert!(value.get("incoming_last_sip_at").is_none());
        assert!(value.get("incoming_last_response_write_error").is_none());
    }

    #[test]
    fn default_reader_start_is_native_and_module_routes_remain_separate() {
        let lifecycle = NativeLifecycle::default();
        assert!(public_route(&hyper::Method::POST, "/api/reader/vowifi"));
        assert!(lifecycle.handles_public_request(&hyper::Method::POST, "/api/reader/vowifi"));
        assert!(public_route(&hyper::Method::POST, "/api/vowifi/calls/dial"));
        assert!(lifecycle.handles_public_request(&hyper::Method::POST, "/api/vowifi/calls/dial"));
        assert!(public_route(
            &hyper::Method::GET,
            "/api/reader/vowifi/sessions"
        ));
        assert!(
            lifecycle.handles_public_request(&hyper::Method::GET, "/api/reader/vowifi/sessions")
        );
        assert!(!public_route(
            &hyper::Method::POST,
            "/api/reader/vowifi/calls/status"
        ));
        assert!(!public_route(&hyper::Method::DELETE, "/api/reader/vowifi"));
        assert!(lifecycle.ordinary_service().is_none());
    }
}

#[cfg(test)]
mod production_integration;
