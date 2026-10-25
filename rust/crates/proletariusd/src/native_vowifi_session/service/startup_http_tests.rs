use super::*;
use http_body_util::BodyExt;
use serde_json::Value;

struct SnapshotIdentity(Mutex<crate::reader_lifecycle::Snapshot>);
impl IdentitySource for SnapshotIdentity {
    fn capture(&self) -> Result<intent::ExpectedIdentity, String> {
        identity_from_snapshot(self.0.lock().unwrap().clone())
    }
    fn snapshot(&self) -> Option<crate::reader_lifecycle::Snapshot> {
        Some(self.0.lock().unwrap().clone())
    }
}
fn snapshot(service: &Service, connected: bool) -> crate::reader_lifecycle::Snapshot {
    let mut selected = service.0.resources.gate.selection_snapshot();
    if !connected {
        selected.reader.clear();
    }
    crate::reader_lifecycle::Snapshot {
        status: json!({"connected":connected}),
        usim_info: connected.then(|| {
            json!({
                "iccid":"894433000000000001", "imsi":"234330000000001",
                "mcc":"234", "mnc":"33", "phone_number":"",
                "private_identity":"", "public_identity":"", "home_domain":""
            })
        }),
        selection: selected,
        esim_manager_available: false,
        notification_aids: Vec::new(),
        esim_profile_name: String::new(),
        pcsc_session: None,
    }
}
fn observing(service: &Service, snapshot: crate::reader_lifecycle::Snapshot) -> Service {
    let mut resources = service.0.resources.clone();
    resources.identity = Arc::new(SnapshotIdentity(Mutex::new(snapshot)));
    Service(Arc::new(Inner {
        sms_epoch: sms::new_epoch().unwrap(),
        resources,
        state: Mutex::new(State::default()),
        mutation: tokio::sync::Mutex::new(()),
    }))
}
fn observing_durable(service: &Service, snapshot: crate::reader_lifecycle::Snapshot) -> Service {
    let observed = observing(service, snapshot);
    observed.0.state.lock().unwrap().requests =
        super::super::operation::RequestLedger::open(&observed.0.resources.data_root, "reader")
            .unwrap();
    observed
}
async fn post(service: &Service, bytes: &[u8]) -> (StatusCode, serde_json::Value) {
    let response = service
        .handle_public(
            Request::builder()
                .method(hyper::Method::POST)
                .uri("/api/reader/vowifi")
                .body(Full::new(Bytes::copy_from_slice(bytes)))
                .unwrap(),
        )
        .await;
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn get(service: &Service, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = service
        .handle_public(
            Request::builder()
                .method(hyper::Method::GET)
                .uri(uri)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await;
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn reconcile(service: &Service, bytes: &[u8]) -> (StatusCode, serde_json::Value) {
    let response = service
        .handle_public(
            Request::builder()
                .method(hyper::Method::POST)
                .uri("/api/reader/vowifi/reconcile")
                .body(Full::new(Bytes::copy_from_slice(bytes)))
                .unwrap(),
        )
        .await;
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[test]
fn reconcile_route_is_rust_owned_post_only() {
    assert!(public_route(
        &hyper::Method::POST,
        "/api/reader/vowifi/reconcile"
    ));
    assert!(!public_route(
        &hyper::Method::GET,
        "/api/reader/vowifi/reconcile"
    ));
}

#[tokio::test]
async fn preadmission_rejections_have_durable_exact_id_terminal_receipts() {
    for scenario in ["no_sim", "busy", "invalid_proxy"] {
        let f = tests::Fixture::new(false);
        let mut card = snapshot(&f.service, scenario != "no_sim");
        if scenario == "busy" {
            card.selection.pending = true;
        }
        let service = observing_durable(&f.service, card);
        let id = format!("rejected-{scenario}");
        let proxy = if scenario == "invalid_proxy" {
            "invalid"
        } else {
            ""
        };
        let body = format!(
            "{{\"enable\":true,\"proxy\":\"{proxy}\",\"observe_operation\":true,\"client_request_id\":\"{id}\"}}"
        );
        let (status, _) = post(&service, body.as_bytes()).await;
        assert_eq!(
            status,
            match scenario {
                "no_sim" => StatusCode::SERVICE_UNAVAILABLE,
                "busy" => StatusCode::CONFLICT,
                _ => StatusCode::BAD_REQUEST,
            },
            "{scenario}"
        );
        // Model a lost HTTP response: the caller only has its saved ID.
        let query = format!("/api/reader/vowifi?observe_operation=true&client_request_id={id}");
        let (status, observed) = get(&service, &query).await;
        assert_eq!(status, StatusCode::OK, "{scenario}");
        assert_eq!(observed["operation"]["client_request_id"], id);
        assert_eq!(observed["operation"]["requested_enable"], true);
        assert_eq!(observed["operation"]["phase"], "rejected");
        assert_eq!(observed["operation"]["outcome"], "failed");
        assert_eq!(observed["operation"]["terminal"], true);
        assert_eq!(observed["operation"]["succeeded"], false);
        assert_eq!(observed["operation"]["cleanup_confirmed"], false);
        let reopened =
            super::super::operation::RequestLedger::open(&service.0.resources.data_root, "reader")
                .unwrap();
        let exact = reopened.lookup(&id, None).unwrap().unwrap().resolve(None);
        assert_eq!(exact.phase, "rejected", "{scenario}");
        let (replay_status, replay) = post(&service, body.as_bytes()).await;
        assert_eq!(replay_status, StatusCode::ACCEPTED, "{scenario}");
        assert_eq!(replay["operation"]["phase"], "rejected");
        f.assert_no_dispatch();
        f.close().await;
    }
}

#[tokio::test]
async fn reconcile_only_tombstones_a_missing_id_after_clean_owner_proof() {
    let f = tests::Fixture::new(false);
    let service = observing_durable(&f.service, snapshot(&f.service, true));
    let body = br#"{"enable":true,"proxy":"","observe_operation":true,"client_request_id":"old-orphan-reader"}"#;
    let (status, receipt) = reconcile(&service, body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        receipt["operation"]["client_request_id"],
        "old-orphan-reader"
    );
    assert_eq!(receipt["operation"]["phase"], "rejected");
    assert_eq!(receipt["operation"]["terminal"], true);
    let (status, observed) = get(
        &service,
        "/api/reader/vowifi?observe_operation=true&client_request_id=old-orphan-reader",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(observed["operation"]["phase"], "rejected");
    let reopened =
        super::super::operation::RequestLedger::open(&service.0.resources.data_root, "reader")
            .unwrap();
    assert!(
        reopened
            .lookup("old-orphan-reader", None)
            .unwrap()
            .is_some()
    );
    let (status, replay) = post(&service, body).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(replay["operation"]["phase"], "rejected");
    f.assert_no_dispatch();
    f.close().await;
}

#[tokio::test]
async fn reconcile_refuses_unresolved_owner_without_tombstoning() {
    let f = tests::Fixture::new(true);
    let service = observing_durable(&f.service, snapshot(&f.service, true));
    service.start(tests::request()).await.unwrap();
    f.entered(1).await;
    let body = br#"{"enable":true,"proxy":"","observe_operation":true,"client_request_id":"orphan-while-owner-active"}"#;
    let (status, response) = reconcile(&service, body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(response["code"], "vowifi_reconcile_owner_unconfirmed");
    assert!(
        service
            .lookup_request("orphan-while-owner-active", None)
            .unwrap()
            .is_none()
    );
    f.resume();
    f.close().await;
}

#[tokio::test]
async fn concurrent_reconcile_and_post_use_one_exact_request_identity() {
    let f = tests::Fixture::new(true);
    let service = observing_durable(&f.service, snapshot(&f.service, true));
    let body = br#"{"enable":true,"proxy":"","observe_operation":true,"client_request_id":"concurrent-original-id"}"#;
    let (posted, reconciled) = tokio::join!(post(&service, body), reconcile(&service, body));
    assert_eq!(posted.0, StatusCode::ACCEPTED);
    assert_eq!(reconciled.0, StatusCode::OK);
    assert_eq!(
        posted.1["operation"]["client_request_id"],
        "concurrent-original-id"
    );
    assert_eq!(
        reconciled.1["operation"]["client_request_id"],
        "concurrent-original-id"
    );
    assert_eq!(
        posted.1["operation"]["operation_id"],
        reconciled.1["operation"]["operation_id"]
    );
    let reopened =
        super::super::operation::RequestLedger::open(&service.0.resources.data_root, "reader")
            .unwrap();
    assert!(
        reopened
            .lookup("concurrent-original-id", None)
            .unwrap()
            .is_some()
    );
    assert!(f.dispatch_count() <= 1);
    f.resume();
    f.close().await;
}

#[tokio::test]
async fn delayed_original_post_cannot_dispatch_after_reconcile_tombstone() {
    let f = tests::Fixture::new(false);
    let service = observing_durable(&f.service, snapshot(&f.service, true));
    let body = br#"{"enable":true,"proxy":"","observe_operation":true,"client_request_id":"delayed-original-id"}"#;
    let (reconciled, posted) = tokio::join!(reconcile(&service, body), async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        post(&service, body).await
    });
    assert_eq!(reconciled.0, StatusCode::OK);
    assert_eq!(reconciled.1["operation"]["phase"], "rejected");
    assert_eq!(posted.0, StatusCode::ACCEPTED);
    assert_eq!(posted.1["operation"]["phase"], "rejected");
    f.assert_no_dispatch();
    f.close().await;
}

#[test]
fn signed_go_port_decode_distinguishes_configuration_from_json_failure() {
    for number in [-9, 0, 500, 65535, 65536, i64::MAX] {
        let body = format!("{{\"enable\":true,\"epdg_port\":{number}}}");
        let (enabled, request) = decode_start(body.as_bytes()).unwrap();
        assert!(enabled);
        match request.freeze() {
            Ok(request) => assert_eq!(request.epdg_port, number.max(0) as u16),
            Err(StartFailure::Internal(message)) => {
                assert_eq!(message, format!("ePDG 端口无效: {number}"))
            }
            Err(_) => panic!("a port is not reader admission"),
        }
    }
    for body in [
        br#"{"epdg_port":1.5}"#.as_slice(),
        br#"{"epdg_port":"500"}"#,
        br#"{"epdg_port":9223372036854775808}"#,
        br#"{"enable":1}"#,
    ] {
        assert!(matches!(decode_start(body), Err("请求格式错误")));
    }
}

#[test]
fn proxy_structure_and_port_validation_matches_go_without_resolving_hostnames() {
    for proxy in [
        "",
        "host.test:1080",
        "socks5://host.test:1080",
        "socks5h://host.test:+1080",
        "[::1]:1080",
        "[fe80::1%en0]:1080",
        "[opaque-host]:80",
        " host.test :80 ",
    ] {
        assert!(validate_start_proxy(proxy).is_ok(), "{proxy}");
    }
    for proxy in ["http://host:80", "host", "::1:1080", "[::1]1080", ":1080"] {
        assert_eq!(
            validate_start_proxy(proxy),
            Err("前置代理地址无效，请手动填写有效的 IP:端口")
        );
    }
    for proxy in ["host:0", "host:-1", "host:65536", "host:word", "host:"] {
        assert_eq!(validate_start_proxy(proxy), Err("前置代理端口无效"));
    }
}

#[tokio::test]
async fn malformed_request_and_bad_proxy_reject_before_no_sim_or_start() {
    let fixture = tests::Fixture::new(false);
    let service = observing(&fixture.service, snapshot(&fixture.service, false));
    for body in [
        br#"{"observe_operation":true,"client_request_id":"startup-invalid-enable","enable":1}"#
            .as_slice(),
        br#"{"observe_operation":true,"client_request_id":"startup-invalid-proxy-type","proxy":7}"#,
    ] {
        assert_eq!(
            post(&service, body).await,
            (StatusCode::BAD_REQUEST, json!({"error":"请求格式错误"}))
        );
    }
    assert_eq!(
        post(&service, br#"{"observe_operation":true,"client_request_id":"startup-fixture","enable":true,"proxy":"bad"}"#).await,
        (
            StatusCode::BAD_REQUEST,
            json!({"error":"前置代理地址无效，请手动填写有效的 IP:端口"})
        )
    );
    assert_eq!(
        post(&service, br#"{"observe_operation":true,"client_request_id":"startup-fixture-port","enable":true,"proxy":"host:0"}"#).await,
        (StatusCode::BAD_REQUEST, json!({"error":"前置代理端口无效"}))
    );
    assert!(service.record().unwrap().is_none());
    assert!(!service.has_intent());
    fixture.close().await;
}

#[tokio::test]
async fn no_sim_is_503_before_configuration_port_error() {
    let fixture = tests::Fixture::new(false);
    let service = observing(&fixture.service, snapshot(&fixture.service, false));
    assert_eq!(
        post(&service, br#"{"observe_operation":true,"client_request_id":"startup-fixture","enable":true,"epdg_port":65536}"#).await,
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":"读卡器未连接或无 SIM 卡"})
        )
    );
    assert!(service.record().unwrap().is_none());
    fixture.close().await;
}

#[tokio::test]
async fn positive_oversize_port_preserves_go_configuration_500_without_dispatch() {
    let fixture = tests::Fixture::new(false);
    let service = observing(&fixture.service, snapshot(&fixture.service, true));
    assert_eq!(
        post(&service, br#"{"observe_operation":true,"client_request_id":"startup-fixture","enable":true,"epdg_port":65536}"#).await,
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error":"启动 VoWiFi 失败: ePDG 端口无效: 65536"})
        )
    );
    assert!(service.record().unwrap().is_none());
    fixture.close().await;
}

#[tokio::test]
async fn pending_selection_is_busy_before_no_sim_and_port_config() {
    let fixture = tests::Fixture::new(false);
    let mut facts = snapshot(&fixture.service, false);
    facts.selection.pending = true;
    let service = observing(&fixture.service, facts);
    assert_eq!(
        post(&service, br#"{"observe_operation":true,"client_request_id":"startup-fixture","enable":true,"epdg_port":65536}"#).await,
        (
            StatusCode::CONFLICT,
            json!({"error":"读卡器 VoWiFi 操作正在进行，请稍后重试"})
        )
    );
    assert_eq!(
        post(&service, br#"{"observe_operation":true,"client_request_id":"startup-fixture-port","enable":true,"proxy":"host:0"}"#)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert!(service.record().unwrap().is_none());
    fixture.close().await;
}

#[tokio::test]
async fn identified_disable_and_first_object_contract_do_not_validate_proxy_or_port() {
    let fixture = tests::Fixture::new(false);
    let mut facts = snapshot(&fixture.service, false);
    facts.selection.pending = true;
    let service = observing(&fixture.service, facts);
    for body in [
        br#"{"observe_operation":true,"client_request_id":"startup-fixture","enable":false,"proxy":"bad","epdg_port":65536}"#.as_slice(),
        br#"{"observe_operation":true,"client_request_id":"startup-second","ENABLE":true,"enable":null,"enable":false,"ignored":true} {}"#,
    ] {
        let (status, value) = post(&service, body).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(value["operation"]["requested_enable"], false);
    }
    assert!(service.record().unwrap().is_none());
    fixture.close().await;
}

#[tokio::test]
async fn request_budget_rejects_before_native_start() {
    let fixture = tests::Fixture::new(false);
    let service = observing(&fixture.service, snapshot(&fixture.service, true));
    let body = format!("{{\"enable\":true,\"ignored\":\"{}\"}}", "x".repeat(4096));
    assert_eq!(
        post(&service, body.as_bytes()).await.0,
        StatusCode::BAD_REQUEST
    );
    assert!(service.record().unwrap().is_none());
    fixture.close().await;
}

#[tokio::test]
async fn observed_start_request_id_replays_exact_operation_and_rejects_payload_drift() {
    let fixture = tests::Fixture::new(false);
    let service = observing(&fixture.service, snapshot(&fixture.service, true));
    service.0.state.lock().unwrap().requests =
        super::super::operation::RequestLedger::open(&service.0.resources.data_root, "reader")
            .unwrap();
    let body = br#"{"enable":true,"observe_operation":true,"client_request_id":"swift-start-1"}"#;
    let (first_status, first) = post(&service, body).await;
    assert_eq!(first_status, StatusCode::ACCEPTED);
    assert_eq!(first["operation"]["client_request_id"], "swift-start-1");
    let operation_id = first["operation"]["operation_id"].clone();
    let generation = first["operation"]["generation"].clone();

    let (replay_status, replay) = post(&service, body).await;
    assert_eq!(replay_status, StatusCode::ACCEPTED);
    assert_eq!(replay["operation"]["operation_id"], operation_id);
    assert_eq!(replay["operation"]["generation"], generation);
    assert_eq!(service.0.state.lock().unwrap().next, 1);

    let (conflict_status, conflict) = post(
        &service,
        br#"{"enable":"changed","observe_operation":true,"client_request_id":"swift-start-1"}"#,
    )
    .await;
    assert_eq!(conflict_status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "vowifi_request_id_conflict");

    let (query_status, query) = get(
        &service,
        "/api/reader/vowifi?observe_operation=true&client_request_id=swift-start-1",
    )
    .await;
    assert_eq!(query_status, StatusCode::OK);
    assert_eq!(query["operation"]["operation_id"], operation_id);
    assert_eq!(query["operation"]["client_request_id"], "swift-start-1");

    let reopened = observing(&service, snapshot(&service, false));
    reopened.0.state.lock().unwrap().requests =
        super::super::operation::RequestLedger::open(&service.0.resources.data_root, "reader")
            .unwrap();
    let (reopen_status, reopen_receipt) = post(&reopened, body).await;
    assert_eq!(reopen_status, StatusCode::ACCEPTED);
    assert_eq!(reopen_receipt["operation"]["operation_id"], operation_id);
    assert_eq!(reopen_receipt["operation"]["generation"], generation);
    assert_eq!(
        post(
            &reopened,
            br#"{"enable":false,"observe_operation":true,"client_request_id":"swift-start-1"}"#
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(reopened.record().unwrap().is_none());
    assert_eq!(reopened.0.state.lock().unwrap().next, 0);
    assert_eq!(
        service.0.state.lock().unwrap().next,
        1,
        "the only owner dispatch belongs to the first admitted request"
    );
    drop(reopened);

    service
        .stop_global(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn maintenance_only_blocks_start_before_no_sim_but_not_disable() {
    let fixture = tests::Fixture::new(false);
    let selected = fixture.service.0.resources.gate.selection_snapshot();
    let maintenance = fixture
        .service
        .0
        .resources
        .ordinary_admission
        .begin_reader_maintenance(
            &selected.reader,
            selected.generation,
            crate::ordinary_vowifi_admission::MaintenancePolicy::Reject,
        )
        .unwrap_or_else(|_| panic!("fixture maintenance"));
    let service = observing(&fixture.service, snapshot(&fixture.service, false));
    assert_eq!(
        post(
            &service,
            br#"{"observe_operation":true,"client_request_id":"startup-fixture","enable":true}"#
        )
        .await,
        (
            StatusCode::CONFLICT,
            json!({"error":"读卡器 VoWiFi 操作正在进行，请稍后重试"})
        )
    );
    assert_eq!(
        post(
            &service,
            br#"{"observe_operation":true,"client_request_id":"startup-fixture-disable","enable":false}"#
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert!(maintenance.is_current());
    assert!(service.record().unwrap().is_none());
    maintenance
        .release_confirmed()
        .unwrap_or_else(|_| panic!("actual empty fixture cleanup"));
    fixture.close().await;
}

#[tokio::test]
async fn idle_get_without_sim_keeps_required_swift_status_fields_without_start() {
    let fixture = tests::Fixture::new(false);
    let service = observing(&fixture.service, snapshot(&fixture.service, false));
    let response = service
        .handle_public(
            Request::builder()
                .method(hyper::Method::GET)
                .uri("/api/reader/vowifi")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["available"], false);
    assert_eq!(value["tunnel_state"], "disconnected");
    assert_eq!(value["ims_state"], "idle");
    assert_eq!(value["progress_step"], 0);
    assert_eq!(value["incoming_reachability"], "unverified");
    assert_eq!(value["message"], "读卡器未连接或无 SIM 卡");
    assert!(service.record().unwrap().is_none());
    fixture.close().await;
}

#[tokio::test]
async fn idle_get_with_sim_publishes_native_configuration_and_stable_imei_without_lease() {
    let fixture = tests::Fixture::new(false);
    let service = observing(&fixture.service, snapshot(&fixture.service, true));
    let before = service.0.resources.gate.selection_snapshot();
    let a = service.public_status().await;
    let b = service.public_status().await;
    assert_eq!(a["available"], true);
    assert_eq!(a["mcc"], "234");
    assert_eq!(a["mnc"], "33");
    assert_eq!(a["imsi"], "234330000000001");
    assert_eq!(a["tunnel_state"], "disconnected");
    assert_eq!(a["message"], "VoWiFi 未启动");
    assert_eq!(a["progress_step"], 0);
    assert_eq!(a["imei"].as_str().unwrap().len(), 15);
    assert_eq!(a["imei"], b["imei"]);
    assert_eq!(
        before.generation,
        service.0.resources.gate.selection_snapshot().generation
    );
    assert!(!service.0.resources.gate.native_reader_session_active());
    assert!(
        !service
            .0
            .resources
            .ordinary_admission
            .reader_maintenance_active()
    );
    assert!(service.record().unwrap().is_none());
    fixture.close().await;
}

#[tokio::test]
async fn durable_reader_service_reopen_replays_admitted_and_submitted_before_dispatch() {
    use super::super::operation::{Clock, Facts, Receipt, RequestLedger, request_fingerprint};
    for phase in ["admitted", "submitted"] {
        let fixture = tests::Fixture::new(false);
        let root = &fixture.service.0.resources.data_root;
        let bytes =
            br#"{"enable":true,"observe_operation":true,"client_request_id":"reader-crash-1"}"#;
        let now = tokio::time::Instant::now();
        let old = Clock::start(now, Duration::from_secs(90)).project(
            now,
            "old-reader",
            17,
            Facts {
                generation: 8,
                phase: phase.into(),
                ready: false,
                sms_ready: false,
                call_ready: false,
                cleanup_confirmed: false,
                superseded: false,
                error: None,
            },
        );
        let mut ledger = RequestLedger::open(root, "reader").unwrap();
        ledger
            .insert(Receipt::capture(
                "reader-crash-1",
                &request_fingerprint(bytes),
                &old,
            ))
            .unwrap();
        drop(ledger);
        let mut resources = fixture.service.0.resources.clone();
        resources.identity = Arc::new(SnapshotIdentity(Mutex::new(snapshot(
            &fixture.service,
            false,
        ))));
        let reopened = Service(Arc::new(Inner {
            sms_epoch: sms::new_epoch().unwrap(),
            resources,
            state: Mutex::new(State {
                requests: RequestLedger::open(root, "reader").unwrap(),
                ..State::default()
            }),
            mutation: tokio::sync::Mutex::new(()),
        }));
        let (status, replay) = post(&reopened, bytes).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(replay["operation"]["operation_id"], old.operation_id);
        assert_eq!(replay["operation"]["generation"], 8);
        assert_eq!(replay["operation"]["phase"], "unknown");
        assert_eq!(post(&reopened, br#"{"enable":false,"observe_operation":true,"client_request_id":"reader-crash-1"}"#).await.0, StatusCode::CONFLICT);
        assert!(reopened.record().unwrap().is_none());
        assert_eq!(
            reopened.0.state.lock().unwrap().next,
            0,
            "no worker dispatch on replay/conflict"
        );
        fixture.assert_no_dispatch();
        drop(reopened);
        fixture.close().await;
    }
}

#[tokio::test]
async fn public_reader_mutation_identity_bypasses_reject_before_spawn_stop_or_physical_dispatch() {
    let fixture = tests::Fixture::new(false);
    let service = observing(&fixture.service, snapshot(&fixture.service, true));
    for enable in [true, false] {
        for observe in [
            None,
            Some(json!(false)),
            Some(json!(null)),
            Some(json!(true)),
            Some(json!(1)),
            Some(json!("true")),
        ] {
            for identity in [
                None,
                Some(json!(null)),
                Some(json!("")),
                Some(json!("bad id")),
                Some(json!(7)),
                Some(json!("x".repeat(129))),
                Some(json!("swift-route-1")),
            ] {
                if observe == Some(json!(true)) && identity == Some(json!("swift-route-1")) {
                    continue;
                }
                let mut body = json!({"enable":enable});
                if let Some(observe) = &observe {
                    body["observe_operation"] = observe.clone();
                }
                if let Some(identity) = identity {
                    body["client_request_id"] = identity;
                }
                let (status, value) = post(&service, body.to_string().as_bytes()).await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {value}");
                assert_eq!(value["code"], "invalid_operation_observation");
                assert_eq!(
                    service.0.state.lock().unwrap().next,
                    0,
                    "dispatch_count must remain zero"
                );
                assert!(service.record().unwrap().is_none());
                assert!(!service.has_intent());
                fixture.assert_no_dispatch();
            }
        }
    }
    for body in [b"".as_slice(), b"null", b"[]"] {
        assert_eq!(post(&service, body).await.0, StatusCode::BAD_REQUEST);
    }
    assert_eq!(get(&service, "/api/reader/vowifi").await.0, StatusCode::OK);
    fixture.assert_no_dispatch();
    fixture.close().await;
}

// Raw fragments are intentional: a Value/map would discard duplicate fields
// and could hide the exact wire contract this regression protects.
const READER_ENABLE_WIRE_VECTORS: &[(&str, &str, bool)] = &[
    ("canonical-true", r#""enable":true"#, true),
    ("canonical-false", r#""enable":false"#, false),
    ("casefold-only", r#""ENABLE":true"#, true),
    ("mixed-case", r#""EnAbLe":false"#, false),
    ("last-false", r#""enable":true,"ENABLE":false"#, false),
    ("last-true", r#""ENABLE":false,"enable":true"#, true),
    ("same-key", r#""enable":true,"enable":false"#, false),
    ("null-skips-true", r#""enable":true,"ENABLE":null"#, true),
    ("null-then-true", r#""enable":null,"ENABLE":true"#, true),
    ("null-only", r#""enable":null"#, false),
    ("mixed-null-only", r#""ENABLE":null,"enable":null"#, false),
    ("missing", "", false),
];

const READER_INVALID_ENABLE_WIRE: &[&str] = &[
    r#""enable":"true","ENABLE":false"#,
    r#""enable":1"#,
    r#""ENABLE":[]"#,
    r#""enable":true,"ENABLE":{}"#,
    r#""enable":false,"ENABLE":"false","enable":true"#,
];

fn reader_enable_wire(fragment: &str, id: &str, proxy: &str) -> Vec<u8> {
    let comma = if fragment.is_empty() { "" } else { "," };
    format!(
        "{{{fragment}{comma}\"proxy\":{proxy},\"observe_operation\":true,\"client_request_id\":\"{id}\"}}"
    )
    .into_bytes()
}

#[test]
fn reader_requested_enable_uses_ordered_casefold_nullskip_default_false() {
    for &(name, fragment, expected) in READER_ENABLE_WIRE_VECTORS {
        let body = reader_enable_wire(fragment, name, r#""""#);
        assert_eq!(requested_enable(&body), Ok(expected), "{name}");
        assert_eq!(decode_start(&body).unwrap().0, expected, "{name}");
    }
    for fragment in READER_INVALID_ENABLE_WIRE {
        let body = reader_enable_wire(fragment, "invalid-enable", r#""""#);
        assert_eq!(requested_enable(&body), Err("请求格式错误"));
        assert!(matches!(decode_start(&body), Err("请求格式错误")));
    }
    // Preserve the original first-object/null decoder contract. Operation
    // options remain separately strict about observation and request identity.
    assert_eq!(requested_enable(b"null"), Ok(false));
    assert_eq!(requested_enable(br#"{"Enable":true} {}"#), Ok(true));
    assert!(requested_enable(br#"{"enable":["#).is_err());
}

#[tokio::test]
async fn reader_reconcile_wire_direction_survives_reopen_and_late_original_post_without_dispatch() {
    for &(name, fragment, expected) in READER_ENABLE_WIRE_VECTORS {
        let f = tests::Fixture::new(false);
        let service = observing_durable(&f.service, snapshot(&f.service, true));
        let id = format!("wire-reconcile-{name}");
        let body = reader_enable_wire(fragment, &id, r#""""#);
        let fingerprint = super::super::operation::request_fingerprint(&body);
        assert_eq!(decode_start(&body).unwrap().0, expected);
        let (status, response) = reconcile(&service, &body).await;
        assert_eq!(status, StatusCode::OK, "{name}: {response}");
        let operation = &response["operation"];
        assert_eq!(operation["client_request_id"], id);
        assert_eq!(operation["requested_enable"], expected, "{name}");
        assert_eq!(operation["phase"], "rejected");
        assert_eq!(operation["terminal"], true);
        assert_eq!(operation["succeeded"], false);
        assert_eq!(operation["cleanup_confirmed"], false);
        let path = service
            .0
            .resources
            .data_root
            .join("vowifi-reader-request-ledger.json");
        let durable = std::fs::read(&path).unwrap();
        let entries: serde_json::Value = serde_json::from_slice(&durable).unwrap();
        assert_eq!(entries[0]["fingerprint"], fingerprint);

        // Reopen the real durable ledger into a fresh service, rather than
        // merely consulting the original service's in-memory tombstone.
        let reopened = observing(&service, snapshot(&service, true));
        reopened.0.state.lock().unwrap().requests =
            super::super::operation::RequestLedger::open(&service.0.resources.data_root, "reader")
                .unwrap();
        let receipt = reopened
            .lookup_request(&id, Some(&fingerprint))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(receipt.resolve(None)).unwrap(),
            *operation
        );
        let (status, replay) = reconcile(&reopened, &body).await;
        assert_eq!(status, StatusCode::OK, "{name}");
        assert_eq!(replay["operation"], *operation);
        // The original POST is deliberately delivered only after durable
        // reconciliation/reopen. It cannot re-authorize start or stop work.
        let (status, late) = post(&reopened, &body).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{name}");
        assert_eq!(late["operation"], *operation);
        assert_eq!(std::fs::read(path).unwrap(), durable);
        assert!(reopened.record().unwrap().is_none());
        assert_eq!(reopened.0.state.lock().unwrap().next, 0);
        assert!(!reopened.0.resources.gate.native_reader_session_active());
        f.assert_no_dispatch();
        drop(reopened);
        drop(service);
        f.close().await;
    }
}

#[tokio::test]
async fn reader_invalid_enable_type_never_invents_terminal_receipt_or_dispatches() {
    for (index, fragment) in READER_INVALID_ENABLE_WIRE.iter().enumerate() {
        let f = tests::Fixture::new(false);
        let service = observing_durable(&f.service, snapshot(&f.service, true));
        let id = format!("invalid-wire-{index}");
        let body = reader_enable_wire(fragment, &id, r#""""#);
        let path = service
            .0
            .resources
            .data_root
            .join("vowifi-reader-request-ledger.json");
        let before = std::fs::read(&path).ok();
        let (status, response) = reconcile(&service, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(response["code"], "invalid_reconcile_payload");
        let (status, response) = post(&service, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(response["error"], "请求格式错误");
        assert!(service.lookup_request(&id, None).unwrap().is_none());
        let (status, response) = get(
            &service,
            &format!("/api/reader/vowifi?observe_operation=true&client_request_id={id}"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(response["code"], "vowifi_request_id_unknown");
        assert_eq!(std::fs::read(path).ok(), before);
        assert!(service.record().unwrap().is_none());
        f.assert_no_dispatch();
        drop(service);
        f.close().await;
    }
}

#[tokio::test]
async fn reader_connection_decode_rejection_uses_same_enable_direction_and_exact_receipt() {
    for (index, fragment, expected) in [
        (0, r#""enable":true,"ENABLE":false"#, false),
        (1, r#""enable":true,"ENABLE":null"#, true),
        (2, r#""ENABLE":true"#, true),
        (3, r#""enable":null"#, false),
        (4, "", false),
    ] {
        let f = tests::Fixture::new(false);
        let service = observing_durable(&f.service, snapshot(&f.service, true));
        let id = format!("bad-proxy-wire-{index}");
        let body = reader_enable_wire(fragment, &id, "7");
        assert!(matches!(decode_start(&body), Err("请求格式错误")));
        let (status, response) = post(&service, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(response["error"], "请求格式错误");
        let fingerprint = super::super::operation::request_fingerprint(&body);
        let reopened =
            super::super::operation::RequestLedger::open(&service.0.resources.data_root, "reader")
                .unwrap();
        let exact = serde_json::to_value(
            reopened
                .lookup(&id, Some(&fingerprint))
                .unwrap()
                .unwrap()
                .resolve(None),
        )
        .unwrap();
        assert_eq!(exact["requested_enable"], expected);
        assert_eq!(exact["phase"], "rejected");
        assert_eq!(exact["client_request_id"], id);
        let (status, response) = reconcile(&service, &body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["operation"], exact);
        let (status, response) = post(&service, &body).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["operation"], exact);
        assert!(service.record().unwrap().is_none());
        f.assert_no_dispatch();
        drop(service);
        f.close().await;
    }
}

#[tokio::test]
async fn reader_reconcile_duplicate_order_or_boolean_drift_conflicts_without_writes() {
    let f = tests::Fixture::new(false);
    let service = observing_durable(&f.service, snapshot(&f.service, true));
    let id = "wire-order-conflict";
    let body = reader_enable_wire(r#""enable":true,"ENABLE":false"#, id, r#""""#);
    let (status, original) = reconcile(&service, &body).await;
    assert_eq!(status, StatusCode::OK);
    let path = service
        .0
        .resources
        .data_root
        .join("vowifi-reader-request-ledger.json");
    let before = std::fs::read(&path).unwrap();
    for fragment in [r#""enable":false,"ENABLE":true"#, r#""enable":true"#] {
        let changed = reader_enable_wire(fragment, id, r#""""#);
        assert_ne!(
            super::super::operation::request_fingerprint(&body),
            super::super::operation::request_fingerprint(&changed)
        );
        for (status, response) in [
            reconcile(&service, &changed).await,
            post(&service, &changed).await,
        ] {
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(response["code"], "vowifi_request_id_conflict");
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    let (status, replay) = reconcile(&service, &body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["operation"], original["operation"]);
    f.assert_no_dispatch();
    drop(service);
    f.close().await;
}

// These stop tests use the real retained intent and Gate, held at the actual
// lease supplier's pre-admission boundary. No registered IMS/card IO is faked.
async fn held_reader_stop_fixture() -> (tests::Fixture, Service, Arc<Record>) {
    let fixture = tests::Fixture::new(true);
    let service = observing_durable(&fixture.service, snapshot(&fixture.service, true));
    let start = reader_enable_wire(r#""enable":true"#, "reader-stop-original", r#""""#);
    let (status, _) = post(&service, &start).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    fixture.entered(1).await;
    let record = service.record().unwrap().unwrap();
    assert!(record.claim.try_is_current().unwrap());
    assert!(!*record.stop.borrow());
    assert!(!record.task.lock().await.is_finished());
    // Settle the genuine original start observation before sampling disk.
    service
        .lookup_request("reader-stop-original", None)
        .unwrap();
    (fixture, service, record)
}

struct ResumeHeldReaderOnDrop<'a> {
    fixture: &'a tests::Fixture,
    service: &'a Service,
}
impl Drop for ResumeHeldReaderOnDrop<'_> {
    fn drop(&mut self) {
        // Also release the test hold during assertion unwind. This is only a
        // fixture signal; actual production joins remain awaited on success.
        self.service.begin_shutdown();
        self.fixture.resume();
    }
}

struct ReaderStopRootRestore {
    path: PathBuf,
    saved: PathBuf,
}
impl Drop for ReaderStopRootRestore {
    fn drop(&mut self) {
        if self.saved.exists() {
            std::fs::remove_file(&self.path).unwrap();
            std::fs::rename(&self.saved, &self.path).unwrap();
        }
    }
}

fn reader_stop_clock_sample(record: &Record, epoch: &str, now: tokio::time::Instant) -> Value {
    let clock = record.operation.lock().unwrap();
    let facts = |cleanup_confirmed| super::super::operation::Facts {
        generation: record.expected.reader_generation,
        phase: "clock-test".into(),
        ready: false,
        sms_ready: false,
        call_ready: false,
        cleanup_confirmed,
        superseded: false,
        error: None,
    };
    json!({
        "pending":clock.project(now, epoch, record.id, facts(false)),
        "after_old_start_deadline":clock.project(now + Duration::from_secs(120), epoch, record.id, facts(false)),
        "cleanup_outcome":clock.project(now, epoch, record.id, facts(true)),
        "stop":*record.stop.borrow(),
    })
}

async fn finish_held_reader_stop(fixture: tests::Fixture, service: Service, record: Arc<Record>) {
    service.begin_shutdown();
    fixture.resume();
    service
        .finish_before(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    assert!(!record.claim.try_is_current().unwrap());
    assert!(record.task.lock().await.is_finished());
    assert_eq!(fixture.dispatch_count(), 1);
    drop(record);
    drop(service);
    fixture.close().await;
}

#[tokio::test]
async fn reader_stop_before_commit_io_failure_preserves_live_clock_and_same_id_can_retry() {
    let (fixture, service, record) = held_reader_stop_fixture().await;
    let resume = ResumeHeldReaderOnDrop {
        fixture: &fixture,
        service: &service,
    };
    let now = tokio::time::Instant::now();
    let before = reader_stop_clock_sample(&record, &service.0.sms_epoch, now);
    let path = service
        .0
        .resources
        .data_root
        .join("vowifi-reader-request-ledger.json");
    let original = std::fs::read(&path).unwrap();
    let root = service.0.resources.data_root.clone();
    let saved = root.with_extension("reader-stop-io-held");
    let restore = ReaderStopRootRestore {
        path: root.clone(),
        saved: saved.clone(),
    };
    service.0.state.lock().unwrap().before_stop_insert = Some(Box::new(move |_| {
        // This runs only after the handler's initial healthy lookup and the
        // stop candidate was built. A pre-handler fault would test early exit.
        std::fs::rename(&root, &saved).unwrap();
        std::fs::write(&root, b"not-a-directory").unwrap();
    }));
    let body = reader_enable_wire(r#""enable":false"#, "reader-stop-before-io", r#""""#);
    let (status, response) = post(&service, &body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(response["code"], "vowifi_request_ledger_unavailable");
    assert!(restore.saved.is_dir(), "actual filesystem fault hook ran");
    drop(restore);
    assert_eq!(
        reader_stop_clock_sample(&record, &service.0.sms_epoch, now),
        before
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(
        service
            .lookup_request("reader-stop-before-io", None)
            .unwrap()
            .is_none()
    );
    assert!(Arc::ptr_eq(&service.record().unwrap().unwrap(), &record));
    assert!(record.claim.try_is_current().unwrap());
    assert_eq!(fixture.dispatch_count(), 1);
    let (status, response) = post(&service, &body).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(response["operation"]["requested_enable"], false);
    assert_eq!(record.operation.lock().unwrap().sequence, 1);
    assert!(*record.stop.borrow());
    drop(resume);
    finish_held_reader_stop(fixture, service, record).await;
}

#[tokio::test]
async fn reader_stop_committed_ack_loss_keeps_live_clock_and_exact_reopened_id_unknown() {
    for submitted in [false, true] {
        let (fixture, service, record) = held_reader_stop_fixture().await;
        let resume = ResumeHeldReaderOnDrop {
            fixture: &fixture,
            service: &service,
        };
        let now = tokio::time::Instant::now();
        let before = reader_stop_clock_sample(&record, &service.0.sms_epoch, now);
        {
            let mut state = service.0.state.lock().unwrap();
            let hook: Box<dyn FnOnce(&super::super::operation::RequestLedger) + Send> =
                Box::new(|ledger| ledger.test_fail_after_next_persist());
            if submitted {
                state.before_stop_submitted = Some(hook);
            } else {
                state.before_stop_insert = Some(hook);
            }
        }
        let body = reader_enable_wire(r#""enable":false"#, "reader-stop-ack-unknown", r#""""#);
        let (status, response) = post(&service, &body).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(response["code"], "vowifi_request_ledger_unavailable");
        assert_eq!(
            reader_stop_clock_sample(&record, &service.0.sms_epoch, now),
            before
        );
        assert!(record.claim.try_is_current().unwrap());
        assert!(Arc::ptr_eq(&service.record().unwrap().unwrap(), &record));
        assert_eq!(fixture.dispatch_count(), 1);
        let path = service
            .0
            .resources
            .data_root
            .join("vowifi-reader-request-ledger.json");
        let disk: Vec<Value> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let receipt = disk
            .iter()
            .find(|value| value["request_id"] == "reader-stop-ack-unknown")
            .unwrap();
        assert_eq!(
            receipt["snapshot"]["phase"],
            if submitted { "submitted" } else { "admitted" }
        );
        assert_eq!(receipt["requested_enable"], false);
        let committed = std::fs::read(&path).unwrap();
        assert_eq!(post(&service, &body).await.0, StatusCode::CONFLICT);
        assert_eq!(std::fs::read(&path).unwrap(), committed);
        assert_eq!(
            reader_stop_clock_sample(&record, &service.0.sms_epoch, now),
            before
        );
        let reopened =
            super::super::operation::RequestLedger::open(&service.0.resources.data_root, "reader")
                .unwrap();
        let unknown = reopened
            .lookup(
                "reader-stop-ack-unknown",
                Some(&super::super::operation::request_fingerprint(&body)),
            )
            .unwrap()
            .unwrap()
            .resolve(None);
        assert_eq!(unknown.phase, "unknown");
        assert_eq!(unknown.requested_enable, false);
        assert_eq!(
            unknown.client_request_id.as_deref(),
            Some("reader-stop-ack-unknown")
        );
        assert_eq!(
            unknown.operation_id,
            format!("{}-{}-1", service.0.sms_epoch, record.id)
        );
        assert!(!unknown.terminal && !unknown.cleanup_confirmed);
        assert_eq!(record.operation.lock().unwrap().sequence, 0);
        assert!(!*record.stop.borrow());
        drop(resume);
        finish_held_reader_stop(fixture, service, record).await;
    }
}

#[tokio::test]
async fn reader_stop_success_publishes_once_but_waiter_timeout_does_not_retire_held_owner() {
    let (fixture, service, record) = held_reader_stop_fixture().await;
    let resume = ResumeHeldReaderOnDrop {
        fixture: &fixture,
        service: &service,
    };
    let body = reader_enable_wire(r#""enable":false"#, "reader-stop-success", r#""""#);
    let (status, accepted) = post(&service, &body).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(accepted["operation"]["requested_enable"], false);
    assert_eq!(
        accepted["operation"]["operation_id"],
        format!("{}-{}-1", service.0.sms_epoch, record.id)
    );
    assert_eq!(record.operation.lock().unwrap().sequence, 1);
    assert!(*record.stop.borrow());
    let (status, replay) = post(&service, &body).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        replay["operation"]["operation_id"],
        accepted["operation"]["operation_id"]
    );
    assert_eq!(record.operation.lock().unwrap().sequence, 1);
    assert_eq!(fixture.dispatch_count(), 1);
    assert!(
        service
            .finish_before(tokio::time::Instant::now() + Duration::from_millis(20))
            .await
            .is_err()
    );
    assert!(record.claim.try_is_current().unwrap());
    assert!(!record.progress.borrow().cleanup_confirmed);
    assert!(Arc::ptr_eq(&service.record().unwrap().unwrap(), &record));
    assert!(service.0.resources.gate.native_reader_session_active());
    {
        // entered(1) proved the real supplier boundary was reached, and no
        // resume has been sent. The exact acquisition receipt is still owed.
        let current = record.driver.current.lock().unwrap();
        assert_eq!(current.key, Some((record.id, 0)));
        assert!(current.acquire_unconfirmed);
        assert!(current.handle.is_none() && current.unlaunched_lease.is_none());
        assert!(!current.acquisition_cleanup.as_ref().unwrap().confirmed());
    }
    // The detached exact-owner finish waiter may hold record.task while the
    // supplier is held. Do not await that mutex before releasing this hold.
    drop(resume);
    fixture.resume();
    service
        .finish_before(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    assert!(!record.claim.try_is_current().unwrap());
    assert!(record.progress.borrow().cleanup_confirmed);
    assert!(record.task.lock().await.is_finished());
    let (status, final_receipt) = get(
        &service,
        "/api/reader/vowifi?observe_operation=true&client_request_id=reader-stop-success",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(final_receipt["operation"]["outcome"], "cancelled");
    assert_eq!(final_receipt["operation"]["cleanup_confirmed"], true);
    assert_eq!(final_receipt["operation"]["requested_enable"], false);
    finish_held_reader_stop(fixture, service, record).await;
}

#[tokio::test]
async fn g3_reader_stop_control_ignores_broken_ledger_and_joins_only_the_exact_owner() {
    let (fixture, service, record) = held_reader_stop_fixture().await;
    let cleanup = ResumeHeldReaderOnDrop {
        fixture: &fixture,
        service: &service,
    };
    let target = record.control.target(
        "reader",
        &service.0.sms_epoch,
        record.expected.reader_generation,
    );
    let path = service
        .0
        .resources
        .data_root
        .join("vowifi-reader-request-ledger.json");
    let original = std::fs::read(&path).unwrap();
    let get = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/reader/vowifi/stop-control?{}",
            target.query_string()
        ))
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(service.handle_public(get).await.status(), StatusCode::OK);
    assert!(!*record.stop.borrow());
    assert!(!record.control.started());

    std::fs::write(&path, b"unreadable ledger retained by G3 fixture").unwrap();
    let broken = std::fs::read(&path).unwrap();
    let stop = reader_enable_wire(r#""enable":false"#, "g3-reader-normal-io-failed", r#""""#);
    assert_eq!(post(&service, &stop).await.0, StatusCode::CONFLICT);
    assert!(!*record.stop.borrow());
    let req = || {
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/reader/vowifi/stop-control?{}",
                target.query_string()
            ))
            .body(Full::new(Bytes::new()))
            .unwrap()
    };
    let response = service.handle_public(req()).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(
        *record.stop.borrow(),
        "signal is synchronous before observer return"
    );
    assert!(record.claim.try_is_current().unwrap());
    assert!(record.control.busy());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["stop_control"]["owner_join"], "pending");
    assert_eq!(value["stop_control"]["cleanup"], "unconfirmed");
    assert!(value.get("operation").is_none());
    service.handle_public(req()).await;
    assert_eq!(
        std::fs::read(&path).unwrap(),
        broken,
        "temporary control never writes ledger bytes"
    );
    fixture.resume();
    tokio::time::timeout(Duration::from_secs(3), async {
        while record.control.busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!record.claim.try_is_current().unwrap());
    let result = record.control.project(target.clone()).unwrap();
    let value = serde_json::to_value(result).unwrap();
    assert_eq!(value["owner_join"], "joined");
    assert_eq!(value["cleanup"], "confirmed");
    // Restore only this fixture's fault, then use the actual start path to
    // publish a live successor and populate the bounded retired completion.
    std::fs::write(&path, original).unwrap();
    fixture.hold();
    let start = reader_enable_wire(r#""enable":true"#, "g3-reader-successor", r#""""#);
    assert_eq!(post(&service, &start).await.0, StatusCode::ACCEPTED);
    fixture.entered(2).await;
    let successor = service.record().unwrap().unwrap();
    assert!(!Arc::ptr_eq(&successor, &record));
    assert!(!*successor.stop.borrow());
    let before_old_retry = std::fs::read(&path).unwrap();
    assert_eq!(service.handle_public(req()).await.status(), StatusCode::OK);
    assert!(
        !*successor.stop.borrow(),
        "retired POST never stops the live successor"
    );
    let mut stale = target.clone();
    stale.owner_generation += 1;
    let request = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/reader/vowifi/stop-control?{}",
            stale.query_string()
        ))
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(
        service.handle_public(request).await.status(),
        StatusCode::CONFLICT
    );
    assert!(!*successor.stop.borrow());
    assert_eq!(std::fs::read(&path).unwrap(), before_old_retry);
    drop(cleanup);
    service
        .finish_before(tokio::time::Instant::now() + Duration::from_secs(3))
        .await
        .unwrap();
    assert!(!successor.claim.try_is_current().unwrap());
    drop(successor);
    drop(record);
    drop(service);
    fixture.close().await;
}

#[tokio::test]
async fn g3_reader_100k_32mib_and_injected_storage_failures_preserve_stop_and_history() {
    for fault in [
        "count",
        "bytes",
        "readonly-injected",
        "enospc-injected",
        "rename-ack-loss",
    ] {
        let (fixture, service, record) = held_reader_stop_fixture().await;
        let resume = ResumeHeldReaderOnDrop {
            fixture: &fixture,
            service: &service,
        };
        let target = record.control.target(
            "reader",
            &service.0.sms_epoch,
            record.expected.reader_generation,
        );
        let path = service
            .0
            .resources
            .data_root
            .join("vowifi-reader-request-ledger.json");
        let original = std::fs::read(&path).unwrap();
        match fault {
            "count" => {
                assert_eq!(
                    service
                        .0
                        .state
                        .lock()
                        .unwrap()
                        .requests
                        .test_fill_capacity(),
                    100_000
                );
            }
            "bytes" => {
                std::fs::write(&path, vec![b' '; 32 * 1024 * 1024 + 1]).unwrap();
            }
            "readonly-injected" => service
                .0
                .state
                .lock()
                .unwrap()
                .requests
                .test_write_fault(std::io::ErrorKind::ReadOnlyFilesystem),
            "enospc-injected" => service
                .0
                .state
                .lock()
                .unwrap()
                .requests
                .test_write_fault(std::io::ErrorKind::StorageFull),
            _ => {
                service.0.state.lock().unwrap().before_stop_insert =
                    Some(Box::new(|ledger| ledger.test_fail_after_next_persist()));
            }
        }
        let stop = reader_enable_wire(r#""enable":false"#, "g3-reader-denied-stop", r#""""#);
        assert_eq!(
            post(&service, &stop).await.0,
            StatusCode::CONFLICT,
            "{fault}"
        );
        assert!(!*record.stop.borrow());
        let before = std::fs::read(&path).unwrap();
        let request = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/reader/vowifi/stop-control?{}",
                target.query_string()
            ))
            .body(Full::new(Bytes::new()))
            .unwrap();
        assert_eq!(
            service.handle_public(request).await.status(),
            StatusCode::ACCEPTED,
            "{fault}"
        );
        assert!(*record.stop.borrow());
        assert!(record.claim.try_is_current().unwrap());
        fixture.resume();
        tokio::time::timeout(Duration::from_secs(3), async {
            while record.control.busy() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "no durable writes during fallback {fault}"
        );
        if fault == "count" {
            assert_eq!(
                service.0.state.lock().unwrap().requests.test_count(),
                100_000
            );
        }
        if fault == "rename-ack-loss" {
            let reopened = super::super::operation::RequestLedger::open(
                &service.0.resources.data_root,
                "reader",
            )
            .unwrap();
            let receipt = reopened
                .lookup("g3-reader-denied-stop", None)
                .unwrap()
                .unwrap()
                .resolve(None);
            assert_eq!(receipt.outcome.as_deref(), Some("unknown"));
            assert!(!receipt.succeeded && !receipt.cleanup_confirmed && !receipt.terminal);
        }
        // Restore only this fixture's faulted bytes for independent teardown;
        // no production recovery/ledger cleanup path performs this operation.
        if fault == "bytes" {
            std::fs::write(&path, original).unwrap();
        }
        drop(resume);
        drop(record);
        drop(service);
        fixture.close().await;
    }
}

#[tokio::test]
async fn g3_reader_head_current_retired_stale_invalid_never_signals_or_writes() {
    let (fixture, service, record) = held_reader_stop_fixture().await;
    let root = service.0.resources.data_root.join("g3-head-server");
    let mut server = crate::reader_pcsc_gateway_tests::state(&root);
    let lifecycle = service.0.resources.lifecycle.upgrade().unwrap();
    server.native_vowifi_lifecycle = lifecycle.clone();
    assert!(lifecycle.ordinary_service.set(Ok(service.clone())).is_ok());
    let target = record.control.target(
        "reader",
        &service.0.sms_epoch,
        record.expected.reader_generation,
    );
    let path = service
        .0
        .resources
        .data_root
        .join("vowifi-reader-request-ledger.json");
    let before = std::fs::read(&path).unwrap();
    let query = target.query_string();
    for (query, status) in [
        (query.clone(), StatusCode::OK),
        (
            query.replace(&target.daemon_epoch, "stale-epoch"),
            StatusCode::CONFLICT,
        ),
        (
            query.replace(
                &format!("owner_generation={}", target.owner_generation),
                "owner_generation=01",
            ),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let observed = record.stop.subscribe();
        assert_eq!(
            super::super::stop_control::test_head_request(
                server.clone(),
                &format!("/api/reader/vowifi/stop-control?{query}")
            )
            .await,
            status
        );
        assert!(!observed.has_changed().unwrap());
        assert!(!*record.stop.borrow());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/reader/vowifi/stop-control?{}",
            target.query_string()
        ))
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(
        service.handle_public(req).await.status(),
        StatusCode::ACCEPTED
    );
    fixture.resume();
    tokio::time::timeout(Duration::from_secs(2), async {
        while record.control.busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    {
        let mut state = service.0.state.lock().unwrap();
        state.last_stop = Some((target.clone(), record.control.clone()));
        state.record = None;
    }
    let observed = record.stop.subscribe();
    assert_eq!(
        super::super::stop_control::test_head_request(
            server.clone(),
            &format!("/api/reader/vowifi/stop-control?{}", target.query_string())
        )
        .await,
        StatusCode::OK
    );
    assert!(!observed.has_changed().unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    server.sms_forwarding.shutdown().await.unwrap();
    server.reader_pcsc.shutdown().await.unwrap();
    drop(server);
    drop(record);
    drop(service);
    fixture.close().await;
}
