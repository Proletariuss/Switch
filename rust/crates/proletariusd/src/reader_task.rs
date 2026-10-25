//! Cancellation-safe blocking reader work shared by native authentication APIs.
use std::time::Duration;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ReaderTaskError {
    Timeout,
    WorkerFailed,
    Busy,
}
impl std::fmt::Display for ReaderTaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Timeout => "reader worker timed out",
            Self::WorkerFailed => "reader worker failed",
            Self::Busy => "reader authority is busy or no longer current",
        })
    }
}

struct AbortQueuedOnDrop(tokio::task::AbortHandle);
impl Drop for AbortQueuedOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The lease authority is checked before waiting for blocking-pool capacity.
/// After dispatch only the actual blocking closure may release this guard;
/// cancellation of its HTTP/session waiter cannot release physical ownership.
pub(crate) async fn run_in_pool<F, T, P>(
    pool: &crate::blocking_pool::BlockingPool,
    physical: Option<P>,
    operation: F,
) -> Result<T, ReaderTaskError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
    P: Send + 'static,
{
    pool.run(move || {
        let _physical = physical;
        operation()
    })
    .await
    .map_err(|_| ReaderTaskError::WorkerFailed)
}

/// Cancellation prevents a queued operation from starting. A worker already
/// inside a synchronous hardware call is NOT killed: its transaction/lock
/// must remain owned until that call actually returns.
pub(crate) async fn run<F, T>(
    reader: &str,
    operation: F,
    deadline: Duration,
) -> Result<T, ReaderTaskError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_with_gate(
        crate::reader_reservation::gate(),
        reader,
        operation,
        deadline,
    )
    .await
}

pub(crate) async fn run_with_gate<F, T>(
    gate: std::sync::Arc<crate::reader_reservation::Gate>,
    reader: &str,
    operation: F,
    deadline: Duration,
) -> Result<T, ReaderTaskError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = gate.admit(reader).map_err(|_| ReaderTaskError::Busy)?;
    let mut task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    });
    let _guard = AbortQueuedOnDrop(task.abort_handle());
    tokio::time::timeout(deadline, &mut task)
        .await
        .map_err(|_| ReaderTaskError::Timeout)?
        .map_err(|_| ReaderTaskError::WorkerFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const BLE: &str = "BLE:73B7446E-E6CE-C90C-C844-485389FE8C1D";

    #[tokio::test]
    async fn native_capacity_wait_holds_guard_and_cancellation_never_submits() {
        use std::{
            future::{Future, poll_fn},
            sync::atomic::{AtomicBool, Ordering},
            task::Poll,
        };
        let gate = Arc::new(crate::reader_reservation::Gate::default());
        let pool = crate::blocking_pool::BlockingPool::new(1);
        let occupied_pool = pool.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let occupied = tokio::spawn(async move {
            occupied_pool
                .run(move || {
                    started.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(3)).unwrap();
                })
                .await
                .unwrap();
        });
        ready.await.unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let worker_called = called.clone();
        let mut queued = Box::pin(run_in_pool(
            &pool,
            Some(gate.track_ble_io(BLE).unwrap()),
            move || {
                worker_called.store(true, Ordering::SeqCst);
            },
        ));
        assert!(poll_fn(|cx| Poll::Ready(queued.as_mut().poll(cx).is_pending())).await);
        gate.test_confirm_go_exit("new-producer");
        assert!(
            gate.admit(BLE).is_err(),
            "queued accepted work must survive producer exit"
        );
        drop(queued);
        assert!(
            gate.admit(BLE).is_ok(),
            "unsubmitted cancelled work may release"
        );
        release.send(()).unwrap();
        occupied.await.unwrap();
        pool.run(|| ()).await.unwrap();
        assert!(!called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn native_running_worker_outlives_its_waiter_and_go_producer() {
        let gate = Arc::new(crate::reader_reservation::Gate::default());
        let pool = crate::blocking_pool::BlockingPool::new(1);
        let worker_pool = pool.clone();
        let physical = gate.track_ble_io(BLE).unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            run_in_pool(&worker_pool, Some(physical), move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
            })
            .await
        });
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        gate.test_confirm_go_exit("new-producer");
        assert!(gate.admit(BLE).is_err());
        release.send(()).unwrap();
        pool.run(|| ()).await.unwrap();
        assert!(gate.admit(BLE).is_ok());
    }

    #[test]
    fn dropping_waiter_cancels_queued_physical_work() {
        use std::{
            future::{Future, poll_fn},
            sync::atomic::{AtomicBool, Ordering},
            task::Poll,
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release, wait) = std::sync::mpsc::channel();
            let (started, start) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                wait.recv().unwrap();
            });
            start.await.unwrap();
            let called = Arc::new(AtomicBool::new(false));
            let worker_called = called.clone();
            let mut future = Box::pin(run(
                "test-reader",
                move || worker_called.store(true, Ordering::SeqCst),
                Duration::from_secs(2),
            ));
            let queued = poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await;
            drop(future);
            release.send(()).unwrap();
            blocker.await.unwrap();
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(queued);
            assert!(!called.load(Ordering::SeqCst));
        });
    }
    #[tokio::test]
    async fn cancellation_and_timeout_keep_running_worker_lock_until_return() {
        for cancel in [false, true] {
            let lock = Arc::new(Mutex::new(()));
            let worker_lock = lock.clone();
            let (started, start) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let (done, finished) = tokio::sync::oneshot::channel();
            let job = tokio::spawn(run(
                "test-reader",
                move || {
                    let guard = worker_lock.lock().unwrap();
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                    drop(guard);
                    done.send(()).unwrap();
                },
                Duration::from_millis(100),
            ));
            start.await.unwrap();
            if cancel {
                job.abort();
                assert!(job.await.unwrap_err().is_cancelled());
            } else {
                assert_eq!(job.await.unwrap(), Err(ReaderTaskError::Timeout));
            }
            let held = lock.try_lock().is_err();
            release.send(()).unwrap();
            finished.await.unwrap();
            assert!(held, "waiter exit must not release hardware ownership");
            assert!(lock.try_lock().is_ok());
        }
    }
}
