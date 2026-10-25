use super::*;
use crate::{reader_pcsc, reader_reservation, reader_session_lease};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
const READER: &str = "registration-number-reader";
const ICCID: &str = "894433000000000001";
const IMSI: &str = "234330000000001";
const NUMBER: &str = "+447700900123";
static NEXT: AtomicU64 = AtomicU64::new(0);
struct NoIo;
impl proletarius_sim::ApduTransport for NoIo {
    type Error = reader_pcsc::Failure;
    fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        panic!("registration notes must never write or read a SIM")
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
struct Supplier {
    session: reader_pcsc::LifecycleBinding,
}
impl reader_session_lease::NativeSessionSupplier for Supplier {
    fn pre_admit(
        self: Arc<Self>,
        _: String,
        _: reader_reservation::NativePinPolicy,
        _: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
        _: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
    ) -> reader_session_lease::PreAdmissionFuture {
        Box::pin(async { Ok(1) })
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
        Ok(
            json!({"reader":b.reader,"reader_generation":b.reader_generation,"session":self.session,"identity":{"iccid":ICCID,"imsi":IMSI,"mcc":"234","mnc":"33"}}),
        )
    }
    fn finalize(
        self: Arc<Self>,
        _: reader_reservation::NativePinBinding,
        _: Option<reader_session_lease::NativeCardContext>,
        _: String,
        _: String,
    ) -> reader_session_lease::SupplierFuture {
        Box::pin(async { Err("metadata cannot mutate card".into()) })
    }
}
struct Fixture {
    root: PathBuf,
    gate: Arc<Gate>,
    lease: Arc<reader_session_lease::NativeReaderSessionLease>,
    _supplier: Arc<dyn reader_session_lease::NativeSessionSupplier>,
    physical: reader_pcsc::Owner,
    session: reader_pcsc::LifecycleBinding,
}
impl Fixture {
    async fn new() -> Self {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap())
            .join("tmp/iteration-491-registration-identity")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
        std::fs::create_dir_all(&root).unwrap();
        let producer = "native-registration-number-producer";
        let physical = reader_pcsc::Owner::with_factory(
            producer.into(),
            Arc::new(|_| Ok(Box::new(NoIo))),
            Arc::new(|| Ok(vec![READER.into()])),
            Arc::new(|_, _| panic!("metadata cannot reset card")),
        );
        let session = physical.native_open(READER).unwrap();
        let gate = Arc::new(Gate::default());
        gate.initialize_business_producer("go-unused");
        gate.initialize_native_reader_producer(producer);
        gate.initialize_native_lifecycle(producer).unwrap();
        let transition = gate
            .prepare_native_transition(
                producer,
                &reader_reservation::SelectionSnapshot::default(),
                reader_reservation::NativeTransitionKind::Connect,
                READER,
                reader_reservation::NativeAuthority::Idle,
            )
            .unwrap();
        gate.commit_native_transition(
            &transition,
            Some(&session),
            reader_reservation::NativeReaderFacts {
                metadata: Some(
                    json!({"iccid":ICCID,"imsi":IMSI,"mcc":"234","mnc":"33","phone_number":""}),
                ),
                ..Default::default()
            },
        )
        .unwrap();
        let supplier: Arc<dyn reader_session_lease::NativeSessionSupplier> = Arc::new(Supplier {
            session: session.clone(),
        });
        gate.set_native_session_supplier(Arc::downgrade(&supplier));
        let lease = Arc::new(
            reader_session_lease::NativeReaderSessionLease::acquire(
                Path::new("no-go-or-card.sock"),
                None,
                READER,
                gate.clone(),
                physical.clone(),
            )
            .await
            .unwrap(),
        );
        Self {
            root,
            gate,
            lease,
            _supplier: supplier,
            physical,
            session,
        }
    }
    fn captured(&self) -> Captured {
        Captured {
            reader: READER.into(),
            generation: 1,
            iccid: Zeroizing::new(ICCID.into()),
            imsi: Zeroizing::new(IMSI.into()),
            sim_phone: Zeroizing::new(String::new()),
            configured_identity: Zeroizing::new("sip:configured@ims.example".into()),
        }
    }
    fn store(&self) -> profile_notes::Store {
        profile_notes::Store::new(&self.root)
    }
    fn seed(&self) {
        std::fs::write(self.root.join("profile-notes.json"),json!({format!("{ICCID}F"):{"label":"user custom label","phone":"+447700900000","tags":"personal,important","future":{"nested":[null,true,"preserved"]}},"894433000000000002":{"label":"another card","phone":"+85212345678","tags":"keep","future":[1,2,3]}}).to_string()).unwrap();
    }
    fn notes(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.root.join("profile-notes.json")).unwrap())
            .unwrap()
    }
    async fn finish(&self) {
        self.lease.release().await.unwrap();
        self.physical.native_close(&self.session).unwrap();
    }
}
#[tokio::test]
async fn actual_register_number_updates_exact_cache_and_survives_store_restart_without_overwriting_notes()
 {
    let f = Fixture::new().await;
    f.seed();
    let mut owner = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
    let key = crate::native_vowifi_supervisor::ReaderSessionKey::new(READER, ICCID).unwrap();
    let permit = owner.reserve(key.clone()).unwrap();
    let registered =
        crate::native_sms_originating::tests::registered_fixture_with_registration_headers(
            permit.stop_signal(),
            "<sip:+447700900123@ims.example>, <tel:+447700900999>",
            None,
        )
        .await;
    let session = registered.session.as_ref().unwrap();
    assert_eq!(session.final_response().header("Service-Route"), None);
    let (reporter, status) = Reporter::new(Binding {
        reader: READER.into(),
        reader_generation: 1,
        session_generation: permit.generation(),
        key,
    });
    reporter.tunnel_ready(&proletarius_vowifi::TunnelConfiguration::default());
    reporter.register_started();
    reporter.registered(session);
    observe_bound(
        f.captured(),
        session,
        &permit,
        &f.lease,
        f.gate.clone(),
        f.root.clone(),
        &reporter,
    )
    .await;
    assert_eq!(status.borrow().registered_msisdn.as_deref(), Some(NUMBER));
    assert_eq!(
        status.borrow().registered_number_source,
        Some("associated_uri")
    );
    assert!(status.borrow().registration_identity_error.is_none());
    let (_, facts, _) = f.gate.native_reader_snapshot();
    assert_eq!(facts.unwrap().metadata.unwrap()["phone_number"], NUMBER);
    let after = f.notes();
    let note = &after[format!("{ICCID}F")];
    assert_eq!(note["label"], "user custom label");
    assert_eq!(note["tags"], "personal,important");
    assert_eq!(note["phone"], NUMBER);
    assert_eq!(note["future"], json!({"nested":[null,true,"preserved"]}));
    assert!(after.get(ICCID).is_none());
    assert_eq!(after["894433000000000002"]["phone"], "+85212345678");
    assert_eq!(after["894433000000000002"]["future"], json!([1, 2, 3]));
    // Construct a fresh Store from the persisted file, as after daemon restart.
    assert_eq!(
        profile_notes::Store::new(&f.root)
            .registered_phone_fallback(ICCID)
            .unwrap(),
        NUMBER
    );
    registered.finish().await;
    f.finish().await;
}
#[tokio::test]
async fn missing_register_number_uses_sim_then_existing_profile_fallback_without_writing() {
    let f = Fixture::new().await;
    f.seed();
    let original = std::fs::read(f.root.join("profile-notes.json")).unwrap();
    let (_stop, stopped) = watch::channel(false);
    let mut captured = f.captured();
    captured.sim_phone = Zeroizing::new("+447700900888".into());
    let result = commit(captured, f.gate.clone(), f.store(), None, stopped.clone());
    assert_eq!(result.number.as_deref(), Some("+447700900888"));
    assert_eq!(result.source, Some("sim"));
    let result = commit(f.captured(), f.gate.clone(), f.store(), None, stopped);
    assert_eq!(result.number.as_deref(), Some("+447700900000"));
    assert_eq!(result.source, Some("profile_note"));
    assert_eq!(
        std::fs::read(f.root.join("profile-notes.json")).unwrap(),
        original
    );
    assert_eq!(
        f.gate.native_reader_snapshot().1.unwrap().metadata.unwrap()["phone_number"],
        ""
    );
    f.finish().await;
}
#[tokio::test]
async fn stale_generation_identity_and_cancel_cannot_touch_notes_or_replacement_metadata() {
    let f = Fixture::new().await;
    f.seed();
    let original = std::fs::read(f.root.join("profile-notes.json")).unwrap();
    let (stop, stopped) = watch::channel(false);
    for kind in 0..3 {
        let mut captured = f.captured();
        match kind {
            0 => captured.generation = 2,
            1 => captured.iccid = Zeroizing::new("894433000000000002".into()),
            _ => captured.imsi = Zeroizing::new("234330000000002".into()),
        };
        let result = commit(
            captured,
            f.gate.clone(),
            f.store(),
            Some(NUMBER.into()),
            stopped.clone(),
        );
        assert!(result.number.is_none());
    }
    stop.send_replace(true);
    assert!(
        commit(
            f.captured(),
            f.gate.clone(),
            f.store(),
            Some(NUMBER.into()),
            stopped
        )
        .number
        .is_none()
    );
    assert_eq!(
        std::fs::read(f.root.join("profile-notes.json")).unwrap(),
        original
    );
    assert_eq!(
        f.gate.native_reader_snapshot().1.unwrap().metadata.unwrap()["phone_number"],
        ""
    );
    f.finish().await;
}
#[tokio::test]
async fn corrupt_or_ambiguous_store_is_visible_and_preserves_bytes_and_actual_number() {
    let f = Fixture::new().await;
    let (_stop, stopped) = watch::channel(false);
    for (bytes,code) in [("corrupt".to_string(),"native_registered_phone_store_unavailable"),(json!({ICCID:{"label":"one","phone":"","tags":"a"},format!("{ICCID}F"):{"label":"two","phone":"","tags":"b"}}).to_string(),"native_registered_phone_ambiguous_iccid")] {
        std::fs::write(f.root.join("profile-notes.json"),bytes.as_bytes()).unwrap();
        let result=commit(f.captured(),f.gate.clone(),f.store(),Some(NUMBER.into()),stopped.clone());assert_eq!(result.error,Some(code));assert_eq!(result.number.as_deref(),Some(NUMBER));assert_eq!(result.source,Some("associated_uri"));assert_eq!(std::fs::read_to_string(f.root.join("profile-notes.json")).unwrap(),bytes);
    }
    assert_eq!(
        f.gate.native_reader_snapshot().1.unwrap().metadata.unwrap()["phone_number"],
        NUMBER
    );
    f.finish().await;
}
#[test]
fn missing_zero_and_failed_register_never_produce_a_learned_number() {
    for (status, identity, expected) in [
        (200, "<tel:+00000000>", None),
        (200, "<sip:subscriber@ims.example>", None),
        (401, "<tel:+447700900123>", None),
        (403, "<tel:+447700900123>", None),
        (200, "<tel:+447700900123>", Some(NUMBER)),
    ] {
        let response=proletarius_ims::SipResponse::parse(format!("SIP/2.0 {status} Fixture\r\nCall-ID: fixture\r\nCSeq: 1 REGISTER\r\nP-Associated-URI: {identity}\r\nContent-Length: 0\r\n\r\n").as_bytes()).unwrap();
        assert_eq!(
            registered_number(
                &response,
                "sip:configured@ims.example",
                &["<sip:10.20.30.9:6060;lr>".into()]
            )
            .as_deref(),
            expected
        );
    }
}
#[tokio::test]
async fn fifo_timeout_is_unsubmitted_and_does_not_release_the_occupied_worker() {
    let f = Fixture::new().await;
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let prior = f.lease.clone();
    let first = tokio::spawn(async move {
        prior
            .run_card_worker(&crate::blocking_pool::BlockingPool::new(1), move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(6)).unwrap();
            })
            .await
    });
    ready.await.unwrap();
    let submitted = Arc::new(AtomicBool::new(false));
    let observed = submitted.clone();
    let result = f
        .lease
        .run_registration_metadata_worker(&crate::blocking_pool::BlockingPool::new(1), move || {
            observed.store(true, Ordering::SeqCst)
        })
        .await;
    assert_eq!(result, Err(crate::reader_task::ReaderTaskError::Busy));
    assert!(!submitted.load(Ordering::SeqCst));
    assert!(f.gate.native_reader_session_active());
    assert!(!first.is_finished());
    assert!(!f.root.join("profile-notes.json").exists());
    release.send(()).unwrap();
    first.await.unwrap().unwrap();
    f.finish().await;
}
#[tokio::test]
async fn stop_before_store_admission_rejects_commit_but_waits_for_actual_worker() {
    let f = Fixture::new().await;
    f.seed();
    let original = std::fs::read(f.root.join("profile-notes.json")).unwrap();
    let lock = std::fs::File::create(f.root.join("profile-notes.json.lock")).unwrap();
    lock.lock().unwrap();
    let (stop, stopped) = watch::channel(false);
    let lease = f.lease.clone();
    let captured = f.captured();
    let gate = f.gate.clone();
    let store = f.store();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (committed, commit_returned) = tokio::sync::oneshot::channel();
    let (release_worker, worker_completion) = std::sync::mpsc::channel();
    let job = tokio::spawn(async move {
        lease
            .run_registration_metadata_worker(
                &crate::blocking_pool::BlockingPool::new(1),
                move || {
                    started.send(()).unwrap();
                    let result = commit(captured, gate, store, Some(NUMBER.into()), stopped);
                    // Stop may reject commit before it reaches the file lock.
                    // Keep the actual worker alive independently of that valid
                    // early return so release is tested against real completion.
                    committed.send(()).unwrap();
                    worker_completion.recv().unwrap();
                    result
                },
            )
            .await
    });
    ready.await.unwrap();
    stop.send_replace(true);
    let original_lease = f.lease.clone();
    let retiring = tokio::spawn(async move { original_lease.release().await });
    tokio::task::yield_now().await;
    assert!(!retiring.is_finished());
    assert!(!job.is_finished());
    assert!(f.gate.native_reader_session_active());
    drop(lock);
    commit_returned.await.unwrap();
    assert!(!job.is_finished());
    assert!(!retiring.is_finished());
    assert!(f.gate.native_reader_session_active());
    release_worker.send(()).unwrap();
    job.await.unwrap().unwrap();
    retiring.await.unwrap().unwrap();
    assert_eq!(
        std::fs::read(f.root.join("profile-notes.json")).unwrap(),
        original
    );
    assert_eq!(
        f.gate.native_reader_snapshot().1.unwrap().metadata.unwrap()["phone_number"],
        ""
    );
    f.physical.native_close(&f.session).unwrap();
}
#[tokio::test]
async fn completed_commit_is_not_undone_by_stop_and_release_waits_for_worker_completion() {
    let f = Fixture::new().await;
    f.seed();
    let lease = f.lease.clone();
    let store = f.store();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let job = tokio::spawn(async move {
        lease
            .run_registration_metadata_worker(
                &crate::blocking_pool::BlockingPool::new(1),
                move || {
                    let result = store.remember_registered_phone(ICCID, NUMBER, || true);
                    assert_eq!(result, Ok(Some(true)));
                    started.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(5)).unwrap();
                    result
                },
            )
            .await
    });
    ready.await.unwrap();
    assert_eq!(f.notes()[format!("{ICCID}F")]["phone"], NUMBER);
    let original = f.lease.clone();
    let retiring = tokio::spawn(async move { original.release().await });
    tokio::task::yield_now().await;
    assert!(!retiring.is_finished());
    assert!(f.gate.native_reader_session_active());
    release.send(()).unwrap();
    assert_eq!(job.await.unwrap().unwrap(), Ok(Some(true)));
    retiring.await.unwrap().unwrap();
    assert_eq!(
        profile_notes::Store::new(&f.root)
            .registered_phone_fallback(ICCID)
            .unwrap(),
        NUMBER
    );
    f.physical.native_close(&f.session).unwrap();
}
