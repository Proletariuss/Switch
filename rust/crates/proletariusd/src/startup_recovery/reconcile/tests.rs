use super::*;
use crate::call_mode_adb::startup_recovery::{
    ManagedVoiceArrivalProbe, ManagedVoiceScenario, OriginalManagedVoiceFixture,
};
use crate::call_mode_restore_journal::{Journal, Mutation, MutationKind, Receipt};
use crate::module_radio_transport::CallModeModuleIdentity;
use crate::usb_at_transport::recovery::reconcile::{
    ReconciliationArrivalProbe, ReconciliationFixtureScenario as Scenario,
    scripted_reconcile_observed, scripted_reconcile_owned_observed,
};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

struct Fixture {
    root: PathBuf,
    lease: Option<crate::daemon_lifetime::Lease>,
    owned_voice: Option<Arc<OriginalManagedVoiceFixture>>,
}
impl Fixture {
    async fn pending() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/iteration-592-reconciliation")
            .join(crate::snapshot::new_instance_id());
        Self::pending_at(root, false).await
    }
    async fn owned_pending() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/iteration-593-owned-reconciliation")
            .join(crate::snapshot::new_instance_id());
        Self::pending_at(root, true).await
    }
    async fn pending_at(root: PathBuf, owned: bool) -> Self {
        Self::pending_with_upgrade(root, owned, false).await
    }
    async fn pending_with_upgrade(root: PathBuf, owned: bool, upgraded: bool) -> Self {
        let lease = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        if upgraded {
            for name in ["call-mode-operation.lock", ".module-adb-interface.lock"] {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(root.join(name))
                    .unwrap();
            }
            assert!(
                crate::module_recovery::legacy_locks::migrate(&root, &lease.root_proof()).unwrap()
            );
            crate::call_mode_lifecycle::invalidate_pristine_marker(&root).unwrap();
        }
        let owned_voice = if owned {
            Some(Arc::new(
                OriginalManagedVoiceFixture::create(
                    &root,
                    &lease.root_proof(),
                    "original-producer",
                    1,
                )
                .unwrap(),
            ))
        } else {
            None
        };
        let f = Self {
            root,
            lease: Some(lease),
            owned_voice,
        };
        let id = crate::call_mode_backups::create_native_backup(
            &f.root,
            identity(),
            usb(),
            voice(),
            false,
            &f.proof(),
        )
        .await
        .unwrap();
        let backup = crate::call_mode_backups::capture_native_backup(&f.root, &id, &f.proof())
            .await
            .unwrap();
        let mut original_intent = intent();
        original_intent.voice_recovery_binding =
            f.owned_voice.as_ref().map(|voice| voice.binding());
        let journal = Journal::begin(&f.root, &f.proof(), &backup, original_intent)
            .await
            .unwrap();
        journal
            .safety_checkpoint(backup.id(), &backup.sha256())
            .unwrap();
        let _unknown = journal
            .prepare_step(Mutation {
                operation_id: "original-unknown".into(),
                kind: MutationKind::Ims { target: 1 },
            })
            .unwrap();
        for name in ["call-mode-operation.lock", ".module-adb-interface.lock"]
            .into_iter()
            .filter(|_| !upgraded)
        {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(f.root.join(name))
                .unwrap();
        }
        f
    }
    fn proof(&self) -> RootProof {
        self.lease.as_ref().unwrap().root_proof()
    }
    fn gate(&self) -> Gate {
        Gate::inspect_with_lifetime(self.lease.as_ref().unwrap())
    }
    fn restart(&mut self) {
        drop(self.lease.take());
        self.lease = Some(crate::daemon_lifetime::Lease::acquire(&self.root).unwrap());
    }
    fn raw_journal(&self) -> Vec<(String, Vec<u8>, u64)> {
        raw_files(&self.root.join("call-mode-restore.active"))
    }
    fn reacquire(&self) {
        let op = acquire_bound_existing_recovery_lock(
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
        op.release().unwrap();
    }
    async fn settle(
        &self,
        gate: &Gate,
        scenario: Scenario,
    ) -> Result<SettledWorker, PhysicalFailure> {
        let gate = gate.clone();
        let root = self.root.clone();
        let probe = ReconciliationArrivalProbe::new();
        let worker_probe = probe.clone();
        let result = tokio::task::spawn_blocking(move || {
            blocking_run_with_native(gate, root, Arc::new(AtomicBool::new(false)), |scope| {
                scripted_reconcile_observed(scope, scenario, None, &worker_probe)
            })
        })
        .await
        .unwrap();
        // This assertion runs after the real JoinHandle returns, outside the
        // entire owner/native callback catch. A caught early panic cannot make
        // an expected-error scenario look as if its late boundary was reached.
        probe.assert_reached();
        result
    }

    async fn settle_owned(
        &self,
        gate: &Gate,
        scenario: ManagedVoiceScenario,
    ) -> Result<SettledWorker, PhysicalFailure> {
        let native_probe = ReconciliationArrivalProbe::new();
        let voice_probe = ManagedVoiceArrivalProbe::new();
        let worker_native = native_probe.clone();
        let worker_voice = voice_probe.clone();
        let fixture = self.owned_voice.as_ref().unwrap().clone();
        let root = self.root.clone();
        let mutation_root = root.clone();
        let gate = gate.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let observed_cancel = cancel.clone();
        let hook_cancel = cancel.clone();
        let hook: Option<Box<dyn FnMut() + Send>> = match scenario {
            ManagedVoiceScenario::CancelBeforeStop => Some(Box::new(move || {
                hook_cancel.store(true, Ordering::Release);
            })),
            ManagedVoiceScenario::LedgerFailure => Some(Box::new(move || {
                use std::io::Write;
                let typed = storage::attempts(&File::open(&mutation_root).unwrap())
                    .unwrap()
                    .unwrap();
                assert_eq!(typed.len(), 1);
                let attempts = fs::read_dir(mutation_root.join(storage::NAME))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| {
                        path.is_dir()
                            && path.file_name().unwrap().to_str().is_some_and(|name| {
                                name.len() == 64
                                    && name
                                        .bytes()
                                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                            })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(attempts.len(), 1);
                let mut record = OpenOptions::new()
                    .append(true)
                    .open(attempts[0].join("000001.json"))
                    .unwrap();
                record.write_all(b" ").unwrap();
                record.sync_all().unwrap();
            })),
            _ => None,
        };
        let result = tokio::task::spawn_blocking(move || {
            blocking_run_with_native(gate, root, cancel, |scope| {
                scripted_reconcile_owned_observed(
                    scope,
                    fixture,
                    scenario,
                    &worker_voice,
                    hook,
                    &worker_native,
                )
            })
        })
        .await
        .unwrap();
        // Both observational assertions are outside the entire callback catch
        // and after actual checked closes, unlocks and blocking-worker join.
        native_probe.assert_reached();
        voice_probe.assert_reached(scenario);
        if scenario == ManagedVoiceScenario::CancelBeforeStop {
            assert!(observed_cancel.load(Ordering::Acquire));
        }
        result
    }

    fn archive(&self, completion: &CompletedReconciliation) -> PathBuf {
        self.root
            .join(storage::NAME)
            .join(&completion.publication_id)
            .join("recovered-original/journal")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.lease.take()); /* Root retains the complete fixture evidence. */
    }
}
fn identity() -> CallModeModuleIdentity {
    CallModeModuleIdentity {
        imei: "123456789012345".into(),
        firmware: "592-private-fixture".into(),
    }
}
fn usb() -> UsbComposition {
    UsbComposition {
        vendor_id: 0x2c7c,
        product_id: 0x2400,
        flags: [0; 7],
    }
}
fn voice() -> VoiceConfiguration {
    VoiceConfiguration {
        ims: 1,
        volte_capability: 1,
        volte_disabled: 0,
    }
}
fn intent() -> Intent {
    Intent{producer_id:"original-producer".into(),reservation_token:"original-reservation".into(),reservation_sequence:1,operation_id:"original-restore".into(),module_imei:identity().imei,current_usb:usb(),current_voice:voice(),target_usb:usb(),target_voice:Some(voice()),recovery_binding:Some(serde_json::from_value(serde_json::json!({"schema_version":1,"vendor_id":0x2c7c,"product_id":0x0125,"bus_number":1,"port_numbers":[2,3],"serial_language":1033,"serial_number":"592-exact-original","interface_number":2,"alternate_setting":0,"endpoint_in":0x81,"endpoint_out":0x02})).unwrap()),voice_recovery_binding:None}
}
fn raw_files(directory: &Path) -> Vec<(String, Vec<u8>, u64)> {
    let mut entries = fs::read_dir(directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
        .into_iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_str().unwrap().into(),
                fs::read(&p).unwrap(),
                fs::symlink_metadata(p).unwrap().ino(),
            )
        })
        .collect()
}

#[tokio::test]
async fn genuine_unknown_full_native_session_join_receipt_retirement_same_gate_and_restart() {
    let mut f = Fixture::pending().await;
    let original = f.raw_journal();
    let gate = f.gate();
    let completed = f
        .settle(&gate, Scenario::Success)
        .await
        .unwrap()
        .finalize()
        .unwrap();
    assert!(!f.root.join("call-mode-restore.active").exists());
    assert_eq!(raw_files(&f.archive(&completed)), original);
    let old = crate::call_mode_restore_journal::inspect_readonly_directory(
        &File::open(f.archive(&completed)).unwrap(),
    )
    .unwrap();
    assert_eq!(old.summary.unknown_steps, 1);
    assert!(!old.summary.receipt_present);
    completed.verify_for_gate(&f.proof()).unwrap();
    assert!(Gate::default().consume_reconciliation(&completed).is_err());
    gate.consume_reconciliation(&completed).unwrap();
    assert!(!gate.is_quarantined());
    f.reacquire();
    drop(completed);
    drop(gate);
    f.restart();
    assert!(!f.gate().is_quarantined());
}

#[tokio::test]
async fn genuine_physical_reconciliation_supersedes_revoked_lock_upgrade_provenance() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime/iteration-670-upgrade-then-recovery")
        .join(crate::snapshot::new_instance_id());
    let mut f = Fixture::pending_with_upgrade(root, false, true).await;
    let gate = f.gate();
    assert!(gate.is_quarantined());
    assert!(f.root.join("module-legacy-locks-v1/revoked").exists());
    let completion = f
        .settle(&gate, Scenario::Success)
        .await
        .unwrap()
        .finalize()
        .unwrap();
    gate.consume_reconciliation(&completion).unwrap();
    assert!(!gate.is_quarantined());
    assert!(f.root.join("module-legacy-locks-v1/revoked").exists());
    drop(completion);
    drop(gate);
    f.restart();
    assert!(!f.gate().is_quarantined());
    assert!(!crate::module_recovery::legacy_locks::migrate(&f.root, &f.proof()).unwrap());
}
#[tokio::test]
async fn recovery_allows_real_new_backup_normal_journal_finish_archive_and_later_startup() {
    let mut f = Fixture::pending().await;
    let original = f.raw_journal();
    let gate = f.gate();
    let completed = f
        .settle(&gate, Scenario::Success)
        .await
        .unwrap()
        .finalize()
        .unwrap();
    let archived = f.archive(&completed);
    gate.consume_reconciliation(&completed).unwrap();
    let id = crate::call_mode_backups::create_native_backup(
        &f.root,
        identity(),
        usb(),
        voice(),
        true,
        &f.proof(),
    )
    .await
    .unwrap();
    // A completed read-only backup does not require a subsequent write journal.
    assert!(!f.gate().is_quarantined());
    let backup = crate::call_mode_backups::capture_native_backup(&f.root, &id, &f.proof())
        .await
        .unwrap();
    let mut next = intent();
    next.operation_id = "new-normal-restore".into();
    let normal = Journal::begin(&f.root, &f.proof(), &backup, next)
        .await
        .unwrap();
    assert!(f.gate().is_quarantined());
    let receipt = Receipt {
        restored_at: "2026-10-19T00:00:00Z".into(),
        changed: false,
        module_imei: identity().imei,
        usb: usb(),
        voice: Some(voice()),
    };
    normal.target_verified(receipt.clone()).unwrap();
    normal.released().unwrap();
    normal.finish_after_release(receipt).unwrap();
    assert!(!f.gate().is_quarantined());
    assert_eq!(raw_files(&archived), original);
    drop(normal);
    drop(completed);
    drop(gate);
    f.restart();
    assert!(!f.gate().is_quarantined());
    assert_eq!(raw_files(&archived), original);
}
#[tokio::test]
async fn reboot_unknown_is_preserved_then_strict_successor_can_settle_new_attempt() {
    let f = Fixture::pending().await;
    let gate = f.gate();
    let completed = f
        .settle(&gate, Scenario::RebootAckUnknown)
        .await
        .unwrap()
        .finalize()
        .unwrap();
    let records = f
        .proof()
        .with_root(&f.root, storage::attempts)
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(records[0].records.iter().any(|r| matches!(
        r.event,
        ledger::Event::Outcome {
            outcome: SubmissionOutcome::SubmittedUnknown,
            ..
        }
    )));
    gate.consume_reconciliation(&completed).unwrap();
}
#[tokio::test]
async fn config_unknown_keeps_pending_then_next_lifetime_performs_fresh_compensation() {
    let mut f = Fixture::pending().await;
    let original = f.raw_journal();
    let gate = f.gate();
    assert!(f.settle(&gate, Scenario::ConfigWriteUnknown).await.is_err());
    f.reacquire();
    assert_eq!(f.raw_journal(), original);
    drop(gate);
    f.restart();
    let next = f.gate();
    let completed = f
        .settle(&next, Scenario::Success)
        .await
        .unwrap()
        .finalize()
        .unwrap();
    next.consume_reconciliation(&completed).unwrap();
    assert_eq!(raw_files(&f.archive(&completed)), original);
}
#[tokio::test]
async fn native_close_failure_retains_typed_debt_and_never_physical_receipt() {
    let f = Fixture::pending().await;
    let original = f.raw_journal();
    let gate = f.gate();
    let result = f.settle(&gate, Scenario::FinalCloseError).await;
    assert!(result.err().unwrap().cleanup_unconfirmed());
    f.reacquire();
    assert_eq!(f.raw_journal(), original);
    let attempts = f
        .proof()
        .with_root(&f.root, storage::attempts)
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!attempts[0].records.iter().any(|r| matches!(
        r.event,
        ledger::Event::PhysicalSettled | ledger::Event::CompletionReceipt { .. }
    )));
}
#[tokio::test]
async fn final_voice_unconfirmed_cannot_become_native_or_completion_receipt() {
    let f = Fixture::pending().await;
    let gate = f.gate();
    assert!(
        f.settle(&gate, Scenario::SuccessorVoiceUnconfirmed)
            .await
            .is_err()
    );
    f.reacquire();
    let attempts = f
        .proof()
        .with_root(&f.root, storage::attempts)
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!attempts[0].records.iter().any(|r| matches!(
        r.event,
        ledger::Event::NativeClosed | ledger::Event::CompletionReceipt { .. }
    )));
}
#[tokio::test]
async fn already_target_still_has_one_fresh_reboot_and_successor_voice_settlement() {
    let f = Fixture::pending().await;
    let gate = f.gate();
    let completed = f
        .settle(&gate, Scenario::AlreadyTarget)
        .await
        .unwrap()
        .finalize()
        .unwrap();
    let attempts = f
        .proof()
        .with_root(&f.root, storage::attempts)
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        attempts[0]
            .records
            .iter()
            .filter(|r| matches!(
                r.event,
                ledger::Event::Prepare {
                    mutation: ReconciliationMutation::Reboot,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(
        attempts[0]
            .records
            .iter()
            .any(|r| matches!(r.event, ledger::Event::SuccessorVoiceSettled { .. }))
    );
    gate.consume_reconciliation(&completed).unwrap();
}
#[tokio::test]
async fn cancel_after_durable_prepare_blocks_actual_write_and_preserves_original() {
    let f = Fixture::pending().await;
    let original = f.raw_journal();
    let gate = f.gate();
    let root = f.root.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    let observed_cancel = cancel.clone();
    let probe = ReconciliationArrivalProbe::new();
    let worker_probe = probe.clone();
    let g = gate.clone();
    let result = tokio::task::spawn_blocking(move || {
        blocking_run_with_native(g, root, cancel, |scope| {
            scripted_reconcile_observed(
                scope,
                Scenario::Success,
                Some(Box::new(move |command| {
                    if command.starts_with("AT+QCFG=\"USBCFG\",") {
                        c.store(true, Ordering::Release)
                    }
                })),
                &worker_probe,
            )
        })
    })
    .await
    .unwrap();
    probe.assert_reached();
    assert!(observed_cancel.load(Ordering::Acquire));
    assert!(result.is_err());
    assert_eq!(f.raw_journal(), original);
    f.reacquire();
}
#[tokio::test]
async fn physical_settled_rename_crash_resumes_only_metadata_after_new_lifetime() {
    let mut f = Fixture::pending().await;
    let original = f.raw_journal();
    let gate = f.gate();
    let settled = f.settle(&gate, Scenario::Success).await.unwrap();
    {
        let mut w = settled.identity.ledger.lock().unwrap();
        w.store.append(ledger::Event::HostSettled).unwrap();
        w.store.append(ledger::Event::PhysicalSettled).unwrap();
        w.store
            .append(ledger::Event::RetirementPrepare {
                slot: "call-mode-restore.active".into(),
                archive: "journal".into(),
            })
            .unwrap();
        w.store
            .retire_slot("call-mode-restore.active", "journal")
            .unwrap();
    }
    drop(settled);
    drop(gate);
    f.restart();
    let next = f.gate();
    assert!(next.is_quarantined());
    let completed =
        Coordinator::reconcile_with_cancel(&next, &f.root, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(raw_files(&f.archive(&completed)), original);
    next.consume_reconciliation(&completed).unwrap();
    assert!(!next.is_quarantined());
}
#[tokio::test]
async fn published_original_mutation_or_unregistered_backup_never_gets_coverage() {
    let f = Fixture::pending().await;
    let gate = f.gate();
    let completed = f
        .settle(&gate, Scenario::Success)
        .await
        .unwrap()
        .finalize()
        .unwrap();
    let archived = f.archive(&completed);
    gate.consume_reconciliation(&completed).unwrap();
    let extra = f.root.join("module-backups/unclaimed.json");
    fs::write(&extra, b"{}").unwrap();
    fs::set_permissions(&extra, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(f.gate().is_quarantined());
    fs::remove_file(extra).unwrap();
    let first = fs::read_dir(archived)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = fs::read(&first).unwrap();
    bytes.push(b' ');
    fs::write(first, bytes).unwrap();
    assert!(f.gate().is_quarantined());
}
#[tokio::test]
async fn final_publication_cannot_waive_new_invalid_pristine_or_legacy_execution_evidence() {
    for name in [
        ".call-mode-pristine-v1",
        crate::module_recovery::LEGACY_EXECUTION_PENDING,
    ] {
        let f = Fixture::pending().await;
        let gate = f.gate();
        let completed = f
            .settle(&gate, Scenario::Success)
            .await
            .unwrap()
            .finalize()
            .unwrap();
        let path = f.root.join(name);
        fs::write(&path, b"invalid-new-evidence").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(gate.consume_reconciliation(&completed).is_err());
        assert!(gate.is_quarantined());
    }
}
#[tokio::test]
async fn successor_wrong_identity_and_failed_candidate_close_never_publish() {
    for scenario in [
        Scenario::SuccessorWrongImei,
        Scenario::FailedCandidateCloseError,
    ] {
        let f = Fixture::pending().await;
        let original = f.raw_journal();
        let gate = f.gate();
        let err = f.settle(&gate, scenario).await.err().unwrap();
        if matches!(scenario, Scenario::FailedCandidateCloseError) {
            assert!(err.cleanup_unconfirmed());
        }
        assert_eq!(f.raw_journal(), original);
        f.reacquire();
    }
}
#[tokio::test]
async fn callback_panic_is_redacted_cleanup_debt_with_both_locks_actually_released() {
    let f = Fixture::pending().await;
    let original = f.raw_journal();
    let gate = f.gate();
    let error = f
        .settle(&gate, Scenario::CallbackPanic)
        .await
        .err()
        .unwrap();
    assert!(error.cleanup_unconfirmed());
    assert!(!error.to_string().contains("fixture"));
    assert_eq!(f.raw_journal(), original);
    f.reacquire();
}

#[tokio::test]
async fn failed_original_candidate_must_close_before_same_device_fallback_full_chain_and_restart() {
    let mut f = Fixture::pending().await;
    let original = f.raw_journal();
    let gate = f.gate();
    let completed = f
        .settle(&gate, Scenario::SuccessorProbeFallback)
        .await
        .unwrap()
        .finalize()
        .unwrap();
    let archive = f.archive(&completed);
    let old = crate::call_mode_restore_journal::inspect_readonly_directory(
        &File::open(&archive).unwrap(),
    )
    .unwrap();
    assert_eq!(old.summary.unknown_steps, 1);
    assert!(!old.summary.receipt_present);
    assert_eq!(raw_files(&archive), original);
    completed.verify_for_gate(&f.proof()).unwrap();
    f.reacquire();
    gate.consume_reconciliation(&completed).unwrap();
    assert!(!gate.is_quarantined());
    drop(completed);
    drop(gate);
    f.restart();
    assert!(!f.gate().is_quarantined());
    let retained = crate::call_mode_restore_journal::inspect_readonly_directory(
        &File::open(&archive).unwrap(),
    )
    .unwrap();
    assert_eq!(retained.summary.unknown_steps, 1);
    assert!(!retained.summary.receipt_present);
    assert_eq!(raw_files(&archive), original);
}

// Keep the genuine pending journal and its actual lease for the finite I/O worker.
pub(super) async fn prepare_pending_for_finite_io(
    root: &Path,
) -> (crate::daemon_lifetime::Lease, Gate) {
    let mut fixture = Fixture::pending_at(root.to_path_buf(), false).await;
    let gate = fixture.gate();
    assert!(gate.is_quarantined());
    let lease = fixture.lease.take().unwrap();
    (lease, gate)
}

// A sibling crash worker may stop the actual finalizer at publication boundaries.
// Its returned authority and settled token are produced by the same real owner,
// original managed-control importer and joined native/ADB executor as production.
pub(super) async fn prepare_publication_crash(
    root: &Path,
) -> (crate::daemon_lifetime::Lease, Gate, SettledWorker) {
    let mut fixture = Fixture::pending_at(root.to_path_buf(), true).await;
    let gate = fixture.gate();
    let settled = fixture
        .settle_owned(&gate, ManagedVoiceScenario::Running)
        .await
        .unwrap();
    let lease = fixture.lease.take().unwrap();
    (lease, gate, settled)
}

async fn owned_success(scenario: ManagedVoiceScenario) {
    let mut f = Fixture::owned_pending().await;
    let original_journal = f.raw_journal();
    let original_control = raw_files(&f.root.join("voice-control"));
    let journal_inode = fs::metadata(f.root.join("call-mode-restore.active"))
        .unwrap()
        .ino();
    let control_inode = fs::metadata(f.root.join("voice-control")).unwrap().ino();
    let gate = f.gate();
    assert!(gate.is_quarantined());
    let completed = f
        .settle_owned(&gate, scenario)
        .await
        .unwrap()
        .finalize()
        .unwrap();
    let archived_journal = f.archive(&completed);
    let archived_control = archived_journal.parent().unwrap().join("voice-control");
    assert_eq!(raw_files(&archived_journal), original_journal);
    assert!(
        raw_files(&archived_control) == original_control,
        "original private control inode/raw evidence changed"
    );
    assert_eq!(
        fs::metadata(&archived_journal).unwrap().ino(),
        journal_inode
    );
    assert_eq!(
        fs::metadata(&archived_control).unwrap().ino(),
        control_inode
    );
    assert!(!f.root.join("call-mode-restore.active").exists());
    assert!(!f.root.join("voice-control").exists());
    let original = crate::call_mode_restore_journal::inspect_readonly_directory(
        &File::open(&archived_journal).unwrap(),
    )
    .unwrap();
    assert_eq!(original.summary.unknown_steps, 1);
    assert!(!original.summary.receipt_present);
    completed.verify_for_gate(&f.proof()).unwrap();
    let attempts = f
        .proof()
        .with_root(&f.root, storage::attempts)
        .unwrap()
        .unwrap()
        .unwrap();
    let voice = attempts[0]
        .records
        .iter()
        .find_map(|record| match &record.event {
            ledger::Event::SuccessorVoiceSettled { evidence } => Some(evidence),
            _ => None,
        })
        .unwrap();
    // Restart uses the production archived-key HMAC/transcript verifier, not a
    // retained live proof or a reconstructed success boolean.
    crate::call_mode_adb::startup_recovery::verify_settlement_evidence(
        voice,
        Some(&File::open(&archived_control).unwrap()),
        Some(&f.owned_voice.as_ref().unwrap().binding()),
    )
    .unwrap();
    let mut forged = voice.clone();
    let signed = forged["authenticated_responses"][0]["signed_response"]
        .as_str()
        .unwrap();
    let mut forged_wire = signed.as_bytes().to_vec();
    assert_eq!(forged_wire.pop(), Some(b'\n'));
    let last = forged_wire.pop().unwrap();
    forged_wire.push(if last == b'0' { b'1' } else { b'0' });
    forged_wire.push(b'\n');
    let forged_wire = String::from_utf8(forged_wire).unwrap();
    forged["authenticated_responses"][0]["sha256"] = storage::digest(forged_wire.as_bytes()).into();
    forged["authenticated_responses"][0]["signed_response"] = forged_wire.into();
    assert!(
        crate::call_mode_adb::startup_recovery::verify_settlement_evidence(
            &forged,
            Some(&File::open(&archived_control).unwrap()),
            Some(&f.owned_voice.as_ref().unwrap().binding()),
        )
        .is_err(),
        "recomputed transcript digest must not bypass original-key HMAC"
    );
    gate.consume_reconciliation(&completed).unwrap();
    assert!(!gate.is_quarantined());
    f.reacquire();
    drop(completed);
    drop(gate);
    f.restart();
    assert!(!f.gate().is_quarantined());
    crate::call_mode_adb::startup_recovery::verify_settlement_evidence(
        voice,
        Some(&File::open(&archived_control).unwrap()),
        Some(&f.owned_voice.as_ref().unwrap().binding()),
    )
    .unwrap();
    assert_eq!(raw_files(&archived_journal), original_journal);
    assert!(
        raw_files(&archived_control) == original_control,
        "original private control inode/raw evidence changed"
    );
}
#[tokio::test]
async fn owned_running_stop_signed_exit_dsp_release_close_dual_retirement_same_gate_restart() {
    owned_success(ManagedVoiceScenario::Running).await;
}
#[tokio::test]
async fn owned_initially_exited_still_cleans_releases_and_verifies_hmac_after_restart() {
    owned_success(ManagedVoiceScenario::InitiallyExited).await;
}

async fn owned_failure(scenario: ManagedVoiceScenario) {
    let mut f = Fixture::owned_pending().await;
    let original_journal = f.raw_journal();
    let original_control = raw_files(&f.root.join("voice-control"));
    let gate = f.gate();
    let result = f.settle_owned(&gate, scenario).await;
    assert!(result.is_err());
    if scenario == ManagedVoiceScenario::CloseError {
        assert!(result.as_ref().err().unwrap().cleanup_unconfirmed());
    }
    assert!(gate.is_quarantined());
    assert_eq!(f.raw_journal(), original_journal);
    assert!(
        raw_files(&f.root.join("voice-control")) == original_control,
        "original private control inode/raw evidence changed"
    );
    f.reacquire();
    if scenario != ManagedVoiceScenario::LedgerFailure {
        let attempts = f
            .proof()
            .with_root(&f.root, storage::attempts)
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!attempts[0].records.iter().any(|record| matches!(
            record.event,
            ledger::Event::VoiceSettled { .. }
                | ledger::Event::NativeClosed
                | ledger::Event::PhysicalSettled
                | ledger::Event::CompletionReceipt { .. }
        )));
    } else {
        assert!(
            f.proof()
                .with_root(&f.root, storage::attempts)
                .unwrap()
                .is_err()
        );
    }
    drop(result);
    drop(gate);
    f.restart();
    assert!(f.gate().is_quarantined());
    assert_eq!(f.raw_journal(), original_journal);
    assert!(
        raw_files(&f.root.join("voice-control")) == original_control,
        "original private control inode/raw evidence changed"
    );
}
#[tokio::test]
async fn owned_bad_mac_nonce_pid_or_boot_never_prepares_configuration_or_activates() {
    for scenario in [
        ManagedVoiceScenario::BadMac,
        ManagedVoiceScenario::BadNonce,
        ManagedVoiceScenario::WrongPid,
        ManagedVoiceScenario::WrongBoot,
    ] {
        owned_failure(scenario).await;
    }
}
#[tokio::test]
async fn owned_stop_or_release_unknown_ack_preserves_original_pending_without_receipt() {
    for scenario in [
        ManagedVoiceScenario::StopAckLost,
        ManagedVoiceScenario::ReleaseAckLost,
    ] {
        owned_failure(scenario).await;
    }
}
#[tokio::test]
async fn owned_active_dsp_or_pcm_never_releases_or_publishes() {
    for scenario in [
        ManagedVoiceScenario::DspActive,
        ManagedVoiceScenario::PlaybackOpen,
        ManagedVoiceScenario::CaptureOpen,
    ] {
        owned_failure(scenario).await;
    }
}
#[tokio::test]
async fn owned_actual_adb_close_failure_blocks_completion_and_activation() {
    owned_failure(ManagedVoiceScenario::CloseError).await;
}
#[tokio::test]
async fn owned_cancellation_at_prepared_stop_finishes_retained_cleanup_without_receipt() {
    owned_failure(ManagedVoiceScenario::CancelBeforeStop).await;
}
#[tokio::test]
async fn owned_ledger_corruption_at_prepared_stop_finishes_retained_cleanup_without_receipt() {
    owned_failure(ManagedVoiceScenario::LedgerFailure).await;
}

// Genuine production publication fixture shared by actual lifecycle handlers
// and independently owned process-crash tests. No receipt/proof constructors.
pub(crate) async fn prepare_completed_backup_lifecycle(
    root: &Path,
    owned: bool,
) -> (crate::daemon_lifetime::Lease, Gate) {
    let mut fixture = Fixture::pending_at(root.to_path_buf(), owned).await;
    let gate = fixture.gate();
    let settled = if owned {
        fixture
            .settle_owned(&gate, ManagedVoiceScenario::Running)
            .await
            .unwrap()
    } else {
        fixture.settle(&gate, Scenario::Success).await.unwrap()
    };
    let completed = settled.finalize().unwrap();
    completed.verify_for_gate(&fixture.proof()).unwrap();
    gate.consume_reconciliation(&completed).unwrap();
    assert!(!gate.is_quarantined());
    let lease = fixture.lease.take().unwrap();
    (lease, gate)
}
