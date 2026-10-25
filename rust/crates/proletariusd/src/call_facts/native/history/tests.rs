use super::*;
use crate::call_facts::tests::{Directory, records};
use std::sync::mpsc;

fn fact(at: i64, action: NativeFactAction) -> NativeFact {
    NativeFact {
        owner_id: "history-owner".into(),
        owner_epoch: 1,
        at: OffsetDateTime::from_unix_timestamp(at).unwrap(),
        action,
    }
}

fn ims_fact(at: i64, state: &str) -> NativeFact {
    fact(
        at,
        NativeFactAction::Ims {
            incoming: true,
            number: "123".into(),
            call_id: "history-call".into(),
            state: state.into(),
        },
    )
}

#[tokio::test]
async fn unavailable_history_never_claims_or_confirms_archive_and_has_owned_diagnostic() {
    let directory = Directory::new();
    let facts = directory.facts();
    let lane = NativeHistory::from_start(&facts, Err("worker creation failed".into()));
    assert_eq!(facts.history_error().as_deref(), Some(UNAVAILABLE));
    assert!(lane.worker_retired(), "this attempt created no worker");
    assert!(!state_lock(&facts.sources[2]).unwrap().native_claimed);
    assert_eq!(
        lane.try_push(fact(100, NativeFactAction::Answered)),
        Err(UNAVAILABLE.into())
    );
    assert_eq!(
        lane.retire(OffsetDateTime::from_unix_timestamp(101).unwrap()),
        Err(UNAVAILABLE.into())
    );
    assert_eq!(lane.drain().await, Err(UNAVAILABLE.into()));
    assert!(records(&facts).is_empty());
    let retained = lane.clone();
    drop(lane);
    assert_eq!(facts.history_error().as_deref(), Some(UNAVAILABLE));
    drop(retained);
    assert!(facts.history_error().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn occupied_history_preserves_real_fsync_owner_and_later_source_while_degraded() {
    let directory = Directory::new();
    let facts = directory.facts();
    let (entered, entry) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, released) = mpsc::sync_channel(1);
    let released = Mutex::new(released);
    *facts.before_ingest.lock().unwrap() = Some(Arc::new(move || {
        if let Some(entered) = entered.lock().unwrap().take() {
            entered.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }));
    let old = NativeSink::start_reader_ims(facts.clone(), "old-reader-ims".into(), 8).unwrap();
    old.try_push(ims_fact(100, "incoming")).unwrap();
    old.try_push(ims_fact(101, "ended")).unwrap();
    entry.await.unwrap();
    old.retire(OffsetDateTime::from_unix_timestamp(102).unwrap())
        .unwrap();
    let (frozen, before_queue) = {
        let state = state_lock(&facts.sources[2]).unwrap();
        (
            state.frozen.as_ref().unwrap().record.clone(),
            old.0.lane.queue.lock().unwrap().facts.clone(),
        )
    };
    let degraded = NativeHistory::start_reader_ims(facts.clone(), "next-reader-ims".into(), 8);
    assert!(matches!(degraded, NativeHistory::Unavailable(_)));
    assert!(facts.history_error().unwrap().contains(UNAVAILABLE));
    assert!(
        degraded
            .try_push(fact(103, NativeFactAction::Answered))
            .is_err()
    );
    assert!(degraded.retire(OffsetDateTime::now_utc()).is_err());
    assert!(degraded.drain().await.is_err());
    assert!(degraded.worker_retired());
    assert!(!old.worker_retired());
    {
        let state = state_lock(&facts.sources[2]).unwrap();
        assert_eq!(state.producer_id, "old-reader-ims");
        assert!(state.native_claimed);
        assert_eq!(state.frozen.as_ref().unwrap().record, frozen);
        assert!(state.history_error.is_none());
        assert_eq!(old.0.lane.queue.lock().unwrap().facts, before_queue);
    }
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), old.drain())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(records(&facts), vec![frozen]);
    assert!(old.worker_retired());
    // The active diagnostic is independent of both the old source retiring
    // and a third source successfully acquiring it. Dropping the degraded
    // handle later cannot release or rewrite that third producer's claim.
    let next = NativeHistory::start_reader_ims(facts.clone(), "third-reader-ims".into(), 8);
    assert!(matches!(next, NativeHistory::Active(_)));
    assert_eq!(facts.history_error().as_deref(), Some(UNAVAILABLE));
    drop(degraded);
    assert!(facts.history_error().is_none());
    assert_eq!(
        state_lock(&facts.sources[2]).unwrap().producer_id,
        "third-reader-ims"
    );
    assert!(state_lock(&facts.sources[2]).unwrap().native_claimed);
    next.retire(OffsetDateTime::from_unix_timestamp(104).unwrap())
        .unwrap();
    next.drain().await.unwrap();
    assert!(next.worker_retired());
}

#[tokio::test]
async fn unavailable_lane_never_clears_old_storage_failure_or_its_frozen_terminal() {
    let directory = Directory::new();
    let facts = directory.facts();
    std::fs::create_dir(facts.store.path()).unwrap();
    let old = NativeSink::start_reader_ims(facts.clone(), "failed-store-reader".into(), 8).unwrap();
    old.try_push(ims_fact(100, "incoming")).unwrap();
    old.try_push(ims_fact(101, "ended")).unwrap();
    old.retire(OffsetDateTime::from_unix_timestamp(102).unwrap())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while state_lock(&facts.sources[2])
            .unwrap()
            .native_pending_error
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (frozen, pending) = {
        let state = state_lock(&facts.sources[2]).unwrap();
        (
            state.frozen.as_ref().unwrap().record.clone(),
            state.native_pending_error.clone().unwrap(),
        )
    };
    let lane = NativeHistory::start_reader_ims(facts.clone(), "degraded-reader".into(), 8);
    assert!(matches!(lane, NativeHistory::Unavailable(_)));
    assert!(lane.drain().await.is_err());
    assert!(lane.worker_retired());
    assert!(!old.worker_retired());
    let aggregate = facts.history_error().unwrap();
    assert!(aggregate.contains(&pending) && aggregate.contains(UNAVAILABLE));
    drop(lane);
    {
        let state = state_lock(&facts.sources[2]).unwrap();
        assert_eq!(state.producer_id, "failed-store-reader");
        assert!(state.native_claimed);
        assert_eq!(
            state.native_pending_error.as_deref(),
            Some(pending.as_str())
        );
        assert_eq!(state.frozen.as_ref().unwrap().record, frozen);
    }
    assert!(facts.history_error().unwrap().contains(&pending));
    std::fs::remove_dir(facts.store.path()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), old.drain())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(records(&facts), vec![frozen]);
    assert!(facts.history_error().is_none());
}
