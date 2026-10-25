use super::*;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{fs::DirBuilder, os::unix::fs::DirBuilderExt};
use tokio::net::UnixListener;

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);
const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const READER: &str = "BLE:12345678-1234-1234-1234-123456789abc";

struct SocketFixture(PathBuf, PathBuf);
impl SocketFixture {
    fn bind() -> io::Result<(Self, UnixListener)> {
        let root = (0..16)
            .find_map(|_| {
                let id = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
                let root =
                    std::env::temp_dir().join(format!("switch-ble-{}-{id}", std::process::id()));
                match DirBuilder::new().mode(0o700).create(&root) {
                    Ok(()) => Some(Ok(root)),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .transpose()?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::AlreadyExists, "socket fixture collision")
            })?;
        let path = root.join("bridge.sock");
        let listener = UnixListener::bind(&path)?;
        Ok((Self(path, root), listener))
    }
}
impl Drop for SocketFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_dir(&self.1);
    }
}

async fn read_bridge(listener: &UnixListener) -> (UnixStream, Value) {
    let (stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    reader.read_line(&mut request).await.unwrap();
    (reader.into_inner(), serde_json::from_str(&request).unwrap())
}

async fn bridge_once(response: &'static str) -> (SocketFixture, tokio::task::JoinHandle<Value>) {
    let (socket, listener) = SocketFixture::bind().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, request) = read_bridge(&listener).await;
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        request
    });
    (socket, task)
}

async fn result_value(response: Response<Full<Bytes>>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn control_actions_preserve_swift_contract_without_a_go_service() {
    for (action, reader, expected_status) in [
        (Action::Scan, None, StatusCode::ACCEPTED),
        (
            Action::Connect,
            Some("12345678-1234-1234-1234-123456789abc".to_owned()),
            StatusCode::ACCEPTED,
        ),
        (Action::Disconnect, None, StatusCode::OK),
    ] {
        let (socket, server) = bridge_once(r#"{"ok":true}"#).await;
        let gate = Arc::new(crate::reader_reservation::Gate::default());
        let response = start_action(
            gate.clone(),
            Some(&socket.0),
            Some(TOKEN),
            action,
            reader.clone(),
        )
        .unwrap()
        .await
        .unwrap();
        assert_eq!(response.status(), expected_status);
        let request = server.await.unwrap();
        assert_eq!(request["token"], TOKEN);
        assert_eq!(request["action"], action.bridge_name());
        assert_eq!(
            request.get("reader").and_then(Value::as_str),
            reader.as_deref()
        );
        assert!(gate.admit(READER).is_ok());
    }
}

#[tokio::test]
async fn bridge_rejections_preserve_codes_and_configuration_failure_releases_admission() {
    let gate = Arc::new(crate::reader_reservation::Gate::default());
    let (socket, server) = bridge_once(r#"{"ok":false,"errorCode":"ble_reader_not_found"}"#).await;
    let response = start_action(
        gate.clone(),
        Some(&socket.0),
        Some(TOKEN),
        Action::Connect,
        Some("missing".into()),
    )
    .unwrap()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        result_value(response).await["error"],
        "BLE bridge: ble_reader_not_found"
    );
    server.await.unwrap();
    let response = start_action(gate.clone(), None, Some(TOKEN), Action::Scan, None).unwrap_err();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(gate.admit(READER).is_ok());
    assert!(crate::reader_ble_apdu::acquire_io_permit(&socket.0).is_ok());
}

#[tokio::test]
async fn native_card_owners_reject_control_before_any_swift_submission() {
    let (socket, listener) = SocketFixture::bind().unwrap();
    for reservation in [false, true] {
        let gate = Arc::new(crate::reader_reservation::Gate::default());
        let mut worker = None;
        let mut owner = None;
        if reservation {
            owner = Some(gate.acquire(None, Some(READER), Some(1)).unwrap());
        } else {
            worker = Some(gate.admit(READER).unwrap());
        }
        let response = start_action(
            gate.clone(),
            Some(&socket.0),
            Some(TOKEN),
            Action::Disconnect,
            None,
        )
        .unwrap_err();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            result_value(response).await["code"],
            "reader_operation_active"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        drop(worker);
        if let Some(owner) = owner {
            gate.release(&owner).unwrap();
        }
    }
}

#[tokio::test]
async fn submitted_card_io_rejects_control_and_returns_native_gate() {
    let gate = Arc::new(crate::reader_reservation::Gate::default());
    let (socket, listener) = SocketFixture::bind().unwrap();
    let _permit = crate::reader_ble_apdu::acquire_io_permit(&socket.0).unwrap();
    let response = start_action(
        gate.clone(),
        Some(&socket.0),
        Some(TOKEN),
        Action::Disconnect,
        None,
    )
    .unwrap_err();
    assert!(
        result_value(response).await["error"]
            .as_str()
            .unwrap()
            .contains("busy")
    );
    assert!(gate.admit(READER).is_ok());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancellation_retains_both_native_permits_until_bridge_reply() {
    let gate = Arc::new(crate::reader_reservation::Gate::default());
    let (socket, listener) = SocketFixture::bind().unwrap();
    let worker = start_action(
        gate.clone(),
        Some(&socket.0),
        Some(TOKEN),
        Action::Disconnect,
        None,
    )
    .unwrap();
    // Dropping the HTTP wait before dispatch must not open a gap either.
    drop(worker);
    assert!(gate.admit(READER).is_err());
    assert!(crate::reader_ble_apdu::acquire_io_permit(&socket.0).is_err());
    let (mut stream, request) = read_bridge(&listener).await;
    assert_eq!(request["action"], "disconnect");
    assert!(gate.admit(READER).is_err());
    assert!(crate::reader_ble_apdu::acquire_io_permit(&socket.0).is_err());
    stream.write_all(b"{\"ok\":true}\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(card) = gate.admit(READER) {
                if crate::reader_ble_apdu::acquire_io_permit(&socket.0).is_ok() {
                    drop(card);
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn oversized_bridge_frame_is_bounded_and_releases_native_admission() {
    let gate = Arc::new(crate::reader_reservation::Gate::default());
    let (socket, listener) = SocketFixture::bind().unwrap();
    let worker = start_action(
        gate.clone(),
        Some(&socket.0),
        Some(TOKEN),
        Action::Scan,
        None,
    )
    .unwrap();
    let (mut stream, _) = read_bridge(&listener).await;
    let bytes = vec![b'x'; MAX_BRIDGE_RESPONSE_BYTES + 1];
    let _ = stream.write_all(&bytes).await;
    let response = tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        result_value(response).await["error"]
            .as_str()
            .unwrap()
            .contains("frame limit")
    );
    assert!(gate.admit(READER).is_ok());
}

#[test]
fn public_control_routes_are_explicit() {
    assert!(matches!(
        Action::from_path(&Method::POST, "/api/reader/ble/scan"),
        Some(Action::Scan)
    ));
    assert!(matches!(
        Action::from_path(&Method::POST, "/api/reader/ble/connect"),
        Some(Action::Connect)
    ));
    assert!(matches!(
        Action::from_path(&Method::POST, "/api/reader/ble/disconnect"),
        Some(Action::Disconnect)
    ));
    assert!(Action::from_path(&Method::GET, "/api/reader/ble/scan").is_none());
    assert!(Action::from_path(&Method::POST, "/api/internal/reader/ble-operation").is_none());
}

#[test]
fn lifecycle_body_requires_strong_id_and_complete_observation_binding() {
    let valid = br#"{"client_request_id":"123e4567-e89b-42d3-a456-426614174000","reader":"BLE:12345678-1234-1234-1234-123456789abc","reader_generation":0,"inventory_revision":9,"instance_id":"instance"}"#;
    let input = lifecycle_input(valid).unwrap();
    assert_eq!(input.reader_generation, 0);
    assert_eq!(input.inventory_revision, 9);

    for invalid in [
        br#"{"client_request_id":"weak","reader":"BLE:12345678-1234-1234-1234-123456789abc","reader_generation":0,"inventory_revision":9,"instance_id":"instance"}"#.as_slice(),
        br#"{"client_request_id":"123e4567-e89b-42d3-a456-426614174000","reader":"BLE:12345678-1234-1234-1234-123456789abc","reader_generation":0,"inventory_revision":0,"instance_id":"instance"}"#,
        br#"{"client_request_id":"123e4567-e89b-42d3-a456-426614174000","reader":"BLE:12345678-1234-1234-1234-123456789abc","reader_generation":0,"inventory_revision":9,"instance_id":"instance","extra":true}"#,
    ] {
        assert!(lifecycle_input(invalid).is_err());
    }
}
