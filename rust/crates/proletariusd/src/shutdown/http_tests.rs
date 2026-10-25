//! Exercise the production HTTP/UDS shutdown wrappers, not a replica gate.
use crate::shutdown::CleanupGate;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UnixStream;

fn state(root: &Path) -> crate::ServerState {
    let sms_inbox = crate::message_archive::InboxState::default();
    crate::ServerState {
        call_mode: Default::default(),
        module_call: Default::default(),
        reader_lifecycle: Default::default(),
        multistandby_card: Default::default(),
        multistandby_service: Default::default(),
        app_token: Some("app-fixture".to_owned()),
        internal_token: "internal-fixture".to_owned(),
        multistandby_store: crate::multistandby_store::Store::open(&root.join("multi")).unwrap(),
        multistandby_operations: crate::multistandby_operations::Store::open(&root).unwrap(),
        multi_runtime_report: Arc::new(std::sync::Mutex::new(
            crate::multistandby_runtime_report::Journal::default(),
        )),
        device_send_store: crate::device_send_store::Store::open(&root.join("device")).unwrap(),
        forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(&root).unwrap(),
        sms_forwarding: crate::sms_forwarding::Service::default(),
        sms_inbox: sms_inbox.clone(),
        usb_sms_receive: crate::test_usb_sms_receiver(
            sms_inbox.clone(),
            root.join("sms-archive.json"),
        ),
        call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
        call_facts: crate::call_facts::CallFacts::new(
            proletarius_store::ArchiveStore::new(root.join("call-log.json")),
            "call-producer-fixture".to_owned(),
        ),
        go_socket_path: PathBuf::new(),
        availability: crate::BackendAvailability::new(),
        reader_discovery: Arc::new(std::sync::Mutex::new(crate::ReaderDiscovery::default())),
        snapshot_instance_id: "test".to_owned(),
        snapshot_revision: Arc::new(crate::AtomicU64::new(0)),
        eap_exchanges: Arc::new(crate::eap_identity::EapExchangeStore::default()),
        native_vowifi_lifecycle: Arc::new(crate::native_vowifi_session::NativeLifecycle::default()),
        ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
        reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
        reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
        modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
        qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
        network_traffic: crate::network_traffic::Service::default(),
        usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
        module_radio: crate::test_module_radio(&root),
    }
}

type Sender = hyper::client::conn::http1::SendRequest<Full<Bytes>>;
async fn connection(
    state: crate::ServerState,
    gate: CleanupGate,
    internal: bool,
) -> (
    Sender,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<Result<(), hyper::Error>>,
) {
    let (client, server) = UnixStream::pair().unwrap();
    let serving = tokio::spawn(async move {
        if internal {
            crate::serve_internal_shutdown_with_gate(server, state, gate).await;
        } else {
            crate::serve_shutdown_callback_with_gate(server, state, gate).await;
        }
    });
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let driving = tokio::spawn(async move {
        // Finish HTTP normally, then close the owned socket by dropping its
        // parts. A second write-side shutdown after the server has closed its
        // UDS can return ENOTCONN on macOS despite a complete HTTP response.
        // Protocol/read/write errors still propagate to roundtrip's assertions.
        connection.without_shutdown().await.map(drop)
    });
    (sender, serving, driving)
}

fn request(path: &str, body: Value, internal: bool, authenticated: bool) -> Request<Full<Bytes>> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "localhost");
    if authenticated {
        builder = if internal {
            builder.header("x-switch-internal-token", "internal-fixture")
        } else {
            builder.header("x-proletarius-app-token", "app-fixture")
        };
    }
    builder
        .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
        .unwrap()
}

async fn roundtrip(
    state: &crate::ServerState,
    gate: &CleanupGate,
    internal: bool,
    authenticated: bool,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    roundtrip_request(
        state,
        gate,
        internal,
        request(path, body, internal, authenticated),
    )
    .await
}

async fn roundtrip_request(
    state: &crate::ServerState,
    gate: &CleanupGate,
    internal: bool,
    request: Request<Full<Bytes>>,
) -> (StatusCode, Value) {
    let (mut sender, serving, driving) = connection(state.clone(), gate.clone(), internal).await;
    let response = sender.send_request(request).await.unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    drop(sender);
    tokio::time::timeout(Duration::from_secs(2), driving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), serving)
        .await
        .unwrap()
        .unwrap();
    (status, value)
}

fn archive_record() -> Value {
    json!({"id":"closed-gate-record", "device":"module", "direction":"out",
        "number":"+441234", "answered":true, "started_at":"2026-10-04T12:00:00Z",
        "ended_at":"2026-10-04T12:01:00Z", "duration":60})
}

async fn finish_fixture(state: crate::ServerState, root: PathBuf) {
    let actor = state.modem_actor.clone();
    tokio::task::spawn_blocking(move || actor.shutdown())
        .await
        .unwrap()
        .unwrap();
    state.modem_actor.finish_sms_receiver().await.unwrap();
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn closed_production_http_gates_reject_writes_and_preserve_authentication() {
    let root = crate::tests::test_directory("shutdown-closed-http-gate");
    let state = state(&root);
    let gate = CleanupGate::default();
    gate.close().await;
    for internal in [true, false] {
        for authenticated in [false, true] {
            let (path, body) = if internal {
                ("/api/internal/call-archive/ingest", archive_record())
            } else {
                (
                    "/api/v2/vowifi/eap-exchange/release",
                    json!({"exchange_id":"0".repeat(64)}),
                )
            };
            let (status, body) =
                roundtrip(&state, &gate, internal, authenticated, path, body).await;
            assert_eq!(
                status,
                if authenticated {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::UNAUTHORIZED
                }
            );
            if authenticated {
                assert_eq!(body["code"], "backend_stopping");
            }
        }
    }
    assert!(
        !state.call_archive.path().exists(),
        "late ingest must not create an archive"
    );
    finish_fixture(state, root).await;
}

fn observation(sequence: u64, state: &str, at: &str) -> Value {
    json!({"producer_id":"call-producer-fixture", "source":"module_ims",
        "owner_id":"module-fixture", "owner_epoch":1, "sequence":sequence,
        "action":"ims", "direction":"out", "number":"+441234", "call_id":"shutdown-call-fixture",
        "state":state, "observed_at":at})
}

#[tokio::test]
async fn cancelled_production_http_waiter_retains_gate_until_actual_archive_commit() {
    let root = crate::tests::test_directory("shutdown-retained-http-handler");
    let state = state(&root);
    let gate = CleanupGate::default();
    let path = "/api/internal/call-archive/observe";
    let (status, _) = roundtrip(
        &state,
        &gate,
        true,
        true,
        path,
        observation(1, "outgoing", "2026-01-01T12:00:00Z"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, blocked) = std::sync::mpsc::sync_channel(1);
    let blocked = Mutex::new(blocked);
    state
        .call_facts
        .set_before_ingest_hook(Some(Arc::new(move || {
            entered.lock().unwrap().take().unwrap().send(()).unwrap();
            blocked
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        })));
    let (mut sender, serving, driving) = connection(state.clone(), gate.clone(), true).await;
    let waiter = tokio::spawn(async move {
        sender
            .send_request(request(
                path,
                observation(2, "ended", "2026-01-01T12:00:04Z"),
                true,
                true,
            ))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap();
    // The real HTTP handler has now admitted and started the real archive
    // operation. Disposing both socket tasks must not dispose that handler.
    serving.abort();
    assert!(serving.await.unwrap_err().is_cancelled());
    waiter.abort();
    let _ = waiter.await;
    driving.abort();
    let _ = driving.await;
    let closing = gate.clone();
    let mut close = tokio::spawn(async move {
        closing.close().await;
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut close)
            .await
            .is_err(),
        "connection cancellation must not release the live handler's permit"
    );
    assert!(!state.call_archive.path().exists());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), close)
        .await
        .unwrap()
        .unwrap();
    state.call_facts.set_before_ingest_hook(None);
    let store = state.call_archive.clone();
    let records = crate::archive_io::run(move || store.load()).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["duration"], 4);
    let (status, body) = roundtrip(
        &state,
        &gate,
        true,
        true,
        "/api/internal/call-archive/ingest",
        archive_record(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "backend_stopping");
    assert_eq!(state.call_archive.load().unwrap(), records);
    finish_fixture(state, root).await;
}

#[tokio::test]
async fn closed_internal_gate_preserves_retired_route_and_token_precedence() {
    let root = crate::tests::test_directory("shutdown-route-token-precedence");
    let state = state(&root);
    let gate = CleanupGate::default();
    gate.close().await;
    for path in [
        "/api/internal/reader-service",
        "/api/internal/reader-ble-transport",
        "/api/internal/reader-pcsc/connect",
        "/api/internal/multistandby/reader-reservation/operation/prepare",
        "/api/internal/multistandby/card/submit",
        "/api/internal/unknown-shutdown-fixture",
        "/api/internal/call-archive/ingest",
    ] {
        for token in [None, Some("wrong-internal-token"), Some("internal-fixture")] {
            let mut request = request(path, archive_record(), true, false);
            if let Some(token) = token {
                request
                    .headers_mut()
                    .insert("x-switch-internal-token", token.parse().unwrap());
            }
            let (status, body) = roundtrip_request(&state, &gate, true, request).await;
            let (expected, code) = if path != "/api/internal/call-archive/ingest" {
                (StatusCode::NOT_FOUND, "route_not_found")
            } else if token == Some("internal-fixture") {
                (StatusCode::SERVICE_UNAVAILABLE, "backend_stopping")
            } else {
                (StatusCode::UNAUTHORIZED, "internal_authentication_required")
            };
            assert_eq!(status, expected, "path={path}, token={token:?}");
            assert_eq!(body["code"], code, "path={path}, token={token:?}");
            assert!(
                !state.call_archive.path().exists(),
                "rejected request wrote archive: path={path}, token={token:?}"
            );
        }
    }
    finish_fixture(state, root).await;
}
