use super::*;
use crate::native_vowifi_supervisor::SessionGeneration;
use crate::{reader_pcsc, reader_reservation, reader_session_lease};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Notify;

struct NoIo(Arc<AtomicUsize>);
impl proletarius_sim::ApduTransport for NoIo {
    type Error = reader_pcsc::Failure;
    fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("service fixture must never issue APDU");
    }
}
impl reader_pcsc::Physical for NoIo {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn proletarius_sim::ApduTransport<Error = reader_pcsc::Failure>),
    ) -> Result<(), reader_pcsc::Failure> {
        body(self);
        Ok(())
    }
    fn close(&mut self) -> Result<(), reader_pcsc::Failure> {
        Ok(())
    }
}
struct Identity(Mutex<intent::ExpectedIdentity>);
impl IdentitySource for Identity {
    fn capture(&self) -> Result<intent::ExpectedIdentity, String> {
        Ok(self.0.lock().unwrap().clone())
    }
}
struct Supplier {
    ordinary_owner: Mutex<Option<std::sync::Weak<Inner>>>,
    maintenance: Mutex<Option<std::sync::Weak<crate::ordinary_vowifi_admission::Maintenance>>>,
    session: reader_pcsc::LifecycleBinding,
    held: AtomicBool,
    resume: Notify,
    entered: Notify,
    pre_calls: AtomicUsize,
}
impl reader_session_lease::NativeSessionSupplier for Supplier {
    fn capture_reader_maintenance(
        &self,
        reader: &str,
        generation: u64,
        policy: reader_reservation::NativePinPolicy,
        ordinary: Option<&Arc<crate::ordinary_vowifi_admission::Claim>>,
    ) -> Result<
        Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
        proletarius_domain::BackendFailure,
    > {
        let Some(inner) = self
            .ordinary_owner
            .lock()
            .unwrap()
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        else {
            return Ok(None);
        };
        let gate = &inner.resources.ordinary_admission;
        if let Some(claim) = ordinary {
            gate.validate_reader_claim(claim, reader, generation)
                .map_err(
                    |e| proletarius_domain::BackendFailure::ReaderAdmissionRejected {
                        code: e.code,
                    },
                )?;
            return Ok(None);
        }
        let policy = if policy == reader_reservation::NativePinPolicy::Mutation {
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain
        } else {
            crate::ordinary_vowifi_admission::MaintenancePolicy::Reject
        };
        let maintenance = gate
            .begin_reader_maintenance(reader, generation, policy)
            .map_err(
                |e| proletarius_domain::BackendFailure::ReaderAdmissionRejected { code: e.code },
            )?;
        *self.maintenance.lock().unwrap() = Some(Arc::downgrade(&maintenance));
        Ok(Some(maintenance))
    }
    fn drain_reader_maintenance(
        self: Arc<Self>,
        maintenance: Arc<crate::ordinary_vowifi_admission::Maintenance>,
    ) -> reader_session_lease::AdmissionFuture {
        Box::pin(async move {
            let inner = self
                .ordinary_owner
                .lock()
                .unwrap()
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .ok_or("fixture service unavailable")?;
            Service(inner)
                .stop_for_maintenance(
                    &maintenance,
                    tokio::time::Instant::now() + Duration::from_secs(2),
                )
                .await
        })
    }
    fn pre_admit(
        self: Arc<Self>,
        _: String,
        _: reader_reservation::NativePinPolicy,
        _ordinary: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
        _maintenance: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
    ) -> reader_session_lease::PreAdmissionFuture {
        Box::pin(async move {
            self.pre_calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_waiters();
            loop {
                let wait = self.resume.notified();
                tokio::pin!(wait);
                wait.as_mut().enable();
                if !self.held.load(Ordering::SeqCst) {
                    break;
                }
                wait.await;
            }
            Ok(4)
        })
    }
    fn admit(
        self: Arc<Self>,
        _: reader_reservation::NativePinBinding,
    ) -> reader_session_lease::AdmissionFuture {
        Box::pin(async { Ok(()) })
    }
    fn snapshot(
        &self,
        b: &reader_reservation::NativePinBinding,
    ) -> Result<serde_json::Value, String> {
        // Valid lease identity; the request deliberately has an invalid ePDG
        // hostname so production prepare rejects before proxy/DNS/card AKA.
        Ok(
            json!({"reader":b.reader,"reader_generation":b.reader_generation,"session":self.session,
            "identity":{"iccid":"894433000000000001","imsi":"234330000000001","mcc":"234","mnc":"33"}}),
        )
    }
    fn finalize(
        self: Arc<Self>,
        _: reader_reservation::NativePinBinding,
        _: Option<reader_session_lease::NativeCardContext>,
        _: String,
        _: String,
    ) -> reader_session_lease::SupplierFuture {
        Box::pin(async { Err("fixture forbids mutation".into()) })
    }
}
pub(super) struct Fixture {
    pub(super) service: Service,
    lifecycle: Arc<NativeLifecycle>,
    identity: Arc<Identity>,
    supplier: Arc<Supplier>,
    apdu: Arc<AtomicUsize>,
}
impl Fixture {
    pub(super) fn assert_no_dispatch(&self) {
        assert_eq!(self.supplier.pre_calls.load(Ordering::SeqCst), 0);
        assert_eq!(self.apdu.load(Ordering::SeqCst), 0);
    }
    pub(super) fn new(held: bool) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        let id = NEXT.fetch_add(1, Ordering::SeqCst);
        let reader = format!("native-service-reader-{id}");
        let apdu = Arc::new(AtomicUsize::new(0));
        let count = apdu.clone();
        let inventory = reader.clone();
        let physical = reader_pcsc::Owner::with_factory(
            "native-service-test-producer-000001".into(),
            Arc::new(move |_| Ok(Box::new(NoIo(count.clone())))),
            Arc::new(move || Ok(vec![inventory.clone()])),
            Arc::new(|_, _| panic!("fixture forbids reset")),
        );
        let session = physical.native_open(&reader).unwrap();
        let gate = Arc::new(reader_reservation::Gate::default());
        gate.initialize_business_producer("legacy-fixture");
        gate.initialize_native_reader_producer("native-service-test-producer-000001");
        gate.test_seed_selection(&reader, 4);
        let supplier = Arc::new(Supplier {
            ordinary_owner: Mutex::new(None),
            maintenance: Mutex::new(None),
            session,
            held: AtomicBool::new(held),
            resume: Notify::new(),
            entered: Notify::new(),
            pre_calls: AtomicUsize::new(0),
        });
        let erased: Arc<dyn reader_session_lease::NativeSessionSupplier> = supplier.clone();
        gate.set_native_session_supplier(Arc::downgrade(&erased));
        let identity = Arc::new(Identity(Mutex::new(intent::ExpectedIdentity {
            reader,
            reader_generation: 4,
            iccid: "894433000000000001".into(),
            imsi: "234330000000001".into(),
        })));
        let data_root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "tmp/iteration-484-service-{id}-{}",
            crate::snapshot::new_instance_id()
        ));
        let lifecycle = Arc::new(NativeLifecycle::default());
        lifecycle.activate().unwrap();
        let resources = Resources {
            legacy_module_claim: Arc::new(|| false),
            ordinary_admission: lifecycle.ordinary_admission(),
            identity: identity.clone(),
            lifecycle: Arc::downgrade(&lifecycle),
            gate,
            pcsc: physical,
            socket: PathBuf::from("unused.sock"),
            token: None,
            data_root: data_root.clone(),
            transactions: Default::default(),
            calls: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(data_root.join("archive.json")),
                "native-service-fixture".into(),
            ),
        };
        Self {
            service: Service(Arc::new(Inner {
                sms_epoch: sms::new_epoch().unwrap(),
                resources,
                state: Mutex::new(State::default()),
                mutation: tokio::sync::Mutex::new(()),
            })),
            lifecycle,
            identity,
            supplier,
            apdu,
        }
    }
    fn enable_maintenance_supplier(&self) {
        *self.supplier.ordinary_owner.lock().unwrap() = Some(Arc::downgrade(&self.service.0));
    }
    async fn maintenance(&self) -> Arc<crate::ordinary_vowifi_admission::Maintenance> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(maintenance) = self
                    .supplier
                    .maintenance
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
                {
                    return maintenance;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap()
    }
    fn driver(&self) -> Arc<LiveDriver> {
        Arc::new(LiveDriver {
            ordinary_claim: None,
            resources: self.service.0.resources.clone(),
            current: Mutex::new(AttemptState::default()),
        })
    }
    fn attempt(&self) -> intent::Attempt {
        intent::Attempt {
            intent_id: 1,
            number: 0,
            expected: self.identity.capture().unwrap(),
            request: request(),
        }
    }
    pub(super) fn resume(&self) {
        self.supplier.held.store(false, Ordering::SeqCst);
        self.supplier.resume.notify_waiters();
    }
    pub(super) async fn entered(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let wait = self.supplier.entered.notified();
                tokio::pin!(wait);
                wait.as_mut().enable();
                if self.supplier.pre_calls.load(Ordering::SeqCst) >= count {
                    break;
                }
                wait.await;
            }
        })
        .await
        .unwrap();
    }
    async fn lease(&self) -> Arc<NativeReaderSessionLease> {
        Arc::new(
            NativeReaderSessionLease::acquire(
                Path::new("unused.sock"),
                None,
                &self.identity.capture().unwrap().reader,
                self.service.0.resources.gate.clone(),
                self.service.0.resources.pcsc.clone(),
            )
            .await
            .unwrap(),
        )
    }
    fn assert_idle(&self) {
        assert!(!self.service.0.resources.gate.native_reader_session_active());
        assert_eq!(self.apdu.load(Ordering::SeqCst), 0);
    }
    pub(super) fn dispatch_count(&self) -> usize {
        self.supplier.pre_calls.load(Ordering::SeqCst)
    }
    pub(super) async fn close(self) {
        self.service.begin_shutdown();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        self.service.finish_before(deadline).await.unwrap();
        self.lifecycle.finish_shutdown(deadline).await.unwrap();
        self.assert_idle();
        self.service
            .0
            .resources
            .pcsc
            .native_close(&self.supplier.session)
            .unwrap();
    }
}
pub(super) fn request() -> intent::FrozenRequest {
    intent::FrozenRequest {
        proxy: "invalid-never-discovered".into(),
        epdg: "invalid/never-resolved.invalid".into(),
        epdg_port: 5443,
    }
}
async fn run(
    driver: Arc<LiveDriver>,
    attempt: intent::Attempt,
    stop: watch::Receiver<bool>,
) -> intent::AttemptExit {
    let (progress, _) = watch::channel(intent::AttemptProgress::Starting);
    tokio::time::timeout(Duration::from_secs(2), driver.run(attempt, stop, progress))
        .await
        .unwrap()
}

#[tokio::test]
async fn real_acquire_prepare_failure_releases_exact_native_lease_before_terminal_result() {
    let f = Fixture::new(false);
    let driver = f.driver();
    let attempt = f.attempt();
    let (_stop, stop) = watch::channel(false);
    let exit = run(driver.clone(), attempt.clone(), stop).await;
    assert!(
        matches!(exit.reason,intent::EndReason::StartupFailed {ref error} if error=="native_carrier_selection_failed")
    );
    assert!(exit.cleanup_confirmed);
    f.assert_idle();
    assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 1);
    let lease = driver
        .current
        .lock()
        .unwrap()
        .unlaunched_lease
        .clone()
        .unwrap();
    assert!(
        lease
            .ordinary_lease()
            .unwrap()
            .release_confirmed()
            .await
            .unwrap()
    );
    driver.clone().await_cleanup(attempt).await.unwrap();
    let replacement = f.lease().await;
    replacement.release_with_confirmation().await.unwrap();
    f.close().await;
}
#[tokio::test]
async fn stopped_before_start_issues_no_supplier_admission_or_apdu() {
    let f = Fixture::new(true);
    let driver = f.driver();
    let (_stop, stop) = watch::channel(true);
    let exit = run(driver.clone(), f.attempt(), stop).await;
    assert!(matches!(exit.reason, intent::EndReason::Stopped));
    assert!(exit.cleanup_confirmed);
    assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 0);
    assert!(driver.current.lock().unwrap().key.is_none());
    f.close().await;
}
#[tokio::test]
async fn identity_changed_while_real_pre_admission_is_pending_releases_old_lease_without_prepare() {
    let f = Fixture::new(true);
    let driver = f.driver();
    let attempt = f.attempt();
    let (_stop, stop) = watch::channel(false);
    let worker = tokio::spawn(run(driver.clone(), attempt, stop));
    f.entered(1).await;
    assert!(f.service.0.resources.gate.native_reader_session_active());
    {
        let mut identity = f.identity.0.lock().unwrap();
        identity.reader_generation = 5;
        identity.iccid = "894433999999999999".into();
    }
    f.resume();
    let exit = worker.await.unwrap();
    assert!(matches!(exit.reason, intent::EndReason::ReaderChanged));
    assert!(exit.cleanup_confirmed);
    f.close().await;
}
#[tokio::test]
async fn concurrent_start_stop_timeout_and_old_record_stop_keep_exact_intent_owner() {
    let f = Fixture::new(true);
    let (a, b) = tokio::join!(f.service.start(request()), f.service.start(request()));
    assert_ne!(a.is_ok(), b.is_ok());
    f.entered(1).await;
    let old = f.service.record().unwrap().unwrap();
    let expected = old.expected.clone();
    f.service
        .stop(
            Some((&expected.reader, expected.reader_generation + 1)),
            tokio::time::Instant::now() + Duration::from_millis(10),
        )
        .await
        .unwrap();
    assert!(!*old.stop.borrow());
    assert_eq!(
        f.service
            .stop(
                None,
                tokio::time::Instant::now() + Duration::from_millis(10)
            )
            .await
            .unwrap_err(),
        "native_intent_shutdown_unconfirmed"
    );
    assert!(!old.task.lock().await.is_finished());
    assert!(f.service.busy(&expected.reader, expected.reader_generation));
    assert!(f.service.start(request()).await.is_err());
    f.resume();
    f.service
        .finish_before(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    f.assert_idle();
    f.supplier.held.store(true, Ordering::SeqCst);
    f.service.start(request()).await.unwrap();
    f.entered(2).await;
    let new = f.service.record().unwrap().unwrap();
    assert!(new.id > old.id);
    old.request_stop();
    assert!(!*new.stop.borrow());
    assert_eq!(new.progress.borrow().intent_id, new.id);
    new.request_stop();
    f.resume();
    f.service
        .finish_before(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    f.close().await;
}
#[tokio::test]
async fn real_card_worker_release_timeout_is_retained_until_that_worker_actually_finishes() {
    let f = Fixture::new(false);
    let driver = f.driver();
    let attempt = f.attempt();
    driver.current.lock().unwrap().key = Some((attempt.intent_id, attempt.number));
    let lease = f.lease().await;
    lease.set_release_timeout(Duration::from_millis(10));
    let card = lease
        .pcsc_context(f.service.0.resources.pcsc.clone())
        .unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (complete, finish) = std::sync::mpsc::channel();
    let captured = lease.clone();
    let worker = tokio::spawn(async move {
        captured
            .run_card_worker(&crate::blocking_pool::BlockingPool::new(1), move || {
                card.with_transaction(|_| {
                    started.send(()).unwrap();
                    finish.recv_timeout(Duration::from_secs(5)).unwrap();
                })
            })
            .await
    });
    ready.await.unwrap();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    let exit = driver
        .release_unlaunched(
            lease.clone(),
            intent::EndReason::StartupFailed {
                error: "fixture_prepare_failed".into(),
            },
        )
        .await;
    assert!(!exit.cleanup_confirmed);
    assert!(f.service.0.resources.gate.native_reader_session_active());
    assert!(driver.clone().await_cleanup(attempt.clone()).await.is_err());
    assert!(!lease.release_confirmed().await.unwrap());
    complete.send(()).unwrap();
    // Releasing the worker's barrier is not its terminal proof. Observe the
    // same captured lease until the real coordinator acknowledges release;
    // its 10ms observer timeout must remain an error while cleanup is pending.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if driver.clone().await_cleanup(attempt.clone()).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    assert!(lease.release_confirmed().await.unwrap());
    f.close().await;
}
#[tokio::test]
async fn acquisition_unknown_and_wrong_attempt_cannot_ack_until_real_gate_owner_is_idle() {
    let f = Fixture::new(false);
    let driver = f.driver();
    let attempt = f.attempt();
    {
        let mut state = driver.current.lock().unwrap();
        state.key = Some((attempt.intent_id, attempt.number));
        state.acquire_unconfirmed = true;
    }
    let lease = f.lease().await;
    assert!(f.service.0.resources.gate.native_reader_session_active());
    let mut wrong = attempt.clone();
    wrong.number += 1;
    assert_eq!(
        driver.clone().await_cleanup(wrong).await.unwrap_err(),
        "native_attempt_cleanup_identity_mismatch"
    );
    assert!(driver.clone().await_cleanup(attempt.clone()).await.is_err());
    lease.release_with_confirmation().await.unwrap();
    driver.await_cleanup(attempt).await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn global_shutdown_rejects_late_service_start_before_supplier_or_apdu() {
    let f = Fixture::new(true);
    // A separately/lazily constructed service has not seen its own shutdown
    // flag. The activated daemon lifecycle remains the authoritative gate.
    assert!(f.lifecycle.ordinary_service().is_none());
    assert!(f.lifecycle.state.lock().unwrap().owner.is_some());
    f.lifecycle.begin_shutdown().unwrap();
    assert!(!f.service.0.state.lock().unwrap().stopping);
    assert_eq!(
        f.service.start(request()).await.unwrap_err(),
        "native_lifecycle_not_accepting"
    );
    assert!(f.service.record().unwrap().is_none());
    assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 0);
    f.assert_idle();
    f.close().await;
}

async fn wait_released(claim: &crate::ordinary_vowifi_admission::Claim) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while claim.is_current() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
fn go_claim(
    f: &Fixture,
    sequence: u64,
) -> Result<Arc<crate::ordinary_vowifi_admission::Claim>, crate::ordinary_vowifi_admission::Failure>
{
    f.service.0.resources.ordinary_admission.acquire_go(
        "go-service-fixture",
        "reader",
        "go-intent",
        sequence,
    )
}
fn configure_go(f: &Fixture) {
    f.service
        .0
        .resources
        .ordinary_admission
        .set_go_producer("go-service-fixture")
        .unwrap();
}

#[tokio::test]
async fn shared_go_claim_blocks_real_prepare_then_terminal_join_allows_go_handoff() {
    let f = Fixture::new(false);
    configure_go(&f);
    let go = go_claim(&f, 1).unwrap();
    assert!(f.service.start(request()).await.is_err());
    assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 0);
    assert!(f.service.record().unwrap().is_none());
    go.release_confirmed().unwrap();
    f.service.start(request()).await.unwrap();
    let old = f.service.record().unwrap().unwrap();
    // No stop/finish request: the retained observer must harvest the actual
    // prepare failure, exact lease release and task join by itself.
    wait_released(&old.claim).await;
    assert_eq!(old.progress.borrow().phase, intent::Phase::Failed);
    assert!(old.progress.borrow().cleanup_confirmed);
    assert!(old.task.lock().await.is_finished());
    assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 1);
    f.assert_idle();
    assert!(!f.service.has_intent());
    let successor = go_claim(&f, 2).unwrap();
    old.request_stop();
    old.finish_before(tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    assert!(
        successor.is_current(),
        "old completion must not release Go successor"
    );
    assert!(f.service.start(request()).await.is_err());
    assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 1);
    successor.release_confirmed().unwrap();
    f.close().await;
}

#[tokio::test]
async fn cancelled_startup_http_waiter_retains_shared_claim_until_real_pre_worker_cleanup() {
    let f = Fixture::new(true);
    configure_go(&f);
    let service = f.service.clone();
    let waiter = tokio::spawn(async move {
        let id = service.start(request()).await.unwrap();
        service.await_initial_preparation(id).await
    });
    f.entered(1).await;
    let record = f.service.record().unwrap().unwrap();
    waiter.abort();
    assert!(
        waiter
            .await
            .err()
            .expect("cancelled startup waiter")
            .is_cancelled()
    );
    assert!(record.claim.is_current());
    assert!(
        !*record.stop.borrow(),
        "dropping HTTP waiter must not stop retained intent"
    );
    assert!(go_claim(&f, 1).is_err());
    assert!(
        f.service
            .stop(
                None,
                tokio::time::Instant::now() + Duration::from_millis(10)
            )
            .await
            .is_err()
    );
    assert!(record.claim.is_current());
    assert!(f.service.start(request()).await.is_err());
    f.resume();
    wait_released(&record.claim).await;
    f.assert_idle();
    let go = go_claim(&f, 1).unwrap();
    assert!(!f.service.has_intent());
    go.release_confirmed().unwrap();
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_go_and_native_start_have_one_actual_owner_before_any_card_admission() {
    let f = Fixture::new(true);
    configure_go(&f);
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let service = f.service.clone();
    let native_barrier = barrier.clone();
    let native = tokio::spawn(async move {
        native_barrier.wait().await;
        service.start(request()).await
    });
    let gate = f.service.0.resources.ordinary_admission.clone();
    let go = tokio::spawn(async move {
        barrier.wait().await;
        gate.acquire_go("go-service-fixture", "reader", "go-intent", 1)
    });
    let (native, go) = tokio::join!(native, go);
    let (native, go) = (native.unwrap(), go.unwrap());
    assert_ne!(native.is_ok(), go.is_ok());
    if let Ok(go) = go {
        assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 0);
        go.release_confirmed().unwrap();
    } else {
        f.entered(1).await;
        let record = f.service.record().unwrap().unwrap();
        record.request_stop();
        f.resume();
        wait_released(&record.claim).await;
        assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 1);
    }
    f.close().await;
}

struct IntentFixtureDriver {
    first: Option<intent::EndReason>,
    unknown: bool,
    calls: AtomicUsize,
    cleanup_ready: AtomicBool,
    changed: Notify,
}
impl intent::Driver for IntentFixtureDriver {
    fn check_identity(&self, _: &intent::ExpectedIdentity) -> Result<(), String> {
        Ok(())
    }
    fn run(
        self: Arc<Self>,
        attempt: intent::Attempt,
        mut stop: watch::Receiver<bool>,
        progress: watch::Sender<intent::AttemptProgress>,
    ) -> intent::DriverFuture<intent::AttemptExit> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.changed.notify_waiters();
            if self.unknown {
                return intent::AttemptExit {
                    reason: intent::EndReason::StartupFailed {
                        error: "fixture_failure".into(),
                    },
                    cleanup_confirmed: false,
                };
            }
            if attempt.number == 0 {
                if let Some(reason) = &self.first {
                    return intent::AttemptExit {
                        reason: reason.clone(),
                        cleanup_confirmed: true,
                    };
                }
            }
            progress.send_replace(intent::AttemptProgress::Registered(
                SessionGeneration::from_value(10 + u64::from(attempt.number)).unwrap(),
            ));
            while !*stop.borrow_and_update() {
                if stop.changed().await.is_err() {
                    break;
                }
            }
            intent::AttemptExit {
                reason: intent::EndReason::Stopped,
                cleanup_confirmed: true,
            }
        })
    }
    fn await_cleanup(
        self: Arc<Self>,
        _: intent::Attempt,
    ) -> intent::DriverFuture<Result<(), String>> {
        Box::pin(async move {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.cleanup_ready.load(Ordering::SeqCst) {
                    return Ok(());
                }
                changed.await;
            }
        })
    }
}
impl Fixture {
    fn retained_intent(
        &self,
        first: Option<intent::EndReason>,
        unknown: bool,
        closed_watch: bool,
    ) -> (Arc<Record>, Arc<IntentFixtureDriver>) {
        let driver = Arc::new(IntentFixtureDriver {
            first,
            unknown,
            calls: AtomicUsize::new(0),
            cleanup_ready: AtomicBool::new(false),
            changed: Notify::new(),
        });
        let expected = self.identity.capture().unwrap();
        let claim = self
            .service
            .0
            .resources
            .ordinary_admission
            .acquire_native_reader(
                &self.service.0.sms_epoch,
                1,
                &expected.reader,
                expected.reader_generation,
            )
            .unwrap();
        let task = intent::spawn(
            driver.clone(),
            1,
            expected.clone(),
            request(),
            intent::Policy {
                initial_delay: Duration::from_millis(100),
                maximum_delay: Duration::from_millis(100),
                identity_poll: Duration::from_millis(5),
                cleanup_retry: Duration::from_millis(5),
                jitter: Duration::ZERO,
            },
        )
        .unwrap();
        let progress = if closed_watch {
            let (sender, receiver) = watch::channel(task.snapshot());
            drop(sender);
            receiver
        } else {
            task.subscribe()
        };
        let record = Arc::new(Record {
            id: 1,
            expected,
            driver: self.driver(),
            stop: task.stop_sender(),
            progress,
            task: tokio::sync::Mutex::new(task),
            claim,
            observer: Mutex::new(None),
            operation: Mutex::new(super::super::operation::Clock::start(
                tokio::time::Instant::now(),
                Duration::from_secs(90),
            )),
        });
        {
            let mut state = self.service.0.state.lock().unwrap();
            state.next = 1;
            state.record = Some(record.clone());
        }
        record.observe_terminal();
        (record, driver)
    }
}
async fn wait_phase(record: &Record, phase: intent::Phase) {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut progress = record.progress.clone();
        loop {
            if progress.borrow_and_update().phase == phase {
                break;
            }
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn receipt_observation_fences_successor_publication_and_keeps_new_clock_receipt_live() {
    use super::super::operation::{Clock, Options, Receipt, request_fingerprint};
    let f = Fixture::new(false);
    let (first, _) = f.retained_intent(None, false, false);
    first.request_stop();
    first
        .finish_before(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    let expected = f.identity.capture().unwrap();
    let claim = f
        .service
        .0
        .resources
        .ordinary_admission
        .acquire_native_reader(
            &f.service.0.sms_epoch,
            2,
            &expected.reader,
            expected.reader_generation,
        )
        .unwrap();
    let driver = Arc::new(IntentFixtureDriver {
        first: None,
        unknown: false,
        calls: AtomicUsize::new(0),
        cleanup_ready: AtomicBool::new(false),
        changed: Notify::new(),
    });
    let task = intent::spawn(
        driver,
        2,
        expected.clone(),
        request(),
        intent::Policy::default(),
    )
    .unwrap();
    let second = Arc::new(Record {
        id: 2,
        expected,
        driver: f.driver(),
        stop: task.stop_sender(),
        progress: task.subscribe(),
        task: tokio::sync::Mutex::new(task),
        claim,
        observer: Mutex::new(None),
        operation: Mutex::new(Clock::start(
            tokio::time::Instant::now(),
            Duration::from_secs(90),
        )),
    });
    second.observe_terminal();
    let fingerprint = request_fingerprint(br#"{"enable":true}"#);
    let mut state = f.service.0.state.lock().unwrap();
    let old_projection = f
        .service
        .operation_projection_in_state(&state, Some(&first))
        .unwrap();
    state
        .requests
        .insert(Receipt::capture("reader-a", &fingerprint, &old_projection))
        .unwrap();

    let publisher = f.service.clone();
    let published = second.clone();
    let (attempted, observed_attempt) = std::sync::mpsc::channel();
    let admission = std::thread::spawn(move || {
        attempted
            .send(matches!(
                publisher.0.state.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ))
            .unwrap();
        let mut state = publisher.0.state.lock().unwrap();
        state.next = 2;
        state.record = Some(published.clone());
        let projection = publisher
            .operation_projection_in_state(&state, Some(&published))
            .unwrap();
        state
            .requests
            .insert(Receipt::capture("reader-b", &fingerprint, &projection))
            .unwrap();
    });
    assert!(
        observed_attempt
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
    );
    // Publication of B and its receipt is excluded for the entire A sample /
    // ledger-observation transaction, not just for the final lookup.
    let a = f
        .service
        .lookup_request_locked(&mut state, "reader-a", None)
        .unwrap()
        .unwrap();
    let a = a.resolve(Some(old_projection.clone()));
    assert_eq!(a.operation_id, old_projection.operation_id);
    assert_ne!(a.outcome.as_deref(), Some("superseded"));
    drop(state);
    admission.join().unwrap();
    let current_b = f.service.operation_projection().unwrap();
    let b = f
        .service
        .lookup_request("reader-b", None)
        .unwrap()
        .unwrap()
        .resolve(Some(current_b.clone()));
    assert_eq!(b.operation_id, current_b.operation_id);
    assert!(!b.terminal);
    assert_ne!(b.outcome.as_deref(), Some("superseded"));
    let a = f
        .service
        .lookup_request("reader-a", None)
        .unwrap()
        .unwrap()
        .resolve(Some(current_b));
    assert_eq!(a.outcome.as_deref(), Some("superseded"));

    let bytes = br#"{"enable":false,"observe_operation":true,"client_request_id":"reader-b-stop"}"#;
    let options = Options::body(bytes).unwrap();
    let stop = f
        .service
        .request_observed_stop(&options, &request_fingerprint(bytes))
        .unwrap();
    let stop = f
        .service
        .lookup_request("reader-b-stop", None)
        .unwrap()
        .unwrap()
        .resolve(Some(stop));
    assert_eq!(stop.generation, b.generation);
    assert_ne!(stop.operation_id, b.operation_id);
    assert!(!stop.requested_enable);
    assert_ne!(stop.outcome.as_deref(), Some("superseded"));
    let b = f
        .service
        .lookup_request("reader-b", None)
        .unwrap()
        .unwrap()
        .resolve(Some(stop));
    assert_eq!(b.outcome.as_deref(), Some("superseded"));
    f.assert_no_dispatch();
    f.close().await;
}

#[tokio::test]
async fn joined_cleanup_harvest_allows_observation_and_shutdown_and_revalidates_attempt() {
    let f = Fixture::new(false);
    let lease = f.lease().await;
    let sim = SimIdentity {
        mcc: "234".into(),
        mnc: "33".into(),
        imsi: lease.identity.imsi.to_string(),
        iccid: lease.identity.iccid.to_string(),
        ..Default::default()
    };
    let carrier = Registry::embedded()
        .unwrap()
        .resolve(&sim)
        .unwrap()
        .unwrap();
    let (config, runtime) = carrier
        .native_session_tunnel_config(&sim, None, "epdg.fixture.invalid", Some(4500))
        .unwrap();
    let ims = carrier
        .native_ims_register_profile_with_home_domain(&sim, None, None, false)
        .unwrap();
    let context = NativeSessionContext {
        reader: lease.reader.clone(),
        reader_generation: lease.reader_generation,
        session_key: crate::native_vowifi_supervisor::ReaderSessionKey::new(
            &lease.reader,
            &lease.identity.iccid,
        )
        .unwrap(),
        sms_subscription_id: native_device_sms_subscription_id(
            &lease.identity.iccid,
            &lease.identity.imsi,
        ),
        mcc: sim.mcc,
        mnc: sim.mnc,
        carrier_id: carrier.id,
        config,
        runtime,
        access_isim: false,
        subscriber_identity: resolve_session_subscriber_identity(&ims, &lease.identity).unwrap(),
        device_identity: ims
            .resolve_device_identity(Some("356938035643809"))
            .unwrap(),
        ims,
        ike_device_identity_imei: None,
        configuration_status: json!({}),
        transport: SelectedTransport {
            mode: proletarius_vowifi::ProxyMode::Socks5UdpDomain,
            proxy: Some("127.0.0.1:9".parse().unwrap()),
            proxy_error: None,
            proxy_source: None,
        },
        card: SessionCard::leased(lease.clone()),
    };
    let handle = f
        .lifecycle
        .launch_ordinary(
            context,
            ordinary::Inputs {
                sms_archive: Default::default(),
                settings: startup_settings().unwrap(),
                reader_gate: f.service.0.resources.gate.clone(),
                data_root: f.service.0.resources.data_root.clone(),
                transactions: f.service.0.resources.transactions.clone(),
                call_facts: f.service.0.resources.calls.clone(),
            },
        )
        .unwrap_or_else(|_| panic!("cleanup lock-order fixture admission"));
    // This current-thread runtime has not polled the task: stop before any
    // network or AKA work, but retain its actual joined SessionResult.
    handle.request_shutdown();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let joined = {
                let mut state = f.lifecycle.state.lock().unwrap();
                let owner = state.owner.as_mut().unwrap();
                owner.reap_ordinary_tasks();
                owner
                    .ordinary_results
                    .iter()
                    .any(|(generation, _)| *generation == handle.generation)
            };
            if joined {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(released(&handle.snapshot()));
    assert!(lease.release_confirmed().await.unwrap());
    let attempt = f.attempt();
    let driver = f.driver();
    *driver.current.lock().unwrap() = AttemptState {
        key: Some((attempt.intent_id, attempt.number)),
        handle: Some(handle.clone()),
        ..Default::default()
    };
    let expected = attempt.expected.clone();
    let claim = f
        .service
        .0
        .resources
        .ordinary_admission
        .acquire_native_reader(
            &f.service.0.sms_epoch,
            1,
            &expected.reader,
            expected.reader_generation,
        )
        .unwrap();
    let task = intent::spawn(
        Arc::new(IntentFixtureDriver {
            first: None,
            unknown: false,
            calls: AtomicUsize::new(0),
            cleanup_ready: AtomicBool::new(false),
            changed: Notify::new(),
        }),
        1,
        expected.clone(),
        request(),
        intent::Policy::default(),
    )
    .unwrap();
    let record = Arc::new(Record {
        id: 1,
        expected,
        driver: driver.clone(),
        stop: task.stop_sender(),
        progress: task.subscribe(),
        task: tokio::sync::Mutex::new(task),
        claim,
        observer: Mutex::new(None),
        operation: Mutex::new(super::super::operation::Clock::start(
            tokio::time::Instant::now(),
            Duration::from_secs(90),
        )),
    });
    f.service.0.state.lock().unwrap().record = Some(record.clone());
    assert!(
        f.lifecycle
            .ordinary_service
            .set(Ok(f.service.clone()))
            .is_ok()
    );
    driver
        .collect_cleanup_result(&attempt, &handle, || {
            assert!(
                driver.current.try_lock().is_ok(),
                "lifecycle harvest must run without driver.current"
            );
            // Exercise the two other edges of the former lock ring while this
            // real result collector is paused immediately before lifecycle access.
            let observer = f.service.clone();
            let shutdown = f.lifecycle.clone();
            let (finished, observed_finish) = std::sync::mpsc::channel();
            let overlap = std::thread::spawn(move || {
                observer.lookup_request("unknown-observer", None).unwrap();
                shutdown.begin_shutdown().unwrap();
                finished.send(()).unwrap();
            });
            observed_finish
                .recv_timeout(Duration::from_secs(2))
                .expect("observer and shutdown must not wait on the collector's driver lock");
            overlap.join().unwrap();
            f.lifecycle
                .take_ordinary_result(handle.generation)
                .unwrap()
                .map_err(str::to_owned)
        })
        .unwrap();
    assert!(driver.current.lock().unwrap().result.is_some());
    driver.clone().await_cleanup(attempt.clone()).await.unwrap();

    // A concurrent collector may install the exact result while this caller
    // sees no new lifecycle result. Preserve its receipt rather than failing
    // or replacing it with the second collector's transient error.
    let result = driver.current.lock().unwrap().result.take().unwrap();
    driver
        .collect_cleanup_result(&attempt, &handle, || {
            driver.current.lock().unwrap().result = Some(result);
            Err("native_ordinary_worker_join_unconfirmed".into())
        })
        .unwrap();
    driver
        .collect_cleanup_result(&attempt, &handle, || {
            panic!("retained result must be idempotent")
        })
        .unwrap();

    let result = driver.current.lock().unwrap().result.take().unwrap();
    assert_eq!(
        driver.collect_cleanup_result(&attempt, &handle, || {
            driver.current.lock().unwrap().key = Some((attempt.intent_id, attempt.number + 1));
            Ok(result)
        }),
        Err("native_attempt_cleanup_identity_mismatch".into())
    );
    assert!(driver.current.lock().unwrap().result.is_none());
    assert!(
        record.claim.is_current(),
        "identity drift must never acknowledge the intent claim"
    );
    // The fixture deliberately acquires exactly one lease before launching
    // the stopped ordinary worker; observation/cleanup must acquire no more.
    assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.apdu.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn operation_observation_deadline_does_not_stop_or_release_pending_owner() {
    let f = Fixture::new(false);
    configure_go(&f);
    let (record, _) = f.retained_intent(None, false, false);
    *record.operation.lock().unwrap() =
        super::super::operation::Clock::start(tokio::time::Instant::now(), Duration::ZERO);
    // On this current-thread executor the retained intent has not run yet.
    // Expire only its observation clock, without advancing its protocol task.
    let expired = f.service.operation_projection().unwrap();
    assert!(expired.observation_expired);
    assert!(!expired.terminal && !expired.cleanup_confirmed && !expired.succeeded);
    assert!(record.claim.is_current() && !*record.stop.borrow());
    assert!(go_claim(&f, 1).is_err());
    record.request_stop();
    wait_released(&record.claim).await;
    f.close().await;
}

#[tokio::test]
async fn operation_observation_expiry_and_cancel_wait_for_retained_owner_cleanup() {
    let f = Fixture::new(false);
    configure_go(&f);
    let (record, driver) = f.retained_intent(None, true, false);
    wait_phase(&record, intent::Phase::CleanupUnconfirmed).await;
    *record.operation.lock().unwrap() =
        super::super::operation::Clock::start(tokio::time::Instant::now(), Duration::ZERO);
    // Unknown cleanup is a failure fact, never a resource-release receipt.
    let projection = f.service.operation_projection().unwrap();
    assert!(!projection.cleanup_confirmed && !projection.succeeded);
    assert!(record.claim.is_current());
    let original_id = projection.operation_id;
    let cap = super::super::operation::Options {
        observe_operation: Some(true),
        operation_id: Some(original_id.clone()),
        operation_generation: Some(projection.generation),
        client_request_id: None,
    };
    let stopping = f.service.request_observed_stop(&cap, "test").unwrap();
    assert_eq!(
        f.service.request_observed_stop(&cap, "test").unwrap_err(),
        "vowifi_operation_stale"
    );
    assert_ne!(stopping.operation_id, original_id);
    assert!(!stopping.succeeded && !stopping.cleanup_confirmed);
    assert!(record.claim.is_current());
    let waiter_record = record.clone();
    let waiter = tokio::spawn(async move {
        waiter_record
            .finish_before(tokio::time::Instant::now() + Duration::from_secs(30))
            .await
    });
    tokio::task::yield_now().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(record.claim.is_current());
    driver.cleanup_ready.store(true, Ordering::SeqCst);
    driver.changed.notify_waiters();
    wait_released(&record.claim).await;
    let completed = f.service.operation_projection().unwrap();
    assert_eq!(completed.operation_id, stopping.operation_id);
    assert!(completed.terminal && completed.succeeded && completed.cleanup_confirmed);
    assert_eq!(completed.outcome.as_deref(), Some("cancelled"));
    f.close().await;
}

#[tokio::test]
async fn operation_projection_generation_drift_cannot_publish_ready_or_authorize_stale_stop() {
    let f = Fixture::new(false);
    configure_go(&f);
    let (record, _) = f.retained_intent(None, false, false);
    wait_phase(&record, intent::Phase::Registered).await;
    let before = f.service.operation_projection().unwrap();
    f.identity.0.lock().unwrap().reader_generation += 1;
    let drifted = f.service.operation_projection().unwrap();
    assert_eq!(drifted.outcome.as_deref(), Some("superseded"));
    assert!(!drifted.succeeded);
    let stale = super::super::operation::Options {
        observe_operation: Some(true),
        operation_id: Some(before.operation_id),
        operation_generation: Some(before.generation + 1),
        client_request_id: None,
    };
    assert_eq!(stale.validate(&drifted), Err("vowifi_operation_stale"));
    record.request_stop();
    wait_released(&record.claim).await;
    f.close().await;
}

#[tokio::test]
async fn renewal_and_reauthentication_backoff_keep_one_claim_across_actual_intent_attempts() {
    for reason in [
        intent::EndReason::RegistrationRenewal,
        intent::EndReason::PlannedReauthentication,
    ] {
        let f = Fixture::new(false);
        configure_go(&f);
        let (record, driver) = f.retained_intent(Some(reason), false, false);
        wait_phase(&record, intent::Phase::Backoff).await;
        assert!(record.progress.borrow().cleanup_confirmed);
        assert!(
            record.claim.is_current(),
            "attempt release is not whole-intent release"
        );
        assert!(go_claim(&f, 1).is_err());
        wait_phase(&record, intent::Phase::Registered).await;
        assert_eq!(driver.calls.load(Ordering::SeqCst), 2);
        assert!(record.claim.is_current());
        record.request_stop();
        wait_released(&record.claim).await;
        let successor = go_claim(&f, 1).unwrap();
        record
            .finish_before(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        assert!(successor.is_current());
        successor.release_confirmed().unwrap();
        f.close().await;
    }
}

#[tokio::test]
async fn cleanup_unknown_and_cancelled_finish_hold_claim_even_after_record_publication_is_dropped()
{
    let f = Fixture::new(false);
    configure_go(&f);
    let (record, driver) = f.retained_intent(None, true, false);
    wait_phase(&record, intent::Phase::CleanupUnconfirmed).await;
    record.request_stop();
    let waiter_record = record.clone();
    let waiter = tokio::spawn(async move {
        waiter_record
            .finish_before(tokio::time::Instant::now() + Duration::from_secs(30))
            .await
    });
    tokio::task::yield_now().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(record.claim.is_current());
    assert!(go_claim(&f, 1).is_err());
    let claim = record.claim.clone();
    f.service.0.state.lock().unwrap().record = None;
    drop(record);
    assert!(
        claim.is_current(),
        "record/HTTP Drop cannot grant uncertain owner release"
    );
    driver.cleanup_ready.store(true, Ordering::SeqCst);
    driver.changed.notify_waiters();
    wait_released(&claim).await;
    let go = go_claim(&f, 1).unwrap();
    go.release_confirmed().unwrap();
    f.close().await;
}

#[tokio::test]
async fn closed_progress_is_not_release_proof_and_observer_does_not_block_new_start_mutex() {
    let f = Fixture::new(false);
    configure_go(&f);
    let (record, driver) = f.retained_intent(None, false, true);
    tokio::time::timeout(Duration::from_secs(2), async {
        while driver.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Give the closed-watch observer a turn; it must only poll is_finished.
    tokio::task::yield_now().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), f.service.start(request()))
            .await
            .unwrap()
            .is_err()
    );
    assert!(record.claim.is_current());
    assert!(go_claim(&f, 1).is_err());
    record.request_stop();
    wait_released(&record.claim).await;
    let go = go_claim(&f, 1).unwrap();
    go.release_confirmed().unwrap();
    f.close().await;
}

#[tokio::test]
async fn last_service_drop_stops_real_pre_worker_but_claim_waits_for_actual_cleanup() {
    let f = Fixture::new(true);
    configure_go(&f);
    let service = Service(Arc::new(Inner {
        sms_epoch: sms::new_epoch().unwrap(),
        resources: f.service.0.resources.clone(),
        state: Mutex::new(State::default()),
        mutation: tokio::sync::Mutex::new(()),
    }));
    service.start(request()).await.unwrap();
    f.entered(1).await;
    let record = service.record().unwrap().unwrap();
    let owner = Arc::downgrade(&service.0);
    drop(service);
    assert!(owner.upgrade().is_none());
    assert!(*record.stop.borrow());
    assert!(record.claim.is_current());
    assert!(f.service.0.resources.gate.native_reader_session_active());
    assert!(go_claim(&f, 1).is_err());
    f.resume();
    wait_released(&record.claim).await;
    assert!(record.task.lock().await.is_finished());
    f.assert_idle();
    let go = go_claim(&f, 1).unwrap();
    go.release_confirmed().unwrap();
    f.close().await;
}

#[tokio::test]
async fn stop_during_backoff_joins_once_and_old_waiter_cannot_release_a_new_native_owner() {
    let f = Fixture::new(true);
    configure_go(&f);
    let (old, driver) = f.retained_intent(
        Some(intent::EndReason::RuntimeLost {
            error: "fixture_network_lost".into(),
            retryable: true,
        }),
        false,
        false,
    );
    wait_phase(&old, intent::Phase::Backoff).await;
    assert!(go_claim(&f, 1).is_err());
    old.request_stop();
    wait_released(&old.claim).await;
    assert_eq!(
        driver.calls.load(Ordering::SeqCst),
        1,
        "stop must not start a replacement attempt"
    );
    let go = go_claim(&f, 1).unwrap();
    assert!(!f.service.has_intent());
    go.release_confirmed().unwrap();
    assert_eq!(f.service.start(request()).await.unwrap(), 2);
    f.entered(1).await;
    let current = f.service.record().unwrap().unwrap();
    old.finish_before(tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    old.request_stop();
    assert!(current.claim.is_current());
    assert!(!*current.stop.borrow());
    current.request_stop();
    f.resume();
    wait_released(&current.claim).await;
    f.close().await;
}

#[tokio::test]
async fn rejected_spawn_releases_undispatched_claim_and_next_start_uses_a_fresh_sequence() {
    let f = Fixture::new(false);
    {
        let mut identity = f.identity.0.lock().unwrap();
        identity.iccid.clear();
        identity.imsi.clear();
    }
    assert_eq!(
        f.service.start(request()).await.unwrap_err(),
        "native_vowifi_intent_identity_invalid"
    );
    assert!(!f.service.has_intent());
    assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 0);
    {
        let mut identity = f.identity.0.lock().unwrap();
        identity.iccid = "894433000000000001".into();
        identity.imsi = "234330000000001".into();
    }
    assert_eq!(f.service.start(request()).await.unwrap(), 2);
    let record = f.service.record().unwrap().unwrap();
    wait_released(&record.claim).await;
    assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn maintenance_publishes_before_stopping_and_retains_barrier_through_actual_pre_worker_exit()
{
    let f = Fixture::new(true);
    configure_go(&f);
    f.service.start(request()).await.unwrap();
    f.entered(1).await;
    let record = f.service.record().unwrap().unwrap();
    let gate = &f.service.0.resources.ordinary_admission;
    assert!(
        gate.begin_reader_maintenance(
            &record.expected.reader,
            4,
            crate::ordinary_vowifi_admission::MaintenancePolicy::Reject
        )
        .is_err()
    );
    let maintenance = gate
        .begin_reader_maintenance(
            &record.expected.reader,
            4,
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain,
        )
        .unwrap();
    assert!(
        !*record.stop.borrow(),
        "publishing fence does not fake stop acknowledgement"
    );
    assert!(
        f.service
            .stop_for_maintenance(
                &maintenance,
                tokio::time::Instant::now() + Duration::from_millis(10)
            )
            .await
            .is_err()
    );
    assert!(*record.stop.borrow());
    assert!(record.claim.is_current());
    assert!(maintenance.is_current());
    assert!(maintenance.release_confirmed().is_err());
    assert!(go_claim(&f, 1).is_err());
    assert!(f.service.start(request()).await.is_err());
    f.resume();
    f.service
        .stop_for_maintenance(
            &maintenance,
            tokio::time::Instant::now() + Duration::from_secs(2),
        )
        .await
        .unwrap();
    f.assert_idle();
    assert!(maintenance.ordinary_drained());
    assert!(
        go_claim(&f, 1).is_err(),
        "whole maintenance remains fenced after ordinary release"
    );
    maintenance.release_confirmed().unwrap();
    let go = go_claim(&f, 1).unwrap();
    go.release_confirmed().unwrap();
    f.close().await;
}

#[tokio::test]
async fn backoff_is_rejected_and_maintenance_stops_only_its_exact_captured_record() {
    let f = Fixture::new(false);
    let (record, _) = f.retained_intent(Some(intent::EndReason::RegistrationRenewal), false, false);
    wait_phase(&record, intent::Phase::Backoff).await;
    let gate = &f.service.0.resources.ordinary_admission;
    assert!(
        gate.begin_reader_maintenance(
            &record.expected.reader,
            4,
            crate::ordinary_vowifi_admission::MaintenancePolicy::Reject
        )
        .is_err()
    );
    assert!(
        gate.begin_reader_maintenance(
            &record.expected.reader,
            5,
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain
        )
        .is_err()
    );
    assert!(!*record.stop.borrow());
    let foreign_gate = crate::ordinary_vowifi_admission::Gate::default();
    let foreign_owner = foreign_gate
        .acquire_native_reader("foreign", 1, &record.expected.reader, 4)
        .unwrap();
    let foreign = foreign_gate
        .begin_reader_maintenance(
            &record.expected.reader,
            4,
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain,
        )
        .unwrap();
    assert!(
        f.service
            .stop_for_maintenance(
                &foreign,
                tokio::time::Instant::now() + Duration::from_secs(1)
            )
            .await
            .is_err()
    );
    assert!(
        !*record.stop.borrow(),
        "foreign maintenance cannot stop the matching-looking reader"
    );
    foreign.abort_undispatched().unwrap();
    foreign_owner.release_confirmed().unwrap();
    let own = gate
        .begin_reader_maintenance(
            &record.expected.reader,
            4,
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain,
        )
        .unwrap();
    f.service
        .stop_for_maintenance(&own, tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(record.progress.borrow().phase, intent::Phase::Stopped);
    own.release_confirmed().unwrap();
    f.close().await;
}

#[tokio::test]
async fn ordinary_known_rejection_does_not_wait_for_the_maintenance_pre_that_is_waiting_for_it() {
    let f = Fixture::new(false);
    let (record, _) = f.retained_intent(None, false, false);
    wait_phase(&record, intent::Phase::Registered).await;
    let maintenance = f
        .service
        .0
        .resources
        .ordinary_admission
        .begin_reader_maintenance(
            &record.expected.reader,
            4,
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain,
        )
        .unwrap();
    let pre = f
        .service
        .0
        .resources
        .gate
        .begin_native_pre_admission(
            &record.expected.reader,
            reader_reservation::NativePinPolicy::Mutation,
        )
        .unwrap();
    assert!(f.service.0.resources.gate.native_reader_session_active());
    let driver = Arc::new(LiveDriver {
        ordinary_claim: Some(record.claim.clone()),
        resources: f.service.0.resources.clone(),
        current: Mutex::new(AttemptState::default()),
    });
    let attempt = f.attempt();
    let (_stop, stop) = watch::channel(false);
    let exit = run(driver.clone(), attempt.clone(), stop).await;
    assert!(
        exit.cleanup_confirmed,
        "the unrelated maintenance pre is not this failed attempt's resource"
    );
    assert!(
        driver
            .current
            .lock()
            .unwrap()
            .acquisition_cleanup
            .as_ref()
            .unwrap()
            .confirmed()
    );
    driver.await_cleanup(attempt).await.unwrap();
    assert!(
        f.service
            .0
            .resources
            .gate
            .native_pre_admission_current(&pre)
    );
    f.service
        .stop_for_maintenance(
            &maintenance,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap();
    f.service
        .0
        .resources
        .gate
        .finish_native_pre_admission(&pre)
        .unwrap();
    maintenance.release_confirmed().unwrap();
    assert_eq!(f.supplier.pre_calls.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn mutation_of_starting_native_session_stops_held_identity_pre_before_taking_its_own_pre() {
    let f = Fixture::new(true);
    f.enable_maintenance_supplier();
    f.service.start(request()).await.unwrap();
    f.entered(1).await;
    let old = f.service.record().unwrap().unwrap();
    assert!(f.service.0.resources.gate.native_pre_admission_active());
    let reader = old.expected.reader.clone();
    let gate = f.service.0.resources.gate.clone();
    let owner = f.service.0.resources.pcsc.clone();
    let mutation = tokio::spawn(async move {
        NativeReaderSessionLease::acquire_for_esim_mutation(
            Path::new("no-go.sock"),
            None,
            &reader,
            gate,
            owner,
        )
        .await
    });
    let maintenance = f.maintenance().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while !*old.stop.borrow() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !mutation.is_finished(),
        "Starting's occupied pre is not an immediate mutation rejection"
    );
    assert_eq!(
        f.supplier.pre_calls.load(Ordering::SeqCst),
        1,
        "mutation has not stolen the old physical pre"
    );
    assert!(maintenance.is_current());
    mutation.abort();
    assert!(
        mutation
            .await
            .err()
            .expect("cancelled mutation waiter")
            .is_cancelled()
    );
    assert!(maintenance.is_current());
    assert!(old.claim.is_current());
    assert!(f.service.start(request()).await.is_err());
    f.resume();
    wait_released(&old.claim).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while maintenance.is_current() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    f.assert_idle();
    assert_eq!(f.apdu.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn cancelled_card_worker_and_lease_timeout_keep_maintenance_until_exact_physical_completion()
{
    let f = Fixture::new(false);
    f.enable_maintenance_supplier();
    let lease = f.lease().await;
    let maintenance = f.maintenance().await;
    let context = lease
        .pcsc_context(f.service.0.resources.pcsc.clone())
        .unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (complete, done) = std::sync::mpsc::channel();
    let captured = lease.clone();
    let worker = tokio::spawn(async move {
        captured
            .run_card_worker(&crate::blocking_pool::BlockingPool::new(1), move || {
                context.with_transaction(|_| {
                    entered.send(()).unwrap();
                    done.recv_timeout(Duration::from_secs(5)).unwrap();
                })
            })
            .await
    });
    ready.await.unwrap();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    lease.set_release_timeout(Duration::from_millis(10));
    assert!(lease.release_with_confirmation().await.is_err());
    assert!(maintenance.is_current());
    assert!(f.service.start(request()).await.is_err());
    assert!(f.service.0.resources.gate.native_reader_session_active());
    complete.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !lease.release_confirmed().await.unwrap() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    assert!(!maintenance.is_current());
    f.assert_idle();
    f.service.start(request()).await.unwrap();
    let record = f.service.record().unwrap().unwrap();
    wait_released(&record.claim).await;
    f.close().await;
}

#[tokio::test]
async fn unknown_go_claim_global_stop_is_fail_closed_until_exact_owner_exit() {
    for source in ["reader", "module"] {
        let f = Fixture::new(false);
        configure_go(&f);
        let claim = f
            .service
            .0
            .resources
            .ordinary_admission
            .acquire_go("go-service-fixture", source, "frozen-stop", 1)
            .unwrap();
        for _ in 0..2 {
            assert_eq!(
                f.service
                    .stop_global(tokio::time::Instant::now() + Duration::from_secs(1))
                    .await,
                Err("legacy_stop_unconfirmed")
            );
            assert!(claim.is_current());
            assert!(f.service.start(request()).await.is_err());
        }
        claim.release_confirmed().unwrap();
        f.service
            .stop_global(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        f.close().await;
    }
}
#[tokio::test]
async fn native_only_global_stop_joins_intent_without_compatibility_rpc() {
    let f = Fixture::new(false);
    let (record, _) = f.retained_intent(None, false, false);
    wait_phase(&record, intent::Phase::Registered).await;
    f.service
        .stop_global(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    assert!(!record.claim.is_current());
    f.close().await;
}
#[tokio::test]
async fn released_native_diagnostics_do_not_mask_subsequent_reader_or_module_go_owner() {
    let f = Fixture::new(false);
    configure_go(&f);
    let (record, _) = f.retained_intent(None, false, false);
    wait_phase(&record, intent::Phase::Registered).await;
    assert!(f.service.reader_observation().unwrap().session_active);
    record.request_stop();
    wait_released(&record.claim).await;
    let terminal = f.service.reader_observation().unwrap();
    assert!(!terminal.session_active && !terminal.sms_ready);
    for (sequence, source) in [(1, "reader"), (2, "module")] {
        let go = f
            .service
            .0
            .resources
            .ordinary_admission
            .acquire_go("go-service-fixture", source, "replacement", sequence)
            .unwrap();
        assert!(f.service.reader_observation().is_none());
        assert!(!f.service.retains_route_owner());
        assert!(f.service.retained_status().is_none());
        go.release_confirmed().unwrap();
    }
    assert!(f.service.reader_observation().is_some());
    f.close().await;
}
#[tokio::test]
async fn stale_identity_and_stop_keep_native_authority_but_never_publish_sms_readiness() {
    let f = Fixture::new(false);
    let (record, _) = f.retained_intent(None, false, false);
    wait_phase(&record, intent::Phase::Registered).await;
    f.identity.0.lock().unwrap().iccid = "replacement".into();
    let observation = f.service.reader_observation().unwrap();
    assert!(!observation.identity_valid);
    assert!(!observation.sms_ready);
    assert!(observation.subscription_id.is_none());
    assert_eq!(
        observation.reader_generation,
        record.expected.reader_generation
    );
    record.request_stop();
    wait_released(&record.claim).await;
    f.close().await;
}
#[test]
fn failed_service_initialization_is_an_unavailable_native_observation() {
    let lifecycle = NativeLifecycle::default();
    assert!(lifecycle.reader_observation().is_none());
    assert!(
        lifecycle
            .ordinary_service
            .set(Err("fixture init failure".into()))
            .is_ok()
    );
    let observation = lifecycle.reader_observation().unwrap();
    assert_eq!(observation.status["available"], false);
    assert!(!observation.identity_valid && !observation.session_active && !observation.sms_ready);
    assert!(lifecycle.handles_public_request(&hyper::Method::GET, "/api/reader/vowifi"));
}
struct RebuildStartupDriver {
    calls: AtomicUsize,
    cleanup_ready: AtomicBool,
    changed: Notify,
    rebuild_retry: ordinary::status::RetryClass,
}
impl intent::Driver for RebuildStartupDriver {
    fn check_identity(&self, _: &intent::ExpectedIdentity) -> Result<(), String> {
        Ok(())
    }
    fn run(
        self: Arc<Self>,
        attempt: intent::Attempt,
        mut stop: watch::Receiver<bool>,
        progress: watch::Sender<intent::AttemptProgress>,
    ) -> intent::DriverFuture<intent::AttemptExit> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if attempt.number >= 2 {
                progress.send_replace(intent::AttemptProgress::Registered(
                    SessionGeneration::from_value(attempt.number as u64 + 1).unwrap(),
                ));
                while !*stop.borrow() {
                    if stop.changed().await.is_err() {
                        break;
                    }
                }
                return intent::AttemptExit {
                    reason: intent::EndReason::Stopped,
                    cleanup_confirmed: true,
                };
            }
            intent::AttemptExit {
                reason: if attempt.number == 0 {
                    intent_end_reason(
                        ordinary::status::EndReason::NetworkFailure,
                        ordinary::status::RetryClass::RetryAfterRelease,
                        Some("runtime_connection_lost"),
                    )
                } else {
                    intent_end_reason(
                        ordinary::status::EndReason::StartupFailure,
                        self.rebuild_retry,
                        Some("rebuild_startup_failed"),
                    )
                },
                cleanup_confirmed: attempt.number == 0,
            }
        })
    }
    fn await_cleanup(
        self: Arc<Self>,
        _: intent::Attempt,
    ) -> intent::DriverFuture<Result<(), String>> {
        Box::pin(async move {
            while !self.cleanup_ready.load(Ordering::SeqCst) {
                self.changed.notified().await;
            }
            Ok(())
        })
    }
}
#[tokio::test]
async fn typed_recovery_start_retries_only_transient_and_only_after_exact_cleanup() {
    for transient in [false, true] {
        let f = Fixture::new(false);
        let expected = f.identity.capture().unwrap();
        let driver = Arc::new(RebuildStartupDriver {
            calls: AtomicUsize::new(0),
            cleanup_ready: AtomicBool::new(false),
            changed: Notify::new(),
            rebuild_retry: if transient {
                ordinary::status::RetryClass::RetryAfterRelease
            } else {
                ordinary::status::RetryClass::Permanent
            },
        });
        let claim = f
            .service
            .0
            .resources
            .ordinary_admission
            .acquire_native_reader(
                &f.service.0.sms_epoch,
                1,
                &expected.reader,
                expected.reader_generation,
            )
            .unwrap();
        let task = intent::spawn(
            driver.clone(),
            1,
            expected.clone(),
            request(),
            intent::Policy {
                initial_delay: Duration::from_millis(5),
                maximum_delay: Duration::from_millis(5),
                identity_poll: Duration::from_millis(5),
                cleanup_retry: Duration::from_millis(5),
                jitter: Duration::ZERO,
            },
        )
        .unwrap();
        let record = Arc::new(Record {
            id: 1,
            expected,
            driver: f.driver(),
            stop: task.stop_sender(),
            progress: task.subscribe(),
            task: tokio::sync::Mutex::new(task),
            claim,
            observer: Mutex::new(None),
            operation: Mutex::new(super::super::operation::Clock::start(
                tokio::time::Instant::now(),
                Duration::from_secs(90),
            )),
        });
        f.service.0.state.lock().unwrap().record = Some(record.clone());
        record.observe_terminal();
        wait_phase(&record, intent::Phase::CleanupUnconfirmed).await;
        assert!(record.claim.is_current());
        assert_eq!(driver.calls.load(Ordering::SeqCst), 2);
        driver.cleanup_ready.store(true, Ordering::SeqCst);
        driver.changed.notify_one();
        if transient {
            wait_phase(&record, intent::Phase::Registered).await;
            assert_eq!(driver.calls.load(Ordering::SeqCst), 3);
            assert!(
                record.claim.is_current(),
                "the retained intent still owns its claim"
            );
            assert!(record.progress.borrow().last_error.is_none());
            record.request_stop();
            wait_phase(&record, intent::Phase::Stopped).await;
        } else {
            wait_phase(&record, intent::Phase::Failed).await;
            assert_eq!(driver.calls.load(Ordering::SeqCst), 2);
            assert_eq!(
                record.progress.borrow().last_error.as_deref(),
                Some("rebuild_startup_failed")
            );
        }
        wait_released(&record.claim).await;
        f.close().await;
    }
}

#[tokio::test]
async fn public_disable_never_releases_unknown_compatibility_reader_or_module_claim() {
    for source in ["reader", "module"] {
        let f = Fixture::new(false);
        configure_go(&f);
        let claim = f
            .service
            .0
            .resources
            .ordinary_admission
            .acquire_go("go-service-fixture", source, "frozen-global-stop", 9)
            .unwrap();
        let response = f
            .service
            .handle_public(
                Request::builder()
                    .method(hyper::Method::POST)
                    .uri("/api/reader/vowifi")
                    .body(Full::new(Bytes::from_static(
                        br#"{"enable":false,"proxy":"invalid","epdg_port":999999,"observe_operation":true,"client_request_id":"compatibility-stop-fixture"}"#,
                    )))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(claim.is_current());
        claim.release_confirmed().unwrap();
        f.service
            .stop_global(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        f.close().await;
    }
}

#[tokio::test]
async fn public_status_and_sessions_keep_starting_and_stopping_until_actual_worker_exit() {
    use http_body_util::BodyExt;
    async fn get(service: &Service, path: &str) -> serde_json::Value {
        let response = service
            .handle_public(
                Request::builder()
                    .uri(path)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }
    let f = Fixture::new(true);
    f.service.start(request()).await.unwrap();
    f.entered(1).await;
    let status = get(&f.service, "/api/reader/vowifi").await;
    assert_eq!(status["tunnel_state"], "connecting");
    assert_eq!(status["cleanup_confirmed"], false);
    assert_eq!(status["stopping"], false);
    let sessions = get(&f.service, "/api/reader/vowifi/sessions").await;
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
    assert!(sessions.get("legacy_sessions_unavailable").is_none());
    assert_eq!(sessions["sessions"][0]["reader_generation"], 4);
    f.service.begin_shutdown();
    let status = get(&f.service, "/api/reader/vowifi").await;
    assert_eq!(status["stopping"], true);
    assert_eq!(status["cleanup_confirmed"], false);
    assert_eq!(status["sms_ready"], false);
    assert_eq!(status["incoming_listener_started"], false);
    assert_eq!(
        status["progress_desc"],
        "正在释放 VoWiFi 资源，尚未确认关闭"
    );
    assert_eq!(
        get(&f.service, "/api/reader/vowifi/sessions").await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        f.service.has_intent(),
        "a blocked worker must retain its actual claim"
    );
    f.supplier.held.store(false, Ordering::SeqCst);
    f.supplier.resume.notify_waiters();
    f.service
        .finish_before(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    let status = get(&f.service, "/api/reader/vowifi").await;
    assert_eq!(status["cleanup_confirmed"], true);
    assert_eq!(status["stopping"], false);
    assert!(!f.service.has_intent());
    f.close().await;
}

#[test]
fn intent_startup_mapping_preserves_typed_retry_and_never_retries_setup_or_stop() {
    use ordinary::status::{EndReason as End, RetryClass as Retry};
    for retry in [Retry::Stop, Retry::Permanent, Retry::RetryAfterRelease] {
        let reason = intent_end_reason(End::StartupFailure, retry, Some("typed_startup"));
        assert_eq!(
            matches!(reason, intent::EndReason::StartupTransient { .. }),
            retry == Retry::RetryAfterRelease
        );
        assert!(matches!(
            intent_end_reason(End::SetupFailure, retry, None),
            intent::EndReason::StartupFailed { .. }
        ));
        assert_eq!(
            intent_end_reason(End::UserStop, retry, None),
            intent::EndReason::Stopped
        );
        assert_eq!(
            intent_end_reason(End::ReaderChanged, retry, None),
            intent::EndReason::ReaderChanged
        );
    }
}

#[tokio::test]
async fn reader_public_ingress_pending_and_ready_empty_keep_exact_old_errors_without_dispatch() {
    struct Incomplete {
        first: Mutex<Option<Bytes>>,
        ready_empty: bool,
        _pinned: std::marker::PhantomPinned,
    }
    impl hyper::body::Body for Incomplete {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            let this = self.as_ref().get_ref();
            if let Some(bytes) = this.first.lock().unwrap().take() {
                std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes))))
            } else if this.ready_empty {
                std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::new()))))
            } else {
                std::task::Poll::Pending
            }
        }
    }
    let f = Fixture::new(false);
    let request = |path, ready_empty| {
        Request::builder().method("POST").uri(path)
        .body(Incomplete {
            first: Mutex::new(Some(Bytes::from_static(br#"{"enable":true,"observe_operation":true,"client_request_id":"reader-incomplete-body"}"#))),
            ready_empty, _pinned: std::marker::PhantomPinned,
        }).unwrap()
    };
    let (pending, empty) = tokio::time::timeout(Duration::from_secs(4), async {
        tokio::join!(
            f.service
                .handle_public(request("/api/reader/vowifi", false)),
            f.service
                .handle_public(request("/api/reader/vowifi/reconcile", true)),
        )
    })
    .await
    .expect("both original two-second ingress deadlines finish");
    for response in [pending, empty] {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value, json!({"error":"请求格式错误"}));
    }
    let state = f.service.0.state.lock().unwrap();
    assert_eq!(state.next, 0);
    assert!(state.record.is_none());
    assert!(
        state
            .requests
            .lookup("reader-incomplete-body", None)
            .unwrap()
            .is_none()
    );
    drop(state);
    assert!(
        !f.service
            .0
            .resources
            .ordinary_admission
            .has_ordinary_claim()
    );
    f.assert_no_dispatch();
    f.close().await;
}
