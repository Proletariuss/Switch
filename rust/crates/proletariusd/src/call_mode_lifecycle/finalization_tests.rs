//! Production finalization -> radio -> USB actor -> typed AT Plan. Only the
//! command transport and explicit ownership-only target are scripted; neither
//! provides a native Device or durable physical binding.
use super::*;
use crate::usb_at_transport::{PhysicalTarget, ReenumerationAnchor, Transport, TransportError};

#[derive(Clone, Copy)]
enum Fault {
    None,
    WrongIdentity,
    UsbDrift,
    UsbError,
    UsbIncomplete,
    VoiceError,
    VoiceDrift,
}
struct Script {
    fault: Fault,
    commands: StdMutex<Vec<String>>,
    opens: AtomicUsize,
    identity_reads: AtomicUsize,
    usb_reads: AtomicUsize,
    voice_reads: AtomicUsize,
    before_final_voice: StdMutex<Option<Box<dyn FnOnce() + Send>>>,
}
struct ScriptedTransport(Arc<Script>, PhysicalTarget);
impl Transport for ScriptedTransport {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
        Ok(self.1.clone())
    }
    fn capture_reenumeration_anchor(
        &mut self,
        deadline: std::time::Instant,
        guard: crate::module_radio::CallAdmission,
    ) -> Result<ReenumerationAnchor, TransportError> {
        if !guard() || std::time::Instant::now() >= deadline {
            return Err(TransportError {
                code: "usb_at_reopen_revoked",
                message: "Fixture capture authority or budget ended".into(),
                response: String::new(),
                uncertain: false,
            });
        }
        let anchor = ReenumerationAnchor::synthetic_for_tests(self.1.clone(), (0x2c7c, 0x0125));
        assert_eq!(anchor.durable_binding().unwrap(), None);
        Ok(anchor)
    }
    fn command(
        &mut self,
        command: &str,
        follow_up: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        assert!(follow_up.is_none());
        let script = &self.0;
        script.commands.lock().unwrap().push(command.into());
        match command {
            "AT+CGSN" => {
                let final_read = script.identity_reads.fetch_add(1, Ordering::AcqRel) != 0;
                Ok(format!(
                    "{}\r\nOK",
                    if final_read && matches!(script.fault, Fault::WrongIdentity) {
                        "999999999999999"
                    } else {
                        "123456789012345"
                    }
                ))
            }
            "ATI" => Ok("controlled firmware\r\nOK".into()),
            "AT+CLCC" => Ok("OK".into()),
            // The synchronous readiness projection queries support even when
            // the original target disables ADB. This is not authorization.
            "AT+QADBKEY?" => Ok("ERROR".into()),
            "AT+QCFG=\"USBCFG\"" => {
                let final_read = script.usb_reads.fetch_add(1, Ordering::AcqRel) != 0;
                if final_read && matches!(script.fault, Fault::UsbError) {
                    return Err(TransportError {
                        code: "controlled_final_read_failed",
                        message: "controlled final USB read failure".into(),
                        response: String::new(),
                        uncertain: false,
                    });
                }
                if final_read && matches!(script.fault, Fault::UsbIncomplete) {
                    return Ok("OK".into());
                }
                Ok(format!(
                    "+QCFG: \"usbcfg\",0x2C7C,0x2400,{},0,1,0,1,0,0\r\nOK",
                    if final_read && matches!(script.fault, Fault::UsbDrift) {
                        0
                    } else {
                        1
                    }
                ))
            }
            "AT+QCFG=\"ims\"" => {
                let final_read = script.voice_reads.fetch_add(1, Ordering::AcqRel) != 0;
                if final_read && matches!(script.fault, Fault::VoiceError) {
                    return Ok("ERROR".into());
                }
                Ok(format!(
                    "+QCFG: \"ims\",{},1\r\nOK",
                    if final_read && matches!(script.fault, Fault::VoiceDrift) {
                        0
                    } else {
                        1
                    }
                ))
            }
            "AT+QCFG=\"volte_disable\"" => {
                if script.voice_reads.load(Ordering::Acquire) >= 2 {
                    if let Some(hook) = script.before_final_voice.lock().unwrap().take() {
                        hook();
                    }
                }
                Ok("+QCFG: \"volte_disable\",0\r\nOK".into())
            }
            other => panic!("finalization must not write, authorize, reboot or rebind: {other}"),
        }
    }
    fn description(&self) -> String {
        "controlled finalization AT transport".into()
    }
}

async fn fixture(
    fault: Fault,
    schema2: bool,
) -> (
    Fixture,
    Arc<Script>,
    CallModeReservation,
    Operation,
    crate::call_mode_backups::CapturedNativeBackup,
) {
    let mut fixture = Fixture::new();
    let gate = fixture.service.0.radio.module_recovery().clone();
    let script = Arc::new(Script {
        fault,
        commands: StdMutex::new(Vec::new()),
        opens: AtomicUsize::new(0),
        identity_reads: AtomicUsize::new(0),
        usb_reads: AtomicUsize::new(0),
        voice_reads: AtomicUsize::new(0),
        before_final_voice: StdMutex::new(None),
    });
    let factory_script = script.clone();
    let usb = crate::usb_at::Owner::with_factory(
        PRODUCER.into(),
        Arc::new(move || {
            factory_script.opens.fetch_add(1, Ordering::AcqRel);
            Ok(Box::new(ScriptedTransport(
                factory_script.clone(),
                PhysicalTarget::synthetic_for_tests(),
            )))
        }),
    )
    .with_recovery_gate_for_tests(gate.clone());
    usb.claim_native_management().unwrap();
    let session = usb.open_native_session().await.unwrap();
    let radio = Radio::new(
        fixture.root.clone(),
        usb.clone(),
        fixture.modem.clone(),
        BackendAvailability::with_module_recovery(gate),
        PRODUCER.into(),
    );
    radio.configure_native_usb(&session).await.unwrap();
    fixture.service = Service::new(&fixture.root, radio, fixture.service.0.inbox.clone(), usb);
    let proof = fixture.lifetime.as_ref().unwrap().root_proof();
    let original_id = save_restore_fixture(&fixture.root, &proof).await;
    let id = if schema2 {
        let original_path = fixture.root.join("module-backups").join(&original_id);
        let original_metadata = std::fs::metadata(&original_path).unwrap();
        let original_raw = std::fs::read(&original_path).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original_raw).unwrap();
        value["schema_version"] = json!(2);
        value.as_object_mut().unwrap().remove("voice");
        let schema2_raw = serde_json::to_vec(&value).unwrap();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/call-mode/backups/import")
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(schema2_raw.clone())))
            .unwrap();
        let response = crate::call_mode_backups::handle_public_with_proof(
            request,
            &fixture.root,
            Ok(proof.clone()),
        )
        .await;
        let status = response.status();
        let response: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "schema2 fixture must use actual public import"
        );
        let imported_id = response["backup"]["id"].as_str().unwrap().to_owned();
        assert_ne!(imported_id, original_id);
        let after = std::fs::metadata(&original_path).unwrap();
        assert_eq!(
            (after.dev(), after.ino()),
            (original_metadata.dev(), original_metadata.ino()),
            "public schema2 import replaced the original v3 inode"
        );
        assert!(
            std::fs::read(&original_path).unwrap() == original_raw,
            "public schema2 import changed the original committed v3 bytes"
        );
        assert!(
            std::fs::read(fixture.root.join("module-backups").join(&imported_id)).unwrap()
                == schema2_raw,
            "public schema2 import did not preserve the submitted raw payload"
        );
        imported_id
    } else {
        original_id
    };
    let captured = crate::call_mode_backups::capture_native_backup(&fixture.root, &id, &proof)
        .await
        .unwrap();
    let lease = fixture
        .service
        .0
        .radio
        .reserve_call_mode_restore_operation()
        .await
        .unwrap();
    let operation = fixture
        .service
        .0
        .state
        .begin(
            PRODUCER,
            Status::new("restoring_usb", "controlled finalization"),
        )
        .unwrap();
    (fixture, script, lease, operation, captured)
}

// Finalization-only cases enter through the same durable intent/attachment
// helper as the full restore, while the target reads remain real actor work.
async fn prepared(
    fault: Fault,
    schema2: bool,
) -> (
    Fixture,
    Arc<Script>,
    CallModeReservation,
    Operation,
    crate::call_mode_backups::CapturedNativeBackup,
) {
    let result = fixture(fault, schema2).await;
    let (fixture, _, lease, operation, captured) = &result;
    fixture
        .service
        .begin_restore_journal(
            operation,
            lease,
            captured,
            &captured.backup().module.imei,
            captured.backup().usb,
            VoiceConfiguration {
                ims: 1,
                volte_capability: 1,
                volte_disabled: 0,
            },
        )
        .await
        .unwrap();
    result
}

async fn release(fixture: &Fixture, lease: &CallModeReservation, operation: &Operation) {
    fixture
        .service
        .0
        .radio
        .release_call_mode_operation(lease)
        .await
        .unwrap();
    fixture.service.0.usb_at.shutdown().await.unwrap();
    assert!(fixture.service.0.state.end(operation));
}

#[tokio::test]
async fn final_restore_typed_read_error_incomplete_or_wrong_target_never_publishes_receipt() {
    for fault in [
        Fault::WrongIdentity,
        Fault::UsbDrift,
        Fault::UsbError,
        Fault::UsbIncomplete,
        Fault::VoiceError,
        Fault::VoiceDrift,
    ] {
        let (fixture, script, lease, operation, captured) = prepared(fault, false).await;
        let result = fixture
            .service
            .finish_restore(&operation, &lease, &captured, "", false)
            .await;
        assert!(
            result.is_err(),
            "final typed read must reject an unconfirmed target"
        );
        assert!(fixture.service.0.state.snapshot().last_restore.is_none());
        if matches!(fault, Fault::WrongIdentity) {
            assert_eq!(
                script.identity_reads.load(Ordering::Acquire),
                2,
                "capture succeeds first; the fresh final typed identity must still be read and rejected"
            );
        }
        assert!(
            script
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|command| command == "AT+CGSN")
        );
        assert_eq!(
            script.opens.load(Ordering::Acquire),
            1,
            "finalization must not reopen AT"
        );
        assert!(!fixture.root.join(".module-adb-interface.lock").exists());
        release(&fixture, &lease, &operation).await;
    }
}

#[tokio::test]
async fn final_restore_accepts_nonready_original_and_preserves_schema2_voice_without_writes() {
    for schema2 in [false, true] {
        let fault = if schema2 {
            Fault::VoiceDrift
        } else {
            Fault::None
        };
        let (fixture, script, lease, operation, captured) = prepared(fault, schema2).await;
        let expected_id = captured.id().to_owned();
        let expected_hash = captured.sha256();
        let status = fixture
            .service
            .finish_restore(&operation, &lease, &captured, "", false)
            .await
            .unwrap();
        assert!(fixture.service.0.state.snapshot().last_restore.is_none());
        assert!(fixture.service.0.state.running());
        fixture
            .service
            .complete_operation(
                &operation,
                &lease,
                Some(PreparedRestore {
                    captured,
                    receipt: status,
                }),
                true,
            )
            .await;
        let status = fixture.service.0.state.snapshot();
        assert_ne!(
            status.state, "ready",
            "original USB deliberately disables ADB/UAC"
        );
        let receipt = status.last_restore.unwrap();
        assert_eq!(receipt["backup_id"], expected_id);
        assert_eq!(receipt["backup_sha256"], expected_hash);
        assert_eq!(receipt["changed"], false);
        assert_eq!(script.usb_reads.load(Ordering::Acquire), 2);
        assert_eq!(script.voice_reads.load(Ordering::Acquire), 2);
        assert_eq!(script.opens.load(Ordering::Acquire), 1);
        {
            let commands = script.commands.lock().unwrap();
            assert_eq!(
                commands
                    .iter()
                    .filter(|command| command.as_str() == "AT+QADBKEY?")
                    .count(),
                1,
                "each nonready target must make exactly one synchronous read-only support query"
            );
            assert!(
                commands.iter().all(|command| matches!(
                    command.as_str(),
                    "AT+CGSN"
                        | "ATI"
                        | "AT+CLCC"
                        | "AT+QCFG=\"USBCFG\""
                        | "AT+QCFG=\"ims\""
                        | "AT+QCFG=\"volte_disable\""
                        | "AT+QADBKEY?"
                )),
                "finalization recorded a command outside the exact read-only fixture set"
            );
        }
        assert!(!fixture.root.join(".module-adb-interface.lock").exists());
        assert!(!fixture.service.0.state.running());
        fixture.service.0.usb_at.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn restore_receipt_waits_for_original_borrowed_worker_and_real_release() {
    let (fixture, script, lease, operation, captured) = prepared(Fault::None, false).await;
    let status = fixture
        .service
        .finish_restore(&operation, &lease, &captured, "", false)
        .await
        .unwrap();
    let pin = fixture
        .service
        .0
        .radio
        .borrow_call_mode_operation_voice(&lease)
        .unwrap();
    let before_commands = script.commands.lock().unwrap().clone();
    let service = fixture.service.clone();
    let op = operation.clone();
    let original = lease.clone();
    let completing = tokio::spawn(async move {
        service
            .complete_operation(
                &op,
                &original,
                Some(PreparedRestore {
                    captured,
                    receipt: status,
                }),
                true,
            )
            .await;
    });
    // The retained pin makes the real reservation actor reject release. Wait
    // for that acknowledgement, not a scheduler delay, before checking state.
    until(|| fixture.service.0.state.snapshot().state == "failed").await;
    assert!(!completing.is_finished());
    assert!(fixture.service.0.state.running());
    assert!(fixture.service.0.state.snapshot().last_restore.is_none());
    assert!(matches!(
        crate::call_mode_backups::try_call_mode_operation_file_lock(&fixture.root),
        Err(crate::call_mode_backups::OperationFileLockError::Busy)
    ));
    assert_eq!(*script.commands.lock().unwrap(), before_commands);
    drop(pin);
    tokio::time::timeout(Duration::from_secs(3), completing)
        .await
        .unwrap()
        .unwrap();
    assert!(fixture.service.0.state.snapshot().last_restore.is_some());
    assert!(!fixture.service.0.state.running());
    assert!(
        fixture
            .service
            .0
            .radio
            .validate_call_mode_operation_lock(&lease)
            .is_err()
    );
    assert_eq!(*script.commands.lock().unwrap(), before_commands);
    fixture.service.0.usb_at.shutdown().await.unwrap();
}

#[tokio::test]
async fn restore_input_or_root_changed_after_target_verification_never_publishes_receipt() {
    for lose_root in [false, true] {
        let (mut fixture, script, lease, operation, captured) = prepared(Fault::None, false).await;
        let status = fixture
            .service
            .finish_restore(&operation, &lease, &captured, "", false)
            .await
            .unwrap();
        let before_commands = script.commands.lock().unwrap().clone();
        if lose_root {
            drop(fixture.lifetime.take());
        } else {
            std::fs::write(
                fixture.root.join("module-backups").join(captured.id()),
                b"{}",
            )
            .unwrap();
        }
        tokio::time::timeout(
            Duration::from_secs(3),
            fixture.service.complete_operation(
                &operation,
                &lease,
                Some(PreparedRestore {
                    captured,
                    receipt: status,
                }),
                true,
            ),
        )
        .await
        .unwrap();
        assert!(fixture.service.0.state.snapshot().last_restore.is_none());
        assert_eq!(fixture.service.0.state.snapshot().state, "failed");
        assert!(!fixture.service.0.state.running());
        assert!(
            fixture
                .service
                .0
                .radio
                .validate_call_mode_operation_lock(&lease)
                .is_err()
        );
        assert_eq!(*script.commands.lock().unwrap(), before_commands);
        fixture.service.0.usb_at.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn restore_lock_name_replaced_while_release_waits_settles_without_success_receipt() {
    let (fixture, script, lease, operation, captured) = prepared(Fault::None, false).await;
    let receipt = fixture
        .service
        .finish_restore(&operation, &lease, &captured, "", false)
        .await
        .unwrap();
    let pin = fixture
        .service
        .0
        .radio
        .borrow_call_mode_operation_voice(&lease)
        .unwrap();
    let service = fixture.service.clone();
    let op = operation.clone();
    let original = lease.clone();
    let before_commands = script.commands.lock().unwrap().clone();
    let completing = tokio::spawn(async move {
        service
            .complete_operation(
                &op,
                &original,
                Some(PreparedRestore { captured, receipt }),
                true,
            )
            .await;
    });
    until(|| fixture.service.0.state.snapshot().state == "failed").await;
    assert!(!completing.is_finished());
    let path = fixture.root.join("call-mode-operation.lock");
    let retained = fixture.root.join("retained-operation.lock");
    std::fs::rename(&path, &retained).unwrap();
    std::fs::write(&path, []).unwrap();
    drop(pin);
    tokio::time::timeout(Duration::from_secs(3), completing)
        .await
        .unwrap()
        .unwrap();
    let state = fixture.service.0.state.snapshot();
    assert!(state.last_restore.is_none());
    assert!(state.summary.contains("原操作锁路径已变化"));
    assert!(!fixture.service.0.state.running());
    let old = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(retained)
        .unwrap();
    old.try_lock().unwrap();
    old.unlock().unwrap();
    assert_eq!(*script.commands.lock().unwrap(), before_commands);
    fixture.service.0.usb_at.shutdown().await.unwrap();
}

#[tokio::test]
async fn equal_restore_run_returns_pending_receipt_until_outer_owner_releases() {
    let (fixture, script, lease, operation, captured) = fixture(Fault::None, false).await;
    let expected_id = captured.id().to_owned();
    let expected_hash = captured.sha256();
    let pending = fixture
        .service
        .run(Job::Restore(captured), &operation, &lease)
        .await;
    assert!(pending.is_some());
    assert!(fixture.service.0.state.snapshot().last_restore.is_none());
    assert!(fixture.service.0.state.running());
    fixture
        .service
        .complete_operation(&operation, &lease, pending, true)
        .await;
    let status = fixture.service.0.state.snapshot();
    let receipt = status.last_restore.unwrap();
    assert_eq!(receipt["backup_id"], expected_id);
    assert_eq!(receipt["backup_sha256"], expected_hash);
    assert_eq!(receipt["changed"], false);
    assert!(!fixture.service.0.state.running());
    assert_eq!(script.opens.load(Ordering::Acquire), 1);
    assert!(
        !fixture
            .root
            .join(crate::call_mode_restore_journal::ACTIVE_NAME)
            .exists()
    );
    let archive = fixture
        .root
        .join(crate::call_mode_restore_journal::ARCHIVE_NAME);
    let runs: Vec<_> = std::fs::read_dir(archive)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(runs.len(), 1);
    let mut records: Vec<_> = std::fs::read_dir(runs[0].path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    records.sort();
    let records: Vec<serde_json::Value> = records
        .iter()
        .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap())
        .collect();
    let events: Vec<_> = records
        .iter()
        .map(|value| value["event"]["event"].as_str().unwrap())
        .collect();
    assert_eq!(events, ["intent", "target_verified", "released", "receipt"]);
    let next = fixture
        .service
        .0
        .radio
        .reserve_call_mode_operation()
        .await
        .unwrap();
    fixture
        .service
        .0
        .radio
        .release_call_mode_operation(&next)
        .await
        .unwrap();
    fixture.service.0.usb_at.shutdown().await.unwrap();
}

#[tokio::test]
async fn restore_archive_conflict_after_actual_release_keeps_pending_evidence_without_success() {
    let (fixture, script, lease, operation, captured) = prepared(Fault::None, false).await;
    let receipt = fixture
        .service
        .finish_restore(&operation, &lease, &captured, "", false)
        .await
        .unwrap();
    let before = script.commands.lock().unwrap().clone();
    let foreign = fixture.root.join("foreign-archive");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("unrelated"), b"preserve").unwrap();
    std::os::unix::fs::symlink(
        &foreign,
        fixture
            .root
            .join(crate::call_mode_restore_journal::ARCHIVE_NAME),
    )
    .unwrap();
    fixture
        .service
        .complete_operation(
            &operation,
            &lease,
            Some(PreparedRestore { captured, receipt }),
            true,
        )
        .await;
    let state = fixture.service.0.state.snapshot();
    assert!(state.last_restore.is_none());
    assert_eq!(state.state, "failed");
    assert!(!fixture.service.0.state.running());
    assert!(
        fixture
            .service
            .0
            .radio
            .validate_call_mode_operation_lock(&lease)
            .is_err()
    );
    assert!(
        fixture
            .root
            .join(crate::call_mode_restore_journal::ACTIVE_NAME)
            .is_dir()
    );
    assert_eq!(
        std::fs::read(foreign.join("unrelated")).unwrap(),
        b"preserve"
    );
    assert_eq!(*script.commands.lock().unwrap(), before);
    assert!(
        fixture
            .service
            .0
            .radio
            .reserve_call_mode_operation()
            .await
            .is_err()
    );
    assert_eq!(
        fixture.service.start_enable(false).await.status(),
        StatusCode::CONFLICT
    );
    fixture.service.0.usb_at.shutdown().await.unwrap();
}

#[tokio::test]
async fn restore_active_namespace_replaced_after_verification_releases_without_success() {
    let (fixture, script, lease, operation, captured) = prepared(Fault::None, false).await;
    let receipt = fixture
        .service
        .finish_restore(&operation, &lease, &captured, "", false)
        .await
        .unwrap();
    let before = script.commands.lock().unwrap().clone();
    let active = fixture
        .root
        .join(crate::call_mode_restore_journal::ACTIVE_NAME);
    let retained = fixture.root.join("retained-active");
    std::fs::rename(&active, &retained).unwrap();
    std::fs::create_dir(&active).unwrap();
    std::fs::write(active.join("unrelated"), b"preserve").unwrap();
    fixture
        .service
        .complete_operation(
            &operation,
            &lease,
            Some(PreparedRestore { captured, receipt }),
            true,
        )
        .await;
    let state = fixture.service.0.state.snapshot();
    assert!(state.last_restore.is_none());
    assert_eq!(state.state, "failed");
    assert!(!fixture.service.0.state.running());
    assert!(
        fixture
            .service
            .0
            .radio
            .validate_call_mode_operation_lock(&lease)
            .is_err()
    );
    assert_eq!(
        std::fs::read(active.join("unrelated")).unwrap(),
        b"preserve"
    );
    assert!(std::fs::read_dir(retained).unwrap().count() >= 2);
    assert_eq!(*script.commands.lock().unwrap(), before);
    assert!(
        fixture
            .service
            .0
            .radio
            .reserve_call_mode_operation()
            .await
            .is_err()
    );
    assert_eq!(
        fixture.service.start_enable(false).await.status(),
        StatusCode::CONFLICT
    );
    fixture.service.0.usb_at.shutdown().await.unwrap();
}

#[tokio::test]
async fn existing_restore_intent_prevents_settlement_and_new_configuration_writes() {
    let (fixture, script, lease, operation, captured) = fixture(Fault::None, false).await;
    let active = fixture
        .root
        .join(crate::call_mode_restore_journal::ACTIVE_NAME);
    std::fs::write(&active, b"old-unresolved-intent").unwrap();
    let pending = fixture
        .service
        .run(Job::Restore(captured), &operation, &lease)
        .await;
    assert!(pending.is_none());
    fixture
        .service
        .complete_operation(&operation, &lease, pending, true)
        .await;
    assert_eq!(std::fs::read(active).unwrap(), b"old-unresolved-intent");
    assert!(fixture.service.0.state.snapshot().last_restore.is_none());
    assert!(!fixture.service.0.state.running());
    assert!(!fixture.root.join(".module-adb-interface.lock").exists());
    assert!(script.commands.lock().unwrap().iter().all(|command| {
        [
            "AT+CLCC",
            "AT+CGSN",
            "ATI",
            "AT+QCFG=\"USBCFG\"",
            "AT+QCFG=\"ims\"",
            "AT+QCFG=\"volte_disable\"",
        ]
        .contains(&command.as_str())
    }));
    assert!(
        fixture
            .service
            .0
            .radio
            .validate_call_mode_operation_lock(&lease)
            .is_err()
    );
    assert!(
        fixture
            .service
            .0
            .radio
            .reserve_call_mode_operation()
            .await
            .is_err()
    );
    assert_eq!(
        fixture.service.start_enable(false).await.status(),
        StatusCode::CONFLICT
    );
    fixture.service.0.usb_at.shutdown().await.unwrap();
}

#[tokio::test]
async fn final_restore_post_read_lock_or_backup_change_prevents_receipt() {
    for change_lock in [true, false] {
        let (fixture, script, lease, operation, captured) = prepared(Fault::None, false).await;
        let path = if change_lock {
            fixture.root.join("call-mode-operation.lock")
        } else {
            fixture.root.join("module-backups").join(captured.id())
        };
        *script.before_final_voice.lock().unwrap() = Some(Box::new(move || {
            if change_lock {
                std::fs::rename(&path, path.with_extension("retained")).unwrap();
                std::fs::write(&path, b"replacement").unwrap();
            } else {
                std::fs::write(&path, b"{}").unwrap();
            }
        }));
        assert!(
            fixture
                .service
                .finish_restore(&operation, &lease, &captured, "", false)
                .await
                .is_err()
        );
        assert!(fixture.service.0.state.snapshot().last_restore.is_none());
        assert_eq!(script.voice_reads.load(Ordering::Acquire), 2);
        release(&fixture, &lease, &operation).await;
    }
}
