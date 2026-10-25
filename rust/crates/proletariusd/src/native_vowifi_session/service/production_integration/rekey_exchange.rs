//! Real public HTTP start and SMS across consecutive peer CHILD replacements.
use super::sms_exchange::sms_http;
use super::sms_peer::{Command, Event, OUTGOING, RECIPIENT};
use super::*;
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_child_retirement_crossed_delete_sms_and_zero_auth_reacquisition() {
    if std::env::var_os("SWITCH_PRODUCTION_499_REKEY_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/production-499-rekey-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output=tokio::task::spawn_blocking(move||std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","native_vowifi_session::service::production_integration::rekey_exchange::peer_child_retirement_crossed_delete_sms_and_zero_auth_reacquisition","--nocapture"]).env("SWITCH_PRODUCTION_499_REKEY_CHILD","1").env("PROLETARIUS_VOWIFI_CONFIG",config).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(40),async {
        let fixture=Fixture::new();let state=fixture.sms_state();
        let subscription=native_device_sms_subscription_id(ICCID,IMSI).unwrap();
        let (peer,mut sms,rekey)=ike_peer::Peer::with_rekey_sms(fixture.root.clone(),subscription.clone()).await;
        let (code,body)=sms_http(&state,"POST","/api/reader/vowifi",json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true,"proxy":peer.proxy.to_string(),"epdg":"epdg.invalid","epdg_port":500})).await;
        assert_eq!(code,StatusCode::ACCEPTED,"{body}");let first=fixture.registered().await;
        let handle=fixture.service.record().unwrap().unwrap().driver.handle().unwrap();
        assert_eq!(sms.next().await,Event::IncomingAccepted);assert_eq!(sms.next().await,Event::IncomingRpAck);
        // Carrier omits DELETE entirely; daemon must retire that exact old SPI
        // after the data-plane grace without disturbing the protected TCP flow.
        rekey.child(false).await;
        let body=json!({"client_request_id":"production-499-outgoing","phone":RECIPIENT,"message":OUTGOING,"device":"reader","expected_subscription_id":subscription});
        let (code,reply)=sms_http(&state,"POST","/api/sms/send-intents",body.clone()).await;assert_eq!(code,StatusCode::OK,"{reply}");
        let (code,reply)=sms_http(&state,"POST","/api/sms/send",body).await;assert_eq!(code,StatusCode::OK,"{reply}");assert_eq!(reply["sent"],true);
        assert!(matches!(sms.next().await,Event::Submitted(_)));assert_eq!(sms.next().await,Event::SubmitRpAckAccepted);
        // A second genuine CREATE_CHILD_SA proves old control state no longer
        // blocks rekey. Cross the two authenticated old-SA DELETE exchanges.
        rekey.child(true).await;
        sms.commands.send(Command::Duplicate).await.unwrap();
        assert_eq!(sms.next().await,Event::IncomingAccepted);assert_eq!(sms.next().await,Event::IncomingRpAck);
        let rows=proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json")).load().unwrap();assert_eq!(rows.len(),2);
        assert_eq!(fixture.service.sms_binding().unwrap().generation,first.generation);
        assert_eq!(peer.trace.ike.load(Ordering::SeqCst),1);assert_eq!(peer.trace.deletes.load(Ordering::SeqCst),0);
        assert_eq!(fixture.card.auth.load(Ordering::SeqCst),2,"CHILD rekeys must not repeat SIM authentication");
        // Authenticated zero is immediate reauthentication, not 'absent'. A
        // new real IKE/EAP/REGISTER must follow actual old worker/lease cleanup.
        rekey.lifetime(0).await;
        tokio::time::timeout(Duration::from_secs(15),async {
            loop {if fixture.service.sms_binding().is_ok_and(|v|v.generation.value()>first.generation.value()){break;}tokio::time::sleep(Duration::from_millis(10)).await;}
        }).await.unwrap_or_else(|error|panic!("reacquire {error:?}: status={}, old={:?}, ike={}, registers={}, deletes={}, intent={:?}",fixture.service.status(),handle.snapshot(),peer.trace.ike.load(Ordering::SeqCst),peer.trace.registers.load(Ordering::SeqCst),peer.trace.deletes.load(Ordering::SeqCst),fixture.service.record().unwrap().map(|v|v.progress.borrow().clone())));
        assert!(handle.controls.is_closed());assert!(handle.snapshot().cleanup_confirmed);assert!(handle.snapshot().lease_released);
        assert_eq!(handle.snapshot().termination.unwrap().reason,ordinary::status::EndReason::PlannedReauthentication);
        assert!(fixture.service.resolve_sms_binding(&first.binding).is_err());
        assert_eq!(peer.trace.ike.load(Ordering::SeqCst),2);assert_eq!(peer.trace.registers.load(Ordering::SeqCst),2);assert_eq!(peer.trace.deletes.load(Ordering::SeqCst),1);
        assert_eq!(fixture.card.auth_finished.load(Ordering::SeqCst),4,"new EAP and IMS must really reacquire the card and complete AKA");
        peer.trace.stop_requested.store(true,Ordering::SeqCst);gateway::stop(&fixture).await;
        fixture.finish().await;peer.finish().await;
        call_survives_rekey().await;
    }).await.expect("real rekey and auth-lifetime recovery deadline");
}

async fn call_survives_rekey() {
    use super::call_exchange::{http, media, state};
    use super::call_peer::{Command, Event};
    let fixture = Fixture::new();
    install_synthetic_idle_module_call_observer(&fixture).await;
    let (peer, mut client, rekey) = ike_peer::Peer::with_rekey_calls().await;
    let (code, body) = http(
        &fixture,
        "POST",
        "/api/reader/vowifi",
        json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true,"proxy":peer.proxy.to_string(),"epdg":"epdg.invalid","epdg_port":500}),
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{body}");
    let binding = fixture.registered().await;
    let idle = state(&fixture, "idle").await;
    let generation = idle["generation"].as_u64().unwrap();
    let (code, body) = http(
        &fixture,
        "POST",
        "/api/reader/vowifi/calls/dial",
        json!({"generation":generation,"peer":call_peer::PEER}),
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{body}");
    assert_eq!(client.next().await, Event::Invite);
    client.commands.send(Command::AnswerOutgoing).await.unwrap();
    assert_eq!(client.next().await, Event::Ack);
    let active = state(&fixture, "active").await;
    media(&fixture, &mut client, &active, "EVS").await;
    rekey.child(false).await;
    let continued = state(&fixture, "active").await;
    assert_eq!(continued["generation"], generation);
    assert_eq!(
        continued["call"]["audio_token"],
        active["call"]["audio_token"]
    );
    media(&fixture, &mut client, &continued, "EVS").await;
    let (code, body) = http(
        &fixture,
        "POST",
        "/api/reader/vowifi/calls/hangup",
        json!({"generation":generation}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(client.next().await, Event::Bye);
    let ended = state(&fixture, "ended").await;
    assert_eq!(ended["call"]["media_ready"], false);
    assert!(ended["call"].get("audio_token").is_none());
    let port = active["call"]["audio_port"].as_u64().unwrap() as u16;
    let reclaimed = tokio::net::UdpSocket::bind(("127.0.0.1", port))
        .await
        .expect("actual PCM socket must retire before ended");
    drop(reclaimed);
    assert_eq!(
        fixture.service.sms_binding().unwrap().binding,
        binding.binding
    );
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    gateway::stop(&fixture).await;
    let rows = proletarius_store::ArchiveStore::new(fixture.root.join("calls.json"))
        .load()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["answered"], true);
    fixture.finish().await;
    peer.finish().await;
}
