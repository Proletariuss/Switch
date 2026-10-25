//! Real Service + IKE + protected REGISTER + active EVS/RTP/PCM renewal.
use super::call_exchange::{http, media, state};
use super::call_peer::{Command, Event, RefreshMode};
use super::*;
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn active_call_register_refresh_preserves_media_and_bounds_failures() {
    if std::env::var_os("SWITCH_PRODUCTION_495_REFRESH_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/production-495-refresh-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier-overrides.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output=tokio::task::spawn_blocking(move||std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","native_vowifi_session::service::production_integration::refresh_exchange::active_call_register_refresh_preserves_media_and_bounds_failures","--nocapture"]).env("SWITCH_PRODUCTION_495_REFRESH_CHILD","1").env("PROLETARIUS_VOWIFI_CONFIG",config).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "refresh child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for mode in [
        RefreshMode::Accepted,
        RefreshMode::Challenge401,
        RefreshMode::Challenge407,
        RefreshMode::ChallengeRejected,
        RefreshMode::Rejected,
        RefreshMode::NewSecurity,
        RefreshMode::Expire,
        RefreshMode::StopPending,
        RefreshMode::StopAka,
    ] {
        tokio::time::timeout(Duration::from_secs(18), scenario(mode))
            .await
            .unwrap_or_else(|_| panic!("refresh scenario deadline: {mode:?}"));
    }
}
async fn scenario(mode: RefreshMode) {
    let fixture = Fixture::new();
    install_synthetic_idle_module_call_observer(&fixture).await;
    let (peer, mut client) = ike_peer::Peer::with_refresh_calls(mode).await;
    let (code, body) = http(
        &fixture,
        "POST",
        "/api/reader/vowifi",
        json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true,"proxy":peer.proxy.to_string(),"epdg":"epdg.invalid","epdg_port":500}),
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{body}");
    let binding = fixture.registered().await;
    let held_auth = (mode == RefreshMode::StopAka).then(|| fixture.card.hold_next_auth());
    fixture.card.auth_delay_ms.store(400, Ordering::SeqCst);
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
    assert_eq!(client.next().await, Event::RefreshRequest);
    let success = matches!(
        mode,
        RefreshMode::Accepted | RefreshMode::Challenge401 | RefreshMode::Challenge407
    );
    if success {
        if mode != RefreshMode::Accepted {
            assert_eq!(client.next().await, Event::ProbeDuringAka);
            assert_eq!(client.next().await, Event::RefreshRequest);
        }
        assert_eq!(client.next().await, Event::Refreshed);
        // Cross the ORIGINAL actual expiry. A flag-only refresh, mismatched
        // grant, duplicate revoked grant or session rebuild cannot pass this.
        tokio::time::sleep(Duration::from_millis(1400)).await;
        let continued = state(&fixture, "active").await;
        for field in ["call_id", "audio_port", "audio_token"] {
            assert_eq!(
                active["call"][field], continued["call"][field],
                "preserve {field}"
            );
        }
        assert_eq!(continued["generation"], generation);
        media(&fixture, &mut client, &continued, "EVS").await;
        if mode == RefreshMode::Accepted {
            assert_eq!(client.next().await, Event::RefreshRequest);
            assert_eq!(client.next().await, Event::Refreshed);
            let renewed_again = state(&fixture, "active").await;
            assert_eq!(
                renewed_again["call"]["audio_token"],
                active["call"]["audio_token"]
            );
            media(&fixture, &mut client, &renewed_again, "EVS").await;
        }
        assert_eq!(
            fixture.service.sms_binding().unwrap().binding,
            binding.binding
        );
        assert_eq!(peer.trace.ike.load(Ordering::SeqCst), 1);
        assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 0);
        assert_eq!(
            fixture.card.auth.load(Ordering::SeqCst),
            if mode == RefreshMode::Accepted { 2 } else { 3 }
        );
        let (code, body) = http(
            &fixture,
            "POST",
            "/api/reader/vowifi/calls/hangup",
            json!({"generation":generation}),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert_eq!(client.next().await, Event::Bye);
    } else if mode == RefreshMode::StopAka {
        held_auth.as_ref().unwrap().entered().await;
        assert_eq!(fixture.card.auth.load(Ordering::SeqCst), 3);
        assert_eq!(fixture.card.auth_finished.load(Ordering::SeqCst), 2);
        client
            .commands
            .send(Command::ProbeDuringHeldAka)
            .await
            .unwrap();
        assert_eq!(client.next().await, Event::ProbeDuringAka);
        assert_eq!(fixture.card.auth.load(Ordering::SeqCst), 3);
        assert_eq!(
            fixture.card.auth_finished.load(Ordering::SeqCst),
            2,
            "probe must be processed while physical AKA remains in flight"
        );
    } else if mode != RefreshMode::StopPending {
        if mode == RefreshMode::ChallengeRejected {
            assert_eq!(client.next().await, Event::ProbeDuringAka);
            assert_eq!(client.next().await, Event::RefreshRequest);
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if fixture.service.record().unwrap().is_some_and(|r| {
                    let p = r.progress.borrow();
                    p.phase == intent::Phase::Backoff && p.cleanup_confirmed
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("failure must retire active media, SIP, IKE and exact lease before retry");
        assert!(fixture.service.sms_binding().is_err());
        assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 1);
        let record = fixture.service.record().unwrap().unwrap();
        let current = record.driver.current.lock().unwrap();
        let result = current.result.as_ref().unwrap();
        assert!(result.lease_release.as_ref().unwrap().is_ok());
        let ordinary::Outcome::Running(Ok(exit)) = &result.outcome else {
            panic!("missing actual joined actor")
        };
        assert!(exit.clean());
        assert!(exit.consumer.as_ref().unwrap().media_cleanup.is_ok());
        assert_eq!(
            fixture.card.auth.load(Ordering::SeqCst),
            if mode == RefreshMode::ChallengeRejected {
                3
            } else {
                2
            },
            "only actual challenge may issue AKA"
        );
    }
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    {
        let stopping = gateway::stop(&fixture);
        tokio::pin!(stopping);
        if let Some(held_auth) = held_auth.as_ref() {
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut stopping)
                    .await
                    .is_err(),
                "stop must retain physical AKA until its actual worker returns"
            );
            assert_eq!(fixture.card.auth.load(Ordering::SeqCst), 3);
            assert_eq!(fixture.card.auth_finished.load(Ordering::SeqCst), 2);
            held_auth.release();
        }
        stopping.await;
    }
    assert_eq!(
        fixture.card.auth_finished.load(Ordering::SeqCst),
        fixture.card.auth.load(Ordering::SeqCst),
        "stop must join physical AKA before releasing its lease"
    );
    let port = active["call"]["audio_port"].as_u64().unwrap() as u16;
    let socket = tokio::net::UdpSocket::bind(("127.0.0.1", port))
        .await
        .expect("exact PCM port must be released after stop/failure");
    drop(socket);
    let trace = peer.trace.clone();
    fixture.finish().await;
    peer.finish().await;
    assert_eq!(trace.ike.load(Ordering::SeqCst), 1);
    assert_eq!(trace.deletes.load(Ordering::SeqCst), 1);
    eprintln!("verified active-call refresh {mode:?}");
}
