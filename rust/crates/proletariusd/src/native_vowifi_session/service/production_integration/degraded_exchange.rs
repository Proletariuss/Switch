//! Failed optional history and absent originating routes must not stop actual
//! IKE/REGISTER or incoming SMS. No registration/readiness outcome is injected.
use super::sms_peer::Event;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_registration_and_sms_survive_history_failure_and_missing_send_route() {
    if std::env::var_os("SWITCH_PRODUCTION_492_DEGRADED_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/production-492-degraded-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier-overrides.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "native_vowifi_session::service::production_integration::degraded_exchange::real_registration_and_sms_survive_history_failure_and_missing_send_route", "--nocapture"])
                .env("SWITCH_PRODUCTION_492_DEGRADED_CHILD", "1")
                .env("PROLETARIUS_VOWIFI_CONFIG", config).output().unwrap()
        }).await.unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for missing_route in [false, true] {
        tokio::time::timeout(Duration::from_secs(30), async {
            let fixture = if missing_route { Fixture::new_with_routes("", "") } else { Fixture::new() };
            let state = fixture.sms_state();
            let calls = fixture.service.0.resources.calls.clone();
            // An old history worker exists; this is not permission to retire it.
            let old_history = crate::call_facts::NativeSink::start_reader_ims(calls.clone(), "retained-history-owner".into(), 8).unwrap();
            let subscription = native_device_sms_subscription_id(ICCID, IMSI).unwrap();
            let (peer, mut client) = ike_peer::Peer::with_sms(fixture.root.clone(), subscription.clone()).await;
            let response = gateway::request(&fixture, Request::builder().method("POST").uri("/api/reader/vowifi")
                .body(Full::new(Bytes::from(serde_json::to_vec(&json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true,"proxy":peer.proxy.to_string(),"epdg":"epdg.invalid","epdg_port":500})).unwrap()))).unwrap()).await;
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            tokio::time::timeout(Duration::from_secs(12), async {
                loop {
                    let status = fixture.service.status();
                    assert_ne!(status["tunnel_state"], "error", "{status}");
                    if status["call_ready"] == true { break; }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.unwrap();
            let status = fixture.service.status();
            assert_eq!(status["tunnel_state"], "connected");
            assert_eq!(status["ims_state"], "registered");
            assert_eq!(status["sms_ready"], !missing_route);
            assert_eq!(status["progress_step"], if missing_route {4} else {5});
            assert_eq!(status["call_ready"], true);
            assert!(calls.history_error().is_some(), "history degradation must be visible");
            assert_eq!(client.next().await, Event::IncomingAccepted);
            assert_eq!(client.next().await, Event::IncomingRpAck);
            let archive = proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json")).load().unwrap();
            assert_eq!(archive.len(), 1);
            assert_eq!(archive[0]["direction"], "in");
            if missing_route {
                assert!(fixture.service.sms_binding().is_err());
            } else {
                let body = json!({"client_request_id":"production-492-degraded-outgoing", "phone":sms_peer::RECIPIENT,"message":sms_peer::OUTGOING,"device":"reader","expected_subscription_id":subscription});
                let (code, prepared) = sms_exchange::sms_http(&state, "POST", "/api/sms/send-intents", body.clone()).await;
                assert_eq!(code, StatusCode::OK, "{prepared}");
                let (code, sent) = sms_exchange::sms_http(&state, "POST", "/api/sms/send", body).await;
                assert_eq!(code, StatusCode::OK, "{sent}");
                assert_eq!(sent["sent"], true);
                assert!(matches!(client.next().await, Event::Submitted(_)));
                assert_eq!(client.next().await, Event::SubmitRpAckAccepted);
            }
            peer.trace.stop_requested.store(true, Ordering::SeqCst);
            gateway::stop(&fixture).await;
            let record = fixture.service.record().unwrap().unwrap();
            {
                let current = record.driver.current.lock().unwrap();
                let result = current.result.as_ref().unwrap();
                let ordinary::Outcome::Running(Ok(exit)) = &result.outcome else {panic!("actual worker did not exit")};
                assert!(exit.call_archive.is_err(), "missing history was not a successful archive");
                assert!(exit.call_archive_retired, "this generation created no history worker");
                assert!(result.lease_release.as_ref().is_some_and(Result::is_ok));
            }
            assert!(!old_history.worker_retired(), "new generation retired unrelated history");
            fixture.finish().await;
            peer.finish().await;
            old_history.retire(time::OffsetDateTime::now_utc()).unwrap();
            old_history.drain().await.unwrap();
            assert!(old_history.worker_retired());
        }).await.unwrap();
    }
}
