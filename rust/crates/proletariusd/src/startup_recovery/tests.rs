use super::*;
use crate::call_mode_backups::acquire_bound_recovery_lock;
use crate::daemon_lifetime::Lease;
use crate::module_recovery::LEGACY_EXECUTION_PENDING;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};

struct Fixture {
    root: PathBuf,
    lease: Option<Lease>,
}
impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/iteration-586-startup-recovery")
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
    fn pending(&self) {
        self.write(LEGACY_EXECUTION_PENDING, br#"{"historical":"unknown"}"#);
    }
    fn ready_pending(&self) {
        self.pending();
        self.write("call-mode-operation.lock", b"");
        self.write(".module-adb-interface.lock", b"");
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.root.join(name))
            .unwrap();
        std::io::Write::write_all(&mut file, bytes).unwrap();
    }
    fn owner(&self, gate: &Gate) -> RecoveryOwner {
        Coordinator::begin_inspection(gate, &self.root, Instant::now() + INSPECTION_BUDGET).unwrap()
    }
}

#[test]
fn clean_startup_does_not_create_or_acquire_recovery_locks() {
    let f = Fixture::new();
    let gate = f.gate();
    let inspection = Coordinator::inspect(&gate, &f.root).unwrap();
    assert_eq!(inspection.startup_logging().state, "not_needed");
    assert!(!f.root.join("call-mode-operation.lock").exists());
    assert!(!f.root.join(".module-adb-interface.lock").exists());
    assert!(gate.admit_operation("ordinary-module").is_ok());
    assert!(gate.capture_recovery_authority(&f.root).is_err());
    assert!(inspection.captured.is_none() && inspection.witnesses.is_none());
}

#[test]
fn missing_original_locks_leave_every_existing_byte_and_name_unchanged() {
    for present in [
        None,
        Some("call-mode-operation.lock"),
        Some(".module-adb-interface.lock"),
    ] {
        let f = Fixture::new();
        f.pending();
        if let Some(name) = present {
            f.write(name, b"");
        }
        let snapshot = || {
            let mut files: Vec<_> = fs::read_dir(&f.root)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    let metadata = fs::symlink_metadata(entry.path()).unwrap();
                    (
                        entry.file_name(),
                        metadata.dev(),
                        metadata.ino(),
                        metadata.mode(),
                        metadata.len(),
                        metadata.mtime(),
                        metadata.mtime_nsec(),
                        metadata.ctime(),
                        metadata.ctime_nsec(),
                        fs::read(entry.path()).unwrap(),
                    )
                })
                .collect();
            files.sort();
            files
        };
        let gate = f.gate();
        let before = snapshot();
        let root_before = fs::metadata(&f.root).unwrap();
        assert!(Coordinator::inspect(&gate, &f.root).is_err());
        assert_eq!(snapshot(), before);
        let root_after = fs::metadata(&f.root).unwrap();
        assert_eq!(
            (
                root_before.dev(),
                root_before.ino(),
                root_before.mode(),
                root_before.nlink(),
                root_before.mtime(),
                root_before.mtime_nsec(),
                root_before.ctime(),
                root_before.ctime_nsec()
            ),
            (
                root_after.dev(),
                root_after.ino(),
                root_after.mode(),
                root_after.nlink(),
                root_after.mtime(),
                root_after.mtime_nsec(),
                root_after.ctime(),
                root_after.ctime_nsec()
            )
        );
        assert_eq!(
            f.root.join("call-mode-operation.lock").exists(),
            present == Some("call-mode-operation.lock")
        );
        assert_eq!(
            f.root.join(".module-adb-interface.lock").exists(),
            present == Some(".module-adb-interface.lock")
        );
        assert!(
            gate.admit_operation("ordinary-after-unavailable-inspection")
                .is_err()
        );
    }
}

#[test]
fn quarantined_capture_retains_bytes_but_explicitly_releases_both_kernel_locks() {
    let f = Fixture::new();
    f.ready_pending();
    let gate = f.gate();
    let bytes = fs::read(f.root.join(LEGACY_EXECUTION_PENDING)).unwrap();
    let inspection = Coordinator::inspect(&gate, &f.root).unwrap();
    let report = inspection.startup_logging();
    assert_eq!(report.state, "metadata_inspected");
    assert!(report.ordinary_quarantine_preserved && !report.recovery_completed);
    assert_eq!(report.scope, "metadata_only");
    assert!(report.evidence_nodes >= 3 && report.evidence_bytes == bytes.len());
    assert_eq!(report.evidence_sha256.as_ref().unwrap().len(), 64);
    assert!(gate.admit_operation("ordinary-after-inspection").is_err());
    inspection.verify(&gate).unwrap();
    // Independent real locks succeed while the captured read-only witnesses
    // remain alive. An incidental cloned locked fd would make these fail.
    let operation =
        acquire_bound_recovery_lock(&f.root, &f.proof(), RecoveryLockKind::Operation, false)
            .unwrap();
    let adb =
        acquire_bound_recovery_lock(&f.root, &f.proof(), RecoveryLockKind::AdbInterface, false)
            .unwrap();
    inspection.verify(&gate).unwrap();
    adb.release().unwrap();
    operation.release().unwrap();
    assert_eq!(
        fs::read(f.root.join(LEGACY_EXECUTION_PENDING)).unwrap(),
        bytes
    );
}

#[test]
fn metadata_right_is_same_gate_finite_and_cannot_outlive_real_owner() {
    let f = Fixture::new();
    f.ready_pending();
    let gate = f.gate();
    let foreign_gate = f.gate();
    let mut owner = f.owner(&gate);
    let right = owner.right();
    right
        .validate_for(&gate, RecoveryScope::MetadataInspection)
        .unwrap();
    assert!(
        right
            .validate_for(&foreign_gate, RecoveryScope::MetadataInspection)
            .is_err()
    );
    owner.capture(&f.root, &right).unwrap();
    let diagnostic = owner.inspect_metadata(&right).unwrap();
    let inspection = owner.release(diagnostic).unwrap();
    assert!(
        right
            .validate_for(&gate, RecoveryScope::MetadataInspection)
            .is_err()
    );
    inspection.verify(&gate).unwrap();

    let mut owner = f.owner(&gate);
    Arc::get_mut(&mut owner.identity).unwrap().deadline = Instant::now();
    let expired = owner.right();
    assert!(owner.capture(&f.root, &expired).is_err());
    assert!(
        expired
            .validate_for(&gate, RecoveryScope::MetadataInspection)
            .is_err()
    );
    drop(owner);
    assert!(expired.0.upgrade().is_none());
    assert!(gate.admit_operation("not-converted").is_err());
}

#[test]
fn foreign_root_and_path_only_gate_never_create_locks_in_an_unowned_root() {
    let f = Fixture::new();
    f.pending();
    let gate = f.gate();
    let foreign = Fixture::new();
    assert!(Coordinator::inspect(&gate, &foreign.root).is_err());
    let path_gate = Gate::inspect(&f.root);
    assert!(Coordinator::inspect(&path_gate, &f.root).is_err());
    for root in [&f.root, &foreign.root] {
        assert!(!root.join("call-mode-operation.lock").exists());
        assert!(!root.join(".module-adb-interface.lock").exists());
    }
}

#[test]
fn operation_then_adb_busy_failure_releases_its_actual_original_operation_lock() {
    let f = Fixture::new();
    f.ready_pending();
    let gate = f.gate();
    let adb =
        acquire_bound_recovery_lock(&f.root, &f.proof(), RecoveryLockKind::AdbInterface, false)
            .unwrap();
    assert!(Coordinator::inspect(&gate, &f.root).is_err());
    let operation =
        acquire_bound_recovery_lock(&f.root, &f.proof(), RecoveryLockKind::Operation, false)
            .unwrap();
    operation.release().unwrap();
    adb.release().unwrap();
    Coordinator::inspect(&gate, &f.root)
        .unwrap()
        .verify(&gate)
        .unwrap();
}

#[test]
fn all_fixed_slots_capture_runtime_backup_and_absence_without_ignoring_new_evidence() {
    let f = Fixture::new();
    f.ready_pending();
    fs::create_dir(f.root.join("module-backups")).unwrap();
    fs::set_permissions(
        f.root.join("module-backups"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    f.write(
        "module-backups/original.json",
        br#"{"kept":"raw original bytes"}"#,
    );
    fs::create_dir(f.root.join("voice-runtime")).unwrap();
    fs::create_dir(f.root.join("voice-runtime/version")).unwrap();
    f.write(
        "voice-runtime/version/resource",
        b"untrusted legacy runtime, never executed",
    );
    let gate = f.gate();
    let inspection = Coordinator::inspect(&gate, &f.root).unwrap();
    assert!(inspection.startup_logging().evidence_nodes >= 8);
    inspection.verify(&gate).unwrap();
    // An unrelated reader store does not mutate this fixed evidence set.
    f.write("reader-cache.json", b"reader remains usable");
    inspection.verify(&gate).unwrap();
    f.write(".call-mode-pristine-v1", b"");
    assert!(inspection.verify(&gate).is_err());
    assert!(gate.is_quarantined());
}

#[test]
fn control_records_are_pinned_and_new_control_names_invalidate_existing_inspection() {
    for prior in [false, true] {
        let f = Fixture::new();
        f.ready_pending();
        if prior {
            fs::create_dir(f.root.join("voice-control")).unwrap();
            fs::set_permissions(
                f.root.join("voice-control"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            f.write(
                "voice-control/session.json",
                b"original private control bytes",
            );
        }
        let gate = f.gate();
        let inspection = Coordinator::inspect(&gate, &f.root).unwrap();
        inspection.verify(&gate).unwrap();
        if prior {
            fs::write(
                f.root.join("voice-control/session.json"),
                b"changed private control bytes",
            )
            .unwrap();
        } else {
            fs::create_dir(f.root.join("voice-control")).unwrap();
        }
        assert!(inspection.verify(&gate).is_err());
        assert!(gate.is_quarantined());
    }
}

#[test]
fn control_secret_is_not_logged_and_unsafe_control_names_are_not_followed() {
    for shape in ["symlink", "hardlink", "fifo", "public", "oversize"] {
        let f = Fixture::new();
        f.ready_pending();
        let path = f.root.join("voice-control");
        match shape {
            "symlink" => symlink("absent", &path).unwrap(),
            "hardlink" => fs::hard_link(f.root.join(LEGACY_EXECUTION_PENDING), &path).unwrap(),
            "fifo" => {
                nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap()
            }
            "public" => {
                fs::create_dir(&path).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();
            }
            "oversize" => {
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .unwrap();
                file.set_len(1_100_001).unwrap();
            }
            _ => unreachable!(),
        }
        let gate = f.gate();
        assert!(Coordinator::inspect(&gate, &f.root).is_err(), "{shape}");
        assert!(fs::symlink_metadata(path).is_ok());
    }
    let f = Fixture::new();
    f.ready_pending();
    fs::create_dir(f.root.join("voice-control")).unwrap();
    fs::set_permissions(
        f.root.join("voice-control"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    f.write(
        "voice-control/session.json",
        b"sensitive-control-key-is-never-a-diagnostic",
    );
    let gate = f.gate();
    let inspection = Coordinator::inspect(&gate, &f.root).unwrap();
    let diagnostic = serde_json::to_string(&inspection.startup_logging()).unwrap();
    assert!(!diagnostic.contains("sensitive-control-key"));
}

#[test]
fn pinned_bytes_replacement_permissions_root_and_ended_lifetime_all_invalidate_capture() {
    for change in ["bytes", "inode", "permissions", "root", "lease"] {
        let mut f = Fixture::new();
        f.ready_pending();
        let gate = f.gate();
        let inspection = Coordinator::inspect(&gate, &f.root).unwrap();
        let pending = f.root.join(LEGACY_EXECUTION_PENDING);
        match change {
            "bytes" => fs::write(&pending, b"different raw evidence").unwrap(),
            "inode" => {
                let bytes = fs::read(&pending).unwrap();
                fs::rename(&pending, f.root.join("original-pending")).unwrap();
                f.write(LEGACY_EXECUTION_PENDING, &bytes);
            }
            "permissions" => {
                fs::set_permissions(&pending, fs::Permissions::from_mode(0o666)).unwrap()
            }
            "root" => {
                fs::rename(&f.root, f.root.with_extension("original-held-root")).unwrap();
                fs::create_dir(&f.root).unwrap();
                fs::set_permissions(&f.root, fs::Permissions::from_mode(0o700)).unwrap();
            }
            "lease" => drop(f.lease.take()),
            _ => unreachable!(),
        }
        assert!(inspection.verify(&gate).is_err(), "{change}");
        assert!(gate.is_quarantined());
    }
}

#[test]
fn unsafe_symlink_hardlink_fifo_public_root_and_oversize_evidence_fail_without_repair() {
    for shape in ["symlink", "hardlink", "fifo", "public-root", "oversize"] {
        let f = Fixture::new();
        f.ready_pending();
        let gate = f.gate();
        let target = f.root.join("module-backups");
        match shape {
            "symlink" => symlink(f.root.join(LEGACY_EXECUTION_PENDING), &target).unwrap(),
            "hardlink" => fs::hard_link(f.root.join(LEGACY_EXECUTION_PENDING), &target).unwrap(),
            "fifo" => nix::unistd::mkfifo(&target, nix::sys::stat::Mode::from_bits_truncate(0o600))
                .unwrap(),
            "public-root" => {
                fs::set_permissions(&f.root, fs::Permissions::from_mode(0o755)).unwrap()
            }
            "oversize" => {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&target)
                    .unwrap()
                    .set_len(1_100_001)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let metadata = fs::symlink_metadata(&target).ok();
        assert!(Coordinator::inspect(&gate, &f.root).is_err(), "{shape}");
        if let Some(before) = metadata {
            let after = fs::symlink_metadata(&target).unwrap();
            assert_eq!(
                (before.dev(), before.ino(), before.mode(), before.len()),
                (after.dev(), after.ino(), after.mode(), after.len()),
                "{shape}"
            );
        }
        assert!(gate.is_quarantined());
    }
}

#[test]
fn bounded_capture_rejects_excess_names_without_truncating_to_a_successful_snapshot() {
    let f = Fixture::new();
    f.ready_pending();
    fs::create_dir(f.root.join("module-backups")).unwrap();
    for index in 0..257 {
        f.write(&format!("module-backups/{index}.json"), b"{}");
    }
    let gate = f.gate();
    assert!(Coordinator::inspect(&gate, &f.root).is_err());
    assert_eq!(
        fs::read_dir(f.root.join("module-backups")).unwrap().count(),
        257
    );
    assert!(Coordinator::begin_inspection(&gate, &f.root, Instant::now()).is_err());
}

#[test]
fn actual_root_aba_during_capture_is_rejected_without_freezing_unrelated_future_reader_work() {
    let f = Fixture::new();
    f.ready_pending();
    let gate = f.gate();
    let owner = f.owner(&gate);
    let right = owner.right();
    let calls = std::cell::Cell::new(0usize);
    let result = f
        .proof()
        .with_root(&f.root, |held| {
            evidence::EvidenceTree::capture(held, &|| {
                owner.check(&right)?;
                let count = calls.get() + 1;
                calls.set(count);
                if count == 4 {
                    // Real filesystem change after the original root stamp and
                    // before the first file read, leaving the final name absent.
                    f.write(".call-mode-pristine-v1", b"");
                    fs::remove_file(f.root.join(".call-mode-pristine-v1"))?;
                }
                Ok(())
            })
        })
        .unwrap();
    assert!(calls.get() >= 4 && result.is_err());
    assert!(!f.root.join(".call-mode-pristine-v1").exists());
    drop(owner);
    let inspection = Coordinator::inspect(&gate, &f.root).unwrap();
    f.write("reader-cache-after-capture", b"independent reader work");
    inspection.verify(&gate).unwrap();
}

#[tokio::test]
async fn actual_journal_capture_preserves_typed_unknown_and_never_grants_replay_or_unseals_gate() {
    use crate::call_mode_restore_journal::{Intent, Journal, Mutation, MutationKind};
    use crate::module_radio_transport::{
        CallModeModuleIdentity, UsbComposition, VoiceConfiguration,
    };
    let f = Fixture::new();
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
        &f.root,
        CallModeModuleIdentity {
            imei: "123456789012345".into(),
            firmware: "metadata-fixture".into(),
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
            recovery_binding: None,
            voice_recovery_binding: None,
        },
    )
    .await
    .unwrap();
    journal
        .safety_checkpoint(backup.id(), &backup.sha256())
        .unwrap();
    let token = journal
        .prepare_step(Mutation {
            operation_id: "voice-change".into(),
            kind: MutationKind::Ims { target: 1 },
        })
        .unwrap();
    f.write("call-mode-operation.lock", b"");
    f.write(".module-adb-interface.lock", b"");
    let gate = f.gate();
    let inspection = Coordinator::inspect(&gate, &f.root).unwrap();
    let captured = inspection
        .captured
        .as_ref()
        .unwrap()
        .journal
        .as_ref()
        .unwrap();
    assert_eq!(captured.intent().producer_id, "original-producer");
    assert!(
        captured.intent().recovery_binding.is_none(),
        "an old Intent is not a device binding"
    );
    assert_eq!(captured.summary().unknown_steps, 1);
    assert_eq!(captured.summary().prepared_steps, 1);
    assert_eq!(captured.evidence_sha256().len(), 64);
    assert!(!inspection.startup_logging().recovery_completed);
    assert!(gate.admit_operation("ordinary-after-capture").is_err());
    inspection.verify(&gate).unwrap();
    // Real journal mutation invalidates the retained snapshot; inspection
    // neither records a terminal event nor discards the original Unknown.
    token.record_submitted_unknown().unwrap();
    assert!(inspection.verify(&gate).is_err());
    assert!(gate.is_quarantined());
}
