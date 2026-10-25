//! Actual Service/RequestLedger transactions over private disk roots. Retained
//! Record fixtures have no IMS/physical worker; these are not protocol tests.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

type BeforeInsert = Box<dyn FnOnce(&mut super::super::operation::RequestLedger) + Send>;
#[derive(Default)]
pub(super) struct Hooks {
    before_insert: Mutex<Option<BeforeInsert>>,
    submitted_ack: AtomicBool,
    release_failure: AtomicBool,
    actual_releases: AtomicUsize,
    provisional_race: Mutex<Option<Arc<ProvisionalRace>>>,
}
impl Hooks {
    pub(super) fn before_insert(&self, ledger: &mut super::super::operation::RequestLedger) {
        let hook = self.before_insert.lock().unwrap().take();
        if let Some(hook) = hook {
            hook(ledger);
        }
    }
    pub(super) fn before_submitted(&self, ledger: &mut super::super::operation::RequestLedger) {
        if self.submitted_ack.swap(false, Ordering::SeqCst) {
            ledger.test_fail_after_next_persist();
        }
    }
    pub(super) fn before_provisional_lock(&self) {
        let race = self.provisional_race.lock().unwrap().clone();
        if let Some(race) = race {
            race.arrive();
        }
    }
    pub(super) fn note_actual_release(&self) {
        self.actual_releases.fetch_add(1, Ordering::SeqCst);
    }
    pub(super) fn fail_release(&self) -> bool {
        self.release_failure.swap(false, Ordering::SeqCst)
    }
    fn on_insert(
        &self,
        hook: impl FnOnce(&mut super::super::operation::RequestLedger) + Send + 'static,
    ) {
        *self.before_insert.lock().unwrap() = Some(Box::new(hook));
    }
}
// Two actual stop calls both reach the release boundary before either may
// acquire the record lock. The wait is bounded so a fixture fault cannot hang.
#[derive(Default)]
struct ProvisionalRace {
    arrivals: Mutex<usize>,
    changed: std::sync::Condvar,
}
impl ProvisionalRace {
    fn arrive(&self) {
        let mut arrivals = self.arrivals.lock().unwrap();
        *arrivals += 1;
        self.changed.notify_all();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while *arrivals < 2 {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "second actual stop did not reach release boundary"
            );
            let (next, _) = self.changed.wait_timeout(arrivals, remaining).unwrap();
            arrivals = next;
        }
    }
}
struct Fixture {
    root: PathBuf,
    server: crate::ServerState,
    service: Service,
}
impl Fixture {
    fn new() -> Self {
        Self::with_seed(None)
    }
    fn with_seed(seed: Option<Vec<Value>>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "module-persistence-{}",
            crate::snapshot::new_instance_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        // The shared ServerState fixture places its actual store under multi.
        let data_root = root.join("multi");
        if let Some(seed) = seed {
            let store = proletarius_store::ArchiveStore::with_limits(
                data_root.join("vowifi-module-request-ledger.json"),
                32 * 1024 * 1024,
                100_000,
            )
            .unwrap();
            store
                .mutate(|items| {
                    *items = seed;
                    Ok(())
                })
                .unwrap();
        }
        let mut server = crate::reader_pcsc_gateway_tests::state(&root);
        server.usb_at = crate::usb_at::Owner::unavailable_for_tests("persistence-no-device".into());
        server.native_vowifi_lifecycle.activate().unwrap();
        let service = server
            .native_vowifi_lifecycle
            .module_service(&server)
            .unwrap();
        assert_eq!(
            service.0.root, data_root,
            "seed and Service must share the actual data root"
        );
        Self {
            root,
            server,
            service,
        }
    }
    fn path(&self) -> PathBuf {
        self.service
            .0
            .root
            .join("vowifi-module-request-ledger.json")
    }
    fn io_fault_on_insert(&self) -> BlockedRoot {
        let path = self.root.clone();
        let saved = self.root.with_extension("held-for-io-fault");
        let restore = BlockedRoot {
            path: path.clone(),
            saved: saved.clone(),
        };
        self.service.0.persistence_hooks.on_insert(move |_| {
            // ArchiveStore repairs parent mode, so chmod is not a real fault.
            // Replace only this private fixture's root with a regular file.
            std::fs::rename(&path, &saved).unwrap();
            std::fs::write(&path, b"not-a-directory").unwrap();
            assert!(std::fs::read_dir(&path).is_err());
        });
        restore
    }
    fn retained_record(&self, retired: bool) -> Arc<Record> {
        let record = Arc::new(Record {
            control: Default::default(),
            id: 1,
            claim: self
                .service
                .0
                .admission
                .acquire_native_module(&self.service.0.epoch, 1)
                .unwrap(),
            connection: request::automatic(),
            stop: watch::channel(false).0,
            state: Mutex::new(RecordState {
                operation: Some(super::super::operation::Clock::start(
                    tokio::time::Instant::now(),
                    START_OPERATION_BUDGET,
                )),
                operation_done: true,
                operation_enable: true,
                error: Some("retained-start-result".into()),
                ..Default::default()
            }),
            completed: Notify::new(),
            join: Default::default(),
        });
        let options =
            super::super::operation::Options::body(&body("retained-start", true)).unwrap();
        let mut state = self.service.0.state.lock().unwrap();
        self.service
            .remember_request(&mut state, &options, &"a".repeat(64), Some(&record))
            .unwrap();
        state.next = 1;
        state.record = Some(record.clone());
        if retired {
            record.claim.release_confirmed().unwrap();
            record.state.lock().unwrap().retired = true;
        }
        drop(state);
        // Settle the original receipt before the transaction under test.
        self.service.lookup_request("retained-start", None).unwrap();
        record
    }
    fn assert_unlaunched_released(&self, expected_next: u64) {
        let state = self.service.0.state.lock().unwrap();
        assert_eq!(state.next, expected_next);
        assert!(state.record.is_none());
        assert!(!self.service.0.admission.has_ordinary_claim());
        drop(state);
        let reader = self
            .service
            .0
            .admission
            .acquire_native("other-reader-after-rejection", expected_next)
            .unwrap();
        reader.release_confirmed().unwrap();
    }
    async fn close(self) {
        // Only this test's explicit Record fixture can be manually completed;
        // no live worker/card exists in these control-transaction tests.
        if let Some(record) = self.service.record() {
            assert!(!record.join.started());
            record.claim.release_confirmed().unwrap();
            record.join.confirm_unspawned().unwrap();
            record.state.lock().unwrap().complete_cleanup();
        }
        self.server
            .native_vowifi_lifecycle
            .finish_shutdown(tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .unwrap();
        self.server.sms_forwarding.shutdown().await.unwrap();
        self.server.reader_pcsc.shutdown().await.unwrap();
        drop(self.service);
        drop(self.server);
        std::fs::remove_dir_all(&self.root).unwrap();
        assert!(!self.root.exists());
    }
}
struct BlockedRoot {
    path: PathBuf,
    saved: PathBuf,
}
impl Drop for BlockedRoot {
    fn drop(&mut self) {
        if self.saved.exists() {
            std::fs::remove_file(&self.path).unwrap();
            std::fs::rename(&self.saved, &self.path).unwrap();
        }
    }
}
fn body(id: &str, enable: bool) -> Vec<u8> {
    serde_json::to_vec(&json!({"enable":enable,"observe_operation":true,"client_request_id":id}))
        .unwrap()
}
async fn post(f: &Fixture, bytes: &[u8]) -> (StatusCode, Value) {
    let response = f
        .service
        .handle(
            Request::builder()
                .method("POST")
                .uri("/api/vowifi")
                .body(Full::new(Bytes::copy_from_slice(bytes)))
                .unwrap(),
        )
        .await;
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn record_sample(record: &Record) -> Value {
    let state = record.state.lock().unwrap();
    json!({
        "clock":state.operation.as_ref().map(|clock| serde_json::to_value(clock.receipt("clock-sample", &"b".repeat(64), "sample", record.id, record.id)).unwrap()),
        "operation_done":state.operation_done,"operation_enable":state.operation_enable,
        "cancel_requested":state.cancel_requested,"stopping":state.stopping,
        "error":state.error,"operation_deadline":format!("{:?}",state.operation_deadline),
        "retired":state.retired,"cleanup_error":state.cleanup_error,
        "stop":*record.stop.borrow()
    })
}
fn disk_receipt(f: &Fixture, id: &str) -> Option<Value> {
    let values: Vec<Value> = serde_json::from_slice(&std::fs::read(f.path()).unwrap()).unwrap();
    values.into_iter().find(|value| value["request_id"] == id)
}

#[tokio::test]
async fn first_receipt_actual_io_failure_releases_claim_and_consumes_gate_sequence() {
    let f = Fixture::new();
    let readonly = f.io_fault_on_insert();
    assert_eq!(
        post(&f, &body("first-io-rejected", true)).await.0,
        StatusCode::CONFLICT
    );
    drop(readonly);
    f.assert_unlaunched_released(1);
    assert!(!f.path().exists(), "no receipt rename occurred");
    // Another failed attempt must use a fresh gate sequence in this same epoch.
    let readonly = f.io_fault_on_insert();
    assert_eq!(
        post(&f, &body("second-io-rejected", true)).await.0,
        StatusCode::CONFLICT
    );
    drop(readonly);
    f.assert_unlaunched_released(2);
    f.close().await;
}

#[tokio::test]
async fn actual_byte_capacity_failure_releases_unlaunched_claim_without_evicting_history() {
    let limit = 32 * 1024 * 1024;
    let receipt =
        super::super::operation::Receipt::rejected("retained-history", &"c".repeat(64), false, "");
    let mut values = vec![serde_json::to_value(receipt).unwrap()];
    let base = serde_json::to_vec(&values).unwrap().len();
    values[0]["snapshot"]["message"] = json!("x".repeat(limit - base - 64));
    assert_eq!(serde_json::to_vec(&values).unwrap().len(), limit - 64);
    let f = Fixture::with_seed(Some(values));
    let original = std::fs::read(f.path()).unwrap();
    assert_eq!(
        post(&f, &body("actual-byte-cap-rejected", true)).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(std::fs::read(f.path()).unwrap(), original);
    f.assert_unlaunched_released(1);
    let reopened =
        super::super::operation::RequestLedger::open(&f.service.0.root, "module").unwrap();
    assert!(reopened.lookup("retained-history", None).unwrap().is_some());
    assert!(
        reopened
            .lookup("actual-byte-cap-rejected", None)
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn unconfirmed_provisional_release_stays_visible_and_only_exact_stop_retries_release() {
    let f = Fixture::new();
    f.service
        .0
        .persistence_hooks
        .release_failure
        .store(true, Ordering::SeqCst);
    let readonly = f.io_fault_on_insert();
    assert_eq!(
        post(&f, &body("release-unconfirmed", true)).await.0,
        StatusCode::CONFLICT
    );
    drop(readonly);
    let record = f.service.record().expect("exact unlaunched owner retained");
    assert!(!record.join.started());
    assert!(record.claim.try_is_current().unwrap());
    let state = record.state.lock().unwrap();
    assert!(state.provisional && state.stopping && !state.retired);
    assert!(state.cleanup_error.is_some());
    drop(state);
    assert!(f.service.status().get("cleanup_error").is_some());
    f.service
        .0
        .persistence_hooks
        .release_failure
        .store(true, Ordering::SeqCst);
    assert_eq!(
        f.service
            .stop_before(tokio::time::Instant::now() + Duration::from_secs(1))
            .await,
        Err("module_vowifi_cleanup_unconfirmed")
    );
    assert!(record.claim.try_is_current().unwrap());
    assert!(!record.state.lock().unwrap().retired);
    f.service
        .stop_before(tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    assert!(!record.claim.try_is_current().unwrap());
    assert!(record.state.lock().unwrap().retired);
    f.close().await;
}

#[tokio::test]
async fn first_or_submitted_commit_ack_loss_releases_zero_dispatch_owner_but_reopens_unknown() {
    for submitted in [false, true] {
        let f = Fixture::new();
        if submitted {
            f.service
                .0
                .persistence_hooks
                .submitted_ack
                .store(true, Ordering::SeqCst);
        } else {
            f.service
                .0
                .persistence_hooks
                .on_insert(|ledger| ledger.test_fail_after_next_persist());
        }
        let bytes = body("start-ack-unknown", true);
        assert_eq!(post(&f, &bytes).await.0, StatusCode::CONFLICT);
        f.assert_unlaunched_released(1);
        assert!(
            disk_receipt(&f, "start-ack-unknown").is_some(),
            "real mutation committed before simulated lost ACK"
        );
        assert_eq!(post(&f, &bytes).await.0, StatusCode::CONFLICT);
        assert_eq!(f.service.0.state.lock().unwrap().next, 1);
        let reopened =
            super::super::operation::RequestLedger::open(&f.service.0.root, "module").unwrap();
        let unknown = reopened
            .lookup("start-ack-unknown", None)
            .unwrap()
            .unwrap()
            .resolve(None);
        assert_eq!(
            unknown.client_request_id.as_deref(),
            Some("start-ack-unknown")
        );
        assert_eq!(unknown.phase, "unknown");
        assert!(!unknown.terminal && !unknown.cleanup_confirmed);
        f.close().await;
    }
}

#[tokio::test]
async fn live_and_retired_disable_before_commit_failure_preserve_clock_flags_and_original_receipt()
{
    for retired in [false, true] {
        let f = Fixture::new();
        let record = f.retained_record(retired);
        let before = record_sample(&record);
        let old_receipt = disk_receipt(&f, "retained-start").unwrap();
        let readonly = f.io_fault_on_insert();
        assert_eq!(
            post(&f, &body("disable-before-commit", false)).await.0,
            StatusCode::CONFLICT
        );
        drop(readonly);
        assert_eq!(record_sample(&record), before);
        assert_eq!(disk_receipt(&f, "retained-start").unwrap(), old_receipt);
        assert!(disk_receipt(&f, "disable-before-commit").is_none());
        assert_eq!(record.claim.try_is_current().unwrap(), !retired);
        // Known-before-commit insert lookup failure does not poison the ledger.
        // Retry the same explicit ID after the actual filesystem fault is gone.
        assert_eq!(
            post(&f, &body("disable-before-commit", false)).await.0,
            StatusCode::ACCEPTED
        );
        let receipt = disk_receipt(&f, "disable-before-commit").unwrap();
        assert_eq!(receipt["requested_enable"], false);
        assert_eq!(
            receipt["snapshot"]["operation_id"],
            format!("{}-1-1", f.service.0.epoch)
        );
        assert_eq!(*record.stop.borrow(), !retired);
        f.close().await;
    }
}

#[tokio::test]
async fn disable_real_commit_ack_loss_preserves_live_state_and_original_id_reopens_unknown() {
    for (retired, submitted) in [(false, false), (false, true), (true, false)] {
        let f = Fixture::new();
        let record = f.retained_record(retired);
        let before = record_sample(&record);
        let old_receipt = disk_receipt(&f, "retained-start").unwrap();
        if submitted {
            f.service
                .0
                .persistence_hooks
                .submitted_ack
                .store(true, Ordering::SeqCst);
        } else {
            f.service
                .0
                .persistence_hooks
                .on_insert(|ledger| ledger.test_fail_after_next_persist());
        }
        let bytes = body("disable-ack-unknown", false);
        assert_eq!(post(&f, &bytes).await.0, StatusCode::CONFLICT);
        assert_eq!(record_sample(&record), before);
        assert_eq!(disk_receipt(&f, "retained-start").unwrap(), old_receipt);
        let persisted = disk_receipt(&f, "disable-ack-unknown").unwrap();
        assert_eq!(persisted["requested_enable"], false);
        assert_eq!(
            persisted["snapshot"]["operation_id"],
            format!("{}-1-1", f.service.0.epoch)
        );
        assert!(!*record.stop.borrow());
        assert_eq!(post(&f, &bytes).await.0, StatusCode::CONFLICT);
        assert_eq!(record_sample(&record), before);
        let reopened =
            super::super::operation::RequestLedger::open(&f.service.0.root, "module").unwrap();
        let unknown = reopened
            .lookup("disable-ack-unknown", None)
            .unwrap()
            .unwrap()
            .resolve(None);
        assert_eq!(unknown.requested_enable, false);
        assert_eq!(unknown.phase, "unknown");
        assert_eq!(unknown.operation_id, format!("{}-1-1", f.service.0.epoch));
        assert!(!unknown.terminal);
        f.close().await;
    }
}

#[tokio::test]
async fn successful_staged_disable_preserves_pending_start_cancellation_contract() {
    let f = Fixture::new();
    let record = f.retained_record(false);
    {
        let mut state = record.state.lock().unwrap();
        state.operation_done = false;
        state.error = None;
    }
    let (status, accepted) = post(&f, &body("cancel-pending-start", false)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(accepted["cancelling"], true);
    let state = record.state.lock().unwrap();
    assert!(
        state.cancel_requested && state.operation_enable && !state.operation_done && state.stopping
    );
    assert_eq!(state.operation.as_ref().unwrap().sequence, 1);
    let clock = state.operation.as_ref().unwrap();
    let stopped = clock.project(
        tokio::time::Instant::now(),
        &f.service.0.epoch,
        record.id,
        super::super::operation::Facts {
            generation: record.id,
            phase: "stopped".into(),
            ready: false,
            sms_ready: false,
            call_ready: false,
            cleanup_confirmed: true,
            superseded: false,
            error: None,
        },
    );
    assert_eq!(stopped.outcome.as_deref(), Some("cancelled"));
    assert!(!stopped.requested_enable);
    drop(state);
    assert!(*record.stop.borrow());
    assert_eq!(
        disk_receipt(&f, "cancel-pending-start").unwrap()["requested_enable"],
        false
    );
    f.close().await;
}

#[tokio::test]
async fn rejected_successor_restores_exact_previous_retired_record_and_burns_sequence() {
    let f = Fixture::new();
    let previous = f.retained_record(true);
    let before = record_sample(&previous);
    let original = std::fs::read(f.path()).unwrap();
    let blocked = f.io_fault_on_insert();
    assert_eq!(
        post(&f, &body("successor-before-commit", true)).await.0,
        StatusCode::CONFLICT
    );
    drop(blocked);
    let state = f.service.0.state.lock().unwrap();
    assert_eq!(state.next, 2);
    assert!(Arc::ptr_eq(state.record.as_ref().unwrap(), &previous));
    assert!(!f.service.0.admission.has_ordinary_claim());
    drop(state);
    assert_eq!(record_sample(&previous), before);
    assert_eq!(std::fs::read(f.path()).unwrap(), original);
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_provisional_stops_release_exact_claim_once_and_both_confirm_retirement() {
    let f = Fixture::new();
    f.service
        .0
        .persistence_hooks
        .release_failure
        .store(true, Ordering::SeqCst);
    let blocked = f.io_fault_on_insert();
    assert_eq!(
        post(&f, &body("concurrent-release-owner", true)).await.0,
        StatusCode::CONFLICT
    );
    drop(blocked);
    let record = f.service.record().unwrap();
    assert!(record.state.lock().unwrap().provisional);
    assert!(record.claim.try_is_current().unwrap());
    assert!(!record.join.started());
    assert_eq!(
        f.service
            .0
            .persistence_hooks
            .actual_releases
            .load(Ordering::SeqCst),
        0
    );
    *f.service
        .0
        .persistence_hooks
        .provisional_race
        .lock()
        .unwrap() = Some(Arc::new(ProvisionalRace::default()));
    let first = f.service.clone();
    let second = f.service.clone();
    let a = tokio::spawn(async move {
        first
            .stop_before(tokio::time::Instant::now() + Duration::from_secs(10))
            .await
    });
    let b = tokio::spawn(async move {
        second
            .stop_before(tokio::time::Instant::now() + Duration::from_secs(10))
            .await
    });
    assert_eq!(a.await.unwrap(), Ok(()));
    assert_eq!(b.await.unwrap(), Ok(()));
    *f.service
        .0
        .persistence_hooks
        .provisional_race
        .lock()
        .unwrap() = None;
    assert_eq!(
        f.service
            .0
            .persistence_hooks
            .actual_releases
            .load(Ordering::SeqCst),
        1
    );
    assert!(!record.claim.try_is_current().unwrap());
    assert!(!f.service.0.admission.has_ordinary_claim());
    let state = record.state.lock().unwrap();
    assert!(state.retired && !state.provisional);
    assert!(state.cleanup_error.is_none());
    drop(state);
    f.service
        .stop_before(tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(
        f.service
            .0
            .persistence_hooks
            .actual_releases
            .load(Ordering::SeqCst),
        1
    );
    f.close().await;
}

#[tokio::test]
async fn g3_module_exact_stop_survives_ledger_failure_cancel_and_retired_before_join() {
    let f = Fixture::new();
    let record = f.retained_record(false);
    let target = record
        .control
        .target("module", &f.service.0.epoch, record.id);
    let get = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/vowifi/stop-control?{}",
            target.query_string()
        ))
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(f.service.handle(get).await.status(), StatusCode::OK);
    assert!(!*record.stop.borrow());
    assert!(!record.control.started());
    let (released, release) = tokio::sync::oneshot::channel();
    let (done, tail) = tokio::sync::oneshot::channel();
    let worker = record.clone();
    record
        .join
        .spawn(async move {
            release.await.unwrap();
            worker.claim.release_confirmed().unwrap();
            worker.state.lock().unwrap().complete_cleanup();
            worker.completed.notify_waiters();
            tail.await.unwrap();
        })
        .unwrap();
    let path = f.path();
    std::fs::write(&path, b"g3-preserved-broken-module-ledger").unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let response = f
        .service
        .handle(
            Request::builder()
                .method("POST")
                .uri("/api/vowifi")
                .body(Full::new(Bytes::from(body(
                    "g3-normal-stop-blocked",
                    false,
                ))))
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(!*record.stop.borrow());
    let req = || {
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/vowifi/stop-control?{}",
                target.query_string()
            ))
            .body(Full::new(Bytes::new()))
            .unwrap()
    };
    assert_eq!(f.service.handle(req()).await.status(), StatusCode::ACCEPTED);
    assert!(*record.stop.borrow());
    assert!(record.claim.try_is_current().unwrap());
    let observer = f.service.clone();
    let observer = tokio::spawn(async move {
        observer
            .stop_before(tokio::time::Instant::now() + Duration::from_secs(3))
            .await
    });
    tokio::task::yield_now().await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    released.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !record.state.lock().unwrap().retired {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(record.control.busy(), "retired is not a true outer join");
    assert_eq!(
        f.service
            .request_with_observation(true, request::automatic(), None, "")
            .err()
            .unwrap(),
        "module_vowifi_busy"
    );
    done.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while record.control.busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let successor = Arc::new(Record {
        control: Default::default(),
        id: 2,
        claim: f
            .service
            .0
            .admission
            .acquire_native_module(&f.service.0.epoch, 2)
            .unwrap(),
        connection: request::automatic(),
        stop: watch::channel(false).0,
        state: Mutex::new(RecordState::default()),
        completed: Notify::new(),
        join: Default::default(),
    });
    {
        let mut state = f.service.0.state.lock().unwrap();
        state.last_stop = Some((target.clone(), record.control.clone()));
        state.record = Some(successor.clone());
        state.next = 2;
    }
    assert_eq!(f.service.handle(req()).await.status(), StatusCode::OK);
    assert!(!*successor.stop.borrow(), "old POST never stops successor");
    let mut stale = target.clone();
    stale.daemon_epoch.push('x');
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/vowifi/stop-control?{}", stale.query_string()))
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(
        f.service.handle(request).await.status(),
        StatusCode::CONFLICT
    );
    assert!(!*successor.stop.borrow());
    // Teardown only manually constructed, never-dispatched successor fixture.
    f.close().await;
}

#[tokio::test]
async fn g3_module_capacity_and_injected_readonly_enospc_ack_loss_preserve_history() {
    for fault in [
        "count",
        "bytes",
        "readonly-injected",
        "enospc-injected",
        "rename-ack-loss",
    ] {
        let f = Fixture::new();
        let record = f.retained_record(false);
        let target = record
            .control
            .target("module", &f.service.0.epoch, record.id);
        let (finish, finished) = tokio::sync::oneshot::channel();
        let worker = record.clone();
        record
            .join
            .spawn(async move {
                finished.await.unwrap();
                worker.claim.release_confirmed().unwrap();
                worker.state.lock().unwrap().complete_cleanup();
                worker.completed.notify_waiters();
            })
            .unwrap();
        let path = f.path();
        match fault {
            "count" => {
                assert_eq!(
                    f.service
                        .0
                        .state
                        .lock()
                        .unwrap()
                        .requests
                        .test_fill_capacity(),
                    100_000
                );
            }
            "bytes" => std::fs::write(&path, vec![b' '; 32 * 1024 * 1024 + 1]).unwrap(),
            "readonly-injected" => f
                .service
                .0
                .state
                .lock()
                .unwrap()
                .requests
                .test_write_fault(std::io::ErrorKind::ReadOnlyFilesystem),
            "enospc-injected" => f
                .service
                .0
                .state
                .lock()
                .unwrap()
                .requests
                .test_write_fault(std::io::ErrorKind::StorageFull),
            _ => f
                .service
                .0
                .persistence_hooks
                .on_insert(|ledger| ledger.test_fail_after_next_persist()),
        }
        let response = f
            .service
            .handle(
                Request::builder()
                    .method("POST")
                    .uri("/api/vowifi")
                    .body(Full::new(Bytes::from(body(
                        "g3-module-blocked-stop",
                        false,
                    ))))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{fault}");
        assert!(!*record.stop.borrow());
        let before = std::fs::read(&path).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/vowifi/stop-control?{}",
                target.query_string()
            ))
            .body(Full::new(Bytes::new()))
            .unwrap();
        assert_eq!(
            f.service.handle(req).await.status(),
            StatusCode::ACCEPTED,
            "{fault}"
        );
        assert!(*record.stop.borrow());
        assert!(record.claim.try_is_current().unwrap());
        finish.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while record.control.busy() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "{fault} fallback does not write history"
        );
        if fault == "count" {
            assert_eq!(
                f.service.0.state.lock().unwrap().requests.test_count(),
                100_000
            );
        }
        if fault == "rename-ack-loss" {
            let ledger =
                super::super::operation::RequestLedger::open(&f.service.0.root, "module").unwrap();
            let unknown = ledger
                .lookup("g3-module-blocked-stop", None)
                .unwrap()
                .unwrap()
                .resolve(None);
            assert_eq!(unknown.outcome.as_deref(), Some("unknown"));
            assert!(!unknown.terminal && !unknown.succeeded && !unknown.cleanup_confirmed);
        }
        f.service.0.state.lock().unwrap().record = None;
        f.close().await;
    }
}

#[tokio::test]
async fn g3_poisoned_control_reservation_never_changes_stopping_or_sends_signal() {
    let f = Fixture::new();
    let record = f.retained_record(false);
    record.control.test_poison();
    let target = record
        .control
        .target("module", &f.service.0.epoch, record.id);
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/vowifi/stop-control?{}",
            target.query_string()
        ))
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(
        f.service.handle(req).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(!record.state.lock().unwrap().stopping);
    assert!(!*record.stop.borrow());
    assert!(record.claim.try_is_current().unwrap());
    f.close().await;
}

#[tokio::test]
async fn g3_module_head_current_retired_stale_invalid_never_signals_or_writes() {
    let f = Fixture::new();
    let record = f.retained_record(false);
    let target = record
        .control
        .target("module", &f.service.0.epoch, record.id);
    let query = target.query_string();
    let before = std::fs::read(f.path()).unwrap();
    for (query, status) in [
        (query.clone(), StatusCode::OK),
        (
            query.replace(&target.daemon_epoch, "stale-epoch"),
            StatusCode::CONFLICT,
        ),
        (
            query.replace("owner_generation=1", "owner_generation=01"),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let observed = record.stop.subscribe();
        assert_eq!(
            super::super::stop_control::test_head_request(
                f.server.clone(),
                &format!("/api/vowifi/stop-control?{query}")
            )
            .await,
            status
        );
        assert!(!observed.has_changed().unwrap());
        assert!(!*record.stop.borrow());
        assert_eq!(std::fs::read(f.path()).unwrap(), before);
    }
    // This fixture has never installed any network/RF task; explicitly prove
    // that zero-dispatch release before publishing a retired completion.
    record.claim.release_confirmed().unwrap();
    record.join.confirm_unspawned().unwrap();
    record.state.lock().unwrap().complete_cleanup();
    record
        .control
        .begin(
            || {
                record.stop.send_replace(true);
            },
            {
                let owner = record.control.clone();
                async move {
                    owner.finish(true, true, None);
                }
            },
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while record.control.busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    {
        let mut state = f.service.0.state.lock().unwrap();
        state.last_stop = Some((target.clone(), record.control.clone()));
        state.record = None;
    }
    let observed = record.stop.subscribe();
    assert_eq!(
        super::super::stop_control::test_head_request(
            f.server.clone(),
            &format!("/api/vowifi/stop-control?{}", target.query_string())
        )
        .await,
        StatusCode::OK
    );
    assert!(!observed.has_changed().unwrap());
    assert_eq!(std::fs::read(f.path()).unwrap(), before);
    f.close().await;
}
