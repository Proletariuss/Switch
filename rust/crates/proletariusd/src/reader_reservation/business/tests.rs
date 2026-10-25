use super::*;

const PRODUCER: &str = "go-producer-current";
fn gate_for_test() -> Arc<Gate> {
    let gate = Arc::new(Gate::default());
    gate.initialize_business_producer(PRODUCER);
    gate
}
fn command(action: &str, sequence: u64, role: &str) -> Command {
    let mut value = json!({"action":action,"producer_id":PRODUCER,
        "token":format!("reader-business-token-{sequence:032}"),"sequence":sequence,"role":role});
    if role == "multistandby" {
        value["reader"] = json!("reader-a");
        value["reader_generation"] = json!(7);
    }
    if action == "transfer" {
        value["from_role"] = json!("operation");
    }
    serde_json::from_value(value).unwrap()
}
fn run(gate: &Gate, action: &str, sequence: u64, role: &str) -> Result<Value, &'static str> {
    gate.business_command(&command(action, sequence, role), false)
}

#[test]
fn simultaneous_business_acquire_has_one_owner() {
    let gate = gate_for_test();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let threads: Vec<_> = ["operation", "native_session"]
        .into_iter()
        .enumerate()
        .map(|(i, role)| {
            let gate = gate.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                run(&gate, "acquire", i as u64 + 1, role).is_ok()
            })
        })
        .collect();
    barrier.wait();
    assert_eq!(
        threads
            .into_iter()
            .filter(|t| t.thread().id() != std::thread::current().id())
            .map(|t| t.join().unwrap() as usize)
            .sum::<usize>(),
        1
    );
}

#[test]
fn transfer_is_atomic_and_stale_defer_cannot_release_native_owner() {
    let gate = gate_for_test();
    run(&gate, "acquire", 1, "operation").unwrap();
    run(&gate, "transfer", 1, "native_session").unwrap();
    run(&gate, "transfer", 1, "native_session").unwrap();
    assert_eq!(run(&gate, "release", 1, "operation"), Err("owner_mismatch"));
    assert!(
        gate.acquire_ble_control("ble-token-000000000000000000000000000000")
            .is_err()
    );
    assert_eq!(run(&gate, "acquire", 2, "operation"), Err("card_busy"));
    run(&gate, "release", 1, "native_session").unwrap();
    run(&gate, "acquire", 3, "operation").unwrap();
    run(&gate, "release", 1, "native_session").unwrap();
    assert_eq!(run(&gate, "acquire", 4, "native_session"), Err("card_busy"));
}

#[test]
fn release_before_delayed_acquire_revokes_capability_without_ttl_or_history_eviction() {
    let gate = gate_for_test();
    run(&gate, "release", 1, "operation").unwrap();
    assert_eq!(run(&gate, "acquire", 1, "operation"), Err("owner_expired"));
    for sequence in 2..260 {
        run(&gate, "acquire", sequence, "operation").unwrap();
        run(&gate, "release", sequence, "operation").unwrap();
    }
    assert_eq!(run(&gate, "acquire", 1, "operation"), Err("owner_expired"));
    run(&gate, "release", 1, "operation").unwrap();
}

#[test]
fn exact_acquire_retry_survives_later_rejected_sequence() {
    let gate = gate_for_test();
    let acquired = run(&gate, "acquire", 1, "operation").unwrap();
    assert_eq!(run(&gate, "acquire", 2, "native_session"), Err("card_busy"));
    assert_eq!(run(&gate, "acquire", 1, "operation").unwrap(), acquired);
    run(&gate, "release", 1, "operation").unwrap();
    assert_eq!(
        run(&gate, "acquire", 2, "native_session"),
        Err("owner_expired")
    );
}

#[test]
fn multi_transfer_atomically_binds_physical_reader_and_release_waits_for_cleanup() {
    let gate = gate_for_test();
    run(&gate, "acquire", 1, "operation").unwrap();
    run(&gate, "transfer", 1, "multistandby").unwrap();
    let command = command("release", 1, "multistandby");
    assert!(gate.admit("reader-a").is_err());
    assert_eq!(
        gate.release(command.token.as_deref().unwrap()),
        Err("owner_mismatch")
    );
    let worker = gate.admit_shutdown_cleanup("reader-a").unwrap();
    assert_eq!(gate.business_command(&command, false), Err("card_busy"));
    assert_eq!(run(&gate, "release", 1, "operation"), Err("owner_mismatch"));
    drop(worker);
    gate.business_command(&command, false).unwrap();
    assert!(gate.admit("reader-a").is_ok());
    assert!(gate.admit_shutdown_cleanup("reader-a").is_err());
}

#[test]
fn failed_multi_transfer_keeps_original_operation_owner() {
    let gate = gate_for_test();
    let worker = gate.admit("reader-a").unwrap();
    run(&gate, "acquire", 1, "operation").unwrap();
    assert_eq!(run(&gate, "transfer", 1, "multistandby"), Err("card_busy"));
    assert_eq!(
        run(&gate, "release", 1, "multistandby"),
        Err("owner_mismatch")
    );
    drop(worker);
    run(&gate, "transfer", 1, "multistandby").unwrap();
}

#[test]
fn multi_binding_mismatch_never_releases_reservation() {
    let gate = gate_for_test();
    run(&gate, "acquire", 1, "multistandby").unwrap();
    let mut bad = command("release", 1, "multistandby");
    bad.reader_generation = Some(8);
    assert_eq!(gate.business_command(&bad, false), Err("owner_mismatch"));
    assert!(gate.admit("reader-a").is_err());
    run(&gate, "release", 1, "multistandby").unwrap();
}

#[test]
fn producer_replacement_rejects_old_requests_and_keeps_actual_rust_worker() {
    let gate = gate_for_test();
    run(&gate, "acquire", 1, "operation").unwrap();
    let worker = gate.admit("reader-a").unwrap();
    super::super::confirmed_go_exit_on(&gate, "replacement");
    assert_eq!(run(&gate, "release", 1, "operation"), Err("stale_producer"));
    assert_eq!(run(&gate, "acquire", 2, "operation"), Err("stale_producer"));
    let mut next = command("acquire", 1, "multistandby");
    next.producer_id = "replacement".into();
    assert_eq!(gate.business_command(&next, false), Err("card_busy"));
    drop(worker);
    next.sequence = Some(2);
    gate.business_command(&next, false).unwrap();
}

#[test]
fn go_exit_preserves_native_ble_control() {
    let gate = gate_for_test();
    let permit = gate
        .acquire_ble_control("ble-token-000000000000000000000000000000")
        .unwrap();
    super::super::confirmed_go_exit_on(&gate, "replacement");
    let mut next = command("acquire", 1, "operation");
    next.producer_id = "replacement".into();
    assert_eq!(gate.business_command(&next, false), Err("card_busy"));
    drop(permit);
    next.sequence = Some(2);
    gate.business_command(&next, false).unwrap();
}

#[test]
fn shutdown_only_allows_status_and_exact_release() {
    let gate = gate_for_test();
    run(&gate, "acquire", 1, "operation").unwrap();
    assert_eq!(
        gate.business_command(&command("transfer", 1, "native_session"), true),
        Err("backend_stopping")
    );
    gate.business_command(&command("release", 1, "operation"), true)
        .unwrap();
    assert_eq!(
        gate.business_command(&command("acquire", 2, "operation"), true),
        Err("backend_stopping")
    );
    let status: Command =
        serde_json::from_value(json!({"action":"status","producer_id":PRODUCER})).unwrap();
    assert_eq!(
        gate.business_command(&status, true).unwrap()["operation_active"],
        false
    );
}

#[test]
fn strict_command_validation_and_status_do_not_expose_tokens() {
    let gate = gate_for_test();
    run(&gate, "acquire", 1, "operation").unwrap();
    let status: Command =
        serde_json::from_value(json!({"action":"status","producer_id":PRODUCER})).unwrap();
    let view = gate.business_command(&status, false).unwrap();
    assert_eq!(view["operation_active"], true);
    assert!(view.get("token").is_none());
    for role in ["operation", "native_session", "multistandby"] {
        let mut invalid = command("acquire", 2, role);
        invalid.sequence = Some(0);
        assert_eq!(
            gate.business_command(&invalid, false),
            Err("invalid_request")
        );
    }
    assert!(
        serde_json::from_value::<Command>(
            json!({"action":"status","producer_id":PRODUCER,"bogus":true})
        )
        .is_err()
    );
    let mut invalid = command("transfer", 1, "native_session");
    invalid.from_role = Some(Role::Multistandby);
    assert_eq!(
        gate.business_command(&invalid, false),
        Err("invalid_request")
    );
}

fn configured_ordinary_gate() -> (Arc<Gate>, Arc<crate::ordinary_vowifi_admission::Gate>) {
    let physical = gate_for_test();
    let ordinary = Arc::new(crate::ordinary_vowifi_admission::Gate::default());
    ordinary.set_go_producer(PRODUCER).unwrap();
    physical.set_ordinary_admission(ordinary.clone()).unwrap();
    (physical, ordinary)
}

#[test]
fn native_intent_and_multistandby_start_have_one_atomic_winner() {
    let (physical, ordinary) = configured_ordinary_gate();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let native = {
        let ordinary = ordinary.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            ordinary
                .acquire_native_reader("native-epoch", 1, "reader-a", 7)
                .is_ok()
        })
    };
    let multi = {
        let physical = physical.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            run(&physical, "acquire", 1, "multistandby").is_ok()
        })
    };
    barrier.wait();
    assert_eq!(
        usize::from(native.join().unwrap()) + usize::from(multi.join().unwrap()),
        1
    );
}

#[test]
fn multistandby_keeps_module_coexistence_but_excludes_reader_intents() {
    let (physical, ordinary) = configured_ordinary_gate();
    let module = ordinary
        .acquire_go(PRODUCER, "module", "module-token", 1)
        .unwrap();
    run(&physical, "acquire", 1, "multistandby").unwrap();
    assert!(module.is_current());
    module.release_confirmed().unwrap();
    assert!(
        ordinary
            .acquire_go(PRODUCER, "reader", "reader-token", 2)
            .is_err()
    );
    assert!(
        ordinary
            .acquire_native_reader("native-epoch", 1, "reader-a", 7)
            .is_err()
    );
    run(&physical, "release", 1, "multistandby").unwrap();
    assert!(
        ordinary
            .acquire_native_reader("native-epoch", 2, "reader-a", 7)
            .is_ok()
    );
}

#[test]
fn multistandby_unknown_ble_work_keeps_maintenance_after_go_exit() {
    let (physical, ordinary) = configured_ordinary_gate();
    let reader = "BLE:73B7446E-E6CE-C90C-C844-485389FE8C1D";
    let input = json!({"action":"acquire","producer_id":PRODUCER,
        "token":"multi-ble-token-000000000000000000000000","sequence":1,"role":"multistandby",
        "reader":reader,"reader_generation":7});
    physical
        .test_business_command(input.clone(), false)
        .unwrap();
    let worker = physical.track_go_ble_io(reader, PRODUCER).unwrap();
    let mut release = input;
    release["action"] = json!("release");
    assert_eq!(
        physical.test_business_command(release.clone(), false),
        Err("card_busy")
    );
    physical.test_confirm_go_exit("replacement-go");
    assert!(
        ordinary
            .acquire_native_reader("native-epoch", 1, reader, 7)
            .is_err()
    );
    drop(worker);
    let current = ordinary
        .acquire_native_reader("native-epoch", 2, reader, 7)
        .unwrap();
    assert_eq!(
        physical.test_business_command(release, false),
        Err("stale_producer")
    );
    assert!(current.is_current());
}

#[test]
fn confirmed_go_exit_releases_idle_multistandby_maintenance() {
    let (physical, ordinary) = configured_ordinary_gate();
    run(&physical, "acquire", 1, "multistandby").unwrap();
    physical.test_confirm_go_exit("replacement-go");
    assert!(
        ordinary
            .acquire_native_reader("native-epoch", 1, "reader-a", 7)
            .is_ok()
    );
}

#[test]
fn failed_multistandby_transfer_preserves_operation_and_does_not_leak_fence() {
    let (physical, ordinary) = configured_ordinary_gate();
    run(&physical, "acquire", 1, "operation").unwrap();
    let worker = physical.admit("reader-a").unwrap();
    assert_eq!(
        run(&physical, "transfer", 1, "multistandby"),
        Err("card_busy")
    );
    assert!(
        ordinary
            .acquire_native_reader("native-epoch", 1, "reader-a", 7)
            .is_err()
    );
    assert_eq!(run(&physical, "release", 1, "operation"), Err("card_busy"));
    drop(worker);
    run(&physical, "transfer", 1, "multistandby").unwrap();
    assert_eq!(
        run(&physical, "release", 1, "operation"),
        Err("owner_mismatch")
    );
    run(&physical, "release", 1, "multistandby").unwrap();
    assert!(
        ordinary
            .acquire_native_reader("native-epoch", 2, "reader-a", 7)
            .is_ok()
    );
}

#[test]
fn legacy_reader_business_and_native_intent_have_one_atomic_winner() {
    for role in ["operation", "native_session"] {
        let (physical, ordinary) = configured_ordinary_gate();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let native = {
            let ordinary = ordinary.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                ordinary
                    .acquire_native_reader("native-epoch", 1, "reader-a", 7)
                    .is_ok()
            })
        };
        let legacy = {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                run(&physical, "acquire", 1, role).is_ok()
            })
        };
        barrier.wait();
        assert_eq!(
            usize::from(native.join().unwrap()) + usize::from(legacy.join().unwrap()),
            1
        );
    }
}

#[test]
fn legacy_business_blocks_native_backoff_but_preserves_go_reader_aka() {
    let (physical, ordinary) = configured_ordinary_gate();
    let reader = ordinary
        .acquire_go(PRODUCER, "reader", "reader-token", 1)
        .unwrap();
    run(&physical, "acquire", 1, "operation").unwrap();
    assert!(reader.is_current());
    // An unrelated card operation must not cause the ordinary Go reader to stop.
    assert!(
        ordinary
            .begin_reader_maintenance(
                "reader-a",
                7,
                crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain
            )
            .is_err()
    );
    run(&physical, "transfer", 1, "native_session").unwrap();
    let maintenance = ordinary
        .begin_reader_maintenance(
            "reader-a",
            7,
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain,
        )
        .unwrap();
    reader.release_confirmed().unwrap();
    assert!(!maintenance.ordinary_drained());
    assert_eq!(
        run(&physical, "release", 1, "operation"),
        Err("owner_mismatch")
    );
    run(&physical, "release", 1, "native_session").unwrap();
    assert!(maintenance.ordinary_drained());
    maintenance.release_confirmed().unwrap();
    let current = ordinary
        .acquire_native_reader("native-epoch", 1, "reader-a", 7)
        .unwrap();
    // Native intent remains exclusive even when it temporarily has no physical lease.
    assert_eq!(run(&physical, "acquire", 2, "operation"), Err("card_busy"));
    assert!(current.is_current());
}

#[test]
fn go_exit_cannot_release_legacy_business_until_actual_ble_worker_exits() {
    for role in ["operation", "native_session"] {
        let (physical, ordinary) = configured_ordinary_gate();
        run(&physical, "acquire", 1, role).unwrap();
        let worker = physical
            .track_go_ble_io("BLE:73B7446E-E6CE-C90C-C844-485389FE8C1D", PRODUCER)
            .unwrap();
        physical.test_confirm_go_exit("replacement-go");
        assert!(
            ordinary
                .acquire_native_reader("native-epoch", 1, "reader-a", 7)
                .is_err()
        );
        assert_eq!(run(&physical, "release", 1, role), Err("stale_producer"));
        drop(worker);
        let current = ordinary
            .acquire_native_reader("native-epoch", 2, "reader-a", 7)
            .unwrap();
        assert_eq!(run(&physical, "release", 1, role), Err("stale_producer"));
        assert!(current.is_current());
    }
}

#[test]
fn confirmed_go_exit_releases_idle_legacy_business_guard() {
    for role in ["operation", "native_session"] {
        let (physical, ordinary) = configured_ordinary_gate();
        run(&physical, "acquire", 1, role).unwrap();
        physical.test_confirm_go_exit("replacement-go");
        assert!(
            ordinary
                .acquire_native_reader("native-epoch", 1, "reader-a", 7)
                .is_ok()
        );
    }
}
