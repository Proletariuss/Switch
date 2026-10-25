use super::super::tests::{Directory, records};
use super::*;
use std::sync::mpsc;

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("controlled facts transition did not complete");
}

fn fact(at: i64, action: NativeFactAction) -> NativeFact {
    NativeFact {
        owner_id: "native-owner".into(),
        owner_epoch: 1,
        at: OffsetDateTime::from_unix_timestamp(at).unwrap(),
        action,
    }
}

#[tokio::test]
async fn native_producer_survives_go_replacement_and_drains_ordered_retirement() {
    let directory = Directory::new();
    let facts = directory.facts();
    let sink = NativeSink::start(facts.clone(), "native-cs".into(), 8).unwrap();
    assert!(!sink.worker_retired());
    assert!(NativeSink::start(facts.clone(), "other-native-cs".into(), 8).is_err());
    sink.try_push(fact(
        100,
        NativeFactAction::Begin {
            incoming: true,
            number: "123".into(),
        },
    ))
    .unwrap();
    facts.replace_producer("new-go-producer".into()).unwrap();
    sink.try_push(fact(101, NativeFactAction::Answered))
        .unwrap();
    sink.retire(OffsetDateTime::from_unix_timestamp(105).unwrap())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), sink.drain())
        .await
        .unwrap()
        .unwrap();
    assert!(sink.worker_retired());
    let records = records(&facts);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["direction"], "in");
    assert_eq!(records[0]["answered"], true);
    assert_eq!(records[0]["duration"], 5);
    assert_eq!(
        state_lock(&facts.sources[0]).unwrap().producer_id,
        "native-cs"
    );
    assert_eq!(
        state_lock(&facts.sources[1]).unwrap().producer_id,
        "new-go-producer"
    );
    assert!(state_lock(&facts.sources[0]).unwrap().owner_retired);
    let next = NativeSink::start(facts, "next-native-cs".into(), 8).unwrap();
    next.retire(OffsetDateTime::from_unix_timestamp(106).unwrap())
        .unwrap();
    next.drain().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_fsync_keeps_admission_short_and_reserved_retire_after_queue_saturation() {
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
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        }
    }));
    let sink = NativeSink::start(facts.clone(), "native-cs".into(), 2).unwrap();
    sink.try_push(fact(
        100,
        NativeFactAction::Begin {
            incoming: false,
            number: "123".into(),
        },
    ))
    .unwrap();
    until(|| {
        state_lock(&facts.sources[0])
            .unwrap()
            .last
            .as_ref()
            .is_some_and(|last| last.sequence == 1)
    })
    .await;
    sink.try_push(fact(101, NativeFactAction::End)).unwrap();
    entry.await.unwrap();
    let frozen = state_lock(&facts.sources[0])
        .unwrap()
        .frozen
        .as_ref()
        .unwrap()
        .record
        .clone();
    // The real archive worker is blocked, yet callback-side admissions and
    // unrelated Go producer replacement finish without waiting on fsync.
    sink.try_push(fact(102, NativeFactAction::Number("late".into())))
        .unwrap();
    assert!(
        sink.try_push(fact(103, NativeFactAction::Answered))
            .is_err()
    );
    facts.replace_producer("new-go".into()).unwrap();
    sink.retire(OffsetDateTime::from_unix_timestamp(104).unwrap())
        .unwrap();
    assert_eq!(sink.0.lane.queue.lock().unwrap().facts.len(), 3);
    let retained = sink.clone();
    let waiter = tokio::spawn(async move { retained.drain().await });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(!sink.worker_retired());
    release.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), sink.drain())
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(records(&facts), vec![frozen]);
    assert!(state_lock(&facts.sources[0]).unwrap().owner_retired);
    assert!(sink.0.lane.queue.lock().unwrap().facts.is_empty());
    assert!(sink.history_error().unwrap().contains("队列已满"));
    assert!(sink.worker_retired());
}

#[tokio::test]
async fn storage_failure_retries_frozen_payload_before_later_call_and_clears_only_pending_error() {
    let directory = Directory::new();
    let facts = directory.facts();
    let sink = NativeSink::start(facts.clone(), "native-cs".into(), 8).unwrap();
    std::fs::create_dir(facts.store.path()).unwrap();
    sink.try_push(fact(
        100,
        NativeFactAction::Begin {
            incoming: false,
            number: "123".into(),
        },
    ))
    .unwrap();
    sink.try_push(fact(101, NativeFactAction::End)).unwrap();
    until(|| sink.history_error().is_some()).await;
    assert!(!sink.worker_retired());
    let frozen = state_lock(&facts.sources[0])
        .unwrap()
        .frozen
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(frozen.fact.sequence, 2);
    sink.try_push(fact(
        102,
        NativeFactAction::Begin {
            incoming: true,
            number: "456".into(),
        },
    ))
    .unwrap();
    sink.retire(OffsetDateTime::from_unix_timestamp(104).unwrap())
        .unwrap();
    assert_eq!(
        sink.0.lane.queue.lock().unwrap().facts.front().unwrap(),
        &frozen.fact
    );
    std::fs::remove_dir(facts.store.path()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), sink.drain())
        .await
        .unwrap()
        .unwrap();
    let records = records(&facts);
    assert_eq!(records.len(), 2);
    assert!(records.contains(&frozen.record));
    let next = records
        .iter()
        .find(|record| record["number"] == "456")
        .unwrap();
    assert_eq!(next["direction"], "in");
    assert_eq!(next["duration"], 2);
    assert!(sink.0.lane.queue.lock().unwrap().facts.is_empty());
    assert!(sink.history_error().is_none());
    assert!(sink.worker_retired());
}

#[tokio::test]
async fn drain_handles_completion_both_before_and_after_waiter_registration() {
    for early in [true, false] {
        let directory = Directory::new();
        let facts = directory.facts();
        let sink = NativeSink::start(facts, "native-cs".into(), 8).unwrap();
        if early {
            sink.retire(OffsetDateTime::from_unix_timestamp(100).unwrap())
                .unwrap();
            until(|| sink.0.lane.queue.lock().unwrap().finished).await;
        }
        let waiter = sink.drain();
        tokio::pin!(waiter);
        if !early {
            tokio::select! {
                biased;
                _ = &mut waiter => panic!("unretired lane cannot be drained"),
                _ = std::future::ready(()) => {},
            }
            sink.retire(OffsetDateTime::from_unix_timestamp(100).unwrap())
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(3), &mut waiter)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn early_answer_timestamp_is_ingested_after_accepted_begin_without_rewriting_it() {
    let directory = Directory::new();
    let facts = directory.facts();
    let sink = NativeSink::start(facts.clone(), "native-cs".into(), 8).unwrap();
    sink.try_push_batch(vec![
        fact(
            104,
            NativeFactAction::Begin {
                incoming: false,
                number: "123".into(),
            },
        ),
        fact(102, NativeFactAction::Answered),
    ])
    .unwrap();
    until(|| {
        state_lock(&facts.sources[0])
            .unwrap()
            .last
            .as_ref()
            .is_some_and(|last| last.sequence == 2)
    })
    .await;
    let state = state_lock(&facts.sources[0]).unwrap();
    assert_eq!(
        state.last.as_ref().unwrap().observed_at,
        "1970-01-01T00:01:42Z"
    );
    assert_eq!(
        state.active.as_ref().unwrap().started_at,
        OffsetDateTime::from_unix_timestamp(104).unwrap()
    );
    assert!(state.active.as_ref().unwrap().answered);
    drop(state);
    sink.retire(OffsetDateTime::from_unix_timestamp(106).unwrap())
        .unwrap();
    sink.drain().await.unwrap();
    assert_eq!(records(&facts)[0]["duration"], 2);
}

fn ims_fact(at: i64, state: &str, call_id: &str) -> NativeFact {
    fact(
        at,
        NativeFactAction::Ims {
            incoming: true,
            number: "123".into(),
            call_id: call_id.into(),
            state: state.into(),
        },
    )
}

#[tokio::test]
async fn reader_ims_lane_survives_go_restart_and_archives_separately_from_module() {
    let directory = Directory::new();
    let facts = directory.facts();
    let reader = NativeSink::start_reader_ims(facts.clone(), "native-reader".into(), 8).unwrap();
    let module = NativeSink::start(facts.clone(), "native-module".into(), 8).unwrap();
    assert!(NativeSink::start_reader_ims(facts.clone(), "replacement".into(), 8).is_err());
    reader
        .try_push(ims_fact(100, "incoming", "reader-call"))
        .unwrap();
    module
        .try_push(fact(
            100,
            NativeFactAction::Begin {
                incoming: false,
                number: "456".into(),
            },
        ))
        .unwrap();
    until(|| state_lock(&facts.sources[2]).unwrap().active.is_some()).await;
    facts.replace_producer("replacement-go".into()).unwrap();
    assert_eq!(
        state_lock(&facts.sources[2]).unwrap().producer_id,
        "native-reader"
    );
    assert_eq!(
        state_lock(&facts.sources[1]).unwrap().producer_id,
        "replacement-go"
    );
    reader
        .try_push(ims_fact(102, "active", "reader-call"))
        .unwrap();
    reader
        .try_push(ims_fact(106, "ended", "reader-call"))
        .unwrap();
    reader
        .retire(OffsetDateTime::from_unix_timestamp(107).unwrap())
        .unwrap();
    module
        .retire(OffsetDateTime::from_unix_timestamp(108).unwrap())
        .unwrap();
    reader.drain().await.unwrap();
    module.drain().await.unwrap();
    let entries = records(&facts);
    assert_eq!(entries.len(), 2);
    let entry = entries
        .iter()
        .find(|entry| entry["device"] == "reader")
        .unwrap();
    assert_eq!(entry["answered"], true);
    assert_eq!(entry["direction"], "in");
    assert_eq!(entry["duration"], 6);
    let next = NativeSink::start_reader_ims(facts, "next-reader".into(), 8).unwrap();
    next.retire(OffsetDateTime::from_unix_timestamp(109).unwrap())
        .unwrap();
    next.drain().await.unwrap();
}

#[tokio::test]
async fn native_reader_ims_claim_rejects_go_observation_even_with_matching_producer() {
    let directory = Directory::new();
    let facts = directory.facts();
    let sink = NativeSink::start_reader_ims(facts.clone(), "native-reader".into(), 8).unwrap();
    let observation = ims_fact(100, "incoming", "wrong-source")
        .observation(Source::ReaderIms, "native-reader", 1)
        .unwrap();
    let error = facts.observe_go(observation, false).unwrap_err();
    assert_eq!(error.code, "native_call_source_owned");
    assert!(state_lock(&facts.sources[2]).unwrap().active.is_none());
    sink.retire(OffsetDateTime::from_unix_timestamp(101).unwrap())
        .unwrap();
    sink.drain().await.unwrap();
    assert!(records(&facts).is_empty());
}

#[test]
fn native_reader_claim_cannot_steal_live_go_call() {
    let directory = Directory::new();
    let facts = directory.facts();
    let producer = state_lock(&facts.sources[2]).unwrap().producer_id.clone();
    let observation = ims_fact(100, "incoming", "go-call")
        .observation(Source::ReaderIms, &producer, 1)
        .unwrap();
    facts.observe_go(observation, false).unwrap();
    assert!(NativeSink::start_reader_ims(facts.clone(), "native-reader".into(), 8).is_err());
    assert_eq!(state_lock(&facts.sources[2]).unwrap().producer_id, producer);
    assert_eq!(
        state_lock(&facts.sources[2])
            .unwrap()
            .active
            .as_ref()
            .unwrap()
            .call_id
            .as_deref(),
        Some("go-call")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_finished_does_not_prove_join_and_cancelled_concurrent_drains_share_real_join() {
    let directory = Directory::new();
    let sink = NativeSink::start_reader_ims(directory.facts(), "native-reader".into(), 8).unwrap();
    let (entered, entry) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, released) = mpsc::sync_channel(1);
    let released = Mutex::new(released);
    *sink.0.lane.before_worker_exit.lock().unwrap() = Some(Arc::new(move || {
        entered.lock().unwrap().take().unwrap().send(()).unwrap();
        released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
    }));
    sink.retire(OffsetDateTime::from_unix_timestamp(100).unwrap())
        .unwrap();
    entry.await.unwrap();
    assert!(sink.0.lane.queue.lock().unwrap().finished);
    assert!(!sink.worker_retired());
    let first_sink = sink.clone();
    let first = tokio::spawn(async move { first_sink.drain().await });
    until(|| matches!(*sink.0.worker.state.lock().unwrap(), JoinState::Joining)).await;
    let second_sink = sink.clone();
    let mut second = tokio::spawn(async move { second_sink.drain().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut second)
            .await
            .is_err()
    );
    assert!(!first.is_finished());
    assert!(!sink.worker_retired());
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(!sink.worker_retired());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(sink.worker_retired());
    sink.drain().await.unwrap();
}

#[tokio::test]
async fn retired_worker_preserves_rejected_head_native_claim_and_sticky_archive_error() {
    let directory = Directory::new();
    let facts = directory.facts();
    let sink = NativeSink::start_reader_ims(facts.clone(), "native-reader".into(), 8).unwrap();
    let mut rejected = ims_fact(101, "active", "reader-call");
    rejected.owner_id = "conflicting-owner".into();
    sink.try_push_batch(vec![
        ims_fact(100, "incoming", "reader-call"),
        rejected.clone(),
    ])
    .unwrap();
    sink.retire(OffsetDateTime::from_unix_timestamp(102).unwrap())
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), sink.drain())
            .await
            .unwrap()
            .is_err()
    );
    assert!(sink.worker_retired());
    let queue = sink.0.lane.queue.lock().unwrap();
    assert_eq!(
        queue.facts.front().unwrap(),
        &rejected
            .observation(Source::ReaderIms, "native-reader", 2)
            .unwrap()
    );
    assert_eq!(queue.facts.len(), 2); // Failed head plus the exact retained retire.
    drop(queue);
    let error = sink.history_error().unwrap();
    let state = state_lock(&facts.sources[2]).unwrap();
    assert!(state.native_claimed);
    assert!(!state.owner_retired);
    assert!(state.active.is_some());
    drop(state);
    assert!(records(&facts).is_empty());
    assert_eq!(sink.drain().await.unwrap_err(), error);
    assert_eq!(sink.history_error().as_deref(), Some(error.as_str()));
    assert!(NativeSink::start_reader_ims(facts, "replacement".into(), 8).is_err());
}

#[tokio::test]
async fn cancelled_drain_does_not_retain_sink_owner_or_prevent_drop_closing_worker() {
    let directory = Directory::new();
    let sink = NativeSink::start(directory.facts(), "native-cs".into(), 8).unwrap();
    let completion = sink.0.worker.clone();
    let waiting_sink = sink.clone();
    let waiter = tokio::spawn(async move { waiting_sink.drain().await });
    until(|| matches!(*completion.state.lock().unwrap(), JoinState::Joining)).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(!sink.worker_retired());
    drop(sink);
    until(|| completion.retired.load(Ordering::Acquire)).await;
    assert!(matches!(
        &*completion.state.lock().unwrap(),
        JoinState::Joined(Ok(()))
    ));
}

#[tokio::test]
async fn joined_panicked_worker_has_retirement_proof_but_never_a_successful_drain() {
    let directory = Directory::new();
    let sink = NativeSink::start(directory.facts(), "native-cs".into(), 8).unwrap();
    *sink.0.lane.before_worker_exit.lock().unwrap() =
        Some(Arc::new(|| panic!("worker exit fixture")));
    sink.retire(OffsetDateTime::from_unix_timestamp(100).unwrap())
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(3), sink.drain())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error, "native call worker panicked");
    assert!(sink.worker_retired());
    assert_eq!(sink.drain().await.unwrap_err(), error);
}

#[tokio::test]
async fn rejected_retire_still_wakes_and_joins_the_actual_worker_without_inventing_a_terminal() {
    let directory = Directory::new();
    let facts = directory.facts();
    let sink = NativeSink::start(facts.clone(), "native-cs".into(), 8).unwrap();
    sink.0.lane.queue.lock().unwrap().sequence = u64::MAX;
    assert_eq!(
        sink.retire(OffsetDateTime::from_unix_timestamp(100).unwrap())
            .unwrap_err(),
        "native call sequence exhausted"
    );
    tokio::time::timeout(Duration::from_secs(3), sink.drain())
        .await
        .unwrap()
        .unwrap();
    assert!(sink.worker_retired());
    assert!(sink.0.lane.queue.lock().unwrap().facts.is_empty());
    assert!(state_lock(&facts.sources[0]).unwrap().last.is_none());
    assert!(records(&facts).is_empty());
}
