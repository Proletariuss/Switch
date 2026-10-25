use super::*;

const PRODUCER: &str = "fixture-producer";
const BLE: &str = "BLE:73B7446E-E6CE-C90C-C844-485389FE8C1D";
const OTHER_BLE: &str = "BLE:73B7446E-E6CE-C90C-C844-485389FE8C1D-other";

fn local_gate() -> Arc<Gate> {
    let gate = Arc::new(Gate::default());
    gate.initialize_business_producer(PRODUCER);
    gate
}
fn token(sequence: u64) -> String {
    format!("{sequence:064x}")
}
fn command(
    action: &str,
    sequence: u64,
    kind: &str,
    expected: &str,
    generation: u64,
    reader: &str,
    authority: Value,
) -> Command {
    serde_json::from_value(json!({"action":action,"producer_id":PRODUCER,"token":token(sequence),"sequence":sequence,"kind":kind,"expected_reader":expected,"expected_generation":generation,"reader":reader,"authority":authority})).unwrap()
}
fn connect(sequence: u64, reader: &str) -> Command {
    command(
        "prepare",
        sequence,
        "connect",
        "",
        0,
        reader,
        json!({"kind":"idle"}),
    )
}
fn action(command: &Command, action: &str) -> Command {
    let i = command.intent().unwrap();
    Command {
        action: action.into(),
        producer_id: i.producer,
        token: Some(i.token),
        sequence: Some(i.sequence),
        kind: Some(i.kind),
        expected_reader: Some(i.expected_reader),
        expected_generation: Some(i.expected_generation),
        reader: Some(i.reader),
        authority: Some(i.authority),
        previous_iccid: i.previous_iccid,
        current_iccid: i.current_iccid,
        metadata: i.metadata,
    }
}
fn run(gate: &Gate, command: &Command) -> Result<Value, &'static str> {
    gate.lifecycle_command(command, false, None)
}
fn select_ble(gate: &Gate) {
    let q = connect(1, BLE);
    run(gate, &q).unwrap();
    run(gate, &action(&q, "commit")).unwrap();
}

#[test]
fn observation_publication_requires_current_settled_generation() {
    let gate = local_gate();
    select_ble(&gate);
    assert_eq!(gate.with_selected_generation(BLE, 1, || 42), Ok(42));
    for (reader, generation) in [(OTHER_BLE, 1), (BLE, 0), (BLE, 2)] {
        assert_eq!(
            gate.with_selected_generation(reader, generation, || panic!("stale publication")),
            Err::<(), _>("reader_generation_changed")
        );
    }
    let pending = command(
        "prepare",
        2,
        "disconnect",
        BLE,
        1,
        BLE,
        json!({"kind":"idle"}),
    );
    run(&gate, &pending).unwrap();
    assert_eq!(
        gate.with_selected_generation(BLE, 1, || panic!("pending publication")),
        Err::<(), _>("reader_generation_changed")
    );
    run(&gate, &action(&pending, "abort")).unwrap();
    assert_eq!(gate.with_selected_generation(BLE, 1, || 43), Ok(43));
    gate.0.lock().unwrap().operations.quiescing = true;
    assert_eq!(
        gate.with_selected_generation(BLE, 1, || panic!("stopping publication")),
        Err::<(), _>("reader_generation_changed")
    );
}

#[test]
fn observation_publication_holds_generation_until_durable_callback_finishes() {
    let gate = local_gate();
    select_ble(&gate);
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let (finish, wait_finish) = std::sync::mpsc::channel();
    let worker_gate = gate.clone();
    let worker = std::thread::spawn(move || {
        worker_gate.with_selected_generation(BLE, 1, || {
            entered.send(()).unwrap();
            wait_finish.recv().unwrap();
            42
        })
    });
    wait_entered
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    // A transition uses this same mutex; it cannot commit during persistence.
    assert!(matches!(
        gate.0.try_lock(),
        Err(std::sync::TryLockError::WouldBlock)
    ));
    finish.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), Ok(42));
    let transition = command(
        "prepare",
        2,
        "disconnect",
        BLE,
        1,
        BLE,
        json!({"kind":"idle"}),
    );
    run(&gate, &transition).unwrap();
    run(&gate, &action(&transition, "commit")).unwrap();
    assert_eq!(
        gate.with_selected_generation(BLE, 1, || panic!("old card publication")),
        Err::<(), _>("reader_generation_changed")
    );
}

#[test]
fn reader_metadata_is_generation_bound_and_invalidated_by_identity_commit() {
    let gate = local_gate();
    select_ble(&gate);
    let old = json!({"iccid":"8986000000000000000","imsi":"454030000000000"});
    let new = json!({"iccid":"8986000000000000001","imsi":"454030000000001"});
    let mut initial = command(
        "prepare",
        2,
        "identity",
        BLE,
        1,
        BLE,
        json!({"kind":"idle"}),
    );
    initial.previous_iccid = Some("8986000000000000000".into());
    initial.current_iccid = Some("8986000000000000000".into());
    initial.metadata = Some(old.clone());
    run(&gate, &initial).unwrap();
    run(&gate, &action(&initial, "commit")).unwrap();
    assert_eq!(gate.reader_metadata(BLE, 1), Some(old.clone()));

    let mut aborted = command(
        "prepare",
        3,
        "identity",
        BLE,
        1,
        BLE,
        json!({"kind":"idle"}),
    );
    aborted.previous_iccid = Some("8986000000000000000".into());
    aborted.current_iccid = Some("8986000000000000001".into());
    aborted.metadata = Some(new.clone());
    run(&gate, &aborted).unwrap();
    run(&gate, &action(&aborted, "abort")).unwrap();
    assert_eq!(gate.selection_snapshot().generation, 1);
    assert_eq!(gate.reader_metadata(BLE, 1), Some(old.clone()));

    let mut transition = command(
        "prepare",
        4,
        "identity",
        BLE,
        1,
        BLE,
        json!({"kind":"idle"}),
    );
    transition.previous_iccid = Some("8986000000000000000".into());
    transition.current_iccid = Some("8986000000000000001".into());
    transition.metadata = Some(new.clone());
    run(&gate, &transition).unwrap();
    assert_eq!(gate.reader_metadata(BLE, 1), None);
    run(&gate, &action(&transition, "commit")).unwrap();

    assert_eq!(gate.selection_snapshot().generation, 2);
    assert_eq!(gate.reader_metadata(BLE, 1), None);
    assert_eq!(gate.reader_metadata(BLE, 2), Some(new));
}

#[test]
fn identity_metadata_is_validated_before_transition_is_prepared() {
    let gate = local_gate();
    select_ble(&gate);
    for metadata in [
        json!({"iccid":"8986000000000000001"}),
        json!({"iccid":"8986000000000000000F"}),
        json!({"imsi":"454030000000000"}),
        json!("not-an-object"),
    ] {
        let sequence = gate.0.lock().unwrap().selection.high_water + 1;
        let mut transition = command(
            "prepare",
            sequence,
            "identity",
            BLE,
            1,
            BLE,
            json!({"kind":"idle"}),
        );
        transition.current_iccid = Some("8986000000000000000".into());
        transition.metadata = Some(metadata);
        assert_eq!(run(&gate, &transition), Err("invalid_reader_metadata"));
        assert!(!gate.selection_snapshot().pending);
    }
}

#[test]
fn rust_native_finalizer_borrows_selection_without_go_business_ownership() {
    let gate = local_gate();
    select_ble(&gate);
    let binding = gate
        .begin_native_pin(BLE, NativePinPolicy::Mutation)
        .unwrap();
    gate.activate_native_pin(&binding).unwrap();
    let idle = command("prepare", 2, "rebuild", BLE, 1, BLE, json!({"kind":"idle"}));
    assert_eq!(run(&gate, &idle), Err("card_busy"));
    let q = command(
        "prepare",
        3,
        "rebuild",
        BLE,
        1,
        BLE,
        json!({
            "kind":"business", "token":binding.lease_id,
            "sequence":binding.lease_sequence, "role":"native_session"
        }),
    );
    assert_eq!(run(&gate, &q), Err("card_busy"));
    gate.begin_native_finalize(&binding).unwrap();
    // Rejected lifecycle prepare numbers are retired: submit a fresh exact
    // transition under the already accepted native finalizer capability.
    let q = command(
        "prepare",
        4,
        "rebuild",
        BLE,
        1,
        BLE,
        q.authority
            .as_ref()
            .map(|a| {
                json!({
                    "kind":a.kind,"token":a.token,"sequence":a.sequence,"role":a.role
                })
            })
            .unwrap(),
    );
    run(&gate, &q).unwrap();
    assert!(!gate.native_workers_idle(&binding));
    assert_eq!(gate.finish_native_finalize(&binding), Err("card_busy"));
    run(&gate, &action(&q, "commit")).unwrap();
    gate.finish_native_finalize(&binding).unwrap();
    assert_eq!(gate.selection_snapshot().generation, 2);
    let worker = gate.track_native_worker(&binding).unwrap();
    gate.drain_native_pin(&binding).unwrap();
    assert_eq!(gate.finish_native_pin(&binding), Err("card_busy"));
    drop(worker);
    gate.finish_native_pin(&binding).unwrap();
}

#[test]
fn go_exit_retires_projection_but_cannot_bypass_rust_native_cleanup() {
    let gate = local_gate();
    select_ble(&gate);
    let binding = gate
        .begin_native_pin(BLE, NativePinPolicy::CardOnly)
        .unwrap();
    gate.activate_native_pin(&binding).unwrap();
    let worker = gate.track_native_worker(&binding).unwrap();
    super::super::confirmed_go_exit_on(&gate, PRODUCER);
    let q = connect(1, BLE);
    assert_eq!(run(&gate, &q), Err("card_busy"));
    assert_eq!(gate.finish_native_pin(&binding), Err("card_busy"));
    drop(worker);
    gate.finish_native_pin(&binding).unwrap();
    let q = connect(2, BLE);
    run(&gate, &q).unwrap();
    run(&gate, &action(&q, "commit")).unwrap();
}

#[test]
fn completed_native_callback_retires_uncertain_rebuild_without_replaying_card_work() {
    let gate = local_gate();
    select_ble(&gate);
    let binding = gate
        .begin_native_pin(BLE, NativePinPolicy::Mutation)
        .unwrap();
    gate.activate_native_pin(&binding).unwrap();
    gate.begin_native_finalize(&binding).unwrap();
    let q = command(
        "prepare",
        2,
        "rebuild",
        BLE,
        1,
        BLE,
        json!({
            "kind":"business", "token":binding.lease_id,
            "sequence":binding.lease_sequence, "role":"native_session"
        }),
    );
    run(&gate, &q).unwrap();
    let physical = gate.track_ble_io(BLE).unwrap();
    assert!(!gate.native_physical_workers_idle(&binding));
    assert_eq!(
        gate.retire_native_finalizer_selection(&binding),
        Err("card_busy")
    );
    drop(physical);
    assert!(!gate.native_workers_idle(&binding));
    assert!(gate.native_physical_workers_idle(&binding));
    assert_eq!(gate.retire_native_finalizer_selection(&binding), Ok(true));
    let selection = gate.selection_snapshot();
    assert!(selection.reader.is_empty());
    assert_eq!(selection.generation, 2);
    assert!(!selection.pending);
    assert_eq!(
        run(&gate, &action(&q, "commit")),
        Err("transition_finished")
    );
    assert_eq!(run(&gate, &q).unwrap()["phase"], "retired");
    assert_eq!(gate.retire_native_finalizer_selection(&binding), Ok(false));
    gate.finish_native_finalize(&binding).unwrap();
    assert!(gate.track_native_worker(&binding).is_err());
    gate.finish_native_pin(&binding).unwrap();
    // Fresh connect has to name the new empty projection. No old cached
    // reader/card identity is resurrected by completion of the failed reset.
    let q = command("prepare", 3, "connect", "", 2, BLE, json!({"kind":"idle"}));
    run(&gate, &q).unwrap();
    run(&gate, &action(&q, "commit")).unwrap();
    assert_eq!(gate.selection_snapshot().generation, 3);
}

#[test]
fn unstarted_native_finalizer_invalidates_pre_switch_identity_after_physical_completion() {
    let gate = local_gate();
    select_ble(&gate);
    let binding = gate
        .begin_native_pin(BLE, NativePinPolicy::Mutation)
        .unwrap();
    gate.activate_native_pin(&binding).unwrap();
    gate.begin_native_finalize(&binding).unwrap();
    let physical = gate.track_ble_io(BLE).unwrap();
    assert_eq!(
        gate.retire_native_finalizer_selection(&binding),
        Err("card_busy")
    );
    assert_eq!(gate.selection_snapshot().reader, BLE);
    drop(physical);
    assert_eq!(gate.retire_native_finalizer_selection(&binding), Ok(true));
    let selection = gate.selection_snapshot();
    assert!(selection.reader.is_empty());
    assert_eq!(selection.generation, 2);
    assert_eq!(gate.retire_native_finalizer_selection(&binding), Ok(false));
    gate.finish_native_finalize(&binding).unwrap();
    gate.finish_native_pin(&binding).unwrap();
    assert!(matches!(
        gate.begin_native_pin(BLE, NativePinPolicy::Identity),
        Err("stale_reader_generation")
    ));
}

#[test]
fn native_cleanup_preserves_a_commit_that_preceded_its_lost_reply() {
    let gate = local_gate();
    select_ble(&gate);
    let binding = gate
        .begin_native_pin(BLE, NativePinPolicy::Mutation)
        .unwrap();
    gate.activate_native_pin(&binding).unwrap();
    gate.begin_native_finalize(&binding).unwrap();
    let q = command(
        "prepare",
        2,
        "rebuild",
        BLE,
        1,
        BLE,
        json!({
            "kind":"business", "token":binding.lease_id,
            "sequence":binding.lease_sequence, "role":"native_session"
        }),
    );
    run(&gate, &q).unwrap();
    let receipt = run(&gate, &action(&q, "commit")).unwrap();
    assert_eq!(gate.retire_native_finalizer_selection(&binding), Ok(false));
    assert_eq!(run(&gate, &action(&q, "commit")).unwrap(), receipt);
    assert_eq!(gate.selection_snapshot().reader, BLE);
    assert_eq!(gate.selection_snapshot().generation, 2);
    gate.finish_native_finalize(&binding).unwrap();
}

#[test]
fn native_worker_requires_the_exact_acquired_authority_and_generation() {
    let gate = local_gate();
    select_ble(&gate);
    let authority = gate
        .begin_native_pin(BLE, NativePinPolicy::CardOnly)
        .unwrap();
    gate.activate_native_pin(&authority).unwrap();
    let physical = gate.track_native_worker(&authority).unwrap();
    assert_eq!(gate.begin_native_finalize(&authority), Err("card_busy"));
    drop(physical);
    for field in [
        "producer_id",
        "lease_id",
        "lease_sequence",
        "reader",
        "reader_generation",
        "policy",
    ] {
        let mut invalid = serde_json::to_value(&authority).unwrap();
        invalid[field] = match field {
            "lease_sequence" | "reader_generation" => json!(2),
            "policy" => json!("identity"),
            _ => json!("wrong"),
        };
        let invalid: NativePinBinding = serde_json::from_value(invalid).unwrap();
        assert!(gate.track_native_worker(&invalid).is_err(), "{field}");
    }
    gate.drain_native_pin(&authority).unwrap();
    gate.finish_native_pin(&authority).unwrap();
    let replacement = gate
        .begin_native_pin(BLE, NativePinPolicy::CardOnly)
        .unwrap();
    gate.activate_native_pin(&replacement).unwrap();
    // A delayed acquire response must not borrow the replacement lease.
    assert!(gate.track_native_worker(&authority).is_err());
    confirmed_go_exit_on(&gate, "next-producer");
    assert!(matches!(
        gate.track_native_worker(&authority),
        Err("stale_producer")
    ));
}

#[test]
fn physical_ble_work_prevents_business_expiry_and_logical_replacement() {
    let gate = local_gate();
    select_ble(&gate);
    business(&gate, "acquire", 1, "native_session").unwrap();
    let physical = gate.track_ble_io(BLE).unwrap();
    assert_eq!(
        business(&gate, "release", 1, "native_session"),
        Err("card_busy")
    );
    let q = command(
        "prepare",
        2,
        "rebuild",
        BLE,
        1,
        BLE,
        business_authority(1, "native_session"),
    );
    assert_eq!(run(&gate, &q), Err("card_busy"));
    assert!(gate.admit(BLE).is_err());
    assert!(matches!(
        gate.acquire_ble_control(&token(80)),
        Err("card_busy")
    ));
    drop(physical);
    business(&gate, "release", 1, "native_session").unwrap();
}

#[test]
fn go_exit_does_not_retire_an_unfinished_rust_ble_job() {
    let gate = local_gate();
    select_ble(&gate);
    let physical = gate.track_ble_io(BLE).unwrap();
    super::super::confirmed_go_exit_on(&gate, "next-producer");
    let status = gate
        .test_business_command(
            json!({"action":"status","producer_id":"next-producer"}),
            false,
        )
        .unwrap();
    assert_eq!(status["operation_active"], true);
    let acquire = json!({"action":"acquire","producer_id":"next-producer","token":token(80),"sequence":1,"role":"operation"});
    assert_eq!(
        gate.test_business_command(acquire.clone(), false),
        Err("card_busy")
    );
    let mut q = connect(1, BLE);
    q.producer_id = "next-producer".into();
    assert_eq!(run(&gate, &q), Err("card_busy"));
    assert!(gate.admit(BLE).is_err());
    drop(physical);
    let mut acquire = acquire;
    acquire["sequence"] = json!(2);
    acquire["token"] = json!(token(81));
    assert!(gate.test_business_command(acquire, false).is_ok());
}

#[test]
fn prepared_transition_cannot_commit_before_its_ble_job_finishes() {
    let gate = local_gate();
    select_ble(&gate);
    business(&gate, "acquire", 1, "operation").unwrap();
    let q = command(
        "prepare",
        2,
        "rebuild",
        BLE,
        1,
        BLE,
        business_authority(1, "operation"),
    );
    run(&gate, &q).unwrap();
    let physical = gate.track_ble_io(BLE).unwrap();
    assert_eq!(run(&gate, &action(&q, "commit")), Err("card_busy"));
    assert!(gate.selection_snapshot().pending);
    assert_eq!(business(&gate, "release", 1, "operation"), Err("card_busy"));
    drop(physical);
    run(&gate, &action(&q, "commit")).unwrap();
    assert_eq!(gate.selection_snapshot().generation, 2);
    business(&gate, "release", 1, "operation").unwrap();
}

#[test]
fn new_operation_requires_confirmed_selection_but_existing_capability_can_finish() {
    let gate = local_gate();
    assert_eq!(
        gate.acquire_operation(&token(90), BLE, 1),
        Err("stale_reader_generation")
    );
    select_ble(&gate);
    assert_eq!(
        gate.acquire_operation(&token(90), "Reader B", 1),
        Err("stale_reader_generation")
    );
    assert_eq!(
        gate.acquire_operation(&token(90), BLE, 2),
        Err("stale_reader_generation")
    );
    gate.acquire_operation(&token(90), BLE, 1).unwrap();
    let q = command(
        "prepare",
        2,
        "rebuild",
        BLE,
        1,
        BLE,
        json!({"kind":"operation","token":token(90),"reader":BLE,"reader_generation":1}),
    );
    run(&gate, &q).unwrap();
    run(&gate, &action(&q, "commit")).unwrap();
    assert_eq!(gate.selection_snapshot().generation, 2);
    // The exact accepted capability can retry and release after its own
    // rebuild; it cannot mint a fresh capability for the retired generation.
    gate.acquire_operation(&token(90), BLE, 1).unwrap();
    assert_eq!(
        gate.acquire_operation(&token(91), BLE, 1),
        Err("stale_reader_generation")
    );
    gate.release_operation(&token(90), BLE, 1).unwrap();
    assert_eq!(
        gate.acquire_operation(&token(90), BLE, 1),
        Err("owner_expired")
    );
    assert_eq!(
        gate.acquire_operation(&token(90), BLE, 2),
        Err("owner_expired")
    );
    gate.acquire_operation(&token(91), BLE, 2).unwrap();
    super::super::confirmed_go_exit_on(&gate, "next-producer");
    assert_eq!(
        gate.acquire_operation(&token(91), BLE, 2),
        Err("stale_reader_generation")
    );
}
fn business(gate: &Gate, action: &str, sequence: u64, role: &str) -> Result<Value, &'static str> {
    let mut q = json!({"action":action,"producer_id":PRODUCER,"token":token(sequence),"sequence":sequence,"role":role});
    if role == "multistandby" {
        q["reader"] = json!(BLE);
        q["reader_generation"] = json!(1);
    }
    gate.test_business_command(q, false)
}
fn business_authority(sequence: u64, role: &str) -> Value {
    json!({"kind":"business","token":token(sequence),"sequence":sequence,"role":role})
}
fn binding(reader: &str) -> LifecycleBinding {
    LifecycleBinding {
        producer: PRODUCER.into(),
        reader: reader.into(),
        session_id: token(900),
        session_sequence: 1,
    }
}

#[test]
fn prepared_transition_is_exclusive_and_exact_retries_increment_once() {
    let gate = local_gate();
    let q = connect(1, BLE);
    let pending = run(&gate, &q).unwrap();
    assert_eq!(pending["phase"], "prepared");
    assert_eq!(pending["snapshot"]["generation"], 0);
    assert_eq!(run(&gate, &q).unwrap(), pending);
    assert!(gate.selected_reader_generation().is_none());
    assert!(gate.admit("Reader B").is_err());
    assert_eq!(
        gate.acquire_operation(&token(8), "Reader B", 1),
        Err("card_busy")
    );
    assert!(matches!(
        gate.acquire_ble_control(&token(9)),
        Err("card_busy")
    ));
    assert_eq!(business(&gate, "acquire", 5, "operation"), Err("card_busy"));
    let result = run(&gate, &action(&q, "commit")).unwrap();
    assert_eq!(result["snapshot"]["generation"], 1);
    assert_eq!(result["snapshot"]["pending"], false);
    assert_eq!(run(&gate, &action(&q, "commit")).unwrap(), result);
    assert_eq!(run(&gate, &q).unwrap(), result);
    assert_eq!(gate.selected_reader_generation(), Some((BLE.into(), 1)));
    assert_eq!(run(&gate, &action(&q, "abort")), Err("transition_finished"));
}

#[test]
fn abort_before_prepare_fences_delayed_work_and_stopping_allows_cleanup() {
    let gate = local_gate();
    let q = connect(1, BLE);
    let aborted = run(&gate, &action(&q, "abort")).unwrap();
    assert_eq!(aborted["phase"], "aborted");
    assert_eq!(run(&gate, &q).unwrap(), aborted);
    assert_eq!(
        run(&gate, &action(&q, "commit")),
        Err("transition_finished")
    );
    assert_eq!(gate.selection_snapshot().generation, 0);
    let next = connect(2, BLE);
    assert_eq!(
        gate.lifecycle_command(&next, true, None),
        Err("backend_stopping")
    );
    run(&gate, &next).unwrap();
    assert!(
        gate.lifecycle_command(&action(&next, "abort"), true, None)
            .is_ok()
    );
    let last = connect(3, BLE);
    run(&gate, &last).unwrap();
    assert!(
        gate.lifecycle_command(&action(&last, "commit"), true, None)
            .is_ok()
    );
}

#[test]
fn changed_payload_sequence_and_old_receipts_cannot_replace_new_selection() {
    let gate = local_gate();
    let q = connect(1, BLE);
    run(&gate, &q).unwrap();
    let mut changed = action(&q, "prepare");
    changed.reader = Some("Reader B".into());
    assert_eq!(run(&gate, &changed), Err("transition_mismatch"));
    changed = action(&q, "prepare");
    changed.token = Some(token(99));
    assert_eq!(run(&gate, &changed), Err("transition_mismatch"));
    changed = action(&q, "prepare");
    changed.sequence = Some(2);
    assert_eq!(run(&gate, &changed), Err("transition_mismatch"));
    let other = connect(10, "Reader B");
    assert_eq!(run(&gate, &other), Err("card_busy"));
    run(&gate, &action(&q, "commit")).unwrap();
    assert_eq!(run(&gate, &other), Err("transition_expired"));
    let disconnect = command(
        "prepare",
        11,
        "disconnect",
        BLE,
        1,
        BLE,
        json!({"kind":"idle"}),
    );
    run(&gate, &disconnect).unwrap();
    assert_eq!(run(&gate, &q), Err("transition_superseded"));
    run(&gate, &action(&disconnect, "commit")).unwrap();
    assert_eq!(run(&gate, &q), Err("transition_superseded"));
    assert_eq!(gate.selected_reader(), None);
    assert_eq!(gate.selection_snapshot().generation, 2);
}

#[test]
fn identity_advances_only_for_two_known_different_iccids() {
    let gate = local_gate();
    select_ble(&gate);
    let mut expected = 1;
    for (index, (old, new, advance)) in [
        (None, "8986000000000000000", false),
        (Some("8986000000000000000"), "8986000000000000000", false),
        (Some("8986000000000000000"), "8986000000000000001", true),
    ]
    .into_iter()
    .enumerate()
    {
        let mut q = command(
            "prepare",
            index as u64 + 2,
            "identity",
            BLE,
            expected,
            BLE,
            json!({"kind":"idle"}),
        );
        q.previous_iccid = old.map(str::to_owned);
        q.current_iccid = Some(new.to_owned());
        q.metadata = Some(json!({"iccid":new,"imsi":"454030000000001"}));
        run(&gate, &q).unwrap();
        run(&gate, &action(&q, "commit")).unwrap();
        expected += u64::from(advance);
        assert_eq!(gate.selection_snapshot().generation, expected);
    }
}

#[test]
fn borrowed_operation_cannot_release_during_pending_transition() {
    let gate = local_gate();
    select_ble(&gate);
    let capability = token(80);
    gate.acquire_operation(&capability, BLE, 1).unwrap();
    let mut q = command(
        "prepare",
        2,
        "identity",
        BLE,
        1,
        BLE,
        json!({"kind":"operation","token":capability,"reader":BLE,"reader_generation":1}),
    );
    q.current_iccid = Some("8986000000000000000".into());
    q.metadata = Some(json!({"iccid":"8986000000000000000"}));
    run(&gate, &q).unwrap();
    assert_eq!(
        gate.release_operation(&capability, BLE, 1),
        Err("card_busy")
    );
    assert!(gate.acquire_operation(&capability, BLE, 1).is_ok());
    assert_eq!(gate.acquire_operation(&token(81), BLE, 1), Err("card_busy"));
    run(&gate, &action(&q, "abort")).unwrap();
    gate.release_operation(&capability, BLE, 1).unwrap();
    let invalid = command(
        "prepare",
        3,
        "rebuild",
        BLE,
        1,
        BLE,
        json!({"kind":"operation","token":capability,"reader":BLE,"reader_generation":1}),
    );
    assert_eq!(run(&gate, &invalid), Err("card_busy"));
}

#[test]
fn native_business_borrows_current_worker_but_other_capabilities_cannot() {
    let gate = local_gate();
    select_ble(&gate);
    business(&gate, "acquire", 80, "native_session").unwrap();
    let worker = gate.admit(BLE).unwrap();
    let q = command(
        "prepare",
        2,
        "rebuild",
        BLE,
        1,
        BLE,
        business_authority(80, "native_session"),
    );
    run(&gate, &q).unwrap();
    assert_eq!(
        business(&gate, "release", 80, "native_session"),
        Err("card_busy")
    );
    assert!(business(&gate, "acquire", 80, "native_session").is_ok());
    run(&gate, &action(&q, "commit")).unwrap();
    drop(worker);
    business(&gate, "release", 80, "native_session").unwrap();
    assert_eq!(gate.selection_snapshot().generation, 2);
    business(&gate, "acquire", 81, "operation").unwrap();
    let worker = gate.admit(BLE).unwrap();
    let ordinary = command(
        "prepare",
        3,
        "rebuild",
        BLE,
        2,
        BLE,
        business_authority(81, "operation"),
    );
    assert_eq!(run(&gate, &ordinary), Err("card_busy"));
    drop(worker);
    business(&gate, "release", 81, "operation").unwrap();
    business(&gate, "acquire", 82, "native_session").unwrap();
    let unrelated = gate.admit("Reader B").unwrap();
    let wrong_reader = command(
        "prepare",
        4,
        "rebuild",
        BLE,
        2,
        BLE,
        business_authority(82, "native_session"),
    );
    assert_eq!(run(&gate, &wrong_reader), Err("card_busy"));
    drop(unrelated);
}

#[test]
fn nwait_and_invalid_capabilities_cannot_change_selection() {
    let gate = local_gate();
    select_ble(&gate);
    business(&gate, "acquire", 90, "multistandby").unwrap();
    let idle = command("prepare", 2, "rebuild", BLE, 1, BLE, json!({"kind":"idle"}));
    assert_eq!(run(&gate, &idle), Err("card_busy"));
    let forbidden = command(
        "prepare",
        3,
        "rebuild",
        BLE,
        1,
        BLE,
        business_authority(90, "multistandby"),
    );
    assert_eq!(run(&gate, &forbidden), Err("invalid_request"));
    business(&gate, "release", 90, "multistandby").unwrap();
    business(&gate, "acquire", 91, "operation").unwrap();
    for (sequence, role) in [(90, "operation"), (91, "native_session")] {
        let q = command(
            "prepare",
            10 + sequence,
            "rebuild",
            BLE,
            1,
            BLE,
            business_authority(sequence, role),
        );
        assert_eq!(run(&gate, &q), Err("card_busy"));
    }
}

#[test]
fn go_retirement_clears_authority_and_bootstrap_keeps_monotonic_generation() {
    let gate = local_gate();
    select_ble(&gate);
    let old = command("prepare", 2, "rebuild", BLE, 1, BLE, json!({"kind":"idle"}));
    run(&gate, &old).unwrap();
    confirmed_go_exit_on(&gate, "next-producer");
    assert_eq!(
        gate.selection_snapshot(),
        SelectionSnapshot {
            generation: 2,
            ..Default::default()
        }
    );
    assert_eq!(run(&gate, &action(&old, "commit")), Err("stale_producer"));
    let mut bootstrap = connect(1, BLE);
    bootstrap.producer_id = "next-producer".into();
    run(&gate, &bootstrap).unwrap();
    run(&gate, &action(&bootstrap, "commit")).unwrap();
    assert_eq!(gate.selection_snapshot().generation, 3);
    let mut stale = command(
        "prepare",
        2,
        "disconnect",
        BLE,
        1,
        BLE,
        json!({"kind":"idle"}),
    );
    stale.producer_id = "next-producer".into();
    assert_eq!(run(&gate, &stale), Err("stale_reader_generation"));
}

#[test]
fn retirement_retains_real_workers_until_their_physical_work_finishes() {
    let gate = local_gate();
    select_ble(&gate);
    let worker = gate.admit(BLE).unwrap();
    confirmed_go_exit_on(&gate, "next-producer");
    let mut next = connect(1, BLE);
    next.producer_id = "next-producer".into();
    assert_eq!(run(&gate, &next), Err("card_busy"));
    assert_eq!(gate.selection_snapshot().generation, 2);
    drop(worker);
    next.sequence = Some(2);
    next.token = Some(token(2));
    run(&gate, &next).unwrap();
    run(&gate, &action(&next, "commit")).unwrap();
    assert_eq!(gate.selection_snapshot().generation, 3);
}

#[test]
fn borrowed_operation_does_not_override_an_unrelated_native_business_owner() {
    let gate = local_gate();
    select_ble(&gate);
    business(&gate, "acquire", 81, "native_session").unwrap();
    gate.acquire_operation(&token(80), BLE, 1).unwrap();
    let mut q = command(
        "prepare",
        2,
        "identity",
        BLE,
        1,
        BLE,
        json!({"kind":"operation","token":token(80),"reader":BLE,"reader_generation":1}),
    );
    q.current_iccid = Some("8986000000000000000".into());
    q.metadata = Some(json!({"iccid":"8986000000000000000"}));
    assert_eq!(run(&gate, &q), Err("card_busy"));
    gate.release_operation(&token(80), BLE, 1).unwrap();
    business(&gate, "release", 81, "native_session").unwrap();
}

#[test]
fn pcsc_commit_requires_live_exact_session_and_loss_requires_confirmed_same_binding() {
    let gate = local_gate();
    let q = connect(1, "Reader A");
    let live = binding("Reader A");
    run(&gate, &q).unwrap();
    assert_eq!(
        run(&gate, &action(&q, "commit")),
        Err("physical_binding_unconfirmed")
    );
    let mut wrong = live.clone();
    wrong.producer = "retired".into();
    assert_eq!(
        gate.lifecycle_command(&action(&q, "commit"), false, Some(&wrong)),
        Err("physical_binding_unconfirmed")
    );
    gate.lifecycle_command(&action(&q, "commit"), false, Some(&live))
        .unwrap();
    let rebuild = command(
        "prepare",
        2,
        "rebuild",
        "Reader A",
        1,
        "Reader A",
        json!({"kind":"idle"}),
    );
    run(&gate, &rebuild).unwrap();
    wrong = live.clone();
    wrong.session_sequence += 1;
    assert_eq!(
        gate.lifecycle_command(&action(&rebuild, "commit"), false, Some(&wrong)),
        Err("stale_physical_binding")
    );
    gate.lifecycle_command(&action(&rebuild, "abort"), false, None)
        .unwrap();
    let lost = command(
        "prepare",
        3,
        "disconnect",
        "Reader A",
        1,
        "Reader A",
        json!({"kind":"physical_loss","reader":"Reader A","reader_generation":1,"session_id":live.session_id,"session_sequence":live.session_sequence}),
    );
    assert_eq!(run(&gate, &lost), Err("card_busy"));
    let mut lost = action(&lost, "prepare");
    lost.sequence = Some(4);
    lost.token = Some(token(4));
    assert_eq!(
        gate.lifecycle_command(&lost, false, Some(&wrong)),
        Err("card_busy")
    );
    lost.sequence = Some(5);
    lost.token = Some(token(5));
    gate.lifecycle_command(&lost, false, Some(&live)).unwrap();
    assert_eq!(
        gate.lifecycle_command(&action(&lost, "commit"), false, Some(&wrong)),
        Err("stale_physical_binding")
    );
    let result = gate
        .lifecycle_command(&action(&lost, "commit"), false, Some(&live))
        .unwrap();
    assert_eq!(result["snapshot"]["generation"], 2);
    assert_eq!(gate.replay(&action(&lost, "commit")).unwrap(), Some(result));
}

#[test]
fn overflow_is_terminal_and_stale_epoch_cannot_reset_it() {
    let gate = local_gate();
    gate.0.lock().unwrap().selection.snapshot.generation = u64::MAX;
    let q = connect(1, BLE);
    assert_eq!(run(&gate, &q), Err("generation_exhausted"));
    confirmed_go_exit_on(&gate, "next-producer");
    assert_eq!(gate.selection_snapshot().generation, u64::MAX);
    let mut q = connect(1, BLE);
    q.producer_id = "next-producer".into();
    assert_eq!(run(&gate, &q), Err("generation_exhausted"));
}

#[test]
fn schema_snapshot_and_reader_identity_are_strict() {
    let gate = local_gate();
    let status: Command =
        serde_json::from_value(json!({"action":"snapshot","producer_id":PRODUCER})).unwrap();
    assert_eq!(
        gate.replay(&status).unwrap().unwrap(),
        json!({"ok":true,"snapshot":{"reader":"","generation":0,"connected_at_ms":0,"pending":false}})
    );
    let polluted: Command = serde_json::from_value(
        json!({"action":"snapshot","producer_id":PRODUCER,"token":token(8)}),
    )
    .unwrap();
    assert_eq!(gate.replay(&polluted), Err("invalid_request"));
    assert!(
        serde_json::from_value::<Command>(
            json!({"action":"snapshot","producer_id":PRODUCER,"reader_generation":1})
        )
        .is_err()
    );
    for reader in ["", "   ", "Reader\0A", OTHER_BLE] {
        assert_eq!(run(&gate, &connect(1, reader)), Err("invalid_request"));
    }
    let mut invalid = connect(1, BLE);
    invalid.authority.as_mut().unwrap().session_sequence = Some(1);
    assert_eq!(run(&gate, &invalid), Err("invalid_request"));
    invalid = connect(1, BLE);
    invalid.token = Some("short".into());
    assert_eq!(run(&gate, &invalid), Err("invalid_request"));
    invalid = connect(1, BLE);
    invalid.sequence = Some(0);
    assert_eq!(run(&gate, &invalid), Err("invalid_request"));
}
