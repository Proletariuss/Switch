//! Frozen pre-505 BLE HTTP oracle; this entire module is test-only.
//! Historical compatibility transport for retired Go logical-card consumers.
//! Every request carries its immutable Rust workflow capability. Admission is
//! checked before queueing, and both the workflow guard and physical bridge
//! slot survive cancellation until the actual operation has terminal proof.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use proletarius_sim::ApduTransport;
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

use crate::reader_ble_apdu::{BleApduTransport, decode_hex, valid_ble_reader};
use crate::reader_reservation::CardTransaction;

pub(crate) const PATH: &str = "/api/internal/reader-ble-transport";
pub(crate) const MAX_BODY: usize = 160_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CardRequest {
    producer_id: String,
    transaction: CardTransaction,
    action: String,
    reader: String,
    #[serde(default)]
    hex: String,
}

enum Command {
    Apdu(Zeroizing<Vec<u8>>),
    Reset,
    Metadata,
}

enum CommandOutput {
    Apdu(String),
    Reset,
    Metadata(proletarius_reader::ReaderSimMetadata),
}

fn parse(bytes: &[u8]) -> Result<(String, String, Command, CardTransaction), &'static str> {
    let request: CardRequest = serde_json::from_slice(bytes).map_err(|_| "invalid BLE request")?;
    if request.producer_id.is_empty() || request.producer_id.len() > 128 {
        return Err("invalid BLE producer identity");
    }
    let encoded = Zeroizing::new(request.hex);
    if !valid_ble_reader(&request.reader) {
        return Err("invalid BLE reader identity");
    }
    let command = match request.action.as_str() {
        "apdu" => Command::Apdu(Zeroizing::new(
            decode_hex(&encoded).map_err(|_| "invalid BLE APDU")?,
        )),
        "reset" if encoded.is_empty() => Command::Reset,
        "metadata" if encoded.is_empty() => Command::Metadata,
        _ => return Err("invalid BLE action"),
    };
    Ok((
        request.producer_id,
        request.reader,
        command,
        request.transaction,
    ))
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    socket: Option<&Path>,
    token: Option<&str>,
    stopping: bool,
) -> Response<Full<Bytes>> {
    handle_with_gate(
        request,
        socket,
        token,
        stopping,
        crate::reader_reservation::gate(),
    )
    .await
}

async fn handle_with_gate(
    request: Request<Incoming>,
    socket: Option<&Path>,
    token: Option<&str>,
    _stopping: bool,
    gate: std::sync::Arc<crate::reader_reservation::Gate>,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST || request.uri().path() != PATH {
        return failure(StatusCode::NOT_FOUND, "route_not_found", "route not found");
    }
    let body = match Limited::new(request.into_body(), MAX_BODY).collect().await {
        Ok(body) => Zeroizing::new(body.to_bytes().to_vec()),
        Err(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "ble_invalid_request",
                "BLE request exceeds the frame limit",
            );
        }
    };
    let (producer, reader, command, transaction) = match parse(&body) {
        Ok(value) => value,
        Err(error) => return failure(StatusCode::BAD_REQUEST, "ble_invalid_request", error),
    };
    // Shutdown never lends authority by reader name. Only an explicit, still
    // live workflow can finish its restoration; quiescence fences new scopes.
    let (Some(socket), Some(token)) = (
        socket.filter(|p| !p.as_os_str().is_empty()),
        token.filter(|v| v.len() >= 32),
    ) else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "ble_transport_unavailable",
            "BLE bridge is not configured",
        );
    };
    // spawn_blocking deliberately survives the waiting HTTP future. A dropped
    // client never cancels/replays a submitted card write or releases its slot.
    let worker = match execute(
        socket.to_owned(),
        Zeroizing::new(token.to_owned()),
        reader,
        producer,
        command,
        transaction,
        gate,
    ) {
        Ok(worker) => worker,
        Err(error) => {
            return failure(error.status, error.code, &error.message);
        }
    };
    match worker.await {
        Ok(Ok(CommandOutput::Apdu(answer))) => {
            crate::json_response(StatusCode::OK, &json!({"ok":true,"hex":answer}))
        }
        Ok(Ok(CommandOutput::Reset)) => {
            crate::json_response(StatusCode::OK, &json!({"ok":true,"hex":""}))
        }
        Ok(Ok(CommandOutput::Metadata(metadata))) => {
            crate::json_response(StatusCode::OK, &json!({"ok":true,"metadata":metadata}))
        }
        Ok(Err(error)) => {
            let code = safe_bridge_code(&error).unwrap_or("ble_transport_failed");
            failure(StatusCode::BAD_GATEWAY, code, &error)
        }
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "ble_transport_unavailable",
            "BLE transport worker failed; result is unconfirmed",
        ),
    }
}

#[derive(Debug)]
struct DispatchError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

fn execute(
    socket: PathBuf,
    token: Zeroizing<String>,
    reader: String,
    producer: String,
    command: Command,
    transaction: CardTransaction,
    gate: std::sync::Arc<crate::reader_reservation::Gate>,
) -> Result<tokio::task::JoinHandle<Result<CommandOutput, String>>, DispatchError> {
    // Admission must precede dispatch, not just the APDU write. Otherwise a
    // caller cancelled during status/worker queueing could let another card
    // transaction overtake this already accepted command.
    let transaction_guard = gate
        .track_card_io(&transaction, &producer, &reader)
        .map_err(|code| DispatchError {
            status: StatusCode::CONFLICT,
            code,
            message: "BLE workflow authority is no longer available; operation was not submitted"
                .into(),
        })?;
    let mut permit =
        crate::reader_ble_apdu::acquire_io_permit(&socket).map_err(|message| DispatchError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "ble_transport_busy",
            message,
        })?;
    permit
        .bind_go_reader(&reader, &producer, gate.clone())
        .map_err(|code| DispatchError {
            status: StatusCode::CONFLICT,
            code,
            message: "BLE request belongs to an unavailable producer; operation was not submitted"
                .into(),
        })?;
    Ok(tokio::task::spawn_blocking(move || {
        let _transaction_guard = transaction_guard;
        let mut transport = BleApduTransport::connect_reserved_with_gate(
            &socket,
            Some(&token),
            &reader,
            permit,
            gate,
        )?;
        match command {
            Command::Reset => transport.reset().map(|()| CommandOutput::Reset),
            Command::Apdu(apdu) => {
                let bytes = Zeroizing::new(transport.transmit(&apdu)?);
                use std::fmt::Write;
                let mut hex = String::with_capacity(bytes.len() * 2);
                for byte in bytes.iter() {
                    write!(&mut hex, "{byte:02x}").expect("String write cannot fail");
                }
                Ok(CommandOutput::Apdu(hex))
            }
            Command::Metadata => {
                proletarius_reader::read_sim_metadata_auto_with_transport(&mut transport)
                    .map(CommandOutput::Metadata)
                    .map_err(|error| error.to_string())
            }
        }
    }))
}

fn safe_bridge_code(error: &str) -> Option<&str> {
    let error = error.strip_prefix("exchange APDU: ").unwrap_or(error);
    let error = error
        .strip_prefix("APDU transport failed: ")
        .unwrap_or(error);
    let code = error.strip_prefix("BLE bridge: ")?;
    // Frozen Swift ReaderBLEBridge/BluetoothReaderErrorCode wire vocabulary.
    matches!(
        code,
        "ble_unauthorized"
            | "ble_timeout"
            | "ble_reader_not_found"
            | "ble_connect_rejected"
            | "ble_reader_changed"
            | "ble_invalid_apdu"
            | "ble_unknown_action"
            | "ble_unknown_error"
            | "bluetooth_unauthorized"
            | "bluetooth_powered_off"
            | "bluetooth_unsupported"
            | "bluetooth_initialization_timeout"
            | "bluetooth_connect_failed"
            | "bluetooth_connection_timeout"
            | "bluetooth_service_discovery_timeout"
            | "bluetooth_characteristic_discovery_timeout"
            | "bluetooth_notification_timeout"
            | "bluetooth_write_failed"
            | "bluetooth_disconnected"
            | "estk_service_missing"
            | "estk_characteristic_missing"
            | "estk_notifications_failed"
            | "estk_apdu_busy"
            | "estk_apdu_timeout"
            | "estk_apdu_malformed"
            | "reader_reset_unsupported"
    )
    .then_some(code)
}

fn failure(status: StatusCode, code: &str, error: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"code":code,"error":error,"retryable":false}),
    )
}

#[cfg(test)]
mod tests;
