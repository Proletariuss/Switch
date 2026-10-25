//! Rust-owned public BLE scan/connect/disconnect operations.
//!
//! CoreBluetooth remains an app-owned Swift bridge. Rust holds the reader
//! actor's global BLE-control lease while the bridge action is in flight.
//! The detached owner retains admission even when the HTTP caller disappears.

use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::Deserialize;
#[cfg(test)]
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{Instant, timeout_at};
use zeroize::Zeroizing;

const MAX_BRIDGE_RESPONSE_BYTES: usize = 160_000;
const MAX_CONNECT_READER_BYTES: usize = 128;
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct BridgeResponse {
    ok: bool,
    #[serde(default, rename = "errorCode")]
    error_code: String,
}

#[derive(Clone, Copy)]
enum Action {
    Scan,
    Connect,
    Disconnect,
}

impl Action {
    fn from_path(method: &Method, path: &str) -> Option<Self> {
        if method != Method::POST {
            return None;
        }
        match path {
            "/api/reader/ble/scan" => Some(Self::Scan),
            "/api/reader/ble/connect" => Some(Self::Connect),
            "/api/reader/ble/disconnect" => Some(Self::Disconnect),
            _ => None,
        }
    }

    fn bridge_name(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Connect => "connect",
            Self::Disconnect => "disconnect",
        }
    }

    fn success(self) -> Response<Full<Bytes>> {
        match self {
            Self::Scan => crate::json_response(StatusCode::ACCEPTED, &json!({"scanning":true})),
            Self::Connect => {
                crate::json_response(StatusCode::ACCEPTED, &json!({"connecting":true}))
            }
            Self::Disconnect => crate::json_response(StatusCode::OK, &json!({"ok":true})),
        }
    }

    fn failure(self, message: String) -> Response<Full<Bytes>> {
        let (status, code) = match self {
            Self::Scan => (StatusCode::SERVICE_UNAVAILABLE, "ble_scan_failed"),
            Self::Connect => (StatusCode::BAD_GATEWAY, "ble_connect_failed"),
            Self::Disconnect => (StatusCode::BAD_GATEWAY, "ble_disconnect_failed"),
        };
        coded_error(status, code, &message, true)
    }
}

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    bridge_socket: Option<&Path>,
    app_token: Option<&str>,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    let Some(action) = Action::from_path(request.method(), request.uri().path()) else {
        return coded_error(
            StatusCode::NOT_FOUND,
            "route_not_found",
            "route not found",
            false,
        );
    };
    if matches!(action, Action::Scan) {
        let worker = match start_action(
            crate::reader_reservation::gate(),
            bridge_socket,
            app_token,
            action,
            None,
        ) {
            Ok(worker) => worker,
            Err(response) => return response,
        };
        return match worker.await {
            Ok(response) => response,
            Err(_) => coded_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_operation_release_unconfirmed",
                "蓝牙操作结果未确认，请先刷新读卡器状态",
                false,
            ),
        };
    }
    let bytes = match crate::http_server::collect_body(request.into_body(), 4096).await {
        Ok(bytes) => bytes,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Err(_) => {
            return coded_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "蓝牙读卡器请求格式无效",
                false,
            );
        }
    };
    let input = match lifecycle_input(&bytes) {
        Ok(input) => input,
        Err(response) => return response,
    };
    let binding = crate::reader_mutation_intent::Binding {
        reader: input.reader.clone(),
        reader_generation: input.reader_generation,
        inventory_revision: input.inventory_revision,
        instance_id: input.instance_id.clone(),
    };
    let root = state.multistandby_store.data_root().to_path_buf();
    let route = request_path(action);
    let validation_state = state.clone();
    let socket = bridge_socket.map(Path::to_path_buf);
    let token = app_token.map(str::to_owned);
    let dispatch_reader = input.reader.clone();
    crate::reader_mutation_intent::execute(
        &root,
        route,
        &input.client_request_id,
        &bytes,
        binding.clone(),
        || validate_current(&validation_state, action, &binding),
        || async move {
            let worker = match start_action(
                crate::reader_reservation::gate(),
                socket.as_deref(),
                token.as_deref(),
                action,
                Some(dispatch_reader),
            ) {
                Ok(worker) => worker,
                Err(response) => return response,
            };
            match worker.await {
                Ok(response) => response,
                Err(_) => coded_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "reader_operation_release_unconfirmed",
                    "蓝牙操作结果未确认，请先刷新读卡器状态",
                    false,
                ),
            }
        },
    )
    .await
}

fn request_path(action: Action) -> &'static str {
    match action {
        Action::Scan => "/api/reader/ble/scan",
        Action::Connect => "/api/reader/ble/connect",
        Action::Disconnect => "/api/reader/ble/disconnect",
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LifecycleInput {
    client_request_id: String,
    reader: String,
    reader_generation: u64,
    inventory_revision: u64,
    instance_id: String,
}

fn lifecycle_input(bytes: &[u8]) -> Result<LifecycleInput, Response<Full<Bytes>>> {
    let input: LifecycleInput = serde_json::from_slice(bytes).map_err(|_| {
        coded_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "蓝牙读卡器请求格式无效",
            false,
        )
    })?;
    if input.reader.trim().is_empty()
        || input.reader.trim() != input.reader
        || input.reader.len() > MAX_CONNECT_READER_BYTES
        || input.reader.contains('\0')
        || input.inventory_revision == 0
        || input.instance_id.trim().is_empty()
        || input.instance_id.trim() != input.instance_id
        || !crate::reader_mutation_intent::valid_client_request_id(&input.client_request_id)
    {
        return Err(coded_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "蓝牙读卡器请求格式无效",
            false,
        ));
    }
    Ok(input)
}

fn normalized_ble_reader(reader: &str) -> String {
    if reader.starts_with("BLE:") {
        reader.to_ascii_uppercase()
    } else {
        format!("BLE:{}", reader.to_ascii_uppercase())
    }
}

fn validate_current(
    state: &crate::ServerState,
    action: Action,
    binding: &crate::reader_mutation_intent::Binding,
) -> Result<(), Response<Full<Bytes>>> {
    let selection = state.reader_gate.selection_snapshot();
    let inventory = state
        .reader_discovery
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .snapshot();
    let target_matches = match action {
        Action::Connect => selection.reader.is_empty(),
        Action::Disconnect => {
            normalized_ble_reader(&selection.reader) == normalized_ble_reader(&binding.reader)
        }
        Action::Scan => true,
    };
    if !selection.pending
        && selection.generation == binding.reader_generation
        && inventory.revision == binding.inventory_revision
        && state.snapshot_instance_id == binding.instance_id
        && target_matches
    {
        Ok(())
    } else {
        Err(coded_error(
            StatusCode::CONFLICT,
            "stale_reader_binding",
            "蓝牙读卡器目标、代次或设备清单已变化；未执行旧操作",
            false,
        ))
    }
}

fn start_action(
    gate: Arc<crate::reader_reservation::Gate>,
    bridge_socket: Option<&Path>,
    app_token: Option<&str>,
    action: Action,
    reader: Option<String>,
) -> Result<tokio::task::JoinHandle<Response<Full<Bytes>>>, Response<Full<Bytes>>> {
    let ble_token = crate::multistandby_store::random_token().map_err(|_| {
        coded_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_operation_gate_unavailable",
            "读卡器占用状态暂不可用，未执行蓝牙操作",
            true,
        )
    })?;
    // Acquire before dispatch. Neither a cancelled HTTP future nor delayed
    // task scheduling may allow a new card owner to overtake this operation.
    let ble_permit = gate.acquire_ble_control(&ble_token).map_err(|_| {
        coded_error(
            StatusCode::CONFLICT,
            "reader_operation_active",
            "读卡器会话正在使用设备，请稍后重试",
            true,
        )
    })?;
    let socket = bridge_socket
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| action.failure("BLE bridge is not configured".into()))?
        .to_owned();
    let token = app_token
        .filter(|token| token.len() >= 32)
        .ok_or_else(|| action.failure("BLE bridge is not configured".into()))?;
    let token = Zeroizing::new(token.to_owned());
    let io_permit = crate::reader_ble_apdu::acquire_io_permit(&socket)
        .map_err(|error| action.failure(error))?;
    Ok(tokio::spawn(async move {
        let _io_permit = io_permit;
        let bridge_result = bridge_action(&socket, &token, action, reader.as_deref()).await;
        if ble_permit.release().is_err() {
            return coded_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_operation_release_unconfirmed",
                "读卡器操作已提交，但占用状态释放未确认；请先刷新读卡器状态",
                false,
            );
        }
        match bridge_result {
            Ok(()) => action.success(),
            Err(message) => action.failure(message),
        }
    }))
}

async fn bridge_action(
    socket: &Path,
    token: &str,
    action: Action,
    reader: Option<&str>,
) -> Result<(), String> {
    let deadline = Instant::now() + BRIDGE_TIMEOUT;
    let mut stream = timeout_at(deadline, UnixStream::connect(socket))
        .await
        .map_err(|_| "BLE bridge unavailable: context deadline exceeded".to_owned())?
        .map_err(|error| format!("BLE bridge unavailable: {error}"))?;
    let body = match reader {
        Some(reader) => json!({"token":token,"action":action.bridge_name(),"reader":reader}),
        None => json!({"token":token,"action":action.bridge_name()}),
    };
    let mut body =
        serde_json::to_vec(&body).map_err(|error| format!("BLE bridge request: {error}"))?;
    body.push(b'\n');
    timeout_at(deadline, stream.write_all(&body))
        .await
        .map_err(|_| "BLE bridge request: context deadline exceeded".to_owned())?
        .map_err(|error| format!("BLE bridge request: {error}"))?;

    let mut reader = BufReader::new(stream).take((MAX_BRIDGE_RESPONSE_BYTES + 1) as u64);
    let mut line = Vec::with_capacity(512);
    timeout_at(deadline, reader.read_until(b'\n', &mut line))
        .await
        .map_err(|_| "BLE bridge response: context deadline exceeded".to_owned())?
        .map_err(|error| format!("BLE bridge response: {error}"))?;
    if line.is_empty() || line.len() > MAX_BRIDGE_RESPONSE_BYTES || line.last() != Some(&b'\n') {
        return Err("BLE bridge response exceeded the frame limit or ended early".into());
    }
    line.pop();
    let response: BridgeResponse = serde_json::from_slice(&line)
        .map_err(|error| format!("BLE bridge malformed response: {error}"))?;
    if !response.ok {
        return Err(if response.error_code.is_empty() {
            "BLE bridge: ble_unknown_error".into()
        } else {
            format!("BLE bridge: {}", response.error_code)
        });
    }
    Ok(())
}

fn coded_error(
    status: StatusCode,
    code: &'static str,
    error: &str,
    recoverable: bool,
) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":error,"code":code,"recoverable":recoverable}),
    )
}

#[cfg(test)]
mod tests;
