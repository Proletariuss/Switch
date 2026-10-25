//! Native public call-mode restore confirmation and request admission.

use std::time::Duration;

use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::Deserialize;

const PUBLIC_PATH: &str = "/api/call-mode/restore";
const MAX_REQUEST_BYTES: usize = 4 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreRequest {
    #[serde(default)]
    confirm: bool,
    #[serde(default)]
    backup_id: Option<String>,
}

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    service: crate::call_mode_lifecycle::Service,
) -> Response<Full<Bytes>> {
    let (root, binding) = service.public_receipt_scope();
    let expected = binding.clone();
    handle_public_with(request, root, binding, |input| async move {
        service.start_public_restore(input, expected).await
    })
    .await
}

async fn handle_public_with<F, Fut>(
    request: Request<Incoming>,
    root: std::path::PathBuf,
    binding: String,
    dispatch: F,
) -> Response<Full<Bytes>>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Response<Full<Bytes>>>,
{
    if request.uri().path() != PUBLIC_PATH || request.uri().query().is_some() {
        return failure(StatusCode::NOT_FOUND, "route not found", "route_not_found");
    }
    if request.method() != Method::POST {
        return failure(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
            "method_not_allowed",
        );
    }
    let body = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        MAX_REQUEST_BYTES,
        Duration::from_secs(3),
    )
    .await
    {
        Ok(body) => body,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error":"请求体超过大小限制"}),
            );
        }
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::json_response(
                StatusCode::REQUEST_TIMEOUT,
                &serde_json::json!({"error":"请求读取超时"}),
            );
        }
    };
    let (id, stripped) = match crate::public_mutation_receipt::split(&body, true) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let input: RestoreRequest = match serde_json::from_slice(&stripped) {
        Ok(input) => input,
        Err(_) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error":"通话模式还原请求无效"}),
            );
        }
    };
    if !input.confirm {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"error":"需要确认后才会还原模块 USB 配置"}),
        );
    }
    let backup_id = input.backup_id.unwrap_or_default().trim().to_owned();
    if !crate::call_mode_backups::valid_backup_id(&backup_id) {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"error":"备份文件标识无效"}),
        );
    }

    crate::public_mutation_receipt::run(
        &root,
        PUBLIC_PATH,
        id.as_deref().unwrap(),
        &body,
        binding,
        || async { dispatch(backup_id).await },
    )
    .await
}

fn failure(status: StatusCode, error: &str, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &serde_json::json!({"error":error,"code":code}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    async fn response_for(
        body: &[u8],
        path: &str,
        method: Method,
    ) -> (StatusCode, Value, Vec<String>) {
        response_for_in(
            std::env::temp_dir().join(format!(
                "call-mode-public-{}",
                crate::snapshot::new_instance_id()
            )),
            body,
            path,
            method,
        )
        .await
    }
    async fn response_for_in(
        root: std::path::PathBuf,
        body: &[u8],
        path: &str,
        method: Method,
    ) -> (StatusCode, Value, Vec<String>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let (client_io, server_io) = tokio::io::duplex(128 * 1024);
        let server = tokio::spawn(async move {
            hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    TokioIo::new(server_io),
                    service_fn(move |request| {
                        let recorded = recorded.clone();
                        let root = root.clone();
                        async move {
                            Ok::<_, std::convert::Infallible>(
                                handle_public_with(
                                    request,
                                    root.clone(),
                                    "mock-module-1".into(),
                                    move |id| async move {
                                        recorded.lock().unwrap().push(id);
                                        crate::json_response(
                                            StatusCode::ACCEPTED,
                                            &json!({"state":"restoring_usb"}),
                                        )
                                    },
                                )
                                .await,
                            )
                        }
                    }),
                )
                .await
                .unwrap();
        });
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        let response = sender
            .send_request(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Full::new(Bytes::copy_from_slice(body)))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        drop(sender);
        connection.await.unwrap().unwrap();
        server.await.unwrap();
        let calls = calls.lock().unwrap().clone();
        (status, value, calls)
    }
    #[tokio::test]
    async fn confirmed_restore_dispatches_only_the_trimmed_valid_backup_id_once() {
        for id in [
            "usb-before-call-mode-test.json",
            "usb-before-restore-test.json",
            "usb-imported-test.json",
        ] {
            let body = serde_json::to_vec(&json!({"confirm":true,"client_request_id":"test-request-0001","backup_id":format!("  {id}  ")}))
                .unwrap();
            let (status, value, calls) = response_for(&body, PUBLIC_PATH, Method::POST).await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(value["state"], "restoring_usb");
            assert_eq!(calls, vec![id.to_owned()]);
        }
    }
    #[tokio::test]
    async fn restore_rejects_confirmation_null_unknown_and_bad_ids_before_dispatch() {
        for body in [
            b"".as_slice(),
            b"null",
            b"[]",
            b"{",
            br#"{}"#,
            br#"{"confirm":false,"client_request_id":"test-request-0001","backup_id":"usb-before-call-mode-test.json"}"#,
            br#"{"confirm":null}"#,
            br#"{"confirm":true,"client_request_id":"test-request-0001"}"#,
            br#"{"confirm":true,"client_request_id":"test-request-0001","backup_id":null}"#,
            br#"{"confirm":true,"client_request_id":"test-request-0001","backup_id":1}"#,
            br#"{"confirm":true,"client_request_id":"test-request-0001","backup_id":"usb-before-call-mode-test.json","extra":1}"#,
        ] {
            let (status, value, calls) = response_for(body, PUBLIC_PATH, Method::POST).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "body {body:?}");
            assert!(value["error"].is_string());
            assert!(calls.is_empty());
        }
        for id in [
            "",
            " ",
            "../usb-before-call-mode-test.json",
            "/usb-before-call-mode-test.json",
            "other.json",
            "usb-before-call-mode-test.JSON",
            "usb-before-call-mode-test/evil.json",
        ] {
            let body = serde_json::to_vec(
                &json!({"confirm":true,"client_request_id":"test-request-0001","backup_id":id}),
            )
            .unwrap();
            let (status, value, calls) = response_for(&body, PUBLIC_PATH, Method::POST).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(value["error"], "备份文件标识无效");
            assert!(calls.is_empty());
        }
        for (path, method, expected) in [
            (PUBLIC_PATH, Method::GET, StatusCode::METHOD_NOT_ALLOWED),
            (
                "/api/call-mode/restore?x=1",
                Method::POST,
                StatusCode::NOT_FOUND,
            ),
        ] {
            let (status, _, calls) = response_for(
                br#"{"confirm":true,"client_request_id":"test-request-0001","backup_id":"usb-before-call-mode-test.json"}"#,
                path,
                method,
            )
            .await;
            assert_eq!(status, expected);
            assert!(calls.is_empty());
        }
    }

    #[tokio::test]
    async fn public_restore_replays_original_lifecycle_after_successor_and_rejects_payload_drift() {
        let root = std::env::temp_dir().join(format!(
            "call-mode-replay-{}",
            crate::snapshot::new_instance_id()
        ));
        let body = r#"{"client_request_id":"lifecycle-original","confirm":true,"backup_id":"usb-before-call-mode-test.json"}"#;
        let (status, receipt, calls) =
            response_for_in(root.clone(), body.as_bytes(), PUBLIC_PATH, Method::POST).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(calls.len(), 1);
        let newer = body.replace("lifecycle-original", "lifecycle-successor");
        assert_eq!(
            response_for_in(root.clone(), newer.as_bytes(), PUBLIC_PATH, Method::POST)
                .await
                .2
                .len(),
            1
        );
        let (status, replay, calls) =
            response_for_in(root.clone(), body.as_bytes(), PUBLIC_PATH, Method::POST).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(receipt, replay);
        assert!(calls.is_empty());
        let changed = body.replace(
            "usb-before-call-mode-test.json",
            "usb-before-restore-successor.json",
        );
        let (status, _, calls) =
            response_for_in(root.clone(), changed.as_bytes(), PUBLIC_PATH, Method::POST).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(calls.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
