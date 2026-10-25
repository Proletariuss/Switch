//! Per-configuration ownership of queue commands that outlive their observers.
use super::ServiceState;
use proletarius_forwarding::QueueStore;
use proletarius_store::StoreError;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{MutexGuard, Notify, OwnedMutexGuard};

const CAPACITY: usize = 64;
const MAX_ORPHAN_DIAGNOSTICS: usize = 64;

#[derive(Default)]
pub(super) struct Owner {
    state: StdMutex<State>,
    changed: Notify,
    #[cfg(test)]
    hooks: StdMutex<Vec<(Stage, Arc<TestGate>)>>,
    #[cfg(test)]
    test_pool: StdMutex<Option<crate::blocking_pool::BlockingPool>>,
}
#[derive(Default)]
struct State {
    closed: bool,
    pending: usize,
    orphan_errors: usize,
    omitted_errors: usize,
    unconfirmed: bool,
}
#[derive(Clone, Copy)]
enum Outcome {
    Clean,
    KnownError,
    Unconfirmed,
}
fn failure(code: &str) -> StoreError {
    StoreError::Configuration(code.into())
}
impl Owner {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| {
            let mut state = error.into_inner();
            state.unconfirmed = true;
            state
        })
    }
    pub(super) fn close(&self) {
        self.state().closed = true;
    }
    fn register(self: &Arc<Self>) -> Result<(Arc<Completion>, Observer), StoreError> {
        let mut state = self.state();
        if state.closed {
            return Err(failure("sms_forwarding_queue_io_stopping"));
        }
        if state.unconfirmed {
            return Err(failure("sms_forwarding_queue_io_unconfirmed"));
        }
        if state.pending >= CAPACITY {
            return Err(failure("sms_forwarding_queue_io_busy"));
        }
        let Some(pending) = state.pending.checked_add(1) else {
            state.unconfirmed = true;
            return Err(failure("sms_forwarding_queue_io_unconfirmed"));
        };
        state.pending = pending;
        let completion = Arc::new(Completion {
            owner: self.clone(),
            state: StdMutex::new(CompletionState::default()),
        });
        Ok((
            completion.clone(),
            Observer {
                completion,
                delivered: false,
            },
        ))
    }
    fn settle(&self, outcome: Outcome, delivered: bool) {
        {
            let mut state = self.state();
            match state.pending.checked_sub(1) {
                Some(n) => state.pending = n,
                None => state.unconfirmed = true,
            }
            match outcome {
                Outcome::Unconfirmed => state.unconfirmed = true,
                Outcome::KnownError if !delivered => {
                    if state.orphan_errors < MAX_ORPHAN_DIAGNOSTICS {
                        state.orphan_errors = state.orphan_errors.saturating_add(1);
                    } else {
                        state.omitted_errors = state.omitted_errors.saturating_add(1);
                    }
                }
                _ => {}
            }
        }
        self.changed.notify_waiters();
    }
    pub(super) async fn finish(&self) -> Result<(), &'static str> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state();
                if state.pending == 0 {
                    return if state.unconfirmed {
                        Err("sms_forwarding_queue_io_unconfirmed")
                    } else {
                        Ok(())
                    };
                }
            }
            changed.await;
        }
    }
    #[cfg(test)]
    pub(super) fn test_snapshot(&self) -> (usize, bool, usize, bool) {
        let s = self.state();
        (s.pending, s.closed, s.orphan_errors, s.unconfirmed)
    }
    #[cfg(test)]
    pub(super) fn test_unconfirmed(&self) {
        self.state().unconfirmed = true;
        self.changed.notify_waiters();
    }
    #[cfg(test)]
    pub(super) fn test_private_executor(&self, concurrency: usize) {
        *self.test_pool.lock().unwrap() =
            Some(crate::blocking_pool::BlockingPool::new(concurrency));
    }
    #[cfg(test)]
    pub(super) fn test_gate(
        &self,
        stage: Stage,
        fail: bool,
    ) -> (Arc<TestGate>, std::sync::mpsc::Sender<()>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let gate = Arc::new(TestGate {
            entered: Notify::new(),
            wait: StdMutex::new(rx),
            fail,
        });
        self.hooks.lock().unwrap().push((stage, gate.clone()));
        (gate, tx)
    }
    #[cfg(test)]
    pub(super) fn checkpoint(&self, stage: Stage) -> Result<(), StoreError> {
        let gate = {
            let mut hooks = self.hooks.lock().unwrap();
            hooks
                .iter()
                .position(|(s, _)| *s == stage)
                .map(|index| hooks.remove(index).1)
        };
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.wait
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|_| failure("queue_io_test_gate_timeout"))?;
            if gate.fail {
                return Err(failure("queue_io_test_known_error"));
            }
        }
        Ok(())
    }
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    Started,
    QueueCommitted,
}
#[cfg(test)]
pub(super) struct TestGate {
    pub(super) entered: Notify,
    wait: StdMutex<std::sync::mpsc::Receiver<()>>,
    fail: bool,
}

#[derive(Default)]
struct CompletionState {
    worker: Option<Outcome>,
    observer: Option<bool>,
    settled: bool,
}
struct Completion {
    owner: Arc<Owner>,
    state: StdMutex<CompletionState>,
}
impl Completion {
    fn update(&self, change: impl FnOnce(&mut CompletionState)) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                self.owner.state().unconfirmed = true;
                error.into_inner()
            }
        };
        change(&mut state);
        if state.settled {
            return;
        }
        if let (Some(outcome), Some(delivered)) = (state.worker, state.observer) {
            state.settled = true;
            drop(state);
            self.owner.settle(outcome, delivered);
        }
    }
}
struct Observer {
    completion: Arc<Completion>,
    delivered: bool,
}
impl Observer {
    fn delivered(&mut self) {
        self.completion.update(|s| s.observer = Some(true));
        self.delivered = true;
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        if !self.delivered {
            self.completion.update(|s| s.observer = Some(false));
        }
    }
}

struct Command<F> {
    guard: Option<OwnedMutexGuard<QueueStore>>,
    operation: Option<F>,
    completion: Arc<Completion>,
    started: bool,
    outcome: Option<Outcome>,
}
impl<F> Command<F> {
    fn execute<T>(mut self) -> Result<T, StoreError>
    where
        F: FnOnce(&mut QueueStore) -> Result<T, StoreError>,
    {
        self.started = true;
        #[cfg(test)]
        let result = self.completion.owner.checkpoint(Stage::Started);
        #[cfg(not(test))]
        let result: Result<(), StoreError> = Ok(());
        let result = result.and_then(|()| {
            self.operation.take().expect("owned queue operation")(
                self.guard.as_mut().expect("owned queue guard"),
            )
        });
        self.outcome = Some(if result.is_ok() {
            Outcome::Clean
        } else {
            Outcome::KnownError
        });
        // Drop owns the resource-before-completion order, including unwind.
        drop(self);
        result
    }
}
impl<F> Drop for Command<F> {
    fn drop(&mut self) {
        if self.started && self.outcome.is_none() {
            // An unwind can cross rename without updating the memory document.
            // Fence the shared Store before its next runner/producer guard.
            if let Some(queue) = self.guard.as_mut() {
                queue.require_reopen();
            }
        }
        drop(self.guard.take());
        drop(self.operation.take());
        let outcome = self.outcome.unwrap_or(if self.started {
            Outcome::Unconfirmed
        } else {
            Outcome::Clean
        });
        self.completion.update(|s| s.worker = Some(outcome));
    }
}

// Preparation and running observer have one explicit destructor. In particular
// a cancelled lock future or queued archive future cannot retain a queue Arc
// after releasing the service state that authorizes replacement.
struct Request<'a, F, T> {
    state: Option<MutexGuard<'a, ServiceState>>,
    locking: Option<Pin<Box<dyn Future<Output = OwnedMutexGuard<QueueStore>> + Send + 'a>>>,
    guard: Option<OwnedMutexGuard<QueueStore>>,
    operation: Option<F>,
    job: Option<Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>>,
    observer: Option<Observer>,
}
impl<F, T> Drop for Request<'_, F, T> {
    fn drop(&mut self) {
        drop(self.job.take());
        drop(self.locking.take());
        drop(self.guard.take());
        drop(self.operation.take());
        drop(self.observer.take());
        drop(self.state.take());
    }
}
pub(super) fn run<'a, F, T>(
    state: MutexGuard<'a, ServiceState>,
    queue: Arc<tokio::sync::Mutex<QueueStore>>,
    owner: Arc<Owner>,
    operation: F,
) -> impl Future<Output = Result<T, StoreError>> + Send + 'a
where
    F: FnOnce(&mut QueueStore) -> Result<T, StoreError> + Send + 'static,
    T: Send + 'static,
{
    let request = Request {
        state: Some(state),
        locking: Some(Box::pin(queue.lock_owned())),
        guard: None,
        operation: Some(operation),
        job: None,
        observer: None,
    };
    async move {
        let mut request = request;
        let guard = request.locking.as_mut().unwrap().as_mut().await;
        request.guard = Some(guard);
        drop(request.locking.take());
        // Still under state, with no await between Ready, registration and the
        // unique command envelope. Capacity refusal executes no mutation.
        let (completion, observer) = owner.register()?;
        request.observer = Some(observer);
        let command = Command {
            guard: request.guard.take(),
            operation: request.operation.take(),
            completion,
            started: false,
            outcome: None,
        };
        #[cfg(test)]
        let private_pool = owner.test_pool.lock().unwrap().clone();
        #[cfg(test)]
        if let Some(pool) = private_pool {
            request.job = Some(Box::pin(async move {
                pool.run(move || command.execute())
                    .await
                    .map_err(|_| failure("sms_forwarding_queue_io_test_worker_failed"))?
            }));
        } else {
            request.job = Some(Box::pin(crate::archive_io::run(move || command.execute())));
        }
        #[cfg(not(test))]
        {
            request.job = Some(Box::pin(crate::archive_io::run(move || command.execute())));
        }
        let result = request.job.as_mut().unwrap().as_mut().await;
        request.observer.as_mut().unwrap().delivered();
        drop(request);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proletarius_forwarding::{StateKey, StateLimits};
    use std::{future::poll_fn, task::Poll};

    fn queue() -> (std::path::PathBuf, QueueStore) {
        let root = super::super::tests::test_root();
        let queue = QueueStore::open(
            root.join("owned-queue.enc"),
            StateKey::from_slice(&[23; 32]).unwrap(),
            StateLimits::default(),
        )
        .unwrap();
        (root, queue)
    }
    fn reopen(root: &std::path::Path) -> QueueStore {
        QueueStore::open(
            root.join("owned-queue.enc"),
            StateKey::from_slice(&[23; 32]).unwrap(),
            StateLimits::default(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn unpolled_request_owns_preparation_before_returning_future() {
        let (root, queue) = queue();
        let queue = Arc::new(tokio::sync::Mutex::new(queue));
        let state = tokio::sync::Mutex::new(ServiceState::default());
        let owner = Arc::new(Owner::default());
        let request = run(state.lock().await, queue.clone(), owner.clone(), |_| Ok(()));
        assert!(state.try_lock().is_err());
        assert_eq!(Arc::strong_count(&queue), 2);
        // Do not poll: the synchronous constructor already owns an explicit
        // preparation envelope, whose Drop releases queue captures then state.
        drop(request);
        assert!(state.try_lock().is_ok());
        assert_eq!(Arc::strong_count(&queue), 1);
        assert_eq!(owner.test_snapshot().0, 0);
        drop(queue);
        drop(reopen(&root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn preparation_cancel_drops_lock_future_and_queue_capture_before_state() {
        let (root, queue) = queue();
        let queue = Arc::new(tokio::sync::Mutex::new(queue));
        let blocked = queue.lock().await;
        let state = tokio::sync::Mutex::new(ServiceState::default());
        let owner = Arc::new(Owner::default());
        let mut request = Box::pin(run(
            state.lock().await,
            queue.clone(),
            owner.clone(),
            |_| Ok(()),
        ));
        assert!(poll_fn(|cx| Poll::Ready(request.as_mut().poll(cx).is_pending())).await);
        drop(request);
        assert!(state.try_lock().is_ok());
        assert_eq!(Arc::strong_count(&queue), 1);
        assert_eq!(owner.test_snapshot().0, 0);
        drop(blocked);
        drop(queue);
        drop(reopen(&root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn real_pool_capacity_wait_cancel_drops_command_without_late_operation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (root, queue) = queue();
        let owner = Arc::new(Owner::default());
        owner.test_private_executor(1);
        let pool = owner.test_pool.lock().unwrap().clone().unwrap();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let occupied = tokio::spawn(async move {
            pool.run(move || {
                entered.send(()).unwrap();
                wait.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            })
            .await
            .unwrap();
        });
        started.await.unwrap();
        let state = tokio::sync::Mutex::new(ServiceState::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let marker = root.join("unexpected-capacity-write");
        let target = marker.clone();
        // The actual run owns the sole QueueStore Arc; only its command can
        // hold this flock while the real one-slot private pool is occupied.
        let mut request = Box::pin(run(
            state.lock().await,
            Arc::new(tokio::sync::Mutex::new(queue)),
            owner.clone(),
            move |_| {
                called.fetch_add(1, Ordering::SeqCst);
                std::fs::write(target, b"unexpected").unwrap();
                Ok(())
            },
        ));
        assert!(poll_fn(|cx| Poll::Ready(request.as_mut().poll(cx).is_pending())).await);
        assert_eq!(owner.test_snapshot().0, 1);
        assert!(matches!(
            QueueStore::open(
                root.join("owned-queue.enc"),
                StateKey::from_slice(&[23; 32]).unwrap(),
                StateLimits::default()
            ),
            Err(proletarius_forwarding::StateStoreError::Busy)
        ));
        drop(request);
        assert!(state.try_lock().is_ok());
        owner.close();
        tokio::time::timeout(std::time::Duration::from_secs(3), owner.finish())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(owner.test_snapshot(), (0, true, 0, false));
        drop(reopen(&root));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!marker.exists());
        release.send(()).unwrap();
        occupied.await.unwrap();
        // Capacity is now free, but the cancelled operation must not emerge.
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!marker.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn real_tokio_queued_abort_waits_for_actual_command_drop_before_completion() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (root, queue) = queue();
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let occupied = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                wait.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            });
            started.await.unwrap();
            let owner = Arc::new(Owner::default());
            owner.test_private_executor(1);
            let state = tokio::sync::Mutex::new(ServiceState::default());
            let calls = Arc::new(AtomicUsize::new(0));
            let called = calls.clone();
            let marker = root.join("unexpected-tokio-write");
            let target = marker.clone();
            // Private pool capacity is free; this first poll reaches its real
            // spawn_blocking, queued behind the sole occupied Tokio worker.
            let mut request = Box::pin(run(
                state.lock().await,
                Arc::new(tokio::sync::Mutex::new(queue)),
                owner.clone(),
                move |_| {
                    called.fetch_add(1, Ordering::SeqCst);
                    std::fs::write(target, b"unexpected").unwrap();
                    Ok(())
                },
            ));
            assert!(poll_fn(|cx| Poll::Ready(request.as_mut().poll(cx).is_pending())).await);
            assert_eq!(owner.test_snapshot().0, 1);
            assert!(matches!(
                QueueStore::open(
                    root.join("owned-queue.enc"),
                    StateKey::from_slice(&[23; 32]).unwrap(),
                    StateLimits::default()
                ),
                Err(proletarius_forwarding::StateStoreError::Busy)
            ));
            drop(request); // abort request is not itself a completion receipt
            assert!(state.try_lock().is_ok());
            owner.close();
            let mut finish = Box::pin(owner.finish());
            let pending = poll_fn(|cx| Poll::Ready(finish.as_mut().poll(cx).is_pending())).await;
            // Tokio may destroy a queued closure immediately or when dequeued.
            // If completion is observable already, the sole Store capture must
            // already be gone. Otherwise the ledger continues to own the wait.
            if !pending {
                assert_eq!(owner.test_snapshot(), (0, true, 0, false));
                drop(reopen(&root));
            }
            drop(finish);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(!marker.exists());
            release.send(()).unwrap();
            occupied.await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(3), owner.finish())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(owner.test_snapshot(), (0, true, 0, false));
            drop(reopen(&root));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(!marker.exists());
            std::fs::remove_dir_all(root).unwrap();
        });
    }

    #[tokio::test]
    async fn command_releases_last_guard_before_success_and_unstarted_cancel_completion() {
        for execute in [false, true] {
            let (root, queue) = queue();
            let guard = Arc::new(tokio::sync::Mutex::new(queue)).lock_owned().await;
            let owner = Arc::new(Owner::default());
            let (completion, observer) = owner.register().unwrap();
            drop(observer);
            let command = Command {
                guard: Some(guard),
                operation: Some(|_: &mut QueueStore| Ok::<_, StoreError>(())),
                completion,
                started: false,
                outcome: None,
            };
            if execute {
                command.execute().unwrap();
            } else {
                drop(command);
            }
            owner.finish().await.unwrap();
            // No task join or extra queue capture proves this resource release.
            drop(reopen(&root));
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn started_command_panic_releases_guard_but_keeps_unconfirmed_fence() {
        let (root, queue) = queue();
        let queue = Arc::new(tokio::sync::Mutex::new(queue));
        let guard = queue.clone().lock_owned().await;
        let key = StateKey::from_slice(&[23; 32]).unwrap();
        let telegram =
            proletarius_forwarding::DestinationBinding::telegram(&key, "synthetic", "synthetic");
        let bark = proletarius_forwarding::DestinationBinding::bark(
            &key,
            "https://synthetic.invalid",
            "synthetic",
        )
        .unwrap();
        let options = proletarius_forwarding::EnqueueOptions::new(true, &telegram, false, &bark);
        let message = proletarius_forwarding::ForwardingMessage::new(
            "command-committed-before-panic",
            "in",
            "sender",
            "synthetic",
            "2026-01-01T00:00:00Z",
            "reader",
        )
        .unwrap();
        let blocked_message = message.clone();
        let blocked_options =
            proletarius_forwarding::EnqueueOptions::new(true, &telegram, false, &bark);
        let owner = Arc::new(Owner::default());
        let (completion, observer) = owner.register().unwrap();
        drop(observer);
        let command = Command {
            guard: Some(guard),
            operation: Some(move |queue: &mut QueueStore| -> Result<(), StoreError> {
                queue
                    .enqueue_and_persist(&[message], "2026-01-01T00:00:01Z", &options)
                    .unwrap();
                panic!("synthetic queue command panic")
            }),
            completion,
            started: false,
            outcome: None,
        };
        assert!(
            std::thread::spawn(move || command.execute())
                .join()
                .is_err()
        );
        assert_eq!(
            owner.finish().await,
            Err("sms_forwarding_queue_io_unconfirmed")
        );
        let disk_after_panic = std::fs::read(root.join("owned-queue.enc")).unwrap();
        {
            let mut queue = queue.lock().await;
            assert_eq!(
                queue.inspect(),
                Err(proletarius_forwarding::StateStoreError::ReopenRequired)
            );
            assert_eq!(
                queue.enqueue_and_persist(
                    &[blocked_message],
                    "2026-01-01T00:00:02Z",
                    &blocked_options
                ),
                Err(proletarius_forwarding::StateStoreError::ReopenRequired)
            );
        }
        assert_eq!(
            std::fs::read(root.join("owned-queue.enc")).unwrap(),
            disk_after_panic
        );
        drop(queue);
        assert_eq!(reopen(&root).inspect().unwrap().tasks.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn ordinary_orphan_error_is_bounded_and_does_not_fence_retirement() {
        let owner = Arc::new(Owner::default());
        for _ in 0..70 {
            let (completion, observer) = owner.register().unwrap();
            drop(observer);
            completion.update(|s| s.worker = Some(Outcome::KnownError));
        }
        owner.close();
        owner.finish().await.unwrap();
        assert_eq!(owner.test_snapshot(), (0, true, 64, false));
        assert_eq!(owner.state().omitted_errors, 6);
    }
    #[tokio::test]
    async fn capacity_rejection_does_not_poison_and_completion_is_two_sided() {
        let owner = Arc::new(Owner::default());
        let mut leases = Vec::new();
        for _ in 0..64 {
            leases.push(owner.register().unwrap());
        }
        assert!(owner.register().is_err());
        assert!(!owner.test_snapshot().3);
        for (completion, observer) in leases {
            completion.update(|s| s.worker = Some(Outcome::Clean));
            drop(observer);
        }
        owner.close();
        owner.finish().await.unwrap();
        assert_eq!(owner.test_snapshot(), (0, true, 0, false));
        assert!(owner.register().is_err());
    }
    #[tokio::test]
    async fn unconfirmed_and_poison_are_sticky() {
        let owner = Arc::new(Owner::default());
        let (completion, observer) = owner.register().unwrap();
        completion.update(|s| s.worker = Some(Outcome::Unconfirmed));
        drop(observer);
        assert_eq!(
            owner.finish().await,
            Err("sms_forwarding_queue_io_unconfirmed")
        );
        let copy = owner.clone();
        assert!(
            std::thread::spawn(move || {
                let _s = copy.state.lock().unwrap();
                panic!("synthetic owner poison");
            })
            .join()
            .is_err()
        );
        assert!(owner.register().is_err());
        assert_eq!(
            owner.finish().await,
            Err("sms_forwarding_queue_io_unconfirmed")
        );
    }
}
