use super::*;
use std::future::{Future, poll_fn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;

fn failure(message: &str) -> StoreError {
    StoreError::Io(std::io::Error::other(message.to_owned()))
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_archive_work_does_not_block_executor() {
    let executor = Arc::new(Executor::new(2));
    let (send, receive) = std::sync::mpsc::channel();
    let (started, ready) = tokio::sync::oneshot::channel();
    let work = executor.clone();
    let task = tokio::spawn(async move {
        work.run(move || {
            started.send(()).unwrap();
            receive.recv_timeout(Duration::from_secs(3)).unwrap();
            Ok(42)
        })
        .await
    });
    ready.await.unwrap();
    assert_eq!(executor.run(|| Ok(7)).await.unwrap(), 7);
    send.send(()).unwrap();
    assert_eq!(task.await.unwrap().unwrap(), 42);
    executor.ledger.finish_pending().await.unwrap();
}

#[tokio::test]
async fn cancelled_waiter_keeps_real_archive_worker_and_reports_orphan_error() {
    let executor = Arc::new(Executor::new(8));
    let (release, wait) = std::sync::mpsc::channel();
    let (started, ready) = tokio::sync::oneshot::channel();
    let work = executor.clone();
    let waiter = tokio::spawn(async move {
        work.run(move || {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            Err::<(), _>(failure("blocked orphan write failed"))
        })
        .await
    });
    ready.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    // An ordinary sentinel can run on another slot while the first worker
    // remains blocked. The completion ledger must still wait for that worker.
    executor.run(|| Ok(())).await.unwrap();
    let mut drain = Box::pin(executor.ledger.finish_pending());
    assert!(poll_fn(|cx| Poll::Ready(drain.as_mut().poll(cx).is_pending())).await);
    release.send(()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(3), drain)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.contains("blocked orphan write failed"));
    assert_eq!(executor.ledger.finish_pending().await.unwrap_err(), error);
}

#[tokio::test]
async fn delivered_business_error_is_clean_but_started_panic_is_unconfirmed() {
    let executor = Executor::new(1);
    assert!(
        executor
            .run(|| Err::<(), _>(failure("already handled")))
            .await
            .is_err()
    );
    executor.ledger.finish_pending().await.unwrap();
    assert!(
        executor
            .run(|| -> Result<(), StoreError> { panic!("handled panic") })
            .await
            .is_err()
    );
    assert!(
        executor
            .ledger
            .finish_pending()
            .await
            .unwrap_err()
            .contains("unconfirmed")
    );
    // Sticky failure does not close healthy accounting/resource admission.
    assert_eq!(executor.run(|| Ok(9)).await.unwrap(), 9);
    assert!(executor.ledger.finish_pending().await.is_err());
}

#[tokio::test]
async fn cancelled_waiter_records_worker_panic_and_success_is_clean() {
    for panic_worker in [false, true] {
        let executor = Arc::new(Executor::new(1));
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let work = executor.clone();
        let waiter = tokio::spawn(async move {
            work.run(move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
                assert!(!panic_worker, "orphan panic");
                Ok(())
            })
            .await
        });
        ready.await.unwrap();
        waiter.abort();
        let _ = waiter.await;
        release.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), executor.ledger.finish_pending())
            .await
            .unwrap();
        if panic_worker {
            assert!(result.unwrap_err().contains("archive worker panicked"));
        } else {
            result.unwrap();
        }
    }
}

#[tokio::test]
async fn cancellation_while_waiting_for_pool_capacity_never_runs_operation() {
    let executor = Arc::new(Executor::new(1));
    let (release, wait) = std::sync::mpsc::channel();
    let (started, ready) = tokio::sync::oneshot::channel();
    let work = executor.clone();
    let first = tokio::spawn(async move {
        work.run(move || {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            Ok(())
        })
        .await
    });
    ready.await.unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let flag = called.clone();
    let mut queued = Box::pin(executor.run(move || {
        flag.store(true, Ordering::SeqCst);
        Ok(())
    }));
    assert!(poll_fn(|cx| Poll::Ready(queued.as_mut().poll(cx).is_pending())).await);
    drop(queued);
    release.send(()).unwrap();
    first.await.unwrap().unwrap();
    executor.ledger.finish_pending().await.unwrap();
    assert!(!called.load(Ordering::SeqCst));
}

#[test]
fn cancellation_after_submission_before_execution_never_runs_operation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let executor = Executor::new(2);
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let occupied = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        ready.await.unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let flag = called.clone();
        let mut queued = Box::pin(executor.run(move || {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        }));
        assert!(poll_fn(|cx| Poll::Ready(queued.as_mut().poll(cx).is_pending())).await);
        drop(queued);
        release.send(()).unwrap();
        occupied.await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), executor.ledger.finish_pending())
            .await
            .unwrap()
            .unwrap();
        assert!(!called.load(Ordering::SeqCst));
    });
}
// All fixtures below own a private Executor. No fault reaches EXECUTOR.
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::task::{Context, Wake, Waker};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Files {
    directory: PathBuf,
    path: PathBuf,
}
impl Files {
    fn new() -> Self {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../build/runtime/archive-io-tests")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("resource.lock");
        fs::write(&path, b"original").unwrap();
        Self { directory, path }
    }

    fn resource(&self, control: Option<DropControl>) -> (Resource, Arc<AtomicBool>) {
        let file = self.open();
        file.lock().unwrap();
        let released = Arc::new(AtomicBool::new(false));
        (
            Resource {
                _file: Some(file),
                control,
                _tail: ResourceTail(released.clone()),
            },
            released,
        )
    }

    fn open(&self) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .unwrap()
    }

    fn assert_locked(&self) {
        assert!(self.open().try_lock().is_err());
    }

    fn assert_released(&self) {
        self.open().try_lock().unwrap();
        assert_eq!(fs::read(&self.path).unwrap(), b"original");
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}
struct DropControl {
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
    panic: bool,
}
struct Resource {
    // Fields release in this declaration order, even when custom Drop panics.
    _file: Option<File>,
    control: Option<DropControl>,
    _tail: ResourceTail,
}
impl Drop for Resource {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            let _ = control.entered.send(());
            control
                .release
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            assert!(!control.panic, "payload destructor fault");
        }
    }
}
struct ResourceTail(Arc<AtomicBool>);
impl Drop for ResourceTail {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn pending<F: Future>(mut future: Pin<&mut F>) {
    assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}
async fn drain(executor: &Executor) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), executor.ledger.finish_pending())
        .await
        .unwrap()
}
#[derive(Default)]
struct ReadyWake(AtomicBool);
impl Wake for ReadyWake {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}
fn start_with_ready_wake<F: Future>(mut future: Pin<&mut F>) -> Arc<ReadyWake> {
    let signal = Arc::new(ReadyWake::default());
    let waker = Waker::from(signal.clone());
    let mut context = Context::from_waker(&waker);
    assert!(future.as_mut().poll(&mut context).is_pending());
    signal
}

#[tokio::test]
async fn unpolled_request_owns_f_until_its_explicit_drop_and_never_executes() {
    let executor = Executor::new(1);
    let files = Files::new();
    let (resource, released) = files.resource(None);
    let called = Arc::new(AtomicBool::new(false));
    let flag = called.clone();
    let request = executor.run(move || {
        flag.store(true, Ordering::Release);
        drop(resource);
        Ok(())
    });
    assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 1);
    let completion = executor.completion();
    assert_eq!(completion.facts.load(Ordering::Acquire), 0);
    files.assert_locked();
    drop(request);
    assert!(released.load(Ordering::Acquire));
    files.assert_released();
    assert!(!called.load(Ordering::Acquire));
    assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 0);
    drain(&executor).await.unwrap();
}

#[tokio::test]
async fn unpolled_f_drop_barrier_precedes_completion_including_single_unwind() {
    for panic in [false, true] {
        let executor = Executor::new(1);
        let files = Files::new();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let (resource, released) = files.resource(Some(DropControl {
            entered,
            release: wait,
            panic,
        }));
        let called = Arc::new(AtomicBool::new(false));
        let flag = called.clone();
        let request = executor.run(move || {
            flag.store(true, Ordering::Release);
            drop(resource);
            Ok(())
        });
        let completion = executor.completion();
        let dropping = std::thread::spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(request)))
        });
        entering.await.unwrap();
        files.assert_locked();
        assert_eq!(completion.facts.load(Ordering::Acquire) & JOB_DONE, 0);
        let mut finish = Box::pin(executor.ledger.finish_pending());
        pending(finish.as_mut()).await;
        release.send(()).unwrap();
        assert_eq!(dropping.join().unwrap().is_err(), panic);
        assert!(released.load(Ordering::Acquire));
        files.assert_released();
        assert!(!called.load(Ordering::Acquire));
        assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 0);
        assert_eq!(finish.await.is_err(), panic);
    }
}

#[tokio::test]
async fn real_capacity_wait_cancellation_drops_last_resource_without_late_write() {
    let executor = Arc::new(Executor::new(1));
    let (release, wait) = std::sync::mpsc::channel();
    let (started, ready) = tokio::sync::oneshot::channel();
    let occupied = executor.run(move || {
        started.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        Ok(())
    });
    let occupied = tokio::spawn(occupied);
    ready.await.unwrap();
    let files = Files::new();
    let (resource, released) = files.resource(None);
    let called = Arc::new(AtomicBool::new(false));
    let flag = called.clone();
    let marker = files.directory.join("must-not-write");
    let operation_marker = marker.clone();
    let mut request = Box::pin(executor.run(move || {
        flag.store(true, Ordering::Release);
        fs::write(operation_marker, b"late").unwrap();
        drop(resource);
        Ok(())
    }));
    pending(request.as_mut()).await;
    drop(request);
    assert!(released.load(Ordering::Acquire));
    files.assert_released();
    assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 1);
    let mut finish = Box::pin(executor.ledger.finish_pending());
    pending(finish.as_mut()).await;
    release.send(()).unwrap();
    occupied.await.unwrap().unwrap();
    finish.await.unwrap();
    assert!(!called.load(Ordering::Acquire));
    assert!(!marker.exists());
}

#[test]
fn real_tokio_queued_abort_waits_for_closure_drop_before_finish() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let executor = Executor::new(1);
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let occupied = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready.await.unwrap();
        let files = Files::new();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (drop_release, drop_wait) = std::sync::mpsc::channel();
        let (resource, released) = files.resource(Some(DropControl {
            entered,
            release: drop_wait,
            panic: false,
        }));
        let called = Arc::new(AtomicBool::new(false));
        let flag = called.clone();
        let marker = files.directory.join("queued-must-not-write");
        let operation_marker = marker.clone();
        let mut request = Box::pin(executor.run(move || {
            flag.store(true, Ordering::Release);
            fs::write(operation_marker, b"late").unwrap();
            drop(resource);
            Ok(())
        }));
        pending(request.as_mut()).await;
        let completion = executor.completion();
        // Release the occupied worker only after this thread has requested
        // cancellation. Releasing it from the test thread races with Drop and
        // can let the queued job start before its abort is issued.
        let dropping = std::thread::spawn(move || {
            drop(request);
            release.send(()).unwrap();
        });
        // Abort is only a request. Whether disposal starts synchronously or
        // later, the real destructor barrier still holds the last resource.
        files.assert_locked();
        assert_eq!(completion.facts.load(Ordering::Acquire) & JOB_DONE, 0);
        let mut finish = Box::pin(executor.ledger.finish_pending());
        pending(finish.as_mut()).await;
        entering.await.unwrap();
        pending(finish.as_mut()).await;
        files.assert_locked();
        drop_release.send(()).unwrap();
        dropping.join().unwrap();
        occupied.await.unwrap();
        finish.await.unwrap();
        assert!(released.load(Ordering::Acquire));
        files.assert_released();
        assert!(!called.load(Ordering::Acquire));
        assert!(!marker.exists());
    });
}

#[tokio::test]
async fn ready_ack_holds_last_resource_until_unconsumed_output_drop() {
    let executor = Executor::new(1);
    let files = Files::new();
    let (resource, released) = files.resource(None);
    let (release, wait) = std::sync::mpsc::channel();
    let mut request = Box::pin(executor.run(move || {
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        Ok(resource)
    }));
    let completion = executor.completion();
    let ready = start_with_ready_wake(request.as_mut());
    release.send(()).unwrap();
    until(|| ready.0.load(Ordering::Acquire)).await;
    assert_eq!(completion.facts.load(Ordering::Acquire), JOB_DONE);
    files.assert_locked();
    let mut finish = Box::pin(executor.ledger.finish_pending());
    pending(finish.as_mut()).await;
    drop(request);
    finish.await.unwrap();
    assert!(released.load(Ordering::Acquire));
    files.assert_released();
}

#[tokio::test]
async fn ready_t_drop_barrier_and_unwind_are_after_last_owned_field_release() {
    for panic in [false, true] {
        let executor = Executor::new(1);
        let files = Files::new();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (release_drop, wait_drop) = std::sync::mpsc::channel();
        let (resource, released) = files.resource(Some(DropControl {
            entered,
            release: wait_drop,
            panic,
        }));
        let (release_job, wait_job) = std::sync::mpsc::channel();
        let mut request = Box::pin(executor.run(move || {
            wait_job.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(resource)
        }));
        let completion = executor.completion();
        let ready = start_with_ready_wake(request.as_mut());
        release_job.send(()).unwrap();
        until(|| ready.0.load(Ordering::Acquire)).await;
        assert_eq!(completion.facts.load(Ordering::Acquire), JOB_DONE);
        let dropping = std::thread::spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(request)))
        });
        entering.await.unwrap();
        files.assert_locked();
        assert_eq!(completion.facts.load(Ordering::Acquire) & OUTPUT_DONE, 0);
        let mut finish = Box::pin(executor.ledger.finish_pending());
        pending(finish.as_mut()).await;
        release_drop.send(()).unwrap();
        // Tokio may swallow a completed JoinHandle output's destructor panic.
        // The inner OutputWitness, not propagation to this thread, must keep
        // the resource fault sticky after normal unwind field cleanup.
        let _outer_drop_result = dropping.join().unwrap();
        assert!(released.load(Ordering::Acquire));
        files.assert_released();
        assert_eq!(finish.await.is_err(), panic);
        assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 0);
    }
}

#[tokio::test]
async fn live_result_transfer_does_not_claim_caller_owned_resource_destruction() {
    let executor = Executor::new(1);
    let files = Files::new();
    let (resource, released) = files.resource(None);
    let resource = executor.run(move || Ok(resource)).await.unwrap();
    drain(&executor).await.unwrap();
    files.assert_locked();
    assert!(!released.load(Ordering::Acquire));
    drop(resource);
    assert!(released.load(Ordering::Acquire));
    files.assert_released();
}

#[tokio::test]
async fn diagnostic_and_completion_poison_wait_for_both_running_job_and_ready_output() {
    for poison_completion in [false, true] {
        let executor = Executor::new(2);
        let files = Files::new();
        let (resource, released) = files.resource(None);
        let (release_output, wait_output) = std::sync::mpsc::channel();
        let mut output = Box::pin(executor.run(move || {
            wait_output.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(resource)
        }));
        let output_completion = executor.completion();
        let ready = start_with_ready_wake(output.as_mut());
        release_output.send(()).unwrap();
        until(|| ready.0.load(Ordering::Acquire)).await;
        let (started, starting) = tokio::sync::oneshot::channel();
        let (release_job, wait_job) = std::sync::mpsc::channel();
        let job = tokio::spawn(executor.run(move || {
            started.send(()).unwrap();
            wait_job.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        }));
        starting.await.unwrap();
        let ledger = executor.ledger.clone();
        let fault = std::thread::spawn(move || {
            if poison_completion {
                let _guard = output_completion.error.lock().unwrap();
                panic!("completion diagnostic poison");
            } else {
                let _guard = ledger.diagnostics.lock().unwrap();
                panic!("ledger diagnostic poison");
            }
        });
        assert!(fault.join().is_err());
        assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 2);
        let mut finish = Box::pin(executor.ledger.finish_pending());
        pending(finish.as_mut()).await;
        // Cancelling this observer loses no registration or wakeup.
        drop(finish);
        release_job.send(()).unwrap();
        job.await.unwrap().unwrap();
        assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 1);
        let mut finish = Box::pin(executor.ledger.finish_pending());
        pending(finish.as_mut()).await;
        files.assert_locked();
        drop(output);
        assert!(released.load(Ordering::Acquire));
        files.assert_released();
        let error = finish.await.unwrap_err();
        assert!(error.contains("unconfirmed"));
        // Diagnostic poison does not reject already accepted cleanup work.
        assert_eq!(executor.run(|| Ok(17)).await.unwrap(), 17);
        assert!(drain(&executor).await.unwrap_err().contains("unconfirmed"));
    }
}

#[tokio::test]
async fn diagnostic_poison_with_no_actual_owner_is_sticky_and_allows_cleanup() {
    let executor = Executor::new(1);
    let ledger = executor.ledger.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = ledger.diagnostics.lock().unwrap();
            panic!("empty ledger poison");
        })
        .join()
        .is_err()
    );
    assert!(drain(&executor).await.unwrap_err().contains("unconfirmed"));
    assert_eq!(executor.run(|| Ok(3)).await.unwrap(), 3);
    assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 0);
    assert!(drain(&executor).await.is_err());
}

#[tokio::test]
async fn duplicate_facts_settle_once_and_checked_underflow_never_wraps() {
    let executor = Executor::new(1);
    let request = executor.run(|| Ok(()));
    let completion = executor.completion();
    request.await.unwrap();
    for _ in 0..5 {
        completion.mark(JOB_DONE);
        completion.mark(OBSERVER_DONE);
        completion.mark(OUTPUT_DONE);
    }
    assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 0);
    drain(&executor).await.unwrap();

    // Isolated inconsistent accounting fixture, not a recoverable live job.
    let ledger = Arc::new(Ledger::default());
    let completion = Completion::new(ledger.clone());
    completion.armed.store(true, Ordering::Release);
    completion.observer.store(2, Ordering::Release);
    completion.mark(ALL_DONE);
    assert_eq!(ledger.pending.load(Ordering::Acquire), 0);
    assert!(ledger.accounting_failed.load(Ordering::Acquire));
    assert!(
        ledger
            .finish_pending()
            .await
            .unwrap_err()
            .contains("unconfirmed")
    );
    let refused = Completion::new(ledger.clone());
    refused.arm();
    assert!(!refused.armed.load(Ordering::Acquire));
}

#[tokio::test]
async fn checked_overflow_refuses_unarmed_operation_without_decrementing_fake_count() {
    let executor = Executor::new(1);
    // Counter-boundary injection only: there are no usize::MAX actual jobs.
    executor.ledger.pending.store(usize::MAX, Ordering::Release);
    let files = Files::new();
    let (resource, released) = files.resource(None);
    let called = Arc::new(AtomicBool::new(false));
    let flag = called.clone();
    let request = executor.run(move || {
        flag.store(true, Ordering::Release);
        drop(resource);
        Ok(())
    });
    let completion = executor.completion();
    assert!(!completion.armed.load(Ordering::Acquire));
    assert!(request.await.is_err());
    assert!(!called.load(Ordering::Acquire));
    assert!(released.load(Ordering::Acquire));
    files.assert_released();
    assert_eq!(executor.ledger.pending.load(Ordering::Acquire), usize::MAX);
    assert!(executor.ledger.accounting_failed.load(Ordering::Acquire));
    // Do not reset fake count or claim finish can reconstruct corrupted owners.
}

#[tokio::test]
async fn bounded_utf8_orphan_errors_preserve_original_caller_error_and_saturate_omissions() {
    let executor = Executor::new(1);
    let message = "秘密不应拆断UTF8边界".repeat(400);
    let expected = failure(&message).to_string();
    let original = executor
        .run(move || Err::<(), _>(failure(&message)))
        .await
        .unwrap_err();
    assert_eq!(original.to_string(), expected);
    drain(&executor).await.unwrap();
    for _ in 0..(MAX_ORPHAN_ERRORS + 3) {
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let mut request = Box::pin(executor.run(move || {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            Err::<(), _>(failure(&"中文".repeat(2000)))
        }));
        pending(request.as_mut()).await;
        ready.await.unwrap();
        drop(request);
        release.send(()).unwrap();
        until(|| executor.ledger.pending.load(Ordering::Acquire) == 0).await;
    }
    {
        let diagnostics = executor.ledger.diagnostics();
        assert_eq!(diagnostics.orphan_errors.len(), MAX_ORPHAN_ERRORS);
        assert_eq!(diagnostics.omitted_errors, 3);
        assert!(
            diagnostics.orphan_errors.iter().all(|error| {
                error.len() <= MAX_ERROR_BYTES && error.ends_with("...[truncated]")
            })
        );
    }
    let error = drain(&executor).await.unwrap_err();
    assert!(error.contains("3 further orphan archive failures"));
    assert_eq!(drain(&executor).await.unwrap_err(), error);
    // Explicit diagnostic-count boundary injection, not real MAX operations.
    executor.ledger.diagnostics().omitted_errors = usize::MAX;
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let mut request = Box::pin(executor.run(move || {
        started.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        Err::<(), _>(failure("one more"))
    }));
    pending(request.as_mut()).await;
    ready.await.unwrap();
    drop(request);
    release.send(()).unwrap();
    let error = drain(&executor).await.unwrap_err();
    assert!(error.contains(&format!("at least {} further", usize::MAX)));
    assert_eq!(executor.ledger.diagnostics().omitted_errors, usize::MAX);
}

#[tokio::test]
async fn diagnostic_formatter_unwind_remains_unconfirmed_after_error_delivery() {
    #[derive(Debug)]
    struct BadDisplay;
    impl std::fmt::Display for BadDisplay {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("diagnostic formatting fault");
        }
    }
    impl std::error::Error for BadDisplay {}
    let executor = Executor::new(1);
    assert!(
        executor
            .run(|| Err::<(), _>(StoreError::Io(std::io::Error::other(BadDisplay))))
            .await
            .is_err()
    );
    assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 0);
    assert!(drain(&executor).await.unwrap_err().contains("unconfirmed"));
    assert_eq!(executor.run(|| Ok(11)).await.unwrap(), 11);
    assert!(drain(&executor).await.is_err());
}

#[tokio::test]
async fn cancelled_observer_and_job_done_cannot_retire_still_dropping_output() {
    for panic in [false, true] {
        let executor = Executor::new(1);
        let files = Files::new();
        let (drop_entered, dropping) = tokio::sync::oneshot::channel();
        let (drop_release, drop_wait) = std::sync::mpsc::channel();
        let (resource, released) = files.resource(Some(DropControl {
            entered: drop_entered,
            release: drop_wait,
            panic,
        }));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release_job, wait_job) = std::sync::mpsc::channel();
        let mut request = Box::pin(executor.run(move || {
            started.send(()).unwrap();
            wait_job.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(resource)
        }));
        let completion = executor.completion();
        pending(request.as_mut()).await;
        ready.await.unwrap();
        drop(request);
        assert_eq!(completion.facts.load(Ordering::Acquire), OBSERVER_DONE);
        release_job.send(()).unwrap();
        dropping.await.unwrap();
        // Actual discarded output is being destroyed in Tokio's worker while
        // both older facts are already true. This is the three-fact window.
        assert_eq!(
            completion.facts.load(Ordering::Acquire),
            JOB_DONE | OBSERVER_DONE
        );
        assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 1);
        files.assert_locked();
        let mut finish = Box::pin(executor.ledger.finish_pending());
        pending(finish.as_mut()).await;
        drop_release.send(()).unwrap();
        assert_eq!(finish.await.is_err(), panic);
        assert!(released.load(Ordering::Acquire));
        files.assert_released();
    }
}

#[tokio::test]
async fn delivered_started_panic_never_skips_another_registered_owner() {
    let executor = Executor::new(2);
    let files = Files::new();
    let (resource, released) = files.resource(None);
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let blocked = tokio::spawn(executor.run(move || {
        started.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(resource);
        Ok(())
    }));
    ready.await.unwrap();
    assert!(
        executor
            .run(|| -> Result<(), StoreError> { panic!("other worker still owned") })
            .await
            .is_err()
    );
    assert!(executor.ledger.unconfirmed.load(Ordering::Acquire));
    assert_eq!(executor.ledger.pending.load(Ordering::Acquire), 1);
    let mut finish = Box::pin(executor.ledger.finish_pending());
    pending(finish.as_mut()).await;
    drop(finish);
    files.assert_locked();
    release.send(()).unwrap();
    blocked.await.unwrap().unwrap();
    assert!(released.load(Ordering::Acquire));
    files.assert_released();
    assert!(drain(&executor).await.unwrap_err().contains("unconfirmed"));
    assert_eq!(executor.run(|| Ok(23)).await.unwrap(), 23);
    assert!(drain(&executor).await.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn isolated_fault_executor_enrolls_unpolled_captures_until_their_real_drop() {
    struct HeldCapture {
        entered: Option<tokio::sync::oneshot::Sender<()>>,
        release: std::sync::mpsc::Receiver<()>,
    }
    impl Drop for HeldCapture {
        fn drop(&mut self) {
            self.entered.take().unwrap().send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(3)).unwrap();
        }
    }
    let executor = IsolatedTestExecutor::new();
    let (entered, dropping) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let capture = HeldCapture {
        entered: Some(entered),
        release: wait,
    };
    let request = executor.run(move || {
        drop(capture);
        Ok(())
    });
    // No poll occurred: registration is synchronous, like the daemon entry.
    assert_eq!(executor.0.ledger.pending.load(Ordering::Acquire), 1);
    let dropping_thread = std::thread::spawn(move || drop(request));
    let entered = tokio::time::timeout(Duration::from_secs(3), dropping).await;
    if !matches!(entered, Ok(Ok(()))) {
        let _ = release.send(());
        let _ = dropping_thread.join();
        panic!("unpolled capture destructor did not enter");
    }
    let mut finish = Box::pin(executor.finish_pending());
    assert!(poll_fn(|cx| Poll::Ready(finish.as_mut().poll(cx).is_pending())).await);
    assert_eq!(executor.0.ledger.pending.load(Ordering::Acquire), 1);
    release.send(()).unwrap();
    dropping_thread.join().unwrap();
    tokio::time::timeout(Duration::from_secs(3), finish)
        .await
        .expect("capture retirement must settle the private ledger")
        .unwrap();
    assert_eq!(executor.0.ledger.pending.load(Ordering::Acquire), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn durable_cancel_before_failure_is_pending_then_repeatably_visible() {
    let executor = Executor::new(1);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let observer = tokio::spawn(executor.run_durable(
        move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            Err::<(), _>(StoreError::Io(std::io::Error::from_raw_os_error(28)))
        },
        (),
    ));
    started_rx.await.unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert!(
        tokio::time::timeout(Duration::from_millis(25), executor.ledger.finish_pending())
            .await
            .is_err()
    );
    release_tx.send(()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(2), executor.ledger.finish_pending())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.contains("durable archive persistence failed"));
    assert_eq!(executor.ledger.finish_pending().await.unwrap_err(), error);
}

#[tokio::test(flavor = "current_thread")]
async fn durable_send_success_then_receiver_cancel_keeps_output_owner_until_destruction() {
    struct OutputOwner {
        entered: Option<tokio::sync::oneshot::Sender<()>>,
        release: std::sync::mpsc::Receiver<()>,
        destroyed: Arc<AtomicBool>,
    }
    impl Drop for OutputOwner {
        fn drop(&mut self) {
            let _ = self.entered.take().unwrap().send(());
            self.release.recv_timeout(Duration::from_secs(3)).unwrap();
            self.destroyed.store(true, Ordering::Release);
        }
    }
    let executor = Executor::new(1);
    let (entered, dropping) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let destroyed = Arc::new(AtomicBool::new(false));
    let observer = executor.run_durable(
        || Err::<(), _>(failure("private-payload-and-path")),
        OutputOwner {
            entered: Some(entered),
            release: wait,
            destroyed: destroyed.clone(),
        },
    );
    let completion = executor.completion();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !completion.forwarded.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!destroyed.load(Ordering::Acquire));
    assert_eq!(completion.facts.load(Ordering::Acquire) & OUTPUT_DONE, 0);
    // The ACK has been sent successfully, but its receiver is still unpolled.
    // Drop it on a separate thread so its deliberately gated destructor does
    // not stop the runtime that observes the drain's publication ordering.
    let dropper = std::thread::spawn(move || drop(observer));
    dropping.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(25), executor.ledger.finish_pending())
            .await
            .is_err()
    );
    assert_eq!(completion.facts.load(Ordering::Acquire) & OUTPUT_DONE, 0);
    release.send(()).unwrap();
    dropper.join().unwrap();
    assert!(destroyed.load(Ordering::Acquire));
    let error = executor.ledger.finish_pending().await.unwrap_err();
    assert!(error.contains("durable archive persistence failed"));
    assert!(!error.contains("private-payload-and-path"));
    assert_eq!(executor.ledger.finish_pending().await.unwrap_err(), error);
}

#[tokio::test(flavor = "current_thread")]
async fn durable_live_error_keeps_original_error_and_drain_clean() {
    let executor = Executor::new(1);
    let result = executor
        .run_durable(
            || Err::<(), _>(StoreError::Io(std::io::Error::from_raw_os_error(28))),
            (),
        )
        .await;
    assert!(matches!(result, Err(StoreError::Io(error)) if error.raw_os_error() == Some(28)));
    executor.ledger.finish_pending().await.unwrap();
    executor.ledger.finish_pending().await.unwrap();
}

#[test]
fn durable_without_runtime_refuses_without_execution_and_retires_input() {
    let executor = Executor::new(1);
    let called = Arc::new(AtomicBool::new(false));
    let worker = called.clone();
    let observer = executor.run_durable(
        move || {
            worker.store(true, Ordering::Release);
            Ok(())
        },
        (),
    );
    assert!(!called.load(Ordering::Acquire));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert!(matches!(observer.await, Err(StoreError::Io(error)) if error.to_string().contains("runtime unavailable")));
        executor.ledger.finish_pending().await.unwrap();
    });
    assert!(!called.load(Ordering::Acquire));
}

#[test]
fn durable_spawn_into_shutdown_runtime_cancels_before_execution_and_retires() {
    let executor = Executor::new(1);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = runtime.handle().clone();
    runtime.shutdown_timeout(Duration::from_millis(10));
    let called = Arc::new(AtomicBool::new(false));
    let worker = called.clone();
    let entered = handle.enter();
    let observer = executor.run_durable(
        move || {
            worker.store(true, Ordering::Release);
            Ok(())
        },
        (),
    );
    drop(entered);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert!(
            tokio::time::timeout(Duration::from_secs(1), observer)
                .await
                .unwrap()
                .is_err()
        );
        executor.ledger.finish_pending().await.unwrap();
    });
    assert!(!called.load(Ordering::Acquire));
}
