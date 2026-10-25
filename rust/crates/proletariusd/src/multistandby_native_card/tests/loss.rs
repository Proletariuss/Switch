use super::*;
use crate::reader_reservation::{
    NativeAuthority, NativeReaderFacts, NativeTransition, NativeTransitionKind,
};

fn loss(f: &NativeFixture) -> NativeTransition {
    let physical = f.physical_binding();
    f.gate
        .prepare_native_transition(
            &physical.producer,
            &f.gate.native_reader_snapshot().0,
            NativeTransitionKind::Loss,
            &physical.reader,
            NativeAuthority::Loss(physical.clone()),
        )
        .unwrap()
}
async fn entered(signal: &AtomicBool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !signal.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn close_for_loss(f: &NativeFixture) -> Result<bool, crate::reader_pcsc::Failure> {
    let owner = f.owner.clone();
    let binding = f.physical_binding();
    tokio::task::spawn_blocking(move || owner.test_close_for_presence(&binding))
        .await
        .unwrap()
}
fn commit_loss(f: &NativeFixture) {
    f.gate
        .commit_confirmed_native_presence_loss(&f.owner, &loss(f))
        .unwrap();
    f.owner.test_ack_presence_loss(&f.physical_binding());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_waits_real_close_then_reports_lost_without_restoring_replacement() {
    let f = NativeFixture::new();
    let card = f.pool.prepare(f.profile(true)).await.unwrap();
    assert!(!f.original_profile_enabled());
    let closed = f.closed_count();
    let switches = f.switch_count();
    let (unblock, signal) = f.hold_close();
    let owner = f.owner.clone();
    let binding = f.physical_binding();
    let closing = tokio::task::spawn_blocking(move || owner.test_close_for_presence(&binding));
    entered(&signal).await;
    assert_eq!(f.closed_count(), closed);
    assert!(f.pool.shutdown_receipt().await.is_err());
    assert!(f.gate.native_multi_current(&f.pool.parent));
    assert!(*card.lost_receiver().borrow());
    assert!(f.gate.acquire_native_multi(card.reader(), 1).is_err());
    let premature = loss(&f);
    assert!(
        f.gate
            .commit_confirmed_native_presence_loss(&f.owner, &premature)
            .is_err()
    );
    f.gate.abort_native_transition(&premature).unwrap();
    unblock.send(()).unwrap();
    assert!(closing.await.unwrap().unwrap());
    assert_eq!(f.closed_count(), closed + 1);
    commit_loss(&f);
    assert_eq!(
        f.pool.retry_shutdown_receipt().await.unwrap(),
        CleanupReceipt::ConfirmedReaderLost
    );
    assert_eq!(f.transaction_count(), 0);
    assert_eq!(
        f.switch_count(),
        switches,
        "loss cleanup must not ENABLE the disappeared card"
    );
    assert!(
        !f.original_profile_enabled(),
        "confirmed loss must not claim restoration"
    );
    let prior = f.physical_binding();
    let replacement = f.owner.native_open(&prior.reader).unwrap();
    assert_ne!(replacement, prior);
    let connect = f
        .gate
        .prepare_native_transition(
            &prior.producer,
            &f.gate.native_reader_snapshot().0,
            NativeTransitionKind::Connect,
            &prior.reader,
            NativeAuthority::Idle,
        )
        .unwrap();
    f.gate
        .commit_native_transition(&connect, Some(&replacement), NativeReaderFacts::default())
        .unwrap();
    let next = f.gate.acquire_native_multi(&prior.reader, 3).unwrap();
    assert_eq!(
        f.pool.shutdown_receipt().await.unwrap(),
        CleanupReceipt::ConfirmedReaderLost
    );
    assert!(f.gate.native_multi_current(&next));
    assert!(
        card.clone()
            .authenticate(ReaderAkaApplication::Isim, [1; 16], [2; 16])
            .await
            .is_err()
    );
    assert_eq!(f.auth_count(), 0);
    f.gate.release_native_multi(&next).unwrap();
    f.owner.native_close(&replacement).unwrap();
    f.owner.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_close_and_logical_generation_change_keep_pool_parent() {
    let f = NativeFixture::new();
    let card = f.pool.prepare(f.profile(true)).await.unwrap();
    let closed = f.closed_count();
    f.fail_close(true);
    assert_eq!(
        close_for_loss(&f).await.unwrap_err().code,
        "fixture_close_failed"
    );
    assert_eq!(f.closed_count(), closed);
    let loss = loss(&f);
    assert!(
        f.gate
            .commit_confirmed_native_presence_loss(&f.owner, &loss)
            .is_err()
    );
    // Model the forbidden inference explicitly: a logical loss receipt is
    // deliberately committed without the Owner proof. Pool must reject it.
    f.gate
        .commit_native_transition(&loss, None, NativeReaderFacts::default())
        .unwrap();
    assert!(f.pool.shutdown_receipt().await.is_err());
    assert!(f.pool.retry_shutdown_receipt().await.is_err());
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&f.pool.parent)
            .unwrap()
    );
    assert!(f.gate.acquire_native_multi(card.reader(), 2).is_err());
    assert_eq!(f.transaction_count(), 0);
    assert!(!f.original_profile_enabled());
    assert!(f.owner.shutdown().await.is_err());
    f.fail_close(false);
    f.owner.shutdown().await.unwrap();
    // Quarantine shutdown succeeding is still not an exact presence receipt.
    assert!(f.pool.retry_shutdown_receipt().await.is_err());
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&f.pool.parent)
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinary_restore_failure_retains_parent_and_real_retry_restores_original() {
    let f = NativeFixture::new();
    let card = f.pool.prepare(f.profile(true)).await.unwrap();
    assert!(!f.original_profile_enabled());
    f.fail_next_apdu();
    assert!(f.pool.shutdown_receipt().await.is_err());
    assert!(f.gate.native_multi_current(&f.pool.parent));
    assert!(f.gate.acquire_native_multi(card.reader(), 1).is_err());
    assert_eq!(f.transaction_count(), 0);
    assert_eq!(
        f.pool.retry_shutdown_receipt().await.unwrap(),
        CleanupReceipt::OriginalRestored
    );
    assert!(f.original_profile_enabled());
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_cancels_live_profile_and_joins_actual_auth_before_restoration() {
    let f = NativeFixture::new();
    let card = f.pool.prepare(f.profile(true)).await.unwrap();
    let unblock = f.hold_auth();
    let auth = tokio::spawn(card.clone().authenticate(
        ReaderAkaApplication::Isim,
        [0x33; 16],
        [0x44; 16],
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.auth_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.transaction_count(), 1);
    assert!(
        !close_for_loss(&f).await.unwrap(),
        "presence may not close borrowed AUTH"
    );
    f.pool.begin_shutdown();
    assert!(*card.lost_receiver().borrow());
    let pool = f.pool.clone();
    let shutdown = tokio::spawn(async move { pool.shutdown_receipt().await });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    assert!(f.gate.native_multi_current(&f.pool.parent));
    assert_eq!(f.transaction_count(), 1);
    unblock.send(()).unwrap();
    assert!(auth.await.unwrap().is_err());
    assert_eq!(
        shutdown.await.unwrap().unwrap(),
        CleanupReceipt::OriginalRestored
    );
    assert_eq!(f.transaction_count(), 0);
    assert!(f.original_profile_enabled());
    f.finish().await;
}

#[tokio::test]
async fn unused_pool_reports_no_profile_work_instead_of_restoration() {
    let f = NativeFixture::new();
    assert_eq!(
        f.pool.shutdown_receipt().await.unwrap(),
        CleanupReceipt::NoProfileWork
    );
    assert_eq!(f.switch_count(), 0);
    assert_eq!(f.auth_count(), 0);
    f.finish().await;
}
