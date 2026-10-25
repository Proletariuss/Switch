use super::*;
fn facts() -> Facts {
    Facts {
        generation: 7,
        phase: "starting".into(),
        ready: false,
        sms_ready: false,
        call_ready: false,
        cleanup_confirmed: false,
        superseded: false,
        error: None,
    }
}
#[test]
fn mutation_identity_is_required_while_read_observation_remains_optional() {
    for body in [
        br#"{}"#.as_slice(),
        br#"{"observe_operation":false,"client_request_id":"swift-1"}"#,
        br#"{"observe_operation":null,"client_request_id":"swift-1"}"#,
        br#"{"client_request_id":"swift-1"}"#,
        br#"{"observe_operation":true}"#,
        br#"{"observe_operation":true,"client_request_id":null}"#,
        br#"{"observe_operation":true,"client_request_id":""}"#,
        br#"{"observe_operation":true,"client_request_id":"bad id"}"#,
        br#"{"observe_operation":1}"#,
        br#"{"observe_operation":true,"client_request_id":"swift-1","operation_id":"a"}"#,
        br#"{"observe_operation":true,"client_request_id":"swift-1","operation_generation":"bad"}"#,
    ] {
        assert!(
            Options::body(body).is_err(),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    assert!(
        Options::body(br#"{"observe_operation":true,"client_request_id":"swift-1"}"#)
            .unwrap()
            .enabled()
    );
    for query in [
        None,
        Some("operation_generation=legacy"),
        Some("observe_operation=false"),
    ] {
        assert!(!Options::query(query).unwrap().enabled());
    }
    assert!(
        Options::query(Some("observe_operation=true"))
            .unwrap()
            .enabled()
    );
}
#[test]
fn observation_timeout_never_mints_a_cleanup_receipt_or_terminal_stop() {
    let now = Instant::now();
    let mut clock = Clock::start(now, Duration::from_secs(90));
    let projection = clock.project(now + Duration::from_secs(90), "epoch", 1, facts());
    assert!(projection.observation_expired);
    assert!(!projection.terminal && !projection.cleanup_confirmed);
    assert!(projection.outcome.is_none());
    clock.stop(now, Duration::from_secs(30), true).unwrap();
    let pending = clock.project(now + Duration::from_secs(31), "epoch", 1, facts());
    assert!(pending.observation_expired);
    assert!(!pending.terminal && pending.outcome.is_none());
}
#[test]
fn cancellation_only_completes_with_owner_cleanup_and_uses_a_new_operation_id() {
    let now = Instant::now();
    let mut clock = Clock::start(now, Duration::from_secs(90));
    let start = clock.project(now, "epoch", 1, facts());
    clock.stop(now, Duration::from_secs(30), true).unwrap();
    let pending = clock.project(now, "epoch", 1, facts());
    assert_ne!(pending.operation_id, start.operation_id);
    assert!(!pending.terminal);
    let mut cleaned = facts();
    cleaned.cleanup_confirmed = true;
    let done = clock.project(now, "epoch", 1, cleaned);
    assert!(done.terminal && done.cleanup_confirmed);
    assert_eq!(done.outcome.as_deref(), Some("cancelled"));
}
#[test]
fn old_operation_or_generation_cannot_authorize_observation_or_cancellation() {
    let now = Instant::now();
    let clock = Clock::start(now, Duration::from_secs(90));
    let projection = clock.project(now, "epoch", 1, facts());
    let valid = Options {
        observe_operation: Some(true),
        operation_id: Some(projection.operation_id.clone()),
        operation_generation: Some(7),
        client_request_id: None,
    };
    assert!(valid.validate(&projection).is_ok());
    let mut stale = valid.clone();
    stale.operation_generation = Some(6);
    assert_eq!(stale.validate(&projection), Err("vowifi_operation_stale"));
    stale = valid;
    stale.operation_id = Some("old".into());
    assert_eq!(stale.validate(&projection), Err("vowifi_operation_stale"));
}

#[test]
fn request_ledger_replays_exact_identity_and_rejects_payload_drift() {
    let now = Instant::now();
    let projection = Clock::start(now, Duration::from_secs(90)).project(now, "epoch", 4, facts());
    let receipt = Receipt::capture("swift-req-1", "fingerprint-a", &projection);
    let mut ledger = RequestLedger::default();
    ledger.insert(receipt).unwrap();

    let replay = ledger
        .lookup("swift-req-1", Some("fingerprint-a"))
        .unwrap()
        .unwrap()
        .resolve(Some(projection.clone()));
    assert_eq!(replay.operation_id, projection.operation_id);
    assert_eq!(replay.generation, projection.generation);
    assert_eq!(replay.client_request_id.as_deref(), Some("swift-req-1"));
    assert!(matches!(
        ledger.lookup("swift-req-1", Some("fingerprint-b")),
        Err("vowifi_request_id_conflict")
    ));
}

#[test]
fn rejected_receipt_survives_reopen_and_never_borrows_current_owner() {
    let root = std::env::temp_dir().join(format!(
        "vowifi-rejected-{}",
        crate::snapshot::new_instance_id()
    ));
    let mut ledger = RequestLedger::open(&root, "reader").unwrap();
    let fingerprint = "a".repeat(64);
    ledger
        .insert(Receipt::rejected(
            "original-id",
            &fingerprint,
            true,
            "no SIM",
        ))
        .unwrap();
    drop(ledger);
    let reopened = RequestLedger::open(&root, "reader").unwrap();
    let now = Instant::now();
    let mut ready = facts();
    ready.ready = true;
    ready.sms_ready = true;
    ready.call_ready = true;
    let current = Clock::start(now, Duration::ZERO).project(now, "successor", 9, ready);
    let projection = reopened
        .lookup("original-id", Some(&fingerprint))
        .unwrap()
        .unwrap()
        .resolve(Some(current));
    assert_eq!(projection.client_request_id.as_deref(), Some("original-id"));
    assert_eq!(projection.operation_id, "rejected-original-id");
    assert_eq!(projection.outcome.as_deref(), Some("failed"));
    assert!(projection.terminal);
    assert!(!projection.succeeded && !projection.cleanup_confirmed);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn persist_failure_rolls_back_memory_and_fences_future_mutations() {
    let root = std::env::temp_dir().join(format!(
        "vowifi-rollback-{}",
        crate::snapshot::new_instance_id()
    ));
    let path = root.join("vowifi-reader-request-ledger.json");
    let fingerprint = "a".repeat(64);
    let now = Instant::now();
    let projection = Clock::start(now, Duration::from_secs(90)).project(now, "epoch", 1, facts());
    let mut ledger = RequestLedger::open(&root, "reader").unwrap();
    ledger
        .insert(Receipt::capture("existing-id", &fingerprint, &projection))
        .unwrap();
    let original = ledger.entries["existing-id"].snapshot.clone();
    let original_bytes = std::fs::read(&path).unwrap();
    ledger.store = Some(
        ArchiveStore::with_limits(&path, 1, 100_000)
            .unwrap()
            .require_nonempty_ledger()
            .unwrap(),
    );
    assert_eq!(
        ledger.mark_submitted("existing-id"),
        Err("vowifi_request_ledger_unavailable")
    );
    assert_eq!(ledger.entries["existing-id"].snapshot, original);
    assert!(ledger.failed);
    assert_eq!(
        ledger.lookup("existing-id", None).unwrap_err(),
        "vowifi_request_ledger_unavailable"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
    let reopened = RequestLedger::open(&root, "reader").unwrap();
    let recovered = reopened.entries["existing-id"].snapshot.as_ref().unwrap();
    assert_eq!(
        recovered.operation_id,
        original.as_ref().unwrap().operation_id
    );
    assert_eq!(recovered.phase, "unknown");
    assert!(!recovered.terminal && !recovered.succeeded);
    assert!(reopened.entries["existing-id"].recovered);
    std::fs::remove_dir_all(root).unwrap();

    // Recovered receipts deliberately ignore later live projections. Exercise
    // observe rollback on an independent, live (not recovered) owner instead.
    let root = std::env::temp_dir().join(format!(
        "vowifi-observe-rollback-{}",
        crate::snapshot::new_instance_id()
    ));
    let path = root.join("vowifi-reader-request-ledger.json");
    let mut live = RequestLedger::open(&root, "reader").unwrap();
    live.insert(Receipt::capture("existing-id", &fingerprint, &projection))
        .unwrap();
    let original_bytes = std::fs::read(&path).unwrap();
    live.store = Some(
        ArchiveStore::with_limits(&path, 1, 100_000)
            .unwrap()
            .require_nonempty_ledger()
            .unwrap(),
    );
    let mut terminal = projection.clone();
    terminal.terminal = true;
    terminal.outcome = Some("failed".into());
    assert_eq!(
        live.observe(Some(terminal)),
        Err("vowifi_request_ledger_unavailable")
    );
    assert_eq!(live.entries["existing-id"].snapshot, original);
    assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
    assert!(live.failed);
    std::fs::remove_dir_all(root).unwrap();

    let root = std::env::temp_dir().join(format!(
        "vowifi-insert-rollback-{}",
        crate::snapshot::new_instance_id()
    ));
    let mut ledger = RequestLedger::open(&root, "reader").unwrap();
    ledger.store = Some(
        ArchiveStore::with_limits(root.join("vowifi-reader-request-ledger.json"), 1, 100_000)
            .unwrap()
            .require_nonempty_ledger()
            .unwrap(),
    );
    assert_eq!(
        ledger.insert(Receipt::capture("new-id", &fingerprint, &projection)),
        Err("vowifi_request_ledger_unavailable")
    );
    assert!(!ledger.entries.contains_key("new-id"));
    assert!(ledger.failed);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn old_request_receipt_never_borrows_successor_phase() {
    let now = Instant::now();
    let old = Clock::start(now, Duration::from_secs(90)).project(now, "epoch", 4, facts());
    let receipt = Receipt::capture("swift-req-old", "fingerprint", &old);
    let mut successor_facts = facts();
    successor_facts.generation = 8;
    successor_facts.phase = "registered".into();
    successor_facts.ready = true;
    successor_facts.sms_ready = true;
    successor_facts.call_ready = true;
    let successor =
        Clock::start(now, Duration::from_secs(90)).project(now, "epoch", 8, successor_facts);

    let resolved = receipt.resolve(Some(successor));
    assert_eq!(resolved.operation_id, old.operation_id);
    assert_eq!(resolved.generation, old.generation);
    assert_eq!(resolved.phase, "superseded");
    assert_eq!(resolved.outcome.as_deref(), Some("superseded"));
    assert!(resolved.terminal);
    assert!(!resolved.succeeded);
}

#[test]
fn durable_reader_and_module_ledger_reopen_admitted_submitted_and_terminal_without_redispatch() {
    for owner in ["reader", "module"] {
        for phase in ["admitted", "submitted", "terminal"] {
            let root = std::env::temp_dir().join(format!(
                "vowifi-ledger-{}",
                crate::snapshot::new_instance_id()
            ));
            let mut ledger = RequestLedger::open(&root, owner).unwrap();
            let now = Instant::now();
            let mut projection =
                Clock::start(now, Duration::from_secs(90)).project(now, "old-owner", 3, facts());
            projection.phase = phase.into();
            projection.terminal = phase == "terminal";
            if projection.terminal {
                projection.outcome = Some("ready".into());
                projection.succeeded = true;
            }
            let fingerprint = request_fingerprint(br#"{"enable":true,"proxy":"proxy:1080","observe_operation":true,"client_request_id":"durable-1"}"#);
            let mut dispatches = 0;
            assert!(
                ledger
                    .lookup("durable-1", Some(&fingerprint))
                    .unwrap()
                    .is_none()
            );
            ledger
                .insert(Receipt::capture("durable-1", &fingerprint, &projection))
                .unwrap();
            if phase != "admitted" {
                dispatches += 1;
            }
            drop(ledger); // Crash boundary: no terminal observation/cleanup is synthesized.
            let reopened = RequestLedger::open(&root, owner).unwrap();
            let same = request_fingerprint(br#"{ "client_request_id":"durable-1", "observe_operation":true,"proxy":"proxy:1080","enable":true }"#);
            assert_eq!(
                fingerprint, same,
                "canonical property ordering survives a GUI restart"
            );
            let receipt = reopened.lookup("durable-1", Some(&same)).unwrap().unwrap();
            let mut successor = projection.clone();
            successor.operation_id = "new-owner-successor".into();
            successor.generation += 1;
            successor.terminal = true;
            successor.succeeded = true;
            let replay = receipt.resolve(Some(successor));
            assert_eq!(replay.operation_id, projection.operation_id);
            assert_eq!(replay.generation, projection.generation);
            assert_eq!(replay.requested_enable, projection.requested_enable);
            if phase == "terminal" {
                assert_eq!(replay.phase, "terminal");
                assert!(replay.terminal && replay.succeeded);
            } else {
                assert_eq!(replay.phase, "unknown");
                assert!(!replay.terminal && !replay.succeeded && !replay.cleanup_confirmed);
            }
            assert!(matches!(
                reopened.lookup("durable-1", Some("changed-owner-payload")),
                Err("vowifi_request_id_conflict")
            ));
            assert!(dispatches <= 1);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn request_fingerprint_preserves_duplicate_compatibility_field_sequence() {
    let first = request_fingerprint(
        br#"{"enable":true,"proxy":"proxy-a.example:1080","proxy":null,"client_request_id":"same","observe_operation":true}"#,
    );
    let changed_hidden_prefix = request_fingerprint(
        br#"{"enable":true,"proxy":"proxy-b.example:1080","proxy":null,"client_request_id":"same","observe_operation":true}"#,
    );
    assert_ne!(
        first, changed_hidden_prefix,
        "a duplicate prefix retained by the business decoder must not replay another intent"
    );

    let reordered = request_fingerprint(
        br#"{"observe_operation":true,"client_request_id":"same","proxy":"proxy-a.example:1080","proxy":null,"enable":true}"#,
    );
    assert_eq!(
        first, reordered,
        "independent property order remains semantically stable"
    );

    let folded = request_fingerprint(
        br#"{"Observe_Operation":true,"Client_Request_ID":"same","PROXY":"proxy-a.example:1080","proxy":null,"ENABLE":true}"#,
    );
    assert_eq!(
        first, folded,
        "fingerprints follow the same Go-style field-name folding as the decoder"
    );
}

#[test]
fn durable_terminal_observation_is_retained_and_unreadable_store_fails_closed() {
    let root = std::env::temp_dir().join(format!(
        "vowifi-ledger-{}",
        crate::snapshot::new_instance_id()
    ));
    let mut ledger = RequestLedger::open(&root, "reader").unwrap();
    let now = Instant::now();
    let mut projection =
        Clock::start(now, Duration::from_secs(90)).project(now, "epoch", 1, facts());
    let fingerprint = request_fingerprint(br#"{"enable":true,"client_request_id":"req-1"}"#);
    ledger
        .insert(Receipt::capture("req-1", &fingerprint, &projection))
        .unwrap();
    projection.terminal = true;
    projection.outcome = Some("failed".into());
    ledger.observe(Some(projection.clone())).unwrap();
    drop(ledger);
    let reopened = RequestLedger::open(&root, "reader").unwrap();
    assert_eq!(
        reopened
            .lookup("req-1", Some(&fingerprint))
            .unwrap()
            .unwrap()
            .resolve(None)
            .outcome
            .as_deref(),
        Some("failed")
    );
    std::fs::write(root.join("vowifi-reader-request-ledger.json"), b"corrupt").unwrap();
    assert!(RequestLedger::open(&root, "reader").is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn terminal_capacity_never_evicts_old_identity_and_refuses_100001() {
    let now = Instant::now();
    let mut projection =
        Clock::start(now, Duration::from_secs(90)).project(now, "epoch", 1, facts());
    projection.terminal = true;
    projection.outcome = Some("ready".into());
    projection.succeeded = true;
    let fingerprint = "a".repeat(64);
    let mut ledger = RequestLedger::default();
    // Construct the capacity snapshot directly to avoid 100,000 fsyncs.
    // The independent archive byte limit may refuse admission earlier.
    for index in 0..100_000 {
        let id = format!("terminal-{index}");
        ledger
            .entries
            .insert(id.clone(), Receipt::capture(&id, &fingerprint, &projection));
    }
    assert_eq!(
        ledger.insert(Receipt::capture(
            "terminal-100000",
            &fingerprint,
            &projection
        )),
        Err("vowifi_request_ledger_full")
    );
    assert_eq!(ledger.entries.len(), 100_000);
    assert_eq!(
        ledger
            .lookup("terminal-0", Some(&fingerprint))
            .unwrap()
            .unwrap()
            .resolve(None)
            .outcome
            .as_deref(),
        Some("ready")
    );
    assert_eq!(
        ledger
            .lookup("terminal-0", Some(&"b".repeat(64)))
            .unwrap_err(),
        "vowifi_request_id_conflict"
    );
    ledger
        .insert(Receipt::capture("terminal-0", &fingerprint, &projection))
        .unwrap();
    assert_eq!(ledger.entries.len(), 100_000);
}

#[test]
fn reader_and_module_corrupt_reopen_and_live_admission_fail_before_dispatch() {
    for owner in ["reader", "module"] {
        for damage in [b"".as_slice(), b"null", b"[]", b"{broken", b"[{}]"] {
            let root = std::env::temp_dir().join(format!(
                "vowifi-ledger-{}",
                crate::snapshot::new_instance_id()
            ));
            let mut ledger = RequestLedger::open(&root, owner).unwrap();
            let path = root.join(format!("vowifi-{owner}-request-ledger.json"));
            let now = Instant::now();
            let projection =
                Clock::start(now, Duration::from_secs(90)).project(now, "epoch", 1, facts());
            let fingerprint = "a".repeat(64);
            ledger
                .insert(Receipt::capture("old", &fingerprint, &projection))
                .unwrap();
            std::fs::write(&path, damage).unwrap();
            assert!(RequestLedger::open(&root, owner).is_err());
            let mut dispatches = 0;
            // Both owner services require lookup/insert and mark_submitted
            // before invoking a physical dispatcher.
            if ledger.lookup("new", Some(&fingerprint)).is_ok()
                && ledger
                    .insert(Receipt::capture("new", &fingerprint, &projection))
                    .is_ok()
                && ledger.mark_submitted("new").is_ok()
            {
                dispatches += 1;
            }
            assert_eq!(dispatches, 0, "{owner}/{damage:?}");
            assert!(
                ledger
                    .insert(Receipt::capture("other", &fingerprint, &projection))
                    .is_err()
            );
            assert!(ledger.mark_submitted("old").is_err());
            assert_eq!(std::fs::read(path).unwrap(), damage);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn fresh_reader_and_module_ledgers_persist_first_request_and_concurrent_access_retains_receipts() {
    for owner in ["reader", "module"] {
        let root = std::env::temp_dir().join(format!(
            "vowifi-ledger-{}",
            crate::snapshot::new_instance_id()
        ));
        let ledger = std::sync::Arc::new(std::sync::Mutex::new(
            RequestLedger::open(&root, owner).unwrap(),
        ));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|index| {
                let (ledger, barrier) = (ledger.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let now = Instant::now();
                    let projection = Clock::start(now, Duration::from_secs(90)).project(
                        now,
                        "epoch",
                        index,
                        facts(),
                    );
                    barrier.wait();
                    ledger
                        .lock()
                        .unwrap()
                        .insert(Receipt::capture(
                            &format!("request-{index}"),
                            &"a".repeat(64),
                            &projection,
                        ))
                        .unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        drop(ledger);
        let reopened = RequestLedger::open(&root, owner).unwrap();
        assert_eq!(reopened.entries.len(), 8);
        for index in 0..8 {
            assert!(
                reopened
                    .lookup(&format!("request-{index}"), Some(&"a".repeat(64)))
                    .unwrap()
                    .is_some()
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
