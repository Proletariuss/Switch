use super::*;
use crate::daemon_lifetime::Lease;
use crate::module_recovery::Gate;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime/iteration-594-backup-core")
        .join(crate::snapshot::new_instance_id())
}
fn files(path: &Path) -> BTreeMap<PathBuf, (u64, String)> {
    fn visit(base: &Path, path: &Path, out: &mut BTreeMap<PathBuf, (u64, String)>) {
        for row in fs::read_dir(path).unwrap() {
            let path = row.unwrap().path();
            let m = fs::symlink_metadata(&path).unwrap();
            assert!(!m.file_type().is_symlink());
            if m.is_dir() {
                visit(base, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(base).unwrap().into(),
                    (m.ino(), storage::digest(&fs::read(path).unwrap())),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(path, path, &mut out);
    out
}

#[test]
fn legacy_readable_backup_delete_preserves_0644_inode_raw_and_0755_parent() {
    let root = root();
    let lease = Lease::acquire(&root).unwrap();
    let proof = lease.root_proof();
    fs::create_dir(root.join("module-backups")).unwrap();
    fs::set_permissions(
        root.join("module-backups"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let id = "usb-imported-legacy-readable.json";
    fs::write(root.join("module-backups").join(id), b"{}").unwrap();
    fs::set_permissions(
        root.join("module-backups").join(id),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let file = File::open(root.join("module-backups").join(id)).unwrap();
    let initial = file.metadata().unwrap();
    let selected = BackupDeleteSelection::capture(&file, &initial, b"{}").unwrap();
    prepare_delete_selected(&root, &proof, id, &selected)
        .unwrap()
        .commit()
        .unwrap();
    assert!(!root.join("module-backups").join(id).exists());
    let archive = root.join(storage::NAME).join(NAME).join("000001/archive");
    let after = fs::metadata(&archive).unwrap();
    assert_eq!(
        (after.dev(), after.ino(), after.mode(), after.nlink()),
        (initial.dev(), initial.ino(), initial.mode(), 1)
    );
    assert_eq!(fs::read(&archive).unwrap(), b"{}");
    assert_eq!(
        fs::metadata(root.join("module-backups")).unwrap().mode() & 0o7777,
        0o755
    );
    resume(&root, &proof).unwrap();
    assert!(proof.with_root(&root, complete).unwrap().unwrap());
}

#[test]
fn lifecycle_only_unknown_generation_or_unrecorded_stage_is_quarantined_on_fresh_gate() {
    for allocated in [false, true] {
        let root = root();
        let lease = Lease::acquire(&root).unwrap();
        let proof = lease.root_proof();
        assert!(!Gate::inspect_with_lifetime(&lease).is_quarantined());
        let permit = prepare_create(
            &root,
            &proof,
            "usb-imported-pending.json",
            BackupWriteKind::Import,
            b"{}",
        )
        .unwrap();
        if allocated {
            // Actual inode allocation with no durable Ready deliberately cannot
            // be adopted merely because a private filename happens to match.
            let f = File::from(
                openat(
                    &permit.transaction,
                    "stage",
                    OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL,
                    Mode::S_IRUSR | Mode::S_IWUSR,
                )
                .unwrap(),
            );
            f.sync_all().unwrap();
            permit.transaction.sync_all().unwrap();
        } else {
            // Model the real mkdir-to-Intent window while retaining original
            // bytes under an explicit external adversarial fixture directory.
            fs::rename(
                root.join(storage::NAME)
                    .join(NAME)
                    .join("000001/intent.json"),
                root.join("retained-test-intent.json"),
            )
            .unwrap();
        }
        drop(permit);
        let before = files(&root);
        assert!(resume(&root, &proof).is_err());
        assert!(Gate::inspect_with_lifetime(&lease).is_quarantined());
        assert!(
            files(&root) == before,
            "failed metadata resume changed pending evidence"
        );
        drop(lease);
        let lease = Lease::acquire(&root).unwrap();
        assert!(Gate::inspect_with_lifetime(&lease).is_quarantined());
        assert!(
            files(&root) == before,
            "fresh Gate inspection changed pending evidence"
        );
    }
}

#[tokio::test]
async fn complete_generation_capacity_failure_preserves_published_inventory_head_and_fresh_gate() {
    let root = root();
    let (lease, gate) =
        crate::startup_recovery::reconcile::tests::prepare_completed_backup_lifecycle(&root, false)
            .await;
    let proof = lease.root_proof();
    let original = fs::read_dir(root.join("module-backups"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    // Keep each import small; this test exercises generation exhaustion with
    // the test-only reduced cap and production retains its full configured cap.
    let raw = fs::read(original).unwrap();
    let mut exhausted = false;
    let mut completed = 0;
    for index in 0..MAX_GENERATIONS {
        let before = files(&root);
        let id = format!("usb-imported-capacity-{index}.json");
        match prepare_create(&root, &proof, &id, BackupWriteKind::Import, &raw) {
            Ok(permit) => {
                permit.commit().unwrap();
                completed += 1;
            }
            Err(error) => {
                assert!(
                    error.to_string().contains("capacity"),
                    "unexpected non-capacity rejection"
                );
                assert!(
                    files(&root) == before,
                    "capacity failure changed durable generation or live backup inventory"
                );
                assert!(!root.join("module-backups").join(id).exists());
                exhausted = true;
                break;
            }
        }
    }
    assert!(
        completed > 0 && exhausted,
        "finite durable capacity must reject at the configured boundary"
    );
    resume(&root, &proof).unwrap();
    assert!(!Gate::inspect_with_lifetime(&lease).is_quarantined());
    // The retained typed anchor detects disappearance and substitution. This
    // does not claim protection when an adversary erases all trusted history.
    let ns = root.join(storage::NAME).join(NAME);
    let retained = root.join("retained-original-lifecycle");
    fs::rename(&ns, &retained).unwrap();
    assert!(Gate::inspect_with_lifetime(&lease).is_quarantined());
    fs::create_dir(&ns).unwrap();
    fs::set_permissions(&ns, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(Gate::inspect_with_lifetime(&lease).is_quarantined());
    fs::rename(&ns, root.join("retained-foreign-empty-lifecycle")).unwrap();
    fs::rename(&retained, &ns).unwrap();
    assert!(!Gate::inspect_with_lifetime(&lease).is_quarantined());
    drop(gate);
    drop(lease);
    let lease = Lease::acquire(&root).unwrap();
    assert!(!Gate::inspect_with_lifetime(&lease).is_quarantined());
}

#[test]
fn first_namespace_capacity_preflight_creates_no_lock_directory_or_intent() {
    let root = root();
    let lease = Lease::acquire(&root).unwrap();
    let proof = lease.root_proof();
    fs::create_dir(root.join("voice-runtime")).unwrap();
    fs::set_permissions(
        root.join("voice-runtime"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    for index in 0..8 {
        let path = root
            .join("voice-runtime")
            .join(format!("bounded-{index}.json"));
        fs::write(&path, vec![b'x'; 1024 * 1024]).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let before = files(&root);
    let reasons = Gate::inspect_with_lifetime(&lease)
        .status()
        .unwrap()
        .reason_codes
        .clone();
    let result = prepare_create(
        &root,
        &proof,
        "usb-imported-capacity-first.json",
        BackupWriteKind::Import,
        b"{}",
    );
    assert!(result.is_err());
    assert!(!root.join(storage::NAME).exists());
    assert!(
        files(&root) == before,
        "first-use budget rejection allocated lifecycle metadata"
    );
    assert_eq!(
        Gate::inspect_with_lifetime(&lease)
            .status()
            .unwrap()
            .reason_codes,
        reasons
    );
    drop(result);
    drop(lease);
    let lease = Lease::acquire(&root).unwrap();
    assert_eq!(
        Gate::inspect_with_lifetime(&lease)
            .status()
            .unwrap()
            .reason_codes,
        reasons
    );
}
#[test]
fn replaced_transaction_directory_cannot_publish_from_detached_held_descriptor() {
    let root = root();
    let lease = Lease::acquire(&root).unwrap();
    let proof = lease.root_proof();
    let permit = prepare_create(
        &root,
        &proof,
        "usb-imported-generation-replacement.json",
        BackupWriteKind::Import,
        b"{}",
    )
    .unwrap();
    let transaction = root.join(storage::NAME).join(NAME).join("000001");
    fs::rename(&transaction, root.join("retained-original-generation")).unwrap();
    fs::create_dir(&transaction).unwrap();
    fs::set_permissions(&transaction, fs::Permissions::from_mode(0o700)).unwrap();
    let before = files(&root);
    assert!(permit.commit().is_err());
    assert!(
        files(&root) == before,
        "detached transaction wrote into a foreign replacement"
    );
    assert!(
        !root
            .join("module-backups/usb-imported-generation-replacement.json")
            .exists()
    );
    assert!(Gate::inspect_with_lifetime(&lease).is_quarantined());
}

#[tokio::test]
async fn fresh_backup_provenance_never_waives_legacy_baseline_or_other_evidence_slots() {
    use crate::module_recovery::Reason;
    for case in [
        "legacy_with_pristine",
        "orphan_lock",
        "unsafe_lock",
        "active_journal",
    ] {
        let root = root();
        let lease = Lease::acquire(&root).unwrap();
        let gate = Gate::inspect_with_lifetime(&lease);
        let lifecycle = if case == "legacy_with_pristine" {
            Some(
                crate::call_mode_backups::lifecycle_tests::initialize_fresh_call_mode_lifecycle(
                    &root, &gate,
                ),
            )
        } else {
            None
        };
        if let Some(lifecycle) = lifecycle {
            lifecycle.shutdown().await;
            fs::create_dir(root.join("module-backups")).unwrap();
            fs::set_permissions(
                root.join("module-backups"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            fs::write(root.join("module-backups/usb-imported-orphan.json"), b"{}").unwrap();
            fs::set_permissions(
                root.join("module-backups/usb-imported-orphan.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let proof = lease.root_proof();
        prepare_create(
            &root,
            &proof,
            "usb-imported-new.json",
            BackupWriteKind::Import,
            b"{}",
        )
        .unwrap()
        .commit()
        .unwrap();
        if case == "legacy_with_pristine" {
            assert!(crate::call_mode_lifecycle::pristine_marker(&root).unwrap());
            assert!(
                proof
                    .with_root(&root, verified_fresh_backup_slot)
                    .unwrap()
                    .unwrap()
                    .is_none()
            );
        } else {
            assert!(
                !Gate::inspect_with_lifetime(&lease).is_quarantined(),
                "complete fresh backup slot was not recognized"
            );
            assert!(
                proof
                    .with_root(&root, verified_fresh_backup_slot)
                    .unwrap()
                    .unwrap()
                    .is_some()
            );
            match case {
                "orphan_lock" | "unsafe_lock" => {
                    fs::write(root.join("call-mode-operation.lock"), b"").unwrap();
                    fs::set_permissions(
                        root.join("call-mode-operation.lock"),
                        fs::Permissions::from_mode(if case == "unsafe_lock" {
                            0o666
                        } else {
                            0o600
                        }),
                    )
                    .unwrap();
                }
                "active_journal" => {
                    fs::create_dir(root.join(crate::call_mode_restore_journal::ACTIVE_NAME))
                        .unwrap();
                    fs::set_permissions(
                        root.join(crate::call_mode_restore_journal::ACTIVE_NAME),
                        fs::Permissions::from_mode(0o700),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
        }
        let expected = match case {
            "legacy_with_pristine" => Reason::ModuleBackupPresent,
            "orphan_lock" | "unsafe_lock" => Reason::LegacyCallModeLock,
            "active_journal" => Reason::RestoreJournalPending,
            _ => unreachable!(),
        };
        let before = files(&root);
        assert!(
            Gate::inspect_with_lifetime(&lease)
                .status()
                .unwrap()
                .reason_codes
                .contains(&expected)
        );
        assert!(
            files(&root) == before,
            "startup verification changed blocked evidence"
        );
        drop(gate);
        drop(lease);
        let lease = Lease::acquire(&root).unwrap();
        assert!(
            Gate::inspect_with_lifetime(&lease)
                .status()
                .unwrap()
                .reason_codes
                .contains(&expected)
        );
        assert!(
            files(&root) == before,
            "fresh lifetime changed blocked evidence"
        );
    }
}

#[test]
fn fresh_backup_provenance_rechecks_exact_record_and_inventory_bytes() {
    let root = root();
    let lease = Lease::acquire(&root).unwrap();
    let proof = lease.root_proof();
    let id = "usb-imported-fresh-proof.json";
    prepare_create(&root, &proof, id, BackupWriteKind::Import, b"{}")
        .unwrap()
        .commit()
        .unwrap();
    let coverage = proof
        .with_root(&root, verified_fresh_backup_slot)
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!Gate::inspect_with_lifetime(&lease).is_quarantined());
    let record = root
        .join(storage::NAME)
        .join(NAME)
        .join("000001/intent.json");
    let original = fs::read(&record).unwrap();
    let mut changed = original.clone();
    changed.push(b' ');
    fs::write(&record, changed).unwrap();
    assert!(
        proof
            .with_root(&root, |root| coverage.validate(root))
            .unwrap()
            .is_err()
    );
    assert!(Gate::inspect_with_lifetime(&lease).is_quarantined());
    fs::write(&record, original).unwrap();
    // Even identical restored bytes have a different record stamp: the old
    // captured witness cannot be reused across the mutation.
    assert!(
        proof
            .with_root(&root, |root| coverage.validate(root))
            .unwrap()
            .is_err()
    );
    let fresh = proof
        .with_root(&root, verified_fresh_backup_slot)
        .unwrap()
        .unwrap()
        .unwrap();
    fs::write(root.join("module-backups").join(id), b"[]").unwrap();
    assert!(
        proof
            .with_root(&root, |root| fresh.validate(root))
            .unwrap()
            .is_err()
    );
    assert!(Gate::inspect_with_lifetime(&lease).is_quarantined());
}
