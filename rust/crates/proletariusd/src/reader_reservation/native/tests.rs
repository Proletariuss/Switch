use super::*;

const READER: &str = "BLE:73B7446E-E6CE-C90C-C844-485389FE8C1D";
const PRODUCER: &str = "producer-00000000000000000000000000000001";

fn fixture() -> Arc<Gate> {
    let gate = Arc::new(Gate::default());
    gate.initialize_business_producer(PRODUCER);
    gate.test_seed_selection(READER, 7);
    gate
}

#[test]
fn pin_owns_identity_before_rpc_and_excludes_all_other_admission() {
    let gate = fixture();
    let binding = gate
        .begin_native_pin(READER, NativePinPolicy::CardOnly)
        .unwrap();
    assert_eq!(binding.lease_id.len(), 64);
    assert_eq!(binding.producer_id, PRODUCER);
    assert_eq!(binding.reader_generation, 7);
    assert_eq!(binding.lease_sequence, 1);
    assert_eq!(gate.validate_native_pin(&binding, "pin"), Ok(()));
    assert!(gate.track_native_worker(&binding).is_err());
    assert!(
        gate.begin_native_pin(READER, NativePinPolicy::Identity)
            .is_err()
    );
    assert!(gate.admit(READER).is_err());
    assert!(
        gate.acquire_ble_control("control-000000000000000000000000000000")
            .is_err()
    );
    assert_eq!(
        gate.acquire_operation("operation-00000000000000000000000", READER, 7),
        Err("card_busy")
    );
    let status = gate
        .test_business_command(json!({"action":"status","producer_id":PRODUCER}), false)
        .unwrap();
    assert_eq!(status["native_session_active"], true);
    assert_eq!(gate.test_business_command(json!({"action":"acquire","producer_id":PRODUCER,"token":"go-owner-000000000000000000000000000","sequence":1,"role":"operation"}), false), Err("card_busy"));
}

#[test]
fn cancellation_before_pin_ack_installs_cleanup_authority_but_cannot_revive() {
    let gate = fixture();
    let binding = gate
        .begin_native_pin(READER, NativePinPolicy::Identity)
        .unwrap();
    gate.drain_native_pin(&binding).unwrap();
    assert_eq!(gate.validate_native_pin(&binding, "pin"), Err("card_busy"));
    assert_eq!(gate.activate_native_pin(&binding), Err("owner_expired"));
    assert_eq!(gate.validate_native_pin(&binding, "unpin"), Ok(()));
    gate.finish_native_pin(&binding).unwrap();
    assert_eq!(
        gate.validate_native_pin(&binding, "unpin"),
        Err("owner_mismatch")
    );
    let next = gate
        .begin_native_pin(READER, NativePinPolicy::Identity)
        .unwrap();
    assert_eq!(next.lease_sequence, binding.lease_sequence + 1);
    assert!(next.lease_id != binding.lease_id);
    assert_eq!(gate.drain_native_pin(&binding), Err("owner_mismatch"));
    assert_eq!(gate.validate_native_pin(&next, "pin"), Ok(()));
}

#[test]
fn worker_survives_go_exit_and_blocks_replacement_until_actual_completion() {
    let gate = fixture();
    let binding = gate
        .begin_native_pin(READER, NativePinPolicy::CardOnly)
        .unwrap();
    gate.activate_native_pin(&binding).unwrap();
    let worker = gate.track_native_worker(&binding).unwrap();
    gate.test_confirm_go_exit("producer-00000000000000000000000000000002");
    assert!(!gate.native_pin_producer_current(&binding));
    assert!(!gate.native_workers_idle(&binding));
    assert_eq!(gate.finish_native_pin(&binding), Err("card_busy"));
    assert!(gate.track_native_worker(&binding).is_err());
    gate.test_seed_selection(READER, 9);
    assert!(
        gate.begin_native_pin(READER, NativePinPolicy::CardOnly)
            .is_err()
    );
    drop(worker);
    assert!(gate.native_workers_idle(&binding));
    gate.finish_native_pin(&binding).unwrap();
    let next = gate
        .begin_native_pin(READER, NativePinPolicy::CardOnly)
        .unwrap();
    assert_eq!(next.lease_sequence, 2);
    assert_eq!(next.reader_generation, 9);
}

#[test]
fn detached_transport_proof_prevents_release_after_outer_worker_ends() {
    let gate = fixture();
    let binding = gate
        .begin_native_pin(READER, NativePinPolicy::Mutation)
        .unwrap();
    gate.activate_native_pin(&binding).unwrap();
    let outer = gate.track_native_worker(&binding).unwrap();
    let transport = gate.track_ble_io(READER).unwrap();
    gate.drain_native_pin(&binding).unwrap();
    drop(outer);
    assert!(!gate.native_workers_idle(&binding));
    assert_eq!(
        gate.validate_native_pin(&binding, "unpin"),
        Err("card_busy")
    );
    assert_eq!(gate.finish_native_pin(&binding), Err("card_busy"));
    drop(transport);
    assert_eq!(gate.validate_native_pin(&binding, "unpin"), Ok(()));
    gate.finish_native_pin(&binding).unwrap();
}

#[test]
fn finalizer_retains_authority_during_drain_but_excludes_new_card_work() {
    let gate = fixture();
    let binding = gate
        .begin_native_pin(READER, NativePinPolicy::Mutation)
        .unwrap();
    gate.activate_native_pin(&binding).unwrap();
    let worker = gate.track_native_worker(&binding).unwrap();
    assert_eq!(gate.begin_native_finalize(&binding), Err("card_busy"));
    drop(worker);
    gate.begin_native_finalize(&binding).unwrap();
    gate.drain_native_pin(&binding).unwrap();
    assert_eq!(gate.validate_native_pin(&binding, "finalize"), Ok(()));
    assert!(gate.track_native_worker(&binding).is_err());
    assert_eq!(
        gate.validate_native_pin(&binding, "unpin"),
        Err("card_busy")
    );
    assert_eq!(gate.finish_native_pin(&binding), Err("card_busy"));
    gate.finish_native_finalize(&binding).unwrap();
    assert_eq!(
        gate.validate_native_pin(&binding, "finalize"),
        Err("card_busy")
    );
    gate.finish_native_pin(&binding).unwrap();
}

#[test]
fn changed_fingerprint_or_delayed_activation_cannot_borrow_another_pin() {
    let gate = fixture();
    let binding = gate
        .begin_native_pin(READER, NativePinPolicy::Identity)
        .unwrap();
    let mut changed = binding.clone();
    changed.policy = NativePinPolicy::Mutation;
    assert_eq!(
        gate.validate_native_pin(&changed, "pin"),
        Err("owner_mismatch")
    );
    assert_eq!(gate.drain_native_pin(&changed), Err("owner_mismatch"));
    gate.test_seed_selection(READER, 8);
    assert_eq!(
        gate.activate_native_pin(&binding),
        Err("stale_reader_generation")
    );
    gate.drain_native_pin(&binding).unwrap();
    gate.finish_native_pin(&binding).unwrap();
}

#[test]
fn lifecycle_authority_requires_exact_native_finalizer_and_ends_with_receipt() {
    let gate = fixture();
    let binding = gate
        .begin_native_pin(READER, NativePinPolicy::Mutation)
        .unwrap();
    gate.activate_native_pin(&binding).unwrap();
    let matches = || {
        gate.0.lock().unwrap().native.matches_lifecycle(
            PRODUCER,
            READER,
            &binding.lease_id,
            binding.lease_sequence,
            "native_session",
        )
    };
    assert!(!matches());
    gate.begin_native_finalize(&binding).unwrap();
    assert!(matches());
    assert!(!gate.0.lock().unwrap().native.matches_lifecycle(
        PRODUCER,
        "other",
        &binding.lease_id,
        binding.lease_sequence,
        "native_session"
    ));
    gate.finish_native_finalize(&binding).unwrap();
    assert!(!matches());
}

#[test]
fn mutation_preparation_rejects_unrelated_operations_multistandby_and_card_owner() {
    for role in ["operation", "multistandby"] {
        let gate = fixture();
        let mut capability = json!({"action":"acquire","producer_id":PRODUCER,"token":"preexisting-business-00000000000000001","sequence":1,"role":role});
        if role == "multistandby" {
            capability["reader"] = json!(READER);
            capability["reader_generation"] = json!(7);
        }
        gate.test_business_command(capability, false).unwrap();
        assert!(
            gate.begin_native_pre_admission(READER, NativePinPolicy::Mutation)
                .is_err()
        );
    }
    let gate = fixture();
    let pin = gate
        .begin_native_pin(READER, NativePinPolicy::CardOnly)
        .unwrap();
    gate.activate_native_pin(&pin).unwrap();
    assert!(
        gate.begin_native_pre_admission(READER, NativePinPolicy::Mutation)
            .is_err()
    );
}
#[test]
fn retained_preparation_cannot_steal_identity_lease_and_old_workers_still_drain() {
    let gate = fixture();
    let old = gate
        .begin_native_pin(READER, NativePinPolicy::Identity)
        .unwrap();
    gate.activate_native_pin(&old).unwrap();
    let worker = gate.track_native_worker(&old).unwrap();
    let pre = gate
        .begin_native_pre_admission(READER, NativePinPolicy::Mutation)
        .unwrap();
    assert!(gate.begin_native_pin_from_pre(&pre).is_err());
    assert!(
        gate.begin_native_pre_admission(READER, NativePinPolicy::Mutation)
            .is_err()
    );
    gate.drain_native_pin(&old).unwrap();
    assert_eq!(gate.finish_native_pin(&old), Err("card_busy"));
    drop(worker);
    gate.finish_native_pin(&old).unwrap();
    let new = gate.begin_native_pin_from_pre(&pre).unwrap();
    assert_ne!(new.lease_id, old.lease_id);
    assert_eq!(new.reader_generation, old.reader_generation);
    assert_eq!(new.policy, NativePinPolicy::Mutation);
}
