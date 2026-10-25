//! Bounds submitted blocking work without releasing running work on cancellation.
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::{AbortHandle, JoinError};

#[derive(Clone)]
pub(crate) struct BlockingPool {
    slots: Arc<Semaphore>,
}

struct AbortQueuedOnDrop(AbortHandle);

impl Drop for AbortQueuedOnDrop {
    fn drop(&mut self) {
        // Tokio can cancel a queued blocking task, but never interrupts a task
        // already executing. Its permit therefore stays with the actual worker.
        self.0.abort();
    }
}

impl BlockingPool {
    pub(crate) fn new(concurrency: usize) -> Self {
        assert!(concurrency > 0);
        Self {
            slots: Arc::new(Semaphore::new(concurrency)),
        }
    }

    pub(crate) async fn run<F, T>(&self, operation: F) -> Result<T, JoinError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        // Do not consume a blocking thread while waiting for capacity. Dropping
        // this future removes a waiting operation before it can be submitted.
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("private blocking pool is never closed");
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        });
        let _guard = AbortQueuedOnDrop(task.abort_handle());
        task.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{Future, poll_fn};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    #[tokio::test]
    async fn cancellation_keeps_capacity_until_running_worker_returns() {
        let pool = BlockingPool::new(1);
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let first_pool = pool.clone();
        let first = tokio::spawn(async move {
            first_pool
                .run(move || {
                    started.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(3)).unwrap();
                })
                .await
        });
        ready.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(pool.slots.available_permits(), 0);

        let called = Arc::new(AtomicBool::new(false));
        let worker_called = called.clone();
        let mut next = Box::pin(pool.run(move || worker_called.store(true, Ordering::SeqCst)));
        assert!(poll_fn(|cx| Poll::Ready(next.as_mut().poll(cx).is_pending())).await);
        assert!(!called.load(Ordering::SeqCst));
        release.send(()).unwrap();
        next.await.unwrap();
        assert!(called.load(Ordering::SeqCst));
        assert_eq!(pool.slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn cancelling_capacity_wait_never_executes_operation() {
        let pool = BlockingPool::new(1);
        let occupied = pool.slots.clone().acquire_owned().await.unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let worker_called = called.clone();
        let mut job = Box::pin(pool.run(move || worker_called.store(true, Ordering::SeqCst)));
        assert!(poll_fn(|cx| Poll::Ready(job.as_mut().poll(cx).is_pending())).await);
        drop(job);
        drop(occupied);
        pool.run(|| ()).await.unwrap();
        assert!(!called.load(Ordering::SeqCst));
    }

    #[test]
    fn cancelling_tokio_queued_worker_never_executes_operation() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release, wait) = std::sync::mpsc::channel();
            let (started, ready) = tokio::sync::oneshot::channel();
            let occupied = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
            });
            ready.await.unwrap();
            let pool = BlockingPool::new(1);
            let called = Arc::new(AtomicBool::new(false));
            let worker_called = called.clone();
            let mut job = Box::pin(pool.run(move || worker_called.store(true, Ordering::SeqCst)));
            assert!(poll_fn(|cx| Poll::Ready(job.as_mut().poll(cx).is_pending())).await);
            drop(job);
            release.send(()).unwrap();
            occupied.await.unwrap();
            pool.run(|| ()).await.unwrap();
            assert!(!called.load(Ordering::SeqCst));
            assert_eq!(pool.slots.available_permits(), 1);
        });
    }

    #[tokio::test]
    async fn panicking_worker_releases_its_capacity() {
        let pool = BlockingPool::new(1);
        assert!(
            pool.run(|| panic!("fixture worker failure"))
                .await
                .unwrap_err()
                .is_panic()
        );
        assert_eq!(pool.run(|| 7).await.unwrap(), 7);
    }
}
