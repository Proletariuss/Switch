//! Real Hyper ingress, durable queue replacement faults, and actual Worker
//! cancellation. All state and credentials are synthetic workspace fixtures.
use super::*;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use proletarius_forwarding::{ForwardingClock, JsonDocument, StateStoreError};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::future::{Future, poll_fn};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use tokio::io::AsyncReadExt;
use tokio::sync::oneshot;

const QUEUE_ACTION: &str = "/api/sms/forwarding/queue-action";

#[tokio::test(flavor = "current_thread")]
async fn queue_action_real_disk_stall_keeps_executor_and_cancelled_owner_alive() {
    use std::fs::OpenOptions;
    fn take_wake(wake: &Notify) -> bool {
        let mut notified = std::pin::pin!(wake.notified());
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        notified.as_mut().poll(&mut context).is_ready()
    }

    let root = tests::test_root();
    let task_id = seed(&root, true);
    let service = Service::new(&root);
    assert_eq!(
        wire(
            &service,
            "/api/sms/forwarding/configure",
            configuration("http://127.0.0.1:9")
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut payload = retry_body(&service, &task_id, "disk-stall-cancel-owner").await;
    payload["action"] = json!("cancel");
    let replay_payload = payload.clone();
    let (owner, worker) = {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        (active.queue_io.clone(), active.worker.clone())
    };
    assert!(!worker.take_new_work_notification_for_test());
    let _ = take_wake(&service.0.wake);
    let lock_path = root.join("actual-slow-disk.lock");
    let held = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    held.lock().unwrap();
    let (entered, entering) = std::sync::mpsc::channel();
    proletarius_forwarding::test_support::hold_next_replace(
        service.0.state_path.clone(),
        lock_path,
        entered,
    );
    let beats = Arc::new(AtomicUsize::new(0));
    let heartbeat = {
        let beats = beats.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(5)).await;
                beats.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    let action = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .handle(
                    Request::builder()
                        .method(Method::POST)
                        .uri(QUEUE_ACTION)
                        .body(Full::new(Bytes::from(
                            serde_json::to_vec(&payload).unwrap(),
                        )))
                        .unwrap(),
                )
                .await
        })
    };
    let abort = action.abort_handle();
    let (release_ready, can_release) = std::sync::mpsc::channel();
    let (entered_signal, entered_wait) = oneshot::channel();
    let probe = std::thread::spawn(move || {
        entering.recv_timeout(Duration::from_secs(10)).unwrap();
        entered_signal.send(()).unwrap();
        let before = beats.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        let during = beats.load(Ordering::SeqCst).saturating_sub(before);
        println!("queue_action_real_disk_heartbeat_during_150ms={during}");
        abort.abort();
        // The timeout only prevents a RED baseline from hanging the test.
        // Production never uses a timeout to release the queue owner.
        let handshake = can_release.recv_timeout(Duration::from_secs(3)).is_ok();
        drop(held);
        (during, handshake)
    });
    entered_wait.await.unwrap();
    assert!(action.await.unwrap_err().is_cancelled());
    assert_eq!(
        owner.test_snapshot().0,
        1,
        "cancellation released the in-flight disk command"
    );
    assert!(!worker.take_new_work_notification_for_test());
    assert!(!take_wake(&service.0.wake));
    drop(worker);
    let mut stopping = Box::pin(service.shutdown());
    assert!(poll_fn(|cx| Poll::Ready(stopping.as_mut().poll(cx).is_pending())).await);
    drop(stopping);
    assert!(service.0.state.lock().await.active.is_some());
    release_ready.send(()).unwrap();
    let (during, handshake) = tokio::task::spawn_blocking(move || probe.join().unwrap())
        .await
        .unwrap();
    assert!(
        handshake,
        "runtime could not acknowledge the held disk operation"
    );
    assert!(
        during >= 3,
        "Tokio protocol executor stalled during actual queue replacement"
    );
    service.shutdown().await.unwrap();
    assert_eq!(owner.test_snapshot().0, 0);
    let key = StateKey::from_slice(&[17; 32]).unwrap();
    let queue = QueueStore::open(
        root.join("sms-forwarding/state.enc"),
        key,
        StateLimits::default(),
    )
    .unwrap();
    let summary = queue.inspect().unwrap();
    assert!(summary.tasks.iter().all(|task| task.id != task_id));
    assert_eq!(
        summary
            .recent
            .iter()
            .find(|task| task.id == task_id)
            .unwrap()
            .telegram
            .state,
        "cancelled"
    );
    drop(queue);
    let (replay_status, replay) = wire(&service, QUEUE_ACTION, replay_payload).await;
    assert_eq!(replay_status, StatusCode::CONFLICT);
    assert_eq!(replay["mutation_receipt"]["outcome"], "unknown");
    heartbeat.abort();
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

fn configuration(proxy: &str) -> Value {
    json!({
        "client_request_id":"attempt-owner-configure", "client_sequence":1,
        "telegram":{"enabled":true,"bot_token":"synthetic-token","chat_id":"synthetic-chat",
            "proxy":{"mode":"manual","url":proxy}},
        "bark":{"enabled":false}, "state_key":STANDARD.encode([17;32])
    })
}

async fn wire(service: &Service, path: &str, body: Value) -> (StatusCode, Value) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let owner = service.clone();
    let serving = tokio::spawn(async move {
        hyper::server::conn::http1::Builder::new()
            .keep_alive(false)
            .serve_connection(
                TokioIo::new(server),
                service_fn(move |request| {
                    let owner = owner.clone();
                    async move { Ok::<_, Infallible>(owner.handle(request).await) }
                }),
            )
            .await
            .unwrap();
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let response = sender
        .send_request(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("host", "fixture.invalid")
                .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    drop(sender);
    connection.await.unwrap().unwrap();
    serving.await.unwrap();
    (status, value)
}

fn seed(root: &Path, unknown: bool) -> String {
    let key = StateKey::from_slice(&[17; 32]).unwrap();
    let telegram = DestinationBinding::telegram(&key, "synthetic-token", "synthetic-chat");
    let bark = DestinationBinding::bark(&key, "https://api.day.app", "").unwrap();
    let limits = StateLimits::default();
    let document = JsonDocument::from_bytes(
        Zeroizing::new(br#"{"seen":{},"pending":{}}"#.to_vec()),
        limits,
    )
    .unwrap();
    let message = ForwardingMessage::new(
        "attempt-owner-message",
        "in",
        "synthetic-sender",
        "synthetic-body",
        "2026-01-01T00:00:00Z",
        "synthetic-reader",
    )
    .unwrap();
    let (document, _) = document
        .enqueue(
            &[message],
            "2026-01-01T00:00:00Z",
            &EnqueueOptions::new(true, &telegram, false, &bark),
            limits,
        )
        .unwrap();
    let id = document.inspect_go_queue().unwrap().tasks[0].id.clone();
    let mut value: Value = serde_json::from_slice(document.as_bytes()).unwrap();
    if unknown {
        // Offline synthetic fixture, not an executed provider attempt. Public
        // seal/open and QueueStore validate this real SWF1 before the HTTP test.
        let task = &mut value["pending"][&id];
        task["telegram_try"] = json!(1);
        task["telegram_meta"]["result_unknown"] = json!(true);
        task["telegram_meta"]["error_code"] = json!("telegram_submission_unconfirmed");
        task["telegram_meta"]["last_attempt_at"] = json!("2026-01-01T00:00:00Z");
    }
    let document =
        JsonDocument::from_bytes(Zeroizing::new(serde_json::to_vec(&value).unwrap()), limits)
            .unwrap();
    document.inspect_go_queue().unwrap();
    let directory = root.join("sms-forwarding");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.join("state.enc");
    fs::write(
        &path,
        proletarius_forwarding::seal(&key, &document, limits).unwrap(),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    id
}

async fn retry_body(service: &Service, task_id: &str, request_id: &str) -> Value {
    let state = service.0.state.lock().await;
    let active = state.active.as_ref().unwrap();
    let queue = active.worker.queue().lock().await;
    let summary = queue
        .inspect_for_bindings(Some(&active.telegram_binding), Some(&active.bark_binding))
        .unwrap();
    let task = summary
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .unwrap();
    let now = SystemForwardingClock.now_rfc3339().unwrap();
    let status = pending_channel(&task.telegram, true, &now, true);
    json!({"client_request_id":request_id,"id":task_id,"provider":"telegram","action":"retry",
        "expected_configuration_id":active.id,"expected_attempts":task.telegram.attempts,
        "expected_state":status.state,
        "expected_destination_revision":destination_revision(&active.id, task_id, "telegram", &task.updated_at, &status)})
}

/// Stops and joins the real autonomous runner without retiring the configured
/// generation. Direct action-classification tests can then poison the shared
/// queue without racing a periodic provider status observer. The caller must
/// record the observed join result before invoking structured shutdown.
async fn quiesce_real_runner(service: &Service) {
    let runner = {
        let mut state = service.0.state.lock().await;
        let active = state.active.as_mut().unwrap();
        active.shutdown.send(true).unwrap();
        active.runner.take().expect("configured real runner")
    };
    runner.await.unwrap();
}

async fn record_quiesced_runner(service: &Service) {
    let mut state = service.0.state.lock().await;
    let active = state.active.as_mut().unwrap();
    assert!(active.runner.is_none());
    assert!(active.runner_result.is_none());
    active.runner_result = Some(Ok(()));
}

async fn wait_provider_disk_settled(
    worker: &ForwardingWorker<SystemForwardingClock>,
) -> (usize, bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = worker.test_disk_snapshot();
            if snapshot.0 == 0 {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider disk owner did not settle")
}

#[tokio::test(flavor = "current_thread")]
async fn wire_retry_aftercommit_keeps_public_id_unknown_and_reopen_observes_prior_authorization() {
    let root = tests::test_root();
    let task_id = seed(&root, true);
    let service = Service::new(&root);
    // No provider work is due: the seed is Unknown. The manual proxy is never
    // contacted, and no other QueueStore replacement can consume the TLS hook.
    let (status, _) = wire(
        &service,
        "/api/sms/forwarding/configure",
        configuration("http://127.0.0.1:9"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let payload = retry_body(&service, &task_id, "aftercommit-public-retry").await;
    let before = fs::read(&service.0.state_path).unwrap();
    proletarius_forwarding::test_support::fail_next_replace_after_rename_for_path(
        service.0.state_path.clone(),
    );
    let (status, receipt) = wire(&service, QUEUE_ACTION, payload.clone()).await;
    // The actual handler returns503/uncertain; the public wrapper deliberately
    // returns its already-durable409 unknown claim instead of completing it.
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(receipt["mutation_receipt"]["outcome"], "unknown");
    assert_eq!(receipt["code"], "public_mutation_outcome_unknown");
    let committed = fs::read(&service.0.state_path).unwrap();
    assert_ne!(
        before, committed,
        "fault is after real encrypted replacement"
    );
    {
        let state = service.0.state.lock().await;
        assert!(matches!(
            state
                .active
                .as_ref()
                .unwrap()
                .worker
                .queue()
                .lock()
                .await
                .inspect(),
            Err(StateStoreError::ReopenRequired)
        ));
    }
    let (status, replay) = wire(&service, QUEUE_ACTION, payload.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(replay, receipt);
    assert_eq!(fs::read(&service.0.state_path).unwrap(), committed);
    // A different public ID cannot bypass poisoned inspect, which is still
    // under the same queue guard and occurs before any CAS/mutation dispatch.
    let mut second = payload.clone();
    second["client_request_id"] = json!("aftercommit-poisoned-retry");
    let (_, blocked) = wire(&service, QUEUE_ACTION, second).await;
    assert_eq!(blocked["mutation_receipt"]["outcome"], "unknown");
    assert_eq!(fs::read(&service.0.state_path).unwrap(), committed);
    service.shutdown().await.unwrap();
    drop(service);
    let key = StateKey::from_slice(&[17; 32]).unwrap();
    let queue = QueueStore::open(
        root.join("sms-forwarding/state.enc"),
        key,
        StateLimits::default(),
    )
    .unwrap();
    let summary = queue.inspect().unwrap();
    let task = summary
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .unwrap();
    assert!(
        !task.telegram.result_unknown,
        "the prior explicit retry was already written"
    );
    assert_eq!(task.telegram.attempts, 1);
    drop(queue);
    // Reopening the daemon without any active configuration still replays the
    // original receipt and cannot execute a second retry or provider request.
    let reopened = Service::new(&root);
    let (_, replay) = wire(&reopened, QUEUE_ACTION, payload).await;
    assert_eq!(replay, receipt);
    assert_eq!(fs::read(&reopened.0.state_path).unwrap(), committed);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn real_aftercommit_dispatch_returns503_explicit_uncertainty_before_receipt_wrapper() {
    let root = tests::test_root();
    let task_id = seed(&root, true);
    let service = Service::new(&root);
    assert_eq!(
        wire(
            &service,
            "/api/sms/forwarding/configure",
            configuration("http://127.0.0.1:9")
        )
        .await
        .0,
        StatusCode::OK
    );
    // This test isolates the direct queue-action response. Join the actual
    // autonomous runner first so it cannot start a separate inspect against
    // the deliberately poisoned QueueStore during shutdown.
    quiesce_real_runner(&service).await;
    let direct_owner = {
        let state = service.0.state.lock().await;
        state.active.as_ref().unwrap().queue_io.clone()
    };
    let mut payload = retry_body(&service, &task_id, "direct-aftercommit-classification").await;
    payload.as_object_mut().unwrap().remove("client_request_id");
    let body = serde_json::from_value(payload).unwrap();
    proletarius_forwarding::test_support::fail_next_replace_after_rename_for_path(
        service.0.state_path.clone(),
    );
    let result = service
        .execute_queue_action(body, ForwardingChannel::Telegram)
        .await;
    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
    let value: Value =
        serde_json::from_slice(&result.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(value["uncertain"], true);
    assert_eq!(value["retryable"], false);
    assert_eq!(value["code"], "sms_forwarding_queue_persistence_uncertain");
    assert_eq!(
        direct_owner.test_snapshot(),
        (0, false, 0, false),
        "the direct command error was not delivered to its live observer"
    );
    record_quiesced_runner(&service).await;
    service.shutdown().await.unwrap();
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn poisoned_provider_disk_cancel_after_output_is_sticky_but_fully_drained() {
    let root = tests::test_root();
    let task_id = seed(&root, true);
    let service = Service::new(&root);
    assert_eq!(
        wire(
            &service,
            "/api/sms/forwarding/configure",
            configuration("http://127.0.0.1:9")
        )
        .await
        .0,
        StatusCode::OK
    );
    quiesce_real_runner(&service).await;
    let (worker, telegram, bark, direct_owner) = {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        (
            active.worker.clone(),
            active.telegram_binding.clone(),
            active.bark_binding.clone(),
            active.queue_io.clone(),
        )
    };

    // First prove the public operation's own observer receives the real
    // post-rename uncertainty. This poisons the shared QueueStore without
    // making the provider disk owner sticky.
    let mut payload = retry_body(&service, &task_id, "provider-orphan-aftercommit").await;
    payload.as_object_mut().unwrap().remove("client_request_id");
    proletarius_forwarding::test_support::fail_next_replace_after_rename_for_path(
        service.0.state_path.clone(),
    );
    let result = service
        .execute_queue_action(
            serde_json::from_value(payload).unwrap(),
            ForwardingChannel::Telegram,
        )
        .await;
    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
    let value: Value =
        serde_json::from_slice(&result.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(value["code"], "sms_forwarding_queue_persistence_uncertain");
    assert_eq!(value["uncertain"], true);
    assert_eq!(direct_owner.test_snapshot(), (0, false, 0, false));

    // Now start one real provider-disk inspect on the poisoned queue. Hold the
    // blocking output after its Err exists, cancel only the Tokio observer,
    // then release the actual job. JOB + OUTPUT + OBSERVER must all settle
    // before pending reaches zero, and the unobserved error must stay sticky.
    let call = worker.test_disk_call_count();
    let (entered_signal, entered) = oneshot::channel();
    let (release, release_wait) = std::sync::mpsc::channel();
    worker.test_disk_hook(call, true, move || {
        entered_signal.send(()).unwrap();
        release_wait.recv_timeout(Duration::from_secs(10)).unwrap();
    });
    let inspection = {
        let worker = worker.clone();
        tokio::spawn(async move {
            worker
                .inspect_for_bindings(Some(telegram), Some(bark))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    inspection.abort();
    assert!(inspection.await.unwrap_err().is_cancelled());
    assert_eq!(worker.test_disk_snapshot().0, 1);
    release.send(()).unwrap();
    assert_eq!(wait_provider_disk_settled(&worker).await, (0, true));

    record_quiesced_runner(&service).await;
    assert_eq!(
        service.shutdown().await,
        Err("sms_forwarding_release_unconfirmed")
    );
    {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        assert_eq!(active.runner_result, Some(Ok(())));
        assert_eq!(
            active.stop_result,
            Some(Err("sms_forwarding_release_unconfirmed"))
        );
        assert_eq!(worker.test_disk_snapshot(), (0, true));
    }
    drop(service);
    drop(worker);

    // A second provider attempt would have incremented attempts. Reopening the
    // committed bytes proves cancellation caused neither replay nor a second
    // provider submission; the explicit retry alone remains durable.
    let key = StateKey::from_slice(&[17; 32]).unwrap();
    let queue = QueueStore::open(
        root.join("sms-forwarding/state.enc"),
        key,
        StateLimits::default(),
    )
    .unwrap();
    let task = queue
        .inspect()
        .unwrap()
        .tasks
        .into_iter()
        .find(|task| task.id == task_id)
        .unwrap();
    assert_eq!(task.telegram.attempts, 1);
    assert!(!task.telegram.result_unknown);
    drop(queue);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn known_queue_cas_rejection_is_completed_without_uncertainty_or_mutation() {
    let root = tests::test_root();
    let task_id = seed(&root, true);
    let service = Service::new(&root);
    assert_eq!(
        wire(
            &service,
            "/api/sms/forwarding/configure",
            configuration("http://127.0.0.1:9")
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut payload = retry_body(&service, &task_id, "known-cas-rejection").await;
    payload["expected_attempts"] = json!(99);
    let before = fs::read(&service.0.state_path).unwrap();
    let (status, rejection) = wire(&service, QUEUE_ACTION, payload.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(rejection["code"], "forwarding_destination_changed");
    assert_eq!(rejection["mutation_receipt"]["outcome"], "completed");
    assert!(rejection.get("uncertain").is_none());
    assert_eq!(wire(&service, QUEUE_ACTION, payload).await.1, rejection);
    assert_eq!(fs::read(&service.0.state_path).unwrap(), before);
    service.shutdown().await.unwrap();
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

async fn pending<F: Future>(mut future: std::pin::Pin<&mut F>) {
    assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}

#[tokio::test(flavor = "current_thread")]
async fn real_worker_busy_drop_cancelled_stop_waits_for_release_before_same_path_reopen() {
    let root = tests::test_root();
    seed(&root, false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let (entered, connected) = oneshot::channel();
    let (closed, disconnected) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0; 1];
            assert_eq!(socket.read(&mut byte).await.unwrap(), 1);
            header.push(byte[0]);
            assert!(header.len() < 8192);
        }
        assert!(header.starts_with(b"CONNECT api.telegram.org:443 HTTP/1.1\r\n"));
        entered.send(()).unwrap();
        // No CONNECT response, DNS, tunnel, upstream connection or POST. EOF
        // proves the real provider future was dropped on runner shutdown.
        let mut byte = [0; 1];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        closed.send(()).unwrap();
    });
    let service = Service::new(&root);
    assert_eq!(
        wire(
            &service,
            "/api/sms/forwarding/configure",
            configuration(&proxy)
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(5), connected)
        .await
        .unwrap()
        .unwrap();
    let queue = {
        let state = service.0.state.lock().await;
        state.active.as_ref().unwrap().worker.queue().clone()
    };
    let guard = queue.clone().lock_owned().await;
    assert!(guard.inspect().unwrap().tasks[0].telegram.in_flight);
    let mut first = Box::pin(service.shutdown());
    pending(first.as_mut()).await;
    tokio::time::timeout(Duration::from_secs(5), disconnected)
        .await
        .unwrap()
        .unwrap();
    pending(first.as_mut()).await; // join is now observed; drain is still blocked
    drop(first); // cancel only the observer, retaining actual release ownership
    {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        assert_eq!(active.runner_result, Some(Ok(())));
        assert!(active.runner.is_none());
        assert!(active.stop_result.is_none());
        assert!(active.retiring);
    }
    assert!(matches!(
        QueueStore::open(
            root.join("sms-forwarding/state.enc"),
            StateKey::from_slice(&[17; 32]).unwrap(),
            StateLimits::default()
        ),
        Err(StateStoreError::Busy)
    ));
    let mut second = Box::pin(service.shutdown());
    pending(second.as_mut()).await;
    drop(guard);
    second.await.unwrap();
    assert!(service.0.state.lock().await.active.is_none());
    // This test's observation Arc also owns the Store; retire it before asking
    // whether the old production generation really released its lifetime lock.
    drop(queue);
    server.await.unwrap();
    let reopened = QueueStore::open(
        root.join("sms-forwarding/state.enc"),
        StateKey::from_slice(&[17; 32]).unwrap(),
        StateLimits::default(),
    )
    .unwrap();
    let summary = reopened.inspect().unwrap();
    let task = &summary.tasks[0];
    assert!(task.telegram.result_unknown);
    assert!(!task.telegram.in_flight);
    drop(reopened);
    drop(service);
    fs::remove_dir_all(root).unwrap();
}
