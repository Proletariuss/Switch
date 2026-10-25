//! Public UDS dispatch with a native PC/SC owner and scripted card replies.
//! The Go trap deliberately returns success: any compatibility request is a
//! test failure even when its fabricated response would otherwise look valid.
use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub(super) struct GoTrap {
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl GoTrap {
    pub(super) fn install(state: &mut crate::ServerState, root: &std::path::Path) -> Self {
        let socket = root.join("go-trap.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        state.go_socket_path = socket;
        // Make the trap available even to a regressed late dispatch path.
        state.availability.mark_ready();
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let observed = observed.clone();
                let service = service_fn(move |_request: Request<hyper::body::Incoming>| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    async move {
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(
                            br#"{"card_type":"physical_sim","state":"physical_sim","operation":null,"go_trap":true}"#,
                        ))))
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            }
        });
        Self { requests, task }
    }

    pub(super) async fn assert_unused(self) {
        self.task.abort();
        let _ = self.task.await;
        assert_eq!(self.requests.load(Ordering::SeqCst), 0, "Go was contacted");
    }
}

async fn public_request(
    state: crate::ServerState,
    method: &str,
    path: &str,
    body: &[u8],
) -> (StatusCode, serde_json::Value) {
    let (client, server) = UnixStream::pair().unwrap();
    let server = tokio::spawn(async move {
        if let Err(error) = crate::serve_connection(server, state).await {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        sender.send_request(
            Request::builder()
                .method(method)
                .uri(path)
                .header(crate::APP_TOKEN_HEADER, "app-fixture")
                .header("content-type", "application/json")
                .body(Full::new(Bytes::copy_from_slice(body)))
                .unwrap(),
        ),
    )
    .await
    .expect("native reader request waited for a compatibility backend")
    .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    server.await.unwrap();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

fn select_reader(state: &crate::ServerState, reader: &str) {
    state.reader_gate.test_seed_selection(reader, 1);
    state.reader_discovery.lock().unwrap().reconcile(
        vec![proletarius_reader::ReaderObservation {
            name: reader.into(),
            card: proletarius_reader::CardPresence::Present,
            in_use: false,
            muted: false,
            atr: Vec::new(),
        }],
        Some(reader),
    );
}

#[tokio::test]
async fn reader_routes_without_selection_reject_locally_before_body_or_go() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let root = crate::tests::test_directory("reader-native-unselected-routes");
    let mut state = state(&root);
    let trap = GoTrap::install(&mut state, &root);
    for (method, path) in [
        ("GET", "/api/reader/esim"),
        ("GET", "/api/reader/esim/health"),
        ("POST", "/api/reader/esim/download"),
        ("PATCH", "/api/reader/esim/profile"),
        ("DELETE", "/api/reader/esim/profile"),
        ("POST", "/api/reader/esim/switch"),
        ("GET", "/api/reader/esim/notifications"),
    ] {
        let (status, body) = public_request(state.clone(), method, path, b"not-json").await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {path}: {body}"
        );
        assert_eq!(body["code"], "reader_esim_unavailable", "{path}");
        assert_eq!(body["retryable"], true, "{path}");
    }
    trap.assert_unused().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn notification_retry_without_selection_precedes_body_and_receipt_store() {
    let root = crate::tests::test_directory("reader-notification-retry-unselected");
    let mut state = state(&root);
    let trap = GoTrap::install(&mut state, &root);
    let (status, body) = public_request(
        state,
        "POST",
        "/api/reader/esim/notifications/retry",
        b"not-json",
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "reader_esim_unavailable");
    assert_eq!(body["retryable"], true);
    trap.assert_unused().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn bound_ble_selection_admits_download_before_inventory_refresh() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let root = crate::tests::test_directory("reader-ble-selection-admission");
    let state = state(&root);
    let reader = "BLE:12345678-1234-1234-1234-123456789abc";
    let route = "/api/reader/esim/download";

    state.reader_gate.test_seed_ble_selection(reader, 1, None);
    let (status, body) = public_request(state.clone(), "POST", route, b"not-json").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "reader_esim_unavailable");

    let binding = crate::reader_ble_apdu::ConnectionBinding {
        reader: reader.into(),
        bridge_id: "fixture-bridge".into(),
        connection_id: "fixture-connection".into(),
        close_operation_id: "fixture-close".into(),
    };
    state
        .reader_gate
        .test_seed_ble_selection(reader, 2, Some(binding));
    assert!(
        state
            .reader_discovery
            .lock()
            .unwrap()
            .snapshot()
            .readers
            .is_empty()
    );
    let (status, body) = public_request(state.clone(), "POST", route, b"not-json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request_body");

    state.reader_gate.test_seed_ble_selection(reader, 3, None);
    let (status, body) = public_request(state.clone(), "POST", route, b"not-json").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "reader_esim_unavailable");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reader_operation_and_unmatched_confirmation_work_while_go_is_absent() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let root = crate::tests::test_directory("reader-native-operation-no-go");
    let state = state(&root);
    assert!(!state.go_socket_path.exists());
    let (status, before) =
        public_request(state.clone(), "GET", "/api/reader/esim/operation", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert!(before.get("operation").is_some());
    let (status, body) = public_request(
        state.clone(),
        "POST",
        "/api/reader/esim/download/confirm",
        br#"{"operation_id":"not-an-active-reader-gateway-operation","confirm":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "esim_download_confirmation_expired");
    let (status, after) =
        public_request(state.clone(), "GET", "/api/reader/esim/operation", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        after, before,
        "expired confirmation must not modify the local operation"
    );
    let (status, head) = public_request(state, "HEAD", "/api/reader/esim/operation", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert!(head.is_null());
    std::fs::remove_dir_all(root).unwrap();
}

type ReleaseBarrier = (
    tokio::sync::oneshot::Sender<()>,
    std::sync::mpsc::Receiver<()>,
);
struct CardScript {
    replies: VecDeque<Vec<u8>>,
    commands: Vec<Vec<u8>>,
    release: Option<ReleaseBarrier>,
}
struct ScriptedPhysical(Arc<Mutex<CardScript>>);
impl proletarius_sim::ApduTransport for ScriptedPhysical {
    type Error = crate::reader_pcsc::Failure;
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
        let mut script = self.0.lock().unwrap();
        script.commands.push(command.to_vec());
        Ok(script
            .replies
            .pop_front()
            .expect("unexpected physical APDU"))
    }
}
impl crate::reader_pcsc::Physical for ScriptedPhysical {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(
            &mut dyn proletarius_sim::ApduTransport<Error = crate::reader_pcsc::Failure>,
        ),
    ) -> Result<(), crate::reader_pcsc::Failure> {
        body(self);
        let release = self.0.lock().unwrap().release.take();
        if let Some((pending, allow)) = release {
            let _ = pending.send(());
            allow
                .recv_timeout(crate::esim_refresh::WAITER_TIMEOUT + Duration::from_secs(10))
                .unwrap();
        }
        Ok(())
    }
    fn close(&mut self) -> Result<(), crate::reader_pcsc::Failure> {
        Ok(())
    }
}

fn install_card(
    state: &mut crate::ServerState,
    replies: Vec<Vec<u8>>,
    manager: Option<bool>,
    identity: bool,
    release: Option<ReleaseBarrier>,
) -> (Arc<Mutex<CardScript>>, Arc<DirectSupplier>) {
    let script = Arc::new(Mutex::new(CardScript {
        replies: replies.into(),
        commands: Vec::new(),
        release,
    }));
    let opening = script.clone();
    let resetting = script.clone();
    let owner = crate::reader_pcsc::Owner::with_factory(
        "reader-producer-fixture".into(),
        Arc::new(move |_| Ok(Box::new(ScriptedPhysical(opening.clone())))),
        Arc::new(|| Ok(vec!["Reader A".into()])),
        Arc::new(move |_, _| Ok(Box::new(ScriptedPhysical(resetting.clone())))),
    );
    let session = owner.native_open("Reader A").unwrap();
    state.reader_pcsc = owner;
    select_reader(state, "Reader A");
    let supplier = install_supplier(
        state,
        Arc::new(DirectSupplier {
            session: Some(session),
            reject: false,
            esim_manager_available: manager,
            identity,
        }),
    );
    (script, supplier)
}

fn missing_applications() -> Vec<Vec<u8>> {
    proletarius_euicc::KNOWN_ISDR_APPLICATION_AIDS
        .iter()
        .flat_map(|_| [vec![0x01, 0x90, 0x00], vec![0x6a, 0x82], vec![0x90, 0x00]])
        .collect()
}

fn euicc_replies(profile_read_error: bool) -> Vec<Vec<u8>> {
    let mut eid = vec![0xbf, 0x3e, 0x12, 0x5a, 0x10];
    eid.extend_from_slice(&[0x89, 0x04, 0x90, 0x32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    eid.extend_from_slice(&[0x90, 0x00]);
    vec![
        vec![1, 0x90, 0],
        vec![0x90, 0],
        eid,
        vec![0x6a, 0x81],
        vec![0x6a, 0x81], // unavailable optional Info2/Info1
        vec![0x6a, 0x81], // optional configured addresses
        if profile_read_error {
            vec![0x69, 0x82]
        } else {
            vec![0xbf, 0x2d, 2, 0xa0, 0, 0x90, 0]
        },
        vec![0x90, 0], // close first ISD-R
        vec![1, 0x90, 0],
        vec![0x6a, 0x82],
        vec![0x90, 0], // second eSTK.me AID absent
        vec![1, 0x90, 0],
        vec![0x6a, 0x82],
        vec![0x90, 0], // optional product app absent
    ]
}

#[tokio::test]
async fn pcsc_overview_uses_real_native_transaction_without_go() {
    let _journal = crate::esim_test_support::journal_guard().await;
    for partial in [false, true] {
        let root = crate::tests::test_directory(&format!("pcsc-native-overview-{partial}"));
        let mut state = state(&root);
        let trap = GoTrap::install(&mut state, &root);
        state.availability.mark_failed();
        let (script, _supplier) =
            install_card(&mut state, euicc_replies(partial), Some(true), true, None);
        // A partial read stays display-only even when refresh was requested.
        let path = if partial {
            "/api/reader/esim?refresh=true"
        } else {
            "/api/reader/esim"
        };
        let (status, body) = public_request(state.clone(), "GET", path, b"").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["card_type"], "euicc");
        assert_eq!(
            body["chip_info"]["eids"][0]["eid"],
            "89049032000000000000000000000001"
        );
        assert_eq!(
            body["profiles"][0]["read_state"],
            if partial { "error" } else { "ready" }
        );
        assert!(
            body["profiles"][0]["profiles"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let script = script.lock().unwrap();
        assert!(script.replies.is_empty());
        assert_eq!(script.commands[0], [0, 0x70, 0, 0, 1]);
        assert!(
            script
                .commands
                .iter()
                .any(|apdu| apdu.windows(2).any(|tag| tag == [0xbf, 0x3e]))
        );
        drop(script);
        assert!(state.reader_gate.admit("Reader A").is_ok());
        trap.assert_unused().await;
        state.reader_pcsc.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn pcsc_missing_applets_require_positive_identity_for_physical_sim() {
    let _journal = crate::esim_test_support::journal_guard().await;
    for (index, path, manager, identity, expected) in [
        (0, "/api/reader/esim", true, true, StatusCode::OK),
        (1, "/api/reader/esim/health", true, true, StatusCode::OK),
        (2, "/api/reader/esim", true, false, StatusCode::BAD_GATEWAY),
        (
            3,
            "/api/reader/esim/health",
            true,
            false,
            StatusCode::BAD_GATEWAY,
        ),
        (4, "/api/reader/esim", false, true, StatusCode::OK),
        (5, "/api/reader/esim/health", false, true, StatusCode::OK),
    ] {
        let root = crate::tests::test_directory(&format!("pcsc-native-physical-{index}"));
        let mut state = state(&root);
        let trap = GoTrap::install(&mut state, &root);
        let (script, _supplier) = install_card(
            &mut state,
            missing_applications(),
            Some(manager),
            identity,
            None,
        );
        let (status, body) = public_request(state.clone(), "GET", path, b"").await;
        assert_eq!(status, expected, "{body}");
        if identity {
            assert_eq!(body["card_type"], "physical_sim");
            if path.ends_with("health") {
                assert_eq!(body["state"], "physical_sim");
            } else {
                assert_eq!(body["profiles"], serde_json::json!([]));
                assert!(
                    body["capabilities"]
                        .as_object()
                        .unwrap()
                        .values()
                        .all(|value| value.as_bool() == Some(false))
                );
            }
        } else {
            assert!(body.get("card_type").is_none());
            assert_eq!(
                body["code"],
                if path.ends_with("health") {
                    "reader_esim_health_failed"
                } else {
                    "reader_esim_overview_failed"
                }
            );
        }
        assert!(script.lock().unwrap().replies.is_empty());
        trap.assert_unused().await;
        state.reader_pcsc.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn pcsc_security_failure_never_falls_back_to_go_or_physical_sim() {
    let _journal = crate::esim_test_support::journal_guard().await;
    for (index, path) in ["/api/reader/esim", "/api/reader/esim/health"]
        .into_iter()
        .enumerate()
    {
        let root = crate::tests::test_directory(&format!("pcsc-native-security-{index}"));
        let mut state = state(&root);
        let trap = GoTrap::install(&mut state, &root);
        let (script, _supplier) = install_card(
            &mut state,
            vec![vec![1, 0x90, 0], vec![0x69, 0x82], vec![0x90, 0]],
            Some(false),
            true,
            None,
        );
        let (status, body) = public_request(state.clone(), "GET", path, b"").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert!(body.get("card_type").is_none());
        assert_eq!(
            script.lock().unwrap().commands.len(),
            3,
            "must stop after security failure"
        );
        assert!(script.lock().unwrap().replies.is_empty());
        trap.assert_unused().await;
        state.reader_pcsc.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn pcsc_overview_does_not_publish_complete_old_scan_after_generation_changes() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let root = crate::tests::test_directory("pcsc-native-overview-generation");
    let mut state = state(&root);
    let trap = GoTrap::install(&mut state, &root);
    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
    let (allow_tx, allow_rx) = std::sync::mpsc::channel();
    let (script, _supplier) = install_card(
        &mut state,
        euicc_replies(false),
        Some(true),
        true,
        Some((pending_tx, allow_rx)),
    );
    let owned_state = state.clone();
    let mut response = tokio::spawn(async move {
        public_request(owned_state, "GET", "/api/reader/esim?refresh=true", b"").await
    });
    tokio::time::timeout(Duration::from_secs(2), pending_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(
        script.lock().unwrap().replies.is_empty(),
        "the old scan must actually finish"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut response)
            .await
            .is_err()
    );
    // Change only the authoritative generation after the APDUs have completed,
    // while transaction cleanup still owns the physical resource.
    state.reader_gate.test_seed_selection("Reader A", 2);
    allow_tx.send(()).unwrap();
    let (status, body) = tokio::time::timeout(Duration::from_secs(3), response)
        .await
        .unwrap()
        .unwrap();
    state.reader_pcsc.shutdown().await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "esim_refresh_generation_changed");
    assert_eq!(body["retryable"], false);
    assert_eq!(body["refresh"]["scope"], "reader");
    assert_eq!(body["refresh"]["phase"], "blocked");
    assert_eq!(body["refresh"]["outcome"], "generation_changed");
    assert_eq!(body["refresh"]["attempts"], 1);
    assert_eq!(body["refresh"]["retry_performed"], false);
    assert!(body.get("chip_info").is_none());
    assert_eq!(
        state.reader_gate.selected_reader_generation(),
        Some(("Reader A".into(), 2))
    );
    trap.assert_unused().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn esim_refresh_reader_route_timeout_returns_pending_and_keeps_real_pcsc_owner() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let mut receipts = crate::esim_test_support::observe_refresh_receipts();
    let root = crate::tests::test_directory("reader-refresh-timeout-receipt");
    let mut state = state(&root);
    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
    let (allow_tx, allow_rx) = std::sync::mpsc::channel();
    // The worker has already submitted card work and its result is a real
    // security failure; the actual transaction cleanup remains blocked.
    let (script, _supplier) = install_card(
        &mut state,
        vec![vec![1, 0x90, 0], vec![0x69, 0x82], vec![0x90, 0]],
        Some(true),
        false,
        Some((pending_tx, allow_rx)),
    );
    assert!(!state.go_socket_path.exists());
    let owned = state.clone();
    let response = tokio::spawn(async move {
        crate::esim_test_support::refresh_request(owned, "/api/reader/esim?refresh=true").await
    });
    pending_rx.await.unwrap();
    let commands = script.lock().unwrap().commands.len();
    assert_eq!(commands, 3);
    assert!(state.reader_gate.admit("Reader A").is_err());
    let (status, body) = response.await.unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "esim_refresh_cleanup_pending");
    assert_eq!(body["refresh"]["scope"], "reader");
    assert_eq!(body["refresh"]["phase"], "pending");
    assert_eq!(body["refresh"]["outcome"], "cleanup_pending");
    assert_eq!(body["refresh"]["attempts"], 1);
    assert_eq!(body["refresh"]["retry_performed"], false);
    assert_eq!(body["retryable"], false);
    assert_eq!(body["recoverable"], false);
    assert!(!body["refresh"]["operation_id"].as_str().unwrap().is_empty());
    assert!(
        state.reader_gate.admit("Reader A").is_err(),
        "HTTP timeout cannot release the retained lease"
    );
    assert_eq!(
        script.lock().unwrap().commands.len(),
        commands,
        "timeout never replays physical work"
    );
    allow_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if state.reader_gate.admit("Reader A").is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        script.lock().unwrap().commands.len(),
        commands,
        "unknown card result never gets a replay after cleanup"
    );
    let terminal = crate::esim_test_support::next_refresh_receipt(
        &mut receipts,
        body["refresh"]["operation_id"].as_str().unwrap(),
        "failed",
    )
    .await;
    assert_eq!(terminal["operation_id"], body["refresh"]["operation_id"]);
    assert_eq!(terminal["attempts"], 1);
    assert_eq!(terminal["outcome"], "unknown");
    assert_eq!(terminal["retry_performed"], false);
    state.reader_pcsc.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn esim_refresh_reader_route_unknown_result_is_structured_without_physical_replay() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let root = crate::tests::test_directory("reader-refresh-unknown-receipt");
    let mut state = state(&root);
    let (script, _supplier) = install_card(
        &mut state,
        vec![vec![1, 0x90, 0], vec![0x69, 0x82], vec![0x90, 0]],
        Some(true),
        false,
        None,
    );
    let (status, body) =
        crate::esim_test_support::refresh_request(state.clone(), "/api/reader/esim?refresh=true")
            .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["code"], "reader_esim_overview_failed");
    assert_eq!(body["refresh"]["scope"], "reader");
    assert_eq!(body["refresh"]["phase"], "failed");
    assert_eq!(body["refresh"]["outcome"], "unknown");
    assert_eq!(body["refresh"]["attempts"], 1);
    assert_eq!(body["refresh"]["retry_performed"], false);
    assert_eq!(body["retryable"], false);
    assert_eq!(body["recoverable"], false);
    assert!(!body["refresh"]["operation_id"].as_str().unwrap().is_empty());
    assert_eq!(script.lock().unwrap().commands.len(), 3);
    assert!(script.lock().unwrap().replies.is_empty());
    assert!(
        state.reader_gate.admit("Reader A").is_ok(),
        "actual cleanup releases the owner"
    );
    state.reader_pcsc.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
