//! In-place timed renewal with a pending RP report, then three real generations via
//! remote protected-TCP EOF, then recovered public SMS submission.
use super::renewal_peer::{Command, Event};
use super::*;
use crate::native_registered_session::Control;

async fn wait_for(mut predicate: impl FnMut() -> bool, seconds: u64, label: &str) {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("deadline waiting for {label}"));
}
async fn assert_retired(
    fixture: &Fixture,
    handle: &ordinary::Handle,
    expected: ordinary::status::EndReason,
    deletes: usize,
    trace: &ike_peer::Trace,
) {
    wait_for(
        || {
            fixture.service.record().unwrap().is_some_and(|record| {
                let state = record.progress.borrow();
                state.phase == intent::Phase::Backoff && state.cleanup_confirmed
            })
        },
        5,
        "actual cleanup and intent backoff",
    )
    .await;
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.phase, ordinary::Phase::Released);
    assert!(snapshot.cleanup_confirmed && snapshot.lease_released);
    assert_eq!(snapshot.termination.unwrap().reason, expected);
    assert!(
        !fixture
            .service
            .0
            .resources
            .gate
            .native_reader_session_active()
    );
    assert!(fixture.service.sms_binding().is_err());
    assert!(
        handle.controls.is_closed(),
        "old consumer must actually finish before reacquisition"
    );
    assert_eq!(
        trace.deletes.load(Ordering::SeqCst),
        deletes,
        "each retired generation must send authenticated IKE DELETE"
    );
    let record = fixture.service.record().unwrap().unwrap();
    let current = record.driver.current.lock().unwrap();
    let result = current
        .result
        .as_ref()
        .expect("actual ordinary result must be retained and joined");
    assert!(result.lease_release.as_ref().is_some_and(Result::is_ok));
    let ordinary::Outcome::Running(Ok(exit)) = &result.outcome else {
        panic!("missing registered worker exit")
    };
    assert!(exit.clean());
    assert!(exit.local_retired);
    let consumer = exit.consumer.as_ref().unwrap();
    if expected == ordinary::status::EndReason::RemoteFailure {
        assert_eq!(
            exit.actor.as_ref().unwrap().reason,
            crate::native_ims_actor::StopReason::StreamClosed,
            "the second generation must observe actual signaling EOF, not a read error"
        );
    }
    if deletes == 1 {
        use crate::{native_registered_session::Notice, native_sms_consumer::SessionOutcome};
        use proletarius_vowifi::sms_transaction::DeliveryReportState;
        let submitted: Vec<_> = result
            .notices
            .iter()
            .filter_map(|notice| match notice {
                Notice::Sms(SessionOutcome::ReportSubmitted(ticket)) => Some(*ticket),
                _ => None,
            })
            .collect();
        assert_eq!(
            submitted.len(),
            1,
            "the held wire RP-ACK must have exactly one real consumer ticket"
        );
        // Renewal may finish immediately after consuming the final response.
        // The actual consumer then returns its last notice in the joined Exit
        // rather than publishing it to the ordinary diagnostic channel.
        let finished: Vec<_> = result
            .notices
            .iter()
            .chain(consumer.undelivered_notice.iter())
            .filter_map(|notice| match notice {
                Notice::Sms(SessionOutcome::ReportFinished {
                    ticket,
                    state,
                    renewal_reports_drained,
                }) => Some((*ticket, *state, *renewal_reports_drained)),
                _ => None,
            })
            .collect();
        assert_eq!(
            finished,
            vec![(submitted[0], DeliveryReportState::Accepted, false)],
            "renewal must drain the held ticket through its actual correlated accepted response, not timeout or rejection"
        );
        assert!(
            !result.notices.iter().any(|notice| matches!(
                notice,
                Notice::Sms(SessionOutcome::RegistrationRenewalDue { .. })
            )),
            "in-place REGISTER must not request generation replacement"
        );
    }
    assert!(
        consumer.pending_reports.is_empty(),
        "received report final response must drain before release"
    );
    assert!(consumer.timer_failures.is_empty());
    assert!(consumer.media_cleanup.is_ok());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_renewal_drains_report_then_remote_loss_reacquires_same_card_and_sends_sms() {
    if std::env::var_os("SWITCH_PRODUCTION_492_RENEWAL_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/production-492-renewal-config-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier-overrides.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output = tokio::task::spawn_blocking(move || std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_vowifi_session::service::production_integration::renewal_exchange::actual_renewal_drains_report_then_remote_loss_reacquires_same_card_and_sends_sms", "--nocapture"])
            .env("SWITCH_PRODUCTION_492_RENEWAL_CHILD", "1").env("PROLETARIUS_VOWIFI_CONFIG", config).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "renewal child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(80), async {
        let fixture = Fixture::new();
        let state = fixture.sms_state();
        let expected = fixture.service.0.resources.identity.capture().unwrap();
        let subscription = native_device_sms_subscription_id(ICCID, IMSI).unwrap();
        let (server, mut client) = renewal_peer::new(fixture.root.clone(), subscription.clone());
        let peer = ike_peer::Peer::with_renewal(server).await;
        let response = gateway::request(&fixture, Request::builder().method("POST").uri("/api/reader/vowifi")
            .body(Full::new(Bytes::from(serde_json::to_vec(&json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true,"proxy":peer.proxy.to_string(),"epdg":"epdg.invalid","epdg_port":500})).unwrap()))).unwrap()).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(client.next().await, Event::Registered(1));
        let first = fixture.registered().await;
        let first_handle = fixture.service.record().unwrap().unwrap().driver.handle().unwrap();
        let intent_id = fixture.service.record().unwrap().unwrap().id;
        assert_eq!(first.generation.value(), 1);
        // Carrier grants 90s. The unmodified production scheduler renews at
        // 30s; introduce a report just before that real timer, without injecting
        // an event or changing the actor's clock/readiness.
        tokio::time::sleep(Duration::from_secs(27)).await;
        client.command(1, Command::DeliverReport).await;
        assert_eq!(client.next().await, Event::IncomingAccepted);
        assert_eq!(client.next().await, Event::ReportHeld);
        assert_eq!(client.next().await,Event::Refreshed(1));
        assert!(fixture.service.resolve_sms_binding(&first.binding).is_ok(), "valid refreshed association retains SMS admission");
        assert_eq!(peer.trace.ike.load(Ordering::SeqCst), 1);
        assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 0);
        assert!(fixture.service.0.resources.gate.native_reader_session_active());
        assert_eq!(first_handle.snapshot().phase, ordinary::Phase::Registered);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 0, "pending RP report must delay replacement");
        client.command(1, Command::ReleaseReport).await;
        assert_eq!(client.next().await, Event::ReportReleased);
        tokio::time::sleep(Duration::from_millis(150)).await;
        client.command(1,Command::CloseSignaling).await;
        assert_eq!(client.next().await, Event::SignalingClosed(1));
        assert_retired(&fixture, &first_handle, ordinary::status::EndReason::RemoteFailure, 1, &peer.trace).await;
        assert_eq!(client.next().await, Event::Registered(2));
        let second = fixture.registered().await;
        let second_handle = fixture.service.record().unwrap().unwrap().driver.handle().unwrap();
        assert_eq!(second.generation.value(), 2);
        assert_ne!(first.binding, second.binding);
        assert_eq!(second.subscription_id, first.subscription_id);
        assert_eq!(fixture.service.0.resources.identity.capture().unwrap(), expected);
        assert_eq!(fixture.service.record().unwrap().unwrap().id, intent_id);
        assert_eq!(fixture.card.auth.load(Ordering::SeqCst), 4);
        assert!(fixture.service.resolve_sms_binding(&first.binding).is_err());
        client.command(2, Command::CloseSignaling).await;
        assert_eq!(client.next().await, Event::SignalingClosed(2));
        assert_retired(&fixture, &second_handle, ordinary::status::EndReason::RemoteFailure, 2, &peer.trace).await;
        assert_eq!(client.next().await, Event::Registered(3));
        let third = fixture.registered().await;
        assert_eq!(third.generation.value(), 3);
        assert_eq!(fixture.service.0.resources.identity.capture().unwrap(), expected);
        assert_eq!(fixture.service.record().unwrap().unwrap().id, intent_id);
        assert_eq!(fixture.card.auth.load(Ordering::SeqCst), 6);
        assert!(fixture.service.reader_observation().unwrap().sms_ready);
        for stale in [&first, &second] {
            assert!(fixture.service.resolve_sms_binding(&stale.binding).is_err());
            let (reply, received) = tokio::sync::oneshot::channel();
            third.controls.send(Control::SmsStatus { generation: stale.generation, batch_id: "stale-generation".into(), reply }).await.unwrap();
            assert_eq!(received.await.unwrap().unwrap_err().code, "native_session_stale_generation");
        }
        let body = json!({"client_request_id":"production-492-after-recovery","phone":sms_peer::RECIPIENT,"message":sms_peer::OUTGOING,"device":"reader","expected_subscription_id":subscription});
        let (status, prepared) = sms_exchange::sms_http(&state, "POST", "/api/sms/send-intents", body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{prepared}");
        let (status, sent) = sms_exchange::sms_http(&state, "POST", "/api/sms/send", body).await;
        assert_eq!(status, StatusCode::OK, "{sent}"); assert_eq!(sent["sent"], true);
        assert!(matches!(client.next().await, Event::Submitted(_)));
        assert_eq!(client.next().await, Event::SubmitRpAckAccepted);
        let stored = state.device_send_store.apply("get", json!({"client_request_id":"production-492-after-recovery"})).unwrap();
        assert_eq!(stored["state"], "accepted"); assert_eq!(stored["archive_confirmed"], true);
        assert_eq!(stored["subscription_id"], subscription);
        let (reply, received) = tokio::sync::oneshot::channel();
        third.controls.send(Control::SmsStatus { generation: third.generation, batch_id: format!("device_sms_{}", stored["attempt"].as_str().unwrap()), reply }).await.unwrap();
        let receipt = received.await.unwrap().unwrap().unwrap();
        assert_eq!(receipt.generation, third.generation);
        assert_eq!(receipt.rp_results, vec![(1, crate::native_sms_originating::RpResult::Ack)]);
        let rows = proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json")).load().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(fixture.card.closed.load(Ordering::SeqCst), 0, "rebuild must reuse the selected physical card without closing/resetting it");
        peer.trace.stop_requested.store(true, Ordering::SeqCst);
        gateway::stop(&fixture).await;
        assert!(fixture.service.resolve_sms_binding(&third.binding).is_err());
        assert_eq!(client.next().await, Event::SignalingClosed(3));
        let trace = peer.trace.clone(); fixture.finish().await; peer.finish().await;
        assert_eq!(trace.ike.load(Ordering::SeqCst), 3);
        assert_eq!(trace.registers.load(Ordering::SeqCst), 3);
        assert_eq!(trace.deletes.load(Ordering::SeqCst), 3);
    }).await.expect("production multi-generation recovery deadline");
}
