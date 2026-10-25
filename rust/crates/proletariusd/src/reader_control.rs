//! HTTP compatibility projection over the native reader lifecycle.
use crate::{ServerState, json_response, reader_lifecycle::Failure};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
#[cfg(test)]
use serde_json::Value;
use serde_json::json;

#[cfg(test)]
pub(crate) const PATH: &str = "/api/internal/reader-service";

pub(crate) fn failure(error: Failure) -> Response<Full<Bytes>> {
    let mut value = json!({"code":error.code,"error":error.message});
    if error.recoverable {
        value["recoverable"] = json!(true);
    }
    json_response(error.status, &value)
}

fn error(status: StatusCode, code: &str, message: &str) -> Response<Full<Bytes>> {
    json_response(status, &json!({"code":code,"error":message}))
}

fn public_failure(path: &str, error: Failure) -> Response<Full<Bytes>> {
    if error.code == "backend_stopping" || error.code == "esim_operation_busy" {
        return failure(error);
    }
    if matches!(
        error.code,
        "reader_operation_active"
            | "card_busy"
            | "nwait_owns_reader"
            | "reader_lifecycle_pending"
            | "reader_native_lease_active"
            | "reader_busy"
    ) {
        return json_response(
            StatusCode::CONFLICT,
            &json!({"code":"reader_operation_active",
            "error":error.message,"recoverable":true}),
        );
    }
    let (code, prefix, recoverable) = match path {
        "/api/reader/connect" => ("reader_connect_failed", "连接读卡器失败", false),
        "/api/reader/disconnect" => ("reader_disconnect_failed", "断开读卡器失败", false),
        _ => ("reader_refresh_failed", "刷新 SIM 卡信息失败", true),
    };
    let mut value = json!({"code":code,"error":format!("{prefix}: {}", error.message)});
    if recoverable {
        value["recoverable"] = json!(true);
    }
    json_response(StatusCode::BAD_GATEWAY, &value)
}

async fn body(request: Request<Incoming>) -> Result<Bytes, Response<Full<Bytes>>> {
    match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        16384,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(body) => Ok(body),
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "请求格式错误",
        )),
        Err(crate::http_server::BodyReadError::Timeout) => Err(error(
            StatusCode::REQUEST_TIMEOUT,
            "request_timeout",
            "请求读取超时",
        )),
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LifecycleInput {
    client_request_id: String,
    reader: String,
    reader_generation: u64,
    inventory_revision: u64,
    instance_id: String,
    #[serde(default)]
    auto_connect: Option<proletarius_api_contract::ReaderAutoConnect>,
}

fn lifecycle_input(bytes: &[u8]) -> Result<LifecycleInput, &'static str> {
    let input: LifecycleInput = serde_json::from_slice(bytes).map_err(|_| "invalid_json")?;
    if input.reader.trim().is_empty()
        || input.reader.trim() != input.reader
        || input.reader.len() > 512
        || input.reader.chars().any(char::is_control)
        || input.inventory_revision == 0
        || input.instance_id.trim().is_empty()
        || input.instance_id.trim() != input.instance_id
        || !crate::reader_mutation_intent::valid_client_request_id(&input.client_request_id)
        || input.auto_connect.as_ref().is_some_and(|proposal| {
            proposal.reader != input.reader
                || proposal.reader_generation != input.reader_generation
                || proposal.inventory_revision != input.inventory_revision
                || proposal.instance_id != input.instance_id
        })
    {
        Err("invalid_json")
    } else {
        Ok(input)
    }
}

fn binding(input: &LifecycleInput) -> crate::reader_mutation_intent::Binding {
    crate::reader_mutation_intent::Binding {
        reader: input.reader.clone(),
        reader_generation: input.reader_generation,
        inventory_revision: input.inventory_revision,
        instance_id: input.instance_id.clone(),
    }
}

fn validate_current(
    state: &ServerState,
    path: &str,
    binding: &crate::reader_mutation_intent::Binding,
) -> Result<(), Response<Full<Bytes>>> {
    let selection = state.reader_gate.selection_snapshot();
    let inventory = state
        .reader_discovery
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .snapshot();
    let valid_common = !selection.pending
        && selection.generation == binding.reader_generation
        && inventory.revision == binding.inventory_revision
        && state.snapshot_instance_id == binding.instance_id;
    let valid_target = match path {
        "/api/reader/connect" => inventory
            .readers
            .iter()
            .any(|reader| reader.name == binding.reader),
        "/api/reader/disconnect" => selection.reader == binding.reader,
        _ => false,
    };
    if valid_common && valid_target {
        Ok(())
    } else {
        Err(error(
            StatusCode::CONFLICT,
            "stale_reader_binding",
            "读卡器目标、代次或设备清单已变化；未执行旧操作",
        ))
    }
}

pub(crate) async fn info(state: &ServerState) -> Response<Full<Bytes>> {
    match crate::reader_info_projection::capture(state).await {
        Ok(info) => json_response(StatusCode::OK, &info),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_info_unavailable",
            "读卡器信息暂不可用",
        ),
    }
}

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    state: &ServerState,
) -> Response<Full<Bytes>> {
    let path = request.uri().path().to_owned();
    let service = match state.reader_lifecycle() {
        Ok(service) => service,
        Err(e) => return failure(e),
    };
    if path == "/api/reader/refresh" {
        let state = state.clone();
        return match tokio::spawn(async move {
            match service.refresh().await {
                Ok(_) => info(&state).await,
                Err(error) => public_failure("/api/reader/refresh", error),
            }
        })
        .await
        {
            Ok(response) => response,
            Err(_) => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_operation_unconfirmed",
                "读卡器操作结果未确认",
            ),
        };
    }
    if !matches!(
        path.as_str(),
        "/api/reader/connect" | "/api/reader/disconnect"
    ) {
        return error(StatusCode::NOT_FOUND, "route_not_found", "route not found");
    }
    let bytes = match body(request).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    let input = match lifecycle_input(&bytes) {
        Ok(input) => input,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_json", "请求格式错误"),
    };
    let receipt_binding = binding(&input);
    let root = state.multistandby_store.data_root().to_path_buf();
    let state = state.clone();
    let validation_state = state.clone();
    let dispatch_path = path.clone();
    crate::reader_mutation_intent::execute(
        &root,
        &path,
        &input.client_request_id,
        &bytes,
        receipt_binding.clone(),
        || validate_current(&validation_state, &path, &receipt_binding),
        || async move {
            // A disconnected HTTP caller cannot abandon a submitted
            // connect/close. The durable receipt already fences replay.
            match tokio::spawn(async move {
                let result = match dispatch_path.as_str() {
                    "/api/reader/connect" => match input.auto_connect {
                        Some(proposal) => {
                            service
                                .connect_automatic(
                                    proposal,
                                    state.reader_discovery.clone(),
                                    state.snapshot_instance_id.clone(),
                                )
                                .await
                        }
                        None => service.connect(&input.reader).await,
                    },
                    "/api/reader/disconnect" => service.disconnect().await,
                    _ => unreachable!(),
                };
                match result {
                    Err(error) => public_failure(&dispatch_path, error),
                    Ok(_) if dispatch_path == "/api/reader/disconnect" => {
                        json_response(StatusCode::OK, &json!({"ok":true}))
                    }
                    Ok(_) => info(&state).await,
                }
            })
            .await
            {
                Ok(response) => response,
                Err(_) => error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "reader_operation_unconfirmed",
                    "读卡器操作结果未确认",
                ),
            }
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    #[test]
    fn lifecycle_request_requires_strong_identity_and_matching_auto_connect_proof() {
        let valid = br#"{"client_request_id":"123e4567-e89b-42d3-a456-426614174000","reader":"Reader","reader_generation":7,"inventory_revision":9,"instance_id":"instance","auto_connect":{"reader":"Reader","inventory_revision":9,"reader_generation":7,"instance_id":"instance"}}"#;
        let input = lifecycle_input(valid).unwrap();
        let proof = input.auto_connect.unwrap();
        assert_eq!(proof.reader, input.reader);
        assert_eq!(proof.inventory_revision, 9);
        assert_eq!(proof.reader_generation, 7);
        assert_eq!(proof.instance_id, "instance");
        for input in [
            br#"{"client_request_id":"123e4567-e89b-42d3-a456-426614174000","reader":"Other","reader_generation":7,"inventory_revision":9,"instance_id":"instance","auto_connect":{"reader":"Reader","inventory_revision":9,"reader_generation":7,"instance_id":"instance"}}"#.as_slice(),
            br#"{"client_request_id":"weak","reader":"Reader","reader_generation":7,"inventory_revision":9,"instance_id":"instance"}"#,
            br#"{"client_request_id":"123e4567-e89b-42d3-a456-426614174000","reader":"Reader","reader_generation":7,"inventory_revision":9,"instance_id":"instance","extra":true}"#,
        ] {
            assert!(lifecycle_input(input).is_err());
        }
        // Generation zero is a real, observable initial disconnected epoch.
        // It remains safe because the server also binds the inventory revision
        // and process instance before it dispatches the mutation.
        assert!(lifecycle_input(br#"{"client_request_id":"123e4567-e89b-42d3-a456-426614174000","reader":"Reader","reader_generation":0,"inventory_revision":9,"instance_id":"instance"}"#).is_ok());
    }

    #[tokio::test]
    async fn retired_private_reader_service_is_absent_and_public_info_remains_native() {
        use crate::reader_pcsc_gateway_tests::{request_with_body, state};
        let root = crate::tests::test_directory("native-reader-control-routing");
        let state = state(&root);
        let credential = Some(("X-Switch-Internal-Token", "internal-fixture"));
        for (public, stopping) in [(false, false), (true, false), (false, true)] {
            let response = request_with_body(
                state.clone(),
                PATH,
                public,
                stopping,
                credential,
                br#"{"producer_id":"current","action":"snapshot"}"#,
            )
            .await;
            assert_eq!(response.0, StatusCode::NOT_FOUND, "{response:?}");
        }
        let info_response = info(&state).await;
        let info = (
            info_response.status(),
            serde_json::from_slice::<Value>(
                &info_response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes(),
            )
            .unwrap(),
        );
        assert_eq!(info.0, StatusCode::OK);
        assert_eq!(info.1["status"]["connected"], false);
        assert!(info.1.get("usim_info").is_none());
        assert!(!state.go_socket_path.exists());
        state.reader_pcsc.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn public_validation_and_missing_card_errors_are_native_during_go_recovery() {
        use crate::reader_pcsc_gateway_tests::{request_with_body, state};
        let root = crate::tests::test_directory("native-reader-public-routing");
        let state = state(&root);
        let credential = Some(("X-Proletarius-App-Token", "app-fixture"));
        for (path, body, code, status) in [
            ("/api/reader/connect",br#"{"reader":"  "}"#.as_slice(),"invalid_json",StatusCode::BAD_REQUEST),
            ("/api/reader/refresh",b"{}".as_slice(),"reader_refresh_failed",StatusCode::BAD_GATEWAY),
            ("/api/reader/auth",br#"{"rand":"invalid"}"#.as_slice(),"invalid_rand",StatusCode::BAD_REQUEST),
            ("/api/reader/auth",br#"{"rand":"00000000000000000000000000000000","autn":"00000000000000000000000000000000"}"#.as_slice(),"auth_failed",StatusCode::BAD_GATEWAY),
        ] {
            let response = request_with_body(state.clone(),path,true,false,credential,body).await;
            assert_eq!(response.0,status,"{path}: {:?}",response);
            assert_eq!(response.1["code"],code);
        }
        assert!(!state.go_socket_path.exists());
        state.reader_pcsc.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
