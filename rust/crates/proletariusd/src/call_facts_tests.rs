use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Barrier, mpsc};
use std::thread;
use std::time::Duration;

static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub(super) struct Directory(PathBuf);
impl Directory {
    pub(super) fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "switch-call-facts-{}-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos(),
            DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    pub(super) fn facts(&self) -> CallFacts {
        CallFacts::new(
            ArchiveStore::new(self.0.join("calls.json")),
            "producer-1".into(),
        )
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fact(source: Source, sequence: u64, action: Action) -> Observation {
    Observation {
        producer_id: "producer-1".into(),
        source,
        sequence,
        owner_id: "owner-1".into(),
        owner_epoch: 1,
        observed_at: format!("2026-10-05T12:00:{:02}.123456789Z", sequence),
        action,
        direction: Some(Direction::Outgoing),
        number: Some("+441111".into()),
        call_id: None,
        state: None,
    }
}
fn ims(sequence: u64, id: &str, state: &str) -> Observation {
    Observation {
        call_id: Some(id.into()),
        state: Some(state.into()),
        ..fact(Source::ReaderIms, sequence, Action::Ims)
    }
}
fn module_snapshot(sequence: u64, state: &str, number: Option<&str>) -> Observation {
    Observation {
        source: Source::ModuleCs,
        action: Action::Snapshot,
        direction: None,
        state: Some(state.into()),
        number: number.map(str::to_owned),
        ..fact(Source::ModuleCs, sequence, Action::Snapshot)
    }
}
fn accepted(facts: &CallFacts, observation: Observation) {
    assert_eq!(
        facts.observe(observation.clone(), false).unwrap(),
        ack(observation.sequence)
    );
}
fn code(facts: &CallFacts, observation: Observation) -> &'static str {
    facts.observe(observation, false).unwrap_err().code
}
pub(super) fn records(facts: &CallFacts) -> Vec<Value> {
    facts.store.load().unwrap()
}

#[test]
fn cs_aggregates_once_preserving_start_direction_answer_and_nonempty_number() {
    let directory = Directory::new();
    let facts = directory.facts();
    accepted(&facts, fact(Source::ModuleCs, 1, Action::Answered));
    assert!(state_lock(&facts.sources[0]).unwrap().active.is_none());
    let begin = fact(Source::ModuleCs, 2, Action::Begin);
    accepted(&facts, begin.clone());
    accepted(&facts, begin.clone());
    let mut repeated = fact(Source::ModuleCs, 3, Action::Begin);
    repeated.number = Some("wrong-number".into());
    repeated.direction = Some(Direction::Incoming);
    accepted(&facts, repeated);
    let mut empty = fact(Source::ModuleCs, 4, Action::Number);
    empty.number = Some(String::new());
    accepted(&facts, empty);
    accepted(&facts, fact(Source::ModuleCs, 5, Action::Answered));
    let end = fact(Source::ModuleCs, 6, Action::End);
    accepted(&facts, end.clone());
    let all = records(&facts);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["started_at"], begin.observed_at);
    assert_eq!(all[0]["ended_at"], end.observed_at);
    assert_eq!(all[0]["duration"], 4);
    assert_eq!(all[0]["direction"], "out");
    assert_eq!(all[0]["number"], "+441111");
    assert_eq!(all[0]["answered"], true);
    facts.store.replace(Vec::new()).unwrap();
    accepted(&facts, end); // Cached ACK must not resurrect an explicitly cleared history.
    assert!(records(&facts).is_empty());
}

#[test]
fn anonymous_calls_omit_number_preserving_the_public_record_contract() {
    for number in [None, Some(String::new())] {
        let directory = Directory::new();
        let facts = directory.facts();
        let mut begin = fact(Source::ModuleCs, 1, Action::Begin);
        begin.number = number;
        accepted(&facts, begin);
        accepted(&facts, fact(Source::ModuleCs, 2, Action::End));
        let all = records(&facts);
        assert_eq!(all.len(), 1);
        assert!(all[0].get("number").is_none());
        assert_eq!(all[0]["device"], "module");
        assert_eq!(all[0]["direction"], "out");
        assert_eq!(all[0]["answered"], false);
        assert_eq!(all[0]["duration"], 1);
    }
}

#[test]
fn module_snapshots_drive_the_rust_call_lifecycle_and_retry_idempotently() {
    let directory = Directory::new();
    let facts = directory.facts();
    let incoming = module_snapshot(1, "incoming", None);
    accepted(&facts, incoming.clone());
    accepted(&facts, module_snapshot(2, "incoming", Some("+85256600731")));
    accepted(&facts, module_snapshot(3, "active", None));
    let idle = module_snapshot(4, "idle", None);
    accepted(&facts, idle.clone());
    accepted(&facts, idle);

    let records = records(&facts);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["direction"], "in");
    assert_eq!(records[0]["number"], "+85256600731");
    assert_eq!(records[0]["answered"], true);
    assert_eq!(records[0]["started_at"], incoming.observed_at);
}

#[test]
fn module_snapshot_rejects_other_sources_and_unknown_states_before_mutation() {
    let directory = Directory::new();
    let facts = directory.facts();
    let mut invalid_source = module_snapshot(1, "incoming", None);
    invalid_source.source = Source::ReaderIms;
    assert_eq!(code(&facts, invalid_source), "invalid_call_fact");
    let invalid_state = module_snapshot(1, "unknown", None);
    assert_eq!(code(&facts, invalid_state), "invalid_call_fact");
    let state = state_lock(&facts.sources[0]).unwrap();
    assert_eq!(state.owner_epoch, 0);
    assert!(state.last.is_none() && state.active.is_none());
}

#[test]
fn three_sources_have_independent_sequences_active_calls_and_record_ids() {
    let directory = Directory::new();
    let facts = directory.facts();
    for source in [Source::ModuleCs, Source::ModuleIms, Source::ReaderIms] {
        let mut start = if source == Source::ModuleCs {
            fact(source, 1, Action::Begin)
        } else {
            Observation {
                source,
                ..ims(1, "same-dialog", "outgoing")
            }
        };
        start.call_id = Some("same-dialog".into());
        accepted(&facts, start);
    }
    for source in [Source::ReaderIms, Source::ModuleCs, Source::ModuleIms] {
        let mut end = if source == Source::ModuleCs {
            fact(source, 2, Action::End)
        } else {
            Observation {
                source,
                ..ims(2, "same-dialog", "ended")
            }
        };
        end.call_id = Some("same-dialog".into());
        accepted(&facts, end);
    }
    let all = records(&facts);
    assert_eq!(all.len(), 3);
    let ids: std::collections::HashSet<_> = all.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(all.iter().filter(|r| r["device"] == "module").count(), 2);
    assert_eq!(all.iter().filter(|r| r["device"] == "reader").count(), 1);
}

#[test]
fn ims_active_creates_and_old_dialog_terminals_or_unknown_states_never_end_current_call() {
    let directory = Directory::new();
    let facts = directory.facts();
    accepted(&facts, ims(1, "call-a", "active"));
    accepted(&facts, ims(2, "call-a", "ended"));
    accepted(&facts, ims(3, "call-b", "incoming"));
    accepted(&facts, ims(4, "call-b", "ended"));
    accepted(&facts, ims(5, "call-c", "active"));
    accepted(&facts, ims(6, "call-a", "ended"));
    accepted(&facts, ims(7, "call-b", "unavailable"));
    accepted(&facts, ims(8, "call-c", "future-state"));
    let mut missing = ims(9, "call-c", "idle");
    missing.call_id = None;
    accepted(&facts, missing);
    assert_eq!(records(&facts).len(), 2);
    assert_eq!(
        state_lock(&facts.sources[2])
            .unwrap()
            .active
            .as_ref()
            .unwrap()
            .call_id
            .as_deref(),
        Some("call-c")
    );
    accepted(&facts, ims(10, "call-c", "ended"));
    accepted(&facts, ims(11, "call-c", "active"));
    assert!(state_lock(&facts.sources[2]).unwrap().active.is_none());
    let all = records(&facts);
    assert_eq!(all.len(), 3);
    assert_eq!(all[0]["answered"], true);
    assert_eq!(all[0]["duration"], 5);
}

#[test]
fn wrong_owner_cannot_end_current_call_or_revive_a_retired_owner() {
    let directory = Directory::new();
    let facts = directory.facts();
    accepted(&facts, fact(Source::ModuleCs, 1, Action::Begin));
    let mut premature = fact(Source::ModuleCs, 2, Action::Begin);
    premature.owner_id = "owner-2".into();
    premature.owner_epoch = 2;
    assert_eq!(code(&facts, premature), "call_owner_busy");
    accepted(&facts, fact(Source::ModuleCs, 2, Action::Retire));
    let mut next = fact(Source::ModuleCs, 3, Action::Begin);
    next.owner_id = "owner-2".into();
    next.owner_epoch = 2;
    accepted(&facts, next);
    accepted(&facts, fact(Source::ModuleCs, 4, Action::End));
    accepted(&facts, fact(Source::ModuleCs, 5, Action::Begin));
    assert_eq!(records(&facts).len(), 1);
    let state = state_lock(&facts.sources[0]).unwrap();
    assert_eq!(state.owner_id.as_deref(), Some("owner-2"));
    assert!(state.active.is_some());
}

#[test]
fn idle_or_unknown_new_owner_retires_older_generations_and_retire_prevents_same_owner_revival() {
    for state_name in ["idle", "future-state"] {
        let directory = Directory::new();
        let facts = directory.facts();
        let mut current = ims(1, "none-yet", state_name);
        current.owner_id = "owner-2".into();
        current.owner_epoch = 2;
        accepted(&facts, current.clone());
        accepted(&facts, ims(2, "old-delayed-dialog", "active"));
        assert!(state_lock(&facts.sources[2]).unwrap().active.is_none());
        let mut conflicting = current.clone();
        conflicting.sequence = 3;
        conflicting.owner_id = "different-owner-for-epoch-2".into();
        assert_eq!(code(&facts, conflicting), "call_owner_epoch_conflict");
        let retired = Observation {
            sequence: 3,
            action: Action::Retire,
            ..current.clone()
        };
        accepted(&facts, retired);
        let live = Observation {
            sequence: 4,
            state: Some("active".into()),
            ..current
        };
        accepted(&facts, live);
        assert!(state_lock(&facts.sources[2]).unwrap().active.is_none());
        assert!(records(&facts).is_empty());
    }
}

#[test]
fn sequence_retries_compare_full_payload_and_rejected_facts_do_not_consume_sequence() {
    let directory = Directory::new();
    let facts = directory.facts();
    let begin = fact(Source::ModuleCs, 1, Action::Begin);
    accepted(&facts, begin.clone());
    let mut changed = begin.clone();
    changed.observed_at = "2026-10-05T12:01:00Z".into();
    assert_eq!(code(&facts, changed), "call_sequence_conflict");
    assert_eq!(
        code(&facts, fact(Source::ModuleCs, 3, Action::End)),
        "call_sequence_out_of_order"
    );
    accepted(&facts, fact(Source::ModuleCs, 2, Action::End));
    assert_eq!(code(&facts, begin), "call_sequence_out_of_order");
    assert_eq!(records(&facts).len(), 1);
}

#[test]
fn archive_failure_retains_immutable_terminal_until_same_fact_confirms_durability() {
    let directory = Directory::new();
    let facts = directory.facts();
    accepted(&facts, fact(Source::ModuleCs, 1, Action::Begin));
    fs::create_dir(facts.store.path()).unwrap();
    let end = fact(Source::ModuleCs, 2, Action::End);
    let error = facts.observe(end.clone(), false).unwrap_err();
    assert_eq!(error.code, "call_archive_failed");
    assert!(error.retryable);
    let frozen = state_lock(&facts.sources[0])
        .unwrap()
        .frozen
        .as_ref()
        .unwrap()
        .record
        .clone();
    let mut changed = end.clone();
    changed.observed_at = "2026-10-05T12:59:59Z".into();
    assert_eq!(code(&facts, changed), "call_sequence_conflict");
    assert_eq!(
        code(&facts, fact(Source::ModuleCs, 3, Action::Begin)),
        "call_terminal_pending"
    );
    fs::remove_dir(facts.store.path()).unwrap();
    accepted(&facts, end.clone());
    accepted(&facts, end);
    assert_eq!(records(&facts), vec![frozen]);
    let state = state_lock(&facts.sources[0]).unwrap();
    assert!(state.frozen.is_none() && state.active.is_none());
}

#[test]
fn record_id_conflict_is_not_acknowledged_or_dropped() {
    let directory = Directory::new();
    let facts = directory.facts();
    accepted(&facts, fact(Source::ModuleCs, 1, Action::Begin));
    let id = state_lock(&facts.sources[0])
        .unwrap()
        .active
        .as_ref()
        .unwrap()
        .id
        .clone();
    facts
        .store
        .replace(vec![json!({"id":id,"number":"other"})])
        .unwrap();
    assert_eq!(
        code(&facts, fact(Source::ModuleCs, 2, Action::End)),
        "call_event_conflict"
    );
    assert!(state_lock(&facts.sources[0]).unwrap().frozen.is_some());
    assert_eq!(records(&facts)[0]["number"], "other");
}

#[test]
fn producer_replacement_retains_frozen_but_discards_unterminated_active_without_inventing_end() {
    let directory = Directory::new();
    let facts = directory.facts();
    accepted(&facts, ims(1, "frozen-call", "active"));
    let mut still_active = ims(1, "still-active", "active");
    still_active.source = Source::ModuleIms;
    accepted(&facts, still_active);
    fs::create_dir(facts.store.path()).unwrap();
    let end = ims(2, "frozen-call", "ended");
    assert_eq!(code(&facts, end.clone()), "call_archive_failed");
    facts.replace_producer("producer-2".into()).unwrap();
    assert_eq!(code(&facts, end), "call_producer_retired");
    assert!(state_lock(&facts.sources[1]).unwrap().active.is_none());
    fs::remove_dir(facts.store.path()).unwrap();
    let mut next = ims(1, "next-call", "active");
    next.producer_id = "producer-2".into();
    accepted(&facts, next);
    assert_eq!(records(&facts).len(), 1);
    assert!(
        records(&facts)[0]["id"]
            .as_str()
            .unwrap()
            .contains("producer-1")
    );
    assert_eq!(
        state_lock(&facts.sources[2])
            .unwrap()
            .last
            .as_ref()
            .unwrap()
            .sequence,
        1
    );
    facts.finish_pending().unwrap();
    assert_eq!(records(&facts).len(), 1);
}

#[test]
fn replace_during_actual_commit_does_not_wait_for_io_or_overwrite_new_epoch() {
    let directory = Directory::new();
    let facts = directory.facts();
    accepted(&facts, ims(1, "call", "active"));
    let (send, entered) = mpsc::sync_channel(1);
    let barrier = Arc::new(Barrier::new(2));
    let wait = barrier.clone();
    *facts.before_ingest.lock().unwrap() = Some(Arc::new(move || {
        send.send(()).unwrap();
        wait.wait();
    }));
    let worker = facts.clone();
    let joining = thread::spawn(move || worker.observe(ims(2, "call", "ended"), false));
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    facts.replace_producer("producer-2".into()).unwrap();
    assert!(state_lock(&facts.sources[2]).unwrap().frozen.is_some());
    barrier.wait();
    assert_eq!(
        joining.join().unwrap().unwrap_err().code,
        "call_producer_retired"
    );
    *facts.before_ingest.lock().unwrap() = None;
    let state = state_lock(&facts.sources[2]).unwrap();
    assert_eq!(state.producer_id, "producer-2");
    assert!(state.last.is_none() && state.active.is_none() && state.frozen.is_none());
    drop(state);
    assert_eq!(records(&facts).len(), 1);
}

#[test]
fn shutdown_drains_begin_number_answered_and_end_without_executing_physical_calls() {
    let directory = Directory::new();
    let facts = directory.facts();
    let begin = fact(Source::ModuleCs, 1, Action::Begin);
    assert!(facts.observe(begin.clone(), true).is_ok());
    let mut number = fact(Source::ModuleCs, 2, Action::Number);
    number.number = Some("+442222".into());
    assert!(facts.observe(number, true).is_ok());
    assert!(
        facts
            .observe(fact(Source::ModuleCs, 3, Action::Answered), true)
            .is_ok()
    );
    let end = fact(Source::ModuleCs, 4, Action::End);
    assert!(facts.observe(end.clone(), true).is_ok());
    assert!(facts.observe(end, true).is_ok());
    let all = records(&facts);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["number"], "+442222");
    assert_eq!(all[0]["answered"], true);
    assert_eq!(all[0]["duration"], 3);
    // This owner has only an ArchiveStore; no modem/reader/call dispatcher
    // exists in this fixture or in the production aggregation service.
    facts.replace_producer("producer-2".into()).unwrap();
    // A Go restart only retires IMS producers. Native CS keeps its identity
    // and contiguous sequence across that unrelated child replacement.
    accepted(&facts, fact(Source::ModuleCs, 5, Action::Begin));
    assert_eq!(
        state_lock(&facts.sources[0]).unwrap().producer_id,
        "producer-1"
    );
    assert_eq!(
        state_lock(&facts.sources[1]).unwrap().producer_id,
        "producer-2"
    );
    assert_eq!(records(&facts).len(), 1);
}

#[test]
fn source_action_and_live_ims_shape_are_validated_before_owner_or_sequence_mutates() {
    let directory = Directory::new();
    let facts = directory.facts();
    for source in [Source::ModuleIms, Source::ReaderIms] {
        for action in [Action::Begin, Action::Number, Action::Answered, Action::End] {
            assert_eq!(code(&facts, fact(source, 1, action)), "invalid_call_fact");
        }
    }
    let mut wrong_source = ims(1, "call", "active");
    wrong_source.source = Source::ModuleCs;
    assert_eq!(code(&facts, wrong_source), "invalid_call_fact");
    for missing in ["direction", "call_id"] {
        let mut live = ims(1, "call", "active");
        live.owner_epoch = 99;
        if missing == "direction" {
            live.direction = None;
        } else {
            live.call_id = None;
        }
        assert_eq!(code(&facts, live), "invalid_call_fact");
        let state = state_lock(&facts.sources[2]).unwrap();
        assert_eq!(state.owner_epoch, 0);
        assert!(state.last.is_none() && state.active.is_none());
    }
    accepted(&facts, ims(1, "call", "active"));
    let mut missing = ims(2, "call", "active");
    missing.direction = None;
    assert_eq!(code(&facts, missing), "invalid_call_fact");
    accepted(&facts, ims(2, "call", "ended"));
}

#[test]
fn strict_fact_decode_and_validation_rejects_unknown_fields_and_invalid_boundaries() {
    let value = json!({"producer_id":"p","source":"module_cs","sequence":1,"owner_id":"o","owner_epoch":1,"observed_at":"2026-10-05T12:00:00.123456789Z","action":"begin","direction":"out"});
    let valid: Observation = serde_json::from_value(value.clone()).unwrap();
    assert!(valid.validate().is_ok());
    let mut unknown = value.clone();
    unknown["future"] = json!(true);
    assert!(serde_json::from_value::<Observation>(unknown).is_err());
    for (field, invalid) in [
        ("sequence", json!(0)),
        ("owner_epoch", json!(0)),
        ("producer_id", json!("p".repeat(97))),
        ("owner_id", json!("\n")),
        ("observed_at", json!("yesterday")),
        ("number", json!("1".repeat(513))),
    ] {
        let mut changed = value.clone();
        changed[field] = invalid;
        assert!(
            serde_json::from_value::<Observation>(changed)
                .unwrap()
                .validate()
                .is_err(),
            "{field}"
        );
    }
    for (field, invalid) in [
        ("source", json!("other")),
        ("action", json!("other")),
        ("direction", json!("incoming")),
    ] {
        let mut changed = value.clone();
        changed[field] = invalid;
        assert!(
            serde_json::from_value::<Observation>(changed).is_err(),
            "{field}"
        );
    }
}

#[test]
fn finish_pending_retries_frozen_and_keeps_open_calls_unarchived() {
    let directory = Directory::new();
    let facts = directory.facts();
    accepted(&facts, fact(Source::ModuleCs, 1, Action::Begin));
    accepted(&facts, ims(1, "still-active", "active"));
    fs::create_dir(facts.store.path()).unwrap();
    assert_eq!(
        code(&facts, fact(Source::ModuleCs, 2, Action::Retire)),
        "call_archive_failed"
    );
    assert!(facts.finish_pending().is_err());
    fs::remove_dir(facts.store.path()).unwrap();
    facts.finish_pending().unwrap();
    facts.finish_pending().unwrap();
    assert_eq!(records(&facts).len(), 1);
    assert!(state_lock(&facts.sources[2]).unwrap().active.is_some());
}

#[tokio::test]
async fn cancelled_http_waiter_does_not_abort_worker_owned_terminal_commit() {
    let directory = Directory::new();
    let facts = directory.facts();
    accepted(&facts, fact(Source::ModuleCs, 1, Action::Begin));
    let (send, entered) = tokio::sync::oneshot::channel();
    let send = Mutex::new(Some(send));
    let (release, released) = mpsc::sync_channel(1);
    let released = Mutex::new(released);
    *facts.before_ingest.lock().unwrap() = Some(Arc::new(move || {
        send.lock().unwrap().take().unwrap().send(()).unwrap();
        released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
    }));
    let worker = facts.clone();
    let request = tokio::spawn(crate::archive_io::run(move || {
        Ok(worker.observe(fact(Source::ModuleCs, 2, Action::End), false))
    }));
    entered.await.unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    // Taking the source gate joins the real worker, not the cancelled waiter.
    let finishing = facts.clone();
    crate::archive_io::run(move || finishing.finish_pending())
        .await
        .unwrap();
    assert_eq!(records(&facts).len(), 1);
    assert!(state_lock(&facts.sources[0]).unwrap().frozen.is_none());
}

async fn private_request(facts: CallFacts, path: &str, body: Value) -> (StatusCode, Value) {
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    let (client, server) = tokio::io::duplex(32 * 1024);
    let serving = tokio::spawn(async move {
        hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(server), service_fn(move |request| {
            let facts = facts.clone();
            async move { Ok::<_, std::convert::Infallible>(handle(request, facts, false).await) }
        })).await.unwrap();
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let response = sender
        .send_request(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    drop(sender);
    connection.await.unwrap().unwrap();
    serving.await.unwrap();
    (status, body)
}

#[tokio::test]
async fn private_go_observe_cannot_take_native_cs_source() {
    let directory = Directory::new();
    let facts = directory.facts();
    let body = json!({"producer_id":"producer-1","source":"module_cs","sequence":1,"owner_id":"go-cs","owner_epoch":1,
        "observed_at":"2026-10-13T12:00:00Z","action":"begin","direction":"in","number":"123"});
    let (status, result) = private_request(facts.clone(), OBSERVE_PATH, body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(result["code"], "native_call_source_owned");
    assert!(state_lock(&facts.sources[0]).unwrap().last.is_none());
    assert!(records(&facts).is_empty());
}

#[tokio::test]
async fn private_ims_error_is_producer_fenced_and_can_clear_only_current_ims_source() {
    let directory = Directory::new();
    let facts = directory.facts();
    let report =
        json!({"producer_id":"producer-1","source":"module_ims","error":"pending durable ACK"});
    let (status, result) = private_request(facts.clone(), HISTORY_ERROR_PATH, report.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result, json!({"ok":true,"source":"module_ims"}));
    assert_eq!(
        facts.history_error().as_deref(),
        Some("module_ims: pending durable ACK")
    );
    facts.latch_error(Source::ModuleCs, "producer-1", "native sticky".into());
    let mut clearing = report.clone();
    clearing["error"] = json!("");
    assert_eq!(
        private_request(facts.clone(), HISTORY_ERROR_PATH, clearing)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        facts.history_error().as_deref(),
        Some("module_cs: native sticky")
    );
    facts.replace_producer("producer-2".into()).unwrap();
    let (status, result) = private_request(facts.clone(), HISTORY_ERROR_PATH, report.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(result["code"], "call_producer_retired");
    assert_eq!(
        facts.history_error().as_deref(),
        Some("module_cs: native sticky")
    );
    for invalid in [
        json!({"producer_id":"producer-2","source":"module_cs","error":"overwrite native"}),
        json!({"producer_id":"producer-2","source":"module_ims","error":null}),
        json!({"producer_id":"producer-2","source":"module_ims","error":"error","future":true}),
        json!({"producer_id":"producer-2","source":"module_ims","error":"x".repeat(4097)}),
    ] {
        assert_eq!(
            private_request(facts.clone(), HISTORY_ERROR_PATH, invalid)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn native_facts_latches_ims_observe_failure_without_reverse_go_rpc() {
    let directory = Directory::new();
    let facts = directory.facts();
    let invalid_sequence = json!({"producer_id":"producer-1","source":"reader_ims","sequence":2,"owner_id":"ims-owner","owner_epoch":1,
        "observed_at":"2026-10-13T12:00:00Z","action":"ims","state":"active","call_id":"call","direction":"out"});
    let (status, result) = private_request(facts.clone(), OBSERVE_PATH, invalid_sequence).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(result["code"], "call_sequence_out_of_order");
    assert!(facts.history_error().unwrap().contains("reader_ims"));
    assert!(state_lock(&facts.sources[2]).unwrap().active.is_none());
}
