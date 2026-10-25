use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

async fn pending(owner: &Owner) {
    assert!(
        tokio::time::timeout(Duration::from_millis(20), owner.finish_pending())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn closed_bounded_admission_counts_task_tree_through_unconsumed_output() {
    let owner = Owner::default();
    let job = owner.admit().unwrap();
    let mut saturated = Vec::new();
    for _ in 1..LIMIT {
        saturated.push(owner.admit().unwrap());
    }
    assert_eq!(owner.admit().err(), Some("device_sms_owner_busy"));
    let (go, wait) = tokio::sync::oneshot::channel();
    let child = job.spawn(async move {
        wait.await.unwrap();
        42
    });
    drop(job);
    drop(saturated);
    owner.begin_shutdown();
    assert_eq!(owner.admit().err(), Some("backend_stopping"));
    pending(&owner).await;
    go.send(()).unwrap();
    let output = child.await.unwrap();
    pending(&owner).await;
    assert_eq!(output.into_inner(), 42);
    owner.finish_pending().await.unwrap();
    assert_eq!(owner.snapshot(), (0, true, false));
}

#[tokio::test]
async fn admitted_child_can_finalize_after_close_and_cancelled_stop_does_not_release() {
    let owner = Owner::default();
    let job = owner.admit().unwrap();
    owner.begin_shutdown();
    let (go, wait) = tokio::sync::oneshot::channel();
    let child = job.spawn(async move {
        wait.await.unwrap();
    });
    drop(job);
    let observer = owner.clone();
    let stopping = tokio::spawn(async move { observer.finish_pending().await });
    tokio::task::yield_now().await;
    stopping.abort();
    let _ = stopping.await;
    pending(&owner).await;
    go.send(()).unwrap();
    drop(child.await.unwrap());
    owner.finish_pending().await.unwrap();
}

#[tokio::test]
async fn panic_or_abort_before_first_poll_remains_unconfirmed_without_reopening() {
    for panic in [false, true] {
        let owner = Owner::default();
        let job = owner.admit().unwrap();
        let task = job.spawn(async move {
            if panic {
                panic!("controlled SMS owner panic");
            }
            std::future::pending::<()>().await;
        });
        drop(job);
        if !panic {
            task.abort();
        }
        assert!(task.await.is_err());
        owner.begin_shutdown();
        assert!(
            owner
                .finish_pending()
                .await
                .unwrap_err()
                .contains("unconfirmed")
        );
        assert_eq!(owner.snapshot(), (0, true, true));
        assert!(owner.finish_pending().await.is_err());
    }
}

struct Resource(Arc<AtomicBool>);
impl Drop for Resource {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn output_and_capture_resources_retire_before_owner_receipt() {
    let owner = Owner::default();
    let job = owner.admit().unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let resource = Resource(dropped.clone());
    let task = job.spawn(async move { resource });
    drop(job);
    owner.begin_shutdown();
    let output = task.await.unwrap();
    assert!(!dropped.load(Ordering::SeqCst));
    pending(&owner).await;
    drop(output);
    owner.finish_pending().await.unwrap();
    assert!(dropped.load(Ordering::SeqCst));
}
struct Panics;
impl Drop for Panics {
    fn drop(&mut self) {
        panic!("controlled output destructor panic");
    }
}
#[tokio::test]
async fn panicking_output_destructor_pins_uncertainty() {
    let owner = Owner::default();
    let job = owner.admit().unwrap();
    let task = job.spawn(async { Panics });
    drop(job);
    let output = task.await.unwrap();
    assert!(tokio::spawn(async move { drop(output) }).await.is_err());
    owner.begin_shutdown();
    assert!(owner.finish_pending().await.is_err());
    assert_eq!(owner.snapshot(), (0, true, true));
}

#[tokio::test]
async fn reporting_an_unknown_owner_never_authorizes_dependent_close() {
    let owner = Owner::default();
    let job = owner.admit().unwrap();
    let task = job.spawn(async { panic!("controlled unknown") });
    drop(job);
    assert!(task.await.is_err());
    owner.begin_shutdown();
    let closed = Arc::new(AtomicBool::new(false));
    let token = closed.clone();
    let finish = async {
        let _report =
            crate::shutdown::once("device SMS producer owners", owner.finish_pending().await).await;
        token.store(true, Ordering::SeqCst);
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(20), finish)
            .await
            .is_err()
    );
    assert!(!closed.load(Ordering::SeqCst));
}
