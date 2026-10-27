use super::*;
use crate::module_radio_transport::Binding;
use http_body_util::BodyExt;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;

type Observer = Arc<dyn Fn(CallEvent) -> Result<(), String> + Send + Sync>;
#[derive(Clone)]
struct Fake(Arc<FakeInner>);
struct FakeInner {
    observer: ShortMutex<Option<Observer>>,
    queries: ShortMutex<VecDeque<CallStateSnapshot>>,
    query_leases: ShortMutex<Vec<Option<CallReservation>>>,
    fail_next_query: AtomicBool,
    default_state: ShortMutex<CallStateSnapshot>,
    actions: ShortMutex<Vec<(Action, String)>>,
    releases: ShortMutex<Vec<String>>,
    reserve_count: AtomicUsize,
    audio_stops: AtomicUsize,
    execute_wait: AtomicBool,
    query_wait: AtomicBool,
    ready_wait: AtomicBool,
    child_probe: ShortMutex<Option<crate::call_mode_adb::child_tests::HeldProbe>>,
    execute_entered: Semaphore,
    execute_resume: Semaphore,
    query_entered: Semaphore,
    query_resume: Semaphore,
    ready_entered: Semaphore,
    ready_resume: Semaphore,
    fail_command: AtomicBool,
    release_failures: AtomicUsize,
    event_on_execute: ShortMutex<Option<&'static str>>,
    sequence: AtomicUsize,
}
fn snapshot(state: &str, safe_idle: bool) -> CallStateSnapshot {
    CallStateSnapshot {
        response: "OK".into(),
        display: json!({"state":state,"incoming":state=="incoming","active":state=="active"}),
        safe_idle,
    }
}
impl Fake {
    fn new() -> Self {
        Self(Arc::new(FakeInner {
            observer: Default::default(),
            queries: Default::default(),
            query_leases: Default::default(),
            fail_next_query: AtomicBool::new(false),
            default_state: ShortMutex::new(snapshot("idle", true)),
            actions: Default::default(),
            releases: Default::default(),
            reserve_count: AtomicUsize::new(0),
            audio_stops: AtomicUsize::new(0),
            execute_wait: AtomicBool::new(false),
            query_wait: AtomicBool::new(false),
            ready_wait: AtomicBool::new(false),
            child_probe: ShortMutex::new(None),
            execute_entered: Semaphore::new(0),
            execute_resume: Semaphore::new(0),
            query_entered: Semaphore::new(0),
            query_resume: Semaphore::new(0),
            ready_entered: Semaphore::new(0),
            ready_resume: Semaphore::new(0),
            fail_command: AtomicBool::new(false),
            release_failures: AtomicUsize::new(0),
            event_on_execute: Default::default(),
            sequence: AtomicUsize::new(0),
        }))
    }
    fn emit(&self, kind: &str) {
        let event = CallEvent {
            producer_id: "physical".into(),
            epoch: 1,
            binding: Binding::Serial { serial_session: 1 },
            sequence: self.0.sequence.fetch_add(1, Ordering::AcqRel) as u64 + 1,
            observed_at: OffsetDateTime::now_utc().format(&Rfc3339).unwrap(),
            kind: kind.into(),
            number: String::new(),
        };
        self.0.observer.lock().unwrap().as_ref().unwrap()(event).unwrap();
    }
}
impl Driver for Fake {
    fn reserve(&self) -> Work<Result<CallReservation, Failure>> {
        let this = self.clone();
        Box::pin(async move {
            let n = this.0.reserve_count.fetch_add(1, Ordering::AcqRel) as u64 + 1;
            Ok(CallReservation::for_test(
                "physical".into(),
                format!("lease-{n}"),
                n,
                1,
                Binding::Serial { serial_session: 1 },
            ))
        })
    }
    fn release(&self, lease: CallReservation) -> Work<Result<(), Failure>> {
        let this = self.clone();
        Box::pin(async move {
            this.0.releases.lock().unwrap().push(lease.token);
            if this
                .0
                .release_failures
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_sub(1))
                .is_ok()
            {
                Err(Failure {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    code: "cleanup_pending".into(),
                    message: "test release unconfirmed".into(),
                })
            } else {
                Ok(())
            }
        })
    }
    fn query(&self, lease: Option<CallReservation>) -> Work<Result<CallStateSnapshot, Failure>> {
        let this = self.clone();
        Box::pin(async move {
            this.0.query_leases.lock().unwrap().push(lease);
            let fail = this.0.fail_next_query.swap(false, Ordering::AcqRel);
            let result = this
                .0
                .queries
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| this.0.default_state.lock().unwrap().clone());
            if this.0.query_wait.swap(false, Ordering::AcqRel) {
                this.0.query_entered.add_permits(1);
                this.0.query_resume.acquire().await.unwrap().forget();
            }
            if fail {
                return Err(Failure {
                    status: StatusCode::GATEWAY_TIMEOUT,
                    code: "query_unconfirmed".into(),
                    message: "test query failed".into(),
                });
            }
            Ok(result)
        })
    }
    fn execute(
        &self,
        _: CallReservation,
        action: Action,
        value: String,
        admission: Admission,
    ) -> Work<Result<String, Failure>> {
        let this = self.clone();
        Box::pin(async move {
            if !admission() {
                return Err(Failure {
                    status: StatusCode::CONFLICT,
                    code: "stale".into(),
                    message: "stale submission".into(),
                });
            }
            this.0.actions.lock().unwrap().push((action, value));
            let event = *this.0.event_on_execute.lock().unwrap();
            if let Some(event) = event {
                this.emit(event);
            }
            if this.0.execute_wait.swap(false, Ordering::AcqRel) {
                this.0.execute_entered.add_permits(1);
                this.0.execute_resume.acquire().await.unwrap().forget();
            }
            if this.0.fail_command.load(Ordering::Acquire) {
                return Err(Failure {
                    status: StatusCode::GATEWAY_TIMEOUT,
                    code: "unknown".into(),
                    message: "submitted result unknown".into(),
                });
            }
            Ok("OK".into())
        })
    }
    fn ready(&self, _: String) -> Work<Result<(), String>> {
        let this = self.clone();
        Box::pin(async move {
            let probe = this.0.child_probe.lock().unwrap().take();
            if let Some(probe) = probe {
                return crate::call_mode_adb::child_tests::run_command_probe(probe).await;
            }
            if this.0.ready_wait.swap(false, Ordering::AcqRel) {
                this.0.ready_entered.add_permits(1);
                this.0.ready_resume.acquire().await.unwrap().forget();
            }
            Ok(())
        })
    }
    fn stop_audio(&self, _: String) -> Work<Result<(), String>> {
        self.0.audio_stops.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Ok(()) })
    }
    fn lease_current(&self, _: &CallReservation) -> bool {
        true
    }
    fn event_current(&self, event: &CallEvent) -> bool {
        event.producer_id == "physical" && event.epoch == 1
    }
    fn binding(&self) -> Option<Binding> {
        Some(Binding::Serial { serial_session: 1 })
    }
    fn observe(&self, observer: Observer) {
        *self.0.observer.lock().unwrap() = Some(observer);
    }
}
fn fixture(name: &str) -> (Service, Fake, PathBuf) {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let root = std::env::temp_dir().join(format!(
        "module-call-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::AcqRel)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let facts = CallFacts::new(
        proletarius_store::ArchiveStore::new(root.join("calls.json")),
        "go-fixture".into(),
    );
    let fake = Fake::new();
    let service = Service::with_driver(Arc::new(fake.clone()), facts).unwrap();
    (service, fake, root)
}
async fn finish(service: Service, root: PathBuf) {
    service.finish_shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
async fn data(reply: Reply) -> Value {
    serde_json::from_slice(&reply.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
async fn signal(semaphore: &Semaphore) {
    tokio::time::timeout(Duration::from_secs(3), semaphore.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
}

#[tokio::test]
async fn unknown_and_unsafe_idle_cannot_authorize_dial() {
    for (name, state) in [
        ("unknown", snapshot("unknown", false)),
        ("held", snapshot("idle", false)),
    ] {
        let (service, fake, root) = fixture(name);
        *fake.0.default_state.lock().unwrap() = state;
        let reply = service.execute(Action::Dial, "123".into()).await;
        assert_eq!(reply.status(), StatusCode::CONFLICT);
        assert_eq!(data(reply).await["code"], "call_not_idle");
        assert!(fake.0.actions.lock().unwrap().is_empty());
        assert_eq!(fake.0.reserve_count.load(Ordering::Acquire), 0);
        finish(service, root).await;
    }
}
#[tokio::test]
async fn inactive_dtmf_and_answer_do_not_submit_commands() {
    let (service, fake, root) = fixture("inactive");
    assert_eq!(
        data(service.execute(Action::Dtmf, "1".into()).await).await["code"],
        "call_not_active"
    );
    assert_eq!(
        data(service.execute(Action::Answer, "".into()).await).await["code"],
        "no_incoming_call"
    );
    assert!(fake.0.actions.lock().unwrap().is_empty());
    finish(service, root).await;
}
#[tokio::test]
async fn idle_hangup_is_an_idempotent_noop() {
    let (service, fake, root) = fixture("idle-hangup");
    assert_eq!(
        data(service.execute(Action::Hangup, "".into()).await).await,
        json!({"accepted":true,"already_idle":true})
    );
    assert!(fake.0.actions.lock().unwrap().is_empty());
    assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 0);
    finish(service, root).await;
}
#[tokio::test]
async fn late_query_cannot_hide_new_ring_from_ui() {
    let (service, fake, root) = fixture("late-query");
    fake.0.query_wait.store(true, Ordering::Release);
    let observer = tokio::spawn(service.status());
    signal(&fake.0.query_entered).await;
    fake.emit("ring");
    fake.0.query_resume.add_permits(1);
    assert_eq!(data(observer.await.unwrap()).await["state"], "incoming");
    finish(service, root).await;
}
#[tokio::test]
async fn incoming_during_readiness_prevents_dial_submission() {
    let (service, fake, root) = fixture("late-ready");
    fake.0.ready_wait.store(true, Ordering::Release);
    let observer = tokio::spawn(service.execute(Action::Dial, "123".into()));
    signal(&fake.0.ready_entered).await;
    fake.emit("ring");
    fake.0.ready_resume.add_permits(1);
    assert_eq!(data(observer.await.unwrap()).await["code"], "call_not_idle");
    assert!(fake.0.actions.lock().unwrap().is_empty());
    finish(service, root).await;
}
#[tokio::test]
async fn late_hangup_reply_does_not_end_or_stop_audio_for_a_new_ring() {
    let (service, fake, root) = fixture("late-hangup");
    *fake.0.default_state.lock().unwrap() = snapshot("active", false);
    *fake.0.event_on_execute.lock().unwrap() = Some("ring");
    assert_eq!(
        service.execute(Action::Hangup, "".into()).await.status(),
        StatusCode::OK
    );
    assert_eq!(service.0.state.snapshot().display["state"], "incoming");
    assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 0);
    finish(service, root).await;
}
#[tokio::test]
async fn unknown_dial_result_observes_without_replaying_and_retains_busy_protection() {
    let (service, fake, root) = fixture("unknown-dial");
    fake.0.queries.lock().unwrap().extend([
        snapshot("idle", true),
        snapshot("idle", true),
        snapshot("active", false),
    ]);
    fake.0.fail_command.store(true, Ordering::Release);
    assert_eq!(
        service.execute(Action::Dial, "123".into()).await.status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(fake.0.actions.lock().unwrap().len(), 1);
    assert!(service.0.state.snapshot().desired_active);
    assert!(service.0.physical.lock().await.lease.is_some());
    finish(service, root).await;
}
#[tokio::test]
async fn cancelled_http_observer_cannot_release_an_inflight_dial() {
    let (service, fake, root) = fixture("cancel-observer");
    fake.0.execute_wait.store(true, Ordering::Release);
    let observer = tokio::spawn(service.execute(Action::Dial, "123".into()));
    signal(&fake.0.execute_entered).await;
    observer.abort();
    let _ = observer.await;
    assert_eq!(service.0.life.lock().unwrap().jobs, 1);
    let owner = service.clone();
    let mut shutdown = tokio::spawn(async move { owner.finish_shutdown().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut shutdown)
            .await
            .is_err()
    );
    assert!(fake.0.releases.lock().unwrap().is_empty());
    fake.0.execute_resume.add_permits(1);
    shutdown.await.unwrap().unwrap();
    assert_eq!(fake.0.actions.lock().unwrap().len(), 1);
    assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 0);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn failed_release_retries_only_the_same_token_and_never_sends_hangup() {
    let (service, fake, root) = fixture("release");
    assert_eq!(
        service.execute(Action::Dial, "123".into()).await.status(),
        StatusCode::OK
    );
    fake.0.release_failures.store(2, Ordering::Release);
    finish(service, root).await;
    let releases = fake.0.releases.lock().unwrap();
    assert!(releases.len() >= 3);
    assert!(releases.iter().all(|token| token == "lease-1"));
    assert_eq!(
        fake.0.actions.lock().unwrap().as_slice(),
        &[(Action::Dial, "123".into())]
    );
}
#[tokio::test]
async fn go_transport_retirement_does_not_retire_native_call_facts_or_ignore_current_urcs() {
    let (service, fake, root) = fixture("go-replace");
    fake.emit("ring");
    service.retire_transport().await.unwrap();
    fake.emit("connect");
    service.0.facts.replace_producer("new-go".into()).unwrap();
    assert_eq!(service.0.state.snapshot().display["state"], "active");
    assert!(
        service
            .0
            .state
            .revision_current(service.0.state.query_revision())
    );
    service.resume_transport();
    finish(service, root).await;
}

#[tokio::test]
async fn go_rotation_during_native_dial_preserves_worker_lease_audio_and_call_history() {
    let (service, fake, root) = fixture("go-rotation-during-dial");
    fake.0.ready_wait.store(true, Ordering::Release);
    let observer = tokio::spawn(service.execute(Action::Dial, "123".into()));
    signal(&fake.0.ready_entered).await;
    // The managed-module Go-exit path rotates only compatibility call facts.
    // A retained native dial must still finish with its original physical lease.
    service
        .0
        .facts
        .replace_producer("go-after-readiness".into())
        .unwrap();
    assert_eq!(service.0.life.lock().unwrap().jobs, 1);
    fake.0.ready_resume.add_permits(1);
    assert_eq!(observer.await.unwrap().status(), StatusCode::OK);
    fake.emit("connect");
    service
        .0
        .facts
        .replace_producer("go-during-call".into())
        .unwrap();
    assert_eq!(service.0.state.snapshot().display["state"], "active");
    assert_eq!(
        service
            .0
            .physical
            .lock()
            .await
            .lease
            .as_ref()
            .unwrap()
            .token,
        "lease-1"
    );
    assert!(fake.0.releases.lock().unwrap().is_empty());
    assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 0);

    // The modem acknowledges ATH and then confirms voice idle. An OK
    // command alone is not proof that the call or audio route ended.
    fake.0.queries.lock().unwrap().extend([
        snapshot("active", false),
        snapshot("active", false),
        snapshot("idle", true),
    ]);
    assert_eq!(
        service.execute(Action::Hangup, "".into()).await.status(),
        StatusCode::OK
    );
    assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 1);
    service.finish_shutdown().await.unwrap();
    assert_eq!(
        fake.0.actions.lock().unwrap().as_slice(),
        &[(Action::Dial, "123".into()), (Action::Hangup, "".into()),]
    );
    assert_eq!(fake.0.releases.lock().unwrap().as_slice(), &["lease-1"]);
    let records = proletarius_store::ArchiveStore::new(root.join("calls.json"))
        .load()
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["number"], "123");
    assert_eq!(records[0]["answered"], true);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_new_dial_cannot_reuse_an_unconfirmed_release_token() {
    let (service, fake, root) = fixture("cleanup-token");
    fake.emit("ring");
    tokio::time::timeout(Duration::from_secs(3), async {
        while service.0.physical.lock().await.lease.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fake.0.release_failures.store(100, Ordering::Release);
    fake.emit("hangup");
    tokio::time::timeout(Duration::from_secs(3), async {
        while !service.0.physical.lock().await.releasing {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let reply = service.execute(Action::Dial, "123".into()).await;
    assert_eq!(reply.status(), StatusCode::CONFLICT);
    assert_eq!(data(reply).await["code"], "module_radio_cleanup_pending");
    assert!(fake.0.actions.lock().unwrap().is_empty());
    assert_eq!(fake.0.reserve_count.load(Ordering::Acquire), 1);
    fake.0.release_failures.store(0, Ordering::Release);
    finish(service, root).await;
}

#[tokio::test]
async fn a_replaced_or_disconnected_transport_cannot_authorize_from_old_ring() {
    let (service, fake, root) = fixture("stale-ring");
    fake.emit("ring");
    service.sync_binding(None);
    assert_eq!(service.0.state.snapshot().display["state"], "unknown");
    assert_eq!(
        service
            .execute(Action::Answer, String::new())
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert!(fake.0.actions.lock().unwrap().is_empty());
    fake.emit("ring");
    service.retire_transport().await.unwrap();
    service.resume_transport();
    assert_eq!(service.0.state.snapshot().display["state"], "unknown");
    assert_eq!(
        service
            .execute(Action::Answer, String::new())
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert!(fake.0.actions.lock().unwrap().is_empty());
    finish(service, root).await;
}

#[tokio::test]
async fn final_receipt_preserves_history_error_after_release_and_still_joins_sink() {
    let (service, fake, root) = fixture("final-history-error");
    let reservation = fake.reserve().await.unwrap();
    service.0.physical.lock().await.lease = Some(reservation);
    service.0.life.lock().unwrap().shutdown_retire_error =
        Some("injected terminal persistence error".into());
    let receipt = service.finish_shutdown_receipt().await.unwrap();
    assert_eq!(receipt, vec!["injected terminal persistence error"]);
    assert_eq!(fake.0.releases.lock().unwrap().len(), 1);
    assert!(service.0.state.sink().worker_retired());
    assert_eq!(service.finish_shutdown_receipt().await.unwrap(), receipt);
    assert_eq!(fake.0.releases.lock().unwrap().len(), 1);
    assert!(service.finish_shutdown().await.is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn final_receipt_waits_for_exact_lease_release_before_history_retirement() {
    let (service, fake, root) = fixture("final-release-pending");
    let reservation = fake.reserve().await.unwrap();
    let token = reservation.token.clone();
    service.0.physical.lock().await.lease = Some(reservation);
    fake.0.release_failures.store(100, Ordering::Release);
    let owner = service.clone();
    let mut shutdown = tokio::spawn(async move { owner.finish_shutdown_receipt().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut shutdown)
            .await
            .is_err()
    );
    assert!(!service.0.state.sink().worker_retired());
    fake.0.release_failures.store(0, Ordering::Release);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert!(
        fake.0
            .releases
            .lock()
            .unwrap()
            .iter()
            .all(|release| release == &token)
    );
    assert!(service.0.state.sink().worker_retired());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn pending_child_observer_retains_physical_job_and_never_dials_late_or_poison_queued_command()
{
    let (service, fake, root) = fixture("real-child-pending");
    let mut probe = crate::call_mode_adb::child_tests::HeldProbe::new("module-call");
    let supervision_gate = Arc::new(AtomicBool::new(false));
    probe.supervision_gate = Some(supervision_gate.clone());
    *fake.0.child_probe.lock().unwrap() = Some(probe.clone());
    let first = tokio::spawn(service.execute(Action::Dial, "111".into()));
    tokio::time::timeout(Duration::from_secs(3), async {
        while !probe.root.join("child-ready").exists() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    // Queue B before A publishes pending, so any accidental global event
    // subscription would detach B while it still waits for the physical lane.
    let second = tokio::spawn(service.execute(Action::Dial, "222".into()));
    tokio::time::timeout(Duration::from_secs(3), async {
        while service.0.life.lock().unwrap().jobs != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!second.is_finished());
    supervision_gate.store(true, Ordering::Release);
    let reply = tokio::time::timeout(Duration::from_secs(3), first)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(data(reply).await["code"], "module_adb_cleanup_pending");
    assert!(probe.alive() && probe.lock_busy());
    assert!(service.0.physical.try_lock().is_err());
    assert_eq!(service.0.life.lock().unwrap().jobs, 2);
    assert!(fake.0.actions.lock().unwrap().is_empty());
    assert!(fake.0.releases.lock().unwrap().is_empty());
    let status = tokio::time::timeout(Duration::from_millis(300), service.status())
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    // B has its own watch and is queued behind A's exact physical lane. A's
    // already-published error must not detach B and create an unseen late Dial.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(!second.is_finished());
    probe.release();
    let reply = tokio::time::timeout(Duration::from_secs(3), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(
        *fake.0.actions.lock().unwrap(),
        [(Action::Dial, "222".into())]
    );
    assert!(!probe.alive());
    assert!(!probe.lock_busy());
    finish(service, root).await;
    std::fs::remove_dir_all(&probe.root).unwrap();
}

mod clcc_lifecycle;
