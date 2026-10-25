use super::*;
use crate::native_vowifi_session::ordinary::sms_binding_test_support::Facts;
use crate::native_vowifi_supervisor::SessionGeneration;

fn intent(phase: intent::Phase) -> intent::Snapshot {
    intent::Snapshot {
        intent_id: 1,
        attempt: 0,
        generation: Some(SessionGeneration::from_value(1).unwrap()),
        phase,
        last_error: None,
        last_end: None,
        retry_at: None,
        cleanup_confirmed: false,
    }
}

#[tokio::test]
async fn independent_tunnel_registration_and_consumer_stages_match_public_contract() {
    let facts = Facts::registered(
        "status-reader",
        3,
        SessionGeneration::from_value(1).unwrap(),
    );
    let mut snapshot = facts.handle.snapshot();
    snapshot.phase = ordinary::Phase::Starting;
    snapshot.tunnel_started = false;
    snapshot.tunnel_ready = false;
    snapshot.register_started = false;
    snapshot.registered = false;
    snapshot.sms_ready = false;
    snapshot.call_ready = false;
    let mut state = intent(intent::Phase::Starting);
    // Expected states are the independently observable Go public stages.
    for (stage, tunnel, ims, step, description) in [
        (0, "connecting", "idle", 1, "SIM 卡已就绪"),
        (
            1,
            "connecting",
            "idle",
            2,
            "正在通过 EAP-AKA 建立 IKEv2 隧道...",
        ),
        (
            2,
            "connected",
            "idle",
            3,
            "IKEv2、NAT-T 与用户态 ESP 隧道已就绪",
        ),
        (3, "connected", "registering", 4, "正在通过隧道注册 IMS..."),
        (
            4,
            "connected",
            "registered",
            4,
            "IMS 已注册，正在等待 SMS 安全通道",
        ),
        (
            5,
            "connected",
            "registered",
            5,
            "IMS 已注册，SMS 安全通道已就绪",
        ),
    ] {
        snapshot.tunnel_started = stage >= 1;
        snapshot.tunnel_ready = stage >= 2;
        snapshot.register_started = stage >= 3;
        snapshot.registered = stage >= 4;
        if stage == 5 {
            snapshot.phase = ordinary::Phase::Registered;
            snapshot.sms_ready = true;
            snapshot.call_ready = true;
            state.phase = intent::Phase::Registered;
        }
        let mut value = json!({});
        let projected = project(&mut value, &state, Some(&snapshot), false);
        assert_eq!(value["tunnel_state"], tunnel);
        assert_eq!(value["ims_state"], ims);
        assert_eq!(value["progress_step"], step);
        assert_eq!(value["progress_desc"], description);
        assert_eq!(value["sms_ready"], stage == 5);
        assert_eq!(value["call_ready"], stage == 5);
        assert_eq!(projected.tunnel_connected, stage >= 2);
        assert_eq!(projected.ims_registered, stage >= 4);
    }
    snapshot.sms_ready = false;
    let mut value = json!({});
    project(&mut value, &state, Some(&snapshot), false);
    assert_eq!(value["progress_step"], 4);
    assert_eq!(value["sms_ready"], false);
    assert_eq!(value["call_ready"], true);
}

#[tokio::test]
async fn old_attempt_facts_never_make_backoff_stop_or_failed_session_ready() {
    let facts = Facts::registered(
        "status-reader",
        3,
        SessionGeneration::from_value(1).unwrap(),
    );
    let snapshot = facts.handle.snapshot();
    for phase in [
        intent::Phase::Backoff,
        intent::Phase::Draining,
        intent::Phase::Stopped,
        intent::Phase::Failed,
        intent::Phase::CleanupUnconfirmed,
        intent::Phase::ReaderChanged,
    ] {
        let mut value = json!({});
        let projected = project(&mut value, &intent(phase), Some(&snapshot), false);
        assert!(!projected.tunnel_connected && !projected.ims_registered);
        assert_eq!(value["sms_ready"], false);
        assert_eq!(value["call_ready"], false);
        assert_ne!(value["progress_step"], 5);
    }
    let mut value = json!({});
    project(
        &mut value,
        &intent(intent::Phase::Registered),
        Some(&snapshot),
        true,
    );
    assert_eq!(value["tunnel_state"], "disconnected");
    assert_eq!(value["ims_state"], "idle");
    assert_eq!(value["progress_step"], 0);
    let mut next = intent(intent::Phase::Registered);
    next.generation = Some(SessionGeneration::from_value(2).unwrap());
    let projected = project(&mut value, &next, Some(&snapshot), false);
    assert!(!projected.tunnel_connected && !projected.ims_registered);
    assert_eq!(value["sms_ready"], false);
    assert_eq!(value["call_ready"], false);
}

#[test]
fn typed_startup_failure_explains_error_without_changing_readiness_or_cleanup() {
    let diagnostic = super::super::super::startup_diagnostics::startup(
        &NativeRegisteredStartupFailure::Tunnel(NativeTunnelStartError::Startup(
            TunnelStartupError::SaInit(proletarius_vowifi::SaInitError::Timeout),
        )),
    );
    let mut value = json!({"sms_ready":false,"call_ready":false,"cleanup_confirmed":false,"tunnel_state":"error","ims_state":"failed"});
    failure(
        &mut value,
        Some("native_vowifi_startup_failed"),
        Some(diagnostic),
    );
    assert_eq!(value["error_code"], "native_vowifi_startup_failed");
    assert_eq!(value["startup_failure"]["stage"], "ike_sa_init");
    assert_eq!(value["startup_failure"]["code"], "timeout");
    assert_eq!(value["message"], diagnostic.message);
    assert_eq!(value["sms_ready"], false);
    assert_eq!(value["call_ready"], false);
    assert_eq!(value["cleanup_confirmed"], false);
    assert_eq!(value["tunnel_state"], "error");
    assert_eq!(value["ims_state"], "failed");
    assert!(value["startup_failure"].get("detail").is_none());
    let mut stopped = json!({});
    failure(&mut stopped, None, Some(diagnostic));
    assert_eq!(
        stopped,
        json!({}),
        "a diagnosis cannot manufacture failure after user stop"
    );
}
