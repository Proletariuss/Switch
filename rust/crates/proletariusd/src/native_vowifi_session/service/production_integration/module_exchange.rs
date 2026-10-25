//! Public module HTTP -> retained USB SIM/RF -> real IKE/IMS, SMS and EVS.
use super::{sms_exchange::sms_http, *};
use crate::module_vowifi_card::tests::{Block, Trace};
use serde_json::Value;

pub(super) async fn call_state(server: &crate::ServerState, wanted: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let (code, value) =
                sms_http(server, "GET", "/api/vowifi/calls/status", json!({})).await;
            assert_eq!(code, StatusCode::OK, "{value}");
            if value["call"]["state"] == wanted {
                return value;
            }
            assert_ne!(value["call"]["state"], "failed", "{value}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("module public call status")
}
pub(super) async fn active(server: &crate::ServerState) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (code, value) = sms_http(server, "GET", "/api/vowifi", json!({})).await;
            assert_eq!(code, StatusCode::OK, "{value}");
            assert_ne!(value["state"], "failed", "{value}");
            if value["sms_ready"] == true && value["call_ready"] == true {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("module production registration")
}
async fn stopped(server: &crate::ServerState, trace: &Trace) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let (code, value) = sms_http(server, "GET", "/api/vowifi/result", json!({})).await;
            assert_eq!(code, StatusCode::OK, "{value}");
            if value["done"] == true {
                assert!(value.get("error").is_none(), "{value}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("module actual network join and RF restoration");
    assert_eq!(trace.mode.load(Ordering::Acquire), 1);
    assert!(!server.module_radio.vowifi_owns_sim_channel());
    assert!(
        !server
            .native_vowifi_lifecycle
            .retained_module_service()
            .unwrap()
            .owns_session()
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_module_usb_ike_sms_evs_and_confirmed_rf_restore() {
    if std::env::var_os("SWITCH_MODULE_500_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/module-500-config-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4, custom_epdg: epdg.invalid}\n",
        )
        .unwrap();
        let result=tokio::task::spawn_blocking(move||std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","native_vowifi_session::service::production_integration::module_exchange::public_module_usb_ike_sms_evs_and_confirmed_rf_restore","--nocapture"]).env("SWITCH_MODULE_500_CHILD","1").env("PROLETARIUS_VOWIFI_CONFIG",config).output().unwrap()).await.unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(45), scenario())
        .await
        .expect("module public composition deadline");
}
async fn scenario() {
    use super::{
        call_peer::{Command, Event},
        sms_peer::{Command as SmsCommand, Event as SmsEvent},
    };
    let fixture = Fixture::new();
    let mut server = fixture.sms_state();
    let (usb, radio, trace) =
        crate::module_vowifi_card::tests::service_fixture(fixture.root.clone()).await;
    server.usb_at = usb.clone();
    server.module_radio = radio.clone();
    confirm_idle_module_call_observer(&server, radio.clone()).await;
    let subscription = native_device_sms_subscription_id(ICCID, IMSI).unwrap();
    let (peer, mut sms, mut calls) =
        ike_peer::Peer::with_module_business(fixture.root.clone(), subscription.clone()).await;
    crate::system_proxy::TEST_SCUTIL_OUTPUT
        .set(format!(
            "<dictionary> {{\n SOCKSEnable : 1\n SOCKSProxy : 127.0.0.1\n SOCKSPort : {}\n}}\n",
            peer.proxy.port()
        ))
        .unwrap();
    let (code, before) = sms_http(&server, "GET", "/api/vowifi", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{before}");
    assert_eq!(before["ims_registered"], false);
    let (code, start) = sms_http(&server, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{start}");
    let online = active(&server).await;
    assert_eq!(online["cfun_mode"], 4);
    assert_eq!(online["sim_iccid"], ICCID);
    assert_eq!(trace.auth.load(Ordering::Acquire), 2);
    // Enabling the diagnostic owner only changes retained state: this must
    // not open QMI USB or disturb the module's active IMS session.
    server.qmi_enhancement.enable().unwrap();
    let (code, sessions) = sms_http(&server, "GET", "/api/reader/vowifi/sessions", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{sessions}");
    let rows = sessions["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{sessions}");
    assert_eq!(rows[0]["sim_id"], ICCID);
    assert_eq!(rows[0]["imsi"], IMSI);
    assert_eq!(rows[0]["ims_state"], "registered");
    assert_eq!(rows[0]["sms_ready"], true);
    assert_ne!(
        server.availability.snapshot().phase,
        crate::BackendPhase::Ready
    );
    let commands = trace.commands.lock().unwrap().len();
    let (code, device) = sms_http(&server, "GET", "/api/status", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{device}");
    assert_eq!(device["qmi"]["enabled"], true);
    assert_eq!(device["qmi"]["session_initialized"], false);
    let typed: proletarius_api_contract::UpstreamDeviceStatus =
        serde_json::from_value(device.clone()).expect("complete native status DTO");
    assert_eq!(typed.qmi.as_ref().map(|qmi| qmi.enabled), Some(true));
    assert_eq!(typed.iccid.as_deref(), Some(ICCID));
    assert_eq!(typed.imsi.as_deref(), Some(IMSI));
    assert_eq!(typed.phone_number.as_deref(), Some("+447700900123"));
    assert_eq!(typed.operating_mode, Some(4));
    assert_eq!(typed.sim_inserted, Some(true));
    let (code, devices) = sms_http(&server, "GET", "/api/sms/devices", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{devices}");
    assert_eq!(devices["devices"][0]["id"], "dji");
    assert_eq!(devices["devices"][0]["available"], true);
    assert_eq!(devices["devices"][0]["ready"], true);
    assert_eq!(devices["devices"][0]["phone"], "+447700900123");
    assert_eq!(devices["devices"][0]["subscription_id"], subscription);
    assert_eq!(
        trace.commands.lock().unwrap().len(),
        commands,
        "native dashboard capture must be cache-only while IMS owns USB"
    );
    assert_eq!(sms.next().await, SmsEvent::IncomingAccepted);
    assert_eq!(sms.next().await, SmsEvent::IncomingRpAck);
    let body = json!({"client_request_id":"module-500-outgoing","device":"dji","expected_subscription_id":subscription,"phone":sms_peer::RECIPIENT,"message":sms_peer::OUTGOING});
    let (code, prepared) = sms_http(&server, "POST", "/api/sms/send-intents", body.clone()).await;
    assert_eq!(code, StatusCode::OK, "{prepared}");
    let (code, sent) = sms_http(&server, "POST", "/api/sms/send", body.clone()).await;
    assert_eq!(code, StatusCode::OK, "{sent}");
    assert_eq!(sent["sent"], true);
    assert!(matches!(sms.next().await, SmsEvent::Submitted(_)));
    assert_eq!(sms.next().await, SmsEvent::SubmitRpAckAccepted);
    let receipt = server
        .device_send_store
        .apply("get", json!({"client_request_id":"module-500-outgoing"}))
        .unwrap();
    assert_eq!(receipt["state"], "accepted");
    assert_eq!(receipt["archive_confirmed"], true);
    assert!(
        receipt["binding"]
            .as_str()
            .unwrap()
            .starts_with("rust_module_ims:")
    );
    let binding = server
        .native_vowifi_lifecycle
        .retained_module_service()
        .unwrap()
        .sms_binding()
        .unwrap();
    let (reply, result) = tokio::sync::oneshot::channel();
    binding
        .controls
        .send(crate::native_registered_session::Control::SmsStatus {
            generation: binding.generation,
            batch_id: format!("device_sms_{}", receipt["attempt"].as_str().unwrap()),
            reply,
        })
        .await
        .unwrap();
    let accepted = result.await.unwrap().unwrap().unwrap();
    assert_eq!(
        accepted.phase,
        crate::native_sms_originating::Phase::Accepted
    );
    assert_eq!(accepted.generation, binding.generation);
    assert_eq!(accepted.accepted_parts, 1);
    assert_eq!(
        accepted.rp_results,
        vec![(1, crate::native_sms_originating::RpResult::Ack)]
    );
    let (code, repeat) = sms_http(&server, "POST", "/api/sms/send", body).await;
    assert_eq!(code, StatusCode::OK, "{repeat}");
    assert_eq!(repeat["sent"], true);
    sms.commands.send(SmsCommand::Duplicate).await.unwrap();
    assert_eq!(sms.next().await, SmsEvent::IncomingAccepted);
    assert_eq!(sms.next().await, SmsEvent::IncomingRpAck);
    let rows = proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"))
        .load()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|v| v["subscription_id"] == subscription));
    assert!(rows.iter().all(|v| v["device"] == "dji"), "{rows:?}");
    let idle = call_state(&server, "idle").await;
    let generation = idle["generation"].as_u64().unwrap();
    let (code, dial) = sms_http(
        &server,
        "POST",
        "/api/vowifi/calls/dial",
        json!({"generation":generation,"peer":call_peer::PEER}),
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{dial}");
    assert_eq!(calls.next().await, Event::Invite);
    calls.commands.send(Command::AnswerOutgoing).await.unwrap();
    assert_eq!(calls.next().await, Event::Ack);
    let talking = call_state(&server, "active").await;
    call_exchange::media_on(
        &server,
        "/api/vowifi/calls/status",
        &mut calls,
        &talking,
        "EVS",
    )
    .await;
    let denied = crate::module_esim::stop_module_vowifi_before_write(&server)
        .await
        .err()
        .expect("active voice must reject eSIM mutation");
    assert_eq!(denied.code, "module_vowifi_call_active");
    assert_eq!(trace.mode.load(Ordering::Acquire), 4);
    assert_eq!(
        call_state(&server, "active").await["generation"],
        generation
    );
    let (code, hangup) = sms_http(
        &server,
        "POST",
        "/api/vowifi/calls/hangup",
        json!({"generation":generation}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{hangup}");
    assert_eq!(calls.next().await, Event::Bye);
    let ended = call_state(&server, "ended").await;
    assert_eq!(ended["call"]["media_ready"], false);
    assert!(ended["call"].get("audio_token").is_none());
    let port = talking["call"]["audio_port"].as_u64().unwrap() as u16;
    drop(
        tokio::net::UdpSocket::bind(("127.0.0.1", port))
            .await
            .expect("exact PCM socket released"),
    );
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    let (code, stop) = sms_http(&server, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":false})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{stop}");
    stopped(&server, &trace).await;
    assert_eq!(peer.trace.deletes.load(Ordering::Acquire), 1);
    assert_eq!(peer.trace.registers.load(Ordering::Acquire), 1);
    let history = proletarius_store::ArchiveStore::new(fixture.root.join("calls.json"))
        .load()
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["answered"], true);
    assert_eq!(history[0]["device"], "module", "{history:?}");
    assert!(!server.go_socket_path.exists());
    cancellation(&server, &trace).await;
    let (code, start) = sms_http(&server, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{start}");
    active(&server).await;
    crate::module_esim::stop_module_vowifi_before_write(&server)
        .await
        .map_err(|e| e.code)
        .unwrap();
    stopped(&server, &trace).await;
    assert_eq!(peer.trace.deletes.load(Ordering::Acquire), 2);
    let reservation = radio
        .reserve_esim()
        .await
        .expect("actual RF restoration permits the next eSIM operation");
    radio.release_esim(reservation).await.unwrap();
    transport_drain(&server, &trace, &peer).await;
    radio.finish_shutdown().await.unwrap();
    usb.shutdown().await.unwrap();
    fixture.finish_idle_reader().await;
    peer.finish().await;
}
async fn cancellation(server: &crate::ServerState, trace: &Trace) {
    let auth_before = trace.auth.load(Ordering::Acquire);
    let block = Arc::new(Block::default());
    *trace.blocked.lock().unwrap() = Some(("AT+CNUM".into(), block.clone()));
    let (code, reply) = sms_http(server, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{reply}");
    tokio::time::timeout(Duration::from_secs(5), block.entered.notified())
        .await
        .unwrap();
    let (code, reply) = sms_http(server, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":false})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{reply}");
    let (_, pending) = sms_http(server, "GET", "/api/vowifi/result", json!({})).await;
    assert_eq!(
        pending["done"], false,
        "blocked actual USB command must retain operation"
    );
    assert!(server.module_radio.vowifi_owns_sim_channel());
    block.release();
    stopped(server, trace).await;
    assert_eq!(
        trace.auth.load(Ordering::Acquire),
        auth_before,
        "cancelled preparation must not start a fresh network AKA"
    );
}

async fn transport_drain(server: &crate::ServerState, trace: &Trace, peer: &ike_peer::Peer) {
    let (code, start) = sms_http(server, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{start}");
    active(server).await;
    let restore = Arc::new(Block::default());
    *trace.blocked.lock().unwrap() = Some(("AT+CFUN=1".into(), restore.clone()));
    let state = server.clone();
    let drain = tokio::spawn(async move { pause_and_drain_module(&state).await });
    tokio::time::timeout(Duration::from_secs(8), restore.entered.notified())
        .await
        .expect("real restore command entered");
    assert_eq!(
        peer.trace.deletes.load(Ordering::Acquire),
        3,
        "network DELETE must finish before physical RF restoration"
    );
    assert!(
        !drain.is_finished(),
        "Native transport retirement cannot release a submitted USB command"
    );
    assert!(server.module_radio.vowifi_owns_sim_channel());
    assert_eq!(trace.mode.load(Ordering::Acquire), 4);
    paused_enable(server).await;
    restore.release();
    drain.await.unwrap().unwrap();
    paused_enable(server).await;
    stopped(server, trace).await;
    assert_eq!(trace.auth.load(Ordering::Acquire), 6);
    assert_eq!(peer.trace.ike.load(Ordering::Acquire), 3);
    assert_eq!(peer.trace.registers.load(Ordering::Acquire), 3);
    server
        .native_vowifi_lifecycle
        .retained_module_service()
        .unwrap()
        .resume_transport();
    cancellation(server, trace).await;
    assert_eq!(trace.auth.load(Ordering::Acquire), 6);
    assert_eq!(peer.trace.ike.load(Ordering::Acquire), 3);
}

async fn paused_enable(server: &crate::ServerState) {
    let (code, reply) = sms_http(server, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true})).await;
    assert_eq!(code, StatusCode::CONFLICT, "{reply}");
    assert_eq!(
        reply["code"], "module_vowifi_transport_recovering",
        "{reply}"
    );
}

#[tokio::test]
async fn transport_drain_initializes_and_pauses_unstarted_module_before_public_enable() {
    let fixture = Fixture::new();
    let state = fixture.sms_state();
    assert!(
        state
            .native_vowifi_lifecycle
            .retained_module_service()
            .is_none()
    );
    pause_and_drain_module(&state).await.unwrap();
    assert!(
        state
            .native_vowifi_lifecycle
            .retained_module_service()
            .is_some()
    );
    paused_enable(&state).await;
    assert!(
        !state
            .native_vowifi_lifecycle
            .ordinary_admission()
            .has_ordinary_claim()
    );
    fixture.finish_idle_reader().await;
}

// Preserve the lifecycle pause/drain race regression after removal of the
// compatibility process. The session still must join network and RF workers.
async fn pause_and_drain_module(
    state: &crate::ServerState,
) -> Result<(), proletarius_domain::BackendFailure> {
    let service = state
        .native_vowifi_lifecycle
        .module_service(state)
        .map_err(proletarius_domain::BackendFailure::Io)?;
    service.pause_transport();
    crate::wait_for_native_cleanup(
        || service.stop_before(tokio::time::Instant::now() + Duration::from_secs(30)),
        Duration::from_secs(1),
    )
    .await
}
