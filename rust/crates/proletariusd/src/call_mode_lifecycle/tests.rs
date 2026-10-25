//! Offline lifecycle ownership tests. No radio action, ADB probe, or physical
//! USB transaction is submitted by this fixture.
use super::*;
use crate::availability::BackendAvailability;
use crate::modem_actor::ModemActor;
use crate::module_radio_transport::CallModeModuleIdentity;

const PRODUCER: &str = "native-lifecycle-fixture";
struct Fixture {
    service: Service,
    modem: ModemActor,
    root: PathBuf,
    lifetime: Option<crate::daemon_lifetime::Lease>,
    factory_calls: Arc<AtomicUsize>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_legacy_locks(false)
    }
    fn with_legacy_locks(legacy: bool) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/native-call-mode-lifecycle-tests")
            .join(crate::snapshot::new_instance_id());
        std::fs::create_dir_all(&root).unwrap();
        let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        if legacy {
            use std::os::unix::fs::PermissionsExt;
            for name in ["call-mode-operation.lock", ".module-adb-interface.lock"] {
                fs::write(root.join(name), []).unwrap();
                fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
            }
            assert!(
                crate::module_recovery::legacy_locks::migrate(&root, &lifetime.root_proof())
                    .unwrap()
            );
        }
        let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = factory_calls.clone();
        let usb = crate::usb_at::Owner::with_factory(
            PRODUCER.into(),
            Arc::new(move || {
                observed_calls.fetch_add(1, Ordering::AcqRel);
                Err(crate::usb_at_transport::TransportError {
                    code: "usb_at_not_found",
                    message: "controlled lifecycle USB device is absent".into(),
                    response: String::new(),
                    uncertain: false,
                })
            }),
        )
        .with_recovery_gate_for_tests(gate.clone());
        let modem = ModemActor::new_with_module_recovery(gate.clone()).unwrap();
        let inbox = crate::message_archive::InboxState::for_go_producer(PRODUCER.into());
        let radio = Radio::new(
            root.clone(),
            usb.clone(),
            modem.clone(),
            BackendAvailability::with_module_recovery(gate),
            PRODUCER.into(),
        );
        Self {
            service: Service::new(&root, radio, inbox, usb),
            modem,
            root,
            lifetime: Some(lifetime),
            factory_calls,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = self.modem.shutdown() {
            if std::thread::panicking() {
                crate::diagnostics::write_line(format_args!(
                    "fixture modem shutdown unconfirmed during unwind: {error:?}"
                ));
            } else {
                panic!("fixture modem shutdown unconfirmed: {error:?}");
            }
        }
        drop(self.lifetime.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("controlled lifecycle transition did not complete");
}

async fn save_restore_fixture(root: &Path, proof: &crate::daemon_lifetime::RootProof) -> String {
    crate::call_mode_backups::create_native_backup(
        root,
        CallModeModuleIdentity {
            imei: "123456789012345".into(),
            firmware: "fixture".into(),
        },
        UsbComposition {
            vendor_id: 0x2c7c,
            product_id: 0x2400,
            flags: [1, 0, 1, 0, 1, 0, 0],
        },
        VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        },
        false,
        proof,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn foreign_valid_capture_cannot_enter_another_lifecycle_or_open_a_transport() {
    let fixture = Fixture::new();
    let foreign = fixture.root.join("foreign-owned-root");
    let foreign_lease = crate::daemon_lifetime::Lease::acquire(&foreign).unwrap();
    let id = save_restore_fixture(&foreign, &foreign_lease.root_proof()).await;
    let path = foreign.join("module-backups").join(&id);
    let original = std::fs::read(&path).unwrap();
    let captured =
        crate::call_mode_backups::capture_native_backup(&foreign, &id, &foreign_lease.root_proof())
            .await
            .unwrap();
    captured.verify_unchanged().await.unwrap();
    let (send, receive) = oneshot::channel();
    fixture
        .service
        .admit(
            Job::Restore(captured),
            Status::new("restoring_usb", "fixture"),
            send,
        )
        .await;
    assert_eq!(receive.await.unwrap().status(), StatusCode::BAD_REQUEST);
    assert!(!fixture.service.0.state.running());
    assert!(!fixture.service.0.radio.sim_channel_busy());
    assert_eq!(fixture.factory_calls.load(Ordering::Acquire), 0);
    assert!(pristine_marker(&fixture.root).unwrap());
    assert!(!fixture.root.join("call-mode-operation.lock").exists());
    assert!(!fixture.root.join(".module-adb-interface.lock").exists());
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[tokio::test]
async fn ended_lifetime_cannot_be_replaced_by_reacquiring_the_same_path_for_restore() {
    let mut fixture = Fixture::new();
    let proof = fixture.lifetime.as_ref().unwrap().root_proof();
    let id = save_restore_fixture(&fixture.root, &proof).await;
    let path = fixture.root.join("module-backups").join(&id);
    let original = std::fs::read(&path).unwrap();
    let proof = fixture
        .service
        .0
        .radio
        .module_recovery()
        .root_proof_for(&fixture.root)
        .unwrap();
    let captured = crate::call_mode_backups::capture_native_backup(&fixture.root, &id, &proof)
        .await
        .unwrap();
    drop(fixture.lifetime.take());
    let replacement = crate::daemon_lifetime::Lease::acquire(&fixture.root).unwrap();
    assert!(!proof.same_authority(&replacement.root_proof()));
    assert_eq!(
        fixture.service.start_restore(id).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(
        fixture
            .service
            .verify_restore_input(&captured)
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .0
            .radio
            .module_recovery()
            .is_admission_revoked()
    );
    assert_eq!(fixture.factory_calls.load(Ordering::Acquire), 0);
    assert!(!fixture.service.0.state.running());
    assert!(!fixture.root.join("call-mode-operation.lock").exists());
    assert!(!fixture.root.join(".module-adb-interface.lock").exists());
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[tokio::test]
async fn changed_captured_restore_is_rejected_before_radio_or_lifecycle_admission() {
    for mutation in ["content", "identical-inode-replacement", "missing"] {
        let fixture = Fixture::new();
        let usb = UsbComposition {
            vendor_id: 0x2c7c,
            product_id: 0x2400,
            flags: [1, 0, 1, 0, 1, 0, 0],
        };
        let voice = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        let id = crate::call_mode_backups::create_native_backup(
            &fixture.root,
            CallModeModuleIdentity {
                imei: "123456789012345".into(),
                firmware: "fixture".into(),
            },
            usb,
            voice,
            false,
            &fixture.lifetime.as_ref().unwrap().root_proof(),
        )
        .await
        .unwrap();
        let proof = fixture
            .service
            .0
            .radio
            .module_recovery()
            .root_proof_for(&fixture.root)
            .unwrap();
        let captured = crate::call_mode_backups::capture_native_backup(&fixture.root, &id, &proof)
            .await
            .unwrap();
        let path = fixture.root.join("module-backups").join(&id);
        let bytes = std::fs::read(&path).unwrap();
        match mutation {
            "content" => {
                std::fs::write(&path, b"{}").unwrap();
            }
            "identical-inode-replacement" => {
                let replacement = fixture.root.join("replacement");
                std::fs::write(&replacement, &bytes).unwrap();
                std::fs::rename(replacement, &path).unwrap();
            }
            "missing" => {
                std::fs::remove_file(&path).unwrap();
            }
            _ => unreachable!(),
        }
        let (send, receive) = oneshot::channel();
        fixture
            .service
            .admit(
                Job::Restore(captured),
                Status::new("restoring_usb", "fixture"),
                send,
            )
            .await;
        assert_eq!(
            receive.await.unwrap().status(),
            StatusCode::BAD_REQUEST,
            "accepted {mutation}"
        );
        assert!(!fixture.service.0.state.running());
        assert!(!fixture.service.0.radio.sim_channel_busy());
        assert!(!fixture.service.0.operation_started.load(Ordering::Acquire));
        assert!(pristine_marker(&fixture.root).unwrap());
        assert!(!fixture.root.join("call-mode-operation.lock").exists());
        assert!(!fixture.root.join(".module-adb-interface.lock").exists());
    }
}

#[tokio::test]
async fn pristine_native_shutdown_does_not_open_adb_or_create_route_operation_locks() {
    let fixture = Fixture::new();
    assert!(pristine_marker(&fixture.root).unwrap());
    fixture
        .service
        .0
        .inbox
        .bind_native_module_producer("native-pristine".into())
        .unwrap();
    assert!(!fixture.service.requires_runtime_cleanup().await);
    fixture.service.finish_shutdown().await.unwrap();
    fixture.service.finish_shutdown().await.unwrap();
    assert!(!fixture.root.join("call-mode-operation.lock").exists());
    assert!(!fixture.root.join(".module-adb-interface.lock").exists());
}

#[tokio::test]
async fn read_only_probes_can_create_both_locks_without_forcing_adb_shutdown() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("call-mode-operation.lock"), b"").unwrap();
    std::fs::write(fixture.root.join(".module-adb-interface.lock"), b"").unwrap();
    let revision = fixture.service.0.state.observation().0;
    assert!(
        fixture
            .service
            .0
            .state
            .publish_observation(revision, Status::new("unavailable", "read-only probe"))
    );
    assert!(!fixture.service.requires_runtime_cleanup().await);
    assert!(
        fixture
            .service
            .finish_shutdown_receipt()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(pristine_marker(&fixture.root).unwrap());
}

#[tokio::test]
async fn upgraded_idle_legacy_pair_shutdown_is_local_but_physical_intent_keeps_cleanup() {
    let fixture = Fixture::with_legacy_locks(true);
    assert!(!pristine_marker(&fixture.root).unwrap());
    fixture
        .service
        .0
        .inbox
        .bind_native_module_producer("native-upgraded-locks".into())
        .unwrap();
    let revision = fixture.service.0.state.observation().0;
    assert!(
        fixture
            .service
            .0
            .state
            .publish_observation(revision, Status::new("unavailable", "read-only probe"))
    );
    assert!(!fixture.service.requires_runtime_cleanup().await);
    assert!(
        fixture
            .service
            .finish_shutdown_receipt()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .service
            .finish_shutdown_receipt()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.factory_calls.load(Ordering::Acquire), 0);
    assert!(!pristine_marker(&fixture.root).unwrap());

    let physical = Fixture::with_legacy_locks(true);
    physical
        .service
        .0
        .operation_started
        .store(true, Ordering::Release);
    assert!(physical.service.requires_runtime_cleanup().await);
    physical
        .service
        .0
        .operation_started
        .store(false, Ordering::Release);
    invalidate_pristine_marker(&physical.root).unwrap();
    assert!(physical.service.requires_runtime_cleanup().await);
    assert!(
        !crate::module_recovery::legacy_locks::migrate(
            &physical.root,
            &physical.lifetime.as_ref().unwrap().root_proof()
        )
        .unwrap()
    );
    assert!(
        crate::module_recovery::Gate::inspect_with_lifetime(physical.lifetime.as_ref().unwrap())
            .is_quarantined()
    );
    assert_eq!(physical.factory_calls.load(Ordering::Acquire), 0);
    for name in ["call-mode-operation.lock", ".module-adb-interface.lock"] {
        fs::remove_file(physical.root.join(name)).unwrap();
    }
    initialize_pristine_marker(&physical.root);
    assert!(!pristine_marker(&physical.root).unwrap());
}

#[tokio::test]
async fn physical_intent_revokes_pristine_proof_before_any_device_work() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("call-mode-operation.lock"), b"").unwrap();
    assert!(!fixture.service.requires_runtime_cleanup().await);
    invalidate_pristine_marker(&fixture.root).unwrap();
    assert!(!pristine_marker(&fixture.root).unwrap());
    assert!(fixture.service.requires_runtime_cleanup().await);
}

#[tokio::test]
async fn pristine_captured_producer_shutdown_drains_read_owners_before_noop() {
    let fixture = Fixture::new();
    let pin = ReadPin::new(fixture.service.0.clone());
    let owner = fixture.service.clone();
    let shutdown = tokio::spawn(async move { owner.shutdown_producer(PRODUCER.into()).await });
    until(|| !fixture.service.producer_current(PRODUCER)).await;
    assert!(!shutdown.is_finished());
    drop(pin);
    shutdown.await.unwrap().unwrap();
    assert!(!fixture.root.join("call-mode-operation.lock").exists());
    assert!(!fixture.root.join(".module-adb-interface.lock").exists());
}

#[tokio::test]
async fn read_only_observations_stay_pristine_but_accepted_operation_requires_cleanup() {
    let fixture = Fixture::new();
    let service = &fixture.service;
    for state in ["ready", "preparing", "stopping", "failed", "unknown"] {
        let revision = service.0.state.observation().0;
        assert!(
            service
                .0
                .state
                .publish_observation(revision, Status::new(state, "fixture"))
        );
        assert!(
            !service.requires_runtime_cleanup().await,
            "read-only {state}"
        );
    }
    let operation = service
        .0
        .state
        .begin(
            PRODUCER,
            Status::new("disconnected", "operation still pending"),
        )
        .unwrap();
    assert!(service.requires_runtime_cleanup().await);
    service.0.operation_started.store(true, Ordering::Release);
    service.0.state.end(&operation);
    assert!(service.requires_runtime_cleanup().await);
}

#[tokio::test]
async fn backup_runtime_and_prior_adb_evidence_survive_lazy_lifecycle_reconstruction() {
    let fixture = Fixture::new();
    invalidate_pristine_marker(&fixture.root).unwrap();
    for marker in [
        "call-mode-operation.lock",
        ".module-adb-interface.lock",
        "module-backups",
        "voice-runtime",
    ] {
        let path = fixture.root.join(marker);
        std::fs::write(&path, b"fixture").unwrap();
        let replacement = Service::new(
            &fixture.root,
            fixture.service.0.radio.clone(),
            fixture.service.0.inbox.clone(),
            fixture.service.0.usb_at.clone(),
        );
        assert!(
            replacement.requires_runtime_cleanup().await,
            "ignored {marker}"
        );
        std::fs::remove_file(path).unwrap();
    }
    assert!(!fixture.service.requires_runtime_cleanup().await);
}

#[tokio::test]
async fn disconnected_status_with_backup_or_readiness_facts_does_not_skip_cleanup() {
    let fixture = Fixture::new();
    let mut statuses = Vec::new();
    let mut backup = Status::new("disconnected", "backup obligation");
    backup.backup_path = "/fixture/backup".into();
    statuses.push((backup, true));
    let mut runtime = Status::new("disconnected", "runtime was ready");
    runtime.runtime_downloaded = true;
    statuses.push((runtime, false));
    let mut voice = Status::new("disconnected", "voice was configured");
    voice.voice_configured = true;
    statuses.push((voice, true));
    for (status, requires_cleanup) in statuses {
        let revision = fixture.service.0.state.observation().0;
        assert!(
            fixture
                .service
                .0
                .state
                .publish_observation(revision, status)
        );
        assert_eq!(
            fixture.service.requires_runtime_cleanup().await,
            requires_cleanup
        );
    }
}

#[tokio::test]
async fn unreadable_cleanup_evidence_is_not_treated_as_absent() {
    let fixture = Fixture::new();
    let invalid_parent = fixture.root.join("not-a-directory");
    std::fs::write(&invalid_parent, b"fixture").unwrap();
    let service = Service::new(
        &invalid_parent.join("root"),
        fixture.service.0.radio.clone(),
        fixture.service.0.inbox.clone(),
        fixture.service.0.usb_at.clone(),
    );
    assert!(service.requires_runtime_cleanup().await);
}

#[tokio::test]
async fn retirement_cancels_the_producer_but_waits_for_operation_and_read_cleanup() {
    let fixture = Fixture::new();
    let service = &fixture.service;
    let operation = service
        .0
        .state
        .begin(PRODUCER, Status::new("preparing", "worker active"))
        .unwrap();
    let first = ReadPin::new(service.0.clone());
    let second = ReadPin::new(service.0.clone());
    let owner = service.clone();
    let retirement = tokio::spawn(async move { owner.retire_producer(PRODUCER).await });
    until(|| !service.producer_current(PRODUCER)).await;
    assert!(service.current(&operation).is_err());
    assert!(
        service
            .publish(&operation, Status::new("ready", "late worker result"))
            .is_err()
    );
    assert_eq!(service.0.state.snapshot().state, "preparing");
    assert!(!retirement.is_finished());
    drop(first);
    assert_eq!(service.0.active_reads.load(Ordering::Acquire), 1);
    assert!(!retirement.is_finished());
    // Completion of the business operation alone is not physical read drain.
    assert!(service.0.state.end(&operation));
    service.0.finished.notify_waiters();
    tokio::task::yield_now().await;
    assert!(!retirement.is_finished());
    drop(second);
    tokio::time::timeout(Duration::from_secs(3), retirement)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(service.0.active_reads.load(Ordering::Acquire), 0);
    assert!(!service.0.state.running());
    assert!(!service.producer_current(PRODUCER));
    service
        .0
        .inbox
        .replace_go_producer("replacement-producer".into())
        .unwrap();
    assert!(service.producer_current("replacement-producer"));
}

#[tokio::test]
async fn drain_does_not_lose_completion_notifications_before_or_after_registration() {
    let fixture = Fixture::new();
    for _ in 0..32 {
        let pin = ReadPin::new(fixture.service.0.clone());
        let wait = fixture.service.wait_operations();
        tokio::pin!(wait);
        // Poll the actual waiter until it registers and suspends. The biased
        // branch runs it before the immediately-ready test control branch.
        tokio::select! {
            biased;
            _ = &mut wait => panic!("a held read pin must prevent drain"),
            _ = std::future::ready(()) => {},
        }
        drop(pin);
        tokio::time::timeout(Duration::from_secs(3), &mut wait)
            .await
            .unwrap();
        // The notification can also precede waiter creation entirely.
        let completed = ReadPin::new(fixture.service.0.clone());
        drop(completed);
        tokio::time::timeout(Duration::from_secs(3), fixture.service.wait_operations())
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnected_public_status_waiter_keeps_the_real_observation_owner_pinned() {
    let fixture = Fixture::new();
    let service = &fixture.service;
    let (revision, _) = service.0.state.observation();
    assert!(
        service
            .0
            .state
            .publish_observation(revision, Status::new("needs_download", "cached readiness"))
    );
    *service.0.status_producer.lock().unwrap() = PRODUCER.into();
    // The real refresh creates its ReadPin before observation_producer takes
    // this lock. A cached needs_* state then exits without any radio I/O.
    let blocked_observation = service.0.status_producer.lock().unwrap();
    let observer = service.clone();
    let http_waiter = tokio::spawn(async move { observer.public_status().await });
    until(|| service.0.active_reads.load(Ordering::Acquire) == 1).await;
    http_waiter.abort();
    assert!(http_waiter.await.unwrap_err().is_cancelled());
    assert_eq!(service.0.active_reads.load(Ordering::Acquire), 1);
    let drain_owner = service.clone();
    let drain = tokio::spawn(async move { drain_owner.wait_operations().await });
    tokio::task::yield_now().await;
    assert!(!drain.is_finished());
    drop(blocked_observation);
    tokio::time::timeout(Duration::from_secs(3), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(service.0.active_reads.load(Ordering::Acquire), 0);
    assert_eq!(service.0.state.snapshot().state, "needs_download");
}

#[tokio::test]
async fn stale_operation_or_replaced_producer_cannot_publish_into_current_lifecycle() {
    let fixture = Fixture::new();
    let service = &fixture.service;
    let old_observation = service.0.state.observation().0;
    let first = service
        .0
        .state
        .begin(PRODUCER, Status::new("downloading", "first"))
        .unwrap();
    assert!(
        service
            .publish(&first, Status::new("preparing", "first completion"))
            .is_ok()
    );
    assert!(service.0.state.end(&first));
    let second = service
        .0
        .state
        .begin(PRODUCER, Status::new("restoring_usb", "second"))
        .unwrap();
    assert!(
        service
            .publish(&first, Status::new("ready", "stale completion"))
            .is_err()
    );
    assert!(
        !service
            .0
            .state
            .publish_observation(old_observation, Status::new("ready", "old read"))
    );
    assert_eq!(service.0.state.snapshot().state, "restoring_usb");
    service
        .0
        .inbox
        .replace_go_producer("replacement-producer".into())
        .unwrap();
    assert!(
        service
            .publish(&second, Status::new("ready", "old producer completion"))
            .is_err()
    );
    assert!(service.0.state.running());
    assert!(
        service
            .0
            .state
            .begin(
                "replacement-producer",
                Status::new("preparing", "early replacement")
            )
            .is_none()
    );
    // Only the old owner's real cleanup permits the replacement operation.
    service.0.state.end(&second);
    let current = service
        .0
        .state
        .begin(
            "replacement-producer",
            Status::new("preparing", "replacement"),
        )
        .unwrap();
    assert!(
        service
            .publish(&current, Status::new("ready", "current completion"))
            .is_ok()
    );
    assert_eq!(service.0.state.snapshot().state, "ready");
    service.0.state.end(&current);
}

#[test]
fn restore_binding_uses_strict_imei_and_vid_pid_but_allows_expected_configuration_differences() {
    let original = UsbComposition {
        vendor_id: 0x2c7c,
        product_id: 0x2400,
        flags: [1, 0, 1, 0, 1, 0, 0],
    };
    let backup = crate::call_mode_backups::NativeBackup {
        saved_at: timestamp(),
        module: CallModeModuleIdentity {
            imei: "123456789012345".into(),
            firmware: String::new(),
        },
        usb: original,
        voice: None,
    };
    let mut changed = original;
    changed.flags[5] = 1;
    changed.flags[6] = 1;
    assert!(validate_restore_binding(&backup, &backup.module.imei, changed).is_ok());
    for imei in ["987654321098765", "123", "12345678901234x"] {
        let error = validate_restore_binding(&backup, imei, original).unwrap_err();
        assert_eq!(error.detail, "备份所属模块与当前连接模块的 IMEI 不一致");
    }
    for (vid, pid) in [(0x2c7d, 0x2400), (0x2c7c, 0x2401)] {
        let mut different = original;
        different.vendor_id = vid;
        different.product_id = pid;
        let error = validate_restore_binding(&backup, &backup.module.imei, different).unwrap_err();
        assert_eq!(error.detail, "备份与当前模块的 USB VID/PID 不一致");
    }
}

#[tokio::test]
async fn stale_captured_shutdown_revalidates_after_gate_without_stopping_replacement() {
    let fixture = Fixture::new();
    let service = &fixture.service;
    let shutdown_gate = service.0.shutdown_gate.lock().await;
    let owner = service.clone();
    let (started, waiting) = oneshot::channel();
    let shutdown = tokio::spawn(async move {
        let _ = started.send(());
        owner.shutdown_producer(PRODUCER.into()).await
    });
    waiting.await.unwrap();
    service
        .0
        .inbox
        .replace_go_producer("replacement-producer".into())
        .unwrap();
    assert!(!shutdown.is_finished());
    drop(shutdown_gate);
    let error = tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error, "通话模式退出请求所属后端已过期");
    assert!(!service.0.stopping.load(Ordering::Acquire));
    assert!(service.0.cancelled_producer.lock().unwrap().is_none());
    assert!(service.producer_current("replacement-producer"));
    assert!(service.0.shutdown_gate.try_lock().is_ok());
}

#[tokio::test]
async fn producer_retirement_cannot_overtake_captured_shutdown_cleanup_gate() {
    let fixture = Fixture::new();
    let service = &fixture.service;
    let physical_read = ReadPin::new(service.0.clone());
    let shutdown_gate = service.0.shutdown_gate.lock().await;
    let owner = service.clone();
    let retirement = tokio::spawn(async move { owner.retire_producer(PRODUCER).await });
    until(|| !service.producer_current(PRODUCER)).await;
    assert!(!retirement.is_finished());
    drop(physical_read);
    until(|| service.0.active_reads.load(Ordering::Acquire) == 0).await;
    // Read/operation drain is complete, but shutdown's real cleanup proof is
    // still locked. Merely observing zero read pins must not admit replacement.
    tokio::task::yield_now().await;
    assert!(!retirement.is_finished());
    drop(shutdown_gate);
    tokio::time::timeout(Duration::from_secs(3), retirement)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(service.0.shutdown_gate.try_lock().is_ok());
}

#[tokio::test]
async fn managed_go_retirement_does_not_cancel_or_wait_for_native_callmode_work() {
    let fixture = Fixture::new();
    let service = &fixture.service;
    service
        .0
        .inbox
        .bind_native_module_producer("rust-module".into())
        .unwrap();
    let operation = service
        .0
        .state
        .begin(
            "rust-module",
            Status::new("preparing", "retained native work"),
        )
        .unwrap();
    let read = ReadPin::new(service.0.clone());
    // This must finish even while the unrelated native physical operation is active.
    tokio::time::timeout(Duration::from_secs(1), service.retire_producer(PRODUCER))
        .await
        .unwrap()
        .unwrap();
    service
        .0
        .inbox
        .replace_go_producer("replacement-go".into())
        .unwrap();
    assert!(service.current(&operation).is_ok());
    assert_eq!(
        service.observation_producer().as_deref(),
        Some("rust-module")
    );
    assert!(service.0.cancelled_producer.lock().unwrap().is_none());
    assert!(!service.producer_current(PRODUCER));
    assert!(!service.producer_current("replacement-go"));
    service
        .publish(
            &operation,
            Status::new("ready", "confirmed native completion"),
        )
        .unwrap();
    assert_eq!(service.0.state.snapshot().state, "ready");
    assert_eq!(service.0.active_reads.load(Ordering::Acquire), 1);
    drop(read);
    assert!(service.0.state.end(&operation));
    service.0.finished.notify_waiters();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_rotation_during_retained_status_read_keeps_native_readiness_and_details() {
    let fixture = Fixture::new();
    let service = &fixture.service;
    service
        .0
        .inbox
        .bind_native_module_producer("rust-module".into())
        .unwrap();
    *service.0.status_producer.lock().unwrap() = "rust-module".into();
    let revision = service.0.state.observation().0;
    let mut expected = Status::new("needs_download", "native runtime missing");
    expected.detail = "retained native observation".into();
    assert!(service.0.state.publish_observation(revision, expected));
    // Pause the real retained refresh after it owns a read pin but before it
    // checks the observation producer. A Go rotation must not reset the native
    // status or send this read down the disconnected physical-probe branch.
    let blocked_observation = service.0.status_producer.lock().unwrap();
    let owner = service.clone();
    let observer = tokio::spawn(async move { owner.public_status().await });
    until(|| service.0.active_reads.load(Ordering::Acquire) == 1).await;
    service.retire_producer(PRODUCER).await.unwrap();
    service
        .0
        .inbox
        .replace_go_producer("replacement-go".into())
        .unwrap();
    drop(blocked_observation);
    let observed = tokio::time::timeout(Duration::from_secs(3), observer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.state, "needs_download");
    assert_eq!(observed.detail, "retained native observation");
    assert_eq!(*service.0.status_producer.lock().unwrap(), "rust-module");
    assert_eq!(service.0.active_reads.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn final_pristine_receipt_waits_for_read_owner_and_is_reusable_without_adb() {
    let fixture = Fixture::new();
    let pin = ReadPin::new(fixture.service.0.clone());
    let owner = fixture.service.clone();
    let task = tokio::spawn(async move { owner.finish_shutdown_receipt().await });
    until(|| fixture.service.0.stopping.load(Ordering::Acquire)).await;
    assert!(!task.is_finished());
    drop(pin);
    assert!(task.await.unwrap().unwrap().is_empty());
    assert!(
        fixture
            .service
            .finish_shutdown_receipt()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!fixture.root.join("call-mode-operation.lock").exists());
    assert!(!fixture.root.join(".module-adb-interface.lock").exists());
}

#[path = "finalization_tests.rs"]
mod finalization_tests;

async fn invalidated_producer_retains_original_observation(identity_change: &str) {
    let mut radio = crate::module_radio::tests::tests::Fixture::new();
    radio.configure().await;
    let inbox = crate::message_archive::InboxState::for_go_producer("producer-1".into());
    let service = Service::new(
        &radio.root,
        radio.service.clone(),
        inbox.clone(),
        crate::usb_at::Owner::new("producer-1".into()),
    );
    let mut probe = crate::call_mode_adb::child_tests::HeldProbe::new("replaced-status-producer");
    std::fs::remove_dir_all(&probe.root).unwrap();
    probe.root = radio.root.clone();
    service.install_refresh_probe_for_tests(probe.clone());
    let original_owner = service.clone();
    let observer = tokio::spawn(async move { original_owner.public_status().await });
    let status = tokio::time::timeout(Duration::from_secs(3), observer)
        .await
        .unwrap()
        .unwrap();
    assert!(!status.can_retry && status.detail.contains("尚未确认"));
    assert!(probe.alive() && probe.lock_busy());
    let token = service
        .0
        .active_observation
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .token
        .clone();
    // Explicitly model an actionable snapshot belonging to the old producer.
    // Snapshot storage alone is not an authority for the replacement identity.
    let (revision, _) = service.0.state.observation();
    let mut stale = Status::new("needs_usb", "old producer configuration");
    stale.can_enable = true;
    stale.can_download = true;
    stale.can_retry = false;
    assert!(service.0.state.publish_observation(revision, stale));
    match identity_change {
        "replacement" => inbox.replace_go_producer("producer-2".into()).unwrap(),
        "absent" => inbox.clear_module_producers_for_tests(),
        "revoked" => *service.0.cancelled_producer.lock().unwrap() = Some("producer-1".into()),
        _ => panic!("unknown controlled identity transition"),
    }
    assert!(
        service.pending_adb_status().is_none(),
        "old pending metadata must stay identity scoped"
    );
    for _ in 0..4 {
        let status = tokio::time::timeout(Duration::from_millis(300), service.public_status())
            .await
            .unwrap();
        assert!(!status.can_enable && !status.can_download && !status.can_retry);
        let reply = tokio::time::timeout(Duration::from_millis(300), service.retry())
            .await
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&reply.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(value["can_enable"], false);
        assert_eq!(value["can_download"], false);
        assert_eq!(value["can_retry"], false);
        assert_eq!(service.active_status_reads_for_tests(), 1);
        assert!(Arc::ptr_eq(
            &service
                .0
                .active_observation
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .token,
            &token
        ));
        assert_eq!(probe.spawned.load(Ordering::Acquire), 1);
    }
    assert!(radio.service.reserve_call_mode_operation().await.is_err());
    radio.empty();
    probe.release();
    until(|| service.active_status_reads_for_tests() == 0).await;
    assert!(service.0.active_observation.lock().unwrap().is_none());
    assert!(!probe.alive() && !probe.lock_busy());
    radio.empty();
}

#[tokio::test]
async fn replaced_producer_cannot_inherit_controls_while_original_observation_is_active() {
    invalidated_producer_retains_original_observation("replacement").await;
}
#[tokio::test]
async fn absent_producer_cannot_inherit_controls_while_original_observation_is_active() {
    invalidated_producer_retains_original_observation("absent").await;
}
#[tokio::test]
async fn revoked_producer_cannot_inherit_controls_while_original_observation_is_active() {
    invalidated_producer_retains_original_observation("revoked").await;
}

async fn invalid_identity_retry_masks_cached_controls_without_physical_reads(revoked: bool) {
    let fixture = Fixture::new();
    let service = &fixture.service;
    let (revision, _) = service.0.state.observation();
    let mut stale = Status::new("needs_usb", "retired producer configuration");
    stale.can_enable = true;
    stale.can_download = true;
    stale.can_retry = false;
    assert!(service.0.state.publish_observation(revision, stale));
    if revoked {
        *service.0.cancelled_producer.lock().unwrap() = Some(PRODUCER.into());
    } else {
        service.0.inbox.clear_module_producers_for_tests();
    }
    assert!(service.0.active_observation.lock().unwrap().is_none());
    for _ in 0..4 {
        let reply = tokio::time::timeout(Duration::from_millis(300), service.retry())
            .await
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&reply.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(value["can_enable"], false);
        assert_eq!(value["can_download"], false);
        assert_eq!(value["can_retry"], false);
        let status = tokio::time::timeout(Duration::from_millis(300), service.public_status())
            .await
            .unwrap();
        assert!(!status.can_enable && !status.can_download && !status.can_retry);
        assert_eq!(service.active_status_reads_for_tests(), 0);
        assert!(service.0.active_observation.lock().unwrap().is_none());
    }
    assert_eq!(fixture.factory_calls.load(Ordering::Acquire), 0);
    assert!(
        service.0.state.snapshot().can_enable,
        "projection must not rewrite cached ownership facts"
    );
    assert!(!fixture.root.join("call-mode-operation.lock").exists());
    assert!(!fixture.root.join(".module-adb-interface.lock").exists());
}
#[tokio::test]
async fn absent_producer_retry_masks_actionable_cache_without_creating_a_read_owner() {
    invalid_identity_retry_masks_cached_controls_without_physical_reads(false).await;
}
#[tokio::test]
async fn revoked_producer_retry_masks_actionable_cache_without_creating_a_read_owner() {
    invalid_identity_retry_masks_cached_controls_without_physical_reads(true).await;
}

#[tokio::test]
async fn original_status_observer_masks_stale_controls_after_producer_changes_before_real_child_reap()
 {
    let mut radio = crate::module_radio::tests::tests::Fixture::new();
    radio.configure().await;
    let inbox = crate::message_archive::InboxState::for_go_producer("producer-1".into());
    let service = Service::new(
        &radio.root,
        radio.service.clone(),
        inbox.clone(),
        crate::usb_at::Owner::new("producer-1".into()),
    );
    let mut probe =
        crate::call_mode_adb::child_tests::HeldProbe::new("status-join-replaced-producer");
    std::fs::remove_dir_all(&probe.root).unwrap();
    probe.root = radio.root.clone();
    probe.timeout = Duration::from_secs(3);
    service.install_refresh_probe_for_tests(probe.clone());
    let original_owner = service.clone();
    let observer = tokio::spawn(async move { original_owner.public_status().await });
    until(|| probe.root.join("child-ready").exists()).await;
    assert_eq!(service.active_status_reads_for_tests(), 1);
    assert!(probe.alive() && probe.lock_busy());
    assert!(radio.service.reserve_call_mode_operation().await.is_err());
    let (revision, _) = service.0.state.observation();
    let mut stale = Status::new("needs_usb", "old producer configuration");
    stale.can_enable = true;
    stale.can_download = true;
    stale.can_retry = true;
    assert!(service.0.state.publish_observation(revision, stale));
    inbox.replace_go_producer("producer-2".into()).unwrap();
    // Exit normally before the deadline, so no pending event can deliver an
    // earlier conservative response. Exercise the real refresh join boundary.
    std::fs::write(probe.root.join("child-stop"), []).unwrap();
    let status = tokio::time::timeout(Duration::from_secs(3), observer)
        .await
        .unwrap()
        .unwrap();
    assert!(!status.can_enable && !status.can_download && !status.can_retry);
    assert_eq!(service.active_status_reads_for_tests(), 0);
    assert!(service.0.active_observation.lock().unwrap().is_none());
    assert!(!probe.alive() && !probe.lock_busy());
    assert!(crate::call_mode_adb::pending_probe(&probe.root, "producer-1").is_none());
    assert_eq!(probe.spawned.load(Ordering::Acquire), 1);
    assert!(
        service.0.state.snapshot().can_enable,
        "handoff masks projection without rewriting old facts"
    );
    radio.empty();
}
