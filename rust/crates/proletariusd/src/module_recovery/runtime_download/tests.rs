use super::*;
use crate::daemon_lifetime::Lease;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    lease: Option<Lease>,
    runtime: RuntimeManifest,
}
impl Fixture {
    fn new(legacy: bool) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(4).unwrap()
            .join("build/runtime/runtime-download-lineage")
            .join(crate::snapshot::new_instance_id());
        let lease = Lease::acquire(&root).unwrap();
        for name in LOCKS {
            fs::write(root.join(name), []).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
        if legacy {
            assert!(super::super::legacy_locks::migrate(&root, &lease.root_proof()).unwrap());
        } else {
            fs::write(root.join(PRISTINE), []).unwrap();
            fs::set_permissions(root.join(PRISTINE), fs::Permissions::from_mode(0o600)).unwrap();
        }
        let runtime = RuntimeManifest {
            commit: manifest().commit.clone(), version: manifest().version.clone(),
            kernel: manifest().kernel.clone(),
            files: vec![Artifact {
                name: "fixture.bin".into(), size: 3,
                sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
                mode: 0o644, deploy: true,
            }],
        };
        Self { root, lease: Some(lease), runtime }
    }
    fn gate(&self) -> Gate {
        Gate::inspect_with_lifetime_and_artifacts(self.lease.as_ref().unwrap(), &self.runtime.files)
    }
    fn begin(&self) -> io::Result<Option<Directories>> {
        self.lease.as_ref().unwrap().root_proof()
            .with_root(&self.root, |root| begin_in(root, &self.runtime, false)).unwrap()
    }
    fn restart(&mut self) {
        drop(self.lease.take());
        self.lease = Some(Lease::acquire(&self.root).unwrap());
    }
    fn install(&self) {
        let path = self.root.join(RUNTIME).join(&self.runtime.version).join("fixture.bin");
        fs::write(&path, b"abc").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.lease.take());
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn pristine_and_migrated_downloads_survive_network_failure_and_complete_new_lifetimes() {
    for legacy in [false, true] {
        let mut f = Fixture::new(legacy);
        assert!(!f.gate().is_quarantined());
        let original = f.lease.as_ref().unwrap().root_proof()
            .with_root(&f.root, |root| source(root, false)).unwrap().unwrap();
        let directories = f.begin().unwrap().unwrap();
        // The actual worker creates its controlled directories before contacting
        // the network; a failed first request leaves the empty manifest subset.
        drop(directories);
        f.restart();
        let gate = f.gate();
        assert!(!gate.is_quarantined());
        assert!(gate.admit_operation("retry-download").is_ok());
        assert!(f.begin().unwrap().is_some());
        f.install();
        f.restart();
        let gate = f.gate();
        assert!(!gate.is_quarantined());
        assert!(gate.admit_operation("normal").is_ok());
        let current = f.lease.as_ref().unwrap().root_proof()
            .with_root(&f.root, |root| source(root, true)).unwrap().unwrap();
        assert_eq!(current, original);
        assert!(!f.root.join(crate::call_mode_restore_journal::ACTIVE_NAME).exists());
        if legacy {
            assert!(!f.root.join(PRISTINE).exists());
        }
    }
}

#[test]
fn initial_directory_and_partial_receipt_crashes_retry_without_publishing_unknown_runtime() {
    for phase in ["namespace", "runtime", "version", "partial_receipt"] {
        for legacy in [false, true] {
            let mut f = Fixture::new(legacy);
            let store = f.root.join(DIRECTORY);
            fs::create_dir(&store).unwrap();
            fs::set_permissions(&store, fs::Permissions::from_mode(0o700)).unwrap();
            if phase != "namespace" {
                fs::create_dir(store.join(STAGED)).unwrap();
                fs::set_permissions(store.join(STAGED), fs::Permissions::from_mode(0o700)).unwrap();
            }
            if matches!(phase, "version" | "partial_receipt") {
                let version = store.join(STAGED).join(&f.runtime.version);
                fs::create_dir(&version).unwrap();
                fs::set_permissions(&version, fs::Permissions::from_mode(0o700)).unwrap();
            }
            if phase == "partial_receipt" {
                fs::write(store.join(RECEIPT), b"{").unwrap();
                fs::set_permissions(store.join(RECEIPT), fs::Permissions::from_mode(0o600)).unwrap();
            }
            f.restart();
            assert!(!f.gate().is_quarantined(), "{phase} legacy={legacy}");
            assert!(f.begin().unwrap().is_some(), "{phase} legacy={legacy}");
            f.restart();
            assert!(!f.gate().is_quarantined(), "{phase} legacy={legacy}");
        }
    }
}

#[test]
fn committed_prepublication_receipt_resumes_exact_directory_without_network_or_device_io() {
    for legacy in [false, true] {
        let mut f = Fixture::new(legacy);
        drop(f.begin().unwrap().unwrap());
        let runtime_inode = fs::metadata(f.root.join(RUNTIME)).unwrap().ino();
        // This is the exact namespace state after durable COMMITTED and before
        // atomic no-replace publication; receipt and original inode are retained.
        fs::rename(f.root.join(RUNTIME), f.root.join(DIRECTORY).join(STAGED)).unwrap();
        f.restart();
        assert!(!f.gate().is_quarantined());
        drop(f.begin().unwrap().unwrap());
        assert_eq!(fs::metadata(f.root.join(RUNTIME)).unwrap().ino(), runtime_inode);
        f.restart();
        assert!(!f.gate().is_quarantined());
    }
}

#[test]
fn crashed_scratch_bytes_remain_nonexecutable_and_retry_removes_only_bounded_known_parts() {
    let mut f = Fixture::new(false);
    drop(f.begin().unwrap().unwrap());
    let part = f.root.join(DIRECTORY).join(FILES)
        .join(".fixture.bin-00000000000000000000000000000000-0000000000000000.part");
    fs::write(&part, b"a").unwrap();
    fs::set_permissions(&part, fs::Permissions::from_mode(0o600)).unwrap();
    f.restart();
    assert!(!f.gate().is_quarantined());
    drop(f.begin().unwrap().unwrap());
    assert!(!part.exists());
    assert!(fs::read_dir(f.root.join(RUNTIME).join(&f.runtime.version)).unwrap().next().is_none());
}

#[test]
fn physical_intent_and_unsettled_history_never_inherit_download_permission() {
    for legacy in [false, true] {
        for reason in ["physical", "journal", "helper", "legacy_execution", "backup"] {
            let mut f = Fixture::new(legacy);
            drop(f.begin().unwrap().unwrap());
            f.install();
            match reason {
                "physical" => crate::call_mode_lifecycle::invalidate_pristine_marker(&f.root).unwrap(),
                "journal" => fs::write(f.root.join(crate::call_mode_restore_journal::ACTIVE_NAME), b"pending").unwrap(),
                "helper" => fs::create_dir(f.root.join("voice-control")).unwrap(),
                "legacy_execution" => fs::write(f.root.join(super::super::LEGACY_EXECUTION_PENDING), b"pending").unwrap(),
                "backup" => fs::create_dir(f.root.join("module-backups")).unwrap(),
                _ => unreachable!(),
            }
            f.restart();
            let gate = f.gate();
            assert!(gate.is_quarantined(), "{reason} legacy={legacy}");
            assert!(gate.admit_operation("normal").is_err());
            assert!(f.begin().is_err());
        }
    }
}

#[test]
fn receipt_locks_directories_and_installed_bytes_must_keep_exact_provenance() {
    for shape in ["lock", "runtime_inode", "receipt", "extra", "bad_hash", "symlink", "hardlink", "mode", "legacy_source"] {
        let mut f = Fixture::new(true);
        drop(f.begin().unwrap().unwrap());
        f.install();
        let version = f.root.join(RUNTIME).join(&f.runtime.version);
        let file = version.join("fixture.bin");
        match shape {
            "lock" => {
                fs::rename(f.root.join(LOCKS[0]), f.root.join("original-lock")).unwrap();
                fs::write(f.root.join(LOCKS[0]), []).unwrap();
            }
            "runtime_inode" => {
                fs::rename(f.root.join(RUNTIME), f.root.join("original-runtime")).unwrap();
                fs::create_dir(f.root.join(RUNTIME)).unwrap();
            }
            "receipt" => fs::write(f.root.join(DIRECTORY).join(RECEIPT), b"{}").unwrap(),
            "extra" => fs::write(version.join("unknown"), b"abc").unwrap(),
            "bad_hash" => fs::write(&file, b"xyz").unwrap(),
            "symlink" => {
                fs::remove_file(&file).unwrap();
                symlink("missing", &file).unwrap();
            }
            "hardlink" => fs::hard_link(&file, f.root.join("alias")).unwrap(),
            "mode" => fs::set_permissions(&file, fs::Permissions::from_mode(0o666)).unwrap(),
            "legacy_source" => fs::write(f.root.join(super::super::legacy_locks::DIRECTORY).join("provenance.json"), b"{}").unwrap(),
            _ => unreachable!(),
        }
        f.restart();
        assert!(f.gate().is_quarantined(), "{shape}");
        assert!(f.begin().is_err(), "{shape}");
    }
}

#[test]
fn unknown_runtime_is_not_adopted_even_when_it_looks_like_an_empty_download() {
    for legacy in [false, true] {
        let f = Fixture::new(legacy);
        fs::create_dir(f.root.join(RUNTIME)).unwrap();
        assert!(f.begin().unwrap().is_none());
        assert!(!f.root.join(DIRECTORY).exists());
        assert!(f.gate().is_quarantined());
    }
}

#[test]
fn retired_download_obligation_survives_removal_of_original_slots() {
    for legacy in [false, true] {
        let mut f = Fixture::new(legacy);
        drop(f.begin().unwrap().unwrap());
        crate::call_mode_lifecycle::invalidate_pristine_marker(&f.root).unwrap();
        for name in LOCKS {
            fs::remove_file(f.root.join(name)).unwrap();
        }
        fs::remove_dir_all(f.root.join(RUNTIME)).unwrap();
        f.restart();
        let gate = f.gate();
        assert!(gate.is_quarantined());
        assert!(gate.status().unwrap().reason_codes.contains(&super::super::Reason::ReconciliationPending));
        assert!(gate.admit_operation("normal").is_err());
    }
}

#[test]
fn revoked_known_partial_cache_is_only_input_for_physical_baseline_and_never_data_permission() {
    for legacy in [false, true] {
        let f = Fixture::new(legacy);
        drop(f.begin().unwrap().unwrap());
        let artifact = f.root.join(RUNTIME).join(&f.runtime.version).join("fixture.bin");
        fs::write(&artifact, b"bad").unwrap();
        fs::set_permissions(&artifact, fs::Permissions::from_mode(0o644)).unwrap();
        crate::call_mode_lifecycle::invalidate_pristine_marker(&f.root).unwrap();
        let gate = f.gate();
        assert!(gate.is_quarantined());
        f.lease.as_ref().unwrap().root_proof().with_root(&f.root, |root| {
            assert!(legacy_current_source_with_manifest(root, &f.runtime).unwrap());
            assert!(pending_intent(root).unwrap());
            let runtime = open_dir(root, RUNTIME).unwrap();
            let version = open_dir(&runtime, &f.runtime.version).unwrap();
            assert!(verify_artifacts(&version, &f.runtime.files).is_err(),
                "real deployment integrity remains required");
        }).unwrap();
        assert!(gate.admit_operation("metadata-only-must-not-open").is_err());
        assert_eq!(fs::read(&artifact).unwrap(), b"bad");
        assert!(f.root.join(DIRECTORY).join(REVOKED).exists());
        fs::write(f.root.join(DIRECTORY).join(RECEIPT), b"{}").unwrap();
        f.lease.as_ref().unwrap().root_proof().with_root(&f.root, |root| {
            assert!(legacy_current_source_with_manifest(root, &f.runtime).is_err());
        }).unwrap();
    }
}
