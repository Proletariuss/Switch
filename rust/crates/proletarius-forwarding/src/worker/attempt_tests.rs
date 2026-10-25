//! Deterministic software fixtures for real worker submission and release ownership.
use super::*;
use crate::{EnqueueOptions, ForwardingMessage, ProxyMode, StateKey, StateLimits};
use reqwest::Url;
use std::{
    fs,
    future::{Future, poll_fn},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    task::Poll,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const NOW: &str = "2026-01-02T03:04:08Z";
const OK: &str = "{\"ok\":true,\"result\":{}}";
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/tmp/forwarding-attempt-tests");
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("queue.swf1")
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn key() -> StateKey {
    StateKey::from_slice(&[0x63; 32]).unwrap()
}
struct Clock;
impl ForwardingClock for Clock {
    fn now_rfc3339(&self) -> Result<String, WorkerError> {
        Ok(NOW.into())
    }
    fn local_sms_timestamp(&self, _: &str) -> Result<Option<String>, WorkerError> {
        Ok(None)
    }
    fn retry_rfc3339(&self, _: Duration) -> Result<String, WorkerError> {
        Ok("2026-01-02T03:05:08Z".into())
    }
}

struct Reply {
    body: &'static str,
    gate: Option<oneshot::Receiver<()>>,
}
impl Reply {
    fn json(body: &'static str) -> Self {
        Self { body, gate: None }
    }
    fn held(body: &'static str) -> (Self, oneshot::Sender<()>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                body,
                gate: Some(rx),
            },
            tx,
        )
    }
}
struct Mock {
    endpoint: Url,
    bodies: Arc<StdMutex<Vec<serde_json::Value>>>,
    received: mpsc::UnboundedReceiver<usize>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Mock {
    async fn new(script: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let bodies = Arc::new(StdMutex::new(Vec::new()));
        let records = bodies.clone();
        let (sent, received) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            for reply in script {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let (header_end, content_length) = loop {
                    let mut block = [0_u8; 2048];
                    let n = socket.read(&mut block).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&block[..n]);
                    assert!(request.len() < 128 * 1024);
                    let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let bytes = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + bytes {
                        break (end + 4, bytes);
                    }
                };
                let body =
                    serde_json::from_slice(&request[header_end..header_end + content_length])
                        .unwrap();
                let count = {
                    let mut records = records.lock().unwrap();
                    records.push(body);
                    records.len()
                };
                sent.send(count).unwrap();
                if let Some(gate) = reply.gate {
                    let _ = gate.await;
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.body.len(),
                    reply.body
                );
                // A cancelled submit can close the socket before its gated reply.
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        Self {
            endpoint,
            bodies,
            received,
            task: Some(task),
        }
    }
    async fn next(&mut self, count: usize) {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), self.received.recv())
                .await
                .unwrap(),
            Some(count)
        );
    }
    fn count(&self) -> usize {
        self.bodies.lock().unwrap().len()
    }
    async fn join(&mut self) {
        tokio::time::timeout(Duration::from_secs(3), self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn queue_fixture(body: &str) -> (Root, QueueStore, DestinationBinding, String) {
    let root = Root::new();
    let state_key = key();
    let binding = DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
    let disabled =
        DestinationBinding::bark(&state_key, "https://synthetic.invalid", "disabled").unwrap();
    let mut queue = QueueStore::open(root.path(), state_key, StateLimits::default()).unwrap();
    let message = ForwardingMessage::new(
        "attempt-fixture",
        "incoming",
        "synthetic-sender",
        body,
        "2026-01-02T03:04:05Z",
        "synthetic-reader",
    )
    .unwrap();
    queue
        .enqueue_and_persist(
            &[message],
            "2026-01-02T03:04:06Z",
            &EnqueueOptions::new(true, &binding, false, &disabled),
        )
        .unwrap();
    let task = queue.inspect().unwrap().tasks[0].id.clone();
    (root, queue, binding, task)
}
fn worker_fixture(
    endpoint: Url,
    body: &str,
) -> (
    Root,
    Arc<ForwardingWorker<Clock>>,
    DestinationBinding,
    String,
) {
    let (root, queue, binding, task) = queue_fixture(body);
    let sender = TelegramSender::with_endpoint(
        "synthetic-token",
        "synthetic-chat",
        ProxyMode::Direct,
        endpoint,
    )
    .unwrap();
    let worker = Arc::new(
        ForwardingWorker::new(
            queue,
            Some(sender),
            Some(binding.clone()),
            None,
            None,
            Clock,
        )
        .unwrap(),
    );
    (root, worker, binding, task)
}
async fn no_automatic_submit(worker: &ForwardingWorker<Clock>, mock: &Mock, count: usize) {
    for _ in 0..3 {
        assert_eq!(worker.flush_pending().await.unwrap().attempted, 0);
    }
    assert_eq!(mock.count(), count);
}

async fn typed_unknown_retry(multi: bool) {
    let mut replies = Vec::new();
    if multi {
        replies.push(Reply::json(OK));
    }
    replies.extend([Reply::json("{"), Reply::json(OK)]);
    let mut mock = Mock::new(replies).await;
    let body = if multi {
        "x".repeat(5000)
    } else {
        "synthetic body".into()
    };
    let (root, worker, binding, task) = worker_fixture(mock.endpoint.clone(), &body);
    let first = worker.flush_pending().await.unwrap();
    assert_eq!(first.uncertain, 1);
    let expected = if multi { 2 } else { 1 };
    let before = worker.queue.lock().await.inspect().unwrap();
    let progress = before.tasks[0]
        .telegram
        .telegram_checkpoint
        .clone()
        .unwrap();
    assert!(before.tasks[0].telegram.result_unknown);
    assert!(!before.tasks[0].telegram.in_flight);
    assert_eq!(progress.confirmed_parts, if multi { 1 } else { 0 });
    assert!(progress.digests_valid);
    no_automatic_submit(&worker, &mock, expected).await;
    worker
        .queue
        .lock()
        .await
        .retry_destination(
            &task,
            ForwardingChannel::Telegram,
            true,
            &binding,
            "2026-01-02T03:04:09Z",
        )
        .unwrap();
    let retry = worker.flush_pending().await.unwrap();
    assert_eq!(retry.delivered, 1);
    assert_eq!(mock.count(), expected + 1);
    {
        let bodies = mock.bodies.lock().unwrap();
        assert_eq!(bodies[expected - 1], bodies[expected]);
    }
    assert!(
        worker
            .queue
            .lock()
            .await
            .inspect()
            .unwrap()
            .tasks
            .is_empty()
    );
    worker.finish_releases().await.unwrap();
    assert_eq!(worker.flush_pending().await, Err(WorkerError::Stopped));
    mock.join().await;
    drop(worker);
    assert!(
        QueueStore::open(root.path(), key(), StateLimits::default())
            .unwrap()
            .inspect()
            .unwrap()
            .tasks
            .is_empty()
    );
}
#[tokio::test]
async fn typed_unknown_same_store_retry_single_chunk_preserves_automatic_fence() {
    typed_unknown_retry(false).await;
}
#[tokio::test]
async fn typed_unknown_same_store_retry_multi_chunk_never_resends_confirmed_chunk() {
    typed_unknown_retry(true).await;
}

async fn cancelled_unknown_retry(multi: bool) {
    let (held, reply) = Reply::held("{");
    let mut script = Vec::new();
    if multi {
        script.push(Reply::json(OK));
    }
    script.extend([held, Reply::json(OK)]);
    let mut mock = Mock::new(script).await;
    let body = if multi {
        "x".repeat(5000)
    } else {
        "synthetic body".into()
    };
    let (root, worker, binding, task) = worker_fixture(mock.endpoint.clone(), &body);
    let first = {
        let worker = worker.clone();
        tokio::spawn(async move { worker.flush_pending().await })
    };
    let expected = if multi { 2 } else { 1 };
    for n in 1..=expected {
        mock.next(n).await;
    }
    let mut guard = worker.queue.lock().await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(worker.releases.state.lock().unwrap().pending, 1);
    assert!(guard.inspect().unwrap().tasks[0].telegram.in_flight);
    // Pure cancellation does NOT run the typed Unknown explicit-release node.
    assert_eq!(
        guard.retry_destination(
            &task,
            ForwardingChannel::Telegram,
            true,
            &binding,
            "2026-01-02T03:04:09Z"
        ),
        Err(QueueMutationError::InFlight)
    );
    drop(guard);
    worker.releases.finish().await.unwrap();
    let _ = reply.send(());
    no_automatic_submit(&worker, &mock, expected).await;
    let before = worker.queue.lock().await.inspect().unwrap();
    assert_eq!(
        before.tasks[0]
            .telegram
            .telegram_checkpoint
            .as_ref()
            .unwrap()
            .confirmed_parts,
        if multi { 1 } else { 0 }
    );
    worker
        .queue
        .lock()
        .await
        .retry_destination(
            &task,
            ForwardingChannel::Telegram,
            true,
            &binding,
            "2026-01-02T03:04:10Z",
        )
        .unwrap();
    assert_eq!(worker.flush_pending().await.unwrap().delivered, 1);
    {
        let bodies = mock.bodies.lock().unwrap();
        assert_eq!(bodies[expected - 1], bodies[expected]);
    }
    assert_eq!(mock.count(), expected + 1);
    mock.join().await;
    worker.finish_releases().await.unwrap();
    drop(worker);
    assert!(
        QueueStore::open(root.path(), key(), StateLimits::default())
            .unwrap()
            .inspect()
            .unwrap()
            .tasks
            .is_empty()
    );
}
#[tokio::test]
async fn cancel_unknown_same_store_retry_waits_for_release_single_chunk() {
    cancelled_unknown_retry(false).await;
}
#[tokio::test]
async fn cancel_unknown_same_store_retry_waits_for_release_multi_chunk() {
    cancelled_unknown_retry(true).await;
}

#[tokio::test]
async fn typed_unknown_real_worker_late_drop_cannot_clear_new_attempt() {
    let (held, ack) = Reply::held(OK);
    let mut mock = Mock::new(vec![Reply::json("{"), held]).await;
    let (_root, worker, binding, task) = worker_fixture(mock.endpoint.clone(), "synthetic body");
    let unknown = Arc::new(ReleaseBarrier::default());
    let cleanup = Arc::new(ReleaseBarrier::default());
    *worker.unknown_release_pause.lock().unwrap() = Some(unknown.clone());
    *worker.releases.pause.lock().unwrap() = Some(cleanup.clone());
    let first = {
        let w = worker.clone();
        tokio::spawn(async move { w.flush_pending().await })
    };
    tokio::time::timeout(Duration::from_secs(3), unknown.entered.notified())
        .await
        .unwrap();
    let mut guard = worker.queue.lock().await;
    assert!(guard.inspect().unwrap().tasks[0].telegram.result_unknown);
    assert!(!guard.inspect().unwrap().tasks[0].telegram.in_flight);
    // Cancel only AFTER the actual typed-Unknown branch released its owner.
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(worker.releases.state.lock().unwrap().pending, 1);
    guard
        .retry_destination(
            &task,
            ForwardingChannel::Telegram,
            true,
            &binding,
            "2026-01-02T03:04:09Z",
        )
        .unwrap();
    drop(guard);
    tokio::time::timeout(Duration::from_secs(3), cleanup.entered.notified())
        .await
        .unwrap();
    let second = {
        let w = worker.clone();
        tokio::spawn(async move { w.flush_pending().await })
    };
    mock.next(1).await;
    mock.next(2).await;
    assert!(
        worker.queue.lock().await.inspect().unwrap().tasks[0]
            .telegram
            .in_flight
    );
    cleanup.proceed.notify_one();
    worker.releases.finish().await.unwrap();
    assert!(
        worker.queue.lock().await.inspect().unwrap().tasks[0]
            .telegram
            .in_flight
    );
    ack.send(()).unwrap();
    assert_eq!(second.await.unwrap().unwrap().delivered, 1);
    worker.finish_releases().await.unwrap();
    mock.join().await;
}

#[tokio::test]
async fn cancelled_release_drain_waiter_keeps_store_until_real_cleanup_then_reopens() {
    let (held, reply) = Reply::held(OK);
    let mut mock = Mock::new(vec![held]).await;
    let (root, worker, _binding, _task) = worker_fixture(mock.endpoint.clone(), "synthetic body");
    let first = {
        let w = worker.clone();
        tokio::spawn(async move { w.flush_pending().await })
    };
    mock.next(1).await;
    let guard = worker.queue.lock().await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let mut finish = Box::pin(worker.finish_releases());
    poll_fn(|cx| {
        assert!(matches!(finish.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    assert!(matches!(
        QueueStore::open(root.path(), key(), StateLimits::default()),
        Err(StateStoreError::Busy)
    ));
    drop(finish); // Cancel only the drain observer; closure and pending owner survive.
    assert_eq!(worker.flush_pending().await, Err(WorkerError::Stopped));
    assert_eq!(worker.releases.state.lock().unwrap().pending, 1);
    drop(guard);
    worker.finish_releases().await.unwrap();
    drop(worker);
    let reopened = QueueStore::open(root.path(), key(), StateLimits::default()).unwrap();
    assert!(reopened.inspect().unwrap().tasks[0].telegram.result_unknown);
    drop(reopened);
    let _ = reply.send(());
    mock.join().await;
}

fn owned_job() -> (Root, Arc<ReleaseLedger>, ReleaseJob) {
    let (root, mut queue, binding, task) = queue_fixture("synthetic body");
    let token = queue
        .begin_destination_attempt(&task, ForwardingChannel::Telegram, true, &binding, NOW)
        .unwrap();
    let ledger = Arc::new(ReleaseLedger::default());
    let completion = ledger.register().unwrap();
    let job = ReleaseJob {
        queue: Some(Arc::new(Mutex::new(queue))),
        task_id: task,
        channel: ForwardingChannel::Telegram,
        token,
        completion,
    };
    (root, ledger, job)
}
#[tokio::test]
async fn release_completion_drops_its_last_store_arc_before_notifying() {
    let (root, ledger, job) = owned_job();
    let pause = Arc::new(ReleaseBarrier::default());
    *ledger.pause.lock().unwrap() = Some(pause.clone());
    let task = tokio::spawn(job.run());
    pause.entered.notified().await;
    assert!(matches!(
        QueueStore::open(root.path(), key(), StateLimits::default()),
        Err(StateStoreError::Busy)
    ));
    pause.proceed.notify_one();
    ledger.finish().await.unwrap();
    // Do not join the release task first: the completion itself must prove that
    // its final queue capture no longer holds the flock.
    let reopened = QueueStore::open(root.path(), key(), StateLimits::default()).unwrap();
    drop(reopened);
    task.await.unwrap();
}
#[tokio::test]
async fn aborted_release_job_is_sticky_unconfirmed_and_drops_its_store() {
    let (root, ledger, job) = owned_job();
    let task = tokio::spawn(async move {
        let _job = job;
        std::future::pending::<()>().await;
    });
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(ledger.finish().await, Err(WorkerError::ReleaseUnconfirmed));
    assert_eq!(ledger.finish().await, Err(WorkerError::ReleaseUnconfirmed));
    // Resource release is verified separately from permission to retire: the
    // configured owner must KEEP its failure fence even if this file is free.
    assert!(QueueStore::open(root.path(), key(), StateLimits::default()).is_ok());
}
#[tokio::test]
async fn panicked_release_job_is_sticky_unconfirmed() {
    let (_root, ledger, job) = owned_job();
    let task = tokio::spawn(async move {
        let _job = job;
        panic!("synthetic release panic");
    });
    assert!(task.await.unwrap_err().is_panic());
    assert_eq!(ledger.finish().await, Err(WorkerError::ReleaseUnconfirmed));
}
#[test]
fn lock_busy_drop_without_runtime_records_failure_instead_of_clean_retirement() {
    let (_root, mut queue, binding, task) = queue_fixture("synthetic body");
    let token = queue
        .begin_destination_attempt(&task, ForwardingChannel::Telegram, true, &binding, NOW)
        .unwrap();
    let queue = Arc::new(Mutex::new(queue));
    let ledger = Arc::new(ReleaseLedger::default());
    let guard = queue.try_lock().unwrap();
    drop(AttemptReservation::new(
        queue.clone(),
        &task,
        ForwardingChannel::Telegram,
        token,
        ledger.clone(),
    ));
    assert_eq!(ledger.state.lock().unwrap().pending, 0);
    assert!(ledger.state.lock().unwrap().failed);
    assert!(guard.inspect().unwrap().tasks[0].telegram.in_flight);
    drop(guard);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(
        runtime.block_on(ledger.finish()),
        Err(WorkerError::ReleaseUnconfirmed)
    );
}

#[tokio::test]
async fn release_capacity_is_64_and_exhaustion_never_spawns_or_clears_owner() {
    let mut mock = Mock::new(Vec::new()).await;
    let (_root, worker, binding, _) = worker_fixture(mock.endpoint.clone(), "synthetic body");
    let disabled =
        DestinationBinding::bark(&key(), "https://synthetic.invalid", "disabled").unwrap();
    let options = EnqueueOptions::new(true, &binding, false, &disabled);
    let mut guard = worker.queue.lock().await;
    let messages = (0..MAX_PENDING_RELEASES)
        .map(|n| {
            ForwardingMessage::new(
                format!("extra-owner-{n}"),
                "incoming",
                "synthetic",
                "synthetic body",
                "2026-01-02T03:04:05Z",
                "fixture",
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    guard.enqueue_and_persist(&messages, NOW, &options).unwrap();
    let tasks = guard
        .inspect()
        .unwrap()
        .tasks
        .into_iter()
        .map(|t| t.id)
        .collect::<Vec<_>>();
    assert_eq!(tasks.len(), MAX_PENDING_RELEASES + 1);
    let mut tokens = Vec::new();
    for task in &tasks {
        tokens.push(
            guard
                .begin_destination_attempt(task, ForwardingChannel::Telegram, true, &binding, NOW)
                .unwrap(),
        );
    }
    for (task, token) in tasks.iter().zip(tokens.iter()).take(MAX_PENDING_RELEASES) {
        drop(AttemptReservation::new(
            worker.queue.clone(),
            task,
            ForwardingChannel::Telegram,
            token.clone(),
            worker.releases.clone(),
        ));
    }
    assert_eq!(
        worker.releases.state.lock().unwrap().pending,
        MAX_PENDING_RELEASES
    );
    // Every pending job owns exactly one queue Arc. The rejected 65th cleanup
    // must create neither another capture nor another async task.
    assert_eq!(Arc::strong_count(&worker.queue), MAX_PENDING_RELEASES + 1);
    drop(AttemptReservation::new(
        worker.queue.clone(),
        &tasks[MAX_PENDING_RELEASES],
        ForwardingChannel::Telegram,
        tokens[MAX_PENDING_RELEASES].clone(),
        worker.releases.clone(),
    ));
    assert_eq!(
        worker.releases.state.lock().unwrap().pending,
        MAX_PENDING_RELEASES
    );
    assert_eq!(Arc::strong_count(&worker.queue), MAX_PENDING_RELEASES + 1);
    assert!(worker.releases.state.lock().unwrap().failed);
    assert!(
        guard
            .inspect()
            .unwrap()
            .tasks
            .iter()
            .all(|t| t.telegram.in_flight)
    );
    assert_eq!(
        worker.flush_pending().await,
        Err(WorkerError::ReleaseUnconfirmed)
    );
    assert_eq!(mock.count(), 0);
    let mut drain = Box::pin(worker.finish_releases());
    poll_fn(|cx| {
        assert!(matches!(drain.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    drop(drain);
    drop(guard);
    assert_eq!(
        worker.finish_releases().await,
        Err(WorkerError::ReleaseUnconfirmed)
    );
    assert_eq!(
        worker.finish_releases().await,
        Err(WorkerError::ReleaseUnconfirmed)
    );
    let after = worker.queue.lock().await.inspect().unwrap();
    assert!(
        after
            .tasks
            .iter()
            .find(|t| t.id == tasks[MAX_PENDING_RELEASES])
            .unwrap()
            .telegram
            .in_flight
    );
    assert_eq!(Arc::strong_count(&worker.queue), 1);
    mock.join().await;
}

#[tokio::test]
async fn release_failure_after_first_candidate_prevents_next_begin_in_same_flush() {
    let (held, ack) = Reply::held(OK);
    let mut mock = Mock::new(vec![held]).await;
    let (_root, worker, binding, _) = worker_fixture(mock.endpoint.clone(), "first synthetic body");
    let disabled =
        DestinationBinding::bark(&key(), "https://synthetic.invalid", "disabled").unwrap();
    worker
        .queue
        .lock()
        .await
        .enqueue_and_persist(
            &[ForwardingMessage::new(
                "second-queued",
                "incoming",
                "synthetic",
                "second synthetic body",
                "2026-01-02T03:04:05Z",
                "fixture",
            )
            .unwrap()],
            NOW,
            &EnqueueOptions::new(true, &binding, false, &disabled),
        )
        .unwrap();
    let flush = {
        let w = worker.clone();
        tokio::spawn(async move { w.flush_pending().await })
    };
    mock.next(1).await;
    // Inject an independently confirmed cleanup error while the first accepted
    // attempt is in provider I/O. It may finish, but the later candidate must
    // not acquire a token just because this flush began before the error.
    worker.releases.state.lock().unwrap().failed = true;
    ack.send(()).unwrap();
    assert_eq!(flush.await.unwrap(), Err(WorkerError::ReleaseUnconfirmed));
    let remaining = worker.queue.lock().await.inspect().unwrap();
    assert_eq!(remaining.tasks.len(), 1);
    assert_eq!(remaining.tasks[0].telegram.attempts, 0);
    assert!(!remaining.tasks[0].telegram.result_unknown);
    assert_eq!(mock.count(), 1);
    mock.join().await;
    assert_eq!(
        worker.finish_releases().await,
        Err(WorkerError::ReleaseUnconfirmed)
    );
}

#[tokio::test]
async fn poisoned_release_state_cannot_be_recovered_as_clean_completion() {
    let ledger = Arc::new(ReleaseLedger::default());
    let copy = ledger.clone();
    assert!(
        std::thread::spawn(move || {
            let _state = copy.state.lock().unwrap();
            panic!("synthetic state poison");
        })
        .join()
        .is_err()
    );
    assert_eq!(ledger.ensure_open(), Err(WorkerError::ReleaseUnconfirmed));
    assert_eq!(ledger.finish().await, Err(WorkerError::ReleaseUnconfirmed));
    assert_eq!(ledger.finish().await, Err(WorkerError::ReleaseUnconfirmed));
}

#[tokio::test]
async fn closed_admission_still_registers_accepted_owner_cleanup() {
    let (root, ledger, job) = owned_job();
    let queue = job.queue.as_ref().unwrap().clone();
    let token = job.token.clone();
    let task = job.task_id.clone();
    // The temporary component job was never submitted and therefore records a
    // failure; use its real owner with a fresh ledger for the close-order case.
    drop(job);
    assert_eq!(ledger.finish().await, Err(WorkerError::ReleaseUnconfirmed));
    let releases = Arc::new(ReleaseLedger::default());
    releases.close();
    let guard = queue.lock().await;
    drop(AttemptReservation::new(
        queue.clone(),
        &task,
        ForwardingChannel::Telegram,
        token,
        releases.clone(),
    ));
    assert_eq!(releases.state.lock().unwrap().pending, 1);
    drop(guard);
    releases.finish().await.unwrap();
    drop(queue);
    assert!(QueueStore::open(root.path(), key(), StateLimits::default()).is_ok());
}

#[tokio::test]
async fn already_true_shutdown_prevents_real_worker_attempt_and_provider_submission() {
    let mut mock = Mock::new(vec![Reply::json(OK)]).await;
    let (root, worker, _binding, _task) = worker_fixture(mock.endpoint.clone(), "synthetic body");
    let bytes = fs::read(root.path()).unwrap();
    let (_stop, receiver) = watch::channel(true);
    tokio::time::timeout(Duration::from_secs(1), worker.run(receiver))
        .await
        .unwrap()
        .unwrap();
    let state = worker.queue.lock().await.inspect().unwrap();
    assert_eq!(state.tasks.len(), 1);
    assert_eq!(state.tasks[0].telegram.attempts, 0);
    assert!(!state.tasks[0].telegram.in_flight);
    assert!(!state.tasks[0].telegram.result_unknown);
    assert!(state.tasks[0].telegram.telegram_checkpoint.is_none());
    assert_eq!(mock.count(), 0);
    assert_eq!(fs::read(root.path()).unwrap(), bytes);
    worker.finish_releases().await.unwrap();
    let server = mock.task.take().unwrap();
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
