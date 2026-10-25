//! Exercise the deployed HTTP boundary without opening USB or a reader.
use super::*;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};

async fn rejected_prepare(body: Value) -> (StatusCode, Value) {
    rejected_request(INTERNAL_DEPLOY_PATH, body).await
}

async fn rejected_request(path: &str, body: Value) -> (StatusCode, Value) {
    let producer = "prepare-wire-producer";
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime/call-mode-prepare-wire")
        .join(crate::snapshot::new_instance_id());
    let usb = crate::usb_at::Owner::new(producer.into());
    let radio = crate::module_radio::Service::new(
        root.clone(),
        usb.clone(),
        crate::modem_actor::ModemActor::new().unwrap(),
        crate::availability::BackendAvailability::new(),
        producer.into(),
    );
    let inbox = crate::message_archive::InboxState::for_go_producer(producer.into());
    let service = service_fn(move |request| {
        let root = root.clone();
        let inbox = inbox.clone();
        let usb = usb.clone();
        let radio = radio.clone();
        async move {
            Ok::<_, std::convert::Infallible>(
                handle_internal(request, &root, inbox, usb, radio).await,
            )
        }
    });
    let (client_io, server_io) = tokio::io::duplex(16 * 1024);
    let server = tokio::spawn(async move {
        hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(server_io), service)
            .await
            .unwrap();
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client_io))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let response = sender
        .send_request(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    drop(sender);
    connection.await.unwrap().unwrap();
    server.await.unwrap();
    (status, value)
}

#[tokio::test]
async fn prepare_http_requires_complete_operation_identity_before_any_device_work() {
    for fields in [
        json!({}),
        json!({"call_mode_token":"operation"}),
        json!({"call_mode_sequence":1}),
        json!({"call_mode_token":"", "call_mode_sequence":1}),
        json!({"call_mode_token":"operation", "call_mode_sequence":0}),
    ] {
        let mut body = json!({"producer_id":"prepare-wire-producer", "runtime_generation":1});
        body.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let (status, value) = rejected_prepare(body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["code"], "module_voice_operation_invalid");
        assert_eq!(value["ready"], false);
    }
}

#[tokio::test]
async fn prepare_http_rejects_stale_producer_and_invalid_generation_without_usb() {
    let (status, value) = rejected_prepare(json!({
        "producer_id":"retired-prepare-producer", "runtime_generation":1,
        "call_mode_token":"operation", "call_mode_sequence":1,
    }))
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value["code"], "stale_backend_generation");
    for generation in [Value::Null, json!(0)] {
        let (status, value) = rejected_prepare(json!({
            "producer_id":"prepare-wire-producer", "runtime_generation":generation,
            "call_mode_token":"operation", "call_mode_sequence":1,
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["code"], "module_voice_runtime_generation_invalid");
    }
}

#[tokio::test]
async fn prepare_http_does_not_accept_a_skip_lock_claim() {
    let (status, value) = rejected_prepare(json!({
        "producer_id":"prepare-wire-producer", "runtime_generation":1,
        "call_mode_token":"operation", "call_mode_sequence":1, "skip_lock":true,
    }))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["code"], "invalid_request");
}

#[tokio::test]
async fn stop_http_rejects_null_unknown_and_cross_action_policy_without_usb() {
    for policy in [Value::Null, json!("unknown"), json!(1)] {
        let (status, value) = rejected_request(
            INTERNAL_ROUTE_STOP_PATH,
            json!({
                "producer_id":"prepare-wire-producer", "stop_policy":policy,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["code"], "invalid_request");
        assert_eq!(value["stop_phase"], "stop");
        assert!(value.get("active").is_none());
    }
    for path in [
        INTERNAL_ROUTE_START_PATH,
        INTERNAL_ROUTE_STATUS_PATH,
        INTERNAL_DEPLOY_PATH,
        INTERNAL_PROBE_PATH,
        INTERNAL_RUNTIME_STATUS_PATH,
        INTERNAL_RUNTIME_GENERATION_PATH,
    ] {
        let (status, value) = rejected_request(
            path,
            json!({
                "producer_id":"prepare-wire-producer", "stop_policy":"restore",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["code"], "invalid_request");
        assert!(value.get("stop_phase").is_none());
    }
}

#[tokio::test]
async fn restore_stop_http_requires_genuine_operation_credentials_and_status_phase() {
    for fields in [
        json!({}),
        json!({"call_mode_token":"operation"}),
        json!({"call_mode_sequence":1}),
        json!({"call_mode_token":"", "call_mode_sequence":1}),
    ] {
        let mut body = json!({"producer_id":"prepare-wire-producer", "stop_policy":"restore"});
        body.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let (status, value) = rejected_request(INTERNAL_ROUTE_STOP_PATH, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["code"], "module_voice_operation_invalid");
        assert_eq!(value["stop_phase"], "status");
        assert!(value.get("active").is_none());
    }
    let (status, value) = rejected_request(
        INTERNAL_ROUTE_STOP_PATH,
        json!({
            "producer_id":"prepare-wire-producer", "stop_policy":"restore",
            "call_mode_token":"unissued-operation", "call_mode_sequence":1,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value["ready"], false);
    assert_eq!(value["stop_phase"], "status");
    assert!(value.get("active").is_none());
}

#[tokio::test]
async fn ordinary_stop_http_rejects_operation_credentials_and_stale_default_stop() {
    for policy in ["always", "hangup", "shutdown"] {
        let (status, value) = rejected_request(
            INTERNAL_ROUTE_STOP_PATH,
            json!({
                "producer_id":"prepare-wire-producer", "stop_policy":policy,
                "call_mode_token":"operation", "call_mode_sequence":1,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["code"], "module_voice_operation_invalid");
        assert_eq!(value["stop_phase"], "stop");
    }
    let (status, value) = rejected_request(
        INTERNAL_ROUTE_STOP_PATH,
        json!({
            "producer_id":"retired-producer",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value["code"], "stale_backend_generation");
    assert_eq!(value["stop_phase"], "stop");
    assert!(value.get("active").is_none());
}
