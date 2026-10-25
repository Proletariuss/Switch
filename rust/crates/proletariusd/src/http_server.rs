//! Shared pre-dispatch HTTP ingress limits. Never wrap accepted card work in a timeout.
use hyper::server::conn::http1;
use hyper_util::rt::TokioTimer;
use std::time::Duration;

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(5);
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BodyReadError {
    Timeout,
    TooLarge,
    Invalid,
}

/// The entire ingress shares one absolute deadline, including empty frames and
/// tiny chunks. Only collection is cancelled: callers dispatch work afterwards.
pub(crate) async fn collect_body<B>(
    body: B,
    maximum_bytes: usize,
) -> Result<bytes::Bytes, BodyReadError>
where
    B: hyper::body::Body<Data = bytes::Bytes> + Unpin,
{
    collect_body_with_timeout(body, maximum_bytes, BODY_READ_TIMEOUT).await
}

pub(crate) async fn collect_body_with_timeout<B>(
    mut body: B,
    maximum_bytes: usize,
    budget: Duration,
) -> Result<bytes::Bytes, BodyReadError>
where
    B: hyper::body::Body<Data = bytes::Bytes> + Unpin,
{
    use http_body_util::BodyExt;
    let deadline = tokio::time::Instant::now() + budget;
    let mut collected = Vec::new();
    let mut frames_since_yield = 0;
    loop {
        // timeout_at alone may poll an always-ready stream first. Checking the
        // clock also bounds a peer producing endless empty/ready frames.
        if tokio::time::Instant::now() >= deadline {
            return Err(BodyReadError::Timeout);
        }
        let Some(frame) = tokio::time::timeout_at(deadline, body.frame())
            .await
            .map_err(|_| BodyReadError::Timeout)?
        else {
            return Ok(bytes::Bytes::from(collected));
        };
        let frame = frame.map_err(|_| BodyReadError::Invalid)?;
        if let Ok(data) = frame.into_data() {
            if data.len() > maximum_bytes.saturating_sub(collected.len()) {
                return Err(BodyReadError::TooLarge);
            }
            collected.extend_from_slice(&data);
        }
        frames_since_yield += 1;
        if frames_since_yield == 64 {
            tokio::task::yield_now().await;
            frames_since_yield = 0;
        }
    }
}

pub(crate) fn body_timeout_response() -> hyper::Response<http_body_util::Full<bytes::Bytes>> {
    crate::json_response(
        hyper::StatusCode::REQUEST_TIMEOUT,
        &serde_json::json!({
            "error":"请求体接收超时，操作尚未提交",
            "code":"request_body_timeout",
            "retryable":true
        }),
    )
}

pub(crate) fn builder() -> http1::Builder {
    with_header_timeout(HEADER_READ_TIMEOUT)
}

fn with_header_timeout(timeout: Duration) -> http1::Builder {
    let mut builder = http1::Builder::new();
    builder
        .keep_alive(false)
        .timer(TokioTimer::new())
        .header_read_timeout(timeout);
    builder
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::Response;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn incomplete_headers_expire_without_dispatching_a_request() {
        let (mut client, server) = tokio::io::duplex(4096);
        let dispatched = Arc::new(AtomicBool::new(false));
        let called = dispatched.clone();
        let task = tokio::spawn(async move {
            with_header_timeout(Duration::from_millis(30))
                .serve_connection(
                    TokioIo::new(server),
                    service_fn(move |_| {
                        called.store(true, Ordering::SeqCst);
                        async { Ok::<_, Infallible>(Response::new(Full::new(Bytes::new()))) }
                    }),
                )
                .await
        });
        client
            .write_all(b"POST /fixture HTTP/1.1\r\nHost: local\r\n")
            .await
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.unwrap_err().is_timeout());
        assert!(!dispatched.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn header_deadline_does_not_cancel_an_accepted_long_operation() {
        let (mut client, server) = tokio::io::duplex(4096);
        let dispatched = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let called = dispatched.clone();
        let worker_release = release.clone();
        let task = tokio::spawn(async move {
            with_header_timeout(Duration::from_millis(30))
                .serve_connection(
                    TokioIo::new(server),
                    service_fn(move |_| {
                        let called = called.clone();
                        let release = worker_release.clone();
                        async move {
                            called.notify_one();
                            release.notified().await;
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(
                                b"done",
                            ))))
                        }
                    }),
                )
                .await
        });
        client
            .write_all(b"POST /fixture HTTP/1.1\r\nHost: local\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), dispatched.notified())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(90)).await;
        let still_running = !task.is_finished();
        release.notify_one();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap().unwrap();
        assert!(still_running);
        assert!(response.ends_with(b"done"));
    }

    fn body_fixture() -> (
        tokio::io::DuplexStream,
        tokio::task::JoinHandle<Result<(), hyper::Error>>,
        Arc<AtomicBool>,
        Arc<tokio::sync::Semaphore>,
    ) {
        let (client, server) = tokio::io::duplex(4096);
        let dispatched = Arc::new(AtomicBool::new(false));
        let called = dispatched.clone();
        let capacity = Arc::new(tokio::sync::Semaphore::new(1));
        let slots = capacity.clone();
        let task = tokio::spawn(async move {
            builder()
                .serve_connection(
                    TokioIo::new(server),
                    service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let called = called.clone();
                        let slots = slots.clone();
                        async move {
                            // Same retained-handler ownership as the production
                            // request-body reservation; returning must release it.
                            let _permit = slots.acquire_owned().await.unwrap();
                            let response = match collect_body_with_timeout(
                                request.into_body(),
                                4,
                                Duration::from_millis(100),
                            )
                            .await
                            {
                                Ok(bytes) => {
                                    called.store(true, Ordering::SeqCst);
                                    Response::new(Full::new(bytes))
                                }
                                Err(BodyReadError::Timeout) => body_timeout_response(),
                                Err(BodyReadError::TooLarge) => Response::builder()
                                    .status(hyper::StatusCode::PAYLOAD_TOO_LARGE)
                                    .body(Full::new(Bytes::new()))
                                    .unwrap(),
                                Err(BodyReadError::Invalid) => Response::builder()
                                    .status(hyper::StatusCode::BAD_REQUEST)
                                    .body(Full::new(Bytes::new()))
                                    .unwrap(),
                            };
                            Ok::<_, Infallible>(response)
                        }
                    }),
                )
                .await
        });
        (client, task, dispatched, capacity)
    }

    async fn finish_body_fixture(
        mut client: tokio::io::DuplexStream,
        task: tokio::task::JoinHandle<Result<(), hyper::Error>>,
    ) -> Vec<u8> {
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        response
    }

    struct AlwaysReadyEmptyBody;

    impl hyper::body::Body for AlwaysReadyEmptyBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Infallible>>> {
            std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::new()))))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn always_ready_empty_frames_preserve_the_deadline_and_worker_fairness() {
        let heartbeat = Arc::new(AtomicBool::new(false));
        let observed = heartbeat.clone();
        let task = tokio::spawn(async move {
            observed.store(true, Ordering::SeqCst);
        });
        let started = tokio::time::Instant::now();
        let result =
            collect_body_with_timeout(AlwaysReadyEmptyBody, 4, Duration::from_millis(15)).await;
        assert_eq!(result, Err(BodyReadError::Timeout));
        // On one worker this stays false if a Ready body monopolizes the task
        // until its deadline. No await after collection may mask that failure.
        assert!(heartbeat.load(Ordering::SeqCst));
        assert!(started.elapsed() < Duration::from_secs(1));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn unfinished_body_times_out_before_dispatch_and_releases_capacity() {
        let (mut client, task, dispatched, capacity) = body_fixture();
        client
            .write_all(b"POST /fixture HTTP/1.1\r\nHost: local\r\nContent-Length: 4\r\n\r\nx")
            .await
            .unwrap();
        let response = finish_body_fixture(client, task).await;
        assert!(response.starts_with(b"HTTP/1.1 408"));
        assert!(String::from_utf8_lossy(&response).contains("request_body_timeout"));
        assert!(!dispatched.load(Ordering::SeqCst));
        assert_eq!(capacity.available_permits(), 1);
    }

    #[tokio::test]
    async fn tiny_chunks_keep_the_byte_limit_and_trailers_must_finish() {
        let (mut client, task, dispatched, capacity) = body_fixture();
        client.write_all(b"POST /fixture HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n1\r\nb\r\n1\r\nc\r\n1\r\nd\r\n0\r\n\r\n")
            .await.unwrap();
        let response = finish_body_fixture(client, task).await;
        assert!(response.starts_with(b"HTTP/1.1 200") && response.ends_with(b"abcd"));
        assert!(dispatched.load(Ordering::SeqCst));
        assert_eq!(capacity.available_permits(), 1);

        let (mut client, task, dispatched, capacity) = body_fixture();
        client.write_all(b"POST /fixture HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n1\r\nb\r\n1\r\nc\r\n1\r\nd\r\n1\r\ne\r\n0\r\n\r\n")
            .await.unwrap();
        let response = finish_body_fixture(client, task).await;
        assert!(response.starts_with(b"HTTP/1.1 413"));
        assert!(!dispatched.load(Ordering::SeqCst));
        assert_eq!(capacity.available_permits(), 1);

        let (mut client, task, dispatched, capacity) = body_fixture();
        client.write_all(b"POST /fixture HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n0\r\nX-Fixture: unfinished")
            .await.unwrap();
        let response = finish_body_fixture(client, task).await;
        assert!(response.starts_with(b"HTTP/1.1 408"));
        assert!(!dispatched.load(Ordering::SeqCst));
        assert_eq!(capacity.available_permits(), 1);
    }

    #[tokio::test]
    async fn trickle_chunks_do_not_reset_the_absolute_body_deadline() {
        let (mut client, task, dispatched, capacity) = body_fixture();
        client
            .write_all(
                b"POST /fixture HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();
        let started = tokio::time::Instant::now();
        // Each byte arrives comfortably within the budget, while the whole
        // framing exceeds it. Even progress without data cannot extend ingress.
        for byte in b"1\r\na\r\n" {
            if client.write_all(&[*byte]).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let response = finish_body_fixture(client, task).await;
        assert!(response.starts_with(b"HTTP/1.1 408"));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(!dispatched.load(Ordering::SeqCst));
        assert_eq!(capacity.available_permits(), 1);
    }

    #[tokio::test]
    async fn body_deadline_does_not_cancel_an_accepted_long_operation() {
        let (mut client, server) = tokio::io::duplex(4096);
        let dispatched = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let called = dispatched.clone();
        let worker_release = release.clone();
        let task = tokio::spawn(async move {
            builder()
                .serve_connection(
                    TokioIo::new(server),
                    service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let called = called.clone();
                        let release = worker_release.clone();
                        async move {
                            let bytes = collect_body_with_timeout(
                                request.into_body(),
                                4,
                                Duration::from_millis(30),
                            )
                            .await
                            .unwrap();
                            assert_eq!(&bytes[..], b"{}");
                            called.notify_one();
                            release.notified().await;
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(
                                b"done",
                            ))))
                        }
                    }),
                )
                .await
        });
        client
            .write_all(b"POST /fixture HTTP/1.1\r\nHost: local\r\nContent-Length: 2\r\n\r\n{}")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), dispatched.notified())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(90)).await;
        assert!(!task.is_finished());
        release.notify_one();
        let response = finish_body_fixture(client, task).await;
        assert!(response.ends_with(b"done"));
    }
}
