use super::*;
use crate::daemon_lifetime::Lease;
use crate::module_recovery::{Gate, Reason};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

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
            .join("build/runtime/iteration-670-legacy-locks")
            .join(crate::snapshot::new_instance_id());
        let lease = Lease::acquire(&root).unwrap();
        for name in LOCKS {
            fs::write(root.join(name), []).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
        Self {
            root,
            lease: Some(lease),
        }
    }
    fn proof(&self) -> RootProof {
        self.lease.as_ref().unwrap().root_proof()
    }
    fn migrate(&self) -> io::Result<bool> {
        migrate(&self.root, &self.proof())
    }
    fn gate(&self) -> Gate {
        Gate::inspect_with_lifetime(self.lease.as_ref().unwrap())
    }
    fn snapshot(&self) -> Provenance {
        self.proof()
            .with_root(&self.root, capture)
            .unwrap()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.lease.take());
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn genuine_pair_migrates_without_rewriting_locks_and_restart_reuses_exact_receipt() {
    let mut f = Fixture::new();
    let before = f.snapshot();
    assert!(f.gate().is_quarantined());
    assert!(f.migrate().unwrap());
    assert_eq!(f.snapshot(), before);
    assert!(!f.root.join(".call-mode-pristine-v1").exists());
    let gate = f.gate();
    assert!(!gate.is_quarantined());
    assert!(gate.has_clean_legacy_lock_provenance(&f.root));
    assert!(gate.admit_operation("normal").is_ok());
    let receipt = fs::read(f.root.join(DIRECTORY).join(RECEIPT)).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&receipt).unwrap()["historical_recovery_confirmed"],
        false
    );
    drop(f.lease.take());
    f.lease = Some(Lease::acquire(&f.root).unwrap());
    assert!(!f.migrate().unwrap());
    assert!(!f.gate().is_quarantined());
    assert_eq!(f.snapshot(), before);
    assert_eq!(
        fs::read(f.root.join(DIRECTORY).join(RECEIPT)).unwrap(),
        receipt
    );
    // Actual locks were released, not merely represented by a JSON result.
    for kind in [RecoveryLockKind::Operation, RecoveryLockKind::AdbInterface] {
        acquire_bound_existing_recovery_lock(&f.root, &f.proof(), kind, false)
            .unwrap()
            .release()
            .unwrap();
    }
}

#[test]
fn missing_pair_and_every_other_recovery_slot_prevent_migration_without_new_files() {
    for name in LOCKS {
        let f = Fixture::new();
        fs::remove_file(f.root.join(name)).unwrap();
        assert!(!f.migrate().unwrap());
        assert!(f.gate().is_quarantined());
        assert!(!f.root.join(DIRECTORY).exists());
    }
    for name in ABSENT {
        let f = Fixture::new();
        fs::write(f.root.join(name), b"unknown").unwrap();
        assert!(!f.migrate().unwrap(), "{name}");
        assert!(!f.root.join(DIRECTORY).exists(), "{name}");
    }
}

#[test]
fn unsafe_or_nonempty_lock_is_never_repaired_or_migrated() {
    for name in LOCKS {
        for shape in [
            "nonempty",
            "symlink",
            "hardlink",
            "directory",
            "writable",
            "executable",
        ] {
            let f = Fixture::new();
            let path = f.root.join(name);
            match shape {
                "nonempty" => fs::write(&path, b"unknown").unwrap(),
                "symlink" => {
                    fs::remove_file(&path).unwrap();
                    symlink("missing", &path).unwrap();
                }
                "hardlink" => fs::hard_link(&path, f.root.join("other-name")).unwrap(),
                "directory" => {
                    fs::remove_file(&path).unwrap();
                    fs::create_dir(&path).unwrap();
                }
                "writable" => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o620)).unwrap()
                }
                "executable" => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap()
                }
                _ => unreachable!(),
            }
            let before = fs::symlink_metadata(&path).unwrap();
            assert!(!f.migrate().unwrap(), "{name} {shape}");
            let after = fs::symlink_metadata(&path).unwrap();
            assert_eq!(
                (
                    before.ino(),
                    before.len(),
                    before.mode(),
                    before.ctime_nsec()
                ),
                (after.ino(), after.len(), after.mode(), after.ctime_nsec())
            );
            assert!(!f.root.join(DIRECTORY).exists());
        }
    }
}

#[test]
fn either_busy_real_lock_prevents_publication_and_releases_any_partial_acquisition() {
    for kind in [RecoveryLockKind::Operation, RecoveryLockKind::AdbInterface] {
        let f = Fixture::new();
        let held = acquire_bound_existing_recovery_lock(&f.root, &f.proof(), kind, false).unwrap();
        assert!(f.migrate().is_err());
        assert!(!f.root.join(DIRECTORY).exists());
        held.release().unwrap();
        assert!(f.migrate().unwrap());
    }
}

#[test]
fn physical_intent_durably_revokes_and_missing_receipt_never_remigrates() {
    let f = Fixture::new();
    f.migrate().unwrap();
    crate::call_mode_lifecycle::invalidate_pristine_marker(&f.root).unwrap();
    assert!(f.root.join(DIRECTORY).join(REVOKED).exists());
    assert!(!f.migrate().unwrap());
    assert!(f.gate().is_quarantined());
    fs::remove_file(f.root.join(DIRECTORY).join(RECEIPT)).unwrap();
    assert!(!f.migrate().unwrap());
    assert!(f.gate().is_quarantined());
    // Idempotent invalidation never removes the durable revocation.
    invalidate(&f.root).unwrap();
}

#[test]
fn partial_commit_deleted_receipt_and_replaced_inode_all_fail_closed() {
    for shape in [
        "namespace_only",
        "uncommitted",
        "receipt_deleted",
        "inode_replaced",
        "malformed",
    ] {
        let f = Fixture::new();
        f.migrate().unwrap();
        let store = f.root.join(DIRECTORY);
        match shape {
            "namespace_only" => {
                fs::remove_file(store.join(RECEIPT)).unwrap();
                fs::remove_file(store.join(COMMITTED)).unwrap();
            }
            "uncommitted" => fs::remove_file(store.join(COMMITTED)).unwrap(),
            "receipt_deleted" => fs::remove_file(store.join(RECEIPT)).unwrap(),
            "inode_replaced" => {
                fs::rename(f.root.join(LOCKS[0]), f.root.join("old-original-lock")).unwrap();
                fs::write(f.root.join(LOCKS[0]), []).unwrap();
                fs::set_permissions(f.root.join(LOCKS[0]), fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            "malformed" => fs::write(store.join(RECEIPT), b"{}").unwrap(),
            _ => unreachable!(),
        }
        assert!(!f.migrate().unwrap(), "{shape}");
        assert!(f.gate().is_quarantined(), "{shape}");
    }
}

#[test]
fn later_unknown_recovery_evidence_retains_quarantine_without_waiving_other_slots() {
    for name in ABSENT
        .into_iter()
        .filter(|name| *name != ".call-mode-pristine-v1")
    {
        let f = Fixture::new();
        f.migrate().unwrap();
        fs::write(f.root.join(name), b"unknown").unwrap();
        assert!(!f.migrate().unwrap());
        let gate = f.gate();
        assert!(gate.is_quarantined(), "{name}");
        assert!(!gate.has_clean_legacy_lock_provenance(&f.root), "{name}");
        if name == super::super::LEGACY_EXECUTION_PENDING {
            assert!(
                gate.status()
                    .unwrap()
                    .reason_codes
                    .contains(&Reason::LegacyExecutionPending)
            );
        }
    }
}

#[test]
fn ended_or_foreign_lifetime_cannot_migrate() {
    let mut f = Fixture::new();
    let proof = f.proof();
    let other = Fixture::new();
    assert!(migrate(&f.root, &other.proof()).is_err());
    drop(f.lease.take());
    assert!(migrate(&f.root, &proof).is_err());
    assert!(!f.root.join(DIRECTORY).exists());
}

#[test]
fn migration_namespace_without_original_locks_never_becomes_a_clean_root() {
    for revoked in [false, true] {
        let f = Fixture::new();
        f.migrate().unwrap();
        if revoked {
            invalidate(&f.root).unwrap();
        }
        for name in LOCKS {
            fs::remove_file(f.root.join(name)).unwrap();
        }
        assert!(!f.migrate().unwrap());
        let gate = f.gate();
        assert!(gate.is_quarantined());
        assert!(
            gate.status()
                .unwrap()
                .reason_codes
                .contains(&Reason::LegacyLockProvenancePending)
        );
        assert!(!f.root.join(".call-mode-pristine-v1").exists());
    }
}
