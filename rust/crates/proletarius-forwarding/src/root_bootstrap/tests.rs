use super::*;
use fixture::{Moment, OwnerKind, TrackedOwners, create_fresh};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
struct TestRoot {
    parent: PathBuf,
    root: PathBuf,
}
impl TestRoot {
    fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let parent = PathBuf::from(std::env::var_os("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "tmp/b3b-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root: parent.join("new-data-root"),
            parent,
        }
    }
    fn drained(&self) -> fixture::DrainedOwners {
        let owners = TrackedOwners::new(&self.root).unwrap();
        owners.begin_drain().unwrap();
        owners.try_drained().unwrap()
    }
}
impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}
fn ignored(_: Stage, _: Moment) -> Result<(), BootstrapError> {
    Ok(())
}
#[test]
fn b3b_production_entry_rejects_without_creating_even_a_guard() {
    let f = TestRoot::new();
    assert!(matches!(
        bootstrap_production(&f.root),
        Err(BootstrapError {
            stage: Stage::ProductionGate,
            kind: FailureKind::OwnerIntegrationUnproven
        })
    ));
    assert_eq!(fs::read_dir(&f.parent).unwrap().count(), 0);
}
#[test]
fn b3b_all_local_owner_categories_must_finish_and_gate_never_reopens() {
    let f = TestRoot::new();
    let owners = TrackedOwners::new(&f.root).unwrap();
    let mut leases = [
        OwnerKind::Producer,
        OwnerKind::Configuration,
        OwnerKind::SourceWriter,
        OwnerKind::QueueIo,
        OwnerKind::Provider,
    ]
    .map(|k| Some(owners.acquire(k).unwrap()));
    assert!(matches!(
        owners.try_drained(),
        Err(BootstrapError {
            kind: FailureKind::OwnersNotDrained,
            ..
        })
    ));
    owners.begin_drain().unwrap();
    assert!(owners.acquire(OwnerKind::Producer).is_err());
    for lease in &mut leases {
        assert!(matches!(
            owners.try_drained(),
            Err(BootstrapError {
                kind: FailureKind::OwnersNotDrained,
                ..
            })
        ));
        drop(lease.take());
    }
    let token = owners.try_drained().unwrap();
    assert!(owners.try_drained().is_err());
    drop(token);
    assert!(owners.acquire(OwnerKind::Provider).is_err());
    assert!(owners.try_drained().is_err());
}
#[test]
fn b3b_started_thread_keeps_lease_after_callers_stop_waiting() {
    let f = TestRoot::new();
    let owners = TrackedOwners::new(&f.root).unwrap();
    let lease = owners.acquire(OwnerKind::QueueIo).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        rx.recv().unwrap();
        drop(lease);
    });
    owners.begin_drain().unwrap();
    assert!(owners.try_drained().is_err());
    tx.send(()).unwrap();
    worker.join().unwrap();
    assert!(owners.try_drained().is_ok());
}
#[test]
fn b3b_local_sync_sequence_does_not_turn_preflight_into_authority() {
    let f = TestRoot::new();
    let mut trace = Vec::new();
    let receipt = create_fresh(f.drained(), |s, m| {
        trace.push((s, m));
        Ok(())
    })
    .unwrap();
    assert!(
        receipt.report().local_sync_sequence_completed
            && receipt.report().local_tracked_owners_drained
    );
    assert!(
        !receipt.report().production_authorized
            && !receipt.report().daemon_owner_integration_proven
            && !receipt.report().historical_fresh_origin_proven
            && !receipt.report().reopen_can_reissue_receipt
    );
    let after = |s| trace.iter().position(|v| *v == (s, Moment::After)).unwrap();
    assert!(after(Stage::RootParentSync) < after(Stage::SourceCreate));
    assert!(after(Stage::SourceFileSync) < after(Stage::SourceDirectorySync));
    assert!(after(Stage::SourceDirectorySync) < after(Stage::RootCreate));
    assert!(after(Stage::RootDirectorySync) < after(Stage::WitnessCreate));
    assert!(after(Stage::WitnessDirectorySync) < after(Stage::FinalCheck));
    let r = crate::root_preflight::inspect(&f.root).unwrap();
    assert!(r.fresh_intent_consistent);
    assert!(!r.production_authorized && !r.owner_drained && !r.creation_order_durability_proven);
    drop(receipt);
    assert!(matches!(
        create_fresh(f.drained(), ignored),
        Err(BootstrapError {
            kind: FailureKind::Conflict,
            stage: Stage::Mkdir
        })
    ));
}
#[test]
fn b3b_existing_empty_array_partial_or_complete_root_never_adopted() {
    for name in [
        None,
        Some("sms-forwarding-admission.json"),
        Some(crate::root_preflight::SOURCE_FILE),
    ] {
        let f = TestRoot::new();
        fs::create_dir(&f.root).unwrap();
        fs::set_permissions(&f.root, fs::Permissions::from_mode(0o700)).unwrap();
        if let Some(name) = name {
            fs::write(f.root.join(name), b"[]").unwrap();
        }
        let before: Vec<_> = fs::read_dir(&f.root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(matches!(
            create_fresh(f.drained(), ignored),
            Err(BootstrapError {
                kind: FailureKind::Conflict,
                ..
            })
        ));
        let after: Vec<_> = fs::read_dir(&f.root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(before, after);
        if let Some(name) = name {
            assert_eq!(fs::read(f.root.join(name)).unwrap(), b"[]");
        }
    }
}
#[test]
fn b3b_every_creation_boundary_fails_without_receipt_and_retry_never_reseeds() {
    let stages = [
        Stage::ParentOpen,
        Stage::GuardOpen,
        Stage::GuardLock,
        Stage::GuardSync,
        Stage::GuardDirectorySync,
        Stage::Random,
        Stage::Mkdir,
        Stage::RootOpen,
        Stage::RootParentSync,
        Stage::SourceCreate,
        Stage::SourceWrite,
        Stage::SourceFileSync,
        Stage::SourceDirectorySync,
        Stage::RootCreate,
        Stage::RootWrite,
        Stage::RootFileSync,
        Stage::RootDirectorySync,
        Stage::WitnessCreate,
        Stage::WitnessWrite,
        Stage::WitnessFileSync,
        Stage::WitnessDirectorySync,
        Stage::FinalCheck,
    ];
    for stage in stages {
        for moment in [Moment::Before, Moment::After] {
            let f = TestRoot::new();
            let failed = create_fresh(f.drained(), |s, m| {
                if s == stage && m == moment {
                    Err(err(s, FailureKind::Injected))
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(failed, Err(e) if e.stage == stage && e.kind == FailureKind::Injected)
            );
            let exists = f.root.exists();
            let retry = create_fresh(f.drained(), ignored);
            if exists {
                assert!(matches!(
                    retry,
                    Err(BootstrapError {
                        kind: FailureKind::Conflict,
                        ..
                    })
                ));
            } else {
                assert!(retry.is_ok());
            }
        }
    }
}
#[test]
fn b3b_namespace_replacement_archive_insertion_and_guard_replacement_refuse() {
    for case in ["root", "archive", "guard"] {
        let f = TestRoot::new();
        let failed = create_fresh(f.drained(), |s, m| {
            if s == Stage::SourceDirectorySync && m == Moment::After {
                match case {
                    "root" => {
                        fs::rename(&f.root, f.parent.join("saved")).unwrap();
                        fs::create_dir(&f.root).unwrap();
                    }
                    "archive" => {
                        fs::write(f.root.join("sms-forwarding-admission.json"), b"[]").unwrap();
                    }
                    _ => {
                        let guard = fs::read_dir(&f.parent)
                            .unwrap()
                            .map(|e| e.unwrap().path())
                            .find(|p| p.file_name().unwrap().to_string_lossy().ends_with(".lock"))
                            .unwrap();
                        fs::rename(&guard, f.parent.join("old-guard")).unwrap();
                        fs::write(&guard, b"").unwrap();
                    }
                }
            }
            Ok(())
        });
        assert!(matches!(
            failed,
            Err(BootstrapError {
                kind: FailureKind::ChangedDuringCreation,
                ..
            })
        ));
    }
}
#[test]
fn b3b_root_replacement_at_first_exposed_boundary_cannot_become_created_root() {
    let f = TestRoot::new();
    let failed = create_fresh(f.drained(), |s, m| {
        if s == Stage::Mkdir && m == Moment::After {
            fs::rename(&f.root, f.parent.join("saved")).unwrap();
            fs::create_dir(&f.root).unwrap();
            fs::set_permissions(&f.root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Ok(())
    });
    assert!(matches!(
        failed,
        Err(BootstrapError {
            kind: FailureKind::ChangedDuringCreation,
            stage: Stage::RootOpen
        })
    ));
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 0);
}
#[test]
fn b3b_parent_symlink_shared_permissions_and_root_symlink_refuse() {
    let f = TestRoot::new();
    let alias = f.parent.with_extension("alias");
    symlink(&f.parent, &alias).unwrap();
    assert!(TrackedOwners::new(&alias.join("child")).is_err());
    fs::remove_file(alias).unwrap();
    fs::set_permissions(&f.parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(TrackedOwners::new(&f.root).is_err());
    fs::set_permissions(&f.parent, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(f.parent.join("missing"), &f.root).unwrap();
    assert!(matches!(
        create_fresh(f.drained(), ignored),
        Err(BootstrapError {
            kind: FailureKind::Conflict,
            ..
        })
    ));
}
#[test]
fn b3b_actual_exclusive_marker_conflicts_preserve_foreign_bytes() {
    for (stage, name) in [
        (Stage::SourceCreate, crate::root_preflight::SOURCE_FILE),
        (Stage::RootCreate, crate::root_preflight::ROOT_FILE),
        (Stage::WitnessCreate, crate::root_preflight::WITNESS_FILE),
    ] {
        let f = TestRoot::new();
        let failed = create_fresh(f.drained(), |s, m| {
            if s == stage && m == Moment::Before {
                fs::write(f.root.join(name), b"[]").unwrap();
                fs::set_permissions(f.root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
            }
            Ok(())
        });
        assert!(matches!(failed, Err(e) if e.stage == stage && e.kind == FailureKind::Conflict));
        assert_eq!(fs::read(f.root.join(name)).unwrap(), b"[]");
        assert!(matches!(
            create_fresh(f.drained(), ignored),
            Err(BootstrapError {
                kind: FailureKind::Conflict,
                stage: Stage::Mkdir
            })
        ));
        assert_eq!(fs::read(f.root.join(name)).unwrap(), b"[]");
    }
}

#[test]
fn b3b_fifo_or_hardlinked_parent_guard_rejected_before_open() {
    use sha2::{Digest, Sha256};
    for kind in ["fifo", "hardlink"] {
        let f = TestRoot::new();
        let guard = f.parent.join(format!(
            ".switch-bootstrap-{:x}.lock",
            Sha256::digest(b"new-data-root")
        ));
        if kind == "fifo" {
            nix::unistd::mkfifo(&guard, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        } else {
            let foreign = f.parent.join("foreign");
            fs::write(&foreign, b"never open this guard").unwrap();
            fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
            fs::hard_link(&foreign, &guard).unwrap();
        }
        assert!(matches!(
            create_fresh(f.drained(), ignored),
            Err(BootstrapError {
                kind: FailureKind::UnsafePermissions,
                stage: Stage::GuardOpen
            })
        ));
        assert!(!f.root.exists());
    }
}
