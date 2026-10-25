use super::*;
use crate::ordinary_vowifi_admission::{Claim, MaintenancePolicy};
use crate::reader_session_lease::{AcquisitionCleanup, NativeReaderSessionLease};

const ICCID: &str = "8901234567890123456";
const IMSI: &str = "234100896392986";

#[derive(Default)]
struct NativeTrace {
    responses: Mutex<VecDeque<Vec<u8>>>,
    commands: Mutex<Vec<Vec<u8>>>,
    transactions: AtomicUsize,
}
struct NativePhysical(Arc<NativeTrace>);
impl Physical for NativePhysical {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn ApduTransport<Error = PcscFailure>),
    ) -> Result<(), PcscFailure> {
        self.0.transactions.fetch_add(1, Ordering::SeqCst);
        body(self);
        Ok(())
    }
    fn close(&mut self) -> Result<(), PcscFailure> {
        Ok(())
    }
}
impl ApduTransport for NativePhysical {
    type Error = PcscFailure;
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, PcscFailure> {
        self.0.commands.lock().unwrap().push(command.to_vec());
        Ok(self
            .0
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted native APDU"))
    }
}
struct NativeFixture {
    service: Arc<Service>,
    ordinary: Arc<crate::native_vowifi_session::NativeLifecycle>,
    trace: Arc<NativeTrace>,
    selected: SelectionSnapshot,
    session: LifecycleBinding,
}
impl NativeFixture {
    fn new() -> Self {
        let trace = Arc::new(NativeTrace::default());
        let physical = trace.clone();
        let gate = Arc::new(Gate::default());
        let owner = Owner::with_factory(
            "native-only".into(),
            Arc::new(move |_| Ok(Box::new(NativePhysical(physical.clone())))),
            Arc::new(|| Ok(vec!["Reader".into()])),
            Arc::new(|_, _| panic!("ordinary authentication must never reset/reopen")),
        );
        let service = Arc::new(
            Service::new(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../../build/runtime/native-only-488"),
                gate.clone(),
                owner,
                None,
                None,
                "native-only".into(),
            )
            .unwrap(),
        );
        let ordinary = Arc::new(crate::native_vowifi_session::NativeLifecycle::default());
        gate.set_ordinary_admission(ordinary.ordinary_admission())
            .unwrap();
        service.set_ordinary_lifecycle(Arc::downgrade(&ordinary));
        // This represents an already-confirmed selection. No Go producer,
        // business Bridge, Unix server, compatibility token or card read exists.
        let session = service.0.owner.native_open("Reader").unwrap();
        let transition = gate
            .prepare_native_transition(
                "native-only",
                &SelectionSnapshot::default(),
                Kind::Connect,
                "Reader",
                NativeAuthority::Idle,
            )
            .unwrap();
        let selected = gate
            .commit_native_transition(
                &transition,
                Some(&session),
                NativeReaderFacts {
                    metadata: Some(json!({"iccid":ICCID,"imsi":IMSI,"mcc":"234","mnc":"10"})),
                    ..Default::default()
                },
            )
            .unwrap();
        let supplier: Arc<dyn NativeSessionSupplier> = service.clone();
        gate.set_native_session_supplier(Arc::downgrade(&supplier));
        Self {
            service,
            ordinary,
            trace,
            selected,
            session,
        }
    }
    fn claim(&self, id: u64) -> Arc<Claim> {
        self.ordinary
            .ordinary_admission()
            .acquire_native_reader("native-only-epoch", id, "Reader", self.selected.generation)
            .unwrap()
    }
    async fn acquire(
        &self,
        claim: Arc<Claim>,
        cleanup: AcquisitionCleanup,
    ) -> Result<NativeReaderSessionLease, proletarius_domain::BackendFailure> {
        NativeReaderSessionLease::acquire_for_ordinary(
            &PathBuf::from(
                "/Users/proletarius/Desktop/Switch_MAC/build/runtime/never-created-488-go.sock",
            ),
            None,
            "Reader",
            self.service.0.gate.clone(),
            self.service.0.owner.clone(),
            claim,
            cleanup,
        )
        .await
    }
    fn cleanup(&self) -> AcquisitionCleanup {
        AcquisitionCleanup::new(self.service.0.gate.clone())
    }
    fn identity_responses() -> VecDeque<Vec<u8>> {
        let aid = [0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff, 1];
        let mut dir = vec![0x61, 11, 0x4f, 9];
        dir.extend(aid);
        dir.extend([0x90, 0]);
        VecDeque::from([
            vec![0x90, 0],
            vec![0x90, 0],
            dir,
            vec![0x90, 0],
            vec![0x90, 0],
            vec![8, 0x90, 0],
            vec![
                0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68, 0x90, 0,
            ],
            vec![0x6a, 0x82],
            vec![0x90, 0],
            vec![0x90, 0],
            vec![0x90, 0],
            vec![
                0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6, 0x90, 0,
            ],
        ])
    }
    fn aka_responses(isim: bool) -> VecDeque<Vec<u8>> {
        let mut responses = Self::identity_responses();
        let aid = [0xa0, 0, 0, 0, 0x87, 0x10, if isim { 4 } else { 2 }, 0xff, 1];
        let mut dir = vec![0x61, 11, 0x4f, 9];
        dir.extend(aid);
        dir.extend([0x90, 0]);
        let mut success = vec![0xdb, 8];
        success.extend([1; 8]);
        success.push(16);
        success.extend([2; 16]);
        success.push(16);
        success.extend([3; 16]);
        success.extend([0x90, 0]);
        responses.extend([vec![0x90, 0], vec![0x90, 0], dir, vec![0x90, 0], success]);
        responses
    }
}

#[tokio::test]
async fn native_ordinary_without_go_producer_or_bridge_acquires_and_authenticates_eap_and_isim() {
    let fixture = NativeFixture::new();
    assert!(fixture.service.0.bridge.lock().unwrap().is_none());
    let claim = fixture.claim(1);
    let cleanup = fixture.cleanup();
    let lease = fixture
        .acquire(claim.clone(), cleanup.clone())
        .await
        .unwrap();
    assert!(!cleanup.confirmed());
    assert_eq!(
        lease
            .pcsc_context(fixture.service.0.owner.clone())
            .unwrap()
            .binding()
            .session_id,
        fixture.session.session_id
    );
    assert_eq!(lease.identity.iccid.as_str(), ICCID);
    assert_eq!(lease.identity.imsi.as_str(), IMSI);
    for isim in [false, true] {
        *fixture.trace.responses.lock().unwrap() = NativeFixture::aka_responses(isim);
        let result = if isim {
            lease.authenticate_ims(&[0x11; 16], &[0x22; 16]).await
        } else {
            lease.authenticate_ike(&[0x11; 16], &[0x22; 16]).await
        }
        .unwrap();
        assert!(
            matches!(&result, proletarius_vowifi::SimAkaResult::Success {res,ck,ik}
            if res==&vec![1;8] && ck==&[2;16] && ik==&[3;16])
        );
        assert!(fixture.trace.responses.lock().unwrap().is_empty());
    }
    assert_eq!(
        fixture
            .trace
            .commands
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c[1] == 0x88)
            .count(),
        2
    );
    lease.release_with_confirmation().await.unwrap();
    assert!(cleanup.confirmed());
    assert!(claim.is_current()); // lease release is not an intent's actual join.
    claim.release_confirmed().unwrap();
    fixture
        .service
        .0
        .owner
        .native_close(&fixture.session)
        .unwrap();
}

#[tokio::test]
async fn native_ordinary_without_go_still_checks_real_card_identity_before_authenticate() {
    let fixture = NativeFixture::new();
    let claim = fixture.claim(1);
    let cleanup = fixture.cleanup();
    let lease = fixture
        .acquire(claim.clone(), cleanup.clone())
        .await
        .unwrap();
    let mut responses = NativeFixture::identity_responses();
    responses.back_mut().unwrap()[9] = 0xf7;
    *fixture.trace.responses.lock().unwrap() = responses;
    assert!(
        lease
            .authenticate_ike(&[0x11; 16], &[0x22; 16])
            .await
            .is_err()
    );
    assert!(
        !fixture
            .trace
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c[1] == 0x88)
    );
    lease.release_with_confirmation().await.unwrap();
    assert!(cleanup.confirmed());
    claim.release_confirmed().unwrap();
    fixture
        .service
        .0
        .owner
        .native_close(&fixture.session)
        .unwrap();
}

#[tokio::test]
async fn native_ordinary_rejects_foreign_stale_wrong_reader_and_generation_before_card_io() {
    for kind in 0..4 {
        let fixture = NativeFixture::new();
        let foreign = crate::ordinary_vowifi_admission::Gate::default();
        let own_gate = fixture.ordinary.ordinary_admission();
        let gate = if kind == 0 {
            &foreign
        } else {
            own_gate.as_ref()
        };
        let claim = gate
            .acquire_native_reader(
                "invalid-epoch",
                1,
                if kind == 2 { "Other" } else { "Reader" },
                fixture.selected.generation + u64::from(kind == 3),
            )
            .unwrap();
        if kind == 1 {
            claim.release_confirmed().unwrap();
        }
        let cleanup = fixture.cleanup();
        assert!(matches!(
            fixture.acquire(claim.clone(), cleanup.clone()).await,
            Err(proletarius_domain::BackendFailure::ReaderAdmissionRejected { .. })
        ));
        assert!(cleanup.confirmed());
        assert!(!fixture.service.0.gate.native_pre_admission_active());
        assert_eq!(fixture.trace.transactions.load(Ordering::SeqCst), 0);
        assert!(fixture.trace.commands.lock().unwrap().is_empty());
        claim.release_confirmed().unwrap();
        fixture
            .service
            .0
            .owner
            .native_close(&fixture.session)
            .unwrap();
    }
}

#[tokio::test]
async fn native_ordinary_no_go_path_cannot_bypass_maintenance_legacy_or_nwait() {
    let fixture = NativeFixture::new();
    let admission = fixture.ordinary.ordinary_admission();
    let legacy = admission.begin_legacy_reader_business(false).unwrap();
    assert!(
        admission
            .acquire_native_reader(
                "native-only-epoch",
                1,
                "Reader",
                fixture.selected.generation
            )
            .is_err()
    );
    legacy.release_confirmed().unwrap();
    let claim = fixture.claim(2);
    let maintenance = admission
        .begin_reader_maintenance(
            "Reader",
            fixture.selected.generation,
            MaintenancePolicy::StopAndDrain,
        )
        .unwrap();
    let cleanup = fixture.cleanup();
    assert!(
        fixture
            .acquire(claim.clone(), cleanup.clone())
            .await
            .is_err()
    );
    assert!(cleanup.confirmed());
    claim.release_confirmed().unwrap();
    maintenance.release_confirmed().unwrap();
    let claim = fixture.claim(3);
    // The physical-only test reservation represents an already-captured
    // N-wait capability; production business also owns a shared maintenance slot.
    let nwait = fixture
        .service
        .0
        .gate
        .acquire(None, Some("Reader"), Some(fixture.selected.generation))
        .unwrap();
    let cleanup = fixture.cleanup();
    assert!(matches!(
        fixture.acquire(claim.clone(), cleanup.clone()).await,
        Err(proletarius_domain::BackendFailure::ReaderAdmissionRejected { code: "card_busy" })
    ));
    assert!(cleanup.confirmed());
    fixture.service.0.gate.release(&nwait).unwrap();
    claim.release_confirmed().unwrap();
    assert_eq!(fixture.trace.transactions.load(Ordering::SeqCst), 0);
    fixture
        .service
        .0
        .owner
        .native_close(&fixture.session)
        .unwrap();
}

#[tokio::test]
async fn external_identity_without_go_holds_all_ordinary_fence_through_real_eap_and_release() {
    let fixture = NativeFixture::new();
    let lease = NativeReaderSessionLease::acquire(
        &PathBuf::from(
            "/Users/proletarius/Desktop/Switch_MAC/build/runtime/never-created-489-go.sock",
        ),
        None,
        "Reader",
        fixture.service.0.gate.clone(),
        fixture.service.0.owner.clone(),
    )
    .await
    .unwrap();
    let admission = fixture.ordinary.ordinary_admission();
    admission.set_go_producer("late-go").unwrap();
    assert!(
        admission
            .acquire_go("late-go", "module", "late-module", 1)
            .is_err()
    );
    assert!(
        admission
            .acquire_native_reader("native", 1, "Reader", fixture.selected.generation)
            .is_err()
    );
    *fixture.trace.responses.lock().unwrap() = NativeFixture::aka_responses(false);
    assert!(matches!(
        lease
            .authenticate_ike(&[0x11; 16], &[0x22; 16])
            .await
            .unwrap(),
        proletarius_vowifi::SimAkaResult::Success { .. }
    ));
    assert!(fixture.trace.responses.lock().unwrap().is_empty());
    lease.release_with_confirmation().await.unwrap();
    assert!(!fixture.service.0.gate.native_pre_admission_active());
    let module = admission
        .acquire_go("late-go", "module", "late-module", 1)
        .unwrap();
    module.release_confirmed().unwrap();
    fixture
        .service
        .0
        .owner
        .native_close(&fixture.session)
        .unwrap();
}

#[tokio::test]
async fn external_identity_rejects_missing_or_foreign_fence_without_go_or_card_io() {
    let fixture = NativeFixture::new();
    assert!(
        fixture
            .service
            .clone()
            .pre_admit("Reader".into(), NativePinPolicy::Identity, None, None)
            .await
            .is_err()
    );
    let foreign = crate::ordinary_vowifi_admission::Gate::default();
    let guard = foreign
        .begin_reader_maintenance(
            "Reader",
            fixture.selected.generation,
            MaintenancePolicy::RejectAllOrdinary,
        )
        .unwrap();
    assert!(
        fixture
            .service
            .clone()
            .pre_admit(
                "Reader".into(),
                NativePinPolicy::Identity,
                None,
                Some(guard.clone())
            )
            .await
            .is_err()
    );
    assert_eq!(fixture.trace.transactions.load(Ordering::SeqCst), 0);
    assert!(fixture.trace.commands.lock().unwrap().is_empty());
    guard.release_confirmed().unwrap();
    fixture
        .service
        .0
        .owner
        .native_close(&fixture.session)
        .unwrap();
}

struct HeldNativePre {
    service: Arc<Service>,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
impl NativeSessionSupplier for HeldNativePre {
    fn capture_reader_maintenance(
        &self,
        reader: &str,
        generation: u64,
        policy: NativePinPolicy,
        ordinary: Option<&Arc<Claim>>,
    ) -> Result<
        Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
        proletarius_domain::BackendFailure,
    > {
        self.service
            .capture_reader_maintenance(reader, generation, policy, ordinary)
    }
    fn pre_admit(
        self: Arc<Self>,
        reader: String,
        policy: NativePinPolicy,
        ordinary: Option<Arc<Claim>>,
        maintenance: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
    ) -> PreAdmissionFuture {
        Box::pin(async move {
            assert!(ordinary.is_some());
            self.entered.notify_one();
            self.resume.notified().await;
            self.service
                .clone()
                .pre_admit(reader, policy, ordinary, maintenance)
                .await
        })
    }
    fn admit(self: Arc<Self>, binding: NativePinBinding) -> AdmissionFuture {
        self.service.clone().admit(binding)
    }
    fn snapshot(&self, binding: &NativePinBinding) -> Result<Value, String> {
        NativeSessionSupplier::snapshot(self.service.as_ref(), binding)
    }
    fn finalize(
        self: Arc<Self>,
        binding: NativePinBinding,
        context: Option<NativeCardContext>,
        target: String,
        aid: String,
    ) -> SupplierFuture {
        self.service.clone().finalize(binding, context, target, aid)
    }
}

#[tokio::test]
async fn cancelled_native_pre_without_go_remains_owned_until_maintenance_rejection_actually_returns()
 {
    let fixture = NativeFixture::new();
    let held = Arc::new(HeldNativePre {
        service: fixture.service.clone(),
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let supplier: Arc<dyn NativeSessionSupplier> = held.clone();
    fixture
        .service
        .0
        .gate
        .set_native_session_supplier(Arc::downgrade(&supplier));
    let claim = fixture.claim(1);
    let cleanup = fixture.cleanup();
    let gate = fixture.service.0.gate.clone();
    let owner = fixture.service.0.owner.clone();
    let captured = claim.clone();
    let proof = cleanup.clone();
    let waiter = tokio::spawn(async move {
        NativeReaderSessionLease::acquire_for_ordinary(
            &PathBuf::from(
                "/Users/proletarius/Desktop/Switch_MAC/build/runtime/never-created-488-go.sock",
            ),
            None,
            "Reader",
            gate,
            owner,
            captured,
            proof,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), held.entered.notified())
        .await
        .unwrap();
    let maintenance = fixture
        .ordinary
        .ordinary_admission()
        .begin_reader_maintenance(
            "Reader",
            fixture.selected.generation,
            MaintenancePolicy::StopAndDrain,
        )
        .unwrap();
    waiter.abort();
    assert!(waiter.await.err().unwrap().is_cancelled());
    assert!(!cleanup.confirmed());
    assert!(fixture.service.0.gate.native_pre_admission_active());
    assert!(maintenance.is_current());
    assert!(claim.is_current());
    held.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !cleanup.confirmed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!fixture.service.0.gate.native_reader_session_active());
    assert_eq!(fixture.trace.transactions.load(Ordering::SeqCst), 0);
    // The real retained pre has ended; a cancelled public waiter did not prove it.
    claim.release_confirmed().unwrap();
    maintenance.release_confirmed().unwrap();
    fixture
        .service
        .0
        .owner
        .native_close(&fixture.session)
        .unwrap();
}
