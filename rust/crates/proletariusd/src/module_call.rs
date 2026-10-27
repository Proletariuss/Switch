//! Native cellular-call lifecycle. A request observer never owns the physical
//! action: retained jobs keep their radio/session lease until execution and
//! cleanup really finish. IMS calls use their separate existing owner.
use crate::{
    call_control::{Action, Backend, Reply, ReplyFuture},
    call_facts::{CallFacts, NativeSink},
    call_mode_lifecycle, message_archive,
    module_call_state::{Event, Snapshot, State},
    module_radio::{self, CallAction, CallEvent, CallReservation, CallStateSnapshot},
    usb_at,
};
use hyper::StatusCode;
use serde_json::{Value, json};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex as ShortMutex, Weak},
    time::Duration,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, Notify};

use crate::module_radio_transport::Binding;

type Work<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type Admission = Arc<dyn Fn() -> bool + Send + Sync>;
const MAX_ACCEPTED_JOBS: usize = 16;
#[derive(Debug)]
struct Failure {
    status: StatusCode,
    code: String,
    message: String,
}
impl From<module_radio::Failure> for Failure {
    fn from(e: module_radio::Failure) -> Self {
        Self {
            status: e.public_status(),
            code: e.public_code().into(),
            message: e.public_message().into(),
        }
    }
}
impl Failure {
    fn reply(self) -> Reply {
        crate::json_response(
            self.status,
            &json!({"error":self.message,"code":self.code,"recoverable":true}),
        )
    }
}
trait Driver: Send + Sync {
    fn reserve(&self) -> Work<Result<CallReservation, Failure>>;
    fn release(&self, lease: CallReservation) -> Work<Result<(), Failure>>;
    fn query(&self, lease: Option<CallReservation>) -> Work<Result<CallStateSnapshot, Failure>>;
    fn execute(
        &self,
        lease: CallReservation,
        action: Action,
        value: String,
        admission: Admission,
    ) -> Work<Result<String, Failure>>;
    fn ready(&self, producer: String) -> Work<Result<(), String>>;
    fn stop_audio(&self, producer: String) -> Work<Result<(), String>>;
    fn lease_current(&self, lease: &CallReservation) -> bool;
    fn event_current(&self, event: &CallEvent) -> bool;
    fn binding(&self) -> Option<Binding>;
    fn observe(&self, observer: Arc<dyn Fn(CallEvent) -> Result<(), String> + Send + Sync>);
    fn observation_error(&self) -> Option<String> {
        None
    }
}
struct NativeDriver {
    root: PathBuf,
    radio: module_radio::Service,
    inbox: message_archive::InboxState,
    usb: usb_at::Owner,
    mode: call_mode_lifecycle::Service,
}
impl Driver for NativeDriver {
    fn reserve(&self) -> Work<Result<CallReservation, Failure>> {
        let r = self.radio.clone();
        Box::pin(async move { r.reserve_call().await.map_err(Into::into) })
    }
    fn release(&self, lease: CallReservation) -> Work<Result<(), Failure>> {
        let r = self.radio.clone();
        Box::pin(async move { r.release_call(&lease).await.map_err(Into::into) })
    }
    fn query(&self, lease: Option<CallReservation>) -> Work<Result<CallStateSnapshot, Failure>> {
        let r = self.radio.clone();
        Box::pin(async move { r.query_call_state(lease.as_ref()).await.map_err(Into::into) })
    }
    fn execute(
        &self,
        lease: CallReservation,
        action: Action,
        value: String,
        admission: Admission,
    ) -> Work<Result<String, Failure>> {
        let r = self.radio.clone();
        Box::pin(async move {
            let action = match action {
                Action::Dial => CallAction::Dial,
                Action::Answer => CallAction::Answer,
                Action::Hangup => CallAction::Hangup,
                Action::Dtmf => CallAction::Dtmf,
            };
            r.execute_call(&lease, action, &value, admission)
                .await
                .map_err(Into::into)
        })
    }
    fn ready(&self, producer: String) -> Work<Result<(), String>> {
        let mode = self.mode.clone();
        Box::pin(async move { mode.ensure_ready(&producer).await })
    }
    fn stop_audio(&self, producer: String) -> Work<Result<(), String>> {
        let root = self.root.clone();
        let inbox = self.inbox.clone();
        let usb = self.usb.clone();
        let radio = self.radio.clone();
        Box::pin(async move {
            crate::call_mode_adb::stop_native_hangup(&root, inbox, usb, radio, producer).await
        })
    }
    fn lease_current(&self, lease: &CallReservation) -> bool {
        self.radio.call_reservation_current(lease)
    }
    fn event_current(&self, event: &CallEvent) -> bool {
        self.radio.call_event_current(event)
    }
    fn binding(&self) -> Option<Binding> {
        self.radio.current_call_binding()
    }
    fn observe(&self, observer: Arc<dyn Fn(CallEvent) -> Result<(), String> + Send + Sync>) {
        self.radio.set_call_observer(observer);
    }
    fn observation_error(&self) -> Option<String> {
        self.radio.call_observation_error()
    }
}

#[derive(Clone)]
pub(crate) struct Service(Arc<Inner>);
struct Inner {
    driver: Arc<dyn Driver>,
    state: State,
    facts: CallFacts,
    physical: Mutex<Physical>,
    life: ShortMutex<Life>,
    wake: Arc<Notify>,
    drained: Notify,
    retire_gate: Mutex<()>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
#[derive(Default)]
struct Physical {
    lease: Option<CallReservation>,
    releasing: bool,
}
#[derive(Default)]
struct Life {
    stopping: bool,
    paused: bool,
    observations_closed: bool,
    worker_stop: bool,
    shutdown_receipt: Option<Result<Vec<String>, String>>,
    #[cfg(test)]
    shutdown_retire_error: Option<String>,
    jobs: usize,
    binding: Option<Binding>,
}
struct JobPin(Arc<Inner>);
impl Drop for JobPin {
    fn drop(&mut self) {
        self.0.life.lock().unwrap_or_else(|e| e.into_inner()).jobs -= 1;
        self.0.drained.notify_waiters();
    }
}
impl Service {
    pub(crate) fn new(
        root: PathBuf,
        radio: module_radio::Service,
        inbox: message_archive::InboxState,
        usb: usb_at::Owner,
        mode: call_mode_lifecycle::Service,
        facts: CallFacts,
    ) -> Result<Self, String> {
        Self::with_driver(
            Arc::new(NativeDriver {
                root,
                radio,
                inbox,
                usb,
                mode,
            }),
            facts,
        )
    }
    fn with_driver(driver: Arc<dyn Driver>, facts: CallFacts) -> Result<Self, String> {
        let sink = NativeSink::start(facts.clone(), crate::snapshot::new_instance_id(), 128)?;
        let state = State::new(sink);
        state.start_owner("module-cs".into(), 1, OffsetDateTime::now_utc())?;
        let this = Self(Arc::new(Inner {
            driver,
            state,
            facts,
            physical: Mutex::new(Physical::default()),
            life: ShortMutex::new(Life::default()),
            wake: Arc::new(Notify::new()),
            drained: Notify::new(),
            retire_gate: Mutex::new(()),
            worker: Mutex::new(None),
        }));
        let weak = Arc::downgrade(&this.0);
        this.0.driver.observe(Arc::new(move |event| {
            let Some(inner) = weak.upgrade() else {
                return Ok(());
            };
            let mut life = inner.life.lock().unwrap_or_else(|e| e.into_inner());
            if life.observations_closed || !inner.driver.event_current(&event) {
                return Ok(());
            }
            if life
                .binding
                .as_ref()
                .is_some_and(|binding| binding != &event.binding)
            {
                inner.state.invalidate_observation();
            }
            life.binding = Some(event.binding.clone());
            let at = OffsetDateTime::parse(&event.observed_at, &Rfc3339)
                .map_err(|_| "invalid native call observation time".to_owned())?;
            let kind = match event.kind.as_str() {
                "ring" => Event::Ring,
                "clip" => Event::Clip(event.number),
                "connect" => Event::Connect,
                "hangup" => Event::Hangup,
                _ => return Err("invalid native call observation kind".into()),
            };
            inner.state.apply_event(kind, at);
            inner.wake.notify_one();
            Ok(())
        }));
        let weak = Arc::downgrade(&this.0);
        let worker = tokio::spawn(async move {
            Self::radio_worker(weak).await;
        });
        // Constructor runs before the service becomes visible to other tasks.
        *this.0.worker.try_lock().expect("new call worker slot") = Some(worker);
        Ok(this)
    }
    fn pin(&self) -> Result<JobPin, Reply> {
        let mut life = self.0.life.lock().unwrap_or_else(|e| e.into_inner());
        if life.stopping || life.paused {
            return Err(unavailable());
        }
        if life.jobs >= MAX_ACCEPTED_JOBS {
            return Err(crate::json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error":"模块通话请求过多，请稍后重试","code":"module_call_busy","recoverable":true}),
            ));
        }
        life.jobs += 1;
        Ok(JobPin(self.0.clone()))
    }
    fn accepting(&self) -> bool {
        let life = self.0.life.lock().unwrap_or_else(|e| e.into_inner());
        !life.stopping && !life.paused
    }
    fn display(&self) -> Value {
        let mut display = self.0.state.snapshot().display;
        if let Some(error) = self.0.facts.history_error() {
            display["history_error"] = json!(error);
        }
        if let Some(error) = self.0.driver.observation_error() {
            let prior = display["history_error"].as_str().unwrap_or_default();
            display["history_error"] = json!(if prior.is_empty() {
                error
            } else {
                format!("{prior}; {error}")
            });
        }
        display
    }
    fn sync_binding(&self, binding: Option<Binding>) {
        let mut life = self.0.life.lock().unwrap_or_else(|e| e.into_inner());
        if life.binding.is_some() && life.binding != binding {
            self.0.state.invalidate_observation();
        }
        life.binding = binding;
    }
    async fn observe_locked(&self, physical: &Physical) -> Snapshot {
        self.sync_binding(self.0.driver.binding());
        let revision = self.0.state.query_revision();
        let result = self.0.driver.query(physical.lease.clone()).await;
        self.sync_binding(self.0.driver.binding());
        let (display, safe_idle) = match result {
            Ok(value) => (value.display, value.safe_idle),
            Err(_) => (unknown(), false),
        };
        let snapshot =
            self.0
                .state
                .apply_query(revision, display, safe_idle, OffsetDateTime::now_utc());
        self.0.wake.notify_one();
        snapshot
    }
    async fn status_owned(&self) -> Reply {
        // Controls and RF work already retain this same physical lane. A UI
        // poll can use their current snapshot instead of queuing duplicate AT.
        if let Ok(physical) = self.0.physical.try_lock() {
            self.observe_locked(&physical).await;
        }
        crate::json_response(StatusCode::OK, &self.display())
    }
    async fn ensure_lease(&self, physical: &mut Physical) -> Result<CallReservation, Failure> {
        if physical.releasing {
            return Err(Failure {
                status: StatusCode::CONFLICT,
                code: "module_radio_cleanup_pending".into(),
                message: "上一次通话资源清理尚未确认，请稍后重试".into(),
            });
        }
        if let Some(lease) = &physical.lease {
            return Ok(lease.clone());
        }
        let lease = self.0.driver.reserve().await?;
        physical.lease = Some(lease.clone());
        Ok(lease)
    }
    async fn execute_owned(&self, action: Action, value: String) -> Reply {
        let mut physical = match tokio::time::timeout(
            Duration::from_secs(5),
            self.0.physical.lock(),
        )
        .await
        {
            Ok(physical) => physical,
            Err(_) => {
                return crate::json_response(
                    StatusCode::CONFLICT,
                    &json!({"error":"模块通话操作尚未完成，请稍后重试","code":"module_call_busy","recoverable":true}),
                );
            }
        };
        if !self.accepting() {
            return unavailable();
        }
        let initial = self.observe_locked(&physical).await;
        if let Some(reply) = admission_error(action, &initial) {
            return reply;
        }
        if action == Action::Hangup && initial.safe_idle && !initial.pending_dial {
            return crate::json_response(
                StatusCode::OK,
                &json!({"accepted":true,"already_idle":true}),
            );
        }
        let lease = match self.ensure_lease(&mut physical).await {
            Ok(lease) => lease,
            Err(error) => return error.reply(),
        };
        if !self.accepting() || !self.0.driver.lease_current(&lease) {
            self.0.wake.notify_one();
            return unavailable();
        }
        if matches!(action, Action::Dial | Action::Answer) {
            if let Err(error) = self.0.driver.ready(lease.producer_id.clone()).await {
                self.0.wake.notify_one();
                return crate::json_response(StatusCode::CONFLICT, &json!({"error":error}));
            }
        }
        // Readiness and RF acquisition may have taken time. Re-observe under
        // the final lease, then fence again at the actual physical write.
        let current = self.observe_locked(&physical).await;
        if let Some(reply) = admission_error(action, &current) {
            self.0.wake.notify_one();
            return reply;
        }
        if action == Action::Hangup && current.safe_idle && !current.pending_dial {
            self.0.wake.notify_one();
            return crate::json_response(
                StatusCode::OK,
                &json!({"accepted":true,"already_idle":true}),
            );
        }
        let revision = current.revision;
        let dial = if action == Action::Dial {
            self.0.state.begin_dial(OffsetDateTime::now_utc())
        } else {
            None
        };
        if action == Action::Dial && dial.is_none() {
            self.0.wake.notify_one();
            return state_error(action, &self.0.state.snapshot());
        }
        let service = self.clone();
        let physical_lease = lease.clone();
        let pending = dial;
        let guard: Admission = Arc::new(move || {
            if !service.accepting() || !service.0.driver.lease_current(&physical_lease) {
                return false;
            }
            match pending {
                Some(ticket) => {
                    service.0.state.dial_current(ticket)
                        && service.0.state.snapshot().display["state"] == "dialing"
                }
                None => service.0.state.revision_current(revision),
            }
        });
        let result = self
            .0
            .driver
            .execute(lease.clone(), action, value.clone(), guard)
            .await;
        match result {
            Err(error) => {
                if let Some(ticket) = dial {
                    self.0.state.cancel_dial(ticket);
                }
                // Only observe an uncertain command; never replay it.
                self.observe_locked(&physical).await;
                self.0.wake.notify_one();
                crate::json_response(StatusCode::BAD_GATEWAY, &json!({"error":error.message}))
            }
            Ok(response) => {
                let at = OffsetDateTime::now_utc();
                let update = match action {
                    Action::Dial => {
                        self.0
                            .state
                            .complete_dial(dial.expect("admitted dial"), &value, at)
                    }
                    Action::Answer => self.0.state.answer(revision, at),
                    Action::Hangup => {
                        // ATH OK only acknowledges the command (CVHU may ignore
                        // ATH). Observe once under the same retained lease; never
                        // replay a write or terminate a newer incoming call.
                        self.sync_binding(self.0.driver.binding());
                        let observed = self.0.driver.query(Some(lease.clone())).await;
                        self.sync_binding(self.0.driver.binding());
                        match observed {
                            Ok(value)
                                if value.safe_idle
                                    && self.0.driver.lease_current(&lease) =>
                            {
                                self.0.state.end(revision, OffsetDateTime::now_utc())
                            }
                            Ok(value) if self.0.driver.lease_current(&lease) => {
                                self.0.state.apply_query(
                                    revision,
                                    value.display,
                                    false,
                                    OffsetDateTime::now_utc(),
                                )
                            }
                            _ => self.0.state.apply_query(
                                revision,
                                unknown(),
                                false,
                                OffsetDateTime::now_utc(),
                            ),
                        }
                    }
                    Action::Dtmf => self.0.state.snapshot(),
                };
                let mut payload = json!({"accepted":true,"response":response});
                if action == Action::Hangup {
                    let confirmed =
                        update.safe_idle && !update.desired_active && !update.pending_dial;
                    payload["confirmed"] = json!(confirmed);
                    let warning = if !confirmed {
                        payload["code"] = json!("module_call_hangup_unconfirmed");
                        "挂断指令已提交，但尚未确认通话结束；已保留通话保护并继续核对状态".into()
                    } else if update.applied {
                        match self.0.driver.stop_audio(lease.producer_id.clone()).await {
                            Ok(()) => String::new(),
                            Err(error) => {
                                format!("通话已挂断，但模块音频路由清理未完全确认：{error}")
                            }
                        }
                    } else {
                        String::new()
                    };
                    payload["warning"] = json!(warning);
                }
                self.0.wake.notify_one();
                crate::json_response(StatusCode::OK, &payload)
            }
        }
    }
    async fn radio_worker(weak: Weak<Inner>) {
        loop {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let wake = inner.wake.clone().notified_owned();
            tokio::pin!(wake);
            wake.as_mut().enable();
            let service = Self(inner.clone());
            let stop = inner
                .life
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .worker_stop;
            let mut retry = false;
            {
                let mut physical = inner.physical.lock().await;
                let paused = {
                    let l = inner.life.lock().unwrap_or_else(|e| e.into_inner());
                    l.paused || l.stopping
                };
                if physical
                    .lease
                    .as_ref()
                    .is_some_and(|lease| !inner.driver.lease_current(lease))
                {
                    physical.releasing = true;
                }
                let desired = inner.state.snapshot();
                if !paused
                    && (desired.desired_active || desired.pending_dial)
                    && physical.lease.is_none()
                    && !physical.releasing
                {
                    match service.ensure_lease(&mut physical).await {
                        Ok(_) => {}
                        Err(_) => retry = true,
                    }
                }
                // Re-read after asynchronous acquisition. A late idle cannot
                // release a newer pending dial; stale acquisition is cleaned.
                let desired = inner.state.snapshot();
                if (physical.releasing
                    || paused
                    || !(desired.desired_active || desired.pending_dial))
                    && physical.lease.is_some()
                {
                    physical.releasing = true;
                    let lease = physical.lease.clone().expect("checked lease");
                    match inner.driver.release(lease).await {
                        Ok(()) => {
                            physical.lease = None;
                            physical.releasing = false;
                            retry = true;
                        }
                        Err(_) => retry = true,
                    }
                }
                if stop && physical.lease.is_none() {
                    return;
                }
            }
            drop(service);
            drop(inner);
            if retry {
                tokio::select! {_=&mut wake=>{},_=tokio::time::sleep(Duration::from_millis(250))=>{}}
            } else {
                wake.await;
            }
        }
    }
    async fn wait_jobs(&self) {
        loop {
            let done = self.0.drained.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if self.0.life.lock().unwrap_or_else(|e| e.into_inner()).jobs == 0 {
                return;
            }
            done.await;
        }
    }
    async fn release_until_confirmed(&self) {
        let mut physical = self.0.physical.lock().await;
        if let Some(lease) = physical.lease.clone() {
            physical.releasing = true;
            while self.0.driver.release(lease.clone()).await.is_err() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            physical.lease = None;
            physical.releasing = false;
        }
    }
    pub(crate) async fn retire_transport(&self) -> Result<(), String> {
        self.0.life.lock().unwrap_or_else(|e| e.into_inner()).paused = true;
        self.0.wake.notify_one();
        let this = self.clone();
        let task = tokio::spawn(async move {
            let _gate = this.0.retire_gate.lock().await;
            this.wait_jobs().await;
            this.release_until_confirmed().await;
        });
        tokio::time::timeout(Duration::from_secs(180), task)
            .await
            .map_err(|_| "native module call transport drain unconfirmed".to_owned())?
            .map_err(|e| format!("native module call retirement failed: {e}"))
    }
    pub(crate) fn resume_transport(&self) {
        let mut life = self.0.life.lock().unwrap_or_else(|e| e.into_inner());
        // A replaced transport must earn fresh observations even if the OS
        // later reuses its identity. Preserve durable call facts independently.
        self.0.state.invalidate_observation();
        life.binding = None;
        life.paused = false;
        self.0.wake.notify_one();
    }
    pub(crate) fn begin_shutdown(&self) {
        self.0
            .life
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopping = true;
        self.0.wake.notify_one();
    }
    pub(crate) async fn finish_shutdown(&self) -> Result<(), String> {
        let errors = self.finish_shutdown_receipt().await?;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Ok confirms physical retirement, even when history reporting failed.
    /// Err never permits the coordinator to close this owner's transport.
    pub(crate) async fn finish_shutdown_receipt(&self) -> Result<Vec<String>, String> {
        self.begin_shutdown();
        let this = self.clone();
        let task = tokio::spawn(async move {
            let _gate = this.0.retire_gate.lock().await;
            {
                let mut life = this.0.life.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(receipt) = &life.shutdown_receipt {
                    return receipt.clone();
                }
                // A panicked retained finalizer must not become success on retry.
                life.shutdown_receipt =
                    Some(Err("native module call finalizer unconfirmed".into()));
            }
            let result = this.finish_shutdown_owned().await;
            this.0
                .life
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .shutdown_receipt = Some(result.clone());
            result
        });
        // Waiter cancellation does not cancel physical cleanup or history drain.
        task.await
            .map_err(|e| format!("native module call shutdown failed: {e}"))?
    }

    async fn finish_shutdown_owned(&self) -> Result<Vec<String>, String> {
        self.wait_jobs().await;
        {
            let mut life = self.0.life.lock().unwrap_or_else(|e| e.into_inner());
            life.observations_closed = true;
            life.worker_stop = true;
        }
        self.0.wake.notify_one();
        self.release_until_confirmed().await;
        if let Some(worker) = self.0.worker.lock().await.take() {
            worker
                .await
                .map_err(|e| format!("native call radio worker: {e}"))?;
        }
        let mut errors = Vec::new();
        let retirement = self.0.state.retire(OffsetDateTime::now_utc());
        #[cfg(test)]
        let retirement = retirement.and_then(|()| {
            self.0
                .life
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .shutdown_retire_error
                .take()
                .map_or(Ok(()), Err)
        });
        if let Err(error) = retirement {
            errors.push(error);
        }
        // A rejected terminal/history write does not excuse skipping the real
        // archive worker join. Its error is separate from physical call release.
        if let Err(error) = self.0.state.sink().drain().await {
            errors.push(error);
        }
        Ok(errors)
    }
}
impl Backend for Service {
    fn status(&self) -> ReplyFuture {
        let this = self.clone();
        // Pin admission occurs before spawning, so shutdown cannot miss an
        // accepted task whose observer has already disconnected.
        let pin = self.pin();
        Box::pin(async move {
            let pin = match pin {
                Ok(pin) => pin,
                Err(_) => return crate::json_response(StatusCode::OK, &this.display()),
            };
            tokio::spawn(async move {
                let _pin = pin;
                this.status_owned().await
            })
            .await
            .unwrap_or_else(|_| unavailable())
        })
    }
    fn execute(&self, action: Action, value: String) -> ReplyFuture {
        let this = self.clone();
        let pin = self.pin();
        Box::pin(async move {
            let pin = match pin {
                Ok(pin) => pin,
                Err(reply) => return reply,
            };
            let (probe_observer, mut probe_pending) = tokio::sync::watch::channel(None);
            let mut operation = tokio::spawn(async move {
                let _pin = pin;
                // Task-local observation binds only probes issued by this
                // execute_owned command. A queued command cannot inherit the
                // pending state of the command currently owning the lane.
                crate::call_mode_adb::observe_command_probe(
                    probe_observer,
                    this.execute_owned(action, value),
                )
                .await
            });
            tokio::select! {
                result = &mut operation => result.unwrap_or_else(|_| unavailable()),
                changed = probe_pending.changed() => {
                    let pending = if changed.is_ok() { probe_pending.borrow().clone() } else { None };
                    if let Some(pending) = pending {
                        // The supervisor latches its primary failure before
                        // publishing. Later reap can only finish that failure;
                        // it cannot resume Dial/Answer as a hidden late action.
                        crate::json_response(StatusCode::SERVICE_UNAVAILABLE,
                            &json!({"error":pending.message,"code":pending.code,"recoverable":false}))
                    } else {
                        operation.await.unwrap_or_else(|_| unavailable())
                    }
                }
            }
        })
    }
}
fn admission_error(action: Action, state: &Snapshot) -> Option<Reply> {
    let name = state
        .display
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let admitted = match action {
        Action::Dial => name == "idle" && state.safe_idle && !state.pending_dial,
        Action::Answer => name == "incoming",
        Action::Dtmf => name == "active",
        Action::Hangup => true,
    };
    (!admitted).then(|| state_error(action, state))
}
fn state_error(action: Action, state: &Snapshot) -> Reply {
    let name = state
        .display
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let (code, error) = match action {
        Action::Dial => (
            "call_not_idle",
            format!("当前通话状态为 {name}，不能发起新的呼叫"),
        ),
        Action::Answer => (
            "no_incoming_call",
            format!("当前通话状态为 {name}，没有可接听的来电"),
        ),
        Action::Dtmf => ("call_not_active", "只有通话接通后才能发送按键".into()),
        Action::Hangup => ("call_state_changed", "通话状态已变化，请重新确认".into()),
    };
    crate::json_response(
        StatusCode::CONFLICT,
        &json!({"error":error,"code":code,"recoverable":true}),
    )
}
fn unavailable() -> Reply {
    crate::json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &json!({"error":"模块通话正在切换或退出，请稍后重试","code":"module_call_unavailable","recoverable":true}),
    )
}
fn unknown() -> Value {
    json!({"state":"unknown","incoming":false,"active":false})
}

impl Backend for Result<Service, String> {
    fn status(&self) -> ReplyFuture {
        match self {
            Ok(service) => service.status(),
            Err(error) => failed_initialization(error.clone()),
        }
    }
    fn execute(&self, action: Action, value: String) -> ReplyFuture {
        match self {
            Ok(service) => service.execute(action, value),
            Err(error) => failed_initialization(error.clone()),
        }
    }
}
fn failed_initialization(error: String) -> ReplyFuture {
    Box::pin(async move {
        crate::json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"error":error,"code":"module_call_unavailable","recoverable":true}),
        )
    })
}

#[cfg(test)]
mod tests;
