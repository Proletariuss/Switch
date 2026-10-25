//! Real filesystem/kernel-lock tests; no physical device, process or fake proof.
use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

struct Fixture {
    root: PathBuf,
    lease: Option<crate::daemon_lifetime::Lease>,
}
impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/iteration-586-bound-lock")
            .join(crate::snapshot::new_instance_id());
        let lease = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        Self {
            root,
            lease: Some(lease),
        }
    }
    fn proof(&self) -> crate::daemon_lifetime::RootProof {
        self.lease.as_ref().unwrap().root_proof()
    }
    fn lock(&self, kind: Kind) -> PathBuf {
        self.root.join(kind.name())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.lease.take()); /* Retain only this fixture's evidence for root validation. */
    }
}
fn metadata(path: &Path) -> (u64, u64, u32, u32, u32, u64, u64, i64, i64, i64, i64) {
    let value = fs::symlink_metadata(path).unwrap();
    (
        value.dev(),
        value.ino(),
        value.mode(),
        value.uid(),
        value.gid(),
        value.nlink(),
        value.len(),
        value.mtime(),
        value.mtime_nsec(),
        value.ctime(),
        value.ctime_nsec(),
    )
}
fn opened(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

#[test]
fn bound_fresh_locks_are_private_cloexec_and_explicitly_released() {
    let fixture = Fixture::new();
    for kind in [Kind::Operation, Kind::AdbInterface] {
        let guard = acquire_bound(&fixture.root, &fixture.proof(), kind, false).unwrap();
        assert_eq!(guard.file.metadata().unwrap().mode() & 0o7777, 0o600);
        for file in [&guard.file, &guard.directory] {
            assert_ne!(
                nix::fcntl::fcntl(file, nix::fcntl::FcntlArg::F_GETFD).unwrap()
                    & nix::libc::FD_CLOEXEC,
                0
            );
        }
        let witness = guard.release().unwrap();
        witness.validate().unwrap();
        let next = acquire_bound(&fixture.root, &fixture.proof(), kind, false).unwrap();
        witness.validate().unwrap();
        next.release().unwrap().validate().unwrap();
    }
}

#[test]
fn bound_preserves_safe_legacy_root_and_empty_lock_metadata() {
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o755)).unwrap();
    for kind in [Kind::Operation, Kind::AdbInterface] {
        fs::write(fixture.lock(kind), []).unwrap();
        fs::set_permissions(fixture.lock(kind), fs::Permissions::from_mode(0o644)).unwrap();
        let before = metadata(&fixture.lock(kind));
        let guard = acquire_bound(&fixture.root, &fixture.proof(), kind, false).unwrap();
        assert_eq!(metadata(&fixture.lock(kind)), before);
        guard.release().unwrap().validate().unwrap();
        assert_eq!(metadata(&fixture.lock(kind)), before);
        assert_eq!(fs::metadata(&fixture.root).unwrap().mode() & 0o7777, 0o755);
    }
}

#[test]
fn existing_only_missing_locks_leave_root_and_other_fixed_slots_unchanged() {
    let fixture = Fixture::new();
    let root_before = metadata(&fixture.root);
    for kind in [Kind::Operation, Kind::AdbInterface] {
        assert_eq!(
            acquire_bound_existing(&fixture.root, &fixture.proof(), kind, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(!fixture.lock(Kind::Operation).exists());
        assert!(!fixture.lock(Kind::AdbInterface).exists());
        assert_eq!(metadata(&fixture.root), root_before);
    }
}

#[test]
fn existing_only_deleted_after_entry_check_never_recreates_the_original_name() {
    let fixture = Fixture::new();
    for kind in [Kind::Operation, Kind::AdbInterface] {
        let path = fixture.lock(kind);
        fs::write(&path, []).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let original = opened(&path);
        let original_identity = Identity::of(&original.metadata().unwrap());
        let after_removal = std::cell::RefCell::new(None);
        let result = acquire_bound_with_hooks(
            &fixture.root,
            &fixture.proof(),
            kind,
            false,
            false,
            || {
                // This runs after the real nofollow fstatat check, immediately
                // before the real openat. No replacement permission is injected.
                fs::remove_file(&path).unwrap();
                *after_removal.borrow_mut() = Some(metadata(&fixture.root));
            },
            || panic!("an absent existing-only lock cannot reach flock"),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!path.exists());
        assert_eq!(metadata(&fixture.root), after_removal.into_inner().unwrap());
        assert_eq!(
            Identity::of(&original.metadata().unwrap()),
            original_identity
        );
        original.try_lock().unwrap();
        original.unlock().unwrap();
    }
}

#[test]
fn existing_only_safe_lock_preserves_metadata_and_contends_with_creation_capable_entry() {
    let fixture = Fixture::new();
    for kind in [Kind::Operation, Kind::AdbInterface] {
        fs::write(fixture.lock(kind), []).unwrap();
        fs::set_permissions(fixture.lock(kind), fs::Permissions::from_mode(0o644)).unwrap();
        let before = metadata(&fixture.lock(kind));
        let guard = acquire_bound_existing(&fixture.root, &fixture.proof(), kind, false).unwrap();
        assert_eq!(metadata(&fixture.lock(kind)), before);
        assert_eq!(
            acquire_bound(&fixture.root, &fixture.proof(), kind, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(metadata(&fixture.lock(kind)), before);
        let witness = guard.release().unwrap();
        witness.validate().unwrap();
        let legacy = acquire(&fixture.root, kind, false).unwrap();
        assert_eq!(
            acquire_bound_existing(&fixture.root, &fixture.proof(), kind, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        drop(legacy);
        assert_eq!(metadata(&fixture.lock(kind)), before);
        witness.validate().unwrap();
    }
}

#[test]
fn foreign_and_dropped_proofs_cannot_create_any_fixed_lock() {
    let fixture = Fixture::new();
    let foreign = Fixture::new();
    for kind in [Kind::Operation, Kind::AdbInterface] {
        assert!(acquire_bound(&fixture.root, &foreign.proof(), kind, false).is_err());
        assert!(!fixture.lock(kind).exists() && !foreign.lock(kind).exists());
    }
    let mut ended = Fixture::new();
    let proof = ended.proof();
    drop(ended.lease.take());
    for kind in [Kind::Operation, Kind::AdbInterface] {
        assert!(acquire_bound(&ended.root, &proof, kind, false).is_err());
        assert!(!ended.lock(kind).exists());
    }
}

#[test]
fn bound_and_legacy_contend_on_the_same_kernel_lock_without_rewriting() {
    let fixture = Fixture::new();
    for kind in [Kind::Operation, Kind::AdbInterface] {
        let legacy = acquire(&fixture.root, kind, false).unwrap();
        let before = metadata(&fixture.lock(kind));
        assert_eq!(
            acquire_bound(&fixture.root, &fixture.proof(), kind, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            acquire(&fixture.root, kind, false).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(metadata(&fixture.lock(kind)), before);
        drop(legacy);
        let bound = acquire_bound(&fixture.root, &fixture.proof(), kind, false).unwrap();
        assert_eq!(
            acquire(&fixture.root, kind, false).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        bound.release().unwrap();
        drop(acquire(&fixture.root, kind, false).unwrap());
        assert_eq!(metadata(&fixture.lock(kind)), before);
    }
}

#[test]
fn bound_shared_release_preserves_the_other_actual_shared_owner() {
    let fixture = Fixture::new();
    let first = acquire_bound(&fixture.root, &fixture.proof(), Kind::Operation, true).unwrap();
    let second = acquire_bound(&fixture.root, &fixture.proof(), Kind::Operation, true).unwrap();
    let witness = first.release().unwrap();
    assert_eq!(
        acquire(&fixture.root, Kind::Operation, false)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    second.release().unwrap();
    let exclusive = acquire_bound(&fixture.root, &fixture.proof(), Kind::Operation, false).unwrap();
    witness.validate().unwrap();
    exclusive.release().unwrap();
}

#[test]
fn bound_rejects_malicious_shapes_before_opening_and_preserves_them() {
    for shape in 0..7 {
        let fixture = Fixture::new();
        let path = fixture.lock(Kind::Operation);
        match shape {
            0 => fs::write(&path, b"historical-evidence").unwrap(),
            1 => {
                let target = fixture.root.join("symlink-target");
                fs::write(&target, []).unwrap();
                symlink(&target, &path).unwrap();
            }
            2 => fs::create_dir(&path).unwrap(),
            3 => nix::unistd::mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap(),
            4 => {
                fs::write(&path, []).unwrap();
                fs::hard_link(&path, fixture.root.join("second-name")).unwrap();
            }
            5 => {
                fs::write(&path, []).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
                assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o666);
            }
            _ => {
                fs::write(&path, []).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let before = metadata(&path);
        assert!(acquire_bound(&fixture.root, &fixture.proof(), Kind::Operation, false).is_err());
        assert_eq!(metadata(&path), before);
        if shape == 0 {
            assert_eq!(fs::read(&path).unwrap(), b"historical-evidence");
        }
        assert!(!fixture.lock(Kind::AdbInterface).exists());
    }
}

#[test]
fn bound_uses_only_held_root_when_requested_name_is_replaced_during_flock_boundary() {
    let fixture = Fixture::new();
    let retained = fixture.root.with_extension("retained");
    let result = acquire_bound_with_hook(
        &fixture.root,
        &fixture.proof(),
        Kind::Operation,
        false,
        || {
            fs::rename(&fixture.root, &retained).unwrap();
            fs::create_dir(&fixture.root).unwrap();
        },
    );
    assert!(result.is_err());
    assert!(
        !fixture.lock(Kind::Operation).exists(),
        "never open/create through the replacement root"
    );
    let original = opened(&retained.join(Kind::Operation.name()));
    original.try_lock().unwrap();
    original.unlock().unwrap();
}

#[test]
fn replacement_lock_between_open_and_flock_does_not_get_adopted() {
    let fixture = Fixture::new();
    let path = fixture.lock(Kind::Operation);
    let retained = fixture.root.join("retained-operation");
    let result = acquire_bound_with_hook(
        &fixture.root,
        &fixture.proof(),
        Kind::Operation,
        false,
        || {
            fs::rename(&path, &retained).unwrap();
            fs::write(&path, []).unwrap();
        },
    );
    assert!(result.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"");
    let original = opened(&retained);
    original.try_lock().unwrap();
    original.unlock().unwrap();
    let replacement = opened(&path);
    replacement.try_lock().unwrap();
    replacement.unlock().unwrap();
}

#[test]
fn explicit_release_cleans_original_but_never_receipts_a_replaced_namespace() {
    let fixture = Fixture::new();
    let guard = acquire_bound(&fixture.root, &fixture.proof(), Kind::Operation, false).unwrap();
    let path = fixture.lock(Kind::Operation);
    let retained = fixture.root.join("retained-operation");
    fs::rename(&path, &retained).unwrap();
    fs::write(&path, []).unwrap();
    let replacement_before = metadata(&path);
    assert!(guard.release().is_err());
    assert_eq!(metadata(&path), replacement_before);
    let original = opened(&retained);
    original.try_lock().unwrap();
    original.unlock().unwrap();
    let replacement = opened(&path);
    replacement.try_lock().unwrap();
    replacement.unlock().unwrap();
}

#[test]
fn weak_lifetime_binding_does_not_revive_after_same_path_reacquisition() {
    let mut fixture = Fixture::new();
    let old_proof = fixture.proof();
    let guard = acquire_bound(&fixture.root, &old_proof, Kind::Operation, false).unwrap();
    let witness = guard.namespace_witness().unwrap();
    drop(fixture.lease.take());
    assert!(guard.validate().is_err());
    assert!(witness.validate().is_err());
    let next_lifetime = crate::daemon_lifetime::Lease::acquire(&fixture.root).unwrap();
    assert!(acquire_bound(&fixture.root, &old_proof, Kind::AdbInterface, false).is_err());
    assert!(!fixture.lock(Kind::AdbInterface).exists());
    // Cleanup of the exact original still closes it despite ended authority.
    assert!(guard.release().is_err());
    let next = acquire_bound(
        &fixture.root,
        &next_lifetime.root_proof(),
        Kind::Operation,
        false,
    )
    .unwrap();
    assert!(witness.validate().is_err());
    next.release().unwrap();
}

#[test]
fn explicit_unlock_does_not_depend_on_last_duplicate_close_or_witness_drop() {
    let fixture = Fixture::new();
    let guard = acquire_bound(&fixture.root, &fixture.proof(), Kind::Operation, false).unwrap();
    let duplicate = guard.file.try_clone().unwrap();
    let witness = guard.release().unwrap();
    // The duplicate refers to the original OFD; the witness's evidence fd was
    // independently opened. A true explicit unlock permits another owner now.
    let next = acquire(&fixture.root, Kind::Operation, false).unwrap();
    drop(duplicate);
    assert_eq!(
        acquire_bound(&fixture.root, &fixture.proof(), Kind::Operation, false)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    witness.validate().unwrap();
    drop(next);
    acquire_bound(&fixture.root, &fixture.proof(), Kind::Operation, false)
        .unwrap()
        .release()
        .unwrap();
}

#[test]
fn requested_alias_retargeting_invalidates_bound_lock_and_receipt() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    let alias = fixture.root.with_extension("alias");
    symlink(&fixture.root, &alias).unwrap();
    let guard = acquire_bound(&alias, &fixture.proof(), Kind::Operation, false).unwrap();
    fs::remove_file(&alias).unwrap();
    symlink(&other.root, &alias).unwrap();
    assert!(guard.validate().is_err());
    assert!(guard.release().is_err());
    assert!(!other.lock(Kind::Operation).exists());
    let original = opened(&fixture.lock(Kind::Operation));
    original.try_lock().unwrap();
    original.unlock().unwrap();
}
