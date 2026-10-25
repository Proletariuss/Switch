//! Actual serial actor -> shared production IKE/ESP/IMS -> SMS and EVS.
//! Only the SIM and carrier peers are synthetic; all network stays on loopback.
use super::{
    module_exchange::{active, call_state},
    sms_exchange::sms_http,
    *,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_serial_module_ike_sms_evs_and_confirmed_rf_restore() {
    if std::env::var_os("SWITCH_SERIAL_MODULE_509_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/serial-module-509-config-{}",
            crate::snapshot::new_instance_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4, custom_epdg: epdg.invalid}\n",
        )
        .unwrap();
        let result = tokio::task::spawn_blocking(move || std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_vowifi_session::service::production_integration::module_exchange_serial::public_serial_module_ike_sms_evs_and_confirmed_rf_restore", "--nocapture"])
            .env("SWITCH_SERIAL_MODULE_509_CHILD", "1").env("PROLETARIUS_VOWIFI_CONFIG", config)
            .output().unwrap()).await.unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    tokio::time::timeout(Duration::from_secs(60), scenario())
        .await
        .expect("serial production composition deadline");
}

async fn scenario() {
    use super::{
        call_peer::{Command, Event},
        sms_peer::Event as SmsEvent,
    };
    let fixture = Fixture::new();
    install_synthetic_idle_module_call_observer(&fixture).await;
    let serial = crate::native_serial_module_supervision_tests::Fixture::new(false).await;
    let mut server = fixture.sms_state();
    server.usb_at = serial.state.usb_at.clone();
    server.modem_actor = serial.state.modem_actor.clone();
    server.module_radio = serial.state.module_radio.clone();
    server.sms_inbox = serial.state.sms_inbox.clone();
    let subscription = native_device_sms_subscription_id(ICCID, IMSI).unwrap();
    let (peer, mut sms, mut calls) =
        ike_peer::Peer::with_module_business(fixture.root.clone(), subscription.clone()).await;
    crate::system_proxy::TEST_SCUTIL_OUTPUT
        .set(format!(
            "<dictionary> {{\n SOCKSEnable : 1\n SOCKSProxy : 127.0.0.1\n SOCKSPort : {}\n}}\n",
            peer.proxy.port()
        ))
        .unwrap();
    let (code, start) = sms_http(&server, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{start}");
    let online = active(&server).await;
    assert_eq!(online["sim_iccid"], ICCID);
    assert_eq!(online["cfun_mode"], 4);
    assert_eq!(
        serial.script.auth.load(Ordering::Acquire),
        2,
        "EAP and IMS must both authenticate through retained serial CSIM"
    );
    assert!(server.modem_actor.serial_module_held());
    let physical = server.modem_actor.native_serial_session().unwrap();
    assert!(
        server
            .modem_actor
            .refresh_native_serial_metadata(&physical)
            .await
            .is_err(),
        "periodic SIM probes must not invalidate registered serial identity"
    );
    let (code, status) = sms_http(&server, "GET", "/api/status", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{status}");
    assert_eq!(status["iccid"], ICCID);
    assert_eq!(status["imsi"], IMSI);
    assert_eq!(status["operating_mode"], 4);
    assert_eq!(sms.next().await, SmsEvent::IncomingAccepted);
    assert_eq!(sms.next().await, SmsEvent::IncomingRpAck);
    let body = json!({"client_request_id":"serial-509-outgoing","device":"dji","expected_subscription_id":subscription,"phone":sms_peer::RECIPIENT,"message":sms_peer::OUTGOING});
    let (code, prepared) = sms_http(&server, "POST", "/api/sms/send-intents", body.clone()).await;
    assert_eq!(code, StatusCode::OK, "{prepared}");
    let (code, sent) = sms_http(&server, "POST", "/api/sms/send", body).await;
    assert_eq!(code, StatusCode::OK, "{sent}");
    assert_eq!(sent["sent"], true);
    assert!(matches!(sms.next().await, SmsEvent::Submitted(_)));
    assert_eq!(sms.next().await, SmsEvent::SubmitRpAckAccepted);
    let rows = proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"))
        .load()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|v| v["subscription_id"] == subscription && v["device"] == "dji")
    );
    let generation = call_state(&server, "idle").await["generation"]
        .as_u64()
        .unwrap();
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
    assert_eq!(
        crate::module_esim::stop_module_vowifi_before_write(&server)
            .await
            .err()
            .unwrap()
            .code,
        "module_vowifi_call_active"
    );
    let (code, ended) = sms_http(
        &server,
        "POST",
        "/api/vowifi/calls/hangup",
        json!({"generation":generation}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{ended}");
    assert_eq!(calls.next().await, Event::Bye);
    assert_eq!(
        call_state(&server, "ended").await["call"]["media_ready"],
        false
    );
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    let (code, stopped) = sms_http(&server, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":false})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{stopped}");
    let service = server
        .native_vowifi_lifecycle
        .retained_module_service()
        .unwrap();
    service
        .stop_before(tokio::time::Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(peer.trace.deletes.load(Ordering::Acquire), 1);
    assert_eq!(peer.trace.registers.load(Ordering::Acquire), 1);
    assert_eq!(serial.script.cfun.load(Ordering::Acquire), 1);
    assert!(!server.modem_actor.serial_module_held());
    assert!(!server.module_radio.vowifi_owns_sim_channel());
    assert!(!service.owns_session());
    assert!(!server.go_socket_path.exists());
    let reservation = server.module_radio.reserve_esim().await.unwrap();
    server.module_radio.release_esim(reservation).await.unwrap();
    serial.finish().await;
    fixture.finish_idle_reader().await;
    peer.finish().await;
}
