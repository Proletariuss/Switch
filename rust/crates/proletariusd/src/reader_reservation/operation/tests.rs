use super::*;

const READER: &str = "BLE:12345678-1234-1234-1234-123456789abc";
const PRODUCER: &str = "producer-0123456789abcdef0123456789";
fn fixture() -> (Arc<Gate>, OperationBinding) {
    let gate = Arc::new(Gate::default());
    gate.initialize_business_producer(PRODUCER);
    lifecycle::seed_test_selection(&gate, READER, 4);
    let b = OperationBinding {
        producer_id: PRODUCER.into(),
        operation_token: "operation-0123456789abcdef0123456789".into(),
        operation_sequence: 1,
        reader: READER.into(),
        reader_generation: 4,
        purpose: "card".into(),
        target_reader: None,
        authority: Authority::Idle {},
    };
    (gate, b)
}
fn tx(b: &OperationBinding) -> CardTransaction {
    CardTransaction::Operation { binding: b.clone() }
}
fn assert_error<T>(result: Result<T, &'static str>, error: &str) {
    assert_eq!(result.err(), Some(error));
}

#[test]
fn exact_scope_and_serial_jobs_replace_ambient_owner_bypass() {
    let (gate, b) = fixture();
    gate.operation_acquire(&b).unwrap();
    let permit = gate.track_card_io(&tx(&b), PRODUCER, READER).unwrap();
    assert_error(gate.track_card_io(&tx(&b), PRODUCER, READER), "card_busy");
    assert_error(
        gate.track_card_io(&tx(&b), PRODUCER, "Other reader"),
        "stale_reader_generation",
    );
    let mut forged = b.clone();
    forged.operation_sequence += 1;
    assert_error(
        gate.track_card_io(&tx(&forged), PRODUCER, READER),
        "owner_mismatch",
    );
    drop(permit);
    assert!(gate.track_card_io(&tx(&b), PRODUCER, READER).is_ok());
}

#[test]
fn busy_acquire_can_retry_same_binding_but_release_before_acquire_is_terminal() {
    let (gate, b) = fixture();
    let busy = gate.admit(READER).unwrap();
    assert_error(gate.operation_acquire(&b), "card_busy");
    drop(busy);
    assert!(gate.operation_acquire(&b).is_ok());
    let mut late = b.clone();
    late.operation_token.push('x');
    late.operation_sequence += 1;
    assert_eq!(
        gate.operation_command(&late, "release").unwrap()["phase"],
        "released"
    );
    assert_eq!(
        gate.operation_acquire(&late).unwrap().0["phase"],
        "released"
    );
    assert_error(
        gate.track_card_io(&tx(&late), PRODUCER, READER),
        "owner_expired",
    );
}

#[test]
fn renewal_cannot_revive_expired_or_draining_work() {
    let (gate, b) = fixture();
    gate.operation_acquire(&b).unwrap();
    gate.0
        .lock()
        .unwrap()
        .operations
        .entries
        .get_mut(&b.operation_token)
        .unwrap()
        .deadline = Instant::now();
    assert_error(
        gate.track_card_io(&tx(&b), PRODUCER, READER),
        "owner_expired",
    );
    assert_error(gate.operation_command(&b, "renew"), "owner_expired");
    assert_eq!(
        gate.operation_command(&b, "status").unwrap()["phase"],
        "draining"
    );
}

#[tokio::test]
async fn release_and_producer_exit_retain_actual_jobs_until_return() {
    let (gate, b) = fixture();
    gate.operation_acquire(&b).unwrap();
    let permit = gate.track_card_io(&tx(&b), PRODUCER, READER).unwrap();
    gate.operation_command(&b, "release").unwrap();
    let driver = tokio::spawn(drive(
        gate.clone(),
        Owner::new(PRODUCER.to_owned()),
        b.clone(),
    ));
    gate.test_confirm_go_exit("replacement-0123456789abcdef0123456789");
    assert!(gate.admit(READER).is_err());
    assert!(!driver.is_finished());
    drop(permit);
    tokio::time::timeout(Duration::from_secs(2), driver)
        .await
        .unwrap()
        .unwrap();
    assert!(gate.admit(READER).is_ok());
}

#[test]
fn exact_multistandby_parent_can_borrow_and_cannot_release_during_child() {
    let (gate, mut b) = fixture();
    let parent = json!({"action":"acquire","producer_id":PRODUCER,"token":"business-0123456789abcdef0123456789","sequence":1,"role":"multistandby","reader":READER,"reader_generation":4});
    gate.test_business_command(parent.clone(), false).unwrap();
    assert_error(gate.operation_acquire(&b), "card_busy");
    b.authority = Authority::Business {
        token: parent["token"].as_str().unwrap().into(),
        sequence: 1,
        role: "multistandby".into(),
    };
    gate.operation_acquire(&b).unwrap();
    assert!(gate.track_card_io(&tx(&b), PRODUCER, READER).is_ok());
    let mut release = parent;
    release["action"] = json!("release");
    assert_error(gate.test_business_command(release, false), "card_busy");
}

#[test]
fn quiesce_fences_new_work_without_cancelling_existing_scope() {
    let (gate, b) = fixture();
    gate.operation_acquire(&b).unwrap();
    assert_eq!(
        gate.quiesce_operations(PRODUCER).unwrap()["phase"],
        "pending"
    );
    assert!(gate.track_card_io(&tx(&b), PRODUCER, READER).is_ok());
    let mut next = b.clone();
    next.operation_token.push('x');
    next.operation_sequence += 1;
    assert_error(gate.operation_acquire(&next), "backend_stopping");
    assert_error(
        gate.begin_native_pin(READER, NativePinPolicy::Identity),
        "card_busy",
    );
}

fn transition(b: &OperationBinding, action: &str) -> Value {
    json!({"producer_id":PRODUCER,"action":action,"token":"transition-0123456789abcdef0123456789","sequence":1,
        "kind":"identity","expected_reader":READER,"expected_generation":4,"reader":READER,
        "authority":{"kind":"operation","token":b.operation_token,"reader":READER,"reader_generation":4},
        "previous_iccid":"8986000000000000000","current_iccid":"8986000000000000001",
        "metadata":{"iccid":"8986000000000000001"}})
}
#[test]
fn own_lifecycle_commit_advances_effective_scope_without_mutating_capability() {
    let (gate, b) = fixture();
    gate.operation_acquire(&b).unwrap();
    gate.test_lifecycle_command(transition(&b, "prepare"))
        .unwrap();
    let permit = gate.track_card_io(&tx(&b), PRODUCER, READER).unwrap();
    assert_error(
        gate.test_lifecycle_command(transition(&b, "commit")),
        "card_busy",
    );
    drop(permit);
    gate.test_lifecycle_command(transition(&b, "commit"))
        .unwrap();
    let response = gate.operation_command(&b, "status").unwrap();
    assert_eq!(response["binding"]["reader_generation"], 4);
    assert_eq!(response["effective_generation"], 5);
    assert!(gate.track_card_io(&tx(&b), PRODUCER, READER).is_ok());
}

#[tokio::test]
async fn lost_lifecycle_reply_is_retired_only_after_actual_worker() {
    let (gate, b) = fixture();
    gate.operation_acquire(&b).unwrap();
    gate.test_lifecycle_command(transition(&b, "prepare"))
        .unwrap();
    let permit = gate.track_card_io(&tx(&b), PRODUCER, READER).unwrap();
    gate.operation_command(&b, "release").unwrap();
    let driver = tokio::spawn(drive(gate.clone(), Owner::new(PRODUCER.into()), b.clone()));
    assert!(gate.selection_snapshot().pending);
    drop(permit);
    tokio::time::timeout(Duration::from_secs(2), driver)
        .await
        .unwrap()
        .unwrap();
    let snapshot = gate.selection_snapshot();
    assert!(!snapshot.pending);
    assert!(snapshot.reader.is_empty());
    assert_eq!(snapshot.generation, 5);
    assert!(
        gate.test_lifecycle_command(transition(&b, "commit"))
            .is_err()
    );
}

#[test]
fn malformed_or_cross_role_wire_does_not_create_an_owner() {
    let (gate, b) = fixture();
    let base = serde_json::to_value(b).unwrap();
    for key in [
        "producer_id",
        "operation_token",
        "operation_sequence",
        "reader",
        "reader_generation",
        "purpose",
        "authority",
    ] {
        let mut bad = base.clone();
        bad.as_object_mut().unwrap().remove(key);
        assert!(
            gate.test_operation_command("acquire", bad).is_err(),
            "{key}"
        );
    }
    let mut bad = base.clone();
    bad["authority"] = json!({"kind":"idle","token":"extra"});
    assert!(gate.test_operation_command("acquire", bad).is_err());
    assert!(gate.admit(READER).is_ok());
}

#[test]
fn cancelled_native_finalizer_keeps_exact_cleanup_authority_until_finish() {
    let (gate, _) = fixture();
    let binding = gate
        .begin_native_pin(READER, NativePinPolicy::Mutation)
        .unwrap();
    gate.activate_native_pin(&binding).unwrap();
    gate.begin_native_finalize(&binding).unwrap();
    gate.drain_native_pin(&binding).unwrap();
    let finalize = CardTransaction::Native {
        binding: binding.clone(),
        purpose: "finalize".into(),
        operation_sequence: Some(1),
    };
    let job = gate.track_card_io(&finalize, PRODUCER, READER).unwrap();
    assert_error(gate.finish_native_finalize(&binding), "card_busy");
    drop(job);
    gate.finish_native_finalize(&binding).unwrap();
    assert_error(
        gate.track_card_io(&finalize, PRODUCER, READER),
        "owner_expired",
    );
    gate.finish_native_pin(&binding).unwrap();
}

#[test]
fn native_physical_session_requires_current_go_workflow_and_exact_selected_session() {
    let (gate, mut operation) = fixture();
    operation.reader = "Fixture PCSC".into();
    gate.test_seed_selection(&operation.reader, 4);
    gate.initialize_native_reader_producer("native-physical-producer");
    let session = LifecycleBinding {
        producer: "native-physical-producer".into(),
        reader: operation.reader.clone(),
        session_id: "native-session".into(),
        session_sequence: 1,
    };
    gate.0.lock().unwrap().selection.physical = Some(session.clone());
    gate.operation_acquire(&operation).unwrap();
    let permit = gate
        .track_card_io(&tx(&operation), &session.producer, &operation.reader)
        .unwrap();
    let binding = Binding {
        producer_id: session.producer.clone(),
        reader: session.reader.clone(),
        session_id: session.session_id.clone(),
        lease_id: "fixed-go-workflow-lease".into(),
        lease_seq: 1,
    };
    assert!(gate.register_card_pcsc(&tx(&operation), &binding).is_ok());
    let mut unrelated = binding.clone();
    unrelated.session_id = "different-native-session".into();
    assert_error(
        gate.register_card_pcsc(&tx(&operation), &unrelated),
        "stale_reader_session",
    );
    let mut forged = operation.clone();
    forged.operation_sequence += 1;
    assert_error(
        gate.register_card_pcsc(&tx(&forged), &binding),
        "owner_mismatch",
    );
    drop(permit);
    gate.test_confirm_go_exit("go-next");
    assert_error(
        gate.track_card_io(&tx(&operation), &session.producer, &operation.reader),
        "stale_producer",
    );
    assert_eq!(gate.native_reader_snapshot().2, Some(session));
}
