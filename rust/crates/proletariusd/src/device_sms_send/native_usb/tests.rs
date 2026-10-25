use super::*;
use crate::usb_at_transport::{Transport, TransportError};
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicU64, AtomicUsize},
};
use tokio::sync::Notify;

#[derive(Default)]
struct Trace {
    iccid: Mutex<String>,
    commands: Mutex<Vec<String>>,
    sends: AtomicUsize,
    block_send: AtomicBool,
    lose_ack: AtomicBool,
    entered: Notify,
    release: (Mutex<bool>, Condvar),
}
struct Port(Arc<Trace>);
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
        "Native SMS USB".into()
    }
    fn command(
        &mut self,
        command: &str,
        payload: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> std::result::Result<String, TransportError> {
        self.0.commands.lock().unwrap().push(command.into());
        if command.starts_with("AT+CMGS=") {
            assert_eq!(payload.unwrap().last(), Some(&0x1a));
            self.0.sends.fetch_add(1, Ordering::SeqCst);
            self.0.entered.notify_one();
            if self.0.block_send.load(Ordering::SeqCst) {
                let mut release = self.0.release.0.lock().unwrap();
                while !*release {
                    release = self.0.release.1.wait(release).unwrap();
                }
            }
            if self.0.lose_ack.load(Ordering::SeqCst) {
                return Err(TransportError {
                    code: "usb_at_disconnected",
                    message: "lost submitted response".into(),
                    response: String::new(),
                    uncertain: true,
                });
            }
            return Ok("+CMGS: 17\r\nOK\r\n".into());
        }
        Ok(match command {
            "AT+CFUN?" => "+CFUN: 1\r\nOK\r\n".into(),
            "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n".into(),
            "AT+QCCID" => format!("+QCCID: {}\r\nOK\r\n", self.0.iccid.lock().unwrap()),
            "AT+CIMI" => "001010000000001\r\nOK\r\n".into(),
            _ => "OK\r\n".into(),
        })
    }
}
struct Fixture {
    root: PathBuf,
    state: crate::ServerState,
    trace: Arc<Trace>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    async fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build")
            .join(format!("sms502-{}", crate::snapshot::new_instance_id()));
        std::fs::create_dir_all(&root).unwrap();
        let trace = Arc::new(Trace::default());
        *trace.iccid.lock().unwrap() = "8901000000000000000".into();
        let tr = trace.clone();
        let (usb, session) = crate::usb_at::Owner::native_module_fixture(Arc::new(move || {
            Ok(Box::new(Port(tr.clone())))
        }))
        .await;
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
fn body(id: &str) -> Value {
    json!({"client_request_id":id,"phone":"+15550102020","message":"native USB fixture","device":"dji"})
}
async fn request(
    state: &crate::ServerState,
    method: &str,
    path: &str,
    value: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Full::new(Bytes::from(serde_json::to_vec(&value).unwrap())))
        .unwrap();
    let response = handle(request, state).await;
    let status = response.status();
    let data = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&data).unwrap())
}
async fn prepare(f: &Fixture, id: &str) {
    let (status, value) = request(&f.state, "POST", "/api/sms/send-intents", body(id)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["can_submit"], true);
}
#[tokio::test]
async fn default_usb_real_capture_send_release_without_go_preserves_generation_and_no_replay() {
    let f = Fixture::new().await;
    assert!(!f.state.go_socket_path.exists());
    let mut implicit = body("native-first");
    implicit["device"] = json!("");
    let (status, value) = request(&f.state, "POST", "/api/sms/send-intents", implicit).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["device"], "dji");
    let first = f
        .state
        .device_send_store
        .apply("get", json!({"client_request_id":"native-first"}))
        .unwrap();
    prepare(&f, "native-second").await;
    let second = f
        .state
        .device_send_store
        .apply("get", json!({"client_request_id":"native-second"}))
        .unwrap();
    assert_eq!(
        first["binding"], second["binding"],
        "a fresh read of the same card must not invalidate earlier prepared work"
    );
    let (status, value) = request(&f.state, "POST", "/api/sms/send", body("native-first")).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["sent"], true);
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 1);
    assert!(!f.state.module_radio.sim_channel_busy());
    let binding = first["binding"].as_str().unwrap();
    let (session, generation) = usb_route_session_generation(binding).unwrap();
    assert!(
        f.state
            .usb_at
            .native_sms_binding_current(session, generation)
    );
    assert!(
        !f.state
            .usb_at
            .has_active_sim_identity_binding(session, generation),
        "actual short SIM lease released"
    );
    let (status, value) = request(&f.state, "POST", "/api/sms/send", body("native-first")).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 1);
    let archive = std::fs::read_to_string(f.root.join("sms-archive.json")).unwrap();
    assert!(archive.contains("native-first"));
}
#[tokio::test]
async fn actual_changed_iccid_after_claim_becomes_unknown_without_any_cmgs_or_replay() {
    let f = Fixture::new().await;
    prepare(&f, "changed").await;
    let original = f
        .state
        .device_send_store
        .apply("get", json!({"client_request_id":"changed"}))
        .unwrap();
    let (old_session, old_generation) =
        usb_route_session_generation(original["binding"].as_str().unwrap()).unwrap();
    *f.trace.iccid.lock().unwrap() = "8901000000000000001".into();
    let (status, value) = request(&f.state, "POST", "/api/sms/send", body("changed")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
    assert_eq!(value["code"], "sms_send_outcome_unknown");
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 0);
    assert!(!f.state.module_radio.sim_channel_busy());
    assert!(
        !f.state
            .usb_at
            .native_sms_binding_current(old_session, old_generation)
    );
    *f.trace.iccid.lock().unwrap() = "8901000000000000000".into();
    prepare(&f, "replacement-prepare").await;
    assert!(
        !f.state
            .usb_at
            .native_sms_binding_current(old_session, old_generation),
        "returning the old card must not resurrect its consumed identity generation"
    );
    let _ = request(&f.state, "POST", "/api/sms/send", body("changed")).await;
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn lost_cmgs_response_freezes_claim_and_never_falls_back_to_go() {
    let f = Fixture::new().await;
    prepare(&f, "lost").await;
    f.trace.lose_ack.store(true, Ordering::SeqCst);
    let (status, value) = request(&f.state, "POST", "/api/sms/send", body("lost")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
    assert_eq!(value["code"], "sms_send_outcome_unknown");
    let _ = request(&f.state, "POST", "/api/sms/send", body("lost")).await;
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 1);
    assert!(!f.state.module_radio.sim_channel_busy());
    assert!(f.state.usb_at.device_observation_description().is_none());
}
#[tokio::test]
async fn cancelled_http_waiter_and_go_recovery_cannot_release_live_cmgs_or_replay() {
    let f = Fixture::new().await;
    prepare(&f, "cancelled").await;
    f.trace.block_send.store(true, Ordering::SeqCst);
    let state = f.state.clone();
    let job =
        tokio::spawn(
            async move { request(&state, "POST", "/api/sms/send", body("cancelled")).await },
        );
    f.trace.entered.notified().await;
    job.abort();
    let _ = job.await;
    assert!(f.state.module_radio.sim_channel_busy());
    assert!(f.state.module_radio.reserve_esim().await.is_err());
    f.state.device_send_store.recover_go_owned().unwrap();
    let record = f
        .state
        .device_send_store
        .apply("get", json!({"client_request_id":"cancelled"}))
        .unwrap();
    assert_eq!(record["state"], "submitting");
    let _ = request(&f.state, "POST", "/api/sms/send", body("cancelled")).await;
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 1);
    *f.trace.release.0.lock().unwrap() = true;
    f.trace.release.1.notify_all();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let record = f
                .state
                .device_send_store
                .apply("get", json!({"client_request_id":"cancelled"}))
                .unwrap();
            if record["state"] == "accepted" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!f.state.module_radio.sim_channel_busy());
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn busy_native_radio_and_expected_subscription_fail_without_go_or_identity_probe() {
    let f = Fixture::new().await;
    let intent = f.state.module_radio.native_vowifi_intent().unwrap();
    intent.prepare().await.unwrap();
    let n = f.trace.commands.lock().unwrap().len();
    let (status, value) = request(&f.state, "POST", "/api/sms/send-intents", body("busy")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(f.trace.commands.lock().unwrap().len(), n);
    intent.restore().await.unwrap();
    let mut v = body("subscription");
    v["expected_subscription_id"] = json!("unexpected");
    let (status, value) = request(&f.state, "POST", "/api/sms/send-intents", v).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn absent_native_usb_preserves_legacy_selection_codes_and_never_claims_store_failure() {
    let mut f = Fixture::new().await;
    f.state.usb_at.shutdown().await.unwrap();
    f.state.usb_at = crate::usb_at::Owner::unavailable_for_tests("absent-sms".into());
    let before = f.trace.commands.lock().unwrap().len();
    let (status, value) = request(
        &f.state,
        "POST",
        "/api/sms/send-intents",
        body("absent-explicit"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{value}");
    assert_eq!(
        value,
        json!({"code":"sms_device_not_ready","error":"未知号码_DJI：未检测到 DJI 模块","recoverable":true})
    );
    let mut implicit = body("absent-implicit");
    implicit["device"] = json!("");
    let (status, value) = request(&f.state, "POST", "/api/sms/send-intents", implicit).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{value}");
    assert_eq!(
        value,
        json!({"code":"sms_device_unavailable","error":"当前没有可用的短信发送设备","recoverable":true})
    );
    assert_eq!(f.trace.commands.lock().unwrap().len(), before);
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pure_prepare_radio_or_flight_mode_blockage_is_not_a_persistent_store_error() {
    let f = Fixture::new().await;
    let reserve = f.state.module_radio.reserve_esim().await.unwrap();
    let before = f.trace.commands.lock().unwrap().len();
    let (status, value) =
        request(&f.state, "POST", "/api/sms/send-intents", body("reserved")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{value}");
    assert_eq!(value["code"], "sms_device_not_ready");
    assert_eq!(value["recoverable"], true);
    assert_eq!(f.trace.commands.lock().unwrap().len(), before);
    f.state.module_radio.release_esim(reserve).await.unwrap();
    crate::module_radio_store::Store::new(f.root.clone())
        .save(crate::module_radio_store::Preferences {
            enabled: true,
            restore_cfun: 1,
            user_configured: true,
        })
        .unwrap();
    let (status, value) = request(&f.state, "POST", "/api/sms/send-intents", body("flight")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{value}");
    assert_eq!(value["code"], "sms_device_not_ready");
    assert_eq!(value["recoverable"], true);
    assert!(!f.state.module_radio.sim_channel_busy());
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 0);
}

struct OwnedReader;
impl ReaderSms for OwnedReader {
    fn has_intent(&self) -> bool {
        true
    }
    fn capture(
        &self,
    ) -> std::result::Result<crate::native_vowifi_session::NativeSmsBinding, &'static str> {
        panic!("implicit target must be rejected before probing either reader or USB")
    }
    fn resolve(
        &self,
        _: &str,
    ) -> std::result::Result<crate::native_vowifi_session::NativeSmsBinding, &'static str> {
        panic!("selection does not resolve a capability")
    }
}
#[tokio::test]
async fn reader_intent_still_requires_explicit_device_before_any_native_usb_capture() {
    let f = Fixture::new().await;
    let service = Service {
        job: None,
        #[cfg(test)]
        shutdown_hooks: Default::default(),
        #[cfg(test)]
        archive_executor: None,
        compatibility_fixture: false,
        store: f.state.device_send_store.clone(),
        archive: ArchiveStore::new(f.root.join("sms-archive.json")),
        socket: f.state.go_socket_path.clone(),
        app_token: None,
        internal_token: "test".into(),
        usb_at: f.state.usb_at.clone(),
        modem_actor: f.state.modem_actor.clone(),
        module_radio: f.state.module_radio.clone(),
        availability: f.state.availability.clone(),
        native_sms: Arc::new(OwnedReader),
        native_module_sms: Arc::new(RuntimeModuleSms(f.state.native_vowifi_lifecycle.clone())),
        io_timeout: RPC_TIMEOUT,
        claims: Arc::new(Semaphore::new(1)),
    };
    let before = f.trace.commands.lock().unwrap().len();
    let body = SendRequest {
        phone: "12345".into(),
        message: "selection".into(),
        ..Default::default()
    };
    let failure = match service.capture(&body).await {
        Ok(_) => panic!("implicit USB must not steal reader selection"),
        Err(error) => error,
    };
    assert_eq!(failure.0, StatusCode::CONFLICT);
    assert_eq!(failure.1["code"], "sms_device_selection_required");
    assert_eq!(f.trace.commands.lock().unwrap().len(), before);
    assert_eq!(f.trace.sends.load(Ordering::SeqCst), 0);
}
