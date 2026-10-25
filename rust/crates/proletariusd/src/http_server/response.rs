//! Bound only final-response transfer, below Hyper's body buffering. Body EOF
//! does not mean the socket has flushed. No owner, handler, or admission permit
//! is held by this observer; timing out this I/O cannot certify cleanup.
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, body::Incoming};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::future::Future;
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

#[derive(Clone, Copy)]
struct TransferPolicy {
    idle: Duration,
    base: Duration,
    bytes_per_second: u64,
    maximum: Duration,
}
impl TransferPolicy {
    // Conservative local-UDS defaults: 128 MiB receives 18m04s total, while a
    // non-reader retires 15s after the last actual write. Calibrate legitimate
    // slow-response workloads at G8; never renew the absolute transfer limit.
    const LOCAL: Self = Self {
        idle: Duration::from_secs(15),
        base: Duration::from_secs(60),
        bytes_per_second: 128 * 1024,
        maximum: Duration::from_secs(20 * 60),
    };
    fn total(self, bytes: u64) -> Duration {
        self.base
            .saturating_add(Duration::from_secs(bytes.div_ceil(self.bytes_per_second)))
            .min(self.maximum)
    }
    #[cfg(test)]
    fn fixed(idle: Duration, total: Duration) -> Self {
        Self {
            idle,
            base: total,
            bytes_per_second: u64::MAX,
            maximum: total,
        }
    }
}
#[derive(Clone, Copy)]
struct TransferPlan {
    ready: Instant,
    absolute: Instant,
    idle: Duration,
}

pub(crate) async fn serve_full_response<I, S, F>(io: I, service: S) -> Result<(), hyper::Error>
where
    I: AsyncRead + AsyncWrite + Unpin,
    S: Fn(Request<Incoming>) -> F,
    F: Future<Output = Result<Response<Full<Bytes>>, Infallible>>,
{
    serve_with_policy(io, service, TransferPolicy::LOCAL).await
}

#[cfg(test)]
pub(crate) async fn serve_full_response_with_deadlines<I, S, F>(
    io: I,
    service: S,
    idle: Duration,
    total: Duration,
) -> Result<(), hyper::Error>
where
    I: AsyncRead + AsyncWrite + Unpin,
    S: Fn(Request<Incoming>) -> F,
    F: Future<Output = Result<Response<Full<Bytes>>, Infallible>>,
{
    serve_with_policy(io, service, TransferPolicy::fixed(idle, total)).await
}

async fn serve_with_policy<I, S, F>(
    io: I,
    service: S,
    policy: TransferPolicy,
) -> Result<(), hyper::Error>
where
    I: AsyncRead + AsyncWrite + Unpin,
    S: Fn(Request<Incoming>) -> F,
    F: Future<Output = Result<Response<Full<Bytes>>, Infallible>>,
{
    let plan = Arc::new(OnceLock::new());
    let guarded = TransferIo::new(io, plan.clone());
    let invalid_target_line = guarded.request_line.invalid.clone();
    super::builder()
        .serve_connection(
            TokioIo::new(guarded),
            hyper::service::service_fn(move |request| {
                let plan = plan.clone();
                // http::Uri discards #fragment. Record only that raw syntax
                // fact before Hyper, then reject these two additive controls
                // before constructing/polling any business service future.
                let control_path = |path: &str| matches!(path.trim_end_matches('/'), "/api/reader/vowifi/stop-control" | "/api/vowifi/stop-control");
                let rejected = invalid_target_line.load(Ordering::Acquire)
                    && (control_path(request.uri().path()) || crate::public_http::public_alias_uri(request.uri()).is_some_and(|uri| control_path(uri.path())));
                let response = (!rejected).then(|| service(request));
                async move {
                    let response = match response {
                        Some(response) => response.await?,
                        None => {
                            let mut response = crate::json_response(hyper::StatusCode::BAD_REQUEST,
                                &serde_json::json!({"code":"invalid_vowifi_stop_target","error":"invalid_vowifi_stop_target"}));
                            response.headers_mut().insert(
                                hyper::header::CACHE_CONTROL,
                                hyper::header::HeaderValue::from_static("no-store"),
                            );
                            response
                        }
                    };
                    let ready = Instant::now();
                    let bytes = hyper::body::Body::size_hint(response.body())
                        .exact()
                        .unwrap_or(0);
                    // keep_alive(false): exactly one final application response.
                    // In particular, interim 100-Continue must not arm this limit.
                    let _ = plan.set(TransferPlan {
                        ready,
                        absolute: ready + policy.total(bytes),
                        idle: policy.idle,
                    });
                    Ok::<_, Infallible>(response)
                }
            }),
        )
        .await
}

// Constant memory, at most 8192 first-line bytes inspected; no business
// parsing or body buffering. Ordinary routes retain their existing policy.
#[derive(Default)]
struct RequestLineProbe {
    invalid: Arc<AtomicBool>,
    stage: u8,
    inspected: usize,
    done: bool,
}
impl RequestLineProbe {
    fn observe(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if self.done {
                break;
            }
            if self.inspected == 8192 {
                self.invalid.store(true, Ordering::Release);
                self.done = true;
                break;
            }
            self.inspected += 1;
            if *byte == b'\n' {
                self.done = true;
                break;
            }
            if *byte == b' ' {
                self.stage = self.stage.saturating_add(1);
            }
            if self.stage == 1 && *byte == b'#' {
                self.invalid.store(true, Ordering::Release);
            }
        }
    }
}

struct TransferIo<I> {
    request_line: RequestLineProbe,
    io: I,
    plan: Arc<OnceLock<TransferPlan>>,
    timer: Pin<Box<Sleep>>,
    last_progress: Option<Instant>,
}
impl<I> TransferIo<I> {
    fn new(io: I, plan: Arc<OnceLock<TransferPlan>>) -> Self {
        Self {
            request_line: RequestLineProbe::default(),
            io,
            plan,
            timer: Box::pin(tokio::time::sleep(Duration::ZERO)),
            last_progress: None,
        }
    }
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        let Some(plan) = self.plan.get() else {
            return Ok(());
        };
        let deadline = plan
            .absolute
            .min(self.last_progress.unwrap_or(plan.ready) + plan.idle);
        // Check before the underlying poll, even when it is always ready.
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "response transfer deadline",
            ));
        }
        if self.timer.deadline() != deadline {
            self.timer.as_mut().reset(deadline);
        }
        if self.timer.as_mut().poll(cx).is_ready() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "response transfer deadline",
            ));
        }
        Ok(())
    }
    fn progress(&mut self, result: &Poll<io::Result<usize>>) {
        if self.plan.get().is_some() && matches!(result, Poll::Ready(Ok(n)) if *n > 0) {
            self.last_progress = Some(Instant::now());
        }
    }
}
impl<I: AsyncRead + Unpin> AsyncRead for TransferIo<I> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.io).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.request_line.observe(&buf.filled()[before..]);
        }
        result
    }
}
impl<I: AsyncWrite + Unpin> AsyncWrite for TransferIo<I> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check(cx)?;
        let result = Pin::new(&mut self.io).poll_write(cx, buf);
        self.progress(&result);
        result
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.check(cx)?;
        let result = Pin::new(&mut self.io).poll_write_vectored(cx, bufs);
        self.progress(&result);
        result
    }
    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(cx)?;
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(cx)?;
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

#[cfg(test)]
#[path = "response_tests.rs"]
mod tests;
