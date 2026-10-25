use super::*;
use crate::{reader_pcsc, reader_reservation, reader_session_lease};

struct NoIo;
impl proletarius_sim::ApduTransport for NoIo {
    type Error = reader_pcsc::Failure;
    fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        panic!("join regression must not issue APDU")
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
struct Identity(intent::ExpectedIdentity);
impl IdentitySource for Identity {
    fn capture(&self) -> Result<intent::ExpectedIdentity, String> {
        Ok(self.0.clone())
    }
}
struct Supplier(reader_pcsc::LifecycleBinding);
impl reader_session_lease::NativeSessionSupplier for Supplier {
    fn pre_admit(
        self: Arc<Self>,
        _: String,
        _: reader_reservation::NativePinPolicy,
        _ordinary: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
        _maintenance: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
    ) -> reader_session_lease::PreAdmissionFuture {
        Box::pin(async { Ok(4) })
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
            json!({"reader":b.reader,"reader_generation":b.reader_generation,"session":self.0,
            "identity":{"iccid":"894433000000000001","imsi":"234330000000001","mcc":"234","mnc":"33",
            "private_identity":"234330000000001@ims.mnc033.mcc234.3gppnetwork.org","public_identity":"sip:234330000000001@ims.mnc033.mcc234.3gppnetwork.org"}}),
        )
    }
    fn finalize(
        self: Arc<Self>,
        _: reader_reservation::NativePinBinding,
        _: Option<reader_session_lease::NativeCardContext>,
        _: String,
        _: String,
    ) -> reader_session_lease::SupplierFuture {
        Box::pin(async { Err("join regression forbids mutation".into()) })
    }
}

#[tokio::test]
async fn released_lease_does_not_ack_cleanup_until_the_original_task_has_actually_joined() {
    let reader = "cleanup-join-regression";
    let physical = reader_pcsc::Owner::with_factory(
        "cleanup-join-native-producer-0001".into(),
        Arc::new(|_| Ok(Box::new(NoIo))),
        Arc::new(move || Ok(vec![reader.into()])),
        Arc::new(|_, _| panic!("no reset")),
    );
    let session = physical.native_open(reader).unwrap();
    let gate = Arc::new(reader_reservation::Gate::default());
    gate.initialize_business_producer("legacy-join-fixture");
    gate.initialize_native_reader_producer("cleanup-join-native-producer-0001");
    gate.test_seed_selection(reader, 4);
    let supplier: Arc<dyn reader_session_lease::NativeSessionSupplier> =
        Arc::new(Supplier(session));
    gate.set_native_session_supplier(Arc::downgrade(&supplier));
    let lease = Arc::new(
        NativeReaderSessionLease::acquire(
            Path::new("unused.sock"),
            None,
            reader,
            gate.clone(),
            physical.clone(),
        )
        .await
        .unwrap(),
    );
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
    let subscriber_identity = resolve_session_subscriber_identity(&ims, &lease.identity).unwrap();
    let device_identity = ims
        .resolve_device_identity(Some("356938035643809"))
        .unwrap();
    let context = NativeSessionContext {
        reader: reader.into(),
        reader_generation: 4,
        session_key: crate::native_vowifi_supervisor::ReaderSessionKey::new(
            reader,
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
        ims,
        subscriber_identity,
        device_identity,
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
    let lifecycle = Arc::new(NativeLifecycle::default());
    lifecycle.activate().unwrap();
    let data_root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap())
        .join("tmp/iteration-484-cleanup-join");
    let calls = crate::call_facts::CallFacts::new(
        proletarius_store::ArchiveStore::new(data_root.join("archive.json")),
        "join-fixture".into(),
    );
    let transactions = crate::ims_sms_transactions::Service::default();
    let handle = lifecycle
        .launch_ordinary(
            context,
            ordinary::Inputs {
                sms_archive: Default::default(),
                settings: startup_settings().unwrap(),
                reader_gate: gate.clone(),
                data_root: data_root.clone(),
                transactions: transactions.clone(),
                call_facts: calls.clone(),
            },
        )
        .unwrap_or_else(|_| panic!("join fixture admission"));
    handle.request_shutdown(); // Stop before the task's first poll: no tunnel/AKA/network I/O.
    let (published, released_before_join) = tokio::sync::oneshot::channel();
    let (complete, finish) = tokio::sync::oneshot::channel();
    {
        let mut state = lifecycle.state.lock().unwrap();
        let owner = state.owner.as_mut().unwrap();
        let original = owner.ordinary_tasks.remove(&handle.generation).unwrap();
        let (cancel, _) = watch::channel(false);
        // Deterministically widen the production window between publication
        // of the real lease ACK and this retained owner's JoinHandle return.
        owner.ordinary_tasks.insert(
            handle.generation,
            crate::native_ims_actor::Task::spawn(cancel, async move {
                let result = original.finish().await.unwrap();
                assert!(matches!(result.lease_release, Some(Ok(()))));
                published.send(()).unwrap();
                let _ = finish.await;
                result
            }),
        );
    }
    let expected = intent::ExpectedIdentity {
        reader: reader.into(),
        reader_generation: 4,
        iccid: sim.iccid,
        imsi: lease.identity.imsi.to_string(),
    };
    let attempt = intent::Attempt {
        intent_id: 11,
        number: 2,
        expected: expected.clone(),
        request: intent::FrozenRequest {
            proxy: "127.0.0.1:9".into(),
            epdg: "epdg.fixture.invalid".into(),
            epdg_port: 4500,
        },
    };
    let driver = Arc::new(LiveDriver {
        ordinary_claim: None,
        resources: Resources {
            legacy_module_claim: Arc::new(|| false),
            ordinary_admission: lifecycle.ordinary_admission(),
            identity: Arc::new(Identity(expected)),
            lifecycle: Arc::downgrade(&lifecycle),
            gate: gate.clone(),
            pcsc: physical,
            socket: PathBuf::from("unused.sock"),
            token: None,
            data_root,
            transactions,
            calls,
        },
        current: Mutex::new(AttemptState {
            key: Some((11, 2)),
            handle: Some(handle.clone()),
            acquire_unconfirmed: true,
            ..Default::default()
        }),
    });
    tokio::time::timeout(Duration::from_secs(2), released_before_join)
        .await
        .unwrap()
        .unwrap();
    assert!(released(&handle.snapshot()));
    assert!(lease.release_confirmed().await.unwrap());
    assert!(!gate.native_reader_session_active());
    assert_eq!(
        driver.clone().await_cleanup(attempt.clone()).await,
        Err("native_ordinary_worker_join_unconfirmed".into())
    );
    assert!(driver.current.lock().unwrap().result.is_none());
    assert!(
        lifecycle
            .state
            .lock()
            .unwrap()
            .owner
            .as_ref()
            .unwrap()
            .ordinary_tasks
            .contains_key(&handle.generation)
    );
    complete.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while driver.clone().await_cleanup(attempt.clone()).await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(driver.current.lock().unwrap().result.is_some());
    assert!(
        driver.clone().await_cleanup(attempt.clone()).await.is_ok(),
        "the joined result is retained as an idempotent exact-attempt receipt"
    );
    let mut wrong = attempt;
    wrong.number += 1;
    assert_eq!(
        driver.await_cleanup(wrong).await,
        Err("native_attempt_cleanup_identity_mismatch".into())
    );
    assert!(
        lifecycle
            .finish_shutdown(tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_ok()
    );
}
