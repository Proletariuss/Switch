use super::*;
use crate::call_facts::CallFacts;
use proletarius_store::ArchiveStore;
use std::{fs, path::PathBuf, time::Duration};

struct Fixture {
    state: State,
    facts: CallFacts,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "switch-native-cs-{}",
            crate::snapshot::new_instance_id()
        ));
        fs::create_dir_all(&root).unwrap();
        let facts = CallFacts::new(ArchiveStore::new(root.join("calls.json")), "go-1".into());
        let state = State::new(NativeSink::start(facts.clone(), "native-cs".into(), 128).unwrap());
        Self { state, facts, root }
    }
    async fn finish(&self, second: i64) -> Vec<Value> {
        self.state.retire(at(second)).unwrap();
        tokio::time::timeout(Duration::from_secs(3), self.state.sink().drain())
            .await
            .unwrap()
            .unwrap();
        ArchiveStore::new(self.root.join("calls.json"))
            .load()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn at(second: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(second).unwrap()
}

#[tokio::test]
async fn old_query_answer_and_hangup_cannot_hide_or_end_new_incoming() {
    let fixture = Fixture::new();
    let state = &fixture.state;
    let old = state.query_revision();
    state.apply_event(Event::Ring, at(100));
    state.apply_event(Event::Clip("123".into()), at(101));
    let result = state.apply_query(old, display("idle", ""), true, at(102));
    assert!(!result.applied && result.desired_active && !result.safe_idle);
    assert_eq!(result.display["state"], "incoming");
    assert!(!state.answer(old, at(103)).applied);
    assert!(!state.end(old, at(104)).applied);
    assert_eq!(state.snapshot().display["number"], "123");
    let records = fixture.finish(105).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["answered"], false);
}

#[tokio::test]
async fn early_connect_is_buffered_until_atd_acceptance_then_preserves_real_timestamp() {
    let fixture = Fixture::new();
    let state = &fixture.state;
    state.apply_query(state.query_revision(), display("idle", ""), true, at(100));
    let ticket = state.begin_dial(at(101)).unwrap();
    let late_query = state.query_revision();
    state.apply_event(Event::Connect, at(102));
    assert!(state.dial_current(ticket));
    assert!(
        !state
            .apply_query(late_query, display("idle", ""), true, at(103))
            .applied
    );
    let accepted = state.complete_dial(ticket, "456", at(104));
    assert!(accepted.applied && !accepted.pending_dial);
    assert_eq!(accepted.display["state"], "active");
    let records = fixture.finish(106).await;
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0]["started_at"],
        at(104)
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    );
    assert_eq!(records[0]["answered"], true);
    assert_eq!(records[0]["number"], "456");
    assert_eq!(records[0]["duration"], 2);
}

#[tokio::test]
async fn rejected_atd_or_replaced_boundary_never_creates_outgoing_history() {
    for replaced in [false, true] {
        let fixture = Fixture::new();
        let state = &fixture.state;
        state.apply_query(state.query_revision(), display("idle", ""), true, at(100));
        let ticket = state.begin_dial(at(101)).unwrap();
        state.apply_event(Event::Connect, at(102));
        if replaced {
            state.apply_event(Event::Ring, at(103));
        } else {
            assert!(state.cancel_dial(ticket).applied);
        }
        assert!(!state.complete_dial(ticket, "456", at(104)).applied);
        let records = fixture.finish(105).await;
        assert!(records.iter().all(|record| record["direction"] != "out"));
        assert_eq!(records.len(), usize::from(replaced));
    }
}

#[tokio::test]
async fn ring_cache_expires_at_ten_seconds_and_unsafe_idle_never_ends_history() {
    let fixture = Fixture::new();
    let state = &fixture.state;
    state.apply_event(Event::Ring, at(100));
    let cached = state.apply_query(state.query_revision(), display("idle", ""), true, at(109));
    assert_eq!(cached.display["state"], "incoming");
    let unsafe_idle =
        state.apply_query(state.query_revision(), display("idle", ""), false, at(110));
    assert_eq!(unsafe_idle.display["state"], "idle");
    assert!(unsafe_idle.desired_active && !unsafe_idle.safe_idle);
    let safe_idle = state.apply_query(state.query_revision(), display("idle", ""), true, at(111));
    assert!(safe_idle.safe_idle && !safe_idle.desired_active);
    let records = fixture.finish(120).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["duration"], 11);
}

#[tokio::test]
async fn clip_alone_creates_no_call_and_does_not_claim_radio() {
    let fixture = Fixture::new();
    let result = fixture
        .state
        .apply_event(Event::Clip("789".into()), at(100));
    assert!(!result.desired_active && result.display["state"] == "unknown");
    assert!(fixture.finish(101).await.is_empty());
}

#[tokio::test]
async fn binding_change_invalidates_cached_ring_and_tickets_without_inventing_terminal() {
    let fixture = Fixture::new();
    let state = &fixture.state;
    state.apply_event(Event::Ring, at(100));
    state.apply_event(Event::Clip("123".into()), at(101));
    let old = state.query_revision();
    let lost = state.invalidate_observation();
    assert!(lost.applied && lost.desired_active && !lost.safe_idle);
    assert_eq!(
        lost.display,
        json!({"state":"unknown","incoming":false,"active":false})
    );
    assert!(!state.answer(old, at(102)).applied);
    let fresh = state.apply_query(state.query_revision(), display("idle", ""), true, at(102));
    assert!(fresh.safe_idle && !fresh.desired_active);
    assert_eq!(fresh.display["state"], "idle");
    let records = fixture.finish(105).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["duration"], 2);
    assert_eq!(records[0]["answered"], false);
}

#[tokio::test]
async fn native_cs_active_call_is_preserved_across_go_producer_restart() {
    let fixture = Fixture::new();
    fixture.state.apply_event(Event::Ring, at(100));
    fixture.facts.replace_producer("go-2".into()).unwrap();
    assert_eq!(fixture.state.snapshot().display["state"], "incoming");
    fixture.state.apply_event(Event::Connect, at(101));
    fixture.state.apply_event(Event::Hangup, at(102));
    let records = fixture.finish(103).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["answered"], true);
    assert_eq!(records[0]["duration"], 2);
}
