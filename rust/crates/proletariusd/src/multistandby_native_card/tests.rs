use super::*;
use crate::multistandby_card::production::NativeFixture;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_profiles_use_short_real_card_workflows_and_restore_original() {
    let fixture = NativeFixture::new();
    let first = fixture.pool.prepare(fixture.profile(true)).await.unwrap();
    assert_eq!(first.identity().iccid.as_str(), "8901234567890123456");
    assert_eq!(
        fixture.transaction_count(),
        0,
        "prepared profile must not retain a physical lease"
    );
    let second = fixture.pool.prepare(fixture.profile(false)).await.unwrap();
    assert_eq!(fixture.transaction_count(), 0);
    for card in [&first, &second, &first] {
        let result = card
            .clone()
            .authenticate(ReaderAkaApplication::Isim, [0x33; 16], [0x44; 16])
            .await
            .unwrap();
        assert!(matches!(result, SimAkaResult::Success { ref res, .. } if res == &[1;8]));
        assert_eq!(
            fixture.transaction_count(),
            0,
            "AKA must join physical release before returning"
        );
    }
    assert_eq!(fixture.auth_count(), 3);
    assert!(fixture.switch_count() >= 5);
    first.clone().release_with_confirmation().await.unwrap();
    assert!(
        first
            .clone()
            .authenticate(ReaderAkaApplication::Usim, [1; 16], [2; 16])
            .await
            .is_err()
    );
    second.release_with_confirmation().await.unwrap();
    assert_eq!(fixture.auth_count(), 3);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_fifo_cancelled_waiter_keeps_physical_ownership_until_aka_joins() {
    let fixture = NativeFixture::new();
    let first = fixture.pool.prepare(fixture.profile(true)).await.unwrap();
    let second = fixture.pool.prepare(fixture.profile(false)).await.unwrap();
    let unblock = fixture.hold_auth();
    let pending = tokio::spawn(first.clone().authenticate(
        ReaderAkaApplication::Isim,
        [0x33; 16],
        [0x44; 16],
    ));
    tokio::time::timeout(Duration::from_secs(4), async {
        while fixture.auth_count() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    let next = tokio::spawn(second.clone().authenticate(
        ReaderAkaApplication::Isim,
        [0x33; 16],
        [0x44; 16],
    ));
    let release = tokio::spawn(first.clone().release_with_confirmation());
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!next.is_finished());
    assert!(!release.is_finished());
    assert_eq!(fixture.auth_count(), 1);
    assert_eq!(fixture.transaction_count(), 1);
    unblock.send(()).unwrap();
    release.await.unwrap().unwrap();
    next.await.unwrap().unwrap();
    assert_eq!(fixture.auth_count(), 2);
    assert_eq!(fixture.transaction_count(), 0);
    second.release_with_confirmation().await.unwrap();
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_pool_eid_mismatch_and_unpolled_cancel_never_authenticate() {
    let fixture = NativeFixture::new();
    let first = fixture.pool.prepare(fixture.profile(true)).await.unwrap();
    let mut other = fixture.profile(false);
    other.eid = "89049032000000000000000000000002".into();
    assert!(fixture.pool.prepare(other).await.is_err());
    let unused = first
        .clone()
        .authenticate(ReaderAkaApplication::Isim, [0x33; 16], [0x44; 16]);
    drop(unused);
    first.release_with_confirmation().await.unwrap();
    assert_eq!(fixture.auth_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn missing_physical_completion_is_never_acknowledged_as_release() {
    let session = Session::new();
    session
        .requests
        .lock()
        .unwrap()
        .active
        .insert(1, Arc::new(AtomicBool::new(false)));
    drop(Completed {
        session: session.clone(),
        id: 1,
        confirmed: false,
    });
    assert!(session.release().await.is_err());
    assert!(*session.lost.borrow());
}

#[tokio::test]
async fn confirmed_no_submission_can_release_cancelled_queue_entry() {
    let session = Session::new();
    session
        .requests
        .lock()
        .unwrap()
        .active
        .insert(1, Arc::new(AtomicBool::new(false)));
    drop(Completed {
        session: session.clone(),
        id: 1,
        confirmed: true,
    });
    session.release().await.unwrap();
}

mod loss;
