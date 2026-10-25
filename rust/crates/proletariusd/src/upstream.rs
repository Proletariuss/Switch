use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Body;
use hyper::header::{CONNECTION, HOST, HeaderValue};
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use proletarius_domain::BackendFailure;
#[cfg(test)]
use serde::Serialize;
#[cfg(test)]
use serde::de::DeserializeOwned;
use tokio::net::UnixStream;
use tokio::time::{Instant, timeout_at};

const APP_TOKEN_HEADER: &str = "x-proletarius-app-token";
const FORWARD_TIMEOUT: Duration = Duration::from_secs(35);
const LONG_OPERATION_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
struct RequestPolicy {
    timeout: Duration,
    max_response_bytes: usize,
}

fn forward_policy(method: &Method, path: &str) -> RequestPolicy {
    let long_mutation = matches!(*method, Method::POST | Method::PATCH | Method::DELETE)
        && matches!(
            path,
            "/api/sms/send"
                | "/api/vowifi"
                | "/api/reader/vowifi"
                | "/api/esim/switch"
                | "/api/esim/download"
                | "/api/esim/profile"
        );
    let storage_operation = *method == Method::POST
        && matches!(
            path,
            "/api/sms/delete" | "/api/sms/delete-sender" | "/api/sms/clear"
        );
    if long_mutation || storage_operation {
        RequestPolicy {
            timeout: LONG_OPERATION_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
        }
    } else {
        RequestPolicy {
            timeout: FORWARD_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
        }
    }
}

// Frozen POST oracle for the shared send/no-retry transport tests. Production
// callbacks use the authenticated private RPC helpers below.
#[cfg(test)]
async fn post_json_with_policy<I: Serialize, O: DeserializeOwned>(
    socket_path: &Path,
    app_token: Option<&str>,
    path: &'static str,
    input: &I,
    policy: RequestPolicy,
) -> Result<O, BackendFailure> {
    let body = serde_json::to_vec(input)
        .map_err(|error| BackendFailure::GoBackendUnavailable(error.to_string()))?;
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(HOST, HeaderValue::from_static("localhost"))
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|error| BackendFailure::GoBackendUnavailable(error.to_string()))?;
    if let Some(token) = app_token {
        let value = HeaderValue::from_str(token)
            .map_err(|error| BackendFailure::GoBackendUnavailable(error.to_string()))?;
        request.headers_mut().insert(APP_TOKEN_HEADER, value);
    }
    let response = send(request, socket_path, policy).await?;
    if !response.status().is_success() {
        let status = response.status();
        let message = String::from_utf8_lossy(response.body()).trim().to_owned();
        return Err(BackendFailure::GoBackendUnavailable(format!(
            "{path} returned {status}: {}",
            truncate_message(&message)
        )));
    }
    serde_json::from_slice(response.body()).map_err(|error| {
        BackendFailure::GoBackendUnavailable(format!("{path} response schema: {error}"))
    })
}

/// Authenticated, bounded hardware RPC. Return the status separately so the
/// Rust service can preserve public selection errors without exposing transport
/// diagnostics. This method never retries, including a lost execute response.
pub(crate) async fn post_internal_json(
    socket_path: &Path,
    app_token: Option<&str>,
    internal_token: &str,
    path: &'static str,
    input: &serde_json::Value,
    request_timeout: Duration,
) -> Result<(hyper::StatusCode, serde_json::Value), BackendFailure> {
    post_internal_json_bounded(
        socket_path,
        app_token,
        internal_token,
        path,
        input,
        request_timeout,
        64 * 1024,
    )
    .await
}

/// Raw USB SMS snapshots have a distinct bounded response budget; other
/// hardware operations retain their existing smaller limit.
pub(crate) async fn post_internal_json_bounded(
    socket_path: &Path,
    app_token: Option<&str>,
    internal_token: &str,
    path: &'static str,
    input: &serde_json::Value,
    request_timeout: Duration,
    max_response_bytes: usize,
) -> Result<(hyper::StatusCode, serde_json::Value), BackendFailure> {
    let body = serde_json::to_vec(input)
        .map_err(|error| BackendFailure::GoBackendUnavailable(error.to_string()))?;
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(HOST, "localhost")
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .header("x-switch-internal-token", internal_token);
    if let Some(token) = app_token {
        builder = builder.header(APP_TOKEN_HEADER, token);
    }
    let request = builder
        .body(Full::new(Bytes::from(body)))
        .map_err(|error| BackendFailure::GoBackendUnavailable(error.to_string()))?;
    let response = send(
        request,
        socket_path,
        RequestPolicy {
            timeout: request_timeout,
            max_response_bytes,
        },
    )
    .await?;
    let status = response.status();
    let value = serde_json::from_slice(response.body()).map_err(|_| {
        BackendFailure::GoBackendUnavailable("invalid hardware RPC response".into())
    })?;
    Ok((status, value))
}

#[cfg(test)]
async fn get_json_with_policy<T: DeserializeOwned>(
    socket_path: &Path,
    app_token: Option<&str>,
    path: &'static str,
    policy: RequestPolicy,
) -> Result<T, BackendFailure> {
    let mut request = Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Full::new(Bytes::new()))
        .map_err(|error| BackendFailure::GoBackendUnavailable(error.to_string()))?;
    // hyper accepts origin-form URIs for a Unix socket, but Go's net/http
    // server still requires the HTTP/1.1 Host header before routing.
    request
        .headers_mut()
        .insert(HOST, HeaderValue::from_static("localhost"));
    if let Some(token) = app_token {
        let value = HeaderValue::from_str(token)
            .map_err(|error| BackendFailure::GoBackendUnavailable(error.to_string()))?;
        request.headers_mut().insert(APP_TOKEN_HEADER, value);
    }
    let response = send(request, socket_path, policy).await?;
    if !response.status().is_success() {
        let status = response.status();
        let message = String::from_utf8_lossy(response.body()).trim().to_owned();
        return Err(BackendFailure::GoBackendUnavailable(format!(
            "{path} returned {status}: {}",
            truncate_message(&message)
        )));
    }
    serde_json::from_slice(response.body()).map_err(|error| {
        BackendFailure::GoBackendUnavailable(format!("{path} response schema: {error}"))
    })
}

async fn send<B>(
    mut request: Request<B>,
    socket_path: &Path,
    policy: RequestPolicy,
) -> Result<Response<Bytes>, BackendFailure>
where
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let deadline = Instant::now() + policy.timeout;
    let uncertain_on_failure = matches!(
        *request.method(),
        Method::POST | Method::PATCH | Method::DELETE
    ) && forward_policy(request.method(), request.uri().path()).timeout
        == LONG_OPERATION_TIMEOUT;
    // Every gateway request uses a dedicated Unix connection. Advertising
    // close prevents Go from waiting for reuse and avoids false shutdown
    // errors when the one-shot Hyper sender is dropped.
    request
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    let stream = timeout_at(deadline, UnixStream::connect(socket_path))
        .await
        .map_err(|_| BackendFailure::GoBackendUnavailable("connect timeout".into()))?
        .map_err(|error| BackendFailure::GoBackendUnavailable(error.to_string()))?;
    let (mut sender, connection) = timeout_at(
        deadline,
        hyper::client::conn::http1::handshake(TokioIo::new(stream)),
    )
    .await
    .map_err(|_| BackendFailure::GoBackendUnavailable("handshake timeout".into()))?
    .map_err(|error| BackendFailure::GoBackendUnavailable(error.to_string()))?;
    // The response path below reports connection, protocol, timeout, and body
    // failures to the caller. Once a one-shot response has completed, Go may
    // close the peer while Hyper is finishing its HTTP/1 shutdown; that
    // expected teardown must not be emitted as a false daemon error.
    let connection = tokio::spawn(async move {
        let _ = connection.await;
    });
    // A timed-out request must not leave a detached Hyper connection waiting
    // indefinitely. Closing this local connection does not imply that a card
    // write/SMS has been undone, so submitted mutations are never replayed.
    let _connection_guard = ConnectionGuard(connection.abort_handle());
    let response = timeout_at(deadline, sender.send_request(request))
        .await
        .map_err(|_| submitted_failure("response timeout", uncertain_on_failure))?
        .map_err(|error| submitted_failure(&error.to_string(), uncertain_on_failure))?;
    let (parts, body) = response.into_parts();
    let bytes = timeout_at(
        deadline,
        Limited::new(body, policy.max_response_bytes).collect(),
    )
    .await
    .map_err(|_| submitted_failure("body timeout", uncertain_on_failure))?
    .map_err(|error| submitted_failure(&error.to_string(), uncertain_on_failure))?
    .to_bytes();
    Ok(Response::from_parts(parts, bytes))
}

struct ConnectionGuard(tokio::task::AbortHandle);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn submitted_failure(detail: &str, uncertain: bool) -> BackendFailure {
    let message = if uncertain {
        format!("操作结果待确认；请刷新卡片操作状态或短信发送记录，勿重复提交。传输信息：{detail}")
    } else {
        detail.to_owned()
    };
    BackendFailure::GoBackendUnavailable(message)
}

fn truncate_message(message: &str) -> &str {
    let mut end = message.len().min(512);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;
    use tokio::task::JoinHandle;

    use super::{
        FORWARD_TIMEOUT, LONG_OPERATION_TIMEOUT, RequestPolicy, forward_policy,
        get_json_with_policy, post_json_with_policy, truncate_message,
    };
    use hyper::Method;

    static SOCKET_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn test_policy(timeout: Duration, max_response_bytes: usize) -> RequestPolicy {
        RequestPolicy {
            timeout,
            max_response_bytes,
        }
    }

    fn test_socket(label: &str) -> PathBuf {
        let sequence = SOCKET_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "proletarius-upstream-{}-{sequence}-{label}.sock",
            std::process::id()
        ))
    }

    #[test]
    fn esim_and_reader_mutations_do_not_use_the_short_gateway_timeout() {
        // Public SMS refresh is dispatched by Rust and never enters this
        // Go-forward timeout policy in supervised mode.
        for path in [
            "/api/esim/switch",
            "/api/esim/download",
            "/api/esim/profile",
            "/api/reader/vowifi",
            "/api/vowifi",
            "/api/sms/send",
            "/api/sms/delete",
            "/api/sms/delete-sender",
            "/api/sms/clear",
        ] {
            assert_eq!(
                forward_policy(&Method::POST, path).timeout,
                LONG_OPERATION_TIMEOUT
            );
        }
        assert_eq!(
            forward_policy(&Method::GET, "/api/sms/storage").timeout,
            FORWARD_TIMEOUT
        );
        for path in [
            "/api/sms/delete",
            "/api/sms/delete-sender",
            "/api/sms/clear",
            "/api/sms/storage/extra",
        ] {
            assert_eq!(forward_policy(&Method::GET, path).timeout, FORWARD_TIMEOUT);
        }
        for path in [
            "/api/sms/delete",
            "/api/sms/delete-sender",
            "/api/sms/clear",
            "/api/sms/storage",
        ] {
            for method in [Method::PATCH, Method::DELETE, Method::HEAD] {
                assert_eq!(forward_policy(&method, path).timeout, FORWARD_TIMEOUT);
            }
        }
        assert_eq!(
            forward_policy(&Method::POST, "/api/sms/storage").timeout,
            FORWARD_TIMEOUT
        );
        assert_eq!(
            forward_policy(&Method::GET, "/api/reader/vowifi").timeout,
            FORWARD_TIMEOUT
        );
        assert_eq!(
            forward_policy(&Method::GET, "/api/sms/send").timeout,
            FORWARD_TIMEOUT
        );
    }

    async fn spawn_response(
        label: &str,
        response: Vec<u8>,
        delay: Duration,
    ) -> (PathBuf, JoinHandle<Vec<u8>>) {
        let socket_path = test_socket(label);
        let listener = UnixListener::bind(&socket_path).expect("bind mock upstream");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept mock request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let count = stream.read(&mut chunk).await.expect("read mock request");
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if !response.is_empty() {
                stream
                    .write_all(&response)
                    .await
                    .expect("write mock response");
            }
            request
        });
        (socket_path, task)
    }

    fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        let message = "中".repeat(300);
        let truncated = truncate_message(&message);
        assert!(truncated.len() <= 512);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[tokio::test]
    async fn internal_get_sends_host_and_application_token() {
        let (socket, server) = spawn_response(
            "headers",
            http_response("200 OK", br#"{"ok":true}"#),
            Duration::ZERO,
        )
        .await;
        let value: Value = get_json_with_policy(
            &socket,
            Some("fixture-token"),
            "/api/health",
            test_policy(Duration::from_secs(1), 1024),
        )
        .await
        .expect("decode mock health");
        assert_eq!(value["ok"], true);
        let request = String::from_utf8(server.await.expect("mock server task"))
            .expect("request is HTTP text")
            .to_ascii_lowercase();
        assert!(request.starts_with("get /api/health http/1.1\r\n"));
        assert!(request.contains("host: localhost\r\n"));
        assert!(request.contains("x-proletarius-app-token: fixture-token\r\n"));
        let _ = fs::remove_file(socket);
    }

    #[tokio::test]
    async fn response_timeout_is_reported() {
        let (socket, server) =
            spawn_response("timeout", Vec::new(), Duration::from_millis(150)).await;
        let error = get_json_with_policy::<Value>(
            &socket,
            None,
            "/api/status",
            test_policy(Duration::from_millis(20), 1024),
        )
        .await
        .expect_err("slow response must time out");
        assert!(error.to_string().contains("response timeout"));
        let _ = server.await;
        let _ = fs::remove_file(socket);
    }

    #[tokio::test]
    async fn submitted_sms_timeout_is_uncertain_and_never_replayed() {
        let (socket, server) =
            spawn_response("sms-timeout", Vec::new(), Duration::from_millis(60)).await;
        let error = post_json_with_policy::<_, Value>(
            &socket,
            None,
            "/api/sms/send",
            &serde_json::json!({"phone":"fixture","message":"fixture"}),
            test_policy(Duration::from_millis(20), 1024),
        )
        .await
        .expect_err("slow submitted SMS must not be called a definite failure");
        assert!(error.to_string().contains("结果待确认"));
        assert!(error.to_string().contains("勿重复提交"));
        let request = server.await.expect("one request");
        assert_eq!(
            request
                .windows(b"POST /api/sms/send".len())
                .filter(|part| *part == b"POST /api/sms/send")
                .count(),
            1
        );
        let _ = fs::remove_file(socket);
    }

    #[tokio::test]
    async fn oversized_response_is_rejected_before_json_decode() {
        let body = vec![b'x'; 256];
        let (socket, server) =
            spawn_response("oversized", http_response("200 OK", &body), Duration::ZERO).await;
        let error = get_json_with_policy::<Value>(
            &socket,
            None,
            "/api/status",
            test_policy(Duration::from_secs(1), 32),
        )
        .await
        .expect_err("oversized response must fail");
        assert!(error.to_string().contains("length limit exceeded"));
        let _ = server.await;
        let _ = fs::remove_file(socket);
    }

    #[tokio::test]
    async fn malformed_json_and_error_status_remain_distinct() {
        let (schema_socket, schema_server) =
            spawn_response("schema", http_response("200 OK", b"{"), Duration::ZERO).await;
        let schema_error = get_json_with_policy::<Value>(
            &schema_socket,
            None,
            "/api/status",
            test_policy(Duration::from_secs(1), 1024),
        )
        .await
        .expect_err("malformed JSON must fail");
        assert!(schema_error.to_string().contains("response schema"));
        let _ = schema_server.await;
        let _ = fs::remove_file(schema_socket);

        let (status_socket, status_server) = spawn_response(
            "status",
            http_response("503 Service Unavailable", br#"{"error":"starting"}"#),
            Duration::ZERO,
        )
        .await;
        let status_error = get_json_with_policy::<Value>(
            &status_socket,
            None,
            "/api/status",
            test_policy(Duration::from_secs(1), 1024),
        )
        .await
        .expect_err("non-success status must fail");
        assert!(status_error.to_string().contains("503 Service Unavailable"));
        assert!(!status_error.to_string().contains("response schema"));
        let _ = status_server.await;
        let _ = fs::remove_file(status_socket);
    }
}
