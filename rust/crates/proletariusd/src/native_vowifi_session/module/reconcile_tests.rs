//! Reconciliation exercises real module admission/ledger paths with every
//! physical backend unavailable. No modem, card, network or installed app.
use super::*;

struct Fixture {
    root: PathBuf,
    server: crate::ServerState,
    service: Service,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "module-reconcile-{}",
            crate::snapshot::new_instance_id()
        ));
        let mut server = crate::reader_pcsc_gateway_tests::state(&root);
        server.usb_at = crate::usb_at::Owner::unavailable_for_tests("reconcile-no-device".into());
        server.native_vowifi_lifecycle.activate().unwrap();
        let service = server
            .native_vowifi_lifecycle
            .module_service(&server)
            .unwrap();
        Self {
            root,
            server,
            service,
        }
    }
    fn assert_no_dispatch(&self) {
        let state = self.service.0.state.lock().unwrap();
        assert_eq!(state.next, 0);
        assert!(state.record.is_none());
        assert!(!self.service.0.admission.has_ordinary_claim());
    }
    async fn close(self) {
        self.server
            .native_vowifi_lifecycle
            .finish_shutdown(tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .unwrap();
        drop(self.service);
        drop(self.server);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
fn payload(id: &str, enable: bool) -> Vec<u8> {
    serde_json::to_vec(
        &json!({"enable":enable,"proxy_host":"frozen.proxy.example","proxy_port":"1080",
        "observe_operation":true,"client_request_id":id}),
    )
    .unwrap()
}
async fn call(service: &Service, path: &str, bytes: &[u8]) -> (StatusCode, Value) {
    let response = service
        .handle(
            Request::builder()
                .method("POST")
                .uri(path)
                .body(Full::new(Bytes::copy_from_slice(bytes)))
                .unwrap(),
        )
        .await;
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}
async fn original(service: &Service, bytes: &[u8]) -> (StatusCode, Value) {
    call(service, "/api/vowifi", bytes).await
}
async fn reconcile(service: &Service, bytes: &[u8]) -> (StatusCode, Value) {
    call(service, "/api/vowifi/reconcile", bytes).await
}

#[test]
fn module_reconcile_route_is_rust_owned_post_only() {
    use crate::public_http::{Admission, admission};
    assert_eq!(
        admission(&hyper::Method::POST, "/api/vowifi/reconcile"),
        Admission::Allowed
    );
    assert_eq!(
        admission(&hyper::Method::GET, "/api/vowifi/reconcile"),
        Admission::Unknown
    );
    assert_eq!(
        crate::ownership::route_owner("POST", "/api/vowifi/reconcile"),
        crate::ownership::RouteOwner::Rust
    );
}

#[tokio::test]
async fn clean_module_reconcile_commits_one_exact_durable_tombstone_and_blocks_late_post() {
    let f = Fixture::new();
    let bytes = payload("module-missing-original", true);
    let (status, value) = reconcile(&f.service, &bytes).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["operation"]["phase"], "rejected");
    assert_eq!(value["operation"]["requested_enable"], true);
    assert_eq!(value["operation"]["terminal"], true);
    assert_eq!(value["operation"]["succeeded"], false);
    assert_eq!(value["operation"]["cleanup_confirmed"], false);
    let saved = super::super::operation::RequestLedger::open(&f.service.0.root, "module")
        .unwrap()
        .lookup("module-missing-original", None)
        .unwrap()
        .unwrap()
        .resolve(None);
    assert_eq!(serde_json::to_value(saved).unwrap(), value["operation"]);
    let (status, repeated) = reconcile(&f.service, &bytes).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated, value);
    let (status, posted) = original(&f.service, &bytes).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(posted["operation"], value["operation"]);
    let altered = payload("module-missing-original", false);
    assert_eq!(
        reconcile(&f.service, &altered).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(original(&f.service, &altered).await.0, StatusCode::CONFLICT);
    f.assert_no_dispatch();
    f.close().await;
}

#[tokio::test]
async fn module_preadmission_failures_are_durable_and_do_not_borrow_an_owner() {
    for scenario in [
        "invalid_proxy",
        "raw_epdg_ip",
        "transport_recovering",
        "stopping",
        "admission_busy",
        "stale_observation",
    ] {
        let f = Fixture::new();
        let id = format!("module-rejected-{scenario}");
        let mut value = json!({"enable":true,"observe_operation":true,"client_request_id":id});
        let mut held_claim = None;
        match scenario {
            "invalid_proxy" => value["proxy"] = json!("invalid"),
            "raw_epdg_ip" => value["epdg_ip_address"] = json!("198.51.100.1"),
            "transport_recovering" => f.service.pause_transport(),
            "stopping" => f.service.begin_shutdown(),
            "admission_busy" => {
                held_claim = Some(
                    f.service
                        .0
                        .admission
                        .acquire_native("other-reader", 42)
                        .unwrap(),
                )
            }
            "stale_observation" => {
                value["operation_id"] = json!("stale-owner");
                value["operation_generation"] = json!(81);
            }
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        let (status, _) = original(&f.service, &bytes).await;
        assert_eq!(
            status,
            if matches!(scenario, "invalid_proxy" | "raw_epdg_ip") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::CONFLICT
            },
            "{scenario}"
        );
        let (status, reconciled) = reconcile(&f.service, &bytes).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "known rejection does not need clean-owner proof: {scenario}"
        );
        assert_eq!(reconciled["operation"]["client_request_id"], id);
        assert_eq!(reconciled["operation"]["phase"], "rejected");
        assert_eq!(reconciled["operation"]["terminal"], true);
        assert_eq!(reconciled["operation"]["generation"], 0);
        let reopened =
            super::super::operation::RequestLedger::open(&f.service.0.root, "module").unwrap();
        assert_eq!(
            reopened
                .lookup(&id, None)
                .unwrap()
                .unwrap()
                .resolve(None)
                .phase,
            "rejected"
        );
        assert_eq!(
            original(&f.service, &bytes).await.1["operation"],
            reconciled["operation"]
        );
        if let Some(claim) = held_claim {
            claim.release_confirmed().unwrap();
        }
        f.assert_no_dispatch();
        f.close().await;
    }
}

#[tokio::test]
async fn module_reconcile_preserves_existing_recovered_unknown_without_dispatch() {
    let f = Fixture::new();
    let bytes = payload("module-admitted-before-restart", true);
    let options = super::super::operation::Options::body(&bytes).unwrap();
    {
        let mut state = f.service.0.state.lock().unwrap();
        let now = tokio::time::Instant::now();
        let receipt = super::super::operation::Clock::start(now, Duration::from_secs(90)).receipt(
            options.request_id().unwrap(),
            &super::super::operation::request_fingerprint(&bytes),
            "old-owner",
            4,
            4,
        );
        state.requests.insert(receipt).unwrap();
        state.requests =
            super::super::operation::RequestLedger::open(&f.service.0.root, "module").unwrap();
    }
    // Another admitted owner excludes absence, but an exact historical receipt
    // must still be returned with its own ID and nonterminal uncertainty.
    let claim = f
        .service
        .0
        .admission
        .acquire_native("successor", 9)
        .unwrap();
    let (status, value) = reconcile(&f.service, &bytes).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["operation"]["phase"], "unknown");
    assert_eq!(value["operation"]["terminal"], false);
    assert_eq!(value["operation"]["generation"], 4);
    assert!(
        value["operation"]["operation_id"]
            .as_str()
            .unwrap()
            .starts_with("old-owner-")
    );
    claim.release_confirmed().unwrap();
    f.assert_no_dispatch();
    f.close().await;
}

#[tokio::test]
async fn module_missing_id_requires_clean_active_lifecycle_and_complete_ledger() {
    for scenario in [
        "busy",
        "lifecycle_cleanup",
        "finishing",
        "transport",
        "nondurable",
        "missing_archive",
        "corrupt_archive",
    ] {
        let f = Fixture::new();
        let bytes = payload(&format!("orphan-{scenario}"), true);
        let mut held_claim = None;
        match scenario {
            "busy" => {
                held_claim = Some(
                    f.service
                        .0
                        .admission
                        .acquire_native("other-owner", 1)
                        .unwrap(),
                )
            }
            "lifecycle_cleanup" => {
                f.server
                    .native_vowifi_lifecycle
                    .state
                    .lock()
                    .unwrap()
                    .cleanup_failure = Some("fixture-pending")
            }
            "finishing" => {
                f.server
                    .native_vowifi_lifecycle
                    .state
                    .lock()
                    .unwrap()
                    .finishing = true
            }
            "transport" => f.service.pause_transport(),
            "nondurable" => f.service.0.state.lock().unwrap().requests = Default::default(),
            "missing_archive" | "corrupt_archive" => {
                // First establish the nonempty-ledger watermark, then remove
                // or corrupt only the body. Neither can prove historical absence.
                assert_eq!(
                    reconcile(&f.service, &payload("retained-history", false))
                        .await
                        .0,
                    StatusCode::OK
                );
                let path = f.service.0.root.join("vowifi-module-request-ledger.json");
                if scenario == "missing_archive" {
                    std::fs::remove_file(path).unwrap();
                } else {
                    std::fs::write(path, b"[]").unwrap();
                }
            }
            _ => unreachable!(),
        }
        let (status, value) = reconcile(&f.service, &bytes).await;
        assert_eq!(status, StatusCode::CONFLICT, "{scenario}: {value}");
        assert!(matches!(
            value["code"].as_str().unwrap(),
            "vowifi_reconcile_owner_unconfirmed" | "vowifi_request_ledger_unavailable"
        ));
        assert!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .requests
                .lookup(&format!("orphan-{scenario}"), None)
                .ok()
                .flatten()
                .is_none()
        );
        if let Some(claim) = held_claim {
            claim.release_confirmed().unwrap();
        }
        {
            let mut state = f.server.native_vowifi_lifecycle.state.lock().unwrap();
            state.cleanup_failure = None;
            state.finishing = false;
        }
        f.assert_no_dispatch();
        f.close().await;
    }
}

#[tokio::test]
async fn concurrent_module_disable_post_and_reconcile_share_one_receipt() {
    let f = Fixture::new();
    let bytes = payload("concurrent-safe-disable", false);
    let (posted, reconciled) =
        tokio::join!(original(&f.service, &bytes), reconcile(&f.service, &bytes));
    assert_eq!(posted.0, StatusCode::ACCEPTED);
    assert_eq!(reconciled.0, StatusCode::OK);
    assert_eq!(
        posted.1["operation"]["operation_id"],
        reconciled.1["operation"]["operation_id"]
    );
    assert_eq!(
        posted.1["operation"]["client_request_id"],
        "concurrent-safe-disable"
    );
    f.assert_no_dispatch();
    f.close().await;
}

#[tokio::test]
async fn module_reconcile_dispatches_through_real_public_route() {
    let f = Fixture::new();
    let (status, value) = crate::reader_pcsc_gateway_tests::request_with_body(
        f.server.clone(),
        "/api/vowifi/reconcile",
        true,
        false,
        Some(("x-proletarius-app-token", "app-fixture")),
        &payload("real-public-module-reconcile", true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["operation"]["phase"], "rejected");
    f.assert_no_dispatch();
    f.close().await;
}

#[test]
fn requested_enable_matches_ordered_casefold_null_and_invalid_type_semantics() {
    for (body, expected) in [
        (r#"{"enable":false,"Enable":true}"#, Some(true)),
        (r#"{"ENABLE":true}"#, Some(true)),
        (r#"{"enable":false,"enable":true}"#, Some(true)),
        (r#"{"enable":null,"eNaBlE":true}"#, Some(true)),
        (r#"{"ENABLE":true,"enable":false}"#, Some(false)),
        (
            r#"{"enable":true,"enable":null,"ENABLE":false}"#,
            Some(false),
        ),
        (r#"{"enable":true,"Enable":null}"#, None),
        (r#"{"enable":"true","Enable":true}"#, None),
        (r#"{"enable":true,"Enable":"false"}"#, None),
        (r#"{"enable":1}"#, None),
        (r#"{"enable":null}"#, None),
        (r#"{}"#, None),
    ] {
        assert_eq!(
            request::requested_enable(body.as_bytes()).ok(),
            expected,
            "{body}"
        );
        assert_eq!(
            request::decode(body.as_bytes())
                .ok()
                .map(|value| value.enable),
            expected,
            "{body}"
        );
    }
    // Normalization remains a separate strict validation step. Extracting a
    // true intent must never turn an invalid connection into an accepted start.
    let invalid = br#"{"enable":false,"Enable":true,"proxy":"invalid"}"#;
    assert_eq!(request::requested_enable(invalid), Ok(true));
    assert!(request::decode(invalid).is_err());
}

#[tokio::test]
async fn module_rejection_and_reconcile_keep_the_ordered_requested_enable() {
    for (index, fields, expected) in [
        (0, r#""enable":false,"Enable":true"#, true),
        (1, r#""ENABLE":true"#, true),
        (2, r#""enable":false,"enable":true"#, true),
        (3, r#""enable":null,"eNaBlE":true"#, true),
        (4, r#""ENABLE":true,"enable":false"#, false),
        (5, r#""enable":true,"enable":null,"ENABLE":false"#, false),
    ] {
        let f = Fixture::new();
        let id = format!("ordered-rejection-{index}");
        let bytes = format!(
            r#"{{{fields},"proxy":"invalid","observe_operation":true,"client_request_id":"{id}"}}"#
        );
        // Disable intentionally bypasses connection normalization in the legacy
        // decoder; force its own pre-admission rejection without any card work.
        f.service.begin_shutdown();
        let (status, _) = original(&f.service, bytes.as_bytes()).await;
        assert_eq!(
            status,
            if expected {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::CONFLICT
            }
        );
        let (status, value) = reconcile(&f.service, bytes.as_bytes()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["operation"]["requested_enable"], expected);
        assert_eq!(value["operation"]["phase"], "rejected");
        let saved = super::super::operation::RequestLedger::open(&f.service.0.root, "module")
            .unwrap()
            .lookup(&id, None)
            .unwrap()
            .unwrap()
            .resolve(None);
        assert_eq!(saved.requested_enable, expected);
        f.assert_no_dispatch();
        f.close().await;

        // Missing-receipt reconciliation independently parses the same raw
        // field order. Invalid proxy bytes remain undispatched and untouched.
        let clean = Fixture::new();
        let (status, value) = reconcile(&clean.service, bytes.as_bytes()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["operation"]["requested_enable"], expected);
        assert_eq!(value["operation"]["phase"], "rejected");
        assert_eq!(
            original(&clean.service, bytes.as_bytes()).await.1["operation"],
            value["operation"]
        );
        clean.assert_no_dispatch();
        clean.close().await;
    }
}

#[tokio::test]
async fn invalid_or_cleared_enable_never_invents_a_terminal_rejection_identity() {
    for (index, fields) in [
        r#""enable":true,"Enable":null"#,
        r#""enable":"true","Enable":true"#,
        r#""enable":true,"Enable":"false"#,
        r#""enable":1"#,
        r#""enable":null"#,
    ]
    .into_iter()
    .enumerate()
    {
        let f = Fixture::new();
        let id = format!("uncertified-enable-{index}");
        let bytes = format!(
            r#"{{{fields},"proxy":"invalid","observe_operation":true,"client_request_id":"{id}"}}"#
        );
        assert_eq!(
            original(&f.service, bytes.as_bytes()).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            reconcile(&f.service, bytes.as_bytes()).await.0,
            StatusCode::BAD_REQUEST
        );
        assert!(
            f.service.lookup_request(&id, None).unwrap().is_none(),
            "invalid enable invented a bool: {fields}"
        );
        f.assert_no_dispatch();
        f.close().await;
    }
}

#[tokio::test]
async fn module_body_deadline_rejects_unfinished_ingress_before_receipt_or_owner() {
    // This body is deliberately !Unpin. The first frame already contains valid
    // JSON, but missing HTTP-body completion cannot admit any operation.
    struct UnfinishedBody {
        first: Mutex<Option<Bytes>>,
        _pinned: std::marker::PhantomPinned,
    }
    impl hyper::body::Body for UnfinishedBody {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            match self.as_ref().get_ref().first.lock().unwrap().take() {
                Some(bytes) => std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes)))),
                None => std::task::Poll::Pending,
            }
        }
    }
    let f = Fixture::new();
    let id = "module-incomplete-body";
    let response = tokio::time::timeout(
        Duration::from_secs(7),
        f.service.handle(
            Request::builder()
                .method("POST")
                .uri("/api/vowifi")
                .body(UnfinishedBody {
                    first: Mutex::new(Some(Bytes::from(payload(id, true)))),
                    _pinned: std::marker::PhantomPinned,
                })
                .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    let value: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(value["code"], "request_body_timeout");
    assert!(f.service.lookup_request(id, None).unwrap().is_none());
    f.assert_no_dispatch();
    f.close().await;
}

#[tokio::test]
async fn module_reconcile_ready_empty_ingress_never_records_or_dispatches() {
    struct ReadyEmpty {
        first: Mutex<Option<Bytes>>,
        polls: Arc<std::sync::atomic::AtomicUsize>,
        _pinned: std::marker::PhantomPinned,
    }
    impl hyper::body::Body for ReadyEmpty {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            let this = self.as_ref().get_ref();
            this.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let bytes = this.first.lock().unwrap().take().unwrap_or_default();
            std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes))))
        }
    }
    let f = Fixture::new();
    let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let response = tokio::time::timeout(
        Duration::from_secs(4),
        f.service.handle(
            Request::builder()
                .method("POST")
                .uri("/api/vowifi/reconcile")
                .body(ReadyEmpty {
                    first: Mutex::new(Some(Bytes::from(payload("module-ready-empty-body", true)))),
                    polls: polls.clone(),
                    _pinned: std::marker::PhantomPinned,
                })
                .unwrap(),
        ),
    )
    .await
    .expect("ready empty frames cannot bypass the original two-second deadline");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let value: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(value["code"], "invalid_reconcile_payload");
    assert!(polls.load(std::sync::atomic::Ordering::SeqCst) > 64);
    assert!(
        f.service
            .lookup_request("module-ready-empty-body", None)
            .unwrap()
            .is_none()
    );
    f.assert_no_dispatch();
    f.close().await;
}
