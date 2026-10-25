//! Body-compatible native public retry admission; the lifecycle owns refresh,
//! physical reservation, runtime deployment, and the authoritative state.
use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use std::time::Duration;
const PUBLIC_PATH: &str = "/api/call-mode/retry";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    service: crate::call_mode_lifecycle::Service,
) -> Response<Full<Bytes>> {
    handle_public_with(request, || service.retry()).await
}

async fn handle_public_with<F, Fut>(request: Request<Incoming>, retry: F) -> Response<Full<Bytes>>
where
    F: FnOnce() -> Fut,
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
    // The established public endpoint ignores body contents, including empty,
    // null and malformed JSON, while retaining a bounded read.
    match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        MAX_REQUEST_BYTES,
        Duration::from_secs(3),
    )
    .await
    {
        Ok(_) => {}
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "请求体超过大小限制",
                "invalid_request",
            );
        }
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(
                StatusCode::REQUEST_TIMEOUT,
                "请求读取超时",
                "request_timeout",
            );
        }
    }
    retry().await
}
fn failure(status: StatusCode, error: &str, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &serde_json::json!({"error":error,"code":code}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    #[tokio::test]
    async fn legacy_body_contents_never_narrow_native_retry_admission() {
        for body in [
            b"".as_slice(),
            b"null",
            b"[]",
            b"{\"confirm\":true}",
            b"{} {}",
            b"{",
        ] {
            let (client_io, server_io) = tokio::io::duplex(128 * 1024);
            let server = tokio::spawn(async move {
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(server_io),
                        service_fn(|request| async {
                            Ok::<_, std::convert::Infallible>(
                                handle_public_with(request, || async {
                                    crate::json_response(
                                        StatusCode::ACCEPTED,
                                        &serde_json::json!({"state":"preparing"}),
                                    )
                                })
                                .await,
                            )
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
                        .method(Method::POST)
                        .uri(PUBLIC_PATH)
                        .body(Full::new(Bytes::copy_from_slice(body)))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED, "body: {body:?}");
            let value: serde_json::Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(value["state"], "preparing");
            drop(sender);
            connection.await.unwrap().unwrap();
            server.await.unwrap();
        }
    }
}
