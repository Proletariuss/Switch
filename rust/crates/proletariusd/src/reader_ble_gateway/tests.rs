use super::*;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

const READER: &str = "BLE:12345678-1234-1234-1234-123456789abc";
const PRODUCER: &str = "gateway-test-go";
const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> (Self, UnixListener) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ble-gw-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = UnixListener::bind(&path).unwrap();
        (Self(path), listener)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn accept(listener: &UnixListener) -> (UnixStream, Value) {
    let (stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    (reader.into_inner(), serde_json::from_str(&line).unwrap())
}
fn terminal(request: &Value, mut response: Value) -> Value {
    assert_eq!(request["bridge_id"], "fixture-bridge");
    assert_eq!(request["operation_id"].as_str().unwrap().len(), 64);
    response["bridge_id"] = request["bridge_id"].clone();
    response["operation_id"] = request["operation_id"].clone();
    response["terminal"] = json!(true);
    response
}
async fn reply(mut stream: UnixStream, response: Value) {
    let mut bytes = serde_json::to_vec(&response).unwrap();
    bytes.push(b'\n');
    stream.write_all(&bytes).await.unwrap();
}
async fn status_reply(stream: UnixStream) {
    reply(
        stream,
        json!({"ok":true,"state":"connected","reader":READER,
        "operation_tracking":true,"bridge_id":"fixture-bridge"}),
    )
    .await;
}

#[derive(Clone)]
enum Route {
    Card {
        socket: Option<PathBuf>,
        gate: Arc<crate::reader_reservation::Gate>,
        stopping: bool,
    },
    Operation {
        gate: Arc<crate::reader_reservation::Gate>,
        owner: crate::reader_pcsc::Owner,
    },
}
async fn roundtrip(route: Route, path: String, value: Value) -> (StatusCode, Value) {
    let (client, server) = UnixStream::pair().unwrap();
    let service = tokio::spawn(async move {
        if let Err(error) = hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(server),
                service_fn(move |request| {
                    let route = route.clone();
                    async move {
                        Ok::<_, Infallible>(match route {
                            Route::Card {
                                socket,
                                gate,
                                stopping,
                            } => {
                                handle_with_gate(
                                    request,
                                    socket.as_deref(),
                                    Some(TOKEN),
                                    stopping,
                                    gate,
                                )
                                .await
                            }
                            Route::Operation { gate, owner } => {
                                crate::reader_reservation::handle_legacy_operation_wire(
                                    request, owner, gate,
                                )
                                .await
                            }
                        })
                    }
                }),
            )
            .await
        {
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
    let response = sender
        .send_request(
            Request::post(path)
                .body(Full::new(Bytes::from(serde_json::to_vec(&value).unwrap())))
                .unwrap(),
        )
        .await
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
    service.await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

// Admission goes through the production operation RPC and real DTO. Only the
// pre-existing reader selection is seeded; no card authority is bypassed.
struct Scope {
    gate: Arc<crate::reader_reservation::Gate>,
    owner: crate::reader_pcsc::Owner,
    binding: Value,
}
impl Scope {
    async fn new() -> Self {
        let gate = Arc::new(crate::reader_reservation::Gate::default());
        gate.initialize_business_producer(PRODUCER);
        gate.test_seed_selection(READER, 7);
        let value = Self {
            gate,
            owner: crate::reader_pcsc::Owner::new(PRODUCER.into()),
            binding: json!({"producer_id":PRODUCER,"operation_token":"ble-gateway-operation-token-000000001",
                "operation_sequence":1,"reader":READER,"reader_generation":7,
                "purpose":"card","authority":{"kind":"idle"}}),
        };
        let (status, receipt) = value.operation("acquire").await;
        assert_eq!(status, StatusCode::OK, "{receipt}");
        assert_eq!(receipt["binding"], value.binding);
        assert_eq!(receipt["phase"], "active");
        value
    }
    fn transaction(&self) -> CardTransaction {
        serde_json::from_value(self.transaction_value()).unwrap()
    }
    fn transaction_value(&self) -> Value {
        json!({"kind":"operation","binding":self.binding})
    }
    fn input(&self, action: &str) -> Value {
        let mut value = json!({"producer_id":PRODUCER,"transaction":self.transaction_value(),
            "action":action,"reader":READER});
        if action == "apdu" {
            value["hex"] = json!("00A40000");
        }
        value
    }
    async fn operation(&self, action: &str) -> (StatusCode, Value) {
        roundtrip(
            Route::Operation {
                gate: self.gate.clone(),
                owner: self.owner.clone(),
            },
            format!("{}operation/{action}", crate::reader_reservation::PREFIX),
            json!({"binding":self.binding}),
        )
        .await
    }
    async fn card(
        &self,
        socket: Option<PathBuf>,
        input: Value,
        stopping: bool,
    ) -> (StatusCode, Value) {
        roundtrip(
            Route::Card {
                socket,
                gate: self.gate.clone(),
                stopping,
            },
            PATH.into(),
            input,
        )
        .await
    }
    async fn released(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (status, receipt) = self.operation("release").await;
                assert_eq!(status, StatusCode::OK, "{receipt}");
                if receipt["phase"] == "released" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn gateway_binds_apdu_and_reset_and_preserves_bridge_errors() {
    for (action, response, expected_status) in [
        ("apdu", json!({"ok":true,"hex":"Aa009000"}), StatusCode::OK),
        ("reset", json!({"ok":true}), StatusCode::OK),
        (
            "apdu",
            json!({"ok":false,"errorCode":"ble_reader_changed"}),
            StatusCode::BAD_GATEWAY,
        ),
    ] {
        let scope = Scope::new().await;
        let (fixture, listener) = Fixture::new();
        let server = tokio::spawn(async move {
            let (stream, status) = accept(&listener).await;
            assert_eq!(status["action"], "status");
            status_reply(stream).await;
            let (stream, command) = accept(&listener).await;
            assert_eq!(command["reader"], READER);
            assert_eq!(command["token"], TOKEN);
            assert_eq!(command["action"], action);
            if action == "apdu" {
                assert_eq!(command["hex"], "00a40000");
            }
            reply(stream, terminal(&command, response)).await;
        });
        let (status, response) = scope
            .card(Some(fixture.0.clone()), scope.input(action), false)
            .await;
        assert_eq!(status, expected_status, "{response}");
        if status == StatusCode::OK {
            assert_eq!(response["ok"], true);
            if action == "apdu" {
                assert_eq!(response["hex"], "aa009000");
            }
        } else {
            assert_eq!(response["error"], "BLE bridge: ble_reader_changed");
            assert_eq!(response["code"], "ble_reader_changed");
            assert_eq!(response["retryable"], false);
        }
        server.await.unwrap();
        scope.released().await;
    }
}

#[tokio::test]
async fn metadata_action_runs_under_reader_capability_and_fails_closed_on_card_error() {
    let scope = Scope::new().await;
    let (fixture, listener) = Fixture::new();
    let server = tokio::spawn(async move {
        let (stream, status) = accept(&listener).await;
        assert_eq!(status["action"], "status");
        status_reply(stream).await;
        let (stream, command) = accept(&listener).await;
        assert_eq!(command["action"], "apdu");
        assert_eq!(command["reader"], READER);
        assert_eq!(command["hex"], "00a40004023f0000");
        reply(
            stream,
            terminal(
                &command,
                json!({"ok":false,"errorCode":"ble_reader_changed"}),
            ),
        )
        .await;
    });
    let (status, response) = scope
        .card(Some(fixture.0.clone()), scope.input("metadata"), false)
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{response}");
    assert_eq!(response["code"], "ble_reader_changed", "{response}");
    server.await.unwrap();
    scope.released().await;
}

#[tokio::test]
async fn gateway_rejects_invalid_input_and_unconfigured_bridge() {
    let scope = Scope::new().await;
    for mut input in [
        json!({"action":"apdu","hex":"9000"}),
        json!({"action":"apdu","reader":"BLE:invalid","hex":"9000"}),
        json!({"action":"apdu","reader":READER,"hex":"g0"}),
        json!({"action":"apdu","reader":READER,"hex":"aa".repeat(65_536)}),
        json!({"action":"reset","reader":READER,"hex":"9000"}),
        json!({"action":"scan","reader":READER}),
        json!({"action":"reset","reader":READER,"extra":true}),
    ] {
        input["producer_id"] = json!(PRODUCER);
        input["transaction"] = scope.transaction_value();
        assert_eq!(
            scope.card(None, input, false).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        scope.card(None, scope.input("reset"), false).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    scope.released().await;
}

#[tokio::test]
async fn missing_stale_and_released_transaction_never_reaches_bridge() {
    let scope = Scope::new().await;
    let (fixture, listener) = Fixture::new();
    for transaction in [None, Some(Value::Null), Some(json!({"kind":"operation"}))] {
        let mut input = scope.input("reset");
        match transaction {
            Some(value) => input["transaction"] = value,
            None => {
                input.as_object_mut().unwrap().remove("transaction");
            }
        }
        assert_eq!(
            scope.card(Some(fixture.0.clone()), input, false).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut stale = scope.input("reset");
    stale["transaction"]["binding"]["reader_generation"] = json!(6);
    assert_eq!(
        scope.card(Some(fixture.0.clone()), stale, false).await.0,
        StatusCode::CONFLICT
    );
    scope.released().await;
    let (status, receipt) = scope
        .card(Some(fixture.0.clone()), scope.input("reset"), false)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{receipt}");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    assert!(crate::reader_ble_apdu::acquire_io_permit(&fixture.0).is_ok());
}

#[tokio::test]
async fn admission_precedes_dispatch_and_same_scope_cannot_overlap() {
    let scope = Scope::new().await;
    let (fixture, listener) = Fixture::new();
    let worker = execute(
        fixture.0.clone(),
        Zeroizing::new(TOKEN.into()),
        READER.into(),
        PRODUCER.into(),
        Command::Apdu(Zeroizing::new(vec![0, 0xa4, 0, 0])),
        scope.transaction(),
        scope.gate.clone(),
    )
    .unwrap();
    assert!(crate::reader_ble_apdu::acquire_io_permit(&fixture.0).is_err());
    let (stream, status) = accept(&listener).await;
    assert_eq!(status["action"], "status");
    drop(worker);
    // A different bridge socket cannot evade complete-workflow admission.
    let (other, other_listener) = Fixture::new();
    assert!(
        execute(
            other.0.clone(),
            Zeroizing::new(TOKEN.into()),
            READER.into(),
            PRODUCER.into(),
            Command::Reset,
            scope.transaction(),
            scope.gate.clone()
        )
        .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), other_listener.accept())
            .await
            .is_err()
    );
    let (status, receipt) = scope.operation("release").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["phase"], "draining");
    assert!(scope.gate.admit(READER).is_err());
    status_reply(stream).await;
    let (stream, command) = accept(&listener).await;
    assert_eq!(command["action"], "apdu");
    reply(stream, terminal(&command, json!({"ok":true,"hex":"9000"}))).await;
    scope.released().await;
    assert!(scope.gate.admit(READER).is_ok());
}

#[tokio::test]
async fn shutdown_accepts_only_explicit_existing_capability() {
    let scope = Scope::new().await;
    let (fixture, listener) = Fixture::new();
    let mut unbound = scope.input("reset");
    unbound.as_object_mut().unwrap().remove("transaction");
    assert_eq!(
        scope.card(Some(fixture.0.clone()), unbound, true).await.0,
        StatusCode::BAD_REQUEST
    );
    let server = tokio::spawn(async move {
        let (stream, _) = accept(&listener).await;
        status_reply(stream).await;
        let (stream, command) = accept(&listener).await;
        assert_eq!(command["action"], "reset");
        reply(stream, terminal(&command, json!({"ok":true}))).await;
    });
    let (status, response) = scope
        .card(Some(fixture.0.clone()), scope.input("reset"), true)
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    server.await.unwrap();
    scope.released().await;
}

#[tokio::test]
async fn cancelled_uncertain_submission_retains_scope_until_exact_terminal() {
    let scope = Scope::new().await;
    let (fixture, listener) = Fixture::new();
    let worker = execute(
        fixture.0.clone(),
        Zeroizing::new(TOKEN.into()),
        READER.into(),
        PRODUCER.into(),
        Command::Apdu(Zeroizing::new(vec![0, 0xa4, 0, 0])),
        scope.transaction(),
        scope.gate.clone(),
    )
    .unwrap();
    let (stream, _) = accept(&listener).await;
    status_reply(stream).await;
    let (stream, submission) = accept(&listener).await;
    assert_eq!(submission["action"], "apdu");
    drop(stream);
    drop(worker);
    let (status, receipt) = scope.operation("release").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["phase"], "draining");
    let (stream, query) = accept(&listener).await;
    assert_eq!(query["action"], "operation-status");
    assert_eq!(query["operation_id"], submission["operation_id"]);
    assert_eq!(query["bridge_id"], submission["bridge_id"]);
    assert!(query.get("hex").is_none());
    assert!(scope.gate.admit(READER).is_err());
    let mut pending = terminal(
        &submission,
        json!({"ok":false,"errorCode":"ble_operation_pending"}),
    );
    pending["terminal"] = json!(false);
    reply(stream, pending).await;
    let (stream, query) = accept(&listener).await;
    assert_eq!(query["action"], "operation-status");
    assert_eq!(query["operation_id"], submission["operation_id"]);
    assert!(scope.gate.admit(READER).is_err());
    reply(
        stream,
        terminal(&submission, json!({"ok":true,"hex":"9000"})),
    )
    .await;
    scope.released().await;
    assert!(scope.gate.admit(READER).is_ok());
    assert!(crate::reader_ble_apdu::acquire_io_permit(&fixture.0).is_ok());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn producer_identity_required_and_old_producer_never_reaches_bridge() {
    let scope = Scope::new().await;
    let (fixture, listener) = Fixture::new();
    for producer in [Value::Null, json!(""), json!(false), json!("p".repeat(129))] {
        let mut input = scope.input("reset");
        if producer.is_null() {
            input.as_object_mut().unwrap().remove("producer_id");
        } else {
            input["producer_id"] = producer;
        }
        assert_eq!(
            scope.card(Some(fixture.0.clone()), input, false).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut input = scope.input("reset");
    input["producer_id"] = json!("other-producer");
    assert_eq!(
        scope.card(Some(fixture.0.clone()), input, false).await.0,
        StatusCode::CONFLICT
    );
    scope.gate.test_confirm_go_exit("replacement-go");
    let (status, body) = scope
        .card(Some(fixture.0.clone()), scope.input("reset"), false)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "stale_producer");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    assert!(crate::reader_ble_apdu::acquire_io_permit(&fixture.0).is_ok());
}

#[tokio::test]
async fn submitted_scope_guard_survives_producer_exit_until_terminal_proof() {
    let scope = Scope::new().await;
    let (fixture, listener) = Fixture::new();
    let worker = execute(
        fixture.0.clone(),
        Zeroizing::new(TOKEN.into()),
        READER.into(),
        PRODUCER.into(),
        Command::Apdu(Zeroizing::new(vec![0, 0xa4, 0, 0])),
        scope.transaction(),
        scope.gate.clone(),
    )
    .unwrap();
    let (stream, _) = accept(&listener).await;
    status_reply(stream).await;
    let (stream, submission) = accept(&listener).await;
    drop(stream);
    drop(worker);
    scope.gate.test_confirm_go_exit("replacement-go");
    assert!(scope.gate.admit(READER).is_err());
    assert!(crate::reader_ble_apdu::acquire_io_permit(&fixture.0).is_err());
    let (stream, query) = accept(&listener).await;
    assert_eq!(query["action"], "operation-status");
    assert_eq!(query["operation_id"], submission["operation_id"]);
    assert!(scope.gate.admit(READER).is_err());
    reply(
        stream,
        terminal(&submission, json!({"ok":true,"hex":"9000"})),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if crate::reader_ble_apdu::acquire_io_permit(&fixture.0).is_ok()
                && scope.gate.admit(READER).is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (status, body) = scope
        .card(Some(fixture.0.clone()), scope.input("reset"), false)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "stale_producer");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[test]
fn metadata_command_is_reader_bound_and_rejects_apdu_payloads() {
    let input = json!({
        "producer_id": PRODUCER,
        "transaction": {"kind":"operation","binding":{
            "producer_id":PRODUCER,"operation_token":"metadata-operation-token-000000001",
            "operation_sequence":1,"reader":READER,"reader_generation":7,"purpose":"card",
            "authority":{"kind":"idle"}
        }},
        "action":"metadata",
        "reader":READER
    });
    let (_, reader, command, _) = parse(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(reader, READER);
    assert!(matches!(command, Command::Metadata));

    let mut malformed = input;
    malformed["hex"] = json!("00a40000");
    assert!(parse(&serde_json::to_vec(&malformed).unwrap()).is_err());
}

#[test]
fn bridge_error_code_projection_rejects_unstructured_or_sensitive_text() {
    for code in [
        "ble_reader_changed",
        "estk_apdu_timeout",
        "bluetooth_disconnected",
    ] {
        assert_eq!(safe_bridge_code(&format!("BLE bridge: {code}")), Some(code));
        assert_eq!(
            safe_bridge_code(&format!("exchange APDU: BLE bridge: {code}")),
            Some(code)
        );
        assert_eq!(
            safe_bridge_code(&format!(
                "exchange APDU: APDU transport failed: BLE bridge: {code}"
            )),
            Some(code)
        );
    }
    for error in [
        "BLE bridge: token=secret",
        "BLE bridge: ble_private value",
        "BLE bridge: ble_\nsecret",
        "unrelated error",
        "BLE bridge: ble_unknown_secret_value",
    ] {
        assert_eq!(safe_bridge_code(error), None);
    }
}

#[tokio::test]
async fn shutdown_rejects_stale_and_released_capabilities_before_bridge_connection() {
    let scope = Scope::new().await;
    let (fixture, listener) = Fixture::new();
    let mut stale = scope.input("reset");
    stale["transaction"]["binding"]["reader_generation"] = json!(6);
    let (status, _) = scope.card(Some(fixture.0.clone()), stale, true).await;
    assert_eq!(status, StatusCode::CONFLICT);
    scope.released().await;
    let (status, receipt) = scope
        .card(Some(fixture.0.clone()), scope.input("reset"), true)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{receipt}");
    assert_eq!(receipt["code"], "owner_expired");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    assert!(crate::reader_ble_apdu::acquire_io_permit(&fixture.0).is_ok());
}
