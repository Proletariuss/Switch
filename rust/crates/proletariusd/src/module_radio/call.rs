//! Cellular call ownership uses the same radio gate and physical workers as RF
//! policy. A dropped HTTP waiter never drops an admitted physical transaction.
use super::*;
use crate::support_diagnostics::module::{Code, ResultKind, Stage};

pub(crate) type CallAdmission = Arc<dyn Fn() -> bool + Send + Sync>;
pub(crate) type CallCapture = Arc<dyn Fn(Binding, &str) -> bool + Send + Sync>;
pub(crate) type CallObserver = Arc<dyn Fn(CallEvent) -> Result<(), String> + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallAction {
    Dial,
    Answer,
    Hangup,
    Dtmf,
}
#[derive(Clone, Debug)]
pub(crate) struct CallReservation {
    pub(crate) producer_id: String,
    pub(crate) token: String,
    pub(crate) sequence: u64,
    pub(crate) epoch: u64,
    pub(crate) binding: Binding,
    purpose: Purpose,
    owned: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CallStateSnapshot {
    pub(crate) response: String,
    pub(crate) display: Value,
    pub(crate) safe_idle: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct CallEvent {
    pub(crate) producer_id: String,
    pub(crate) epoch: u64,
    pub(crate) binding: Binding,
    pub(crate) sequence: u64,
    pub(crate) observed_at: String,
    pub(crate) kind: String,
    pub(crate) number: String,
}
pub(super) struct CallRequest {
    pub(super) lease: CallReservation,
    pub(super) action: Action,
}
impl CallReservation {
    #[cfg(test)]
    pub(crate) fn for_test(
        producer_id: String,
        token: String,
        sequence: u64,
        epoch: u64,
        binding: Binding,
    ) -> Self {
        Self {
            producer_id,
            token,
            sequence,
            epoch,
            binding,
            purpose: Purpose::Call,
            owned: true,
        }
    }
    fn request(&self, action: &str) -> ReservationRequest {
        ReservationRequest {
            producer_id: self.producer_id.clone(),
            action: action.into(),
            purpose: self.purpose,
            token: self.token.clone(),
            sequence: self.sequence,
        }
    }
}
impl Service {
    pub(crate) fn current_call_binding(&self) -> Option<Binding> {
        let binding = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.retiring {
                return None;
            }
            state.binding.clone()?
        };
        #[cfg(test)]
        if self.0.physical_fixture.lock().unwrap().is_some() {
            return Some(binding);
        }
        let current = match &binding {
            Binding::Serial { serial_session } => {
                self.0.modem.call_binding_current(*serial_session)
            }
            Binding::Usb {
                producer_id,
                session_id,
            } => self.0.usb.call_binding_current(producer_id, session_id),
        };
        current.then_some(binding)
    }
    pub(super) fn flush_call_capture(&self) {
        let binding = self
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .binding
            .clone();
        let capture = self
            .0
            .call_capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let (Some(binding), Some(capture)) = (binding, capture) {
            capture(binding, "");
        }
    }
    pub(crate) fn call_reservation_current(&self, lease: &CallReservation) -> bool {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let matches = call_lease_matches(&state, lease);
        drop(state);
        matches && self.current_call_binding().as_ref() == Some(&lease.binding)
    }
    pub(crate) fn call_event_current(&self, event: &CallEvent) -> bool {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let matches = !state.retiring
            && state.epoch == event.epoch
            && state.producer == event.producer_id
            && state.binding.as_ref() == Some(&event.binding);
        drop(state);
        matches && self.current_call_binding().as_ref() == Some(&event.binding)
    }
    pub(crate) fn call_observation_error(&self) -> Option<String> {
        self.0
            .call_observation_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn set_call_observer(&self, observer: CallObserver) {
        *self
            .0
            .call_observer
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(observer.clone());
        let (producer, epoch, cancelled) = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                state.producer.clone(),
                state.epoch,
                state.epoch_cancel.clone(),
            )
        };
        // This short lock serializes serial/USB admission and callback sequence.
        // The callback contract forbids physical I/O and archive work.
        let ordered = Arc::new(Mutex::new((
            0_u64,
            std::collections::VecDeque::<CallEvent>::new(),
        )));
        let weak = Arc::downgrade(&self.0);
        let capture: CallCapture = Arc::new(move |binding, text| {
            let urc = call_urc(text);
            let owned = urc.is_some();
            if urc.is_none() && !text.is_empty() {
                return false;
            }
            if cancelled.load(Ordering::Acquire) {
                return urc.is_some();
            }
            let Some(inner) = weak.upgrade() else {
                return urc.is_some();
            };
            let configured = {
                let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.epoch != epoch || state.producer != producer || state.retiring {
                    return urc.is_some();
                }
                if state
                    .binding
                    .as_ref()
                    .is_some_and(|current| current != &binding)
                {
                    return urc.is_some();
                }
                state.binding.is_some()
            };
            let mut ordered = ordered.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((kind, number)) = urc {
                let Some(sequence) = ordered.0.checked_add(1) else {
                    inner
                        .call_observation_error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_or_insert("call_observation_sequence_exhausted".into());
                    return true;
                };
                ordered.0 = sequence;
                let event = CallEvent {
                    producer_id: producer.clone(),
                    epoch,
                    binding: binding.clone(),
                    sequence,
                    observed_at: time::OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_default(),
                    kind: kind.into(),
                    number,
                };
                if ordered.1.len() >= 128 {
                    inner
                        .call_observation_error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_or_insert("call_observation_admission_full".into());
                    return true;
                }
                ordered.1.push_back(event);
            }
            if configured {
                while let Some(event) = ordered.1.front().cloned() {
                    if event.binding != binding {
                        ordered.1.pop_front();
                        continue;
                    }
                    if let Err(error) = observer(event) {
                        inner
                            .call_observation_error
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .get_or_insert(error);
                        break; // Retain the exact unacknowledged head and its timestamp.
                    }
                    ordered.1.pop_front();
                }
            }
            owned
        });
        *self
            .0
            .call_capture
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(capture.clone());
        self.0.modem.set_call_capture(capture.clone());
        self.0.usb.set_call_capture(capture);
        // Poll only an already-open owner. Every read is an ordinary radio job;
        // retirement waits for its actual USB transfer and callback to finish.
        let mut task = self
            .0
            .call_observation_task
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if task.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        *task = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let eligible = {
                    let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.stopping {
                        return;
                    }
                    !state.retiring && state.binding.is_some()
                };
                if eligible {
                    let _ = Service(inner).submit(Job::CallPoll).await;
                }
            }
        }));
    }
    pub(crate) async fn reserve_call(&self) -> Result<CallReservation, Failure> {
        let service = self.clone();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let started = Instant::now();
            let result = service.reserve_call_inner().await;
            service.record_call_result(Stage::ReservationAcquire, &result, started.elapsed());
            if let Err(Ok(lease)) = send.send(result) {
                while service.release_call(&lease).await.is_err() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        });
        receive.await.map_err(|_| Failure::stopped())?
    }
    async fn reserve_call_inner(&self) -> Result<CallReservation, Failure> {
        let producer = self
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .producer
            .clone();
        // Resolve/open a native binding, never a Go cache or an RPC loop.
        self.submit(Job::CallModeOpen(Producer {
            producer_id: producer,
        }))
        .await?;
        let lease = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            CallReservation {
                producer_id: state.producer.clone(),
                epoch: state.epoch,
                binding: state.binding.clone().ok_or_else(Failure::unavailable)?,
                token: crate::multistandby_store::random_token()
                    .map_err(|_| Failure::unavailable())?,
                sequence: state
                    .waters
                    .get(&Purpose::Call)
                    .map_or(0, |water| water.sequence)
                    .checked_add(1)
                    .ok_or_else(Failure::stale)?,
                purpose: Purpose::Call,
                owned: true,
            }
        };
        let ack = self
            .submit(Job::Reservation(lease.request("acquire")))
            .await?;
        if ack.get("token").and_then(Value::as_str) != Some(&lease.token)
            || ack.get("sequence").and_then(Value::as_u64) != Some(lease.sequence)
            || ack.get("released").and_then(Value::as_bool) != Some(false)
        {
            while self.release_call(&lease).await.is_err() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            return Err(Failure::unavailable());
        }
        Ok(lease)
    }
    pub(crate) async fn release_call(&self, lease: &CallReservation) -> Result<(), Failure> {
        let started = Instant::now();
        let result = self.release_call_inner(lease).await;
        // A borrowed read lease has no physical release obligation to report.
        if lease.owned {
            self.record_call_result(Stage::ReservationRelease, &result, started.elapsed());
        }
        result
    }
    async fn release_call_inner(&self, lease: &CallReservation) -> Result<(), Failure> {
        if !lease.owned {
            return Ok(());
        }
        let proof = (
            lease.epoch,
            lease.producer_id.clone(),
            lease.token.clone(),
            lease.sequence,
        );
        {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let proofs = if lease.purpose == Purpose::Call {
                &state.confirmed_call_releases
            } else {
                &state.confirmed_call_mode_releases
            };
            if proofs.contains(&proof) {
                return Ok(());
            }
            if state.epoch == lease.epoch
                && !state.waters.get(&lease.purpose).is_some_and(|water| {
                    water.token == lease.token && water.sequence == lease.sequence
                })
            {
                return Err(Failure::stale());
            }
        }
        self.fence(lease.epoch)?;
        let ack = self
            .submit(Job::Reservation(lease.request("release")))
            .await?;
        if ack.get("token").and_then(Value::as_str) != Some(&lease.token)
            || ack.get("sequence").and_then(Value::as_u64) != Some(lease.sequence)
            || ack.get("released").and_then(Value::as_bool) != Some(true)
        {
            return Err(Failure::unavailable());
        }
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if lease.purpose == Purpose::Call {
            state.confirmed_call_releases.insert(proof);
        } else {
            state.confirmed_call_mode_releases.insert(proof);
        }
        Ok(())
    }
    pub(crate) async fn query_call_state(
        &self,
        lease: Option<&CallReservation>,
    ) -> Result<CallStateSnapshot, Failure> {
        let service = self.clone();
        let lease = lease.cloned();
        let (send, receive) = oneshot::channel();
        // Includes acquisition, query and acknowledged release. Cancellation
        // cannot strand a temporary read reservation between these awaits.
        tokio::spawn(async move {
            let _ = send.send(service.query_call_state_inner(lease).await);
        });
        receive.await.map_err(|_| Failure::stopped())?
    }
    async fn query_call_state_inner(
        &self,
        lease: Option<CallReservation>,
    ) -> Result<CallStateSnapshot, Failure> {
        let started = Instant::now();
        let (borrowed, owned) = if let Some(lease) = lease {
            (lease, false)
        } else {
            let read = match self.reserve_call_mode_read().await {
                Ok(read) => read,
                Err(error) => {
                    let result = Err(error);
                    self.record_call_query(Stage::CallQueryAdmission, &result, started.elapsed());
                    return result;
                }
            };
            let binding = self
                .0
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .binding
                .clone();
            let Some(binding) = binding else {
                self.record_module_snapshot(
                    Stage::CallQueryAdmission, ResultKind::Failed, Code::BindingMissing,
                    self.module_diagnostic_snapshot(), started.elapsed(),
                );
                while self.release_call_mode_operation(&read).await.is_err() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                return Err(Failure::unavailable());
            };
            (
                CallReservation {
                    producer_id: read.producer_id,
                    token: read.token,
                    sequence: read.sequence,
                    epoch: read.epoch,
                    binding,
                    purpose: read.purpose,
                    owned: read.owned,
                },
                read.owned,
            )
        };
        let submitted = self
            .submit_with_receipt(Job::CellularCall(CallRequest {
                lease: borrowed.clone(),
                action: Action::CellularCallRead,
            }))
            .await;
        // Admission here means the actor accepted the job; it does not prove
        // physical I/O occurred. A successful snapshot is the read completion.
        let stage = match &submitted {
            Err(error) if !error.admitted => Stage::CallQueryAdmission,
            _ => Stage::CallQueryPhysical,
        };
        let result = submitted
            .map_err(|error| error.failure)
            .and_then(|value| serde_json::from_value(value).map_err(|_| Failure::unavailable()));
        self.record_call_query(stage, &result, started.elapsed());
        if owned {
            while self.release_call(&borrowed).await.is_err() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        result
    }
    pub(crate) async fn execute_call(
        &self,
        lease: &CallReservation,
        action: CallAction,
        value: &str,
        admission: CallAdmission,
    ) -> Result<String, Failure> {
        let action = Action::CellularCall(crate::module_radio_transport::CallCommand {
            action,
            value: value.into(),
            admission,
        });
        action.validate().map_err(Failure::from)?;
        let value = self
            .submit(Job::CellularCall(CallRequest {
                lease: lease.clone(),
                action,
            }))
            .await?;
        value
            .get("response")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(Failure::unavailable)
    }
    pub(super) async fn cellular_call_inner(
        &self,
        input: CallRequest,
        epoch: u64,
    ) -> Result<Value, Failure> {
        {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if !call_lease_matches(&state, &input.lease) {
                return Err(Failure::stale());
            }
            if matches!(&input.action, Action::CellularCall(command) if matches!(command.action,CallAction::Dial|CallAction::Answer|CallAction::Dtmf))
                && suppressed(&state)
            {
                return Err(blocked());
            }
        }
        let read = matches!(input.action, Action::CellularCallRead);
        let outcome = self.physical(input.action, epoch).await?;
        if !(outcome.complete && outcome.verified) {
            return Err(outcome_failure(&outcome, "module_call_unconfirmed"));
        }
        if read {
            serde_json::to_value(call_snapshot(outcome.response))
                .map_err(|_| Failure::unavailable())
        } else {
            Ok(json!({"response":outcome.response}))
        }
    }
    pub(super) async fn poll_call_capture(&self, epoch: u64) -> Result<Value, Failure> {
        self.flush_call_capture();
        let binding = self
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .binding
            .clone();
        if let Some(Binding::Usb {
            producer_id,
            session_id,
        }) = binding
        {
            self.0
                .usb
                .poll_call_capture(producer_id, session_id)
                .await
                .map_err(Failure::from)?;
            self.fence(epoch)?;
        }
        Ok(json!({"observed":true}))
    }
}
pub(super) fn call_lease_matches(state: &State, lease: &CallReservation) -> bool {
    !state.retiring
        && state.epoch == lease.epoch
        && state.producer == lease.producer_id
        && state.binding.as_ref() == Some(&lease.binding)
        && state.reservation.as_ref().is_some_and(|reservation| {
            reservation.purpose == lease.purpose
                && reservation.identity.token == lease.token
                && reservation.identity.sequence == lease.sequence
        })
}
pub(crate) fn call_snapshot(response: String) -> CallStateSnapshot {
    let (display, safe_idle) = crate::modem_actor::project_call_observation(&response);
    CallStateSnapshot {
        response,
        display,
        safe_idle,
    }
}

fn call_urc(text: &str) -> Option<(&'static str, String)> {
    let line = text.trim();
    match line {
        "RING" => Some(("ring", String::new())),
        "NO CARRIER" => Some(("hangup", String::new())),
        "CONNECT" | "MO CONNECTED" => Some(("connect", String::new())),
        _ if line.starts_with("CONNECT ") => Some(("connect", String::new())),
        _ if line.starts_with("+CLIP:") => {
            let number = line
                .strip_prefix("+CLIP:")?
                .trim()
                .split(',')
                .next()?
                .trim()
                .trim_matches('"');
            Some(("clip", number.chars().take(512).collect()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        Service,
        tokio::sync::mpsc::UnboundedReceiver<(Action, oneshot::Sender<Result<Outcome, Failure>>)>,
    ) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/cellular-call-tests")
            .join(crate::snapshot::new_instance_id());
        let gate = crate::module_recovery::Gate::default();
        let service = Service::new(
            root,
            crate::usb_at::Owner::new("cs-producer".into())
                .with_recovery_gate_for_tests(gate.clone()),
            crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap(),
            BackendAvailability::with_module_recovery(gate),
            "cs-producer".into(),
        );
        service
            .0
            .store
            .save(Preferences {
                user_configured: true,
                ..Preferences::default()
            })
            .unwrap();
        service.0.state.lock().unwrap().observed = Outcome {
            complete: true,
            verified: true,
            cfun: Some(1),
            ..Outcome::default()
        };
        service.0.state.lock().unwrap().binding = Some(Binding::Serial { serial_session: 7 });
        let (send, receive) = tokio::sync::mpsc::unbounded_channel();
        *service.0.physical_fixture.lock().unwrap() = Some(send);
        (service, receive)
    }
    #[tokio::test]
    async fn live_revoke_rejects_new_call_business_but_exact_hangup_and_release_settle() {
        let (service, mut requests) = fixture();
        let lease = service.reserve_call().await.unwrap();
        service.module_recovery().revoke_for_tests();
        for (action, value) in [
            (CallAction::Dial, "123"),
            (CallAction::Answer, ""),
            (CallAction::Dtmf, "1"),
        ] {
            let failure = service
                .execute_call(&lease, action, value, Arc::new(|| true))
                .await
                .unwrap_err();
            assert_eq!(failure.code, crate::module_recovery::CODE);
        }
        let worker = service.clone();
        let accepted = lease.clone();
        let hangup = tokio::spawn(async move {
            worker
                .execute_call(&accepted, CallAction::Hangup, "", Arc::new(|| true))
                .await
        });
        let (action, reply) = tokio::time::timeout(Duration::from_secs(3), requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(action, Action::CellularCall(command) if command.action == CallAction::Hangup)
        );
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                response: "OK".into(),
                ..Outcome::default()
            }))
            .unwrap();
        assert_eq!(hangup.await.unwrap().unwrap(), "OK");
        let mut wrong_epoch = lease.clone();
        wrong_epoch.epoch += 1;
        assert!(
            service
                .execute_call(&wrong_epoch, CallAction::Hangup, "", Arc::new(|| true))
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(3), service.release_call(&lease))
            .await
            .unwrap()
            .unwrap();
        service.release_call(&lease).await.unwrap();
        assert!(requests.try_recv().is_err());
        assert!(service.0.state.lock().unwrap().reservation.is_none());
        service.begin_shutdown();
        service.0.modem.shutdown().unwrap();
        let _ = std::fs::remove_dir_all(&service.0.data_root);
    }
    #[test]
    fn strict_idle_never_uses_lossy_display_projection() {
        assert!(call_snapshot("AT+CLCC\r\nOK".into()).safe_idle);
        for text in [
            "+CLCC: 1,0,1,0,0,\"123\"\r\nOK",
            "+CLCC: 1,0,0,0,0\r\nOK",
            "+CLCC: broken\r\nOK",
            "+CLCC invalid\r\nOK",
            "ERROR\r\nOK",
            "RING\r\nOK",
            "OK\r\nRING",
            "",
        ] {
            assert!(!call_snapshot(text.into()).safe_idle, "{text}");
        }
    }
    #[tokio::test]
    async fn first_ring_waits_for_matching_binding_and_keeps_original_order() {
        let (service, _) = fixture();
        service.0.state.lock().unwrap().binding = None;
        let events = Arc::new(Mutex::new(Vec::new()));
        let records = events.clone();
        service.set_call_observer(Arc::new(move |event| {
            records.lock().unwrap().push(event);
            Ok(())
        }));
        let capture = service.0.call_capture.lock().unwrap().clone().unwrap();
        capture(Binding::Serial { serial_session: 88 }, "RING");
        capture(Binding::Serial { serial_session: 7 }, "RING");
        capture(Binding::Serial { serial_session: 7 }, "+CLIP: \"123\",129");
        assert!(events.lock().unwrap().is_empty());
        service.0.state.lock().unwrap().binding = Some(Binding::Serial { serial_session: 7 });
        service.flush_call_capture();
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].sequence, 2);
        assert_eq!(events[1].sequence, 3);
        assert_eq!(events[0].binding, Binding::Serial { serial_session: 7 });
        assert!(!events[0].observed_at.is_empty());
    }
    #[tokio::test]
    async fn call_reservation_fences_binding_and_release_is_exact_and_idempotent() {
        let (service, mut physical) = fixture();
        let lease = service.reserve_call().await.unwrap();
        assert!(service.call_reservation_current(&lease));
        let mut stale = lease.clone();
        stale.binding = Binding::Serial { serial_session: 8 };
        assert!(!service.call_reservation_current(&stale));
        assert_eq!(
            service
                .execute_call(&stale, CallAction::Hangup, "", Arc::new(|| true))
                .await
                .unwrap_err()
                .code,
            "radio_stale_generation"
        );
        assert!(physical.try_recv().is_err());
        service.release_call(&lease).await.unwrap();
        service.release_call(&lease).await.unwrap();
        assert!(!service.call_reservation_current(&lease));
        // A different purpose cannot consume a Call release proof.
        let mut wrong = lease;
        wrong.purpose = Purpose::CallMode;
        assert!(service.release_call(&wrong).await.is_err());
    }
    #[tokio::test]
    async fn dropped_call_waiter_keeps_lease_until_actual_worker_finishes() {
        let (service, mut physical) = fixture();
        let lease = service.reserve_call().await.unwrap();
        let worker_service = service.clone();
        let worker_lease = lease.clone();
        let waiter = tokio::spawn(async move {
            worker_service
                .execute_call(&worker_lease, CallAction::Hangup, "", Arc::new(|| true))
                .await
        });
        let (action, reply) = physical.recv().await.unwrap();
        assert!(matches!(action, Action::CellularCall(_)));
        waiter.abort();
        let releasing = {
            let service = service.clone();
            let lease = lease.clone();
            tokio::spawn(async move { service.release_call(&lease).await })
        };
        tokio::task::yield_now().await;
        assert!(service.call_reservation_current(&lease));
        assert!(!releasing.is_finished());
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                submitted: true,
                response: "OK".into(),
                ..Outcome::default()
            }))
            .unwrap();
        releasing.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn observer_is_ordered_reentrant_sticky_and_rebound_after_retirement() {
        let (service, _) = fixture();
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let checking = service.clone();
        let reject = Arc::new(AtomicBool::new(true));
        service.set_call_observer(Arc::new(move |event| {
            assert!(checking.call_event_current(&event));
            if reject.swap(false, Ordering::AcqRel) {
                return Err("fixture admission failure".into());
            }
            recorded.lock().unwrap().push(event);
            Ok(())
        }));
        let old = service.0.call_capture.lock().unwrap().clone().unwrap();
        let binding = Binding::Serial { serial_session: 7 };
        assert!(old(binding.clone(), "RING"));
        assert!(old(binding.clone(), "+CLIP: \"123\",129"));
        assert!(!old(binding.clone(), "BUSY"));
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            service.call_observation_error().as_deref(),
            Some("fixture admission failure")
        );
        service.retire_producer("replacement".into()).await.unwrap();
        service.0.state.lock().unwrap().binding = Some(Binding::Serial { serial_session: 9 });
        old(binding, "RING");
        let current = service.0.call_capture.lock().unwrap().clone().unwrap();
        current(Binding::Serial { serial_session: 9 }, "MO CONNECTED");
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[2].producer_id, "replacement");
        assert_eq!(events[2].epoch, 1);
    }
    #[tokio::test]
    async fn retired_call_release_requires_real_worker_drain_proof() {
        let (service, mut physical) = fixture();
        let lease = service.reserve_call().await.unwrap();
        let worker = {
            let service = service.clone();
            let lease = lease.clone();
            tokio::spawn(async move {
                service
                    .execute_call(&lease, CallAction::Hangup, "", Arc::new(|| true))
                    .await
            })
        };
        let (_, reply) = physical.recv().await.unwrap();
        let retiring = {
            let service = service.clone();
            tokio::spawn(async move { service.retire_producer("new".into()).await })
        };
        tokio::task::yield_now().await;
        assert!(service.release_call(&lease).await.is_err());
        assert!(!retiring.is_finished());
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                submitted: true,
                ..Outcome::default()
            }))
            .unwrap();
        assert!(worker.await.unwrap().unwrap_err().public_submitted());
        retiring.await.unwrap().unwrap();
        service.release_call(&lease).await.unwrap();
    }

    fn diagnostic_events(
        receiver: &mut tokio::sync::mpsc::Receiver<crate::support_diagnostics::module::Event>,
    ) -> Vec<crate::support_diagnostics::module::Event> {
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() { events.push(event); }
        events
    }
    fn finish_diagnostic_fixture(service: &Service) {
        service.begin_shutdown();
        service.0.modem.shutdown().unwrap();
        let _ = std::fs::remove_dir_all(&service.0.data_root);
    }

    #[tokio::test]
    async fn call_diagnostics_use_typed_snapshot_and_exact_lease_results_without_private_text() {
        use crate::support_diagnostics::module::Owner;
        let (service, mut requests) = fixture();
        let mut events = service.module_diagnostics_for_test();
        let lease = service.reserve_call().await.unwrap();
        let acquired = diagnostic_events(&mut events);
        assert!(acquired.iter().any(|event| event.stage == Stage::ReservationAcquire
            && event.result == ResultKind::Succeeded && event.owner.owner == Owner::Call));

        for (response, expected) in [
            ("+CLCC: 1,0,0,1,0,\"\",128\r\nOK", Code::CallVoiceIdle),
            ("+CLCC: 1,0,0,0,0,\"private-number\",129\r\nOK", Code::CallVoicePresent),
            ("+CLCC: broken\r\nOK", Code::CallUnconfirmed),
        ] {
            let worker = service.clone();
            let current = lease.clone();
            let query = tokio::spawn(async move { worker.query_call_state(Some(&current)).await });
            let (action, reply) = tokio::time::timeout(Duration::from_secs(3), requests.recv())
                .await.unwrap().unwrap();
            assert!(matches!(action, Action::CellularCallRead));
            reply.send(Ok(Outcome { complete: true, verified: true, response: response.into(),
                ..Outcome::default() })).unwrap();
            let snapshot = query.await.unwrap().unwrap();
            assert_eq!(snapshot.safe_idle, expected == Code::CallVoiceIdle);
            let records = diagnostic_events(&mut events);
            let evidence = records.iter().find(|event| event.stage == Stage::CallQueryPhysical).unwrap();
            assert_eq!(evidence.result, ResultKind::Succeeded);
            assert_eq!(evidence.code, expected);
            assert_eq!(evidence.owner.owner, Owner::Call);
            let encoded = serde_json::to_string(evidence).unwrap();
            for secret in ["private-number", "+CLCC", lease.token.as_str(), "response", "number"] {
                assert!(!encoded.contains(secret), "{encoded}");
            }
        }

        let mut stale = lease.clone();
        stale.sequence += 1;
        assert!(service.release_call(&stale).await.is_err());
        let failed = diagnostic_events(&mut events);
        assert!(failed.iter().any(|event| event.stage == Stage::ReservationRelease
            && event.result == ResultKind::Failed && event.code == Code::Stale
            && event.owner.owner == Owner::Call));
        assert!(service.call_reservation_current(&lease));
        service.release_call(&lease).await.unwrap();
        let released = diagnostic_events(&mut events);
        assert!(released.iter().any(|event| event.stage == Stage::ReservationRelease
            && event.result == ResultKind::Succeeded && event.owner.owner == Owner::None));
        assert!(requests.try_recv().is_err());
        finish_diagnostic_fixture(&service);
    }

    #[tokio::test]
    async fn call_diagnostics_capture_conflicting_owner_and_query_admission_without_io() {
        use crate::support_diagnostics::module::Owner;
        let (service, mut requests) = fixture();
        let lease = service.reserve_esim().await.unwrap();
        let mut events = service.module_diagnostics_for_test();
        assert_eq!(service.query_call_state(None).await.unwrap_err().code, "module_radio_busy");
        assert_eq!(service.reserve_call().await.unwrap_err().code, "module_radio_busy");
        let records = diagnostic_events(&mut events);
        assert!(records.iter().any(|event| event.stage == Stage::Admission
            && event.result == ResultKind::Deferred && event.code == Code::ReservationConflict
            && event.owner.owner == Owner::Esim));
        assert!(records.iter().any(|event| event.stage == Stage::CallQueryAdmission
            && event.result == ResultKind::Failed && event.code == Code::Busy
            && event.owner.owner == Owner::Esim));
        assert!(records.iter().any(|event| event.stage == Stage::ReservationAcquire
            && event.result == ResultKind::Failed && event.code == Code::Busy
            && event.owner.owner == Owner::Esim));
        assert!(!records.iter().any(|event| event.stage == Stage::CallQueryPhysical));
        assert!(requests.try_recv().is_err());
        service.release_esim(&lease).await.unwrap();
        finish_diagnostic_fixture(&service);
    }

    #[tokio::test]
    async fn call_diagnostics_do_not_call_actor_admission_physical_io_or_success() {
        let (service, mut requests) = fixture();
        let lease = service.reserve_call().await.unwrap();
        let mut events = service.module_diagnostics_for_test();
        let mut stale = lease.clone();
        stale.epoch += 1;
        assert_eq!(service.query_call_state(Some(&stale)).await.unwrap_err().code,
            "radio_stale_generation");
        let records = diagnostic_events(&mut events);
        assert!(records.iter().any(|event| event.stage == Stage::CallQueryPhysical
            && event.result == ResultKind::Failed && event.code == Code::Stale));
        // The actor accepted the job, but rejected its lease before physical I/O.
        assert!(requests.try_recv().is_err());
        service.release_call(&lease).await.unwrap();
        diagnostic_events(&mut events);
        service.begin_shutdown();
        assert!(service.query_call_state(Some(&lease)).await.is_err());
        let records = diagnostic_events(&mut events);
        assert!(records.iter().any(|event| event.stage == Stage::CallQueryAdmission
            && event.result == ResultKind::Failed));
        assert!(!records.iter().any(|event| event.stage == Stage::CallQueryPhysical));
        assert!(requests.try_recv().is_err());
        finish_diagnostic_fixture(&service);
    }

    #[tokio::test]
    async fn cancelled_call_query_keeps_completion_evidence_and_temporary_release() {
        let (service, mut requests) = fixture();
        let mut events = service.module_diagnostics_for_test();
        let worker = service.clone();
        let query = tokio::spawn(async move { worker.query_call_state(None).await });
        let (action, reply) = tokio::time::timeout(Duration::from_secs(3), requests.recv())
            .await.unwrap().unwrap();
        assert!(matches!(action, Action::CellularCallRead));
        query.abort();
        assert!(query.await.unwrap_err().is_cancelled());
        reply.send(Ok(Outcome { complete: true, verified: true,
            response: "+CLCC: 1,0,0,1,0,\"\",128\r\nOK".into(),
            ..Outcome::default() })).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut completed = false;
            let mut released = false;
            while !(completed && released) {
                let event = events.recv().await.unwrap();
                completed |= event.stage == Stage::CallQueryPhysical
                    && event.result == ResultKind::Succeeded && event.code == Code::CallVoiceIdle;
                released |= event.stage == Stage::ReservationRelease && event.result == ResultKind::Succeeded;
            }
        }).await.unwrap();
        assert!(service.0.state.lock().unwrap().reservation.is_none());
        let next = service.reserve_call().await.unwrap();
        service.release_call(&next).await.unwrap();
        assert!(requests.try_recv().is_err());
        finish_diagnostic_fixture(&service);
    }

    #[tokio::test]
    async fn saturated_call_diagnostic_channel_does_not_delay_lease_release() {
        let (service, mut requests) = fixture();
        let lease = service.reserve_call().await.unwrap();
        let mut events = service.module_diagnostics_for_test();
        for _ in 0..128 {
            service.record_call_result(Stage::ReservationAcquire, &Ok::<_, Failure>(()), Duration::ZERO);
        }
        tokio::time::timeout(Duration::from_secs(3), service.release_call(&lease))
            .await.unwrap().unwrap();
        assert!(!service.call_reservation_current(&lease));
        assert_eq!(diagnostic_events(&mut events).len(), 64);
        assert!(requests.try_recv().is_err());
        finish_diagnostic_fixture(&service);
    }
}
