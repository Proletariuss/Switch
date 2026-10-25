use super::*;

fn fixture() -> (Reporter, watch::Receiver<Snapshot>) {
    Reporter::new(Binding {
        reader: "status-fixture".into(),
        reader_generation: 7,
        session_generation: SessionGeneration::from_value(3).unwrap(),
        key: ReaderSessionKey::new("status-fixture", "894433000000000001").unwrap(),
    })
}

#[test]
fn actual_tunnel_configuration_does_not_claim_registered_or_consumer_readiness() {
    let (reporter, status) = fixture();
    reporter.register_started();
    reporter.consumers_ready("+447802002606", "sip:smsc@ims.example");
    assert!(!status.borrow().register_started);
    assert!(!status.borrow().sms_ready);
    let config = TunnelConfiguration {
        ipv4_addresses: vec!["10.1.2.3".parse().unwrap()],
        ipv6_addresses: vec![("fd00::1".parse().unwrap(), 64)],
        pcscf_servers: vec!["10.1.2.4".parse().unwrap()],
        ..Default::default()
    };
    reporter.tunnel_started();
    assert!(!status.borrow().tunnel_ready);
    reporter.tunnel_ready(&config);
    reporter.register_started();
    reporter.consumers_ready("+447802002606", "sip:smsc@ims.example");
    let snapshot = status.borrow();
    assert!(snapshot.tunnel_started && snapshot.tunnel_ready && snapshot.register_started);
    assert_eq!(snapshot.local_ipv4, config.ipv4_addresses);
    assert_eq!(snapshot.local_ipv6[0], config.ipv6_addresses[0].0);
    assert_eq!(snapshot.pcscf, config.pcscf_servers);
    assert!(!snapshot.registered && !snapshot.sms_ready && !snapshot.call_ready);
    assert_eq!(snapshot.phase, Phase::Starting);
    assert!(snapshot.ims_transport.is_none() && snapshot.registered_msisdn.is_none());
}

#[test]
fn cleanup_without_exact_release_never_allows_rebuild_and_late_failure_cannot_erase_proof() {
    let (reporter, status) = fixture();
    reporter.terminated(Termination::new(
        EndReason::RegistrationRenewal,
        RetryClass::RetryAfterRelease,
        None,
        None,
    ));
    reporter.cleanup_confirmed();
    reporter.cleanup_unconfirmed();
    assert_eq!(status.borrow().phase, Phase::CleanupUnconfirmed);
    assert_eq!(
        status.borrow().cleanup_error,
        Some("native_vowifi_cleanup_unconfirmed")
    );
    assert!(!status.borrow().may_restart());
    reporter.lease_released();
    assert!(status.borrow().may_restart());
    reporter.release_failed("late error containing subscriber secret".into());
    assert_eq!(status.borrow().cleanup_error, None);
    assert_eq!(
        status.borrow().cleanup_detail(),
        Some("late error containing subscriber secret")
    );
    assert!(!format!("{:?}", status.borrow()).contains("subscriber secret"));
    reporter.cleanup_unconfirmed();
    reporter.draining();
    reporter.tunnel_started();
    reporter.tunnel_ready(&TunnelConfiguration::default());
    reporter.consumers_ready("+447802002606", "sip:smsc@ims.example");
    assert_eq!(status.borrow().phase, Phase::Released);
    assert!(status.borrow().may_restart());
    assert!(!status.borrow().sms_ready && !status.borrow().call_ready);
}

#[tokio::test]
async fn observer_closure_never_manufactures_release_or_retry() {
    let (reporter, mut status) = fixture();
    reporter.terminated(Termination::new(
        EndReason::NetworkFailure,
        RetryClass::RetryAfterRelease,
        Some("native_vowifi_network_failed"),
        None,
    ));
    let _ = status.borrow_and_update();
    drop(reporter);
    assert!(status.changed().await.is_err());
    assert!(!status.borrow().lease_released && !status.borrow().may_restart());
}

#[test]
fn private_diagnostic_is_retained_but_not_exposed_as_public_error_or_debug() {
    let detail = "AKA failed for subscriber 234330000000001 via proxy user:secret";
    let termination = Termination::new(
        EndReason::StartupFailure,
        RetryClass::Permanent,
        Some("native_vowifi_startup_failed"),
        Some(detail.into()),
    );
    assert_eq!(termination.detail(), Some(detail));
    assert_eq!(termination.clone().detail(), Some(detail));
    assert!(!format!("{termination:?}").contains("234330000000001"));
    assert!(!format!("{termination:?}").contains("secret"));
    let (reporter, status) = fixture();
    reporter.terminated(termination);
    reporter.lease_released();
    assert!(!status.borrow().may_restart());
    assert!(!format!("{:?}", status.borrow().binding).contains("894433000000000001"));
}

#[test]
fn independent_reporters_cannot_rebind_old_generation_or_card_key() {
    let (old, status) = fixture();
    let mut next_binding = status.borrow().binding.clone();
    next_binding.session_generation = SessionGeneration::from_value(4).unwrap();
    next_binding.key = ReaderSessionKey::new("status-fixture", "894433000000000002").unwrap();
    let (_, next) = Reporter::new(next_binding);
    old.lease_released();
    assert_ne!(status.borrow().binding, next.borrow().binding);
    assert!(!next.borrow().lease_released);
}

#[test]
fn incoming_facts_remain_with_the_captured_session_and_require_real_release_to_close() {
    use proletarius_vowifi::registration_coordinator::{IncomingDiagnostics, IncomingReachability};
    let (old, status) = fixture();
    let mut next_binding = status.borrow().binding.clone();
    next_binding.session_generation = SessionGeneration::from_value(4).unwrap();
    let (_, next) = Reporter::new(next_binding);
    let observed = IncomingDiagnostics {
        listener_started: true,
        accept_count: 1,
        active_connections: 1,
        last_sip_at: Some(std::time::SystemTime::now()),
        reachability: IncomingReachability::Observed,
        ..Default::default()
    };
    old.incoming(observed);
    old.draining();
    assert!(status.borrow().incoming.listener_started);
    assert_eq!(status.borrow().incoming.active_connections, 1);
    assert_eq!(next.borrow().incoming, IncomingDiagnostics::default());
    old.cleanup_unconfirmed();
    assert_eq!(status.borrow().incoming.closed_connections, 0);
    old.lease_released();
    assert!(!status.borrow().incoming.listener_started);
    assert_eq!(status.borrow().incoming.active_connections, 0);
    assert_eq!(status.borrow().incoming.closed_connections, 1);
    old.incoming(observed);
    assert!(!status.borrow().incoming.listener_started);
    assert_eq!(status.borrow().incoming.closed_connections, 1);
    assert_eq!(next.borrow().incoming.accept_count, 0);
    assert_eq!(
        next.borrow().incoming.reachability,
        IncomingReachability::Unverified
    );
}

#[test]
fn heartbeat_miss_withdraws_only_readiness_and_recovery_never_reopens_draining_generation() {
    let (reporter, status) = fixture();
    reporter.heartbeat_health(true);
    assert!(!status.borrow().registered && !status.borrow().sms_ready);
    // Boundary fixture: REGISTER/consumer facts are exercised by real wire tests.
    reporter.state.send_modify(|s| s.registered = true);
    reporter.consumers_ready("+447802002606", "sip:smsc@ims.example");
    reporter.heartbeat_health(false);
    assert!(status.borrow().registered);
    assert!(!status.borrow().sms_ready && !status.borrow().call_ready);
    reporter.heartbeat_health(true);
    assert!(status.borrow().sms_ready && status.borrow().call_ready);
    reporter.draining();
    reporter.heartbeat_health(true);
    assert!(!status.borrow().sms_ready && !status.borrow().call_ready);
    assert_eq!(status.borrow().phase, Phase::Draining);
}

#[test]
fn absent_sms_routes_never_become_send_ready_and_do_not_disable_calls() {
    for (smsc, target, ready) in [
        ("", "sip:smsc@ims.example", false),
        ("+447802002606", "", false),
        ("  ", "sip:smsc@ims.example", false),
        ("+447802002606", "  ", false),
        ("+447802002606", "sip:smsc@ims.example", true),
    ] {
        let (reporter, status) = fixture();
        // Boundary facts only; actual REGISTER and caller wiring are covered
        // by the full local Service/SMS composition.
        reporter.state.send_modify(|s| s.registered = true);
        reporter.consumers_ready(smsc, target);
        assert_eq!(status.borrow().sms_route_ready, ready);
        assert_eq!(status.borrow().sms_ready, ready);
        assert!(status.borrow().call_ready);
        reporter.heartbeat_health(false);
        assert!(!status.borrow().sms_ready && !status.borrow().call_ready);
        reporter.heartbeat_health(true);
        assert_eq!(status.borrow().sms_ready, ready);
        assert!(status.borrow().call_ready);
        reporter.draining();
        reporter.heartbeat_health(true);
        assert!(!status.borrow().sms_ready && !status.borrow().call_ready);
    }
}

#[test]
fn sms_only_heartbeat_recovery_never_enables_voice() {
    let (reporter, status) = fixture();
    reporter.state.send_modify(|s| s.registered = true);
    reporter.profile_consumers_ready("+447802002606", "sip:smsc@ims.example", false);
    assert!(status.borrow().sms_ready);
    assert!(!status.borrow().call_ready);
    reporter.heartbeat_health(false);
    assert!(!status.borrow().sms_ready && !status.borrow().call_ready);
    reporter.heartbeat_health(true);
    assert!(status.borrow().sms_ready);
    assert!(!status.borrow().call_ready);
    reporter.draining();
    reporter.heartbeat_health(true);
    assert!(!status.borrow().sms_ready && !status.borrow().call_ready);
}

#[test]
fn nwait_archive_warning_is_generation_bound_sticky_and_never_restores_readiness() {
    use crate::ims_sms_transactions::NativeSmsArchiveWarning::RawRecordUnmerged;
    let (old, mut old_status) = fixture();
    let mut ready = old_status.borrow().clone();
    ready.phase = Phase::Registered;
    ready.registered = true;
    ready.sms_route_ready = true;
    ready.sms_ready = true;
    old.set_test_snapshot(ready.clone());
    let observer = old.sms_archive_warning_observer();
    let generation = ready.binding.session_generation;
    old_status.borrow_and_update();
    observer.observe(
        SessionGeneration::from_value(generation.value() + 1).unwrap(),
        RawRecordUnmerged,
    );
    assert!(!old_status.has_changed().unwrap());
    assert!(old_status.borrow().sms_archive_warning.is_none());
    observer.observe(generation, RawRecordUnmerged);
    assert_eq!(
        old_status.borrow_and_update().active_sms_archive_warning(),
        Some(RawRecordUnmerged)
    );
    observer.observe(generation, RawRecordUnmerged);
    assert!(
        !old_status.has_changed().unwrap(),
        "duplicate warnings must not create watch churn"
    );
    assert!(old_status.borrow().sms_ready);
    let mut binding = ready.binding.clone();
    binding.reader_generation += 1;
    binding.session_generation = SessionGeneration::from_value(generation.value() + 1).unwrap();
    let (next, next_status) = Reporter::new(binding);
    let next_observer = next.sms_archive_warning_observer();
    next_observer.observe(generation, RawRecordUnmerged);
    assert!(next_status.borrow().sms_archive_warning.is_none());
    old.draining();
    old_status.borrow_and_update();
    observer.observe(generation, RawRecordUnmerged);
    assert!(!old_status.has_changed().unwrap());
    assert!(old_status.borrow().active_sms_archive_warning().is_none());
    assert!(!old_status.borrow().sms_ready);
    old.lease_released();
    old_status.borrow_and_update();
    observer.observe(generation, RawRecordUnmerged);
    assert!(!old_status.has_changed().unwrap());
    assert!(next_status.borrow().sms_archive_warning.is_none());
    assert!(!next_status.borrow().sms_ready);
}

#[test]
fn nwait_archive_warning_requires_registered_fact_on_both_write_and_read() {
    use crate::ims_sms_transactions::NativeSmsArchiveWarning::RawRecordUnmerged;
    let (reporter, mut status) = fixture();
    let mut unregistered = status.borrow().clone();
    unregistered.phase = Phase::Registered;
    unregistered.registered = false;
    reporter.set_test_snapshot(unregistered.clone());
    let observer = reporter.sms_archive_warning_observer();
    let generation = unregistered.binding.session_generation;
    status.borrow_and_update();
    observer.observe(generation, RawRecordUnmerged);
    assert!(!status.has_changed().unwrap());
    assert!(status.borrow().sms_archive_warning.is_none());
    unregistered.sms_archive_warning = Some(RawRecordUnmerged);
    reporter.set_test_snapshot(unregistered.clone());
    assert!(status.borrow().active_sms_archive_warning().is_none());
    unregistered.registered = true;
    reporter.set_test_snapshot(unregistered);
    assert_eq!(
        status.borrow().active_sms_archive_warning(),
        Some(RawRecordUnmerged)
    );
}
