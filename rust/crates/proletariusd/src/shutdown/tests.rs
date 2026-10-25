use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

#[tokio::test]
async fn coordinator_collects_reporting_errors_without_skipping_independent_drains() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (release, released) = oneshot::channel();
    let (reader_started, reader_waiting) = oneshot::channel();
    let (qmi_done, qmi_finished) = oneshot::channel();
    let history = events.clone();
    let module = events.clone();
    let reader = events.clone();
    let qmi = events.clone();
    let archive = events.clone();
    let task = tokio::spawn(coordinate(
        Report::default(),
        async move {
            history.lock().unwrap().push("shared");
            Report::default()
        },
        async move {
            module.lock().unwrap().push("module released");
            confirmed("module", || async { Ok(vec!["history failure".into()]) }).await
        },
        async move {
            reader.lock().unwrap().push("reader waiting");
            reader_started.send(()).unwrap();
            released.await.unwrap();
            reader.lock().unwrap().push("reader released");
            Report::default()
        },
        async move {
            qmi.lock().unwrap().push("qmi released");
            qmi_done.send(()).unwrap();
            let mut report = Report::default();
            report.record("qmi report", "second failure");
            report
        },
        async move {
            archive.lock().unwrap().push("archive drained");
            Report::default()
        },
    ));
    reader_waiting.await.unwrap();
    qmi_finished.await.unwrap();
    assert!(!task.is_finished());
    assert!(!events.lock().unwrap().contains(&"archive drained"));
    release.send(()).unwrap();
    let report = task.await.unwrap();
    assert_eq!(
        report.failures,
        ["module: history failure", "qmi report: second failure"]
    );
    assert!(report.finish().is_err());
    let events = events.lock().unwrap();
    assert_eq!(events.first(), Some(&"shared"));
    assert_eq!(events.last(), Some(&"archive drained"));
}

#[tokio::test]
async fn coordinator_unknown_release_blocks_dependents_until_exact_receipt() {
    let receipt_generation = Arc::new(AtomicU64::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let close_count = Arc::new(AtomicUsize::new(0));
    let generation = receipt_generation.clone();
    let attempts = polls.clone();
    let closes = close_count.clone();
    let (independent_done, independent_finished) = oneshot::channel();
    let task = tokio::spawn(coordinate(
        Report::default(),
        async { Report::default() },
        async move {
            let report = confirmed("module generation 7", || {
                attempts.fetch_add(1, Ordering::SeqCst);
                let actual = generation.load(Ordering::SeqCst);
                async move {
                    if actual == 7 {
                        Ok(Vec::new())
                    } else {
                        Err("physical result unknown".into())
                    }
                }
            })
            .await;
            closes.fetch_add(1, Ordering::SeqCst);
            report
        },
        async move {
            independent_done.send(()).unwrap();
            Report::default()
        },
        async { Report::default() },
        async { Report::default() },
    ));
    independent_finished.await.unwrap();
    assert_eq!(close_count.load(Ordering::SeqCst), 0);
    receipt_generation.store(6, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(3), async {
        while polls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    assert_eq!(close_count.load(Ordering::SeqCst), 0);
    receipt_generation.store(7, Ordering::SeqCst);
    let report = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(close_count.load(Ordering::SeqCst), 1);
    assert_eq!(report.failures.len(), 1);
}

#[tokio::test]
async fn accept_fault_enters_coordinator_and_preserves_original_cause() {
    let mut report = Report::default();
    let activated = AtomicBool::new(true);
    assert!(
        accepted::<()>(
            &mut report,
            "public accept",
            Err(std::io::Error::other("injected"))
        )
        .is_none()
    );
    let drains = AtomicUsize::new(0);
    let report = coordinate(
        report,
        async {
            assert!(activated.load(Ordering::SeqCst));
            drains.fetch_add(1, Ordering::SeqCst);
            Report::default()
        },
        async {
            drains.fetch_add(1, Ordering::SeqCst);
            Report::default()
        },
        async {
            drains.fetch_add(1, Ordering::SeqCst);
            Report::default()
        },
        async {
            drains.fetch_add(1, Ordering::SeqCst);
            Report::default()
        },
        async {
            drains.fetch_add(1, Ordering::SeqCst);
            Report::default()
        },
    )
    .await;
    assert_eq!(drains.load(Ordering::SeqCst), 5);
    assert_eq!(report.failures, ["public accept: injected"]);
}

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn cleanup_gate_drains_admitted_archive_writer_and_rejects_tail_requests() {
    let gate = CleanupGate::default();
    let permit = gate.enter().await.unwrap();
    let closing_gate = gate.clone();
    let (closing, closing_started) = oneshot::channel();
    let finish = tokio::spawn(async move {
        closing.send(()).unwrap();
        closing_gate.close().await;
    });
    closing_started.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!finish.is_finished());
    let next_gate = gate.clone();
    let next_request = tokio::spawn(async move { next_gate.enter().await.is_some() });
    tokio::task::yield_now().await;
    assert!(!next_request.is_finished());
    drop(permit);
    finish.await.unwrap();
    assert!(!next_request.await.unwrap());
    assert!(gate.enter().await.is_none());
    // Repeated finalization cannot reopen admission.
    gate.close().await;
    assert!(gate.enter().await.is_none());
}

#[tokio::test]
async fn accept_failure_retains_owner_and_other_listener_without_busy_spin() {
    let dropped = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    let attempts = polls.clone();
    let owner_flag = DropFlag(dropped.clone());
    let (finish, finished) = oneshot::channel();
    let (input, mut output) = UnixStream::pair().unwrap();
    let next_stream = Arc::new(Mutex::new(Some(input)));
    let (other_served, other_finished) = oneshot::channel();
    let other_served = Arc::new(Mutex::new(Some(other_served)));
    let driver = tokio::spawn(drive_callbacks(
        async move {
            let _flag = owner_flag;
            finished.await.unwrap();
            42
        },
        move || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err(std::io::Error::other("injected callback accept failure")) }
        },
        move || {
            let next = next_stream.lock().unwrap().take();
            async move {
                match next {
                    Some(stream) => Ok(stream),
                    None => std::future::pending().await,
                }
            }
        },
        |_| async {},
        move |mut stream| {
            let done = other_served.lock().unwrap().take().unwrap();
            async move {
                assert_eq!(stream.read_u8().await.unwrap(), 7);
                stream.write_all(&[8]).await.unwrap();
                done.send(()).unwrap();
            }
        },
        4,
    ));
    output.write_all(&[7]).await.unwrap();
    assert_eq!(output.read_u8().await.unwrap(), 8);
    other_finished.await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(!dropped.load(Ordering::SeqCst));
    assert!(!driver.is_finished());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    finish.send(()).unwrap();
    let (value, report) = driver.await.unwrap();
    assert_eq!(value, 42);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(
        report.failures,
        ["cleanup callback accept: injected callback accept failure"]
    );
}

#[tokio::test]
async fn unrepeatable_unknown_close_never_unblocks_archive_or_other_dependents() {
    let (drained, observed) = oneshot::channel();
    let after_close = Arc::new(AtomicBool::new(false));
    let dependent = after_close.clone();
    let task = tokio::spawn(coordinate(
        Report::default(),
        async { Report::default() },
        async move {
            let report = once("physical close", Err("unknown".into())).await;
            dependent.store(true, Ordering::SeqCst);
            report
        },
        async move {
            drained.send(()).unwrap();
            Report::default()
        },
        async { Report::default() },
        async { panic!("archive cannot run before all producer receipts") },
    ));
    observed.await.unwrap();
    assert!(!task.is_finished());
    assert!(!after_close.load(Ordering::SeqCst));
    // Test disposal only; production never times out or aborts this coordinator.
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
