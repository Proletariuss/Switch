use super::*;
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt};
struct Fixture {
    root: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new(seed: bool) -> Self {
        let root = PathBuf::from(std::env::var_os("SWITCH_BUILD_DIR").expect("isolated runtime"))
            .join(format!(
                "tmp/b2-{}-{}",
                std::process::id(),
                TEMP_SEQUENCE.fetch_add(1, Ordering::SeqCst)
            ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("state.enc");
        if seed {
            let doc=JsonDocument::from_bytes(Zeroizing::new(serde_json::to_vec(&json!({"seen":{},"pending":{},"opaque":{"integer":9007199254740993u64},"rust_producer_admission_v1":{
   "schema":1,"lane":"nwait_v1","compatibility":"isolated_fixture_only","binding":binding(),
   "issued_sequence":0,"completed_sequence":0,"cursor":9,"last_receipt_id":"","active":null
  }})).unwrap()),StateLimits::default()).unwrap();
            fs::write(&path, seal(&key(), &doc, StateLimits::default()).unwrap()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        Self { root, path }
    }
    fn open(&self) -> QueueStore {
        QueueStore::open_bound_page_authority(&self.path, key(), StateLimits::default(), &binding())
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn key() -> StateKey {
    StateKey::from_slice(&[7; 32]).unwrap()
}
fn binding() -> ProducerBinding {
    ProducerBinding {
        queue_id: "queue-fixture".into(),
        source_id: "source-fixture".into(),
        handoff_id: "handoff-fixture".into(),
    }
}
fn request() -> PageRequest {
    PageRequest {
        expected_completed_sequence: 0,
        from_cursor: 9,
        to_cursor: 10,
        disposition: PageDisposition::Deliver,
    }
}
fn options(enabled: bool) -> EnqueueOptions {
    let key = key();
    let tg = DestinationBinding::telegram(&key, "synthetic", "synthetic");
    let bark = DestinationBinding::from_go_identity(&key, "bark\0synthetic");
    EnqueueOptions::new(enabled, &tg, false, &bark)
}
fn message(id: &str, time: &str) -> ForwardingMessage {
    ForwardingMessage::new(id, "inbound", "sender", "synthetic body", time, "N待").unwrap()
}
const NOW: &str = "2026-01-02T03:04:05Z";
fn admit(store: &mut QueueStore) -> PageReceipt {
    store
        .admit_page_and_persist(
            &binding(),
            request(),
            &[message("nwait:fixture:first", "2026-01-01T00:00:00Z")],
            NOW,
            &options(true),
        )
        .unwrap()
}

#[test]
fn b2_existing_swf1_without_metadata_and_missing_queue_refuse_without_creation() {
    let f = Fixture::new(false);
    assert!(matches!(
        QueueStore::open_bound_page_authority(&f.path, key(), StateLimits::default(), &binding()),
        Err(AdmissionError::Uninitialized)
    ));
    assert!(!f.path.exists());
    let doc = JsonDocument::from_bytes(
        Zeroizing::new(br#"{"seen":{},"pending":{}}"#.to_vec()),
        StateLimits::default(),
    )
    .unwrap();
    fs::write(&f.path, seal(&key(), &doc, StateLimits::default()).unwrap()).unwrap();
    fs::set_permissions(&f.path, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::read(&f.path).unwrap();
    assert!(matches!(
        QueueStore::open_bound_page_authority(&f.path, key(), StateLimits::default(), &binding()),
        Err(AdmissionError::Uninitialized)
    ));
    assert_eq!(fs::read(&f.path).unwrap(), before);
}
#[test]
fn b2_exact_page_replay_is_read_only_and_conflicting_payload_refuses() {
    let f = Fixture::new(true);
    let mut store = f.open();
    let receipt = admit(&mut store);
    assert_eq!(receipt.queued(), 1);
    let before = fs::read(&f.path).unwrap();
    assert_eq!(admit(&mut store), receipt);
    assert_eq!(fs::read(&f.path).unwrap(), before);
    assert!(matches!(
        store.admit_page_and_persist(
            &binding(),
            request(),
            &[message("changed", NOW)],
            NOW,
            &options(true)
        ),
        Err(AdmissionError::Conflict)
    ));
    assert_eq!(store.inspect().unwrap().tasks.len(), 1);
}
#[test]
fn b2_before_rename_never_commits_receipt_or_task() {
    let f = Fixture::new(true);
    let mut store = f.open();
    let before = fs::read(&f.path).unwrap();
    FAIL_BEFORE_QUEUE_RENAME.with(|v| v.set(true));
    assert!(matches!(
        store.admit_page_and_persist(
            &binding(),
            request(),
            &[message("first", NOW)],
            NOW,
            &options(true)
        ),
        Err(AdmissionError::Store(StateStoreError::Io))
    ));
    assert_eq!(fs::read(&f.path).unwrap(), before);
    assert!(store.active_page_receipt(&binding()).unwrap().is_none());
    assert_eq!(store.inspect().unwrap().tasks.len(), 0);
    drop(store);
    let mut reopened = f.open();
    assert_eq!(admit(&mut reopened).queued(), 1);
}
#[test]
fn b2_after_rename_poison_reopen_has_same_receipt_and_one_task() {
    let f = Fixture::new(true);
    let mut store = f.open();
    FAIL_AFTER_QUEUE_RENAME.with(|v| v.set(true));
    assert!(matches!(
        store.admit_page_and_persist(
            &binding(),
            request(),
            &[message("nwait:fixture:first", "2026-01-01T00:00:00Z")],
            NOW,
            &options(true)
        ),
        Err(AdmissionError::Store(StateStoreError::PersistenceUncertain))
    ));
    assert!(matches!(
        store.active_page_receipt(&binding()),
        Err(AdmissionError::Store(StateStoreError::ReopenRequired))
    ));
    drop(store);
    let mut reopened = f.open();
    let receipt = reopened.active_page_receipt(&binding()).unwrap().unwrap();
    let bytes = fs::read(&f.path).unwrap();
    assert_eq!(admit(&mut reopened), receipt);
    assert_eq!(fs::read(&f.path).unwrap(), bytes);
    assert_eq!(reopened.inspect().unwrap().tasks.len(), 1);
}
#[test]
fn b2_receipt_survives_real_seen_pruning_and_pending_retirement() {
    let f = Fixture::new(true);
    let mut store = f.open();
    let receipt = admit(&mut store);
    let task = store.inspect().unwrap().tasks[0].id.clone();
    store
        .cancel_destination(&task, ForwardingChannel::Telegram, NOW)
        .unwrap();
    assert_eq!(store.inspect().unwrap().tasks.len(), 0);
    let others = (0..10001)
        .map(|i| message(&format!("device:{i}"), "2026-02-01T00:00:00Z"))
        .collect::<Vec<_>>();
    store
        .enqueue_and_persist(&others, NOW, &options(false))
        .unwrap();
    // Demonstrate that the legacy dedup alone now really would recreate a task.
    let (_, legacy_replay) = store
        .current
        .enqueue(
            &[message("nwait:fixture:first", "2026-01-01T00:00:00Z")],
            NOW,
            &options(true),
            StateLimits::default(),
        )
        .unwrap();
    assert_eq!(legacy_replay.queued, 1);
    let before = fs::read(&f.path).unwrap();
    assert_eq!(admit(&mut store), receipt);
    assert_eq!(fs::read(&f.path).unwrap(), before);
    assert_eq!(store.inspect().unwrap().tasks.len(), 0);
    drop(store);
    let mut reopened = f.open();
    assert_eq!(admit(&mut reopened), receipt);
    assert_eq!(reopened.inspect().unwrap().tasks.len(), 0);
}
#[test]
fn b2_empty_and_all_dedup_pages_still_commit_receipt() {
    for dedup in [false, true] {
        let f = Fixture::new(true);
        let mut store = f.open();
        let messages = if dedup {
            vec![message("first", NOW)]
        } else {
            vec![]
        };
        if dedup {
            store
                .enqueue_and_persist(&messages, NOW, &options(true))
                .unwrap();
        }
        let before = fs::read(&f.path).unwrap();
        let receipt = store
            .admit_page_and_persist(&binding(), request(), &messages, NOW, &options(true))
            .unwrap();
        assert_eq!(receipt.newly_seen(), 0);
        assert_ne!(fs::read(&f.path).unwrap(), before);
        drop(store);
        assert!(f.open().active_page_receipt(&binding()).unwrap().is_some());
    }
}
#[test]
fn b2_ack_release_stale_ack_and_retired_page_cannot_reopen_old_slot() {
    let f = Fixture::new(true);
    let mut store = f.open();
    let receipt = admit(&mut store);
    store
        .complete_page_and_persist(&binding(), ProgressAck::for_fixture(&receipt))
        .unwrap();
    store
        .complete_page_and_persist(&binding(), ProgressAck::for_fixture(&receipt))
        .unwrap();
    assert!(matches!(
        store.admit_page_and_persist(&binding(), request(), &[], NOW, &options(true)),
        Err(AdmissionError::Conflict)
    ));
    let next = PageRequest {
        expected_completed_sequence: 1,
        from_cursor: 10,
        to_cursor: 11,
        disposition: PageDisposition::DisabledSkip,
    };
    let second = store
        .admit_page_and_persist(&binding(), next, &[], NOW, &options(false))
        .unwrap();
    assert_eq!(second.sequence(), 2);
    assert!(matches!(
        store.complete_page_and_persist(&binding(), ProgressAck::for_fixture(&receipt)),
        Err(AdmissionError::Conflict)
    ));
    assert_eq!(
        store.active_page_receipt(&binding()).unwrap().unwrap(),
        second
    );
}
#[test]
fn b2_release_before_and_after_rename_preserve_safe_restart_states() {
    for after in [false, true] {
        let f = Fixture::new(true);
        let mut store = f.open();
        let receipt = admit(&mut store);
        if after {
            FAIL_AFTER_QUEUE_RENAME.with(|v| v.set(true));
        } else {
            FAIL_BEFORE_QUEUE_RENAME.with(|v| v.set(true));
        }
        assert!(
            store
                .complete_page_and_persist(&binding(), ProgressAck::for_fixture(&receipt))
                .is_err()
        );
        drop(store);
        let mut reopened = f.open();
        assert_eq!(
            reopened.active_page_receipt(&binding()).unwrap().is_none(),
            after
        );
        reopened
            .complete_page_and_persist(&binding(), ProgressAck::for_fixture(&receipt))
            .unwrap();
        assert!(reopened.active_page_receipt(&binding()).unwrap().is_none());
        assert!(matches!(
            reopened.admit_page_and_persist(&binding(), request(), &[], NOW, &options(true)),
            Err(AdmissionError::Conflict)
        ));
    }
}
#[test]
fn b2_unknown_metadata_binding_and_unproven_compatibility_fail_closed() {
    let f = Fixture::new(true);
    let mut store = f.open();
    let mut wrong = binding();
    wrong.source_id = "alien".into();
    assert!(matches!(
        store.active_page_receipt(&wrong),
        Err(AdmissionError::BindingMismatch)
    ));
    let raw = crate::queue::admission_extension(&store.current)
        .unwrap()
        .unwrap();
    let changed = raw.replace("isolated_fixture_only", "unverified_legacy");
    let next =
        crate::queue::replace_admission_extension(&store.current, &changed, store.limits).unwrap();
    store.persist_admission_document(next).unwrap();
    assert!(matches!(
        store.active_page_receipt(&binding()),
        Err(AdmissionError::CompatibilityUnproven)
    ));
}

#[test]
fn b2_oversize_page_sequence_exhaustion_and_receipt_capacity_never_partially_commit() {
    let f = Fixture::new(true);
    let mut store = f.open();
    let before = fs::read(&f.path).unwrap();
    let messages = (0..101)
        .map(|i| message(&format!("id:{i}"), NOW))
        .collect::<Vec<_>>();
    assert!(matches!(
        store.admit_page_and_persist(&binding(), request(), &messages, NOW, &options(true)),
        Err(AdmissionError::PageTooLarge)
    ));
    assert_eq!(fs::read(&f.path).unwrap(), before);
    store.limits = StateLimits::new(store.current.as_bytes().len() + 1).unwrap();
    assert!(matches!(
        store.admit_page_and_persist(&binding(), request(), &[], NOW, &options(true)),
        Err(AdmissionError::Store(StateStoreError::State(
            StateError::SizeLimit
        )))
    ));
    assert_eq!(fs::read(&f.path).unwrap(), before);
    store.limits = StateLimits::default();
    let mut authority = Authority::decode(&store.current, &binding()).unwrap();
    authority.issued_sequence = u64::MAX;
    authority.completed_sequence = u64::MAX;
    authority.last_receipt_id = "c".repeat(64);
    let next = crate::queue::replace_admission_extension(
        &store.current,
        &authority.encode().unwrap(),
        store.limits,
    )
    .unwrap();
    store.persist_admission_document(next).unwrap();
    let before = fs::read(&f.path).unwrap();
    let exhausted = PageRequest {
        expected_completed_sequence: u64::MAX,
        ..request()
    };
    assert!(matches!(
        store.admit_page_and_persist(&binding(), exhausted, &[], NOW, &options(true)),
        Err(AdmissionError::SequenceExhausted)
    ));
    assert_eq!(fs::read(&f.path).unwrap(), before);
}
#[test]
fn b2_duplicate_or_future_metadata_is_not_normalized_into_valid_authority() {
    for suffix in ["duplicate", "future", "counter"] {
        let f = Fixture::new(true);
        let mut store = f.open();
        let raw = crate::queue::admission_extension(&store.current)
            .unwrap()
            .unwrap();
        let changed = match suffix {
            "duplicate" => raw.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1),
            "future" => raw.replacen("\"schema\":1", "\"schema\":2", 1),
            _ => raw.replacen("\"issued_sequence\":0", "\"issued_sequence\":1", 1),
        };
        let next =
            crate::queue::replace_admission_extension(&store.current, &changed, store.limits)
                .unwrap();
        store.persist_admission_document(next).unwrap();
        let before = fs::read(&f.path).unwrap();
        assert!(matches!(
            store.active_page_receipt(&binding()),
            Err(AdmissionError::Corrupt)
        ));
        assert_eq!(fs::read(&f.path).unwrap(), before);
    }
}

#[test]
fn b2_worker_begin_chunk_ack_finish_keep_receipt_after_task_is_gone() {
    let f = Fixture::new(true);
    let mut store = f.open();
    let receipt = admit(&mut store);
    let task = store.inspect().unwrap().tasks[0].id.clone();
    let target = DestinationBinding::telegram(&key(), "synthetic", "synthetic");
    let raw = crate::queue::admission_extension(&store.current)
        .unwrap()
        .unwrap()
        .to_owned();
    store
        .freeze_telegram_progress(
            &task,
            &target,
            &[Zeroizing::new("synthetic rendered chunk".into())],
        )
        .unwrap();
    assert_eq!(
        crate::queue::admission_extension(&store.current)
            .unwrap()
            .unwrap(),
        raw
    );
    let token = store
        .begin_destination_attempt(&task, ForwardingChannel::Telegram, true, &target, NOW)
        .unwrap();
    store
        .checkpoint_telegram_chunk(&task, true, &target, 0, true, NOW, &token)
        .unwrap();
    store
        .checkpoint_telegram_chunk(&task, true, &target, 1, false, NOW, &token)
        .unwrap();
    store
        .finish_destination_attempt(
            &task,
            ForwardingChannel::Telegram,
            &target,
            NOW,
            &DeliveryOutcome::Delivered,
            &token,
        )
        .unwrap();
    assert_eq!(store.inspect().unwrap().tasks.len(), 0);
    assert_eq!(
        crate::queue::admission_extension(&store.current)
            .unwrap()
            .unwrap(),
        raw
    );
    assert_eq!(admit(&mut store), receipt);
    assert_eq!(store.inspect().unwrap().tasks.len(), 0);
}

#[test]
fn b2_receipt_identity_binds_cursor_disposition_and_counts() {
    for field in ["to_cursor", "disposition", "queued"] {
        let f = Fixture::new(true);
        let mut store = f.open();
        admit(&mut store);
        let raw = crate::queue::admission_extension(&store.current)
            .unwrap()
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(raw).unwrap();
        value["active"][field] = match field {
            "to_cursor" => json!(999),
            "disposition" => json!("disabled_skip"),
            _ => json!(0),
        };
        let changed = serde_json::to_string(&value).unwrap();
        let next =
            crate::queue::replace_admission_extension(&store.current, &changed, store.limits)
                .unwrap();
        store.persist_admission_document(next).unwrap();
        assert!(matches!(
            store.active_page_receipt(&binding()),
            Err(AdmissionError::Corrupt)
        ));
    }
}
