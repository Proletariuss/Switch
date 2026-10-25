use super::*;
fn setup() -> (Arc<Gate>, LifecycleBinding) {
    let gate = Arc::new(Gate::default());
    gate.initialize_native_reader_producer("native");
    gate.initialize_business_producer("go");
    gate.initialize_native_lifecycle("native").unwrap();
    let physical = LifecycleBinding {
        producer: "native".into(),
        reader: "Reader".into(),
        session_id: "session".into(),
        session_sequence: 1,
    };
    let connect = gate
        .prepare_native_transition(
            "native",
            &SelectionSnapshot::default(),
            NativeTransitionKind::Connect,
            "Reader",
            NativeAuthority::Idle,
        )
        .unwrap();
    gate.commit_native_transition(&connect, Some(&physical), facts("8986000000000000000"))
        .unwrap();
    (gate, physical)
}
fn facts(iccid: &str) -> NativeReaderFacts {
    NativeReaderFacts {
        metadata: Some(json!({"iccid":iccid})),
        ..Default::default()
    }
}
fn operation(gate: &Gate, multi: bool) -> CardTransaction {
    let authority = if multi {
        gate.test_business_command(json!({"action":"acquire","producer_id":"go","token":"business-token-0000000000000000000000000000","sequence":1,"role":"multistandby","reader":"Reader","reader_generation":1}),false).unwrap();
        json!({"kind":"business","token":"business-token-0000000000000000000000000000","sequence":1,"role":"multistandby"})
    } else {
        json!({"kind":"idle"})
    };
    let binding = json!({"producer_id":"go","operation_token":"operation-token-000000000000000000000000000","operation_sequence":1,"reader":"Reader","reader_generation":1,"purpose":"card","authority":authority});
    gate.test_operation_command("acquire", binding.clone())
        .unwrap();
    serde_json::from_value(json!({"kind":"operation","binding":binding})).unwrap()
}
#[test]
fn multistandby_workflow_publication_requires_live_exact_transaction_and_retained_job() {
    let (gate, _) = setup();
    let tx = operation(&gate, true);
    assert!(gate.validate_card_workflow(&tx, "Reader", 1).is_ok());
    assert!(gate.validate_card_workflow(&tx, "Other", 1).is_err());
    assert!(gate.validate_card_workflow(&tx, "Reader", 2).is_err());
    assert!(
        gate.publish_multistandby_metadata(&tx, "Reader", 1, facts("new"))
            .is_err()
    );
    let job = gate.track_card_io(&tx, "go", "Reader").unwrap();
    gate.publish_multistandby_metadata(&tx, "Reader", 1, facts("new"))
        .unwrap();
    let (selection, current, _) = gate.native_reader_snapshot();
    assert_eq!(selection.generation, 1);
    assert_eq!(current.unwrap().metadata.unwrap()["iccid"], "new");
    gate.test_confirm_go_exit("replacement");
    assert!(gate.validate_card_workflow(&tx, "Reader", 1).is_err());
    assert!(
        gate.publish_multistandby_metadata(&tx, "Reader", 1, facts("stale"))
            .is_err()
    );
    drop(job);
    assert_eq!(
        gate.native_reader_snapshot().1.unwrap().metadata.unwrap()["iccid"],
        "new"
    );
}

#[test]
fn ordinary_transaction_cannot_publish_multistandby_identity() {
    let (gate, _) = setup();
    let tx = operation(&gate, false);
    let _job = gate.track_card_io(&tx, "go", "Reader").unwrap();
    assert!(gate.validate_card_workflow(&tx, "Reader", 1).is_err());
    assert!(
        gate.publish_multistandby_metadata(&tx, "Reader", 1, facts("forged"))
            .is_err()
    );
}

#[test]
fn controlled_multistandby_refresh_and_rebuild_keep_only_exact_business_generation() {
    let (gate, physical) = setup();
    let tx = operation(&gate, true);
    for kind in [
        NativeTransitionKind::Rebuild,
        NativeTransitionKind::Identity,
    ] {
        let transition = gate
            .prepare_native_transition(
                "native",
                &gate.selection_snapshot(),
                kind,
                "Reader",
                NativeAuthority::Transaction(tx.clone()),
            )
            .unwrap();
        assert!(
            gate.0
                .lock()
                .unwrap()
                .selection
                .borrowed_business("business-token-0000000000000000000000000000", 1)
        );
        assert!(
            gate.0
                .lock()
                .unwrap()
                .selection
                .borrowed_operation("operation-token-000000000000000000000000000")
        );
        let after = gate
            .commit_native_transition(&transition, Some(&physical), facts("8986000000000000001"))
            .unwrap();
        assert_eq!(after.generation, 1);
    }
    let mut forged = tx.clone();
    if let CardTransaction::Operation { binding } = &mut forged {
        binding.operation_sequence = 99;
    }
    assert!(
        gate.prepare_native_transition(
            "native",
            &gate.selection_snapshot(),
            NativeTransitionKind::Rebuild,
            "Reader",
            NativeAuthority::Transaction(forged)
        )
        .is_err()
    );
}
#[test]
fn ordinary_operation_rebuild_advances_effective_generation_without_rewriting_capability() {
    let (gate, physical) = setup();
    let tx = operation(&gate, false);
    let before = gate.selection_snapshot();
    let transition = gate
        .prepare_native_transition(
            "native",
            &before,
            NativeTransitionKind::Rebuild,
            "Reader",
            NativeAuthority::Transaction(tx.clone()),
        )
        .unwrap();
    let after = gate
        .commit_native_transition(&transition, Some(&physical), facts("8986000000000000001"))
        .unwrap();
    assert_eq!(after.generation, 2);
    if let CardTransaction::Operation { binding } = &tx {
        let status = gate
            .test_operation_command("status", serde_json::to_value(binding).unwrap())
            .unwrap();
        assert_eq!(status["binding"]["reader_generation"], 1);
        assert_eq!(status["effective_generation"], 2);
    }
    let next = gate.prepare_native_transition(
        "native",
        &after,
        NativeTransitionKind::Identity,
        "Reader",
        NativeAuthority::Transaction(tx),
    );
    assert!(next.is_ok());
    gate.abort_native_transition(&next.unwrap()).unwrap();
}
#[test]
fn operation_retirement_waits_for_commit_decision_and_real_worker_then_quarantines_unknown() {
    let (gate, _) = setup();
    let tx = operation(&gate, false);
    let transition = gate
        .prepare_native_transition(
            "native",
            &gate.selection_snapshot(),
            NativeTransitionKind::Rebuild,
            "Reader",
            NativeAuthority::Transaction(tx),
        )
        .unwrap();
    let worker = gate.track_native_lifecycle_worker(&transition).unwrap();
    assert!(
        gate.0
            .lock()
            .unwrap()
            .selection
            .retire_operation("operation-token-000000000000000000000000000")
            .is_err()
    );
    drop(worker);
    assert!(
        gate.0
            .lock()
            .unwrap()
            .selection
            .retire_operation("operation-token-000000000000000000000000000")
            .is_err(),
        "worker return alone does not retire a not-yet-published result"
    );
    gate.quarantine_native_transition(&transition).unwrap();
    gate.0
        .lock()
        .unwrap()
        .selection
        .retire_operation("operation-token-000000000000000000000000000")
        .unwrap();
    assert!(!gate.selection_snapshot().pending);
    assert!(gate.selection_snapshot().reader.is_empty());
    assert!(gate.native_reader_snapshot().1.is_none());
}
#[test]
fn unknown_native_finalizer_cleanup_retires_once_without_target_confirmation() {
    let (gate, _) = setup();
    let pin = gate
        .begin_native_pin("Reader", NativePinPolicy::Mutation)
        .unwrap();
    gate.activate_native_pin(&pin).unwrap();
    gate.begin_native_finalize(&pin).unwrap();
    let tx = CardTransaction::Native {
        binding: pin.clone(),
        purpose: "finalize".into(),
        operation_sequence: Some(1),
    };
    let transition = gate
        .prepare_native_transition(
            "native",
            &gate.selection_snapshot(),
            NativeTransitionKind::Rebuild,
            "Reader",
            NativeAuthority::Transaction(tx),
        )
        .unwrap();
    let worker = gate.track_native_lifecycle_worker(&transition).unwrap();
    assert!(gate.retire_native_finalizer_selection(&pin).is_err());
    drop(worker);
    gate.quarantine_native_transition(&transition).unwrap();
    assert!(gate.retire_native_finalizer_selection(&pin).unwrap());
    assert!(!gate.retire_native_finalizer_selection(&pin).unwrap());
    assert_eq!(gate.selection_snapshot().generation, 2);
    assert!(gate.native_reader_snapshot().1.is_none());
    assert!(
        gate.commit_native_transition(&transition, None, facts("8986000000000000001"))
            .is_err()
    );
    gate.finish_native_finalize(&pin).unwrap();
}
