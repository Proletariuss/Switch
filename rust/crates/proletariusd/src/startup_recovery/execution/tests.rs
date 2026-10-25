use super::*;
use crate::call_mode_restore_journal::{Intent, Journal, Mutation, MutationKind};
use crate::daemon_lifetime::Lease;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::AtomicUsize;

struct Fixture {
    root: PathBuf,
    lease: Option<Lease>,
}
impl Fixture {
    fn empty() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/iteration-591-startup-physical-owner")
            .join(crate::snapshot::new_instance_id());
        let lease = Lease::acquire(&root).unwrap();
        Self {
            root,
            lease: Some(lease),
        }
    }
    fn proof(&self) -> crate::daemon_lifetime::RootProof {
        self.lease.as_ref().unwrap().root_proof()
    }
    fn gate(&self) -> Gate {
        Gate::inspect_with_lifetime(self.lease.as_ref().unwrap())
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        use std::io::Write;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.root.join(name))
            .unwrap();
        file.write_all(bytes).unwrap();
    }
    fn locks(&self) {
        self.write("call-mode-operation.lock", b"");
        self.write(".module-adb-interface.lock", b"");
    }
    async fn pending(bound: bool) -> Self {
        let f = Self::empty();
        let usb = UsbComposition {
            vendor_id: 0x2c7c,
            product_id: 0x2400,
            flags: [0; 7],
        };
        let voice = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        let id = crate::call_mode_backups::create_native_backup(
            &f.root,
            CallModeModuleIdentity {
                imei: "123456789012345".into(),
                firmware: "private-fixture-firmware".into(),
            },
            usb,
            voice,
            false,
            &f.proof(),
        )
        .await
        .unwrap();
        let backup = crate::call_mode_backups::capture_native_backup(&f.root, &id, &f.proof())
            .await
            .unwrap();
        let binding = bound.then(|| {
            serde_json::from_value(serde_json::json!({
                "schema_version": 1, "vendor_id": 0x2c7c, "product_id": 0x0125,
                "bus_number": 1, "port_numbers": [2, 3], "serial_language": 1033,
                "serial_number": "private-exact-original-serial", "interface_number": 2,
                "alternate_setting": 0, "endpoint_in": 0x81, "endpoint_out": 0x02
            }))
            .unwrap()
        });
        let journal = Journal::begin(
            &f.root,
            &f.proof(),
            &backup,
            Intent {
                producer_id: "original-producer".into(),
                reservation_token: "original-reservation".into(),
                reservation_sequence: 1,
                operation_id: "original-restore".into(),
                module_imei: "123456789012345".into(),
                current_usb: usb,
                current_voice: voice,
                target_usb: usb,
                target_voice: Some(voice),
                recovery_binding: binding,
                voice_recovery_binding: None,
            },
        )
        .await
        .unwrap();
        journal
            .safety_checkpoint(backup.id(), &backup.sha256())
            .unwrap();
        // A real durable Prepare with no terminal result stays historical Unknown.
        let _old_unknown = journal
            .prepare_step(Mutation {
                operation_id: "original-unknown-write".into(),
                kind: MutationKind::Ims { target: 1 },
            })
            .unwrap();
        f.locks();
        f
    }
    fn reacquire(&self) {
        let operation = acquire_bound_existing_recovery_lock(
            &self.root,
            &self.proof(),
            RecoveryLockKind::Operation,
            false,
        )
        .unwrap();
        let adb = acquire_bound_existing_recovery_lock(
            &self.root,
            &self.proof(),
            RecoveryLockKind::AdbInterface,
            false,
        )
        .unwrap();
        adb.release().unwrap();
        operation.release().unwrap();
    }
    fn owner(&self, gate: &Gate) -> ExecutionOwner {
        ExecutionOwner::begin(
            gate,
            &self.root,
            Arc::new(AtomicBool::new(false)),
            Instant::now() + READ_BUDGET,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.lease.take()); // Keep evidence for root-owned verification.
    }
}
fn native_error() -> TransportError {
    TransportError {
        code: "scripted_read_ended",
        message: "scripted read ended".into(),
        response: "private response must not be logged".into(),
        uncertain: false,
    }
}

#[tokio::test]
async fn actual_typed_bound_unknown_mints_only_one_finite_scope_then_checks_both_releases() {
    let f = Fixture::pending(true).await;
    let gate = f.gate();
    let original = crate::call_mode_restore_journal::capture_for_recovery(&f.root, &f.proof())
        .unwrap()
        .unwrap();
    assert_eq!(original.summary().unknown_steps, 1);
    let calls = AtomicUsize::new(0);
    let escaped = Mutex::new(None);
    let result = run_with_native(
        gate.clone(),
        f.root.clone(),
        Arc::new(AtomicBool::new(false)),
        |scope| {
            calls.fetch_add(1, Ordering::AcqRel);
            scope.check().unwrap();
            assert_eq!(scope.expected_imei(), "123456789012345");
            assert!(scope.deadline() > Instant::now());
            assert_eq!(
                scope.binding(),
                original.intent().recovery_binding.as_ref().unwrap()
            );
            let admission = scope.admission();
            assert!(admission());
            *escaped.lock().unwrap() = Some(admission);
            assert!(
                acquire_bound_existing_recovery_lock(
                    &f.root,
                    &f.proof(),
                    RecoveryLockKind::Operation,
                    false
                )
                .is_err()
            );
            assert!(
                acquire_bound_existing_recovery_lock(
                    &f.root,
                    &f.proof(),
                    RecoveryLockKind::AdbInterface,
                    false
                )
                .is_err()
            );
            Err(native_error())
        },
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("scripted failure became success"),
    };
    assert!(!error.to_string().contains("private response"));
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert!(!escaped.lock().unwrap().as_ref().unwrap()());
    f.reacquire();
    original.verify_unchanged(&f.proof()).unwrap();
    assert_eq!(original.summary().unknown_steps, 1);
    assert!(gate.admit_operation("ordinary-after-native-read").is_err());
}

#[tokio::test]
async fn unbound_pending_journal_is_rejected_before_any_native_context_and_without_mutation() {
    let f = Fixture::pending(false).await;
    let gate = f.gate();
    let original = crate::call_mode_restore_journal::capture_for_recovery(&f.root, &f.proof())
        .unwrap()
        .unwrap();
    let result = run_with_native(
        gate.clone(),
        f.root.clone(),
        Arc::new(AtomicBool::new(false)),
        |_| panic!("unbound journal reached native Context"),
    );
    assert!(result.is_err());
    original.verify_unchanged(&f.proof()).unwrap();
    f.reacquire();
    assert!(gate.is_quarantined());
}

#[test]
fn no_typed_journal_does_not_invent_a_policy_or_touch_native() {
    let f = Fixture::empty();
    f.write(
        crate::module_recovery::LEGACY_EXECUTION_PENDING,
        b"unknown original history",
    );
    f.locks();
    let result = run_with_native(
        f.gate(),
        f.root.clone(),
        Arc::new(AtomicBool::new(false)),
        |_| panic!("metadata-only legacy evidence reached native Context"),
    );
    assert!(matches!(result, Ok(None)));
    f.reacquire();
}

#[tokio::test]
async fn a_failed_native_attempt_cannot_select_again_and_escaped_fence_cannot_outlive_owner() {
    let f = Fixture::pending(true).await;
    let gate = f.gate();
    let mut owner = f.owner(&gate);
    assert!(owner.approve().unwrap());
    let escaped = Mutex::new(None);
    assert!(
        owner
            .read_with(|scope| {
                *escaped.lock().unwrap() = Some(scope.admission());
                Err(native_error())
            })
            .is_err()
    );
    assert!(
        owner
            .read_with(|_| panic!("a second fresh selection was attempted"))
            .is_err()
    );
    let witnesses = owner.release_locks().unwrap();
    for witness in &witnesses {
        witness.validate().unwrap();
    }
    assert!(!escaped.lock().unwrap().as_ref().unwrap()());
    drop(owner);
    assert!(!escaped.lock().unwrap().as_ref().unwrap()());
    f.reacquire();
}

#[tokio::test]
async fn original_evidence_change_deadline_and_cancellation_each_block_native() {
    for failure in ["evidence", "deadline", "cancel"] {
        let f = Fixture::pending(true).await;
        let gate = f.gate();
        let mut owner = f.owner(&gate);
        owner.approve().unwrap();
        match failure {
            "evidence" => f.write(".call-mode-pristine-v1", b"changed original evidence"),
            "deadline" => Arc::get_mut(&mut owner.state).unwrap().deadline = Instant::now(),
            "cancel" => owner.state.cancelled.store(true, Ordering::Release),
            _ => unreachable!(),
        }
        assert!(
            owner
                .read_with(|_| panic!("revoked scope reached native Context"))
                .is_err(),
            "{failure}"
        );
        owner.release_locks().unwrap();
        f.reacquire();
    }
}

#[tokio::test]
async fn busy_second_lock_explicitly_releases_original_first_lock() {
    let f = Fixture::pending(true).await;
    let gate = f.gate();
    let held_adb = acquire_bound_existing_recovery_lock(
        &f.root,
        &f.proof(),
        RecoveryLockKind::AdbInterface,
        false,
    )
    .unwrap();
    assert!(
        ExecutionOwner::begin(
            &gate,
            &f.root,
            Arc::new(AtomicBool::new(false)),
            Instant::now() + READ_BUDGET
        )
        .is_err()
    );
    let operation = acquire_bound_existing_recovery_lock(
        &f.root,
        &f.proof(),
        RecoveryLockKind::Operation,
        false,
    )
    .unwrap();
    operation.release().unwrap();
    held_adb.release().unwrap();
}

#[tokio::test]
async fn foreign_gate_and_ended_lifetime_revoke_physical_fence_but_not_original_release() {
    let mut f = Fixture::pending(true).await;
    let gate = f.gate();
    let mut owner = f.owner(&gate);
    owner.approve().unwrap();
    let foreign_gate = f.gate();
    assert!(owner.state.lineage.validate(&foreign_gate).is_err());
    drop(f.lease.take());
    assert!(
        owner
            .read_with(|_| panic!("ended root reached native Context"))
            .is_err()
    );
    // Root loss prevents success witnesses, while release still closes both
    // originally held kernel resources. Drop is not counted as completion.
    assert!(owner.release_locks().is_err());
    assert!(owner.state.locks.lock().unwrap().operation.is_none());
    assert!(owner.state.locks.lock().unwrap().adb.is_none());
}

#[tokio::test]
async fn cancelled_async_waiter_keeps_real_locks_until_the_blocking_native_worker_returns() {
    let f = Fixture::pending(true).await;
    let gate = f.gate();
    let root = f.root.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (continue_tx, continue_rx) = std::sync::mpsc::channel();
    let (fenced_tx, fenced_rx) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        inspect_with_native(
            &gate,
            &root,
            Arc::new(AtomicBool::new(false)),
            move |scope| {
                assert!(scope.admission()());
                started_tx.send(()).unwrap();
                continue_rx.recv().unwrap();
                fenced_tx.send(scope.check().is_err()).unwrap();
                Err(native_error())
            },
        )
        .await
    });
    started_rx.await.unwrap();
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    assert!(
        acquire_bound_existing_recovery_lock(
            &f.root,
            &f.proof(),
            RecoveryLockKind::Operation,
            false
        )
        .is_err()
    );
    assert!(
        acquire_bound_existing_recovery_lock(
            &f.root,
            &f.proof(),
            RecoveryLockKind::AdbInterface,
            false
        )
        .is_err()
    );
    continue_tx.send(()).unwrap();
    assert!(fenced_rx.await.unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(operation) = acquire_bound_existing_recovery_lock(
            &f.root,
            &f.proof(),
            RecoveryLockKind::Operation,
            false,
        ) {
            operation.release().unwrap();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "retained worker did not release operation lock"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    f.reacquire();
}

#[tokio::test]
async fn shared_native_state_machine_success_returns_only_closed_provisional_facts() {
    use crate::usb_at_transport::recovery::{OwnerFixtureScenario, scripted_read_for_owner};
    let f = Fixture::pending(true).await;
    let gate = f.gate();
    let original = crate::call_mode_restore_journal::capture_for_recovery(&f.root, &f.proof())
        .unwrap()
        .unwrap();
    let escaped = Mutex::new(None);
    let inspection = run_with_native(
        gate.clone(),
        f.root.clone(),
        Arc::new(AtomicBool::new(false)),
        |scope| {
            *escaped.lock().unwrap() = Some(scope.admission());
            scripted_read_for_owner(scope, OwnerFixtureScenario::Success, None)
        },
    )
    .unwrap()
    .expect("eligible typed source has production-shared readback");
    assert_eq!(inspection.identity().imei, "123456789012345");
    assert!(inspection.usb_composition().valid());
    assert!(inspection.voice().valid());
    assert_eq!(
        inspection.close_disposition(),
        CloseDisposition::ReleasedAndHandleDropped
    );
    assert!(!escaped.lock().unwrap().as_ref().unwrap()());
    inspection.verify(&gate).unwrap();
    assert!(inspection.verify(&f.gate()).is_err());
    assert_eq!(inspection.startup_logging().scope, "physical_read_only");
    assert!(!inspection.startup_logging().recovery_completed);
    assert!(inspection.startup_logging().ordinary_quarantine_preserved);
    let diagnostic = serde_json::to_string(inspection.startup_logging()).unwrap();
    for secret in [
        "123456789012345",
        "private-exact-original-serial",
        "private-fixture-firmware",
        "private response",
    ] {
        assert!(!diagnostic.contains(secret));
    }
    f.reacquire();
    original.verify_unchanged(&f.proof()).unwrap();
    assert_eq!(original.summary().unknown_steps, 1);
    assert!(gate.admit_operation("ordinary-after-closed-read").is_err());
    f.write(".call-mode-pristine-v1", b"new unaccounted evidence");
    assert!(inspection.verify(&gate).is_err());
}

#[tokio::test]
async fn shared_native_checked_close_error_retains_typed_debt_and_still_releases_both_locks() {
    use crate::usb_at_transport::recovery::{OwnerFixtureScenario, scripted_read_for_owner};
    let f = Fixture::pending(true).await;
    let result = run_with_native(
        f.gate(),
        f.root.clone(),
        Arc::new(AtomicBool::new(false)),
        |scope| scripted_read_for_owner(scope, OwnerFixtureScenario::CloseError, None),
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("unconfirmed native close became observation"),
    };
    assert!(error.cleanup_unconfirmed());
    assert_eq!(error.to_string(), "usb_at_close_unconfirmed");
    f.reacquire();
}

#[tokio::test]
async fn shared_native_disconnected_close_retains_truthful_retirement_disposition_only() {
    use crate::usb_at_transport::recovery::{OwnerFixtureScenario, scripted_read_for_owner};
    let f = Fixture::pending(true).await;
    let gate = f.gate();
    let inspection = run_with_native(
        gate.clone(),
        f.root.clone(),
        Arc::new(AtomicBool::new(false)),
        |scope| scripted_read_for_owner(scope, OwnerFixtureScenario::Disconnected, None),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        inspection.close_disposition(),
        CloseDisposition::DisconnectedAndHandleDropped
    );
    assert_eq!(
        inspection.startup_logging().native_close,
        "disconnected_and_handle_dropped"
    );
    assert!(!inspection.startup_logging().recovery_completed);
    inspection.verify(&gate).unwrap();
    f.reacquire();
}

#[tokio::test]
async fn shared_native_mid_read_cancellation_checks_closure_and_returns_pending_without_false_debt()
{
    use crate::usb_at_transport::recovery::{OwnerFixtureScenario, scripted_read_for_owner};
    let f = Fixture::pending(true).await;
    let cancelled = Arc::new(AtomicBool::new(false));
    let callback_cancelled = cancelled.clone();
    let result = run_with_native(f.gate(), f.root.clone(), cancelled, |scope| {
        scripted_read_for_owner(
            scope,
            OwnerFixtureScenario::Success,
            Some(Box::new(move |ordinal| {
                if ordinal == 0 {
                    callback_cancelled.store(true, Ordering::Release);
                }
            })),
        )
    });
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("cancelled physical read became observation"),
    };
    assert!(
        !error.cleanup_unconfirmed(),
        "the shared native path explicitly closed its transport"
    );
    f.reacquire();
}

#[tokio::test]
async fn shared_native_callback_panic_is_retained_as_unconfirmed_even_after_checked_retirement() {
    use crate::usb_at_transport::recovery::{OwnerFixtureScenario, scripted_read_for_owner};
    let f = Fixture::pending(true).await;
    let result = run_with_native(
        f.gate(),
        f.root.clone(),
        Arc::new(AtomicBool::new(false)),
        |scope| {
            scripted_read_for_owner(
                scope,
                OwnerFixtureScenario::Success,
                Some(Box::new(|ordinal| {
                    if ordinal == 0 {
                        panic!("scripted callback panic")
                    }
                })),
            )
        },
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("callback panic became observation"),
    };
    assert!(error.cleanup_unconfirmed());
    assert_eq!(error.to_string(), "startup_native_callback_panicked");
    f.reacquire();
}

#[tokio::test]
async fn namespace_loss_does_not_prevent_actual_original_lock_release_and_cannot_be_success() {
    use crate::usb_at_transport::recovery::{OwnerFixtureScenario, scripted_read_for_owner};
    let f = Fixture::pending(true).await;
    let root = f.root.clone();
    let result = run_with_native(
        f.gate(),
        f.root.clone(),
        Arc::new(AtomicBool::new(false)),
        |scope| {
            scripted_read_for_owner(
                scope,
                OwnerFixtureScenario::Success,
                Some(Box::new(move |ordinal| {
                    if ordinal == 0 {
                        fs::rename(
                            root.join(".module-adb-interface.lock"),
                            root.join("renamed-original-adb.lock"),
                        )
                        .unwrap();
                    }
                })),
            )
        },
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("namespace loss became success"),
    };
    assert!(error.cleanup_unconfirmed());
    let original = OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.root.join("renamed-original-adb.lock"))
        .unwrap();
    original
        .try_lock()
        .expect("actual original ADB OFD must have been unlocked and closed");
    original.unlock().unwrap();
    let operation = acquire_bound_existing_recovery_lock(
        &f.root,
        &f.proof(),
        RecoveryLockKind::Operation,
        false,
    )
    .unwrap();
    operation.release().unwrap();
}

#[tokio::test]
async fn poisoned_ownership_boundary_releases_both_original_locks_but_preserves_typed_debt() {
    let f = Fixture::pending(true).await;
    let gate = f.gate();
    let mut owner = f.owner(&gate);
    owner.approve().unwrap();
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = owner.state.locks.lock().unwrap();
        panic!("scripted ownership mutex panic");
    }));
    assert!(poisoned.is_err());
    assert!(owner.state.check().is_err());
    let error = owner.release_locks().unwrap_err();
    assert!(error.cleanup_unconfirmed());
    f.reacquire();
}
