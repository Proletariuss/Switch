//! One Rust-owned refresh intent; HTTP waiters never decide physical retries.
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Response, StatusCode};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

pub(crate) const WAITER_TIMEOUT: Duration = Duration::from_secs(120);

// The HTTP waiter observes attempts; only the retained worker changes them.
#[derive(Clone, Default)]
pub(crate) struct Progress(Arc<AtomicU8>);
impl Progress {
    pub(crate) fn started(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

pub(crate) async fn wait(
    id: &str,
    scope: &'static str,
    progress: &Progress,
    job: &mut tokio::task::JoinHandle<Response<Full<Bytes>>>,
    timeout: Duration,
) -> Response<Full<Bytes>> {
    match tokio::time::timeout(timeout, job).await {
        Ok(Ok(response)) => response,
        // Dropping the waiter never aborts the worker or releases its owner.
        _ => pending(id, scope, progress.0.load(Ordering::Acquire)).await,
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Evidence {
    pub(crate) succeeded: bool,
    pub(crate) preflight_transport: bool,
    pub(crate) cleanup_confirmed: bool,
    pub(crate) submitted_or_unknown: bool,
}
impl Evidence {
    pub(crate) fn preflight(cleanup_confirmed: bool) -> Self {
        Self {
            preflight_transport: true,
            cleanup_confirmed,
            submitted_or_unknown: false,
            succeeded: false,
        }
    }
    pub(crate) fn unknown(cleanup_confirmed: bool) -> Self {
        Self {
            cleanup_confirmed,
            submitted_or_unknown: true,
            ..Self::default()
        }
    }
}

pub(crate) struct Attempt {
    pub(crate) response: Response<Full<Bytes>>,
    pub(crate) evidence: Evidence,
}

#[derive(Serialize)]
struct Outcome<'a> {
    operation_id: &'a str,
    scope: &'a str,
    phase: &'a str,
    outcome: &'a str,
    attempts: u8,
    retry_performed: bool,
    message: &'a str,
}

async fn decorate(response: Response<Full<Bytes>>, outcome: Outcome<'_>) -> Response<Full<Bytes>> {
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("infallible local body")
        .to_bytes();
    let mut value: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        json!({
            "code":"esim_refresh_response_invalid", "error":"无法确认卡片刷新结果",
        })
    });
    if !value.is_object() {
        value = json!({"code":"esim_refresh_response_invalid", "error":"无法确认卡片刷新结果"});
    }
    if matches!(outcome.outcome, "unknown" | "cleanup_pending") {
        value["retryable"] = json!(false);
        value["recoverable"] = json!(false);
    }
    value["refresh"] = json!(outcome);
    #[cfg(test)]
    crate::esim_test_support::record_refresh_receipt(value["refresh"].clone());
    crate::json_response(status, &value)
}

pub(crate) async fn run<A, AF, C, S, SF>(
    scope: &'static str,
    mut attempt: A,
    current: C,
    sleep: S,
) -> Response<Full<Bytes>>
where
    A: FnMut() -> AF,
    AF: Future<Output = Attempt>,
    C: Fn() -> bool,
    S: Fn(Duration) -> SF,
    SF: Future<Output = ()>,
{
    let id = crate::snapshot::new_instance_id();
    run_with_id(&id, scope, &mut attempt, current, sleep).await
}

pub(crate) async fn run_with_id<A, AF, C, S, SF>(
    id: &str,
    scope: &'static str,
    mut attempt: A,
    current: C,
    sleep: S,
) -> Response<Full<Bytes>>
where
    A: FnMut() -> AF,
    AF: Future<Output = Attempt>,
    C: Fn() -> bool,
    S: Fn(Duration) -> SF,
    SF: Future<Output = ()>,
{
    let mut attempts = 0;
    loop {
        if !current() {
            return decorate(crate::json_response(StatusCode::CONFLICT, &json!({
                "code":"esim_refresh_generation_changed", "error":"卡片连接已变化，请查看当前卡片状态", "retryable":false,
            })), Outcome { operation_id:&id, scope, phase:"blocked", outcome:"generation_changed",
                attempts, retry_performed: attempts > 1, message:"卡片连接已变化，本次刷新未继续" }).await;
        }
        attempts += 1;
        let Attempt { response, evidence } = attempt().await;
        if !current() {
            // A successful old-card read is not publishable for its replacement.
            return decorate(crate::json_response(StatusCode::CONFLICT, &json!({
                "code":"esim_refresh_generation_changed", "error":"卡片连接已变化，请查看当前卡片状态", "retryable":false,
            })), Outcome { operation_id:&id, scope, phase:"blocked", outcome:"generation_changed",
                attempts, retry_performed: attempts > 1, message:"卡片连接已变化，本次刷新结果未采用" }).await;
        }
        if response.status().is_success() && evidence.succeeded && evidence.cleanup_confirmed {
            return decorate(
                response,
                Outcome {
                    operation_id: &id,
                    scope,
                    phase: "completed",
                    outcome: "succeeded",
                    attempts,
                    retry_performed: attempts > 1,
                    message: if attempts > 1 {
                        "卡片信息已刷新（服务已完成一次安全重试）"
                    } else {
                        "卡片信息已刷新"
                    },
                },
            )
            .await;
        }
        let (outcome, message) = if !evidence.cleanup_confirmed {
            ("cleanup_pending", "前一次卡片读取清理未确认，本次未重试")
        } else if evidence.submitted_or_unknown {
            ("unknown", "卡片读取结果待确认，本次未重试")
        } else if evidence.preflight_transport && attempts == 1 {
            sleep(Duration::from_millis(800)).await;
            continue;
        } else if evidence.preflight_transport {
            ("retry_limit", "卡片信息仍无法读取，已达到本次一次重试上限")
        } else {
            ("failed", "本次卡片刷新未完成，服务未重试")
        };
        return decorate(
            response,
            Outcome {
                operation_id: &id,
                scope,
                phase: "failed",
                outcome,
                attempts,
                retry_performed: attempts > 1,
                message,
            },
        )
        .await;
    }
}

pub(crate) async fn pending(id: &str, scope: &'static str, attempts: u8) -> Response<Full<Bytes>> {
    decorate(crate::json_response(StatusCode::SERVICE_UNAVAILABLE, &json!({
        "code":"esim_refresh_cleanup_pending", "error":"卡片读取结果或清理仍待确认，本次意图未重发", "retryable":false,
    })), Outcome { operation_id:id, scope, phase:"pending", outcome:"cleanup_pending", attempts,
        retry_performed:attempts > 1, message:"卡片读取结果或清理仍待确认，本次意图未重发" }).await
}

#[cfg(test)]
mod tests;
