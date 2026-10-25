use super::*;
use http_body_util::BodyExt;
use serde_json::json;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

struct SocketFixture(PathBuf);
impl SocketFixture {
    fn bind() -> io::Result<(Self, UnixListener)> {
        let id = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "switch-ble-status-{}-{id}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        Ok((Self(path.clone()), UnixListener::bind(path)?))
    }
}
impl Drop for SocketFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn fake_bridge(response: impl Into<String>) -> (SocketFixture, tokio::task::JoinHandle<()>) {
    let (socket, listener) = SocketFixture::bind().unwrap();
    let response = response.into();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        reader.read_line(&mut request).await.unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request, json!({"token":"a".repeat(32),"action":"status"}));
        let mut stream = reader.into_inner();
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(b"\n").await;
    });
    (socket, task)
}

#[tokio::test]
async fn status_projects_the_core_bluetooth_bridge_fields() {
    let (socket, task) = fake_bridge(
        r#"{"ok":true,"reader":"BLE:1234","devices":[{"id":"1234","name":"ESTKme-RED"}],"state":"connected"}"#,
    )
    .await;
    let result = status(Some(&socket.0), Some(&"a".repeat(32)))
        .await
        .unwrap();
    task.await.unwrap();
    assert_eq!(
        result,
        json!({
            "devices":[{"id":"1234","name":"ESTKme-RED"}],
            "connected":"BLE:1234",
            "state":"connected",
            "error_code":""
        })
    );
}

#[tokio::test]
async fn status_maps_bridge_rejections_to_the_legacy_error_message() {
    let (socket, task) = fake_bridge(r#"{"ok":false,"errorCode":"ble_not_authorized"}"#).await;
    let error = status(Some(&socket.0), Some(&"a".repeat(32)))
        .await
        .unwrap_err();
    task.await.unwrap();
    assert_eq!(error, "BLE bridge: ble_not_authorized");
}

#[tokio::test]
async fn status_preserves_go_zero_values_for_omitted_device_fields() {
    let (socket, task) = fake_bridge(r#"{"ok":true,"devices":[{}]}"#).await;
    let result = status(Some(&socket.0), Some(&"a".repeat(32)))
        .await
        .unwrap();
    task.await.unwrap();
    assert_eq!(
        result,
        json!({
            "devices":[{"id":"","name":""}],
            "connected":"",
            "state":"",
            "error_code":""
        })
    );
}

#[tokio::test]
async fn status_accepts_go_compatible_maximum_frame_size() {
    let response = r#"{"ok":true,"reader":"BLE:1234","state":"connected"}"#;
    let padded = format!(
        "{}{}",
        response,
        " ".repeat(MAX_RESPONSE_BYTES - response.len() - 1)
    );
    assert_eq!(padded.len() + 1, MAX_RESPONSE_BYTES);
    let (socket, task) = fake_bridge(padded).await;
    let result = status(Some(&socket.0), Some(&"a".repeat(32)))
        .await
        .unwrap();
    task.await.unwrap();
    assert_eq!(result["connected"], "BLE:1234");
}

#[tokio::test]
async fn status_rejects_a_frame_larger_than_the_go_limit() {
    let response = " ".repeat(MAX_RESPONSE_BYTES);
    let (socket, task) = fake_bridge(response).await;
    let error = status(Some(&socket.0), Some(&"a".repeat(32)))
        .await
        .unwrap_err();
    task.await.unwrap();
    assert_eq!(error, "BLE bridge response: EOF");
}

#[tokio::test]
async fn status_reports_unconfigured_bridge_without_attempting_io() {
    let error = status(None, Some(&"a".repeat(32))).await.unwrap_err();
    assert_eq!(error, "BLE bridge is not configured");
}

#[tokio::test]
async fn rust_owned_presence_uses_exact_connected_identity() {
    let selected = "BLE:12345678-1234-1234-1234-123456789abc";
    let (socket, task) = fake_bridge(format!(
        r#"{{"ok":true,"reader":"{selected}","state":"connected"}}"#
    ))
    .await;
    let facts = reader_status_facts(Some(&socket.0), Some(&"a".repeat(32)), selected)
        .await
        .unwrap();
    task.await.unwrap();
    assert_eq!(facts, (Some(selected.to_owned()), ReaderPresence::Present));
}

#[tokio::test]
async fn another_connected_reader_proves_only_the_selected_identity_absent() {
    let selected = "BLE:12345678-1234-1234-1234-123456789abc";
    let other = "BLE:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    let (socket, task) = fake_bridge(format!(
        r#"{{"ok":true,"reader":"{other}","state":"connected"}}"#
    ))
    .await;
    let facts = reader_status_facts(Some(&socket.0), Some(&"a".repeat(32)), selected)
        .await
        .unwrap();
    task.await.unwrap();
    assert_eq!(facts, (Some(other.to_owned()), ReaderPresence::Absent));
}

#[tokio::test]
async fn transitional_ble_states_are_indeterminate_not_absent() {
    for state in ["connecting", "discovering", "reconnecting"] {
        let (socket, task) = fake_bridge(format!(r#"{{"ok":true,"state":"{state}"}}"#)).await;
        let facts = reader_status_facts(
            Some(&socket.0),
            Some(&"a".repeat(32)),
            "BLE:12345678-1234-1234-1234-123456789abc",
        )
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(
            facts,
            (None, ReaderPresence::Indeterminate),
            "state={state}"
        );
    }
}

#[tokio::test]
async fn inconsistent_or_unknown_ble_states_fail_closed() {
    let (socket, task) = fake_bridge(
        r#"{"ok":true,"reader":"BLE:12345678-1234-1234-1234-123456789abc","state":"disconnected"}"#,
    )
    .await;
    assert!(
        reader_status_facts(
            Some(&socket.0),
            Some(&"a".repeat(32)),
            "BLE:12345678-1234-1234-1234-123456789abc",
        )
        .await
        .is_err()
    );
    task.await.unwrap();

    let (socket, task) = fake_bridge(r#"{"ok":true,"reader":"","state":"surprise"}"#).await;
    assert!(
        reader_status_facts(
            Some(&socket.0),
            Some(&"a".repeat(32)),
            "BLE:12345678-1234-1234-1234-123456789abc",
        )
        .await
        .is_err()
    );
    task.await.unwrap();
}

#[tokio::test]
async fn public_status_keeps_the_go_compatibility_error_shape() {
    let response = handle_public(None, Some(&"a".repeat(32))).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        json!({
            "error":"BLE bridge is not configured",
            "code":"ble_bridge_unavailable",
            "recoverable":true
        })
    );
}
