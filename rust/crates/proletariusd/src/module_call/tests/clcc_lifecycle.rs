use super::*;

const DATA_ONLY: &str = "AT+CLCC\r\n+CLCC: 1,0,0,1,0,\"\",128\r\nOK\r\n";
const VOICE: &str = "+CLCC: 1,0,0,0,0,\"\",128\r\nOK\r\n";

fn wire(text: &str) -> CallStateSnapshot {
    crate::module_radio::call_snapshot(text.to_owned())
}

async fn lease_present(service: &Service, present: bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while service.0.physical.lock().await.lease.is_some() != present {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn packet_data_polling_and_hangup_never_acquire_call_or_submit_ath() {
    let (service, fake, root) = fixture("clcc-data-idle");
    *fake.0.default_state.lock().unwrap() = wire(DATA_ONLY);
    for _ in 0..4 {
        let value = data(service.status().await).await;
        assert_eq!(value["state"], "idle");
        assert!(!value["number"].as_str().is_some_and(|n| !n.is_empty()));
        assert!(!service.0.state.snapshot().desired_active);
    }
    let reply = data(service.execute(Action::Hangup, String::new()).await).await;
    assert_eq!(reply["already_idle"], true);
    assert_eq!(fake.0.reserve_count.load(Ordering::Acquire), 0);
    assert!(fake.0.actions.lock().unwrap().is_empty());
    assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 0);
    finish(service, root).await;
}

#[tokio::test]
async fn confirmed_data_only_observation_releases_exact_prior_call_without_replaying() {
    let (service, fake, root) = fixture("clcc-data-release");
    *fake.0.default_state.lock().unwrap() = wire(VOICE);
    assert_eq!(data(service.status().await).await["state"], "active");
    lease_present(&service, true).await;
    let token = service.0.physical.lock().await.lease.as_ref().unwrap().token.clone();
    fake.0.release_failures.store(1, Ordering::Release);
    *fake.0.default_state.lock().unwrap() = wire(DATA_ONLY);
    assert_eq!(data(service.status().await).await["state"], "idle");
    lease_present(&service, false).await;
    assert!(fake.0.releases.lock().unwrap().len() >= 2);
    assert!(fake.0.releases.lock().unwrap().iter().all(|value| value == &token));
    assert_eq!(data(service.status().await).await["state"], "idle");
    assert_eq!(fake.0.reserve_count.load(Ordering::Acquire), 1);
    assert!(fake.0.actions.lock().unwrap().is_empty());
    finish(service, root).await;
}

#[tokio::test]
async fn standard_anonymous_voice_held_and_mixed_calls_keep_radio_protection() {
    for (name, response) in [
        ("anonymous", VOICE),
        ("held", "+CLCC: 1,0,1,0,0,\"\",128\r\nOK\r\n"),
        ("incoming", "+CLCC: 1,1,4,0,0,\"\",128\r\nOK\r\n"),
        ("mixed", "+CLCC: 1,0,0,0,0,\"\",128\r\n+CLCC: 2,0,0,1,0,\"\",128\r\nOK\r\n"),
    ] {
        let (service, fake, root) = fixture(name);
        *fake.0.default_state.lock().unwrap() = wire(response);
        let value = data(service.status().await).await;
        assert_ne!(value["state"], "idle");
        assert!(!value["number"].as_str().is_some_and(|n| n.contains(",128")));
        assert!(service.0.state.snapshot().desired_active);
        assert!(!service.0.state.snapshot().safe_idle);
        lease_present(&service, true).await;
        assert!(fake.0.actions.lock().unwrap().is_empty());
        finish(service, root).await;
    }
}

#[tokio::test]
async fn ath_ok_without_voice_idle_never_claims_hangup_or_stops_audio() {
    for (name, after) in [
        ("still-active", VOICE),
        ("uncertain", "+CLCC: 1,0,0,9,0,\"\",128\r\nOK\r\n"),
        ("incomplete", "+CLCC: 1,0,0,1,0,\"\",128\r\n"),
    ] {
        let (service, fake, root) = fixture(name);
        fake.0.queries.lock().unwrap().extend([wire(VOICE), wire(VOICE), wire(after)]);
        *fake.0.default_state.lock().unwrap() = wire(after);
        let response = service.execute(Action::Hangup, String::new()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = data(response).await;
        assert_eq!(value["accepted"], true);
        assert_eq!(value["confirmed"], false);
        assert_eq!(value["code"], "module_call_hangup_unconfirmed");
        assert!(!value["warning"].as_str().unwrap().is_empty());
        assert!(service.0.state.snapshot().desired_active);
        assert!(service.0.physical.lock().await.lease.is_some());
        assert_eq!(fake.0.actions.lock().unwrap().as_slice(), &[(Action::Hangup, String::new())]);
        assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 0);
        finish(service, root).await;
    }
}

#[tokio::test]
async fn ath_ack_then_confirmed_data_only_query_ends_voice_and_cleans_audio_once() {
    let (service, fake, root) = fixture("hangup-confirmed");
    fake.0.queries.lock().unwrap().extend([wire(VOICE), wire(VOICE), wire(DATA_ONLY)]);
    *fake.0.default_state.lock().unwrap() = wire(DATA_ONLY);
    let value = data(service.execute(Action::Hangup, String::new()).await).await;
    assert_eq!(value["confirmed"], true);
    assert_eq!(value["warning"], "");
    assert_eq!(service.0.state.snapshot().display["state"], "idle");
    lease_present(&service, false).await;
    assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 1);
    assert_eq!(fake.0.actions.lock().unwrap().as_slice(), &[(Action::Hangup, String::new())]);
    assert_eq!(data(service.status().await).await["state"], "idle");
    assert_eq!(fake.0.reserve_count.load(Ordering::Acquire), 1);
    finish(service, root).await;
}

#[tokio::test]
async fn ring_during_hangup_confirmation_cannot_be_ended_by_late_data_query() {
    let (service, fake, root) = fixture("hangup-confirmation-ring");
    fake.0.queries.lock().unwrap().extend([wire(VOICE), wire(VOICE), wire(DATA_ONLY)]);
    fake.0.execute_wait.store(true, Ordering::Release);
    let observer = tokio::spawn(service.execute(Action::Hangup, String::new()));
    signal(&fake.0.execute_entered).await;
    // Only the post-command verification is held. A real new RING must beat
    // the older data-only reply even though its bytes were already captured.
    fake.0.query_wait.store(true, Ordering::Release);
    fake.0.execute_resume.add_permits(1);
    signal(&fake.0.query_entered).await;
    fake.emit("ring");
    fake.0.query_resume.add_permits(1);
    let value = data(observer.await.unwrap()).await;
    assert_eq!(value["confirmed"], false);
    assert_eq!(service.0.state.snapshot().display["state"], "incoming");
    assert!(service.0.state.snapshot().desired_active);
    assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 0);
    assert_eq!(fake.0.actions.lock().unwrap().len(), 1);
    finish(service, root).await;
}

fn assert_hangup_queries_keep_lease(fake: &Fake, expected: &CallReservation) {
    let queries = fake.0.query_leases.lock().unwrap();
    assert_eq!(queries.len(), 3);
    assert!(queries[0].is_none());
    for observed in &queries[1..] {
        let observed = observed.as_ref().expect("query must borrow the call lease");
        assert_eq!(observed.producer_id, expected.producer_id);
        assert_eq!(observed.token, expected.token);
        assert_eq!(observed.sequence, expected.sequence);
        assert_eq!(observed.epoch, expected.epoch);
        assert_eq!(observed.binding, expected.binding);
    }
}

#[tokio::test]
async fn unknown_initial_hangup_keeps_protection_until_terminal_evidence() {
    let (service, fake, root) = fixture("hangup-initial-unknown");
    *fake.0.default_state.lock().unwrap() = snapshot("unknown", false);
    let value = data(service.execute(Action::Hangup, String::new()).await).await;
    assert_eq!(value["confirmed"], false);
    assert_eq!(value["code"], "module_call_hangup_unconfirmed");
    let state = service.0.state.snapshot();
    assert_eq!(state.display["state"], "unknown");
    assert!(state.desired_active && !state.safe_idle);
    let lease = service.0.physical.lock().await.lease.clone().unwrap();
    assert_hangup_queries_keep_lease(&fake, &lease);
    assert!(fake.0.releases.lock().unwrap().is_empty());
    assert_eq!(fake.0.actions.lock().unwrap().as_slice(), &[(Action::Hangup, String::new())]);
    assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 0);
    finish(service, root).await;
}

#[tokio::test]
async fn unknown_initial_hangup_with_failed_confirmation_keeps_exact_lease() {
    let (service, fake, root) = fixture("hangup-initial-unknown-query-error");
    *fake.0.default_state.lock().unwrap() = snapshot("unknown", false);
    fake.0.execute_wait.store(true, Ordering::Release);
    let observer = tokio::spawn(service.execute(Action::Hangup, String::new()));
    signal(&fake.0.execute_entered).await;
    fake.0.fail_next_query.store(true, Ordering::Release);
    fake.0.execute_resume.add_permits(1);
    let response = observer.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = data(response).await;
    assert_eq!(value["accepted"], true);
    assert_eq!(value["confirmed"], false);
    assert_eq!(value["code"], "module_call_hangup_unconfirmed");
    let state = service.0.state.snapshot();
    assert_eq!(state.display["state"], "unknown");
    assert!(state.desired_active && !state.safe_idle);
    let lease = service.0.physical.lock().await.lease.clone().unwrap();
    assert_hangup_queries_keep_lease(&fake, &lease);
    assert!(fake.0.releases.lock().unwrap().is_empty());
    assert_eq!(fake.0.actions.lock().unwrap().as_slice(), &[(Action::Hangup, String::new())]);
    assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 0);
    finish(service, root).await;
}

#[tokio::test]
async fn failed_hangup_confirmation_cannot_replace_newer_ring_or_terminal() {
    for event in ["ring", "hangup"] {
        let (service, fake, root) = fixture(event);
        *fake.0.default_state.lock().unwrap() = snapshot("unknown", false);
        fake.0.execute_wait.store(true, Ordering::Release);
        let observer = tokio::spawn(service.execute(Action::Hangup, String::new()));
        signal(&fake.0.execute_entered).await;
        let lease = fake.0.query_leases.lock().unwrap()[1].clone().unwrap();
        fake.0.fail_next_query.store(true, Ordering::Release);
        fake.0.query_wait.store(true, Ordering::Release);
        fake.0.execute_resume.add_permits(1);
        signal(&fake.0.query_entered).await;
        fake.emit(event);
        fake.0.query_resume.add_permits(1);
        let value = data(observer.await.unwrap()).await;
        let state = service.0.state.snapshot();
        if event == "ring" {
            assert_eq!(value["confirmed"], false);
            assert_eq!(state.display["state"], "incoming");
            assert!(state.desired_active && !state.safe_idle);
            assert!(service.0.physical.lock().await.lease.is_some());
        } else {
            assert_eq!(value["confirmed"], true);
            assert_eq!(state.display["state"], "idle");
            assert!(!state.desired_active && state.safe_idle);
            lease_present(&service, false).await;
        }
        assert_hangup_queries_keep_lease(&fake, &lease);
        assert_eq!(fake.0.actions.lock().unwrap().as_slice(), &[(Action::Hangup, String::new())]);
        assert_eq!(fake.0.audio_stops.load(Ordering::Acquire), 0);
        finish(service, root).await;
    }
}
