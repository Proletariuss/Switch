use super::*;
use crate::module_radio_transport::CallModeModuleIdentity;
use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
    lease: Option<crate::daemon_lifetime::Lease>,
    captured: crate::call_mode_backups::CapturedNativeBackup,
}
impl Fixture {
    async fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/iteration-585-journal")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let lease = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        let proof = lease.root_proof();
        let id = crate::call_mode_backups::create_native_backup(
            &root,
            CallModeModuleIdentity {
                imei: "123456789012345".into(),
                firmware: "fixture".into(),
            },
            target_usb(),
            voice(),
            false,
            &proof,
        )
        .await
        .unwrap();
        let captured = crate::call_mode_backups::capture_native_backup(&root, &id, &proof)
            .await
            .unwrap();
        Self {
            root,
            lease: Some(lease),
            captured,
        }
    }
    fn proof(&self) -> RootProof {
        self.lease.as_ref().unwrap().root_proof()
    }
    async fn begin(&self) -> Journal {
        Journal::begin(&self.root, &self.proof(), &self.captured, intent())
            .await
            .unwrap()
    }
    fn summary(&self) -> DiagnosticSummary {
        self.proof()
            .with_root(&self.root, |root| diagnostic_summary(root))
            .unwrap()
            .unwrap()
            .unwrap()
    }
    fn active_bytes(&self) -> Vec<(String, Vec<u8>)> {
        let active = self.root.join(ACTIVE_NAME);
        let mut names: Vec<_> = fs::read_dir(active)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        names
            .into_iter()
            .map(|p| {
                (
                    p.file_name().unwrap().to_str().unwrap().into(),
                    fs::read(p).unwrap(),
                )
            })
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.lease.take()); /* Retain runtime evidence for the validation owner. */
    }
}
fn target_usb() -> UsbComposition {
    UsbComposition {
        vendor_id: 0x2c7c,
        product_id: 0x2400,
        flags: [0, 0, 0, 0, 0, 1, 1],
    }
}
fn current_usb() -> UsbComposition {
    UsbComposition {
        flags: [0; 7],
        ..target_usb()
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
    Intent {
        producer_id: "restore-producer".into(),
        reservation_token: "restore-reservation".into(),
        reservation_sequence: 7,
        operation_id: "restore-op".into(),
        module_imei: "123456789012345".into(),
        current_usb: current_usb(),
        current_voice: VoiceConfiguration {
            ims: 0,
            volte_capability: 1,
            volte_disabled: 1,
        },
        target_usb: target_usb(),
        target_voice: Some(voice()),
        recovery_binding: None,
        voice_recovery_binding: None,
    }
}
fn receipt() -> Receipt {
    Receipt {
        restored_at: String::new(),
        changed: true,
        module_imei: intent().module_imei,
        usb: target_usb(),
        voice: Some(voice()),
    }
}
fn mutation(id: &str) -> Mutation {
    Mutation {
        operation_id: id.into(),
        kind: MutationKind::Ims { target: 1 },
    }
}
fn safety(journal: &Journal, fixture: &Fixture) {
    journal
        .safety_checkpoint(fixture.captured.id(), &fixture.captured.sha256())
        .unwrap();
}
fn fault(journal: &Journal, point: storage::Point) {
    journal
        .0
        .state
        .lock()
        .unwrap()
        .store
        .set_fault(move |actual| {
            if actual == point {
                Err(std::io::Error::other("injected real-sync failure"))
            } else {
                Ok(())
            }
        });
}

#[tokio::test]
async fn intent_is_private_bound_to_captured_input_and_existing_active_is_never_replaced() {
    let fixture = Fixture::new().await;
    let journal = fixture.begin().await;
    let summary = fixture.summary();
    assert_eq!(summary.sequence, 1);
    assert_eq!(summary.prepared_steps, 0);
    assert!(!summary.target_verified && !summary.released && !summary.receipt_present);
    let active = fixture.root.join(ACTIVE_NAME);
    assert_eq!(fs::metadata(&active).unwrap().mode() & 0o7777, 0o700);
    let path = active.join("0001.json");
    let meta = fs::metadata(&path).unwrap();
    assert_eq!(meta.mode() & 0o7777, 0o600);
    assert_eq!(meta.nlink(), 1);
    let before = fixture.active_bytes();
    assert!(
        Journal::begin(&fixture.root, &fixture.proof(), &fixture.captured, intent())
            .await
            .is_err()
    );
    assert_eq!(fixture.active_bytes(), before);
    assert!(journal.matches_reservation("restore-producer", "restore-reservation", 7));
    assert!(!journal.matches_reservation("restore-producer", "restore-reservation", 8));
}

#[tokio::test]
async fn foreign_and_ended_root_authorities_create_no_active_entry() {
    let fixture = Fixture::new().await;
    let foreign = Fixture::new().await;
    assert!(
        Journal::begin(&foreign.root, &foreign.proof(), &fixture.captured, intent())
            .await
            .is_err()
    );
    assert!(!foreign.root.join(ACTIVE_NAME).exists());
    let mut ended = Fixture::new().await;
    let proof = ended.proof();
    drop(ended.lease.take());
    assert!(
        Journal::begin(&ended.root, &proof, &ended.captured, intent())
            .await
            .is_err()
    );
    assert!(!ended.root.join(ACTIVE_NAME).exists());
}

#[tokio::test]
async fn repeated_guard_validation_reuses_one_prepare_and_terminal_token_cannot_replay() {
    let fixture = Fixture::new().await;
    let journal = fixture.begin().await;
    safety(&journal, &fixture);
    let prepared = journal.prepare_step(mutation("ims-one")).unwrap();
    let repeated = journal.prepare_step(mutation("ims-one")).unwrap();
    for _ in 0..3 {
        prepared.validate_for_write(&mutation("ims-one")).unwrap();
        repeated.validate_for_write(&mutation("ims-one")).unwrap();
    }
    assert_eq!(fixture.summary().sequence, 3);
    assert_eq!(fixture.summary().unknown_steps, 1);
    prepared.record_acknowledged().unwrap();
    assert!(prepared.validate_for_write(&mutation("ims-one")).is_err());
    assert_eq!(fixture.summary().sequence, 4);
    // Invalid reuse poisons this Journal; separate test below verifies the
    // successful ACK -> typed readback transition without this misuse.
    assert!(journal.prepare_step(mutation("ims-two")).is_err());
}

#[tokio::test]
async fn acknowledged_write_needs_readback_before_next_mutation_and_unknown_never_washes() {
    let fixture = Fixture::new().await;
    let journal = fixture.begin().await;
    safety(&journal, &fixture);
    let token = journal.prepare_step(mutation("ims-one")).unwrap();
    token.record_acknowledged().unwrap();
    // Some modules still expose the audited original value until reboot.
    // Preserve that actual value instead of falsely recording target success.
    token
        .record_verified_readback(Readback::Ims {
            ims: 0,
            volte_capability: 1,
        })
        .unwrap();
    assert_eq!(fixture.summary().unknown_steps, 0);
    let next = journal
        .prepare_step(Mutation {
            operation_id: "volte-two".into(),
            kind: MutationKind::Volte { disabled: 0 },
        })
        .unwrap();
    next.record_submitted_unknown().unwrap();
    assert!(
        next.record_verified_readback(Readback::Volte { disabled: 0 })
            .is_err()
    );
    assert!(journal.prepare_step(mutation("ims-three")).is_err());
    let summary = fixture.summary();
    assert_eq!(summary.prepared_steps, 2);
    assert_eq!(summary.unknown_steps, 1);
    assert!(journal.finish_after_release(receipt()).is_err());
    let records = fixture.active_bytes();
    let actual: Vec<serde_json::Value> = records
        .iter()
        .map(|(_, bytes)| serde_json::from_slice(bytes).unwrap())
        .collect();
    assert!(
        actual
            .iter()
            .any(|record| record["event"]["event"] == "verified_readback"
                && record["event"]["readback"]["ims"] == 0)
    );
}

#[tokio::test]
async fn reboot_only_refinement_retains_original_unknown_record() {
    let fixture = Fixture::new().await;
    let journal = fixture.begin().await;
    assert!(journal.completed_for_root(&fixture.proof()).is_err());
    safety(&journal, &fixture);
    let token = journal
        .prepare_step(Mutation {
            operation_id: "one-reboot".into(),
            kind: MutationKind::Reboot,
        })
        .unwrap();
    token.record_submitted_unknown().unwrap();
    token.record_reboot_verified_after_successor().unwrap();
    assert_eq!(fixture.summary().unknown_steps, 0);
    let bytes = fixture.active_bytes();
    assert!(
        bytes
            .iter()
            .any(|(_, b)| String::from_utf8_lossy(b).contains("submitted_unknown"))
    );
    assert!(
        bytes
            .iter()
            .any(|(_, b)| String::from_utf8_lossy(b).contains("reboot_successor_verified"))
    );
}

#[tokio::test]
async fn actual_sync_failure_before_prepare_or_after_submission_is_sticky_across_clones() {
    for after_submission in [false, true] {
        for point in [
            storage::Point::FileSync,
            storage::Point::DirectorySync,
            storage::Point::RootSync,
        ] {
            let fixture = Fixture::new().await;
            let journal = fixture.begin().await;
            safety(&journal, &fixture);
            let clone = journal.clone();
            let prepared =
                after_submission.then(|| journal.prepare_step(mutation("one-ims")).unwrap());
            fault(&journal, point);
            if let Some(prepared) = prepared {
                assert!(prepared.record_submitted_unknown().is_err());
            } else {
                assert!(journal.prepare_step(mutation("one-ims")).is_err());
            }
            assert!(clone.prepare_step(mutation("two-ims")).is_err());
            assert!(clone.released().is_err());
            assert!(fixture.root.join(ACTIVE_NAME).is_dir());
            // The failed record may be complete in the live page cache, but no
            // caller may infer durability or continue after the failed syscall.
        }
    }
}

#[tokio::test]
async fn named_active_root_or_record_changes_revoke_writer_without_repair() {
    for case in 0..4 {
        let fixture = Fixture::new().await;
        let journal = fixture.begin().await;
        match case {
            0 => {
                fs::rename(
                    fixture.root.join(ACTIVE_NAME),
                    fixture.root.join("retained-active"),
                )
                .unwrap();
                fs::create_dir(fixture.root.join(ACTIVE_NAME)).unwrap();
            }
            1 => {
                let path = fixture.root.join(ACTIVE_NAME).join("0001.json");
                let mut bytes = fs::read(&path).unwrap();
                let offset = bytes.iter().position(|b| *b == b'7').unwrap();
                bytes[offset] = b'8';
                fs::write(path, bytes).unwrap();
            }
            2 => {
                fs::set_permissions(
                    fixture.root.join(ACTIVE_NAME).join("0001.json"),
                    fs::Permissions::from_mode(0o644),
                )
                .unwrap();
            }
            _ => {
                fs::write(
                    fixture.root.join(ACTIVE_NAME).join("unexpected"),
                    b"do not remove",
                )
                .unwrap();
            }
        }
        assert!(
            journal
                .safety_checkpoint(fixture.captured.id(), &fixture.captured.sha256())
                .is_err()
        );
        assert!(journal.validate_root(&fixture.proof()).is_err());
        assert!(fixture.root.join(ACTIVE_NAME).exists());
    }
}

#[tokio::test]
async fn release_without_target_is_durable_diagnostic_but_never_success_receipt() {
    let fixture = Fixture::new().await;
    let journal = fixture.begin().await;
    journal.released().unwrap();
    let summary = fixture.summary();
    assert!(summary.released);
    assert!(!summary.target_verified);
    assert!(journal.finish_after_release(receipt()).is_err());
    assert!(!fixture.root.join(ARCHIVE_NAME).exists());
}

#[tokio::test]
async fn target_and_actual_release_checkpoints_precede_durable_archive() {
    let fixture = Fixture::new().await;
    let journal = fixture.begin().await;
    let phases = Arc::new(Mutex::new(Vec::new()));
    // A hook registered for another real root cannot affect this journal.
    let other = Fixture::new().await;
    let seen = phases.clone();
    let _hook = set_test_hook(
        &other.root,
        Arc::new(move |phase, _| seen.lock().unwrap().push(phase)),
    )
    .unwrap();
    let mut expected = receipt();
    expected.changed = false;
    journal.target_verified(expected.clone()).unwrap();
    journal.released().unwrap();
    expected.restored_at = "actual-release-time".into();
    let run_id = fixture.summary().run_id;
    journal.finish_after_release(expected).unwrap();
    journal.completed_for_root(&fixture.proof()).unwrap();
    assert!(!fixture.root.join(ACTIVE_NAME).exists());
    assert!(phases.lock().unwrap().is_empty());
    let summary = fixture
        .proof()
        .with_root(&fixture.root, |root| {
            diagnostic_archived_summary(root, &run_id)
        })
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(summary.receipt_present && summary.target_verified && summary.released);
    assert_eq!(summary.sequence, 4);
    assert!(
        fixture
            .proof()
            .with_root(&fixture.root, diagnostic_summary)
            .unwrap()
            .unwrap()
            .is_none()
    );
    for entry in fs::read_dir(fixture.root.join(ARCHIVE_NAME).join(run_id)).unwrap() {
        let metadata = entry.unwrap().metadata().unwrap();
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.mode() & 0o7777, 0o600);
    }
    fs::create_dir(fixture.root.join(ACTIVE_NAME)).unwrap();
    assert!(journal.completed_for_root(&fixture.proof()).is_err());
}

#[tokio::test]
async fn receipt_or_archive_sync_failure_never_returns_success_and_keeps_evidence() {
    for point in [
        storage::Point::FileSync,
        storage::Point::ArchiveLink,
        storage::Point::ArchiveSync,
        storage::Point::ActiveRemove,
        storage::Point::FinalRootSync,
    ] {
        let fixture = Fixture::new().await;
        let journal = fixture.begin().await;
        let mut expected = receipt();
        expected.changed = false;
        journal.target_verified(expected.clone()).unwrap();
        journal.released().unwrap();
        expected.restored_at = "release-completed".into();
        let run_id = fixture.summary().run_id;
        fault(&journal, point);
        assert!(journal.finish_after_release(expected.clone()).is_err());
        assert!(journal.clone().finish_after_release(expected).is_err());
        assert!(
            fixture.root.join(ACTIVE_NAME).exists()
                || fixture.root.join(ARCHIVE_NAME).join(run_id).exists()
        );
    }
}

#[tokio::test]
async fn actual_root_hooks_observe_only_real_durable_boundaries() {
    let fixture = Fixture::new().await;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let seen = phases.clone();
    let _hook = set_test_hook(
        &fixture.root,
        Arc::new(move |phase, summary| seen.lock().unwrap().push((phase, summary.clone()))),
    )
    .unwrap();
    let journal = fixture.begin().await;
    safety(&journal, &fixture);
    let token = journal.prepare_step(mutation("ims-one")).unwrap();
    token.record_acknowledged().unwrap();
    token
        .record_verified_readback(Readback::Ims {
            ims: 1,
            volte_capability: 1,
        })
        .unwrap();
    let mut expected = receipt();
    journal.target_verified(expected.clone()).unwrap();
    journal.released().unwrap();
    expected.restored_at = "released".into();
    journal.finish_after_release(expected).unwrap();
    let phases = phases.lock().unwrap();
    assert_eq!(phases.first().unwrap().0, TestPhase::BeforeIntent);
    assert_eq!(phases.first().unwrap().1.sequence, 0);
    assert_eq!(phases.last().unwrap().0, TestPhase::Archived);
    assert!(phases.last().unwrap().1.receipt_present);
    let prepared = phases
        .iter()
        .find(|(p, _)| *p == TestPhase::Prepared)
        .unwrap();
    assert_eq!(prepared.1.prepared_steps, 1);
    assert_eq!(prepared.1.unknown_steps, 1);
    let released = phases
        .iter()
        .find(|(p, _)| *p == TestPhase::Released)
        .unwrap();
    assert!(released.1.released && !released.1.receipt_present);
}

#[tokio::test]
async fn scanner_rejects_symlink_hardlink_fifo_extra_name_and_truncated_record_without_writing() {
    for case in 0..5 {
        let fixture = Fixture::new().await;
        let _journal = fixture.begin().await;
        let active = fixture.root.join(ACTIVE_NAME);
        let record = active.join("0001.json");
        match case {
            0 => {
                fs::rename(&active, fixture.root.join("retained-active")).unwrap();
                std::os::unix::fs::symlink("retained-active", &active).unwrap();
            }
            1 => fs::hard_link(&record, fixture.root.join("retained-record")).unwrap(),
            2 => {
                fs::remove_file(&record).unwrap();
                nix::unistd::mkfifo(
                    &record,
                    nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
                )
                .unwrap();
            }
            3 => fs::write(active.join("额外"), b"no repair").unwrap(),
            _ => fs::write(&record, b"{partial").unwrap(),
        }
        let named = fs::symlink_metadata(&active).unwrap();
        let fd = File::open(&fixture.root).unwrap();
        assert!(diagnostic_summary(&fd).is_err());
        assert_eq!(fs::symlink_metadata(&active).unwrap().ino(), named.ino());
    }
}

#[tokio::test]
async fn same_length_record_mutation_at_real_readback_cannot_complete_prepare() {
    let fixture = Fixture::new().await;
    let journal = fixture.begin().await;
    safety(&journal, &fixture);
    let path = fixture.root.join(ACTIVE_NAME).join("0003.json");
    journal
        .0
        .state
        .lock()
        .unwrap()
        .store
        .set_fault(move |point| {
            if point == storage::Point::Readback {
                let mut payload = fs::read(&path)?;
                let last = payload.len() - 1;
                payload[last] = b' ';
                fs::write(&path, payload)?;
            }
            Ok(())
        });
    assert!(journal.prepare_step(mutation("one-ims")).is_err());
    assert!(journal.clone().prepare_step(mutation("two-ims")).is_err());
    assert_eq!(
        fs::read_dir(fixture.root.join(ACTIVE_NAME))
            .unwrap()
            .count(),
        3
    );
}

#[tokio::test]
async fn scanner_checks_hash_chain_and_byte_bound_on_the_same_open_file() {
    for oversized in [false, true] {
        let fixture = Fixture::new().await;
        let journal = fixture.begin().await;
        safety(&journal, &fixture);
        let path = fixture.root.join(ACTIVE_NAME).join("0002.json");
        if oversized {
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(MAX_RECORD_BYTES as u64 + 1)
                .unwrap();
        } else {
            let mut record: Record = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            record.previous_sha256 = Some("0".repeat(64));
            fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        }
        let before = fs::read(&path).unwrap();
        assert!(
            fixture
                .proof()
                .with_root(&fixture.root, diagnostic_summary)
                .unwrap()
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn panic_after_durable_prepare_poisoning_never_allows_a_second_mutation() {
    let fixture = Fixture::new().await;
    let _hook = set_test_hook(
        &fixture.root,
        Arc::new(|phase, _| {
            if phase == TestPhase::Prepared {
                panic!("intentional post-sync owner panic");
            }
        }),
    )
    .unwrap();
    let journal = fixture.begin().await;
    safety(&journal, &fixture);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        journal.prepare_step(mutation("one-ims"))
    }));
    assert!(result.is_err());
    assert!(journal.prepare_step(mutation("two-ims")).is_err());
    assert!(!journal.matches_reservation("restore-producer", "restore-reservation", 7));
    let summary = fixture.summary();
    assert_eq!(summary.prepared_steps, 1);
    assert_eq!(summary.unknown_steps, 1);
    assert!(!summary.receipt_present);
}

#[tokio::test]
async fn recovery_capture_pins_typed_unknown_history_without_mutating_it() {
    let fixture = Fixture::new().await;
    let journal = fixture.begin().await;
    safety(&journal, &fixture);
    let _unknown = journal.prepare_step(mutation("unconfirmed-ims")).unwrap();
    let before = fixture.active_bytes();
    let capture = capture_for_recovery(&fixture.root, &fixture.proof())
        .unwrap()
        .unwrap();
    assert_eq!(capture.intent(), &intent());
    assert_eq!(capture.summary().unknown_steps, 1);
    assert_eq!(capture.summary().sequence, 3);
    assert_eq!(capture.events().count(), 3);
    assert!(
        matches!(capture.events().last(), Some(Event::Prepare { mutation }) if mutation.operation_id == "unconfirmed-ims")
    );
    assert!(crate::call_mode_backups::valid_backup_id(
        capture.backup_evidence().0
    ));
    assert_eq!(capture.backup_evidence().1.len(), 64);
    let expected_digest = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(&before.last().unwrap().1)
    );
    assert_eq!(capture.evidence_sha256(), expected_digest);
    capture.verify_unchanged(&fixture.proof()).unwrap();
    assert_eq!(fixture.active_bytes(), before);
    assert!(fixture.summary().unknown_steps > 0);
}

#[tokio::test]
async fn recovery_capture_cannot_absorb_a_later_real_outcome() {
    let fixture = Fixture::new().await;
    let journal = fixture.begin().await;
    safety(&journal, &fixture);
    let step = journal.prepare_step(mutation("one-ims")).unwrap();
    let capture = capture_for_recovery(&fixture.root, &fixture.proof())
        .unwrap()
        .unwrap();
    step.record_acknowledged().unwrap();
    assert!(capture.verify_unchanged(&fixture.proof()).is_err());
    assert_eq!(capture.summary().sequence, 3);
    assert_eq!(capture.events().count(), 3);
    let fresh = capture_for_recovery(&fixture.root, &fixture.proof())
        .unwrap()
        .unwrap();
    assert_eq!(fresh.summary().sequence, 4);
    assert!(matches!(
        fresh.events().last(),
        Some(Event::Outcome {
            outcome: Outcome::Acknowledged,
            ..
        })
    ));
    assert_eq!(fresh.summary().unknown_steps, 1);
}

#[tokio::test]
async fn recovery_capture_rejects_record_inplace_change_and_name_replacement() {
    for replace in [false, true] {
        let fixture = Fixture::new().await;
        let _journal = fixture.begin().await;
        let capture = capture_for_recovery(&fixture.root, &fixture.proof())
            .unwrap()
            .unwrap();
        let path = fixture.root.join(ACTIVE_NAME).join("0001.json");
        let bytes = fs::read(&path).unwrap();
        if replace {
            fs::rename(&path, path.with_extension("saved")).unwrap();
            fs::write(&path, &bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            let mut altered = bytes.clone();
            altered[0] = b'[';
            fs::write(&path, &altered).unwrap();
        }
        let after = fixture.active_bytes();
        assert!(capture.verify_unchanged(&fixture.proof()).is_err());
        assert_eq!(fixture.active_bytes(), after);
    }
}

#[tokio::test]
async fn recovery_capture_rejects_active_symlink_and_directory_replacement() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new().await;
    let _journal = fixture.begin().await;
    let capture = capture_for_recovery(&fixture.root, &fixture.proof())
        .unwrap()
        .unwrap();
    let active = fixture.root.join(ACTIVE_NAME);
    let saved = fixture.root.join("original-active");
    fs::rename(&active, &saved).unwrap();
    symlink(&saved, &active).unwrap();
    assert!(capture.verify_unchanged(&fixture.proof()).is_err());
    assert!(capture_for_recovery(&fixture.root, &fixture.proof()).is_err());
    fs::remove_file(&active).unwrap();
    fs::create_dir(&active).unwrap();
    fs::set_permissions(&active, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(capture.verify_unchanged(&fixture.proof()).is_err());
    assert!(!fs::read(saved.join("0001.json")).unwrap().is_empty());
}

#[tokio::test]
async fn recovery_capture_does_not_accept_foreign_or_ended_root_authority() {
    let mut fixture = Fixture::new().await;
    let foreign = Fixture::new().await;
    let _journal = fixture.begin().await;
    let proof = fixture.proof();
    let capture = capture_for_recovery(&fixture.root, &proof)
        .unwrap()
        .unwrap();
    assert!(capture.verify_unchanged(&foreign.proof()).is_err());
    assert!(capture_for_recovery(&fixture.root, &foreign.proof()).is_err());
    let before = fixture.active_bytes();
    drop(fixture.lease.take());
    assert!(capture.verify_unchanged(&proof).is_err());
    let new_lifetime = crate::daemon_lifetime::Lease::acquire(&fixture.root).unwrap();
    assert!(
        capture
            .verify_unchanged(&new_lifetime.root_proof())
            .is_err()
    );
    assert_eq!(fixture.active_bytes(), before);
    assert!(
        capture_for_recovery(&fixture.root, &new_lifetime.root_proof())
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn recovery_capture_fails_closed_on_extra_files_hardlinks_and_bad_chain() {
    for fault in [0, 1, 2] {
        let fixture = Fixture::new().await;
        let _journal = fixture.begin().await;
        let active = fixture.root.join(ACTIVE_NAME);
        let path = active.join("0001.json");
        match fault {
            0 => fs::write(active.join("extra.json"), b"{}\n").unwrap(),
            1 => fs::hard_link(&path, fixture.root.join("record-alias")).unwrap(),
            _ => {
                let mut value: Record = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value.previous_sha256 = Some("0".repeat(64));
                fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        let before = fixture.active_bytes();
        assert!(capture_for_recovery(&fixture.root, &fixture.proof()).is_err());
        assert_eq!(fixture.active_bytes(), before);
    }
}

#[tokio::test]
async fn legacy_intent_stays_explicitly_unbound_and_preserves_schema_one_bytes() {
    let fixture = Fixture::new().await;
    let _journal = fixture.begin().await;
    let before = fixture.active_bytes();
    let value: serde_json::Value = serde_json::from_slice(&before[0].1).unwrap();
    assert_eq!(value["schema"], 1);
    assert!(value["event"]["intent"].get("recovery_binding").is_none());
    assert!(
        value["event"]["intent"]
            .get("voice_recovery_binding")
            .is_none()
    );
    let capture = capture_for_recovery(&fixture.root, &fixture.proof())
        .unwrap()
        .unwrap();
    assert!(capture.intent().recovery_binding.is_none());
    assert!(capture.intent().voice_recovery_binding.is_none());
    assert_eq!(fixture.active_bytes(), before);
}

fn voice_observation(
    producer: &str,
    observation: serde_json::Value,
) -> crate::call_mode_adb::DurableVoiceBinding {
    serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "producer": producer,
        "owner_instance": "a".repeat(64),
        "registry_revision": 0,
        "observation": observation,
    }))
    .unwrap()
}

#[tokio::test]
async fn original_voice_observation_survives_real_journal_capture_without_rewriting() {
    for observation in [
        serde_json::json!({"kind": "current_process_empty"}),
        serde_json::json!({"kind": "unknown", "reason": "read_unavailable"}),
    ] {
        let fixture = Fixture::new().await;
        let mut request = intent();
        request.voice_recovery_binding =
            Some(voice_observation(&request.producer_id, observation.clone()));
        let journal = Journal::begin(
            &fixture.root,
            &fixture.proof(),
            &fixture.captured,
            request.clone(),
        )
        .await
        .unwrap();
        let before = fixture.active_bytes();
        let record: serde_json::Value = serde_json::from_slice(&before[0].1).unwrap();
        assert_eq!(
            record["event"]["intent"]["voice_recovery_binding"]["observation"],
            observation
        );
        let capture = capture_for_recovery(&fixture.root, &fixture.proof())
            .unwrap()
            .unwrap();
        assert_eq!(capture.intent(), &request);
        assert!(!capture.summary().released && !capture.summary().receipt_present);
        capture.verify_unchanged(&fixture.proof()).unwrap();
        assert_eq!(fixture.active_bytes(), before);
        drop(journal);
    }
}

#[tokio::test]
async fn foreign_voice_producer_is_rejected_before_creating_or_readmitting_a_journal() {
    let fixture = Fixture::new().await;
    let mut request = intent();
    request.voice_recovery_binding = Some(voice_observation(
        "other-producer",
        serde_json::json!({"kind": "current_process_empty"}),
    ));
    assert!(
        Journal::begin(&fixture.root, &fixture.proof(), &fixture.captured, request)
            .await
            .is_err()
    );
    assert!(!fixture.root.join(ACTIVE_NAME).exists());

    let _journal = fixture.begin().await;
    let path = fixture.root.join(ACTIVE_NAME).join("0001.json");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["event"]["intent"]["voice_recovery_binding"] = serde_json::to_value(voice_observation(
        "other-producer",
        serde_json::json!({"kind": "unknown", "reason": "read_unavailable"}),
    ))
    .unwrap();
    fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    let before = fixture.active_bytes();
    assert!(capture_for_recovery(&fixture.root, &fixture.proof()).is_err());
    assert_eq!(fixture.active_bytes(), before);
}
