use super::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
const READER: &str = "BLE:12345678-1234-1234-1234-123456789abc";
const PRODUCER: &str = "native-pin-fixture-producer-00000001";
struct Supplier {
    gate: Arc<Gate>,
    reader: String,
    session: Option<crate::reader_pcsc::LifecycleBinding>,
    pre_panic: AtomicBool,
    admitted: AtomicUsize,
    finalized: AtomicUsize,
    pre_entered: AtomicUsize,
    pre_hold: Mutex<Option<Arc<tokio::sync::Notify>>>,
    stop_lease: Mutex<Option<std::sync::Weak<NativeReaderSessionLease>>>,
    admission_hold: Mutex<Option<Arc<tokio::sync::Notify>>>,
    finalize_hold: Mutex<Option<Arc<tokio::sync::Notify>>>,
    rejection: Mutex<Option<String>>,
    snapshot_override: Mutex<Option<Value>>,
    commit: AtomicBool,
    finalize_error: AtomicBool,
}
impl NativeSessionSupplier for Supplier {
    fn pre_admit(
        self: Arc<Self>,
        _: String,
        policy: crate::reader_reservation::NativePinPolicy,
        _: Option<Arc<Claim>>,
        _maintenance: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
    ) -> crate::reader_session_lease::PreAdmissionFuture {
        Box::pin(async move {
            self.pre_entered.fetch_add(1, Ordering::SeqCst);
            let hold = self.pre_hold.lock().unwrap().clone();
            if policy == NativePinPolicy::Mutation && self.pre_panic.load(Ordering::SeqCst) {
                panic!("fixture validation panic");
            }
            if let Some(hold) = hold {
                hold.notified().await;
            }
            if policy == NativePinPolicy::Mutation {
                let old = self
                    .stop_lease
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade);
                if let Some(old) = old {
                    old.release().await.map_err(|error| error.to_string())?;
                }
            }
            Ok(4)
        })
    }
    fn admit(self: Arc<Self>, _: NativePinBinding) -> AdmissionFuture {
        Box::pin(async move {
            self.admitted.fetch_add(1, Ordering::SeqCst);
            let hold = self.admission_hold.lock().unwrap().clone();
            if let Some(hold) = hold {
                hold.notified().await;
            }
            if let Some(error) = self.rejection.lock().unwrap().clone() {
                Err(error)
            } else {
                Ok(())
            }
        })
    }
    fn snapshot(&self, binding: &NativePinBinding) -> Result<Value, String> {
        let mut value = json!({"lease_id":binding.lease_id,"reader":self.reader,"reader_generation":binding.reader_generation,
            "identity":{"iccid":"8985200014631959072","imsi":"234100896392986","mcc":"234","mnc":"33"},
            "notification_aids":["A0000005591010"],"esim_manager_available":true,"session":self.session});
        if let Some(fields) = self.snapshot_override.lock().unwrap().clone() {
            for (key, field) in fields.as_object().unwrap() {
                value[key] = field.clone();
            }
        }
        Ok(value)
    }
    fn finalize(
        self: Arc<Self>,
        binding: NativePinBinding,
        _: Option<NativeCardContext>,
        target: String,
        _: String,
    ) -> SupplierFuture {
        Box::pin(async move {
            self.finalized.fetch_add(1, Ordering::SeqCst);
            let hold = self.finalize_hold.lock().unwrap().clone();
            if let Some(hold) = hold {
                hold.notified().await;
            }
            if self.commit.load(Ordering::SeqCst) {
                self.gate
                    .test_seed_selection(&binding.reader, binding.reader_generation + 1);
            }
            if self.finalize_error.load(Ordering::SeqCst) {
                Err("native finalizer failed".into())
            } else {
                Ok(EsimSwitchFinalizeResponse {
                    target_identity_confirmed: true,
                    reader_refresh_warning: String::new(),
                    reader_refresh_warning_code: String::new(),
                    reader_info: json!({"iccid":target}),
                })
            }
        })
    }
}
struct Fixture {
    gate: Arc<Gate>,
    owner: crate::reader_pcsc::Owner,
    supplier: Arc<Supplier>,
    reader: String,
}
impl Fixture {
    fn new() -> Self {
        Self::for_owner(
            READER,
            crate::reader_pcsc::Owner::new(PRODUCER.into()),
            None,
        )
    }
    fn for_owner(
        reader: &str,
        owner: crate::reader_pcsc::Owner,
        session: Option<crate::reader_pcsc::LifecycleBinding>,
    ) -> Self {
        let gate = Arc::new(Gate::default());
        gate.initialize_business_producer("go-fixture-producer");
        gate.initialize_native_reader_producer(PRODUCER);
        gate.test_seed_selection(reader, 4);
        let supplier = Arc::new(Supplier {
            gate: gate.clone(),
            reader: reader.into(),
            session,
            pre_panic: AtomicBool::new(false),
            admitted: AtomicUsize::new(0),
            finalized: AtomicUsize::new(0),
            pre_entered: AtomicUsize::new(0),
            pre_hold: Mutex::new(None),
            stop_lease: Mutex::new(None),
            admission_hold: Mutex::new(None),
            finalize_hold: Mutex::new(None),
            rejection: Mutex::new(None),
            snapshot_override: Mutex::new(None),
            commit: AtomicBool::new(true),
            finalize_error: AtomicBool::new(false),
        });
        let erased: Arc<dyn NativeSessionSupplier> = supplier.clone();
        gate.set_native_session_supplier(Arc::downgrade(&erased));
        Self {
            gate,
            owner,
            supplier,
            reader: reader.into(),
        }
    }
    async fn acquire(&self) -> NativeReaderSessionLease {
        self.acquire_policy(NativePinPolicy::Identity)
            .await
            .unwrap()
    }
    async fn acquire_policy(
        &self,
        policy: NativePinPolicy,
    ) -> Result<NativeReaderSessionLease, BackendFailure> {
        NativeReaderSessionLease::acquire_with_policy(
            Path::new("unused-native-no-go.sock"),
            None,
            &self.reader,
            self.gate.clone(),
            self.owner.clone(),
            policy,
            None,
            None,
        )
        .await
    }
    fn busy(&self) -> bool {
        self.gate.admit(&self.reader).is_err()
    }
}
async fn wait_for(predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !predicate() {
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn released(lease: &NativeReaderSessionLease) {
    wait_for(|| lease.coordinator.released()).await;
}

#[tokio::test]
async fn direct_supplier_requires_no_go_socket_and_admits_once() {
    let fixture = Fixture::new();
    let lease = fixture.acquire().await;
    assert_eq!(lease.identity.iccid.as_str(), "8985200014631959072");
    assert_eq!(fixture.supplier.admitted.load(Ordering::SeqCst), 1);
    lease.release().await.unwrap();
    assert!(!fixture.busy());
}
#[tokio::test]
async fn cancelled_acquire_cannot_activate_after_native_admission_returns() {
    let fixture = Fixture::new();
    let hold = Arc::new(tokio::sync::Notify::new());
    *fixture.supplier.admission_hold.lock().unwrap() = Some(hold.clone());
    let gate = fixture.gate.clone();
    let owner = fixture.owner.clone();
    let acquire = tokio::spawn(async move {
        NativeReaderSessionLease::acquire(Path::new("no-go.sock"), None, READER, gate, owner).await
    });
    wait_for(|| fixture.supplier.admitted.load(Ordering::SeqCst) == 1).await;
    acquire.abort();
    let _ = acquire.await;
    assert!(fixture.busy());
    hold.notify_one();
    wait_for(|| !fixture.busy()).await;
    assert_eq!(fixture.supplier.admitted.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn rejected_native_admission_releases_exact_gate_owner() {
    let fixture = Fixture::new();
    *fixture.supplier.rejection.lock().unwrap() = Some("reader IMS session is busy".into());
    let error = fixture
        .acquire_policy(NativePinPolicy::Identity)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("reader IMS session is busy"));
    assert!(!fixture.busy());
}
#[tokio::test]
async fn malformed_snapshot_never_activates_native_admission() {
    for fields in [
        json!({"reader":"different"}),
        json!({"reader_generation":3}),
        json!({"identity":{"iccid":"bad","imsi":"234100896392986","mcc":"234","mnc":"33"}}),
        json!({"notification_aids":["not-hex"]}),
    ] {
        let fixture = Fixture::new();
        *fixture.supplier.snapshot_override.lock().unwrap() = Some(fields);
        assert!(
            fixture
                .acquire_policy(NativePinPolicy::Identity)
                .await
                .is_err()
        );
        assert!(!fixture.busy());
    }
}
#[tokio::test]
async fn identity_policy_remains_strict_while_card_and_mutation_accept_blank_identity() {
    for policy in [
        NativePinPolicy::Identity,
        NativePinPolicy::CardOnly,
        NativePinPolicy::Mutation,
    ] {
        let fixture = Fixture::new();
        *fixture.supplier.snapshot_override.lock().unwrap() =
            Some(json!({"identity":{"iccid":"","imsi":"","mcc":"","mnc":""}}));
        let acquired = fixture.acquire_policy(policy).await;
        if policy == NativePinPolicy::Identity {
            assert!(acquired.is_err());
        } else {
            acquired.unwrap().release().await.unwrap();
        }
        assert!(!fixture.busy());
    }
}
#[tokio::test]
async fn native_ttl_and_local_renew_never_require_compatibility_rpc() {
    let fixture = Fixture::new();
    let lease = fixture.acquire().await;
    lease.renew().await.unwrap();
    lease.coordinator.state.lock().unwrap().deadline = Instant::now();
    lease.coordinator.notify();
    released(&lease).await;
    assert!(lease.renew().await.is_err());
    assert!(!fixture.busy());
}
#[tokio::test]
async fn go_restart_preserves_native_pin_identity_and_selection() {
    let fixture = Fixture::new();
    let lease = fixture.acquire().await;
    fixture.gate.test_confirm_go_exit("go-replacement-producer");
    assert!(
        fixture
            .gate
            .native_pin_producer_current(&lease.coordinator.binding)
    );
    lease.renew().await.unwrap();
    assert!(lease.physical_work_guard().is_ok());
    assert_eq!(
        fixture.gate.native_reader_producer().as_deref(),
        Some(PRODUCER)
    );
    lease.release().await.unwrap();
    assert!(!fixture.busy());
}
#[tokio::test]
async fn finalizer_cancel_retains_owner_does_not_repeat_and_rejects_new_payload() {
    let fixture = Fixture::new();
    let lease = Arc::new(fixture.acquire().await);
    let hold = Arc::new(tokio::sync::Notify::new());
    *fixture.supplier.finalize_hold.lock().unwrap() = Some(hold.clone());
    let tasklease = lease.clone();
    let task = tokio::spawn(async move {
        tasklease
            .finalize_esim_switch("8985200014631959072", "A0000005591010")
            .await
    });
    wait_for(|| fixture.supplier.finalized.load(Ordering::SeqCst) == 1).await;
    task.abort();
    let _ = task.await;
    assert!(
        lease
            .finalize_esim_switch("8985200014631959073", "A0000005591010")
            .await
            .is_err()
    );
    lease.set_release_timeout(Duration::from_millis(15));
    assert!(lease.release().await.is_err());
    assert!(fixture.busy());
    fixture
        .gate
        .test_confirm_go_exit("new-go-during-finalization");
    hold.notify_one();
    released(&lease).await;
    assert_eq!(fixture.supplier.finalized.load(Ordering::SeqCst), 1);
    assert!(!fixture.busy());
}
#[tokio::test]
async fn claimed_success_without_new_generation_invalidates_old_selection() {
    let fixture = Fixture::new();
    fixture.supplier.commit.store(false, Ordering::SeqCst);
    let lease = fixture.acquire().await;
    assert!(
        lease
            .finalize_esim_switch("8985200014631959072", "A0000005591010")
            .await
            .is_err()
    );
    released(&lease).await;
    assert!(fixture.gate.native_reader_snapshot().0.reader.is_empty());
}
#[tokio::test]
async fn native_finalizer_failure_is_not_replayed() {
    let fixture = Fixture::new();
    fixture
        .supplier
        .finalize_error
        .store(true, Ordering::SeqCst);
    let lease = fixture.acquire().await;
    let error = lease
        .finalize_esim_switch("8985200014631959072", "A0000005591010")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("native finalizer failed"));
    released(&lease).await;
    assert!(
        lease
            .finalize_esim_switch("8985200014631959072", "A0000005591010")
            .await
            .is_err()
    );
    assert_eq!(fixture.supplier.finalized.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn finalizer_receipt_waits_for_actual_bridge_worker() {
    let fixture = Fixture::new();
    let lease = Arc::new(fixture.acquire().await);
    let worker = fixture.gate.track_ble_io(READER).unwrap();
    // Finalize admission refuses already-running work.
    assert!(
        lease
            .finalize_esim_switch("8985200014631959072", "A0000005591010")
            .await
            .is_err()
    );
    drop(worker);
    let hold = Arc::new(tokio::sync::Notify::new());
    *fixture.supplier.finalize_hold.lock().unwrap() = Some(hold.clone());
    let tasklease = lease.clone();
    let task = tokio::spawn(async move {
        tasklease
            .finalize_esim_switch("8985200014631959072", "A0000005591010")
            .await
    });
    wait_for(|| fixture.supplier.finalized.load(Ordering::SeqCst) == 1).await;
    let actual = fixture.gate.track_ble_io(READER).unwrap();
    hold.notify_one();
    sleep(Duration::from_millis(30)).await;
    assert!(!task.is_finished());
    assert!(fixture.busy());
    drop(actual);
    task.await.unwrap().unwrap();
    released(&lease).await;
}
#[tokio::test]
async fn weak_supplier_does_not_create_gate_cycle_and_missing_supplier_is_fail_closed() {
    let fixture = Fixture::new();
    let weak = Arc::downgrade(&fixture.supplier);
    let gate = fixture.gate.clone();
    drop(fixture);
    assert!(weak.upgrade().is_none());
    assert!(gate.native_session_supplier().is_err());
}
#[tokio::test]
async fn cancelled_running_worker_retains_gate_until_actual_completion() {
    let fixture = Fixture::new();
    let lease = Arc::new(fixture.acquire().await);
    let (started, start) = tokio::sync::oneshot::channel();
    let (finish, done) = std::sync::mpsc::channel();
    let worker_lease = lease.clone();
    let worker = tokio::spawn(async move {
        worker_lease
            .run_card_worker(&crate::blocking_pool::BlockingPool::new(1), move || {
                let _ = started.send(());
                done.recv().unwrap();
            })
            .await
    });
    start.await.unwrap();
    worker.abort();
    let _ = worker.await;
    lease.set_release_timeout(Duration::from_millis(20));
    assert!(lease.release().await.is_err());
    assert!(fixture.busy());
    finish.send(()).unwrap();
    released(&lease).await;
    assert!(!fixture.busy());
}

#[tokio::test]
async fn concurrent_card_work_waits_in_lease_queue_and_runs_serially() {
    let fixture = Fixture::new();
    let lease = Arc::new(fixture.acquire().await);
    let pool = crate::blocking_pool::BlockingPool::new(2);
    let (first_started, first_ready) = tokio::sync::oneshot::channel();
    let (release_first, first_wait) = std::sync::mpsc::channel();
    let first_lease = lease.clone();
    let first_pool = pool.clone();
    let first = tokio::spawn(async move {
        first_lease
            .run_card_worker(&first_pool, move || {
                first_started.send(()).unwrap();
                first_wait.recv().unwrap();
            })
            .await
    });
    first_ready.await.unwrap();

    let second_started_flag = Arc::new(AtomicBool::new(false));
    let second_started_worker = second_started_flag.clone();
    let (second_started, second_ready) = tokio::sync::oneshot::channel();
    let (release_second, second_wait) = std::sync::mpsc::channel();
    let second_lease = lease.clone();
    let second_pool = pool.clone();
    let second = tokio::spawn(async move {
        second_lease
            .run_card_worker(&second_pool, move || {
                second_started_worker.store(true, Ordering::Release);
                second_started.send(()).unwrap();
                second_wait.recv().unwrap();
            })
            .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        !second_started_flag.load(Ordering::Acquire),
        "a second physical transaction must wait instead of racing the reader"
    );

    release_first.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), second_ready)
        .await
        .unwrap()
        .unwrap();
    release_second.send(()).unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    lease.release().await.unwrap();
}

#[tokio::test]
async fn queued_card_work_revalidates_after_lease_drain() {
    let fixture = Fixture::new();
    let lease = Arc::new(fixture.acquire().await);
    let pool = crate::blocking_pool::BlockingPool::new(2);
    let (first_started, first_ready) = tokio::sync::oneshot::channel();
    let (release_first, first_wait) = std::sync::mpsc::channel();
    let first_lease = lease.clone();
    let first_pool = pool.clone();
    let first = tokio::spawn(async move {
        first_lease
            .run_card_worker(&first_pool, move || {
                first_started.send(()).unwrap();
                first_wait.recv().unwrap();
            })
            .await
    });
    first_ready.await.unwrap();

    let queued_operation_called = Arc::new(AtomicBool::new(false));
    let operation_called = queued_operation_called.clone();
    let second_lease = lease.clone();
    let second_pool = pool.clone();
    let second = tokio::spawn(async move {
        second_lease
            .run_card_worker(&second_pool, move || {
                operation_called.store(true, Ordering::Release);
            })
            .await
    });
    tokio::task::yield_now().await;

    lease.set_release_timeout(Duration::from_millis(20));
    assert!(lease.release().await.is_err());
    release_first.send(()).unwrap();
    first.await.unwrap().unwrap();
    assert!(second.await.unwrap().is_err());
    released(&lease).await;
    assert!(
        !queued_operation_called.load(Ordering::Acquire),
        "queued work must not execute after lease drain"
    );
    assert!(!fixture.busy());
}

#[tokio::test]
async fn physical_card_transaction_queue_rejects_overflow_without_growth() {
    let fixture = Fixture::new();
    let lease = Arc::new(fixture.acquire().await);
    let pool = crate::blocking_pool::BlockingPool::new(1);
    let release_state = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let mut jobs = Vec::new();

    for _ in 0..PHYSICAL_TRANSACTION_CAPACITY {
        let job_lease = lease.clone();
        let job_pool = pool.clone();
        let state = release_state.clone();
        jobs.push(tokio::spawn(async move {
            job_lease
                .run_card_worker(&job_pool, move || {
                    let (lock, ready) = &*state;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = ready.wait(released).unwrap();
                    }
                })
                .await
        }));
    }

    tokio::time::timeout(Duration::from_secs(2), async {
        while lease.coordinator.transaction_slots.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let overflow_lease = lease.clone();
    let overflow_pool = pool.clone();
    assert_eq!(
        overflow_lease
            .run_card_worker(&overflow_pool, || ())
            .await
            .unwrap_err(),
        crate::reader_task::ReaderTaskError::Busy,
        "the queue must fail fast at its fixed capacity"
    );

    {
        let (lock, ready) = &*release_state;
        *lock.lock().unwrap() = true;
        ready.notify_all();
    }
    for job in jobs {
        job.await.unwrap().unwrap();
    }
    assert_eq!(
        lease.coordinator.transaction_slots.available_permits(),
        PHYSICAL_TRANSACTION_CAPACITY
    );
    lease.release().await.unwrap();
}

#[derive(Default)]
struct PhysicalTrace {
    ended: AtomicBool,
    end_gate: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
    open_gate: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
    transactions: AtomicUsize,
}
struct FakePhysical(Arc<PhysicalTrace>);
impl proletarius_sim::ApduTransport for FakePhysical {
    type Error = crate::reader_pcsc::Failure;
    fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Ok(vec![0x90, 0])
    }
}
impl crate::reader_pcsc::Physical for FakePhysical {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(
            &mut dyn proletarius_sim::ApduTransport<Error = crate::reader_pcsc::Failure>,
        ),
    ) -> Result<(), crate::reader_pcsc::Failure> {
        self.0.transactions.fetch_add(1, Ordering::SeqCst);
        body(self);
        if let Some((started, finish)) = self.0.end_gate.lock().unwrap().take() {
            let _ = started.send(());
            finish.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        self.0.ended.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn close(&mut self) -> Result<(), crate::reader_pcsc::Failure> {
        Ok(())
    }
}
fn pcsc_fixture() -> (Fixture, Arc<PhysicalTrace>) {
    let trace = Arc::new(PhysicalTrace::default());
    let opened = trace.clone();
    let reset = trace.clone();
    let owner = crate::reader_pcsc::Owner::with_factory(
        PRODUCER.into(),
        Arc::new(move |_| {
            if let Some((started, finish)) = opened.open_gate.lock().unwrap().take() {
                let _ = started.send(());
                finish.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            Ok(Box::new(FakePhysical(opened.clone())))
        }),
        Arc::new(|| Ok(vec!["PCSC fixture".into()])),
        Arc::new(move |_, _| Ok(Box::new(FakePhysical(reset.clone())))),
    );
    let session = owner.native_open("PCSC fixture").unwrap();
    (
        Fixture::for_owner("PCSC fixture", owner, Some(session)),
        trace,
    )
}
#[tokio::test]
async fn direct_pcsc_supplier_borrows_same_physical_owner_and_exact_binding() {
    let (fixture, trace) = pcsc_fixture();
    let lease = fixture.acquire().await;
    let binding = lease.pcsc.as_ref().unwrap();
    assert_eq!(binding.lease_id, lease.coordinator.binding.lease_id);
    let context = lease.pcsc_context(fixture.owner.clone()).unwrap();
    lease
        .run_card_worker(&crate::blocking_pool::BlockingPool::new(1), move || {
            context.with_transaction(|_| ())
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trace.transactions.load(Ordering::SeqCst), 1);
    lease.release().await.unwrap();
    assert!(trace.ended.load(Ordering::SeqCst));
    assert!(!fixture.busy());
}
#[tokio::test]
async fn pcsc_end_transaction_delays_lease_release_and_retains_cleanup_after_waiter_timeout() {
    let (fixture, trace) = pcsc_fixture();
    let lease = Arc::new(fixture.acquire().await);
    let (started, start) = tokio::sync::oneshot::channel();
    let (finish, done) = std::sync::mpsc::channel();
    *trace.end_gate.lock().unwrap() = Some((started, done));
    lease.set_release_timeout(Duration::from_millis(15));
    let releaser = lease.clone();
    let task = tokio::spawn(async move { releaser.release().await });
    start.await.unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(fixture.busy());
    assert!(!trace.ended.load(Ordering::SeqCst));
    finish.send(()).unwrap();
    released(&lease).await;
    assert!(trace.ended.load(Ordering::SeqCst));
    assert!(!fixture.busy());
}
#[tokio::test]
async fn cancelled_pcsc_acquisition_retains_binding_and_prevents_transaction_after_slow_open() {
    let (fixture, trace) = pcsc_fixture();
    let (started, start) = tokio::sync::oneshot::channel();
    let (finish, done) = std::sync::mpsc::channel();
    *trace.open_gate.lock().unwrap() = Some((started, done));
    let gate = fixture.gate.clone();
    let owner = fixture.owner.clone();
    let task = tokio::spawn(async move {
        NativeReaderSessionLease::acquire(
            Path::new("no-go.sock"),
            None,
            "PCSC fixture",
            gate,
            owner,
        )
        .await
    });
    start.await.unwrap();
    task.abort();
    let _ = task.await;
    assert!(fixture.busy());
    finish.send(()).unwrap();
    wait_for(|| !fixture.busy()).await;
    assert_eq!(trace.transactions.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.supplier.admitted.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn snapshot_cannot_rebind_to_another_pcsc_session_or_producer() {
    for field in ["session_id", "producer"] {
        let (fixture, trace) = pcsc_fixture();
        let mut session = serde_json::to_value(fixture.supplier.session.as_ref().unwrap()).unwrap();
        session[field] = json!("unrelated-native-session");
        *fixture.supplier.snapshot_override.lock().unwrap() = Some(json!({"session":session}));
        assert!(
            fixture
                .acquire_policy(NativePinPolicy::Identity)
                .await
                .is_err()
        );
        assert!(!fixture.busy());
        assert_eq!(trace.transactions.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn mutation_preparation_stops_existing_identity_owner_before_pin_without_stealing_it() {
    let fixture = Fixture::new();
    let old = Arc::new(fixture.acquire().await);
    *fixture.supplier.stop_lease.lock().unwrap() = Some(Arc::downgrade(&old));
    let lease = fixture
        .acquire_policy(NativePinPolicy::Mutation)
        .await
        .unwrap();
    assert!(old.release_confirmed().await.unwrap());
    assert!(fixture.busy());
    assert_ne!(
        old.coordinator.binding.lease_id,
        lease.coordinator.binding.lease_id
    );
    assert_eq!(fixture.supplier.pre_entered.load(Ordering::SeqCst), 2);
    lease.release().await.unwrap();
}
#[tokio::test]
async fn cancelled_preparation_retains_pending_slot_until_stop_completes_without_replay() {
    let fixture = Fixture::new();
    let old = Arc::new(fixture.acquire().await);
    *fixture.supplier.stop_lease.lock().unwrap() = Some(Arc::downgrade(&old));
    let hold = Arc::new(tokio::sync::Notify::new());
    *fixture.supplier.pre_hold.lock().unwrap() = Some(hold.clone());
    let gate = fixture.gate.clone();
    let owner = fixture.owner.clone();
    let task = tokio::spawn(async move {
        NativeReaderSessionLease::acquire_for_esim_mutation(
            Path::new("no-go.sock"),
            None,
            READER,
            gate,
            owner,
        )
        .await
    });
    wait_for(|| fixture.supplier.pre_entered.load(Ordering::SeqCst) == 2).await;
    assert!(!old.release_confirmed().await.unwrap());
    task.abort();
    let _ = task.await;
    assert!(
        fixture
            .acquire_policy(NativePinPolicy::Mutation)
            .await
            .is_err()
    );
    assert_eq!(fixture.supplier.pre_entered.load(Ordering::SeqCst), 2);
    hold.notify_one();
    wait_for(|| !fixture.busy()).await;
    assert!(old.release_confirmed().await.unwrap());
    assert_eq!(
        fixture.supplier.admitted.load(Ordering::SeqCst),
        1,
        "cancelled preflight must never pin a second lease"
    );
}
#[tokio::test]
async fn preparation_generation_change_rejects_late_result_without_repeating_stop() {
    let fixture = Fixture::new();
    let hold = Arc::new(tokio::sync::Notify::new());
    *fixture.supplier.pre_hold.lock().unwrap() = Some(hold.clone());
    let gate = fixture.gate.clone();
    let owner = fixture.owner.clone();
    let task = tokio::spawn(async move {
        NativeReaderSessionLease::acquire_for_esim_mutation(
            Path::new("no-go.sock"),
            None,
            READER,
            gate,
            owner,
        )
        .await
    });
    wait_for(|| fixture.supplier.pre_entered.load(Ordering::SeqCst) == 1).await;
    fixture.gate.test_seed_selection(READER, 5);
    hold.notify_one();
    assert!(task.await.unwrap().is_err());
    assert!(!fixture.busy());
    assert_eq!(fixture.supplier.pre_entered.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.supplier.admitted.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn validation_panic_never_releases_an_existing_physical_owner() {
    let fixture = Fixture::new();
    let old = Arc::new(fixture.acquire().await);
    fixture.supplier.pre_panic.store(true, Ordering::SeqCst);
    assert!(
        fixture
            .acquire_policy(NativePinPolicy::Mutation)
            .await
            .is_err()
    );
    assert!(!old.release_confirmed().await.unwrap());
    assert!(fixture.busy());
    assert_eq!(fixture.supplier.admitted.load(Ordering::SeqCst), 1);
    old.release().await.unwrap();
    assert!(!fixture.busy());
}

#[tokio::test]
async fn ordinary_acquisition_receipt_survives_waiter_cancel_and_waits_for_exact_slow_physical_owner()
 {
    let (fixture, trace) = pcsc_fixture();
    let ordinary = crate::ordinary_vowifi_admission::Gate::default();
    let claim = ordinary
        .acquire_native_reader("ordinary-fixture", 1, &fixture.reader, 4)
        .unwrap();
    let cleanup = AcquisitionCleanup::new(fixture.gate.clone());
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (complete, done) = std::sync::mpsc::channel();
    *trace.open_gate.lock().unwrap() = Some((entered, done));
    let gate = fixture.gate.clone();
    let owner = fixture.owner.clone();
    let reader = fixture.reader.clone();
    let captured_claim = claim.clone();
    let captured_cleanup = cleanup.clone();
    let waiter = tokio::spawn(async move {
        NativeReaderSessionLease::acquire_for_ordinary(
            Path::new("no-go.sock"),
            None,
            &reader,
            gate,
            owner,
            captured_claim,
            captured_cleanup,
        )
        .await
    });
    ready.await.unwrap();
    assert!(!cleanup.confirmed());
    waiter.abort();
    assert!(waiter.await.err().expect("cancelled caller").is_cancelled());
    assert!(
        !cleanup.confirmed(),
        "cancel is not exact physical release proof"
    );
    assert!(fixture.busy());
    assert!(claim.is_current());
    complete.send(()).unwrap();
    wait_for(|| cleanup.confirmed()).await;
    assert!(!fixture.busy());
    assert_eq!(trace.transactions.load(Ordering::SeqCst), 0);
    claim.release_confirmed().unwrap();
}
