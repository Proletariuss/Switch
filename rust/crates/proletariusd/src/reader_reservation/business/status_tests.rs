//! Tests the production status-only handler over UDS with an isolated typed Gate.
use super::*;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use tokio::net::UnixStream;

async fn request(gate: Arc<Gate>, body: &[u8]) -> (StatusCode, Value) {
    let (client, server) = UnixStream::pair().unwrap();
    let server = tokio::spawn(async move {
        let service = service_fn(move |request| {
            let gate = gate.clone();
            async move { Ok::<_, Infallible>(handle_status(request, &gate).await) }
        });
        let result = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(server), service)
            .await;
        if let Err(error) = result {
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
            Request::post(BUSINESS_PATH)
                .body(Full::new(Bytes::copy_from_slice(body)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    server.await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

fn gate() -> Arc<Gate> {
    let gate = Arc::new(Gate::default());
    gate.initialize_business_producer("current");
    gate
}

#[tokio::test]
async fn status_only_wire_cannot_mutate_live_native_authority() {
    let gate = gate();
    gate.initialize_native_reader_producer("native");
    gate.test_seed_selection("Reader A", 1);
    let pin = gate
        .begin_native_pin("Reader A", NativePinPolicy::Identity)
        .unwrap();
    let status = br#"{"action":"status","producer_id":"current"}"#;
    let before = request(gate.clone(), status).await;
    assert_eq!(before.0, StatusCode::OK);
    assert_eq!(before.1["native_session_active"], true);
    for action in ["acquire", "transfer", "release"] {
        let body = json!({"action":action,"producer_id":"current",
            "token":pin.lease_id,"sequence":pin.lease_sequence,"role":"native_session"});
        let response = request(gate.clone(), body.to_string().as_bytes()).await;
        assert_eq!(
            response.0,
            StatusCode::BAD_REQUEST,
            "{action}: {response:?}"
        );
        assert_eq!(request(gate.clone(), status).await, before);
    }
    // Even null capability fields are outside the read-only wire schema.
    for body in [
        br#"{"action":"status","producer_id":"current","token":null}"#.as_slice(),
        br#"{"action":"status","producer_id":"current","reader":"Reader A"}"#,
        br#"{"action":"release","producer_id":"current"}"#,
    ] {
        assert_eq!(request(gate.clone(), body).await.0, StatusCode::BAD_REQUEST);
    }
    gate.drain_native_pin(&pin).unwrap();
    gate.finish_native_pin(&pin).unwrap();
    let after = request(gate.clone(), status).await;
    assert_eq!(after.0, StatusCode::OK);
    assert_eq!(after.1["native_session_active"], false);
}

#[tokio::test]
async fn status_wire_rejects_stale_producers_and_reports_typed_ble_control() {
    let gate = gate();
    let control = gate.acquire_ble_control(&"a".repeat(64)).unwrap();
    let response = request(
        gate.clone(),
        br#"{"action":"status","producer_id":"current"}"#,
    )
    .await;
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1["ble_control_active"], true);
    let stale = request(gate.clone(), br#"{"action":"status","producer_id":"old"}"#).await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    assert_eq!(stale.1["code"], "stale_producer");
    let oversized = vec![b' '; 4097];
    assert_eq!(
        request(gate.clone(), &oversized).await.0,
        StatusCode::BAD_REQUEST
    );
    assert!(gate.acquire_ble_control(&"b".repeat(64)).is_err());
    drop(control);
    let response = request(gate, br#"{"action":"status","producer_id":"current"}"#).await;
    assert_eq!(response.1["ble_control_active"], false);
}
