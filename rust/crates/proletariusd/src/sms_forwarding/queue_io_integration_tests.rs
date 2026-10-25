//! Production Service entry points with the actual queue-I/O command envelope.
//! Barriers pause real running commands, never substitute a mirrored handler.
use super::*;
use proletarius_forwarding::StateStoreError;
use serde_json::{Value, json};
use std::future::{Future, poll_fn};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use tokio::io::AsyncReadExt;
use tokio::sync::oneshot;

const KEY: u8 = 42;
const NOTICE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
struct Proxy {
    url: String,
    connected: Arc<AtomicUsize>,
    disconnected: Arc<AtomicUsize>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Proxy {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let connected = Arc::new(AtomicUsize::new(0));
        let disconnected = Arc::new(AtomicUsize::new(0));
        let (seen, ended) = (connected.clone(), disconnected.clone());
        let (stop, mut stopping) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = tokio::select! { biased; _=&mut stopping=>break, accepted=listener.accept()=>accepted.unwrap() };
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    assert_eq!(socket.read(&mut byte).await.unwrap(), 1);
                    header.push(byte[0]);
                    assert!(header.len() < 8192);
                }
                assert!(header.starts_with(b"CONNECT api.telegram.org:443 HTTP/1.1\r\n"));
                seen.fetch_add(1, Ordering::SeqCst);
                // No CONNECT reply, DNS, TLS tunnel, upstream socket or POST.
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
                ended.fetch_add(1, Ordering::SeqCst);
            }
        });
        Self {
            url,
            connected,
            disconnected,
            stop: Some(stop),
            task: Some(task),
        }
    }
    async fn observed(&self, ended: bool, count: usize) {
        let value = if ended {
            &self.disconnected
        } else {
            &self.connected
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while value.load(Ordering::SeqCst) < count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    async fn join(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(Duration::from_secs(5), self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
fn request(path: &str, body: Value) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
        .unwrap()
}
async fn body(response: Response<Full<Bytes>>) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn message(id: &str) -> ForwardingMessage {
    ForwardingMessage::new(
        id,
        "in",
        "synthetic-sender",
        "synthetic body",
        "2026-01-01T00:00:00Z",
        "synthetic-reader",
    )
    .unwrap()
}
async fn fixture(device: bool) -> (PathBuf, Service, Proxy, String, Arc<queue_io::Owner>) {
    let root = tests::test_root();
    let proxy = Proxy::new().await;
    let service = if device {
        let handoffs = crate::forwarding_handoff_store::Store::open(&root).unwrap();
        let ledger = crate::device_forwarding_store::Store::open(&root).unwrap();
        let handoff = json!({"schema_version":1,"handoff_id":"123e4567-e89b-12d3-a456-426614174000","device_sms":{"accepted_notification_ids":[],"was_enabled":true},"nwait_sms":{"cursor":null,"seeded":false,"was_enabled":null}});
        let response = {
            let _fixture = crate::forwarding_handoff_store::REQUEST_TEST_LOCK
                .lock()
                .await;
            crate::forwarding_handoff_store::handle(
                request("/api/v2/sms/forwarding/handoff", handoff),
                handoffs.clone(),
            )
            .await
        };
        let (status, value) = body(response).await;
        assert_eq!(status, StatusCode::OK, "handoff fixture response: {value}");
        ledger
            .import_handoff(&handoffs.current().unwrap().unwrap())
            .unwrap();
        Service::new_with_admission(&root, handoffs, ledger)
    } else {
        Service::new(&root)
    };
    let config = json!({"client_request_id":"queue-io-configure-fixture","client_sequence":1,
        "telegram":{"enabled":true,"bot_token":"synthetic-token","chat_id":"synthetic-chat","proxy":{"mode":"manual","url":proxy.url}},
        "bark":{"enabled":false},"state_key":STANDARD.encode([KEY;32])});
    let (status, value) = body(
        service
            .handle(request("/api/sms/forwarding/configure", config))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = value["configuration_id"].as_str().unwrap().to_owned();
    let owner = service
        .0
        .state
        .lock()
        .await
        .active
        .as_ref()
        .unwrap()
        .queue_io
        .clone();
    (root, service, proxy, id, owner)
}
fn snapshot(id: &str) -> Value {
    json!({"expected_configuration_id":id,"current_notification_ids":[NOTICE],"include_current_snapshot":true,
    "messages":[{"notification_id":NOTICE,"id":"queue-io-device-message","direction":"in","sender":"synthetic-sender","content":"synthetic body","timestamp":"2026-01-01T00:00:00Z","device":"synthetic-reader"}]})
}
async fn pending<F: Future>(mut future: std::pin::Pin<&mut F>) {
    assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}
async fn entered(gate: &queue_io::TestGate) {
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
}
async fn settled(owner: &queue_io::Owner) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while owner.test_snapshot().0 != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn disk_summary(service: &Service) -> proletarius_forwarding::ForwardingQueueSummary {
    let key = StateKey::from_slice(&[KEY; 32]).unwrap();
    proletarius_forwarding::open(
        &key,
        &fs::read(&service.0.state_path).unwrap(),
        StateLimits::default(),
    )
    .unwrap()
    .inspect_go_queue()
    .unwrap()
}
fn reopened(root: &Path) -> QueueStore {
    QueueStore::open(
        root.join("sms-forwarding/state.enc"),
        StateKey::from_slice(&[KEY; 32]).unwrap(),
        StateLimits::default(),
    )
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn enqueue_admitted_running_cancelled_stop_waiter_retries_and_reopens_after_real_io() {
    let (root, service, mut proxy, _id, owner) = fixture(false).await;
    let (gate, release) = owner.test_gate(queue_io::Stage::Started, false);
    let producer = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .enqueue_admitted(&[message("queue-io-admitted")])
                .await
        })
    };
    entered(&gate).await;
    producer.abort();
    assert!(producer.await.unwrap_err().is_cancelled());
    assert_eq!(owner.test_snapshot().0, 1);
    let mut first = Box::pin(service.shutdown());
    pending(first.as_mut()).await;
    drop(first);
    assert!(service.0.state.lock().await.active.is_some());
    assert!(owner.test_snapshot().1);
    assert!(matches!(
        QueueStore::open(
            service.0.state_path.clone(),
            StateKey::from_slice(&[KEY; 32]).unwrap(),
            StateLimits::default()
        ),
        Err(StateStoreError::Busy)
    ));
    let mut second = Box::pin(service.shutdown());
    pending(second.as_mut()).await;
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.test_snapshot().0, 0);
    assert!(!owner.test_snapshot().3);
    assert!(service.0.state.lock().await.active.is_none());
    assert_eq!(proxy.connected.load(Ordering::SeqCst), 0);
    proxy.join().await;
    let queue = reopened(&root);
    let summary = queue.inspect().unwrap();
    assert_eq!(summary.tasks.len(), 1);
    assert_eq!(summary.tasks[0].telegram.attempts, 0);
    drop(queue);
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_rust_enqueue_still_wakes_provider_after_durable_commit() {
    let (root, service, mut proxy, _id, owner) = fixture(false).await;
    let (gate, release) = owner.test_gate(queue_io::Stage::Started, false);
    let producer = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .enqueue_admitted(&[message("cancelled-rust-enqueue-wake")])
                .await
        })
    };
    entered(&gate).await;
    producer.abort();
    assert!(producer.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    settled(&owner).await;
    // The normal runner poll is five seconds. A cancelled observer must not
    // delay a committed message until that poll.
    tokio::time::timeout(Duration::from_secs(1), proxy.observed(false, 1))
        .await
        .expect("retained queue commit did not wake the provider");
    service.shutdown().await.unwrap();
    proxy.observed(true, 1).await;
    proxy.join().await;
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn device_snapshot_cancel_after_real_queue_commit_drains_second_admission_transaction() {
    let (root, service, mut proxy, id, owner) = fixture(true).await;
    let (gate, release) = owner.test_gate(queue_io::Stage::QueueCommitted, false);
    let producer = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .handle(request(
                    "/api/sms/forwarding/device-snapshot",
                    snapshot(&id),
                ))
                .await
        })
    };
    entered(&gate).await;
    assert_eq!(disk_summary(&service).tasks.len(), 1);
    let admission = service.0.device_admission.as_ref().unwrap().clone();
    assert!(
        !admission
            .current()
            .unwrap()
            .unwrap()
            .accepted_notification_ids
            .contains(&NOTICE.to_owned())
    );
    producer.abort();
    assert!(producer.await.unwrap_err().is_cancelled());
    let mut stop = Box::pin(service.shutdown());
    pending(stop.as_mut()).await;
    assert_eq!(owner.test_snapshot().0, 1);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), stop)
        .await
        .unwrap()
        .unwrap();
    assert!(
        admission
            .current()
            .unwrap()
            .unwrap()
            .accepted_notification_ids
            .contains(&NOTICE.to_owned())
    );
    assert_eq!(owner.test_snapshot().0, 0);
    proxy.join().await;
    let queue = reopened(&root);
    assert_eq!(queue.inspect().unwrap().tasks.len(), 1);
    drop(queue);
    // Durable admission is not an HTTP ACK: the original response observer was cancelled.
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn known_error_with_cancelled_enqueue_observer_is_diagnostic_not_permanent_retirement_fence()
{
    let (root, service, mut proxy, _id, owner) = fixture(false).await;
    // Isolated pool, same real Service/Preparation/OwnedCommand/disk path. A
    // synthetic orphan error must not contaminate the process-global archive ledger.
    owner.test_private_executor(2);
    let (gate, release) = owner.test_gate(queue_io::Stage::Started, true);
    let producer = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .enqueue_admitted(&[message("known-error-before-write")])
                .await
        })
    };
    entered(&gate).await;
    producer.abort();
    assert!(producer.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    settled(&owner).await;
    let (pending, _closed, orphans, unconfirmed) = owner.test_snapshot();
    assert_eq!(pending, 0);
    assert_eq!(orphans, 1);
    assert!(!unconfirmed);
    assert!(
        !service.0.state_path.exists(),
        "Started failure is before first queue write"
    );
    service.shutdown().await.unwrap();
    assert!(service.0.state.lock().await.active.is_none());
    let queue = reopened(&root);
    assert!(queue.inspect().unwrap().tasks.is_empty());
    drop(queue);
    assert_eq!(proxy.connected.load(Ordering::SeqCst), 0);
    proxy.join().await;
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn device_two_file_known_error_keeps_stable_ids_and_same_service_retry_deduplicates() {
    let (root, service, mut proxy, id, owner) = fixture(true).await;
    owner.test_private_executor(2);
    let (gate, release) = owner.test_gate(queue_io::Stage::QueueCommitted, true);
    let payload = snapshot(&id);
    let producer = {
        let service = service.clone();
        let payload = payload.clone();
        tokio::spawn(async move {
            service
                .handle(request("/api/sms/forwarding/device-snapshot", payload))
                .await
        })
    };
    entered(&gate).await;
    assert_eq!(disk_summary(&service).tasks.len(), 1);
    let admission = service.0.device_admission.as_ref().unwrap().clone();
    assert!(
        !admission
            .current()
            .unwrap()
            .unwrap()
            .accepted_notification_ids
            .contains(&NOTICE.to_owned())
    );
    producer.abort();
    assert!(producer.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    settled(&owner).await;
    assert_eq!(owner.test_snapshot().2, 1);
    assert!(!owner.test_snapshot().3);
    assert!(
        !admission
            .current()
            .unwrap()
            .unwrap()
            .accepted_notification_ids
            .contains(&NOTICE.to_owned())
    );
    let (status, value) = body(
        service
            .handle(request("/api/sms/forwarding/device-snapshot", payload))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(value["queued"], 0);
    assert!(
        admission
            .current()
            .unwrap()
            .unwrap()
            .accepted_notification_ids
            .contains(&NOTICE.to_owned())
    );
    assert_eq!(disk_summary(&service).seen_entries, 1);
    assert_eq!(disk_summary(&service).tasks.len(), 1);
    service.shutdown().await.unwrap();
    proxy.join().await;
    let queue = reopened(&root);
    assert_eq!(queue.inspect().unwrap().seen_entries, 1);
    drop(queue);
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_device_snapshot_wakes_committed_queue_even_if_progress_write_fails() {
    let (root, service, mut proxy, id, owner) = fixture(true).await;
    owner.test_private_executor(2);
    let (gate, release) = owner.test_gate(queue_io::Stage::QueueCommitted, true);
    let producer = {
        let service = service.clone();
        let payload = snapshot(&id);
        tokio::spawn(async move {
            service
                .handle(request("/api/sms/forwarding/device-snapshot", payload))
                .await
        })
    };
    entered(&gate).await;
    assert_eq!(disk_summary(&service).tasks.len(), 1);
    producer.abort();
    assert!(producer.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    settled(&owner).await;
    tokio::time::timeout(Duration::from_secs(1), proxy.observed(false, 1))
        .await
        .expect("durable queue admission was not woken after progress failure");
    service.shutdown().await.unwrap();
    proxy.observed(true, 1).await;
    proxy.join().await;
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn unconfirmed_io_finish_never_skips_actual_provider_release_drain() {
    let (root, service, mut proxy, _id, owner) = fixture(false).await;
    service
        .enqueue_admitted(&[message("active-provider-before-io")])
        .await
        .unwrap();
    proxy.observed(false, 1).await;
    let queue = {
        let state = service.0.state.lock().await;
        state.active.as_ref().unwrap().worker.queue().clone()
    };
    let (gate, release) = owner.test_gate(queue_io::Stage::Started, false);
    let producer = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .enqueue_admitted(&[message("second-queued-command")])
                .await
        })
    };
    entered(&gate).await;
    producer.abort();
    assert!(producer.await.unwrap_err().is_cancelled());
    // Queue this observation lock BEFORE the runner's real Drop release. Tokio
    // mutex ordering then lets us keep release blocked after I/O is fully done.
    let mut observing = Box::pin(queue.clone().lock_owned());
    pending(observing.as_mut()).await;
    owner.test_unconfirmed();
    let mut stop = Box::pin(service.shutdown());
    pending(stop.as_mut()).await;
    proxy.observed(true, 1).await;
    pending(stop.as_mut()).await;
    release.send(()).unwrap();
    let guard = tokio::time::timeout(Duration::from_secs(5), observing)
        .await
        .unwrap();
    settled(&owner).await;
    assert!(
        guard
            .inspect()
            .unwrap()
            .tasks
            .iter()
            .any(|t| t.telegram.in_flight)
    );
    // If I/O Err returned through `?` before provider drain this would be Ready.
    pending(stop.as_mut()).await;
    drop(stop);
    assert!(
        service
            .0
            .state
            .lock()
            .await
            .active
            .as_ref()
            .unwrap()
            .stop_result
            .is_none()
    );
    drop(guard);
    let result = tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap();
    assert!(result.is_err());
    assert!(service.0.state.lock().await.active.is_some());
    assert!(
        !queue
            .lock()
            .await
            .inspect()
            .unwrap()
            .tasks
            .iter()
            .any(|t| t.telegram.in_flight)
    );
    proxy.join().await;
    // Failed retirement stays fenced; opening below is only a resource check
    // after explicitly disposing this synthetic failed service and observation Arc.
    drop(service);
    drop(queue);
    let reopened = reopened(&root);
    assert!(
        reopened
            .inspect()
            .unwrap()
            .tasks
            .iter()
            .any(|t| t.telegram.result_unknown)
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

/// Regression executes the actual public enqueue handler and actual QueueStore
/// atomic replacement. The release thread never needs the Tokio scheduler.
#[tokio::test(flavor = "current_thread")]
async fn public_enqueue_held_file_lock_keeps_heartbeat_and_cancelled_owner_until_drain() {
    let (root, service, mut proxy, id, owner) = fixture(false).await;
    fn take_wake(wake: &Notify) -> bool {
        let mut notified = std::pin::pin!(wake.notified());
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        notified.as_mut().poll(&mut context).is_ready()
    }
    {
        let state = service.0.state.lock().await;
        assert!(
            !state
                .active
                .as_ref()
                .unwrap()
                .worker
                .take_new_work_notification_for_test()
        );
        take_wake(&service.0.wake);
    }
    let shutdown = service
        .0
        .state
        .lock()
        .await
        .active
        .as_ref()
        .unwrap()
        .shutdown
        .clone();
    let lock_path = root.join("slow-disk.lock");
    let held = fs::OpenOptions::new()
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
    let producer = {
        let service = service.clone();
        tokio::spawn(async move {
            service.handle(request("/api/sms/forwarding/enqueue", json!({
            "expected_configuration_id":id,
            "messages":[{"id":"public-held-lock", "direction":"in", "sender":"synthetic", "content":"offline", "timestamp":"2026-01-01T00:00:00Z", "device":"synthetic"}]
        }))).await
        })
    };
    let abort = producer.abort_handle();
    let observed_owner = owner.clone();
    let (release_ready, can_release) = std::sync::mpsc::channel();
    let release = std::thread::spawn(move || {
        entering.recv_timeout(Duration::from_secs(10)).unwrap();
        let before = beats.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        let during = beats.load(Ordering::SeqCst).saturating_sub(before);
        abort.abort();
        let _ = shutdown.send(true); // Prevent any provider request after release.
        // Runtime acknowledges the cancelled waiter and cancelled drain observer.
        // Timeout independently releases the lock on the synchronous baseline.
        let handshake = can_release.recv_timeout(Duration::from_secs(3)).is_ok();
        let pending = observed_owner.test_snapshot().0;
        println!("release_handshake={handshake}");
        drop(held);
        (during, pending)
    });
    let result = producer.await;
    let cancelled = result.as_ref().is_err_and(|e| e.is_cancelled());
    let mut drain_waited = false;
    if cancelled {
        assert_eq!(owner.test_snapshot().0, 1);
        let mut first_stop = Box::pin(service.shutdown());
        pending(first_stop.as_mut()).await;
        drop(first_stop); // A cancelled shutdown observer must be retryable.
        assert!(service.0.state.lock().await.active.is_some());
        assert!(matches!(
            QueueStore::open(
                service.0.state_path.clone(),
                StateKey::from_slice(&[KEY; 32]).unwrap(),
                StateLimits::default()
            ),
            Err(StateStoreError::Busy)
        ));
        drain_waited = true;
        let state = service.0.state.lock().await;
        assert!(
            !state
                .active
                .as_ref()
                .unwrap()
                .worker
                .take_new_work_notification_for_test()
        );
        assert!(!take_wake(&service.0.wake));
    }
    let _ = release_ready.send(());
    let (during, retained) = tokio::task::spawn_blocking(move || release.join().unwrap())
        .await
        .unwrap();
    heartbeat.abort();
    let _ = heartbeat.await;
    settled(&owner).await;
    {
        let state = service.0.state.lock().await;
        assert!(
            state
                .active
                .as_ref()
                .unwrap()
                .worker
                .take_new_work_notification_for_test(),
            "durable owned commit must wake worker after waiter cancellation"
        );
        assert!(
            take_wake(&service.0.wake),
            "durable owned commit must wake service after waiter cancellation"
        );
    }
    service.shutdown().await.unwrap();
    proxy.join().await;
    let summary = disk_summary(&service);
    println!(
        "held_file_lock: heartbeat_during_150ms={during}, retained_after_cancel={retained}, waiter_cancelled={}, seen={}, pending={}, provider_connects={}",
        result.as_ref().is_err_and(|e| e.is_cancelled()),
        summary.seen_entries,
        summary.tasks.len(),
        proxy.connected.load(Ordering::SeqCst)
    );
    assert_eq!(summary.seen_entries, 1);
    assert_eq!(summary.tasks.len(), 1);
    assert_eq!(summary.tasks[0].telegram.attempts, 0);
    assert_eq!(proxy.connected.load(Ordering::SeqCst), 0);
    let queue = reopened(&root);
    assert_eq!(queue.inspect().unwrap().seen_entries, 1);
    drop(queue);
    drop(service);
    fs::remove_dir_all(root).unwrap();
    assert!(
        during >= 3,
        "public enqueue blocked current-thread heartbeat while real file lock was held"
    );
    assert!(
        drain_waited,
        "shutdown must wait for actual queue transaction completion"
    );
    assert_eq!(
        retained, 1,
        "cancelled observer released actual running queue command"
    );
    assert!(result.unwrap_err().is_cancelled());
}

/// Freeze is a provider command, not a producer command. The real rename path
/// blocks behind an independent file lock while the one-thread runtime signals
/// stop, cancels a retirement observer, and retains the old generation.
#[tokio::test(flavor = "current_thread")]
async fn provider_freeze_real_disk_block_keeps_stop_and_replacement_bound_to_old_owner() {
    let (root, service, mut proxy, id, _) = fixture(false).await;
    let worker = {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        let options =
            EnqueueOptions::new(true, &active.telegram_binding, false, &active.bark_binding);
        active
            .worker
            .queue()
            .lock()
            .await
            .enqueue_and_persist(
                &[message("provider-blocked-freeze")],
                "2026-01-01T00:00:01Z",
                &options,
            )
            .unwrap();
        active.worker.clone()
    };
    let lock_path = root.join("provider-slow-disk.lock");
    let held = fs::OpenOptions::new()
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
    service.0.wake.notify_one();
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || entering.recv_timeout(Duration::from_secs(5)).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(worker.queue().try_lock().is_err());
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::time::sleep(Duration::from_millis(10)),
    )
    .await
    .unwrap();
    let mut first_stop = Box::pin(service.shutdown());
    pending(first_stop.as_mut()).await;
    drop(first_stop);
    {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        assert_eq!(active.id, id);
        assert!(active.retiring);
    }
    assert!(matches!(
        QueueStore::open(
            service.0.state_path.clone(),
            StateKey::from_slice(&[KEY; 32]).unwrap(),
            StateLimits::default()
        ),
        Err(StateStoreError::Busy)
    ));
    assert_eq!(proxy.connected.load(Ordering::SeqCst), 0);
    let replacement = json!({"client_request_id":"provider-disk-successor", "client_sequence":2,
        "telegram":{"enabled":false}, "bark":{"enabled":false}, "state_key":STANDARD.encode([KEY;32])});
    let mut replacing =
        Box::pin(service.handle(request("/api/sms/forwarding/configure", replacement)));
    pending(replacing.as_mut()).await;
    // The replacement's observer may leave; its partially retired predecessor
    // remains in ServiceState and still holds the actual QueueStore/flock.
    drop(replacing);
    assert_eq!(service.0.state.lock().await.active.as_ref().unwrap().id, id);
    drop(held);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(service.0.state.lock().await.active.is_none());
    assert_eq!(proxy.connected.load(Ordering::SeqCst), 0);
    drop(worker);
    let queue = reopened(&root);
    let task = &queue.inspect().unwrap().tasks[0];
    assert_eq!(task.telegram.attempts, 0);
    assert!(!task.telegram.in_flight);
    drop(queue);
    proxy.join().await;
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn status_real_identity_read_block_does_not_starve_timer_or_cancelled_shutdown() {
    let (root, service, mut proxy, _, _) = fixture(false).await;
    // Park only periodic execution so the next path-scoped verification belongs
    // deterministically to the real public status handler, not its runner.
    {
        let mut state = service.0.state.lock().await;
        let active = state.active.as_mut().unwrap();
        let _ = active.shutdown.send(true);
        active.runner.as_mut().unwrap().await.unwrap();
        let (shutdown, mut receiver) = watch::channel(false);
        active.shutdown = shutdown;
        active.runner = Some(tokio::spawn(async move {
            let _ = receiver.changed().await;
        }));
    }
    let lock_path = root.join("status-slow-identity.lock");
    let held = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    held.lock().unwrap();
    let (entered, entering) = std::sync::mpsc::channel();
    proletarius_forwarding::test_support::hold_next_verification(
        service.0.state_path.clone(),
        lock_path,
        entered,
    );
    let status = {
        let service = service.clone();
        tokio::spawn(async move { service.status().await })
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || entering.recv_timeout(Duration::from_secs(5)).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::time::sleep(Duration::from_millis(10)),
    )
    .await
    .unwrap();
    status.abort();
    assert!(status.await.unwrap_err().is_cancelled());
    let mut stop = Box::pin(service.shutdown());
    pending(stop.as_mut()).await;
    drop(stop);
    assert!(service.0.state.lock().await.active.is_some());
    assert!(matches!(
        QueueStore::open(
            service.0.state_path.clone(),
            StateKey::from_slice(&[KEY; 32]).unwrap(),
            StateLimits::default()
        ),
        Err(StateStoreError::Busy)
    ));
    drop(held);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(service.0.state.lock().await.active.is_none());
    assert_eq!(proxy.connected.load(Ordering::SeqCst), 0);
    drop(reopened(&root));
    proxy.join().await;
    drop(service);
    fs::remove_dir_all(root).unwrap();
}
