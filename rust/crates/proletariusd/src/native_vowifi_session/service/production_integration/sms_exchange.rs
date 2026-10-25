//! Public durable SMS submission and MT archive/RP-ACK on the same production
//! Service/LiveDriver, SIM lease, IKE owner and protected IMS TCP generation.
use super::sms_peer::{Command, Event, OUTGOING, RECIPIENT};
use super::*;
use crate::native_registered_session::Control;
use crate::native_sms_originating::{Phase, RpResult};
use serde_json::Value;
use tokio::sync::oneshot;

impl Fixture {
    pub(super) fn sms_state(&self) -> crate::ServerState {
        self.gateway
            .get_or_init(|| {
                assert!(
                    self.lifecycle
                        .ordinary_service
                        .set(Ok(self.service.clone()))
                        .is_ok()
                );
                let inbox = crate::message_archive::InboxState::default();
                let mut reader_discovery = crate::ReaderDiscovery::default();
                reader_discovery.reconcile(
                    vec![proletarius_reader::ReaderObservation {
                        name: "Production Reader".into(),
                        card: proletarius_reader::CardPresence::Present,
                        in_use: false,
                        muted: false,
                        atr: Vec::new(),
                    }],
                    Some("Production Reader"),
                );
                crate::ServerState {
                    call_mode: Default::default(),
                    module_call: Default::default(),
                    reader_lifecycle: Arc::new(std::sync::OnceLock::from(Ok(self.reader.clone()))),
                    multistandby_card: Default::default(),
                    multistandby_service: Default::default(),
                    app_token: Some(APP_TOKEN.to_owned()),
                    internal_token: "production-491-no-go".into(),
                    multistandby_store: crate::multistandby_store::Store::open(&self.root).unwrap(),
                    multistandby_operations: crate::multistandby_operations::Store::open(
                        &self.root,
                    )
                    .unwrap(),
                    multi_runtime_report: Arc::new(Mutex::new(Default::default())),
                    device_send_store: crate::device_send_store::Store::open(&self.root).unwrap(),
                    forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(
                        &self.root,
                    )
                    .unwrap(),
                    sms_forwarding: Default::default(),
                    sms_inbox: inbox.clone(),
                    usb_sms_receive: crate::test_usb_sms_receiver(
                        inbox,
                        self.root.join("sms-archive.json"),
                    ),
                    usb_at: crate::usb_at::Owner::unavailable_for_tests(
                        "production-491-no-usb".into(),
                    ),
                    module_radio: crate::test_module_radio(&self.root),
                    call_archive: proletarius_store::ArchiveStore::new(
                        self.root.join("calls.json"),
                    ),
                    call_facts: self.service.0.resources.calls.clone(),
                    modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
                    qmi_enhancement: Default::default(),
                    network_traffic: Default::default(),
                    go_socket_path: self.root.join("never-created-go.sock"),
                    availability: crate::BackendAvailability::new(),
                    reader_discovery: Arc::new(Mutex::new(reader_discovery)),
                    reader_pcsc: self.service.0.resources.pcsc.clone(),
                    reader_gate: self.service.0.resources.gate.clone(),
                    snapshot_instance_id: "production-491".into(),
                    snapshot_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    eap_exchanges: Arc::new(Default::default()),
                    native_vowifi_lifecycle: self.lifecycle.clone(),
                    ims_sms_transactions: self.service.0.resources.transactions.clone(),
                }
            })
            .clone()
    }
}
pub(super) async fn sms_http(
    state: &crate::ServerState,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let body = public_call_body(method, path, body);
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
        .unwrap();
    let response = gateway::send(state, request).await;
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn incoming_confirmed(client: &mut sms_peer::Client) {
    assert_eq!(client.next().await, Event::IncomingAccepted);
    assert_eq!(client.next().await, Event::IncomingRpAck);
}

async fn select_reader_from_native_session(
    state: &crate::ServerState,
    requested: &str,
    module_ready: bool,
) -> Value {
    let mut facts =
        serde_json::to_value(crate::sms_device_service::DeviceFacts::default()).unwrap();
    facts["module"]["modem_present"] = json!(module_ready);
    // A compatibility peer can neither hide a real registered reader nor
    // resurrect it after its retained worker and physical lease have stopped.
    facts["reader"] = json!({"connected":true,"session_active":true,
        "identity_valid":true,"ims_registered":true,"sms_ready":true,
        "registered_phone":"+447700900999","subscription_id":"FORGED-COMPAT"});
    let body = serde_json::to_vec(&json!({"requested":requested,"facts":facts})).unwrap();
    let (status, value) = crate::reader_pcsc_gateway_tests::request_with_body(
        state.clone(),
        crate::sms_device_service::SELECT_PATH,
        false,
        false,
        Some(("X-Switch-Internal-Token", "production-491-no-go")),
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

async fn assert_native_reader_devices(state: &crate::ServerState, subscription: Option<&str>) {
    let (status, value) = sms_http(state, "GET", "/api/sms/devices", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let devices = value["devices"].as_array().unwrap();
    let reader = devices
        .iter()
        .find(|device| device["id"] == "reader")
        .unwrap();
    assert_eq!(reader["available"], true, "{reader}");
    assert_eq!(reader["phone"], "+447700900123");
    assert_eq!(reader["ready"], subscription.is_some(), "{reader}");
    assert_eq!(reader["subscription_id"].as_str(), subscription, "{reader}");
    let module = devices.iter().find(|device| device["id"] == "dji").unwrap();
    assert_eq!(module["ready"], false);
    assert_eq!(module["detail"], "未检测到 DJI 模块");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_sms_uses_production_ike_registered_session_archive_claim_and_rp_ack() {
    // The ordinary prepare reads supported user YAML. Isolate this override so
    // parallel workspace tests never mutate process-global carrier settings.
    if std::env::var_os("SWITCH_PRODUCTION_491_SMS_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/production-491-config-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier-overrides.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output=tokio::task::spawn_blocking(move || std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","native_vowifi_session::service::production_integration::sms_exchange::public_sms_uses_production_ike_registered_session_archive_claim_and_rp_ack","--nocapture"])
            .env("SWITCH_PRODUCTION_491_SMS_CHILD","1").env("PROLETARIUS_VOWIFI_CONFIG",config).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "child SMS fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(30),async {
        let fixture=Fixture::new();let state=fixture.sms_state();
        assert_ne!(state.availability.snapshot().phase, crate::BackendPhase::Ready);
        let subscription=native_device_sms_subscription_id(ICCID,IMSI).unwrap();
        let (peer,mut client)=ike_peer::Peer::with_sms_go_exit(fixture.root.clone(),subscription.clone()).await;
        let response=gateway::request(&fixture, Request::builder().method("POST").uri("/api/reader/vowifi")
            .body(Full::new(Bytes::from(serde_json::to_vec(&json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true,"proxy":peer.proxy.to_string(),"epdg":"epdg.invalid","epdg_port":500})).unwrap()))).unwrap()).await;
        assert_eq!(response.status(),StatusCode::ACCEPTED);
        let binding=fixture.registered().await;
        assert_eq!(binding.generation.value(),1);assert_eq!(binding.subscription_id,subscription);
        assert_eq!(fixture.reader.snapshot().usim_info.unwrap()["phone_number"], "+447700900123");
        let notes: Value = serde_json::from_slice(&std::fs::read(fixture.root.join("profile-notes.json")).unwrap()).unwrap();
        assert_eq!(notes[ICCID]["phone"], "+447700900123");
        incoming_confirmed(&mut client).await;
        assert_native_reader_devices(&state, Some(&subscription)).await;
        let selected=select_reader_from_native_session(&state,"reader",false).await;
        assert_eq!(selected["selected"],"reader");assert!(selected.get("error").is_none());
        // A retired peer cannot invent a second ready device and make the
        // genuine native reader ambiguous.
        let selected=select_reader_from_native_session(&state,"",true).await;
        assert_eq!(selected["selected"],"reader");assert!(selected.get("error").is_none());
        let body=json!({"client_request_id":"production-491-outgoing","phone":RECIPIENT,"message":OUTGOING,"device":"reader","expected_subscription_id":subscription});
        let (status,prepared)=sms_http(&state,"POST","/api/sms/send-intents",body.clone()).await;
        assert_eq!(status,StatusCode::OK,"{prepared}");assert_eq!(prepared["state"],"prepared");
        let (status,sent)=sms_http(&state,"POST","/api/sms/send",body.clone()).await;
        assert_eq!(status,StatusCode::OK,"{sent}");assert_eq!(sent["sent"],true);assert_eq!(sent["segments"],1);
        let Event::Submitted(reference)=client.next().await else {panic!("outgoing RP-DATA was not observed")};
        assert_eq!(client.next().await,Event::SubmitRpAckAccepted);
        let record=state.device_send_store.apply("get",json!({"client_request_id":"production-491-outgoing"})).unwrap();
        assert_eq!(record["state"],"accepted");assert_eq!(record["archive_confirmed"],true);
        assert!(record["binding"].as_str().unwrap().starts_with("rust_reader_ims:"));
        assert_eq!(record["subscription_id"],subscription);
        let batch=format!("device_sms_{}",record["attempt"].as_str().unwrap());
        let (reply,result)=oneshot::channel();
        binding.controls.send(Control::SmsStatus {generation:binding.generation,batch_id:batch,reply}).await.unwrap();
        let receipt=result.await.unwrap().unwrap().unwrap();
        assert_eq!(receipt.generation,binding.generation);assert_eq!(receipt.phase,Phase::Accepted);
        assert_eq!(receipt.accepted_parts,1);assert_eq!(receipt.rp_results,vec![(1,RpResult::Ack)]);
        assert!(receipt.rp_results.iter().any(|(_, result)| *result == RpResult::Ack), "wire reference {reference} did not resolve");
        // Public retry of the same immutable intent returns its durable result;
        // the peer rejects any second RP-DATA on this real association.
        let (status,duplicate_send)=sms_http(&state,"POST","/api/sms/send",body).await;
        assert_eq!(status,StatusCode::OK,"{duplicate_send}");assert_eq!(duplicate_send["sent"],true);
        client.commands.send(Command::Duplicate).await.unwrap();incoming_confirmed(&mut client).await;
        let rows=proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json")).load().unwrap();
        assert_eq!(rows.len(),2);assert_eq!(rows.iter().filter(|v|v["direction"]=="in").count(),1);
        let outgoing=rows.iter().find(|v|v["direction"]=="out").unwrap();
        assert_eq!(outgoing["content"],OUTGOING);assert_eq!(outgoing["send_request_id"],"production-491-outgoing");
        assert!(rows.iter().all(|v|v["subscription_id"]==subscription));
        let stable=fixture.service.sms_binding().unwrap();assert_eq!(stable.binding,binding.binding);
        assert_eq!(stable.generation,binding.generation);
        peer.trace.stop_requested.store(true,Ordering::SeqCst);
        gateway::stop(&fixture).await;
        assert!(fixture.service.resolve_sms_binding(&binding.binding).is_err());
        assert_native_reader_devices(&state,None).await;
        let stopped=select_reader_from_native_session(&state,"reader",false).await;
        assert!(stopped.get("selected").is_none());assert_eq!(stopped["error"]["code"],"sms_device_not_ready");
        {
            let record = fixture.service.record().unwrap().unwrap();
            let current = record.driver.current.lock().unwrap();
            let result = current.result.as_ref().expect("actual ordinary worker result must be joined");
            // Successful SMS notices are intentionally omitted from public
            // ordinary diagnostics. The joined real dispatcher still retains
            // any unconfirmed report, so its exit is the authoritative drain.
            let ordinary::Outcome::Running(Ok(exit)) = &result.outcome else { panic!("missing actual registered worker exit") };
            let consumer = exit.consumer.as_ref().expect("dispatcher must join");
            assert_eq!(consumer.generation, binding.generation);
            assert!(consumer.pending_reports.is_empty(), "incoming RP-ACK final responses were not consumed");
            assert!(consumer.timer_failures.is_empty());
            assert!(consumer.media_cleanup.is_ok());
            assert!(result.lease_release.as_ref().is_some_and(Result::is_ok));
        }
        assert_eq!(crate::device_send_store::Store::open(&fixture.root).unwrap().apply("get",json!({"client_request_id":"production-491-outgoing"})).unwrap(),record);
        assert_eq!(state.device_send_store.apply("get",json!({"client_request_id":"production-491-outgoing"})).unwrap(),record);
        let trace=peer.trace.clone();fixture.finish().await;peer.finish().await;
        assert_eq!(trace.ike.load(Ordering::SeqCst),1);assert_eq!(trace.registers.load(Ordering::SeqCst),1);assert_eq!(trace.deletes.load(Ordering::SeqCst),1);
        assert!(!state.go_socket_path.exists());
    }).await.expect("production bidirectional SMS deadline");
}
