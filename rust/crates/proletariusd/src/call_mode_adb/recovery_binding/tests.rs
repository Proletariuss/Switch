//! Data/real lock/actual admission tests. No native USB Device is invented.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn wire() -> serde_json::Value {
    serde_json::json!({"schema_version":1,"producer":"producer-a",
        "owner_instance":"a".repeat(64),"registry_revision":0,
        "observation":{"kind":"current_process_empty"}})
}
fn data() -> DurableVoiceBinding {
    serde_json::from_value(wire()).unwrap()
}
fn encoded(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn interface() -> InterfaceObservation {
    InterfaceObservation {
        interface_number: 4,
        alternate_setting: 1,
        endpoint_in: 0x85,
        endpoint_out: 5,
    }
}
fn helper_wire() -> Vec<String> {
    let mut stat = String::from("321 (helper ) with spaces) S");
    for _ in 0..18 {
        stat.push_str(" 0");
    }
    stat.push_str(" 98765 0\n");
    let argv = format!("{MODULE_VOICE_HELPER}\0--voice-route-session\0--verbose\0").into_bytes();
    vec![
        "SWVOICE1".into(),
        encoded(b"00112233-4455-6677-8899-aabbccddeeff\n"),
        encoded(b"321 98765\n"),
        encoded(stat.as_bytes()),
        encoded(&argv),
        encoded(b"1\n"),
        "1".into(),
        "1".into(),
        "1".into(),
        encoded(stat.as_bytes()),
        encoded(&argv),
        encoded(b"321 98765\n"),
        encoded(b"00112233-4455-6677-8899-aabbccddeeff\n"),
    ]
}
fn bytes(lines: &[String]) -> Vec<u8> {
    format!("{}\n", lines.join("\n")).into_bytes()
}

#[test]
fn checked_shell_status_wrapper_is_decoded_before_strict_original_helper_observation() {
    let protocol = bytes(&helper_wire());
    let token = "0123456789abcdef0123456789abcdef";
    let mut wire = protocol.clone();
    wire.extend_from_slice(format!("\n__SWITCH_ADB_STATUS_{token}_0__\n").as_bytes());
    let (checked, status) = parse_shell_result(&wire, token).unwrap();
    assert_eq!(status, 0);
    assert!(parse_helper(&checked, interface()).is_err());
    let observed = parse_checked_helper(&checked, interface()).unwrap();
    assert_eq!((observed.pid, observed.start_ticks), (321, 98765));
    assert!(parse_checked_helper(&protocol, interface()).is_err());
    let mut extra = checked;
    extra.push(b'\n');
    assert!(parse_checked_helper(&extra, interface()).is_err());
    assert!(parse_checked_helper(b"SWVOICE1", interface()).is_err());
}

#[test]
fn production_parser_keeps_full_binary_argv_and_parses_spaced_parenthesized_comm() {
    let observed = parse_helper(&bytes(&helper_wire()), interface()).unwrap();
    assert_eq!((observed.pid, observed.start_ticks), (321, 98765));
    assert_eq!(
        observed.exact_argv,
        [MODULE_VOICE_HELPER, "--voice-route-session", "--verbose"]
    );
    assert!(observed.route.audio_enabled && observed.route.capture_running);
    let mut binding = data();
    binding.registry_revision = 1;
    binding.observation = Observation::Owned {
        runtime_generation: 3,
        helper: observed,
    };
    let encoded = serde_json::to_value(&binding).unwrap();
    let decoded: DurableVoiceBinding = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, binding);
    assert_eq!(format!("{decoded:?}"), "DurableVoiceBinding { .. }");
}

#[test]
fn production_parser_rejects_pid_boot_argv_races_truncation_and_unknown_route_reads() {
    for index in [9, 10, 11, 12] {
        let mut lines = helper_wire();
        lines[index] = encoded(b"changed");
        assert!(parse_helper(&bytes(&lines), interface()).is_err());
    }
    for argv in [
        format!("{MODULE_VOICE_HELPER}\0--voice-route-session\0--verbose").into_bytes(),
        format!("{MODULE_VOICE_HELPER}\0--voice-route-session-foreign\0--verbose\0").into_bytes(),
        format!("{MODULE_VOICE_HELPER}\0--verbose\0--voice-route-session\0").into_bytes(),
        vec![0xff, 0],
        vec![b'a'; MAX_ARGV + 1],
    ] {
        let mut lines = helper_wire();
        lines[4] = encoded(&argv);
        lines[10] = encoded(&argv);
        assert!(parse_helper(&bytes(&lines), interface()).is_err());
    }
    let mut lines = helper_wire();
    lines[6] = "2".into();
    assert!(parse_helper(&bytes(&lines), interface()).is_err());
    let mut lines = helper_wire();
    lines[2] = encoded(b"1 98765\n");
    lines[11] = lines[2].clone();
    assert!(parse_helper(&bytes(&lines), interface()).is_err());
    assert!(parse_helper(&vec![b'x'; MAX_OUTPUT_BYTES + 1], interface()).is_err());
}

#[test]
fn durable_decoder_validates_all_states_without_turning_fresh_empty_into_history() {
    let mut value = wire();
    assert!(
        serde_json::from_value::<DurableVoiceBinding>(value.clone())
            .unwrap()
            .matches_producer("producer-a")
    );
    value["observation"] = serde_json::json!({"kind":"original_owner_no_owned_cleanup"});
    assert!(serde_json::from_value::<DurableVoiceBinding>(value.clone()).is_err());
    value["registry_revision"] = serde_json::json!(2);
    assert!(serde_json::from_value::<DurableVoiceBinding>(value).is_ok());
    for (key, bad) in [
        ("schema_version", serde_json::json!(2)),
        ("producer", serde_json::json!("bad/producer")),
        ("owner_instance", serde_json::json!("not-a-process-token")),
        ("registry_revision", serde_json::json!(u64::MAX)),
        ("authority", serde_json::json!(true)),
    ] {
        let mut value = wire();
        value[key] = bad;
        assert!(serde_json::from_value::<DurableVoiceBinding>(value).is_err());
    }
    let mut unknown = wire();
    unknown["observation"] = serde_json::json!({"kind":"unknown","reason":"read_unavailable"});
    let unknown: DurableVoiceBinding = serde_json::from_value(unknown).unwrap();
    assert!(!unknown.matches_producer("another-producer"));
}

#[test]
fn registry_revision_observes_actual_retirement_and_rejects_replacement_snapshot() {
    struct NoOpen;
    impl AdbTarget for NoOpen {
        fn open(&mut self) -> Result<Box<dyn BulkIo>, ProbeError> {
            panic!("metadata must not open")
        }
    }
    let gate = crate::module_recovery::Gate::default();
    let original = VoiceCleanupObligation {
        producer: "producer-a".into(),
        generation: 4,
        target: VoiceBoundTarget {
            plan: AdbPlan::from_target(Box::new(NoOpen)),
            capture: None,
            managed: None,
        },
        authority: Arc::new(gate.admit_operation("producer-a").unwrap()),
    };
    let mut registry = VoiceRouteOwnership::default();
    assert_eq!(snapshot(&registry, "producer-a").unwrap().revision, 0);
    registry.begin(original.clone()).unwrap();
    let first = snapshot(&registry, "producer-a").unwrap();
    registry.begin(original.clone()).unwrap();
    still_current(&registry, "producer-a", &first).unwrap();
    registry.retire(&original, false);
    assert_eq!(registry.observation_revision, 1);
    registry.retire(&original, true);
    assert_eq!(registry.observation_revision, 2);
    assert!(still_current(&registry, "producer-a", &first).is_err());
    assert!(
        snapshot(&registry, "producer-a")
            .unwrap()
            .obligation
            .is_none()
    );
    registry.begin(original.clone()).unwrap();
    assert!(snapshot(&registry, "foreign").is_err());
}

#[test]
fn successful_read_needs_actual_close_and_error_read_still_closes() {
    let closed = AtomicUsize::new(0);
    assert!(
        finish_read(Ok(7), || {
            closed.fetch_add(1, Ordering::AcqRel);
            Err(unavailable())
        })
        .is_err()
    );
    assert!(
        finish_read::<()>(Err(unavailable()), || {
            closed.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
        .is_err()
    );
    assert_eq!(closed.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn cancellation_retains_actual_worker_admission_until_close_and_output_drop() {
    let semaphore: &'static Semaphore = Box::leak(Box::new(Semaphore::new(1)));
    let permit = semaphore.acquire().await.unwrap();
    let closed = Arc::new(AtomicBool::new(false));
    let worker_closed = closed.clone();
    let (started, wait) = tokio::sync::oneshot::channel();
    let (release, receiver) = std::sync::mpsc::channel();
    let task = tokio::spawn(retain_capture(async move {
        run_capture_worker(permit, move || {
            started.send(()).unwrap();
            receiver.recv_timeout(Duration::from_secs(4)).unwrap();
            // Explicit blocking I/O substitute; this is the real worker's
            // close receipt, not a fake native Device or physical proof.
            worker_closed.store(true, Ordering::Release);
            Ok((data(), None))
        })
        .await
    }));
    wait.await.unwrap();
    task.abort();
    let _ = task.await;
    assert_eq!(semaphore.available_permits(), 0);
    assert!(!closed.load(Ordering::Acquire));
    release.send(()).unwrap();
    let next = tokio::time::timeout(Duration::from_secs(4), semaphore.acquire())
        .await
        .unwrap()
        .unwrap();
    assert!(closed.load(Ordering::Acquire));
    drop(next);
}

#[tokio::test]
async fn empty_original_registry_capture_never_creates_adb_lock_or_dispatches_io() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime/iteration-587-voice-capture-tests")
        .join(crate::snapshot::new_instance_id());
    let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
    let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = calls.clone();
    let usb = crate::usb_at::Owner::with_factory(
        "producer-a".into(),
        Arc::new(move || {
            factory_calls.fetch_add(1, Ordering::AcqRel);
            panic!("empty registry must not dispatch generic factory");
        }),
    )
    .with_recovery_gate_for_tests(gate.clone());
    usb.claim_native_management().unwrap();
    let trace = crate::usb_at::reconnect_tests::install_default(&usb, "current-session");
    let modem = crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap();
    let radio = crate::module_radio::Service::new(
        root.clone(),
        usb.clone(),
        modem.clone(),
        crate::availability::BackendAvailability::with_module_recovery(gate),
        "producer-a".into(),
    );
    crate::module_radio_store::Store::new(root.clone())
        .save(crate::module_radio_store::Preferences::default())
        .unwrap();
    radio.configure_native_usb("current-session").await.unwrap();
    let lease = radio.reserve_call_mode_restore_operation().await.unwrap();
    let inbox = crate::message_archive::InboxState::for_go_producer("producer-a".into());
    let captured = capture_restore_voice_binding(&root, inbox, usb.clone(), radio.clone(), &lease)
        .await
        .unwrap();
    captured.validate_for_publication().unwrap();
    assert!(matches!(
        &captured.binding().observation,
        Observation::CurrentProcessEmpty | Observation::OriginalOwnerNoOwnedCleanup
    ));
    assert!(!root.join(".module-adb-interface.lock").exists());
    assert!(trace.commands.lock().unwrap().is_empty());
    assert_eq!(calls.load(Ordering::Acquire), 0);
    assert_eq!(ADB_ADMISSION.available_permits(), 0);
    drop(captured);
    radio.release_call_mode_operation(&lease).await.unwrap();
    usb.shutdown().await.unwrap();
    modem.shutdown().unwrap();
    drop(lifetime);
}

#[test]
fn adb_namespace_replacement_invalidates_publication_without_holding_the_flock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime/iteration-587-voice-capture-tests")
        .join(crate::snapshot::new_instance_id());
    let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
    let guard = acquire_capture_interface_lock(&root, &lifetime.root_proof()).unwrap();
    let witness = guard.namespace_witness().unwrap();
    drop(guard);
    let semaphore: &'static Semaphore = Box::leak(Box::new(Semaphore::new(1)));
    let permit = semaphore.try_acquire().unwrap();
    let capture = CapturedRestoreVoiceBinding {
        binding: data(),
        _permit: permit,
        adb_namespace: Some(witness),
        scope: None,
    };
    capture.validate_for_publication().unwrap();
    // The witness does not retain the original locked open file description.
    let next = acquire_interface_lock(&root).unwrap();
    drop(next);
    std::fs::rename(
        root.join(".module-adb-interface.lock"),
        root.join("original-adb-lock"),
    )
    .unwrap();
    std::fs::write(root.join(".module-adb-interface.lock"), []).unwrap();
    assert!(capture.validate_for_publication().is_err());
    drop(capture);
    drop(lifetime);
}
