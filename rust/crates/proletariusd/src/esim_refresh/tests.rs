use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn response(ok: bool) -> Response<Full<Bytes>> {
    crate::json_response(
        if ok {
            StatusCode::OK
        } else {
            StatusCode::BAD_GATEWAY
        },
        &json!({"profiles":[], "error":"synthetic"}),
    )
}
async fn value(response: Response<Full<Bytes>>) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn scenario(
    scope: &'static str,
    first: Evidence,
    second_ok: bool,
    change_generation: bool,
) -> (Value, usize, usize) {
    let calls = Arc::new(AtomicUsize::new(0));
    let sleeps = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicBool::new(true));
    let attempts = calls.clone();
    let generations = current.clone();
    let clocks = sleeps.clone();
    let result = run(
        scope,
        move || {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                Attempt {
                    response: response(n > 0 && second_ok),
                    evidence: if n == 0 {
                        first
                    } else {
                        Evidence {
                            succeeded: second_ok,
                            ..Evidence::preflight(true)
                        }
                    },
                }
            }
        },
        move || generations.load(Ordering::SeqCst),
        move |delay| {
            assert_eq!(delay, Duration::from_millis(800));
            clocks.fetch_add(1, Ordering::SeqCst);
            if change_generation {
                current.store(false, Ordering::SeqCst);
            }
            std::future::ready(())
        },
    )
    .await;
    (
        value(result).await,
        calls.load(Ordering::SeqCst),
        sleeps.load(Ordering::SeqCst),
    )
}

#[tokio::test]
async fn esim_refresh_both_prefixes_retry_only_preflight_transport_after_cleanup() {
    for scope in ["reader", "module"] {
        let (value, calls, sleeps) = scenario(scope, Evidence::preflight(true), true, false).await;
        assert_eq!((calls, sleeps), (2, 1));
        assert_eq!(value["refresh"]["scope"], scope);
        assert_eq!(value["refresh"]["phase"], "completed");
        assert_eq!(value["refresh"]["attempts"], 2);
        assert_eq!(value["refresh"]["retry_performed"], true);
        assert!(
            !value["refresh"]["operation_id"]
                .as_str()
                .unwrap()
                .is_empty()
        );
    }
}
#[tokio::test]
async fn esim_refresh_both_prefixes_submitted_unknown_never_retries() {
    for scope in ["reader", "module"] {
        let (value, calls, sleeps) = scenario(scope, Evidence::unknown(true), true, false).await;
        assert_eq!((calls, sleeps), (1, 0));
        assert_eq!(value["refresh"]["outcome"], "unknown");
    }
}
#[tokio::test]
async fn esim_refresh_both_prefixes_cleanup_pending_never_retries() {
    for scope in ["reader", "module"] {
        let (value, calls, sleeps) = scenario(scope, Evidence::preflight(false), true, false).await;
        assert_eq!((calls, sleeps), (1, 0));
        assert_eq!(value["refresh"]["outcome"], "cleanup_pending");
    }
}
#[tokio::test]
async fn esim_refresh_both_prefixes_generation_change_during_fake_delay_never_retries() {
    for scope in ["reader", "module"] {
        let (value, calls, sleeps) = scenario(scope, Evidence::preflight(true), true, true).await;
        assert_eq!((calls, sleeps), (1, 1));
        assert_eq!(value["refresh"]["outcome"], "generation_changed");
    }
}
#[tokio::test]
async fn esim_refresh_both_prefixes_second_failure_reaches_limit_without_third_attempt() {
    for scope in ["reader", "module"] {
        let (value, calls, sleeps) = scenario(scope, Evidence::preflight(true), false, false).await;
        assert_eq!((calls, sleeps), (2, 1));
        assert_eq!(value["refresh"]["outcome"], "retry_limit");
    }
}
#[tokio::test]
async fn esim_refresh_generic_failure_has_no_retry_and_stale_200_is_not_success() {
    for scope in ["reader", "module"] {
        let mut evidence = Evidence::default();
        evidence.cleanup_confirmed = true;
        let (value, calls, sleeps) = scenario(scope, evidence, true, false).await;
        assert_eq!((calls, sleeps), (1, 0));
        assert_eq!(value["refresh"]["outcome"], "failed");
        let response = run(
            scope,
            || async {
                Attempt {
                    response: response(true),
                    evidence: Evidence::unknown(true),
                }
            },
            || true,
            |_| std::future::ready(()),
        )
        .await;
        assert_eq!(value_of_outcome(response).await, "unknown");
    }
}
async fn value_of_outcome(response: Response<Full<Bytes>>) -> String {
    value(response).await["refresh"]["outcome"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn esim_refresh_waiter_cancel_does_not_cancel_owner_or_skip_cleanup_barrier() {
    for scope in ["reader", "module"] {
        let (entered, observed) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let released = Arc::new(Mutex::new(Some(released)));
        let entered = Arc::new(Mutex::new(Some(entered)));
        let calls = Arc::new(AtomicUsize::new(0));
        let owner_calls = calls.clone();
        let (owner_send, owner_recv) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            let owner = tokio::spawn(run(
                scope,
                move || {
                    let first = owner_calls.fetch_add(1, Ordering::SeqCst) == 0;
                    let receiver = released.lock().unwrap().take();
                    let sender = entered.lock().unwrap().take();
                    async move {
                        if let Some(sender) = sender {
                            sender.send(()).unwrap();
                        }
                        if let Some(receiver) = receiver {
                            receiver.await.unwrap();
                        }
                        Attempt {
                            response: response(!first),
                            evidence: Evidence {
                                succeeded: !first,
                                ..Evidence::preflight(true)
                            },
                        }
                    }
                },
                || true,
                |_| std::future::ready(()),
            ));
            owner_send.send(owner.abort_handle()).unwrap();
            owner.await.unwrap()
        });
        observed.await.unwrap();
        let owner = owner_recv.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert!(!owner.is_finished());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !owner.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn esim_refresh_pending_and_late_unknown_share_id_attempt_and_owner() {
    for scope in ["reader", "module"] {
        let id = format!("retained-{scope}-refresh");
        let worker_id = id.clone();
        let progress = Progress::default();
        let worker_progress = progress.clone();
        let (entered, observed) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let entered = Arc::new(Mutex::new(Some(entered)));
        let released = Arc::new(Mutex::new(Some(released)));
        let owner = Arc::new(AtomicBool::new(false));
        let physical_owner = owner.clone();
        let mut job = tokio::spawn(async move {
            run_with_id(
                &worker_id,
                scope,
                move || {
                    worker_progress.started();
                    let entered = entered.lock().unwrap().take().unwrap();
                    let released = released.lock().unwrap().take().unwrap();
                    let physical_owner = physical_owner.clone();
                    async move {
                        physical_owner.store(true, Ordering::SeqCst);
                        entered.send(()).unwrap();
                        released.await.unwrap();
                        physical_owner.store(false, Ordering::SeqCst);
                        Attempt {
                            response: response(false),
                            evidence: Evidence::unknown(true),
                        }
                    }
                },
                || true,
                |_| async { panic!("unknown attempt must never retry") },
            )
            .await
        });
        observed.await.unwrap();
        let pending = wait(&id, scope, &progress, &mut job, Duration::from_millis(1)).await;
        assert_eq!(pending.status(), StatusCode::SERVICE_UNAVAILABLE);
        let pending = value(pending).await;
        assert!(owner.load(Ordering::SeqCst));
        assert!(!job.is_finished());
        assert_eq!(pending["refresh"]["operation_id"], id);
        assert_eq!(pending["refresh"]["attempts"], 1);
        release.send(()).unwrap();
        let terminal = value(job.await.unwrap()).await;
        assert_eq!(
            terminal["refresh"]["operation_id"],
            pending["refresh"]["operation_id"]
        );
        assert_eq!(
            terminal["refresh"]["attempts"],
            pending["refresh"]["attempts"]
        );
        assert_eq!(terminal["refresh"]["outcome"], "unknown");
        assert_eq!(terminal["refresh"]["retry_performed"], false);
        assert_eq!(terminal["retryable"], false);
        assert!(!owner.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn esim_refresh_pending_retry_flag_reports_prior_worker_attempts() {
    for attempts in [0, 1, 2] {
        let body = value(pending("same-retained-id", "reader", attempts).await).await;
        assert_eq!(body["refresh"]["operation_id"], "same-retained-id");
        assert_eq!(body["refresh"]["attempts"], attempts);
        assert_eq!(body["refresh"]["retry_performed"], attempts > 1);
        assert_eq!(body["retryable"], false);
    }
}
