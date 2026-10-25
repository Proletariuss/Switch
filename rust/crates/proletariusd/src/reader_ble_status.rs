//! Rust-owned public BLE status projection over the app's CoreBluetooth bridge.
//! The bridge remains a narrow Swift system-framework adapter; public routing,
//! authentication, framing, bounds, and error projection live here.

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::{Instant, timeout_at};

const MAX_RESPONSE_BYTES: usize = 160_000;
const DIAL_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeDevice {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeResponse {
    ok: bool,
    #[serde(default)]
    reader: String,
    #[serde(default)]
    devices: Option<Vec<BridgeDevice>>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    error_code: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderPresence {
    Present,
    Absent,
    Indeterminate,
}

pub(crate) async fn handle_public(
    socket: Option<&Path>,
    token: Option<&str>,
) -> Response<Full<Bytes>> {
    match status(socket, token).await {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(message) => crate::json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({
                "error": message,
                "code": "ble_bridge_unavailable",
                "recoverable": true
            }),
        ),
    }
}

async fn status(socket: Option<&Path>, token: Option<&str>) -> Result<Value, String> {
    let socket = socket.filter(|path| !path.as_os_str().is_empty());
    let token = token.filter(|token| token.len() >= 32);
    let (Some(socket), Some(token)) = (socket, token) else {
        return Err("BLE bridge is not configured".into());
    };

    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let dial_deadline = (Instant::now() + DIAL_TIMEOUT).min(deadline);
    let mut stream = timeout_at(dial_deadline, UnixStream::connect(socket))
        .await
        .map_err(|_| {
            if Instant::now() >= deadline {
                "BLE bridge unavailable: context deadline exceeded".to_owned()
            } else {
                "BLE bridge unavailable: i/o timeout".to_owned()
            }
        })?
        .map_err(|error| format!("BLE bridge unavailable: {error}"))?;

    let mut request = serde_json::to_vec(&json!({"token": token, "action": "status"}))
        .map_err(|error| format!("BLE bridge request: {error}"))?;
    request.push(b'\n');
    timeout_at(deadline, stream.write_all(&request))
        .await
        .map_err(|_| "BLE bridge request: context deadline exceeded".to_owned())?
        .map_err(|error| format!("BLE bridge request: {error}"))?;

    let mut line = Vec::with_capacity(512);
    let mut frame_bytes = 0;
    loop {
        let mut byte = [0_u8; 1];
        let read = timeout_at(deadline, stream.read(&mut byte))
            .await
            .map_err(|_| "BLE bridge response: context deadline exceeded".to_owned())?
            .map_err(|error| format!("BLE bridge response: {error}"))?;
        if read == 0 {
            return Err("BLE bridge response: EOF".into());
        }
        frame_bytes += read;
        if frame_bytes > MAX_RESPONSE_BYTES {
            return Err("BLE bridge response: EOF".into());
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }

    let response: BridgeResponse = serde_json::from_slice(&line)
        .map_err(|error| format!("BLE bridge malformed response: {error}"))?;
    if !response.ok {
        let code = if response.error_code.is_empty() {
            "ble_unknown_error"
        } else {
            response.error_code.as_str()
        };
        return Err(format!("BLE bridge: {code}"));
    }

    let devices = response.devices.map_or(Value::Null, |devices| {
        Value::Array(
            devices
                .into_iter()
                .map(|device| json!({"id":device.id,"name":device.name}))
                .collect(),
        )
    });
    Ok(json!({
        "devices": devices,
        "connected": response.reader,
        "state": response.state,
        "error_code": response.error_code
    }))
}

/// Read the CoreBluetooth bridge directly for Rust-owned reader inventory and
/// hot-unplug decisions. Transitional states and bridge failures are
/// indeterminate; callers must never treat them as proof of physical removal.
pub(crate) async fn reader_status_facts(
    socket: Option<&Path>,
    token: Option<&str>,
    selected_reader: &str,
) -> Result<(Option<String>, ReaderPresence), String> {
    let value = status(socket, token).await?;
    let reader = value["connected"].as_str().unwrap_or_default();
    let state = value["state"].as_str().unwrap_or_default();
    if !reader.is_empty() && !crate::reader_ble_apdu::valid_ble_reader(reader) {
        return Err("BLE bridge returned an invalid reader identity".into());
    }
    match state {
        "connected" if !reader.is_empty() => Ok((
            Some(reader.to_owned()),
            if reader == selected_reader {
                ReaderPresence::Present
            } else {
                ReaderPresence::Absent
            },
        )),
        "connecting" | "discovering" | "reconnecting" => Ok((None, ReaderPresence::Indeterminate)),
        "idle" | "scanning" | "disconnected" | "failed" if reader.is_empty() => {
            Ok((None, ReaderPresence::Absent))
        }
        "" => Ok((None, ReaderPresence::Indeterminate)),
        _ => Err("BLE bridge returned an inconsistent reader state".into()),
    }
}

#[cfg(test)]
mod tests;
