use super::*;
use std::sync::{
    Condvar,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

#[derive(Default)]
struct Trace {
    commands: Mutex<Vec<String>>,
    block: Mutex<Option<String>>,
    entered: Notify,
    release: (Mutex<bool>, Condvar),
    fail: AtomicBool,
    cpin: AtomicU64,
    quoted_iccid: AtomicBool,
    drops: AtomicU64,
}
impl Trace {
    fn hold(&self, command: &str) {
        *self.block.lock().unwrap() = Some(command.into());
        *self.release.0.lock().unwrap() = false;
    }
    fn resume(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
    }
    fn count(&self) -> usize {
        self.commands.lock().unwrap().len()
    }
}
struct Port(Arc<Trace>);
impl Drop for Port {
    fn drop(&mut self) {
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl Transport for Port {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn description(&self) -> String {
        "USB exact fixture".into()
    }
    fn command(
        &mut self,
        command: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        self.0.commands.lock().unwrap().push(command.into());
        if self.0.block.lock().unwrap().as_deref() == Some(command) {
            self.0.entered.notify_one();
            let mut release = self.0.release.0.lock().unwrap();
            while !*release {
                release = self.0.release.1.wait(release).unwrap();
            }
            self.0.block.lock().unwrap().take();
        }
        if self.0.fail.load(Ordering::SeqCst) {
            return Err(TransportError {
                code: "usb_at_disconnected",
                message: "fixture lost".into(),
                response: String::new(),
                uncertain: true,
            });
        }
        Ok(match command {
            "AT+CFUN?" => "+CFUN: 1\r\nOK\r\n",
            "ATI" => "ATI\r\nFixture firmware\r\nOK\r\n",
            "AT+CPIN?" if self.0.cpin.load(Ordering::SeqCst) == 1 => "+CME ERROR: 10\r\n",
            "AT+CPIN?" if self.0.cpin.load(Ordering::SeqCst) == 2 => "ERROR\r\n",
            "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n",
            "AT+CSQ" => "+CSQ: 19,99\r\nOK\r\n",
            "AT+CEREG?" => "+CEREG: 0,5\r\nOK\r\n",
            "AT+CREG?" => "+CREG: 0,5\r\nOK\r\n",
            "AT+COPS?" => "+COPS: 0,2,\"46001\",7\r\nOK\r\n",
            "AT+QNWINFO" => "+QNWINFO: \"FDD LTE\",\"46001\",\"LTE BAND 3\",1650\r\nOK\r\n",
            "AT+QCCID" if self.0.quoted_iccid.load(Ordering::SeqCst) => {
                "+QCCID: \"8988212345678901234\"\r\nOK\r\n"
            }
            "AT+QCCID" => "+QCCID: 8988212345678901234\r\nOK\r\n",
            "AT+CIMI" => "234150123456789\r\nOK\r\n",
            "AT+CGSN" => "123456789012345\r\nOK\r\n",
            "AT+QCFG=\"usbnet\"" => "+QCFG: \"usbnet\",1\r\nOK\r\n",
            _ => "OK\r\n",
        }
        .into())
    }
}
async fn fixture() -> (Owner, Arc<Trace>, String) {
    let trace = Arc::new(Trace::default());
    let t = trace.clone();
    let (owner, session) =
        Owner::native_module_fixture(Arc::new(move || Ok(Box::new(Port(t.clone()))))).await;
    (owner, trace, session)
}
#[tokio::test]
async fn actual_public_usb_status_preserves_go_json_and_saved_phone() {
    let (owner, trace, session) = fixture().await;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build")
        .join(format!("status502-{}", crate::snapshot::new_instance_id()));
    std::fs::create_dir_all(&root).unwrap();
    let notes = crate::profile_notes::Store::new(&root);
    notes
        .remember_registered_phone("8988212345678901234", "+441234567890", || true)
        .unwrap();
    let owner = owner.with_profile_notes(notes);
    let observed = owner.native_device_observation(&session).await.unwrap();
    let v = &observed.status;
    assert_eq!(v["operator"], "中国联通");
    assert_eq!(v["phone_number"], "+441234567890");
    assert_eq!(v["signal_dbm"], -75);
    assert_eq!(v["imei"], "123456789012345");
    assert_eq!(v["reg_status"], 5);
    assert_eq!(v["signal_rsrp"], 0);
    assert_eq!(v["ps_attached"], false);
    assert!(v.get("operator_identifier").is_none());
    assert!(v.get("operator_name").is_none());
    assert!(v.get("native_spn").is_none());
    assert_eq!(trace.count(), 14);
    assert!(owner.device_observation_current(&observed));
    assert_eq!(
        owner.cached_device_observation().unwrap().status,
        observed.status
    );
    assert_eq!(trace.count(), 14, "display cache does not probe hardware");
    owner.set_cellular_suppressed(true);
    assert!(owner.cached_device_observation().is_none());
    assert!(!owner.device_observation_current(&observed));
    let start = trace.count();
    let suppressed = owner.native_device_observation(&session).await.unwrap();
    assert_eq!(suppressed.status["network_mode"], "飞行模式");
    assert_eq!(suppressed.status["signal_dbm"], -999);
    assert!(suppressed.status.get("radio_channel").is_none());
    assert!(
        !trace.commands.lock().unwrap()[start..]
            .iter()
            .any(|c| c == "AT+CSQ" || c == "AT+COPS?")
    );
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn lease_and_inhibit_refuse_without_any_at() {
    let (owner, trace, session) = fixture().await;
    owner
        .inner
        .state
        .lock()
        .unwrap()
        .inhibits
        .insert("physical-transition".into(), 1);
    let n = trace.count();
    assert!(owner.native_device_observation(&session).await.is_err());
    assert_eq!(trace.count(), n);
    owner.inner.state.lock().unwrap().inhibits.clear();
    owner
        .acquire(LeaseRequest {
            producer_id: "native-module-test".into(),
            session_id: session.clone(),
            lease_id: "other-sim".into(),
            lease_seq: 1,
            purpose: Purpose::Sim,
        })
        .await
        .unwrap();
    assert!(owner.native_device_observation(&session).await.is_err());
    assert_eq!(trace.count(), n);
    owner
        .release(ReleaseRequest {
            producer_id: "native-module-test".into(),
            session_id: session,
            lease_id: "other-sim".into(),
            lease_seq: 1,
        })
        .await
        .unwrap();
}
#[tokio::test]
async fn cancelled_waiter_retains_physical_io_and_actual_disconnect_retires_stream() {
    let (owner, trace, session) = fixture().await;
    trace.hold("AT+QCCID");
    let task_owner = owner.clone();
    let task_session = session.clone();
    let job =
        tokio::spawn(async move { task_owner.native_device_observation(&task_session).await });
    trace.entered.notified().await;
    job.abort();
    let _ = job.await;
    assert!(owner.inner.state.lock().unwrap().busy);
    assert!(owner.native_device_observation(&session).await.is_err());
    assert_eq!(trace.drops.load(Ordering::SeqCst), 0);
    trace.fail.store(true, Ordering::SeqCst);
    trace.resume();
    tokio::time::timeout(Duration::from_secs(3), async {
        while owner.inner.state.lock().unwrap().busy {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(trace.drops.load(Ordering::SeqCst), 1);
    assert!(owner.device_observation_description().is_none());
    assert!(owner.native_device_observation(&session).await.is_err());
}
#[tokio::test]
async fn reconfiguration_or_suppression_during_collection_never_publishes_old_result() {
    for suppression in [false, true] {
        let (owner, trace, session) = fixture().await;
        trace.hold("AT+QCCID");
        let task_owner = owner.clone();
        let job = tokio::spawn(async move { task_owner.native_device_observation(&session).await });
        trace.entered.notified().await;
        if suppression {
            owner.set_cellular_suppressed(true);
        } else {
            owner
                .configure(
                    ConfigureRequest::Sms {
                        producer_id: "native-module-test".into(),
                        config_seq: 1,
                        serial: true,
                        poll_interval_ms: 8000,
                    },
                    false,
                )
                .unwrap();
        }
        trace.resume();
        assert!(job.await.unwrap().is_err());
        assert!(owner.cached_device_observation().is_none());
    }
}

struct Fixture {
    root: std::path::PathBuf,
    state: crate::ServerState,
    trace: Arc<Trace>,
}
impl Fixture {
    async fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build")
            .join(format!(
                "status502-http-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&root).unwrap();

        let (usb, trace, session) = fixture().await;
        let radio =
            crate::module_radio::Service::native_vowifi_fixture(usb.clone(), session, root.clone())
                .await;
        assert_eq!(radio.native_vowifi_idle_mode().await.unwrap(), 1);
        let inbox = crate::message_archive::InboxState::default();
        let state = crate::ServerState {
            call_mode: Default::default(),
            module_call: Default::default(),
            reader_lifecycle: Default::default(),
            multistandby_card: Default::default(),
            multistandby_service: Default::default(),
            app_token: None,
            internal_token: "test".into(),
            multistandby_store: crate::multistandby_store::Store::open(&root).unwrap(),
            multistandby_operations: crate::multistandby_operations::Store::open(&root).unwrap(),
            device_send_store: crate::device_send_store::Store::open(&root).unwrap(),
            forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(&root).unwrap(),
            sms_forwarding: crate::sms_forwarding::Service::new(&root),
            sms_inbox: inbox.clone(),
            usb_sms_receive: crate::test_usb_sms_receiver(inbox, root.join("sms-archive.json")),
            usb_at: usb,
            module_radio: radio,
            call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
            call_facts: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                "call-producer-fixture".to_owned(),
            ),
            modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
            qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: crate::network_traffic::Service::default(),
            go_socket_path: root.join("go.sock"),
            availability: crate::BackendAvailability::new(),
            reader_discovery: Arc::new(Mutex::new(crate::ReaderDiscovery::default())),
            snapshot_instance_id: "public-http-fixture".into(),
            snapshot_revision: Arc::new(AtomicU64::new(0)),
            eap_exchanges: Arc::new(crate::eap_identity::EapExchangeStore::default()),
            reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
            reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
            native_vowifi_lifecycle: std::sync::Arc::new(
                crate::native_vowifi_session::NativeLifecycle::default(),
            ),
            ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
            multi_runtime_report: Arc::new(Mutex::new(
                crate::multistandby_runtime_report::Journal::default(),
            )),
        };
        Self { root, state, trace }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn public_native_status_without_go_and_busy_failure_fallback() {
    let f = Fixture::new().await;
    assert!(!f.state.go_socket_path.exists());
    let value = crate::device_status_projection::capture(&f.state)
        .await
        .unwrap();
    assert_eq!(value["operator"], "中国联通");
    assert_eq!(value["iccid"], "8988212345678901234");
    let reservation = f.state.module_radio.reserve_esim().await.unwrap();
    let n = f.trace.count();
    let value = crate::device_status_projection::capture(&f.state)
        .await
        .unwrap();
    assert_eq!(value["operator"], "已检测到 USB 设备");
    assert_eq!(value["sim_inserted"], false);
    assert!(value.get("iccid").is_none());
    assert_eq!(f.trace.count(), n);
    f.state
        .module_radio
        .release_esim(reservation)
        .await
        .unwrap();
    f.trace.fail.store(true, Ordering::SeqCst);
    let value = crate::device_status_projection::capture(&f.state)
        .await
        .unwrap();
    assert_eq!(value["operator"], "未连接");
    assert_eq!(value["usb_device"], Value::Null);
    assert!(value.get("iccid").is_none());
}
#[tokio::test]
async fn public_waiter_cancellation_retains_rf_until_physical_worker_finishes() {
    let f = Fixture::new().await;
    f.trace.hold("AT+QCCID");
    let state = f.state.clone();
    let job = tokio::spawn(async move { crate::device_status_projection::capture(&state).await });
    f.trace.entered.notified().await;
    job.abort();
    let _ = job.await;
    assert!(f.state.module_radio.sim_channel_busy());
    assert!(f.state.module_radio.reserve_esim().await.is_err());
    f.trace.resume();
    tokio::time::timeout(Duration::from_secs(3), async {
        while f.state.module_radio.sim_channel_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let value = crate::device_status_projection::capture(&f.state)
        .await
        .unwrap();
    assert_eq!(value["iccid"], "8988212345678901234");
}

#[tokio::test]
async fn changed_status_identity_retires_old_prepared_sms_capability() {
    for old_iccid in ["8988200000000000000", "8988212345678901234"] {
        let (owner, _, session) = fixture().await;
        let previous = {
            let mut state = owner.inner.state.lock().unwrap();
            let generation = state.identity_generation;
            state.module_identity = Some(ModuleSimIdentityCache {
                generation,
                iccid: Zeroizing::new(old_iccid.into()),
                imsi: Zeroizing::new("234150000000000".into()),
            });
            state.native_sms_identity = Some((session.clone(), generation));
            generation
        };
        assert!(owner.native_sms_binding_current(&session, previous));
        let observed = owner.native_device_observation(&session).await.unwrap();
        assert_eq!(observed.status["iccid"], "8988212345678901234");
        assert!(owner.device_observation_current(&observed));
        assert!(!owner.native_sms_binding_current(&session, previous));
        assert!(!owner.owns_native_sms_binding(&session, previous));
        let state = owner.inner.state.lock().unwrap();
        assert!(state.identity_generation > previous);
        assert!(state.module_identity.is_none());
    }
}

#[tokio::test]
async fn status_definite_absence_retires_identity_but_generic_error_does_not_invent_change() {
    let (owner, trace, session) = fixture().await;
    let (_, generation) = owner.capture_native_sms(&session).await.unwrap();
    trace.cpin.store(2, Ordering::SeqCst);
    let observed = owner.native_device_observation(&session).await.unwrap();
    assert_eq!(observed.status["sim_inserted"], false);
    assert!(owner.native_sms_binding_current(&session, generation));
    trace.cpin.store(1, Ordering::SeqCst);
    let observed = owner.native_device_observation(&session).await.unwrap();
    assert_eq!(observed.status["sim_inserted"], false);
    assert!(owner.device_observation_current(&observed));
    assert!(!owner.native_sms_binding_current(&session, generation));
    trace.cpin.store(0, Ordering::SeqCst);
    owner.native_device_observation(&session).await.unwrap();
    assert!(
        !owner.native_sms_binding_current(&session, generation),
        "a display read never revives submission authority"
    );
}

#[tokio::test]
async fn quoted_public_iccid_is_canonicalized_only_for_identity_comparison() {
    let (owner, trace, session) = fixture().await;
    let (_, generation) = owner.capture_native_sms(&session).await.unwrap();
    trace.quoted_iccid.store(true, Ordering::SeqCst);
    let observation = owner.native_device_observation(&session).await.unwrap();
    assert_eq!(observation.status["iccid"], "\"8988212345678901234\"");
    assert!(owner.native_sms_binding_current(&session, generation));
    assert!(owner.device_observation_current(&observation));
}
