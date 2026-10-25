use super::*;
use std::future::poll_fn;
use std::sync::atomic::AtomicBool;
use std::task::{Wake, Waker};
use std::time::Duration;

fn owners(n: usize) -> CoordinatorOwners {
    CoordinatorOwners::new(NonZeroUsize::new(n).unwrap()).unwrap()
}
fn count(owners: &CoordinatorOwners) -> usize {
    owners.0.word.load(Ordering::Acquire) & COUNT
}
async fn pending<F: Future>(mut future: Pin<&mut F>) {
    assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}
async fn until(mut f: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !f() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
struct Noop;
impl Wake for Noop {
    fn wake(self: Arc<Self>) {}
}
fn poll_once<F: Future + Unpin>(f: &mut F) -> Poll<F::Output> {
    let waker = Waker::from(Arc::new(Noop));
    Pin::new(f).poll(&mut Context::from_waker(&waker))
}
struct Last(Arc<AtomicUsize>);
impl Drop for Last {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct RuntimeCapture {
    file: Option<std::fs::File>,
    gate: Option<(
        tokio::sync::oneshot::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
    panic_on_drop: bool,
    last: Last,
}
impl Drop for RuntimeCapture {
    fn drop(&mut self) {
        if let Some((started, wait)) = self.gate.take() {
            let _ = started.send(());
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(!self.panic_on_drop, "controlled Runtime destructor unwind");
    }
}
fn capture(live: &Arc<AtomicUsize>) -> RuntimeCapture {
    live.fetch_add(1, Ordering::SeqCst);
    RuntimeCapture {
        file: None,
        gate: None,
        panic_on_drop: false,
        last: Last(live.clone()),
    }
}
struct HeldFuture {
    runtime: RuntimeCapture,
    ready: Option<Finish>,
}
impl Future for HeldFuture {
    type Output = Finish;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Finish> {
        self.get_mut().ready.map_or(Poll::Pending, Poll::Ready)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_reservation_bounds_capture_before_any_poll_and_drains_unused_tickets() {
    let owners = owners(2);
    let live = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    let mut refused = 0;
    for _ in 0..256 {
        match owners.try_reserve() {
            Ok(ticket) => tasks.push(ticket.into_runner(HeldFuture {
                runtime: capture(&live),
                ready: None,
            })),
            Err(Refusal::Full) => refused += 1,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert_eq!(refused, 254);
    assert_eq!(count(&owners), 2);
    assert_eq!(live.load(Ordering::SeqCst), 2);
    owners.begin_shutdown();
    let mut finish = Box::pin(owners.finish_pending());
    pending(finish.as_mut()).await;
    drop(tasks);
    assert_eq!(live.load(Ordering::SeqCst), 0);
    finish.await.unwrap();
    println!(
        "OWNER_RESERVATION attempts=256 captures=2 refused_before_factory=254 all_enrolled_before_poll=true"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn close_and_reserve_are_linearized_in_one_word() {
    let owners = owners(7);
    let gate = Arc::new(std::sync::Barrier::new(66));
    let (send, receive) = std::sync::mpsc::channel();
    let mut threads = Vec::new();
    for _ in 0..64 {
        let (owners, gate, send) = (owners.clone(), gate.clone(), send.clone());
        threads.push(std::thread::spawn(move || {
            gate.wait();
            send.send(owners.try_reserve()).unwrap();
        }));
    }
    let closer = owners.clone();
    let closer_gate = gate.clone();
    threads.push(std::thread::spawn(move || {
        closer_gate.wait();
        closer.begin_shutdown();
    }));
    gate.wait();
    drop(send);
    let reservations: Vec<_> = receive.iter().collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let accepted = reservations.iter().filter(|r| r.is_ok()).count();
    assert!(accepted <= 7);
    assert_eq!(count(&owners), accepted);
    for _ in 0..64 {
        assert_eq!(owners.try_reserve().err(), Some(Refusal::Closed));
    }
    drop(reservations);
    owners.finish_pending().await.unwrap();
    assert_eq!(count(&owners), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn unpolled_and_started_runtime_drop_hold_real_file_and_owner_until_last_field() {
    for started in [false, true] {
        for panic in [false, true] {
            let owners = owners(1);
            let live = Arc::new(AtomicUsize::new(0));
            let ticket = owners.try_reserve().unwrap();
            let mut runtime = capture(&live);
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
                "runtime-{}-{started}-{panic}.lock",
                std::process::id()
            ));
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .unwrap();
            file.lock().unwrap();
            runtime.file = Some(file);
            let (started_drop, ready_drop) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            runtime.gate = Some((started_drop, wait));
            runtime.panic_on_drop = panic;
            let mut task = ticket.into_runner(HeldFuture {
                runtime,
                ready: None,
            });
            if started {
                assert!(poll_once(&mut task).is_pending());
            }
            owners.begin_shutdown();
            let dropping = std::thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(task)))
            });
            ready_drop.await.unwrap();
            assert_eq!(count(&owners), 1);
            assert_eq!(live.load(Ordering::SeqCst), 1);
            let check = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            assert!(check.try_lock().is_err());
            let mut finish = Box::pin(owners.finish_pending());
            pending(finish.as_mut()).await;
            release.send(()).unwrap();
            assert_eq!(dropping.join().unwrap().is_err(), panic);
            assert_eq!(live.load(Ordering::SeqCst), 0);
            check.lock().unwrap();
            check.unlock().unwrap();
            std::fs::remove_file(path).unwrap();
            let result = finish.await;
            assert_eq!(result.is_err(), started || panic);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn clean_ready_report_cannot_release_before_actual_wrapper_destruction() {
    let owners = owners(1);
    let live = Arc::new(AtomicUsize::new(0));
    let ticket = owners.try_reserve().unwrap();
    let mut task = ticket.into_runner(HeldFuture {
        runtime: capture(&live),
        ready: Some(Finish::Clean),
    });
    assert_eq!(poll_once(&mut task), Poll::Ready(Finish::Clean));
    assert_eq!(count(&owners), 1);
    assert_eq!(live.load(Ordering::SeqCst), 1);
    owners.begin_shutdown();
    let mut finish = Box::pin(owners.finish_pending());
    pending(finish.as_mut()).await;
    drop(task);
    finish.await.unwrap();
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn http_observer_abort_and_empty_io_do_not_cancel_the_actual_coordinator() {
    let owners = owners(1);
    let live = Arc::new(AtomicUsize::new(0));
    let ticket = owners.try_reserve().unwrap();
    let runtime = capture(&live);
    let (response, receive) = tokio::sync::oneshot::channel();
    let (release, hold) = tokio::sync::oneshot::channel();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let runner = tokio::spawn(ticket.into_runner(async move {
        let _runtime = runtime;
        let _ = response.send("same-operation-id");
        entered.send(()).unwrap();
        hold.await.unwrap();
        Finish::Clean
    }));
    let observer = tokio::spawn(async move {
        let _ = receive.await;
        std::future::pending::<()>().await;
    });
    ready.await.unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    owners.begin_shutdown();
    assert_eq!(count(&owners), 1);
    let mut finish = Box::pin(owners.finish_pending());
    pending(finish.as_mut()).await;
    let mut ticks = 0;
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(1)).await;
        ticks += 1;
    }
    assert_eq!(live.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    assert_eq!(runner.await.unwrap(), Finish::Clean);
    finish.await.unwrap();
    println!(
        "OWNER_WITHOUT_IO archive_jobs_modeled=0 http_observer_aborted=true owner_retained=true heartbeat_ticks={ticks}"
    );
}

#[tokio::test]
async fn aborting_true_runner_is_uncertain_even_when_all_resources_destroyed() {
    let owners = owners(1);
    let live = Arc::new(AtomicUsize::new(0));
    let runtime = capture(&live);
    let ticket = owners.try_reserve().unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let runner = tokio::spawn(ticket.into_runner(async move {
        let _runtime = runtime;
        entered.send(()).unwrap();
        std::future::pending::<()>().await;
        Finish::Clean
    }));
    ready.await.unwrap();
    owners.begin_shutdown();
    runner.abort();
    assert!(runner.await.unwrap_err().is_cancelled());
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert!(
        matches!(owners.finish_pending().await,Err(DrainError::Unconfirmed(bits)) if bits&CANCELLED!=0)
    );
}

#[tokio::test]
async fn submitted_then_durable_finish_failure_keeps_original_id_and_uncertain_drain() {
    // Business seam model, not actual Service/journal/radio integration.
    let owners = owners(1);
    let ticket = owners.try_reserve().unwrap();
    let dispatches = Arc::new(AtomicUsize::new(0));
    let attempts = dispatches.clone();
    let runner = tokio::spawn(ticket.into_runner(async move {
        let original_id = "operation-1";
        attempts.fetch_add(1, Ordering::SeqCst);
        assert_eq!(original_id, "operation-1");
        Finish::Uncertain
    }));
    assert_eq!(runner.await.unwrap(), Finish::Uncertain);
    owners.begin_shutdown();
    assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    assert!(
        matches!(owners.finish_pending().await,Err(DrainError::Unconfirmed(bits)) if bits&REPORTED_UNCERTAIN!=0)
    );
    assert_eq!(owners.try_reserve().err(), Some(Refusal::Closed));
    assert_eq!(dispatches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn poll_panic_and_clean_report_destructor_panic_are_not_clean() {
    let owners = owners(2);
    let live = Arc::new(AtomicUsize::new(0));
    let runtime = capture(&live);
    let ticket = owners.try_reserve().unwrap();
    let runner = tokio::spawn(ticket.into_runner(async move {
        let _runtime = runtime;
        panic!("controlled coordinator poll panic");
        #[allow(unreachable_code)]
        Finish::Clean
    }));
    assert!(runner.await.unwrap_err().is_panic());
    assert_eq!(live.load(Ordering::SeqCst), 0);
    let ticket = owners.try_reserve().unwrap();
    let mut runtime = capture(&live);
    runtime.panic_on_drop = true;
    let mut task = ticket.into_runner(HeldFuture {
        runtime,
        ready: Some(Finish::Clean),
    });
    assert_eq!(poll_once(&mut task), Poll::Ready(Finish::Clean));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(task))).is_err());
    owners.begin_shutdown();
    assert_eq!(count(&owners), 0);
    assert!(
        matches!(owners.finish_pending().await,Err(DrainError::Unconfirmed(bits)) if bits&DESTRUCTOR_UNWIND!=0)
    );
}

#[tokio::test]
async fn shutdown_requires_close_and_counter_faults_remain_sticky() {
    let owners = owners(1);
    assert_eq!(
        owners.finish_pending().await,
        Err(DrainError::AdmissionOpen)
    );
    // Isolated inconsistent counter injection; no real owner is forged/released.
    owners.0.retire(0);
    assert_eq!(count(&owners), 0);
    assert_eq!(owners.try_reserve().err(), Some(Refusal::Unavailable));
    owners.begin_shutdown();
    assert!(
        matches!(owners.finish_pending().await,Err(DrainError::Unconfirmed(bits)) if bits&ACCOUNTING_UNCERTAIN!=0)
    );
    assert!(CoordinatorOwners::new(NonZeroUsize::new(usize::MAX).unwrap()).is_err());
}

#[tokio::test]
async fn bounded_metrics_expose_pressure_without_retaining_request_history() {
    let owners = owners(2);
    let a = owners.try_reserve().unwrap();
    let b = owners.try_reserve().unwrap();
    assert_eq!(owners.try_reserve().err(), Some(Refusal::Full));
    owners.begin_shutdown();
    assert_eq!(owners.try_reserve().err(), Some(Refusal::Closed));
    let s = owners.snapshot();
    assert_eq!(
        (
            s.pending,
            s.maximum,
            s.peak,
            s.accepted,
            s.full_refusals,
            s.closed_refusals
        ),
        (2, 2, 2, 2, 1, 1)
    );
    assert!(s.closed);
    drop(a);
    drop(b);
    owners.finish_pending().await.unwrap();
    assert_eq!(owners.snapshot().pending, 0);
}
