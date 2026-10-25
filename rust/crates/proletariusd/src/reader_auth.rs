//! Native selected-reader AKA endpoint. USB/PC/SC and BLE APDU traffic execute
//! through Rust owners; BLE system I/O remains behind the authenticated Swift
//! CoreBluetooth bridge.

use crate::reader_aka_binding::AuthenticationOperationError;
use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use proletarius_reader::{AkaApplication, AuthenticationError};
use proletarius_sim::AkaResult;
use serde::Deserialize;
use serde_json::json;
use std::sync::OnceLock;
use zeroize::{Zeroize, Zeroizing};

const PUBLIC_AUTH_BODY_LIMIT: usize = 8192;

#[derive(Deserialize)]
struct PublicAuthRequest {
    #[serde(default)]
    rand: String,
    #[serde(default)]
    autn: String,
    #[serde(default)]
    mode: String,
}

impl Drop for PublicAuthRequest {
    fn drop(&mut self) {
        self.rand.zeroize();
        self.autn.zeroize();
        self.mode.zeroize();
    }
}

/// Own the selected reader request without a Go authentication fallback.
pub(crate) async fn handle_public(
    request: Request<Incoming>,
    state: &crate::ServerState,
    reader: String,
) -> Response<Full<Bytes>> {
    let lease =
        match crate::reader_session_lease::NativeReaderSessionLease::acquire_for_card_operation(
            &state.go_socket_path,
            state.app_token.as_deref(),
            &reader,
            state.reader_gate.clone(),
            state.reader_pcsc.clone(),
        )
        .await
        {
            Ok(lease) => lease,
            Err(error) => {
                let busy = error.to_string().contains("card_busy")
                    || error.to_string().contains("nwait_owns_reader");
                return if busy {
                    coded_failure(
                        StatusCode::CONFLICT,
                        "reader_operation_active",
                        "读卡器正由 VoWiFi 或卡片会话使用",
                    )
                } else {
                    coded_failure(
                        StatusCode::BAD_GATEWAY,
                        "auth_failed",
                        "鉴权失败: 读卡器会话不可用",
                    )
                };
            }
        };

    if crate::reader_esim_download::selected_reader(state).as_deref() != Some(reader.as_str()) {
        return match lease.release().await {
            Ok(()) => coded_failure(
                StatusCode::BAD_GATEWAY,
                "auth_failed",
                "鉴权失败: 读卡器会话已变化",
            ),
            Err(_) => lease_release_failed(),
        };
    }
    let is_ble = reader.starts_with("BLE:");
    let context = if is_ble {
        None
    } else {
        match lease.pcsc_context(state.reader_pcsc.clone()) {
            Ok(context) => Some(context),
            Err(_) => {
                return match lease.release().await {
                    Ok(()) => coded_failure(
                        StatusCode::BAD_GATEWAY,
                        "auth_failed",
                        "鉴权失败: 读卡器会话已变化",
                    ),
                    Err(_) => lease_release_failed(),
                };
            }
        }
    };

    let (rand, autn) = match read_public_challenge(request).await {
        Ok(challenge) => challenge,
        Err(response) => return validation_after_release(lease, response).await,
    };

    // The detached owner holds the Rust lease and the pinned adapter/card
    // context until the submitted APDU transaction actually returns. Client
    // disconnects therefore cannot release the reader underneath a worker.
    let lease = std::sync::Arc::new(lease);
    let heartbeat = crate::reader_session_lease::NativeReaderSessionHeartbeat::start(lease.clone());
    let worker_lease = lease.clone();
    let selected_reader = reader;
    let ble_socket = std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from);
    let app_token = state.app_token.clone();
    let operation = tokio::spawn(async move {
        let iccid = worker_lease.identity.iccid.clone();
        let imsi = worker_lease.identity.imsi.clone();
        let card_result = worker_lease
            .run_card_worker(auth_pool(), move || {
                if selected_reader.starts_with("BLE:") {
                    let mut transport = crate::reader_ble_apdu::BleApduTransport::connect(
                        ble_socket.as_deref(),
                        app_token.as_deref(),
                        &selected_reader,
                    )?;
                    Ok(authenticate_on_transport(
                        &mut transport,
                        &iccid,
                        &imsi,
                        &rand,
                        &autn,
                    ))
                } else {
                    context
                        .expect("selected non-BLE reader has a PC/SC context")
                        .with_transaction(|transport| {
                            authenticate_on_transport(transport, &iccid, &imsi, &rand, &autn)
                        })
                        .map_err(|error| error.message)
                }
            })
            .await;
        let lease_lost = heartbeat.is_lost();
        heartbeat.stop().await;
        let release = worker_lease.release().await;
        (card_result, lease_lost, release)
    });

    match operation.await {
        Ok((Ok(result), false, Ok(()))) => public_result(result),
        Ok((Err(_), false, Ok(()))) => coded_failure(
            StatusCode::BAD_GATEWAY,
            "auth_failed",
            "鉴权失败: 读卡器任务未完成",
        ),
        Ok((_result, _, Err(_))) => lease_release_failed(),
        Ok((_result, true, Ok(()))) => coded_failure(
            StatusCode::BAD_GATEWAY,
            "auth_failed",
            "鉴权失败: 读卡器会话已失效",
        ),
        Err(_) => coded_failure(
            StatusCode::BAD_GATEWAY,
            "auth_failed",
            "鉴权失败: 读卡器任务未完成",
        ),
    }
}

fn auth_pool() -> &'static crate::blocking_pool::BlockingPool {
    static POOL: OnceLock<crate::blocking_pool::BlockingPool> = OnceLock::new();
    POOL.get_or_init(|| crate::blocking_pool::BlockingPool::new(1))
}

pub(crate) fn authenticate_on_transport<T: proletarius_sim::ApduTransport>(
    transport: &mut T,
    iccid: &str,
    imsi: &str,
    rand: &[u8; 16],
    autn: &[u8; 16],
) -> Result<AkaResult, AuthenticationError>
where
    T::Error: std::fmt::Display,
{
    // The legacy aka and eap-aka labels both select this same USIM EAP-AKA
    // sequence. Identity verification and AUTH stay on one reader lease.
    proletarius_reader::authenticate_bound_with_transport(
        transport,
        AkaApplication::Usim,
        iccid,
        imsi,
        rand,
        autn,
    )
}

fn decode_public_request(
    input: PublicAuthRequest,
) -> Result<(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>), Response<Full<Bytes>>> {
    let rand = decode_hex_challenge(&input.rand).ok_or_else(|| {
        coded_failure(
            StatusCode::BAD_REQUEST,
            "invalid_rand",
            "RAND 必须为 16 字节 hex 字符串",
        )
    })?;
    let autn = decode_hex_challenge(&input.autn).ok_or_else(|| {
        coded_failure(
            StatusCode::BAD_REQUEST,
            "invalid_autn",
            "AUTN 必须为 16 字节 hex 字符串",
        )
    })?;
    let mode = input.mode.trim().to_ascii_lowercase();
    if !mode.is_empty() && mode != "aka" && mode != "eap-aka" {
        return Err(coded_failure(
            StatusCode::BAD_REQUEST,
            "invalid_mode",
            "mode 必须为 aka 或 eap-aka",
        ));
    }
    Ok((rand, autn))
}

fn decode_hex_challenge(value: &str) -> Option<Zeroizing<[u8; 16]>> {
    if value.len() != 32 || !value.is_ascii() {
        return None;
    }
    let mut result = [0_u8; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        result[index] = (high << 4) | low;
    }
    Some(Zeroizing::new(result))
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn public_result(
    result: Result<Result<AkaResult, AuthenticationError>, String>,
) -> Response<Full<Bytes>> {
    match result {
        Ok(Ok(AkaResult::Success {
            mut res,
            mut ck,
            mut ik,
        })) => {
            let payload = json!({
                "res": encode_hex(&res),
                "ck": encode_hex(&ck),
                "ik": encode_hex(&ik),
            });
            res.zeroize();
            ck.zeroize();
            ik.zeroize();
            crate::json_response(StatusCode::OK, &payload)
        }
        Ok(Ok(AkaResult::SynchronizationFailure { .. })) => coded_failure(
            StatusCode::BAD_GATEWAY,
            "auth_failed",
            "鉴权失败: SIM AKA synchronization failure",
        ),
        Ok(Ok(AkaResult::AuthenticationReject)) => coded_failure(
            StatusCode::BAD_GATEWAY,
            "auth_failed",
            "鉴权失败: SIM 卡拒绝鉴权 (Authentication Reject)",
        ),
        Ok(Err(error)) => coded_failure(
            StatusCode::BAD_GATEWAY,
            "auth_failed",
            &format!("鉴权失败: {error}"),
        ),
        Err(error) => coded_failure(
            StatusCode::BAD_GATEWAY,
            "auth_failed",
            &format!("鉴权失败: {error}"),
        ),
    }
}

fn encode_hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

async fn validation_after_release(
    lease: crate::reader_session_lease::NativeReaderSessionLease,
    response: Response<Full<Bytes>>,
) -> Response<Full<Bytes>> {
    match lease.release().await {
        Ok(()) => response,
        Err(_) => lease_release_failed(),
    }
}

fn coded_failure(status: StatusCode, code: &str, message: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &json!({"error":message,"code":code}))
}

fn lease_release_failed() -> Response<Full<Bytes>> {
    coded_failure(
        StatusCode::SERVICE_UNAVAILABLE,
        "reader_operation_active",
        "读卡器会话释放未确认",
    )
}

async fn read_public_challenge(
    request: Request<Incoming>,
) -> Result<(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>), Response<Full<Bytes>>> {
    let bytes = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        PUBLIC_AUTH_BODY_LIMIT,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(body) => Zeroizing::new(body.to_vec()),
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => {
            return Err(coded_failure(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                "请求格式错误",
            ));
        }
        Err(crate::http_server::BodyReadError::Timeout) => {
            return Err(coded_failure(
                StatusCode::REQUEST_TIMEOUT,
                "request_timeout",
                "请求读取超时",
            ));
        }
    };
    let input = serde_json::from_slice::<PublicAuthRequest>(&bytes)
        .map_err(|_| coded_failure(StatusCode::BAD_REQUEST, "invalid_json", "请求格式错误"))?;
    decode_public_request(input)
}

pub(crate) async fn no_selection(request: Request<Incoming>) -> Response<Full<Bytes>> {
    match read_public_challenge(request).await {
        Err(response) => response,
        Ok(_) => coded_failure(
            StatusCode::BAD_GATEWAY,
            "auth_failed",
            "鉴权失败: 未连接读卡器",
        ),
    }
}

#[derive(Deserialize)]
struct AuthRequest {
    reader: String,
    #[serde(default)]
    pcsc: Option<crate::reader_pcsc::Binding>,
    #[serde(default)]
    transaction: Option<crate::reader_reservation::CardTransaction>,
    #[serde(default)]
    aid: Vec<u8>,
    #[serde(default)]
    application: Option<String>,
    #[serde(default)]
    expected_identity: Option<ExpectedIdentity>,
    rand: Vec<u8>,
    autn: Vec<u8>,
}

#[derive(Clone, PartialEq, Deserialize)]
pub(crate) struct ExpectedIdentity {
    pub(crate) iccid: String,
    pub(crate) imsi: String,
}

impl Drop for ExpectedIdentity {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.iccid.zeroize();
        self.imsi.zeroize();
    }
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    owner: crate::reader_pcsc::Owner,
    gate: std::sync::Arc<crate::reader_reservation::Gate>,
) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        8192,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(mut input) = serde_json::from_slice::<AuthRequest>(&body) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if !valid_input(&input) {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_authentication_input",
        );
    }
    if let Some(expected) = &input.expected_identity {
        let application = if input.application.as_deref() == Some("usim") {
            AkaApplication::Usim
        } else {
            AkaApplication::Isim
        };
        if crate::reader_aka_binding::ReaderAkaBinding::new(
            &input.reader,
            application,
            &expected.iccid,
            &expected.imsi,
        )
        .is_err()
        {
            return failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_authentication_input",
            );
        }
    }
    let worker_reader = input.reader.trim().to_owned();
    let transaction = input.transaction.take();
    let physical = input.pcsc.clone();
    let result = crate::reader_pcsc::run_card_work(
        owner.clone(),
        gate,
        &worker_reader,
        transaction.as_ref(),
        physical.as_ref(),
        move || {
            owner
                .with_transaction(input.pcsc.as_ref(), input.reader.trim(), |transport| {
                    if let Some(application) = input.application.as_deref() {
                        let application = match application {
                            "usim" => AkaApplication::Usim,
                            "isim" => AkaApplication::Isim,
                            _ => unreachable!("validated before spawning"),
                        };
                        if let Some(expected) = &input.expected_identity {
                            proletarius_reader::authenticate_bound_with_transport(
                                transport,
                                application,
                                &expected.iccid,
                                &expected.imsi,
                                &input.rand,
                                &input.autn,
                            )
                        } else {
                            proletarius_reader::authenticate_auto_with_transport(
                                transport,
                                application,
                                &input.rand,
                                &input.autn,
                            )
                        }
                    } else {
                        proletarius_reader::authenticate_with_transport(
                            transport,
                            &input.aid,
                            &input.rand,
                            &input.autn,
                        )
                    }
                })
                .map_err(AuthenticationOperationError::Owner)?
                .map_err(AuthenticationOperationError::Card)
        },
        std::time::Duration::from_secs(35),
    )
    .await;
    match result {
        Ok(Ok(AkaResult::Success { res, ck, ik })) => crate::json_response(
            StatusCode::OK,
            &json!({"outcome":"success","res":res,"ck":ck,"ik":ik}),
        ),
        Ok(Ok(AkaResult::SynchronizationFailure { auts })) => crate::json_response(
            StatusCode::OK,
            &json!({"outcome":"synchronization_failure","auts":auts}),
        ),
        Ok(Ok(AkaResult::AuthenticationReject)) => {
            crate::json_response(StatusCode::OK, &json!({"outcome":"authentication_reject"}))
        }
        Ok(Err(AuthenticationOperationError::Owner(error))) => owner_failure(error),
        Ok(Err(AuthenticationOperationError::Card(
            AuthenticationError::ApplicationUnavailable,
        ))) => failure(StatusCode::NOT_FOUND, "reader_application_unavailable"),
        Ok(Err(AuthenticationOperationError::Card(error))) => {
            // RAND/AUTN and derived key material are intentionally never
            // logged. The typed error only contains the PC/SC stage/status,
            // which is enough to diagnose reader ownership and APDU failures.
            crate::diagnostics::write_line(format_args!("switchd-rs reader AKA failed: {error}"));
            let code = error.failure_code();
            let status = match code {
                "reader_card_absent" | "reader_unavailable" | "reader_identity_changed" => {
                    StatusCode::CONFLICT
                }
                "reader_busy" => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::BAD_GATEWAY,
            };
            failure(status, code)
        }
        Err(crate::reader_task::ReaderTaskError::Busy) => {
            failure(StatusCode::CONFLICT, "card_busy")
        }
        Err(crate::reader_task::ReaderTaskError::WorkerFailed) => {
            failure(StatusCode::INTERNAL_SERVER_ERROR, "reader_task_failed")
        }
        Err(crate::reader_task::ReaderTaskError::Timeout) => {
            failure(StatusCode::GATEWAY_TIMEOUT, "reader_authentication_timeout")
        }
    }
}

fn valid_input(input: &AuthRequest) -> bool {
    !input.reader.trim().is_empty()
        && input
            .pcsc
            .as_ref()
            .is_none_or(|binding| binding.validate_for_reader(input.reader.trim()).is_ok())
        && input.expected_identity.as_ref().is_none_or(|identity| {
            input.application.is_some()
                && (18..=22).contains(&identity.iccid.len())
                && (5..=15).contains(&identity.imsi.len())
                && identity
                    .iccid
                    .bytes()
                    .chain(identity.imsi.bytes())
                    .all(|b| b.is_ascii_digit())
        })
        && input.rand.len() == 16
        && input.autn.len() == 16
        && match input.application.as_deref() {
            Some("usim" | "isim") => input.aid.is_empty(),
            None => (5..=16).contains(&input.aid.len()),
            _ => false,
        }
}

fn failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"读卡器 AKA 鉴权失败","code":code,"retryable":false}),
    )
}

/// Preserve the public reader API classifications while retaining the native
/// owner's submitted/uncertain facts. Private owner endpoints keep their own
/// more specific lease/session codes.
pub(crate) fn owner_failure(mut error: crate::reader_pcsc::Failure) -> Response<Full<Bytes>> {
    (error.code, error.status) = match error.code {
        "reader_pcsc_busy" => ("reader_busy", StatusCode::SERVICE_UNAVAILABLE),
        "reader_pcsc_card_absent" => ("reader_card_absent", StatusCode::CONFLICT),
        "reader_pcsc_not_found" => ("reader_unavailable", StatusCode::CONFLICT),
        "reader_pcsc_stale_session" | "reader_pcsc_lease_expired" => {
            ("reader_identity_changed", StatusCode::CONFLICT)
        }
        code => (code, error.status),
    };
    error.response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        path::PathBuf,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    const BLE_READER: &str = "BLE:12345678-1234-1234-1234-123456789abc";
    static NEXT_BLE_SOCKET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    fn ble_auth_responses() -> Vec<Vec<u8>> {
        let aid = [0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff, 1];
        let imsi = [0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68];
        let iccid = [0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6];
        let mut dir = vec![0x61, 11, 0x4f, 9];
        dir.extend_from_slice(&aid);
        dir.extend_from_slice(&[0x90, 0]);
        let mut success = vec![0xdb, 8];
        success.extend([1; 8]);
        success.push(16);
        success.extend([2; 16]);
        success.push(16);
        success.extend([3; 16]);
        success.extend([0x90, 0]);
        vec![
            vec![0x90, 0],
            vec![0x90, 0],
            dir.clone(),
            vec![0x90, 0],
            vec![0x90, 0],
            vec![8, 0x90, 0],
            [imsi.as_slice(), &[0x90, 0]].concat(),
            vec![0x6a, 0x82],
            vec![0x90, 0],
            vec![0x90, 0],
            vec![0x90, 0],
            [iccid.as_slice(), &[0x90, 0]].concat(),
            vec![0x90, 0],
            vec![0x90, 0],
            dir,
            vec![0x90, 0],
            success,
        ]
    }

    fn ble_socket_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "switch-reader-auth-{}-{unique}-{}.sock",
            std::process::id(),
            NEXT_BLE_SOCKET.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn start_fake_ble_auth_bridge(
        path: &PathBuf,
        apdu_responses: Vec<Vec<u8>>,
    ) -> thread::JoinHandle<Vec<serde_json::Value>> {
        let listener = UnixListener::bind(path).unwrap();
        thread::spawn(move || {
            let mut requests = Vec::with_capacity(apdu_responses.len() + 1);
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            requests.push(serde_json::from_str(&line).unwrap());
            serde_json::to_writer(
                &mut stream,
                &serde_json::json!({"ok": true, "reader": BLE_READER, "state": "connected",
                    "operation_tracking": true, "bridge_id": "fixture-bridge"}),
            )
            .unwrap();
            stream.write_all(b"\n").unwrap();

            for apdu_response in apdu_responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let operation_id = request["operation_id"].clone();
                let bridge_id = request["bridge_id"].clone();
                requests.push(request);
                serde_json::to_writer(
                    &mut stream,
                    &serde_json::json!({"ok": true, "hex": encode_hex(&apdu_response),
                        "operation_id": operation_id, "bridge_id": bridge_id, "terminal": true}),
                )
                .unwrap();
                stream.write_all(b"\n").unwrap();
            }
            requests
        })
    }

    #[tokio::test]
    async fn owner_failures_preserve_public_classification_and_uncertainty() {
        for (native_code, native_status, public_code, public_status) in [
            (
                "reader_pcsc_not_found",
                StatusCode::NOT_FOUND,
                "reader_unavailable",
                StatusCode::CONFLICT,
            ),
            (
                "reader_pcsc_card_absent",
                StatusCode::CONFLICT,
                "reader_card_absent",
                StatusCode::CONFLICT,
            ),
            (
                "reader_pcsc_busy",
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_busy",
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "reader_pcsc_stale_session",
                StatusCode::CONFLICT,
                "reader_identity_changed",
                StatusCode::CONFLICT,
            ),
            (
                "reader_pcsc_lease_expired",
                StatusCode::CONFLICT,
                "reader_identity_changed",
                StatusCode::CONFLICT,
            ),
            (
                "reader_pcsc_io_failed",
                StatusCode::BAD_GATEWAY,
                "reader_pcsc_io_failed",
                StatusCode::BAD_GATEWAY,
            ),
            (
                "reader_pcsc_outcome_unknown",
                StatusCode::GATEWAY_TIMEOUT,
                "reader_pcsc_outcome_unknown",
                StatusCode::GATEWAY_TIMEOUT,
            ),
        ] {
            for submitted in [false, true] {
                let response = owner_failure(crate::reader_pcsc::Failure {
                    status: native_status,
                    code: native_code,
                    message: "Reader operation was not confirmed".into(),
                    submitted,
                    uncertain: submitted,
                });
                assert_eq!(response.status(), public_status, "{native_code}");
                let body = response.into_body().collect().await.unwrap().to_bytes();
                let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(payload["code"], public_code, "{native_code}");
                assert_eq!(payload["submitted"], submitted, "{native_code}");
                assert_eq!(payload["uncertain"], submitted, "{native_code}");
                assert_eq!(payload["retryable"], false, "{native_code}");
            }
        }
    }

    #[test]
    fn rejects_partial_secrets_and_prefix_only_aid() {
        for (aid, rand, autn) in [
            (vec![0; 4], vec![0; 16], vec![0; 16]),
            (vec![0; 7], vec![0; 15], vec![0; 16]),
            (vec![0; 7], vec![0; 16], vec![0; 17]),
        ] {
            let input = AuthRequest {
                reader: "Reader".into(),
                pcsc: None,
                transaction: None,
                aid,
                application: None,
                expected_identity: None,
                rand,
                autn,
            };
            assert!(!valid_input(&input));
        }
    }

    #[test]
    fn automatic_selection_requires_known_application_and_no_aid() {
        let mut input = AuthRequest {
            reader: "Reader".into(),
            pcsc: None,
            transaction: None,
            aid: Vec::new(),
            application: Some("isim".into()),
            expected_identity: None,
            rand: vec![0; 16],
            autn: vec![0; 16],
        };
        assert!(valid_input(&input));
        input.aid = vec![0xa0; 8];
        assert!(!valid_input(&input));
        input.aid.clear();
        input.application = Some("sim".into());
        assert!(!valid_input(&input));
    }

    #[tokio::test]
    async fn public_auth_keeps_legacy_validation_order_and_codes() {
        let cases = [
            (
                PublicAuthRequest {
                    rand: "not-hex".into(),
                    autn: "also-not-hex".into(),
                    mode: "bad".into(),
                },
                "invalid_rand",
            ),
            (
                PublicAuthRequest {
                    rand: "00".repeat(16),
                    autn: "not-hex".into(),
                    mode: "bad".into(),
                },
                "invalid_autn",
            ),
            (
                PublicAuthRequest {
                    rand: "00".repeat(16),
                    autn: "11".repeat(16),
                    mode: "bad".into(),
                },
                "invalid_mode",
            ),
        ];
        for (input, expected_code) in cases {
            let response = decode_public_request(input).unwrap_err();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(payload["code"], expected_code);
            assert!(payload.get("recoverable").is_none());
        }
    }

    #[test]
    fn public_auth_accepts_both_legacy_modes_and_lowercases_hex() {
        for mode in ["", " AKA ", "EAP-AKA"] {
            let (rand, autn) = decode_public_request(PublicAuthRequest {
                rand: "Aa".repeat(16),
                autn: "bB".repeat(16),
                mode: mode.into(),
            })
            .unwrap();
            assert_eq!(&*rand, &[0xaa; 16]);
            assert_eq!(&*autn, &[0xbb; 16]);
        }
        assert_eq!(encode_hex(&[0x00, 0xab, 0xff]), "00abff");
    }

    #[tokio::test]
    async fn public_auth_projects_success_and_failure_like_the_legacy_route() {
        let response = public_result(Ok(Ok(AkaResult::Success {
            res: vec![0x00, 0xab],
            ck: [0xcd; 16],
            ik: [0xef; 16],
        })));
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["res"], "00ab");
        assert_eq!(payload["ck"], "cd".repeat(16));
        assert_eq!(payload["ik"], "ef".repeat(16));

        for (result, expected_message) in [
            (
                Ok(Ok(AkaResult::SynchronizationFailure { auts: [0; 14] })),
                "鉴权失败: SIM AKA synchronization failure",
            ),
            (
                Ok(Ok(AkaResult::AuthenticationReject)),
                "鉴权失败: SIM 卡拒绝鉴权 (Authentication Reject)",
            ),
        ] {
            let response = public_result(result);
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(payload["code"], "auth_failed");
            assert_eq!(payload["error"], expected_message);
        }
    }

    #[test]
    fn selected_ble_auth_runs_identity_bound_aka_through_the_app_bridge() {
        let path = ble_socket_path();
        let bridge = start_fake_ble_auth_bridge(&path, ble_auth_responses());
        let token = "a".repeat(32);
        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect_for_test(
            Some(&path),
            Some(&token),
            BLE_READER,
            std::sync::Arc::new(crate::reader_reservation::Gate::default()),
        )
        .unwrap();
        let result = authenticate_on_transport(
            &mut transport,
            "8901234567890123456",
            "234100896392986",
            &[0x11; 16],
            &[0x22; 16],
        )
        .unwrap();
        assert!(matches!(
            result,
            AkaResult::Success { res, ck, ik }
                if res == vec![1; 8] && ck == [2; 16] && ik == [3; 16]
        ));

        let requests = bridge.join().unwrap();
        assert_eq!(requests.len(), 18, "one status and seventeen APDUs");
        assert_eq!(requests[0]["action"], "status");
        assert_eq!(requests[0]["token"], token);
        let auth_commands: Vec<Vec<u8>> = requests[1..]
            .iter()
            .filter_map(|request| request["hex"].as_str())
            .map(|hex| {
                (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                    .collect()
            })
            .filter(|command: &Vec<u8>| command.get(1) == Some(&0x88))
            .collect();
        assert_eq!(auth_commands.len(), 1, "AKA AUTH APDU is submitted once");
        let auth_hex = encode_hex(&auth_commands[0]);
        assert!(auth_hex.contains(&"11".repeat(16)));
        assert!(auth_hex.contains(&"22".repeat(16)));
        assert!(
            requests[1..]
                .iter()
                .all(|request| { request["reader"] == BLE_READER && request["token"] == token })
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn selected_ble_auth_stops_on_reader_identity_change_before_auth_apdu() {
        let path = ble_socket_path();
        let bridge = start_fake_ble_auth_bridge(&path, ble_auth_responses()[..12].to_vec());
        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect_for_test(
            Some(&path),
            Some(&"b".repeat(32)),
            BLE_READER,
            std::sync::Arc::new(crate::reader_reservation::Gate::default()),
        )
        .unwrap();
        let error = authenticate_on_transport(
            &mut transport,
            "8901234567890123457",
            "234100896392986",
            &[0x11; 16],
            &[0x22; 16],
        )
        .unwrap_err();
        assert!(matches!(error, AuthenticationError::IdentityChanged));
        let requests = bridge.join().unwrap();
        assert_eq!(requests.len(), 13, "status plus identity reads only");
        assert!(!requests[1..].iter().any(|request| {
            request["hex"]
                .as_str()
                .is_some_and(|hex| hex.starts_with("0088"))
        }));
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
#[path = "reader_auth/transaction_tests.rs"]
mod transaction_tests;
