use super::*;

#[tokio::test]
async fn quarantine_rejects_shared_voice_and_shutdown_cleanup_admission() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime/p556-radio")
        .join(crate::snapshot::new_instance_id());
    std::fs::create_dir_all(&root).unwrap();
    let gate = crate::module_recovery::Gate::quarantined_for_tests();
    let modem = crate::modem_actor::ModemActor::with_test_port_opener_and_module_recovery(
        |_, _| panic!("quarantined radio reached serial factory"),
        gate.clone(),
    )
    .unwrap();
    let radio = Service::new(
        root.clone(),
        crate::usb_at::Owner::unavailable_for_tests("producer".into())
            .with_recovery_gate_for_tests(gate.clone()),
        modem.clone(),
        BackendAvailability::with_module_recovery(gate),
        "producer".into(),
    );
    let shared = radio
        .borrow_shared_voice_operation("producer", true)
        .err()
        .map(|e| e.code);
    let shutdown = radio
        .borrow_shared_voice_shutdown("producer")
        .await
        .err()
        .map(|e| e.code);
    let idle = radio.finish_shutdown_receipt().await;
    modem.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(root);
    assert_eq!(shared, Some(crate::module_recovery::CODE));
    assert_eq!(shutdown, Some(crate::module_recovery::CODE));
    assert!(
        idle.is_ok(),
        "empty quarantined radio must drain without physical work: {idle:?}"
    );
}
