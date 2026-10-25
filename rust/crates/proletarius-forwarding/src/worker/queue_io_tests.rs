use super::*;
use crate::{StateKey, StateLimits};
use std::{
    future::poll_fn,
    path::PathBuf,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    task::Poll,
    time::Duration,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/tmp/provider-disk-owner-tests");
        std::fs::create_dir_all(&base).unwrap();
        let root = base.join(format!(
            "{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn open(&self) -> QueueStore {
        QueueStore::open(
            self.0.join("queue.enc"),
            StateKey::from_slice(&[31; 32]).unwrap(),
            StateLimits::default(),
        )
        .unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn pending<F: Future>(future: &mut Pin<Box<F>>) {
    assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}

#[tokio::test(flavor = "current_thread")]
async fn unpolled_and_lock_wait_cancellation_drop_preparation_before_completion() {
    let root = Root::new();
    let owner = Arc::new(Owner::default());
    let queue = Arc::new(Mutex::new(root.open()));
    let request = owner.run(queue.clone(), |_| Ok(()));
    assert_eq!(owner.state().pending, 1);
    drop(request);
    owner.finish().await.unwrap();
    assert_eq!(Arc::strong_count(&queue), 1);
    let guard = queue.lock().await;
    let mut request = Box::pin(owner.run(queue.clone(), |_| Ok(())));
    pending(&mut request).await;
    drop(request);
    owner.finish().await.unwrap();
    assert_eq!(Arc::strong_count(&queue), 1);
    drop(guard);
    drop(queue);
    drop(root.open());
}

#[tokio::test(flavor = "current_thread")]
async fn started_cancel_keeps_real_guard_and_slot_while_timer_and_stop_progress() {
    let root = Root::new();
    let owner = Arc::new(Owner::default());
    let queue = Arc::new(Mutex::new(root.open()));
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let mut request = Box::pin(owner.run(queue.clone(), move |_| {
        entered.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        Ok(())
    }));
    pending(&mut request).await;
    ready.await.unwrap();
    drop(request);
    assert_eq!(owner.slots.available_permits(), 0);
    assert!(queue.try_lock().is_err());
    owner.close();
    let mut drain = Box::pin(owner.finish());
    pending(&mut drain).await;
    // The same single-thread runtime must dispatch stop/watch and a real timer
    // while a synchronous command still owns the queue on its blocking thread.
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_millis(500), async {
        stopped.changed().await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    })
    .await
    .unwrap();
    drop(drain);
    assert_eq!(owner.state().pending, 1);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), owner.finish())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.slots.available_permits(), 1);
    assert!(queue.try_lock().is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn output_owns_real_begin_and_cleanup_registration_precedes_disk_drain() {
    use super::super::{AttemptReservation, ReleaseLedger};
    use crate::{DestinationBinding, EnqueueOptions, ForwardingChannel, ForwardingMessage};
    let root = Root::new();
    let key = StateKey::from_slice(&[31; 32]).unwrap();
    let telegram = DestinationBinding::telegram(&key, "synthetic", "synthetic");
    let bark = DestinationBinding::bark(&key, "https://synthetic.invalid", "synthetic").unwrap();
    let mut store = root.open();
    let message = ForwardingMessage::new(
        "begin-output",
        "in",
        "sender",
        "body",
        "2026-01-01T00:00:00Z",
        "reader",
    )
    .unwrap();
    store
        .enqueue_and_persist(
            &[message],
            "2026-01-01T00:00:01Z",
            &EnqueueOptions::new(true, &telegram, false, &bark),
        )
        .unwrap();
    let id = store.inspect().unwrap().tasks[0].id.clone();
    let queue = Arc::new(Mutex::new(store));
    let owner = Arc::new(Owner::default());
    let releases = Arc::new(ReleaseLedger::default());
    let completion = owner.register().unwrap();
    let observer = Observer {
        completion: completion.clone(),
        live: false,
    };
    drop(observer);
    let captured = queue.clone();
    let cleanup = releases.clone();
    let task = id.clone();
    let command = Command {
        admission: None,
        guard: Some(queue.clone().lock_owned().await),
        operation: Some(move |queue: &mut QueueStore| {
            let token = queue.begin_destination_attempt(
                &task,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-01T00:00:02Z",
            )?;
            Ok(AttemptReservation::new(
                captured,
                &task,
                ForwardingChannel::Telegram,
                token,
                cleanup,
            ))
        }),
        permit: Some(owner.slots.clone().acquire_owned().await.unwrap()),
        witness: JobWitness {
            completion,
            started: false,
            output_created: false,
        },
    };
    let output = std::thread::spawn(move || command.execute())
        .join()
        .unwrap();
    assert_eq!(owner.state().pending, 1);
    let guard = queue.lock().await;
    assert!(guard.inspect().unwrap().tasks[0].telegram.in_flight);
    drop(output); // ready BeginAck never transferred to its observer
    assert_eq!(releases.state().pending, 1);
    owner.close();
    owner.finish().await.unwrap();
    let mut finish = Box::pin(releases.finish());
    pending(&mut finish).await;
    drop(guard);
    finish.await.unwrap();
    let summary = queue.lock().await.inspect().unwrap();
    assert!(!summary.tasks[0].telegram.in_flight);
    assert!(summary.tasks[0].telegram.result_unknown);
    drop(queue);
    let summary = root.open().inspect().unwrap();
    assert!(summary.tasks[0].telegram.result_unknown);
}

#[tokio::test(flavor = "current_thread")]
async fn registration_capacity_is_64_and_refusal_never_mutates() {
    let root = Root::new();
    let owner = Arc::new(Owner::default());
    let queue = Arc::new(Mutex::new(root.open()));
    let called = Arc::new(AtomicUsize::new(0));
    let mut requests = Vec::new();
    for _ in 0..CAPACITY {
        requests.push(owner.run(queue.clone(), |_| Ok(())));
    }
    let count = called.clone();
    assert_eq!(
        owner
            .run(queue.clone(), move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await,
        Err(WorkerError::QueueIoBusy)
    );
    assert_eq!(owner.state().pending, CAPACITY);
    drop(requests);
    owner.close();
    owner.finish().await.unwrap();
    assert_eq!(called.load(Ordering::SeqCst), 0);
    assert_eq!(
        owner.run(queue.clone(), |_| Ok(())).await,
        Err(WorkerError::Stopped)
    );
    assert_eq!(Arc::strong_count(&queue), 1);
}

#[test]
fn tokio_queued_abort_retires_only_when_actual_command_is_destroyed() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = Root::new();
        let owner = Arc::new(Owner::default());
        let queue = Arc::new(Mutex::new(root.open()));
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let occupied = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready.await.unwrap();
        let called = Arc::new(AtomicUsize::new(0));
        let count = called.clone();
        let mut request = Box::pin(owner.run(queue.clone(), move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        pending(&mut request).await;
        drop(request);
        owner.close();
        if owner.state().pending == 0 {
            assert!(queue.try_lock().is_ok());
        }
        release.send(()).unwrap();
        occupied.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), owner.finish())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(called.load(Ordering::SeqCst), 0);
        assert!(queue.try_lock().is_ok());
    });
}

#[tokio::test(flavor = "current_thread")]
async fn output_destructor_panic_is_sticky_and_still_retires_real_resources() {
    struct BadOutput;
    impl Drop for BadOutput {
        fn drop(&mut self) {
            panic!("synthetic output destructor");
        }
    }
    let root = Root::new();
    let owner = Arc::new(Owner::default());
    let completion = owner.register().unwrap();
    drop(Observer {
        completion: completion.clone(),
        live: false,
    });
    let command = Command {
        admission: None,
        guard: Some(Arc::new(Mutex::new(root.open())).lock_owned().await),
        operation: Some(|_: &mut QueueStore| Ok(BadOutput)),
        permit: Some(owner.slots.clone().acquire_owned().await.unwrap()),
        witness: JobWitness {
            completion,
            started: false,
            output_created: false,
        },
    };
    let output = std::thread::spawn(move || command.execute())
        .join()
        .unwrap();
    assert!(std::thread::spawn(move || drop(output)).join().is_err());
    assert_eq!(owner.finish().await, Err(WorkerError::QueueIoUnconfirmed));
    assert_eq!(owner.state().pending, 0);
    drop(root.open());
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_executor_capacity_wait_destroys_preparation_without_late_operation() {
    let root = Root::new();
    let owner = Arc::new(Owner::default());
    let occupied = owner.slots.clone().acquire_owned().await.unwrap();
    let queue = Arc::new(Mutex::new(root.open()));
    let called = Arc::new(AtomicUsize::new(0));
    let count = called.clone();
    let mut request = Box::pin(owner.run(queue.clone(), move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    pending(&mut request).await;
    assert!(queue.try_lock().is_err());
    drop(request);
    owner.close();
    owner.finish().await.unwrap();
    assert_eq!(Arc::strong_count(&queue), 1);
    assert!(queue.try_lock().is_ok());
    drop(occupied);
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(called.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn unconsumed_error_output_is_sticky_but_live_error_is_observed() {
    let root = Root::new();
    let queue = Arc::new(Mutex::new(root.open()));
    let owner = Arc::new(Owner::default());
    let error = WorkerError::Queue(crate::QueueMutationError::StorageIo);
    assert_eq!(
        owner.run(queue.clone(), move |_| Err::<(), _>(error)).await,
        Err(error)
    );
    owner.finish().await.unwrap();
    let completion = owner.register().unwrap();
    drop(Observer {
        completion: completion.clone(),
        live: false,
    });
    let command = Command {
        admission: None,
        guard: Some(queue.clone().lock_owned().await),
        operation: Some(move |_: &mut QueueStore| Err::<(), _>(error)),
        permit: Some(owner.slots.clone().acquire_owned().await.unwrap()),
        witness: JobWitness {
            completion,
            started: false,
            output_created: false,
        },
    };
    let output = std::thread::spawn(move || command.execute())
        .join()
        .unwrap();
    assert_eq!(owner.state().pending, 1);
    drop(output);
    assert_eq!(owner.state().pending, 0);
    assert_eq!(owner.finish().await, Err(WorkerError::QueueIoUnconfirmed));
    assert_eq!(
        owner.run(queue.clone(), |_| Ok(())).await,
        Err(WorkerError::QueueIoUnconfirmed)
    );
    // The owner's missing acknowledgement does not rewrite a healthy store.
    queue.lock().await.inspect().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn only_owner_proven_closed_before_invocation_stopped_can_drain_without_observer() {
    for before_invocation in [true, false] {
        let root = Root::new();
        let owner = Arc::new(Owner::default());
        let admission = Arc::new(ReleaseLedger::default());
        admission.close();
        let called = Arc::new(AtomicUsize::new(0));
        let count = called.clone();
        let completion = owner.register().unwrap();
        drop(Observer {
            completion: completion.clone(),
            live: false,
        });
        let command = Command {
            admission: before_invocation.then_some(admission),
            guard: Some(Arc::new(Mutex::new(root.open())).lock_owned().await),
            operation: Some(move |_: &mut QueueStore| {
                count.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(WorkerError::Stopped)
            }),
            permit: Some(owner.slots.clone().acquire_owned().await.unwrap()),
            witness: JobWitness {
                completion,
                started: false,
                output_created: false,
            },
        };
        let output = std::thread::spawn(move || command.execute())
            .join()
            .unwrap();
        assert_eq!(
            called.load(Ordering::SeqCst),
            usize::from(!before_invocation)
        );
        drop(output);
        assert_eq!(
            owner.finish().await,
            if before_invocation {
                Ok(())
            } else {
                Err(WorkerError::QueueIoUnconfirmed)
            }
        );
        drop(root.open());
    }
}
