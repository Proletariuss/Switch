use super::*;
use crate::daemon_lifetime::Lease;
use crate::module_recovery::Gate;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PAYLOAD: &[u8] = b"verified startup runtime";

struct Root {
    path: PathBuf,
    lease: Option<Lease>,
}
impl Root {
    fn unique_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/e583")
            .join(crate::snapshot::new_instance_id())
    }
    fn new() -> Self {
        let path = Self::unique_path();
        let lease = Lease::acquire(&path).unwrap();
        Self {
            path,
            lease: Some(lease),
        }
    }
    fn new_unleased() -> Self {
        let path = Self::unique_path();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self { path, lease: None }
    }
    fn pristine(&self) {
        for name in [PRISTINE, NAMES[2], NAMES[3]] {
            fs::write(self.path.join(name), []).unwrap();
        }
    }
    fn runtime(&self) -> Vec<Artifact> {
        self.pristine();
        let directory = self.version();
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("runtime.bin"), PAYLOAD).unwrap();
        fs::set_permissions(
            directory.join("runtime.bin"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        vec![artifact()]
    }
    fn version(&self) -> PathBuf {
        self.path.join(RUNTIME).join(&manifest().version)
    }
    fn directory(&self) -> File {
        File::from(
            openat(
                &File::open(&self.path).unwrap(),
                OsStr::new("."),
                OFlag::O_RDONLY
                    | OFlag::O_DIRECTORY
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC
                    | OFlag::O_NONBLOCK,
                Mode::empty(),
            )
            .unwrap(),
        )
    }
    fn scan(&self, artifacts: &[Artifact]) -> Vec<Reason> {
        self.lease
            .as_ref()
            .unwrap()
            .root_proof()
            .with_root(&self.path, |held| inspect(held, artifacts))
            .unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        self.lease.take();
        let _ = fs::remove_dir_all(&self.path);
    }
}
fn artifact() -> Artifact {
    Artifact {
        name: "runtime.bin".into(),
        size: PAYLOAD.len() as u64,
        sha256: format!("{:x}", Sha256::digest(PAYLOAD)),
        mode: 0o644,
        deploy: true,
    }
}

// Synthetic signed bytes exercise only the startup receipt verifier. They do
// not stand in for execution of the production DSP/USB close chain.
fn completed_control_fixture(root: &Root) -> (PathBuf, PathBuf) {
    use switch_voice_supervisor::{Command, Exit, State, encode_response};
    let directory = root.path.join("voice-control");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let session = "00112233445566778899aabbccddeeff";
    let nonce = "ab".repeat(32);
    let record = serde_json::to_vec(&serde_json::json!({"schema_version":1,
        "session":session,"producer":"synthetic-receipt","generation":1,"key":vec![7u8;32]}))
    .unwrap();
    let response = encode_response(
        session,
        &nonce,
        Command::Release,
        State::Exited,
        123,
        Exit::Code(0),
        &[7; 32],
    )
    .unwrap();
    let receipt = serde_json::to_vec(&serde_json::json!({"schema_version":1,
        "session":session,"producer":"synthetic-receipt","generation":1,
        "record_sha256":format!("{:x}",Sha256::digest(&record)),
        "release_nonce":nonce,"signed_release":response}))
    .unwrap();
    let names = (
        directory.join(format!("{session}.json")),
        directory.join(format!("{session}.complete.json")),
    );
    for (path, bytes) in [(&names.0, &record), (&names.1, &receipt)] {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    names
}

#[test]
fn completed_control_receipt_exempts_only_its_own_slot_and_detects_changed_bytes() {
    let root = Root::new();
    root.pristine();
    let (_, receipt) = completed_control_fixture(&root);
    assert!(root.scan(&[]).is_empty());
    fs::write(
        root.path.join("module-backups"),
        b"unresolved unrelated evidence",
    )
    .unwrap();
    let reasons = root.scan(&[]);
    assert_eq!(reasons, vec![Reason::ModuleBackupPresent]);
    assert!(!reasons.contains(&Reason::ManagedVoiceControlPending));
    let mut data = fs::read(&receipt).unwrap();
    let position = data.iter().position(|byte| *byte == b'a').unwrap();
    data[position] = b'b';
    fs::write(receipt, data).unwrap();
    assert!(root.scan(&[]).contains(&Reason::ManagedVoiceControlPending));
}

#[test]
fn completed_control_capture_retains_names_and_descriptors_across_final_validation() {
    for change in ["bytes", "replacement", "extra", "missing"] {
        let root = Root::new();
        root.pristine();
        let (record, _) = completed_control_fixture(&root);
        let reasons = inspect_with_hook(&root.directory(), &[], |phase| {
            if phase == Phase::BeforeFinalCheck {
                match change {
                    "bytes" => fs::write(&record, b"changed bytes").unwrap(),
                    "replacement" => {
                        let old = fs::read(&record).unwrap();
                        fs::rename(&record, root.path.join("preserved-original-record")).unwrap();
                        fs::write(&record, old).unwrap();
                        fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
                    }
                    "extra" => {
                        fs::write(root.path.join("voice-control/unknown"), b"unknown").unwrap()
                    }
                    "missing" => {
                        fs::rename(&record, root.path.join("preserved-original-record")).unwrap()
                    }
                    _ => unreachable!(),
                }
            }
        });
        assert!(
            reasons.contains(&Reason::ManagedVoiceControlPending),
            "{change}"
        );
    }
}

#[test]
fn active_restore_journal_any_shape_quarantines_without_following_or_rewriting() {
    use std::os::unix::fs::MetadataExt;
    let active = crate::call_mode_restore_journal::ACTIVE_NAME;
    for shape in [
        "empty",
        "malformed",
        "oversize",
        "directory",
        "symlink",
        "hardlink",
        "fifo",
    ] {
        let root = Root::new();
        root.pristine();
        let path = root.path.join(active);
        let target = root.path.join("preserved-journal-target");
        match shape {
            "empty" => fs::write(&path, []).unwrap(),
            "malformed" => fs::write(&path, b"not a restore journal").unwrap(),
            "oversize" => {
                let file = File::create(&path).unwrap();
                file.set_len(16 * 1024 * 1024).unwrap();
            }
            "directory" => fs::create_dir(&path).unwrap(),
            "symlink" | "hardlink" => {
                fs::write(&target, b"preserved foreign bytes").unwrap();
                if shape == "symlink" {
                    symlink(&target, &path).unwrap();
                } else {
                    fs::hard_link(&target, &path).unwrap();
                }
            }
            "fifo" => nix::unistd::mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap(),
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&path).unwrap();
        let gate = Gate::inspect_with_lifetime(root.lease.as_ref().unwrap());
        let status = gate.status().unwrap();
        assert!(
            gate.is_quarantined() && gate.is_admission_revoked(),
            "{shape}"
        );
        assert!(
            status.reason_codes.contains(&Reason::RestoreJournalPending),
            "{shape}"
        );
        assert!(
            status
                .reason_codes
                .contains(&Reason::RestoreJournalUnreadable),
            "{shape}"
        );
        assert!(!status.historical_recovery_confirmed);
        assert!(gate.admit_operation("ordinary").is_err());
        let after = fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.mode(),
                before.len(),
                before.nlink()
            ),
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.len(),
                after.nlink()
            ),
            "{shape}"
        );
        if target.exists() {
            assert_eq!(fs::read(target).unwrap(), b"preserved foreign bytes");
        }
    }
}

#[test]
fn active_journal_create_or_remove_during_held_root_scan_never_becomes_clean() {
    for initially_present in [false, true] {
        let root = Root::new();
        root.pristine();
        let active = root
            .path
            .join(crate::call_mode_restore_journal::ACTIVE_NAME);
        if initially_present {
            fs::write(&active, b"unfinished predecessor").unwrap();
        }
        let reasons = inspect_with_hook(&root.directory(), &[], |phase| {
            if phase == Phase::EvidenceCaptured {
                if initially_present {
                    fs::remove_file(&active).unwrap();
                } else {
                    fs::write(&active, b"unfinished predecessor").unwrap();
                }
            }
        });
        assert!(reasons.contains(&Reason::RestoreJournalPending));
        assert!(reasons.contains(&Reason::RestoreJournalUnreadable));
        assert!(reasons.contains(&Reason::UnreadableEvidence));
    }
}

#[test]
fn archived_receipt_namespace_never_clears_independent_legacy_evidence() {
    let root = Root::new();
    root.pristine();
    let archive = root
        .path
        .join(crate::call_mode_restore_journal::ARCHIVE_NAME);
    fs::create_dir(&archive).unwrap();
    fs::write(
        archive.join("untrusted-receipt.json"),
        b"not a recovery authority",
    )
    .unwrap();
    assert!(root.scan(&[]).is_empty());
    fs::write(
        root.path.join(LEGACY_EXECUTION_PENDING),
        b"preserve old evidence",
    )
    .unwrap();
    assert_eq!(root.scan(&[]), vec![Reason::LegacyExecutionPending]);
    assert_eq!(
        fs::read(root.path.join(LEGACY_EXECUTION_PENDING)).unwrap(),
        b"preserve old evidence"
    );
}

#[test]
fn stable_real_root_preserves_pristine_formats_runtime_hashes_and_metadata() {
    let root = Root::new();
    let artifacts = root.runtime();
    fs::write(root.path.join(RUNTIME).join(DOWNLOAD_LOCK), []).unwrap();
    for name in [NAMES[2], NAMES[3]] {
        fs::set_permissions(root.path.join(name), fs::Permissions::from_mode(0o644)).unwrap();
    }
    // The historical empty marker was metadata-only, including mode 0000.
    fs::set_permissions(root.path.join(PRISTINE), fs::Permissions::from_mode(0o000)).unwrap();
    let directory = root.directory();
    let before = (Stamp::of(&directory).unwrap(), snapshot(&directory));
    assert!(root.scan(&artifacts).is_empty());
    assert_eq!(
        (Stamp::of(&directory).unwrap(), snapshot(&directory)),
        before
    );
    assert_eq!(
        fs::read(root.version().join("runtime.bin")).unwrap(),
        PAYLOAD
    );
    // Synthetic files never satisfy production's embedded manifest.
    assert_eq!(
        Gate::inspect_with_lifetime(root.lease.as_ref().unwrap())
            .status()
            .unwrap()
            .reason_codes,
        vec![Reason::LegacyVoiceRuntime]
    );
}

#[test]
fn actual_lifetime_gate_preserves_reason_order_and_never_reads_pending_payload() {
    let root = Root::new();
    fs::write(root.path.join(PRISTINE), b"invalid").unwrap();
    fs::write(root.path.join(NAMES[2]), []).unwrap();
    fs::write(root.path.join(NAMES[3]), []).unwrap();
    fs::create_dir(root.path.join("module-backups")).unwrap();
    fs::create_dir(root.path.join(RUNTIME)).unwrap();
    symlink(
        "unreadable-target",
        root.path.join(LEGACY_EXECUTION_PENDING),
    )
    .unwrap();
    let before = snapshot(&root.directory());
    let gate = Gate::inspect_with_lifetime(root.lease.as_ref().unwrap());
    assert_eq!(
        gate.status().unwrap().reason_codes,
        vec![
            Reason::LegacyExecutionPending,
            Reason::InvalidPristineMarker,
            Reason::LegacyCallModeLock,
            Reason::LegacyAdbInterfaceLock,
            Reason::ModuleBackupPresent,
            Reason::LegacyVoiceRuntime,
        ]
    );
    assert!(!gate.status().unwrap().historical_recovery_confirmed);
    assert_eq!(snapshot(&root.directory()), before);
}

#[test]
fn runtime_rejects_bad_shapes_and_unexpected_names_on_held_directories() {
    for shape in [
        "file-symlink",
        "file-hardlink",
        "file-directory",
        "wrong-mode",
        "wrong-size",
        "wrong-hash",
        "extra",
        "non-ascii",
        "old-version",
        "version-link",
        "runtime-link",
        "download-lock-link",
        "download-lock-bytes",
    ] {
        let root = Root::new();
        let artifacts = root.runtime();
        let file = root.version().join("runtime.bin");
        match shape {
            "file-symlink" | "file-hardlink" => {
                fs::rename(&file, root.path.join("preserved-target")).unwrap();
                if shape == "file-symlink" {
                    symlink(root.path.join("preserved-target"), &file).unwrap();
                } else {
                    fs::hard_link(root.path.join("preserved-target"), &file).unwrap();
                }
            }
            "file-directory" => {
                fs::remove_file(&file).unwrap();
                fs::create_dir(&file).unwrap();
            }
            "wrong-mode" => fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap(),
            "wrong-size" => fs::write(&file, b"short").unwrap(),
            "wrong-hash" => fs::write(&file, vec![b'X'; PAYLOAD.len()]).unwrap(),
            "extra" => fs::write(root.version().join("unknown"), []).unwrap(),
            // APFS rejects invalid UTF-8 at filename creation. Exercise a
            // real non-ASCII entry here; raw byte decoding has its own tests.
            "non-ascii" => fs::write(root.version().join("额外-runtime"), []).unwrap(),
            "old-version" => fs::create_dir(root.path.join(RUNTIME).join("old-version")).unwrap(),
            "version-link" => {
                fs::rename(root.version(), root.path.join("preserved-version")).unwrap();
                symlink(root.path.join("preserved-version"), root.version()).unwrap();
            }
            "runtime-link" => {
                fs::rename(root.path.join(RUNTIME), root.path.join("preserved-runtime")).unwrap();
                symlink(root.path.join("preserved-runtime"), root.path.join(RUNTIME)).unwrap();
            }
            "download-lock-link" => {
                symlink("missing", root.path.join(RUNTIME).join(DOWNLOAD_LOCK)).unwrap()
            }
            "download-lock-bytes" => {
                fs::write(root.path.join(RUNTIME).join(DOWNLOAD_LOCK), b"unfinished").unwrap()
            }
            _ => unreachable!(),
        }
        assert_eq!(
            root.scan(&artifacts),
            vec![Reason::LegacyVoiceRuntime],
            "{shape}"
        );
    }
}

#[test]
fn root_path_aba_never_hashes_a_foreign_tree_even_when_the_name_is_restored() {
    let root = Root::new();
    let artifacts = root.runtime();
    let foreign = Root::new();
    foreign.pristine();
    fs::create_dir_all(foreign.version()).unwrap();
    fs::write(
        foreign.version().join("runtime.bin"),
        b"must never be hashed",
    )
    .unwrap();
    let saved = root.path.with_extension("held");
    let original = root.directory();
    let original_stamp = Stamp::of(&original).unwrap();
    let mut reads = 0;
    let reasons = root
        .lease
        .as_ref()
        .unwrap()
        .root_proof()
        .with_root(&root.path, |held| {
            inspect_with_hook(held, &artifacts, |phase| match phase {
                Phase::RootCaptured => {
                    fs::rename(&root.path, &saved).unwrap();
                    symlink(&foreign.path, &root.path).unwrap();
                }
                Phase::ArtifactRead(_) => reads += 1,
                Phase::BeforeFinalCheck => {
                    fs::remove_file(&root.path).unwrap();
                    fs::rename(&saved, &root.path).unwrap();
                }
                _ => {}
            })
        })
        .unwrap();
    assert_eq!(
        reads,
        artifacts.len(),
        "all actual hashes came from the held original tree"
    );
    assert_eq!(
        fs::read(root.version().join("runtime.bin")).unwrap(),
        PAYLOAD
    );
    assert_eq!(
        fs::read(foreign.version().join("runtime.bin")).unwrap(),
        b"must never be hashed"
    );
    // A visible directory ctime change is evidence, not a false clean result.
    if Stamp::of(&original).unwrap() != original_stamp {
        assert_eq!(reasons, vec![Reason::UnreadableEvidence]);
    } else {
        assert!(reasons.is_empty());
    }
}

#[test]
fn replaced_runtime_or_version_names_never_adopt_foreign_files() {
    for replace_runtime in [false, true] {
        let root = Root::new();
        let artifacts = root.runtime();
        let mut reads = 0;
        let directory = root.directory();
        let reasons = inspect_with_hook(&directory, &artifacts, |phase| {
            if phase == Phase::RuntimePinned {
                let path = if replace_runtime {
                    root.path.join(RUNTIME)
                } else {
                    root.version()
                };
                fs::rename(&path, path.with_extension("retained")).unwrap();
                fs::create_dir(&path).unwrap();
            }
            if matches!(phase, Phase::ArtifactRead(_)) {
                reads += 1;
            }
        });
        if replace_runtime {
            assert_eq!(
                reads,
                artifacts.len(),
                "the held runtime still supplied its original artifact before the root-name check rejected replacement"
            );
        } else {
            assert_eq!(
                reads, 0,
                "the renamed version is an extra entry in the held runtime and must fail its inventory before any artifact read"
            );
        }
        assert!(reasons.contains(&Reason::LegacyVoiceRuntime));
    }
}

#[test]
fn subdirectory_aba_rejects_observable_stamps_without_claiming_an_atomic_snapshot() {
    for replace_runtime in [false, true] {
        let root = Root::new();
        let artifacts = root.runtime();
        let path = if replace_runtime {
            root.path.join(RUNTIME)
        } else {
            root.version()
        };
        let held = File::open(&path).unwrap();
        let before = Stamp::of(&held).unwrap();
        let saved = root.path.join("temporary-held-directory");
        let mut reads = 0;
        let reasons = inspect_with_hook(&root.directory(), &artifacts, |phase| {
            if phase == Phase::NamesCaptured {
                fs::rename(&path, &saved).unwrap();
                fs::create_dir(&path).unwrap();
                fs::remove_dir(&path).unwrap();
                fs::rename(&saved, &path).unwrap();
            }
            if matches!(phase, Phase::ArtifactRead(_)) {
                reads += 1;
            }
        });
        assert_eq!(reads, artifacts.len());
        assert_eq!(
            fs::read(root.version().join("runtime.bin")).unwrap(),
            PAYLOAD
        );
        if Stamp::of(&held).unwrap() != before {
            assert!(reasons.contains(&Reason::LegacyVoiceRuntime));
        } else {
            // Even when this filesystem exposes no directory-stamp delta,
            // data is still read from the original held tree, never the replacement.
            assert!(reasons.is_empty() || reasons == vec![Reason::UnreadableEvidence]);
        }
    }
}

#[test]
fn same_inode_hash_and_metadata_cannot_be_split_across_a_mutation() {
    for phase in [
        Phase::ArtifactPinned(0),
        Phase::ArtifactRead(0),
        Phase::BeforeFinalCheck,
    ] {
        let root = Root::new();
        let artifacts = root.runtime();
        let file = root.version().join("runtime.bin");
        let before = fs::metadata(&file).unwrap();
        let reasons = inspect_with_hook(&root.directory(), &artifacts, |actual| {
            if actual == phase {
                fs::write(&file, vec![b'X'; PAYLOAD.len()]).unwrap();
            }
        });
        let after = fs::metadata(&file).unwrap();
        assert_eq!(
            (before.dev(), before.ino(), before.len()),
            (after.dev(), after.ino(), after.len())
        );
        assert!(reasons.contains(&Reason::LegacyVoiceRuntime), "{phase:?}");
    }
}

#[test]
fn final_namespace_inventory_and_absent_evidence_are_rechecked() {
    for kind in [
        "extra",
        "pending",
        "backup",
        "marker",
        "lock",
        "version-mode",
    ] {
        let root = Root::new();
        let artifacts = root.runtime();
        let reasons = inspect_with_hook(&root.directory(), &artifacts, |phase| {
            if phase != Phase::BeforeFinalCheck {
                return;
            }
            match kind {
                "extra" => fs::write(root.version().join("late-entry"), []).unwrap(),
                "pending" => symlink("missing", root.path.join(LEGACY_EXECUTION_PENDING)).unwrap(),
                "backup" => fs::create_dir(root.path.join("module-backups")).unwrap(),
                "marker" => fs::write(root.path.join(PRISTINE), b"changed").unwrap(),
                "lock" => fs::write(root.path.join(NAMES[2]), b"legacy").unwrap(),
                "version-mode" => {
                    let mode = fs::metadata(root.version()).unwrap().mode() & 0o7777;
                    fs::set_permissions(root.version(), fs::Permissions::from_mode(mode ^ 0o010))
                        .unwrap();
                }
                _ => unreachable!(),
            }
        });
        let expected = match kind {
            "pending" => Reason::LegacyExecutionPending,
            "backup" => Reason::ModuleBackupPresent,
            "marker" => Reason::InvalidPristineMarker,
            "lock" => Reason::LegacyCallModeLock,
            _ => Reason::LegacyVoiceRuntime,
        };
        assert!(reasons.contains(&expected), "{kind}: {reasons:?}");
    }
}

#[test]
fn root_stamp_changes_are_only_startup_observations_not_live_generation_tokens() {
    let root = Root::new();
    root.pristine();
    let gate = Gate::inspect_with_lifetime(root.lease.as_ref().unwrap());
    assert!(!gate.is_quarantined());
    fs::write(
        root.path.join("ordinary-new-store"),
        b"normal later startup",
    )
    .unwrap();
    assert!(!gate.is_admission_revoked());
    let reasons = inspect_with_hook(&root.directory(), &[], |phase| {
        if phase == Phase::BeforeFinalCheck {
            fs::create_dir(root.path.join("during-scan")).unwrap();
        }
    });
    assert_eq!(reasons, vec![Reason::UnreadableEvidence]);
}

#[test]
fn real_requested_root_alias_and_ended_lease_cannot_adopt_another_authority() {
    // The alias must be the first lease for this unique root. A preliminary
    // direct-path lease and immediate reacquire test file-lock timing, not
    // the requested-name and ended-authority properties below.
    let mut root = Root::new_unleased();
    let alias = root.path.with_extension("alias");
    symlink(&root.path, &alias).unwrap();
    root.lease = Some(Lease::acquire(&alias).unwrap());
    root.pristine();
    let gate = Gate::inspect_with_lifetime(root.lease.as_ref().unwrap());
    assert!(!gate.is_quarantined());
    assert!(gate.root_proof_for(&alias).is_ok());
    let foreign = Root::new();
    fs::remove_file(&alias).unwrap();
    symlink(&foreign.path, &alias).unwrap();
    assert!(gate.is_admission_revoked());
    assert_eq!(
        gate.status().unwrap().reason_codes,
        vec![Reason::LifetimeAuthorityChanged]
    );
    fs::remove_file(&alias).unwrap();
    symlink(&root.path, &alias).unwrap();
    assert!(
        gate.is_admission_revoked(),
        "a restored alias cannot undo monotonic revocation"
    );
    let proof = root.lease.as_ref().unwrap().root_proof();
    root.lease.take();
    // This test proves ended authority cannot run work, not immediate OS
    // lock reacquisition. Same-path successor ownership has a dedicated
    // daemon_lifetime test; do not couple this alias check to its timing.
    assert!(matches!(
        proof.with_root(&root.path, |_| panic!("ended authority ran work")),
        Err(crate::daemon_lifetime::Error::AuthorityEnded)
    ));
    assert!(gate.root_proof_for(&root.path).is_err());
    fs::remove_file(alias).unwrap();
}

#[test]
fn actual_lease_drop_during_bounded_scan_fails_the_outer_authority_check() {
    let mut root = Root::new();
    let artifacts = root.runtime();
    let proof = root.lease.as_ref().unwrap().root_proof();
    let result = proof.with_root(&root.path, |held| {
        inspect_with_hook(held, &artifacts, |phase| {
            if phase == Phase::BeforeFinalCheck {
                root.lease.take();
            }
        })
    });
    assert!(result.is_err());
    assert!(proof.with_root(&root.path, |_| ()).is_err());
}

#[test]
fn fifo_shape_child() {
    let Some(path) = std::env::var_os("SWITCH_STARTUP_EVIDENCE_FIFO_ROOT") else {
        return;
    };
    let path = PathBuf::from(path);
    let owned_parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime/e583");
    assert_eq!(path.parent(), Some(owned_parent.as_path()));
    assert!(fs::symlink_metadata(&path).unwrap().is_dir());
    let kind = std::env::var("SWITCH_STARTUP_EVIDENCE_FIFO_KIND").unwrap();
    let directory = File::open(&path).unwrap();
    let target = path
        .join(RUNTIME)
        .join(&manifest().version)
        .join("runtime.bin");
    let mut replaced = false;
    let reasons = inspect_with_hook(&directory, &[artifact()], |phase| {
        if kind == "replace-after-open" && phase == Phase::ArtifactPinned(0) {
            fs::rename(&target, target.with_extension("held")).unwrap();
            nix::unistd::mkfifo(&target, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
            replaced = true;
        }
    });
    assert!(reasons.contains(&Reason::LegacyVoiceRuntime));
    assert_eq!(replaced, kind == "replace-after-open");
    println!("STARTUP_EVIDENCE_FIFO_CHILD_COMPLETED");
}

#[test]
fn fifo_expected_artifact_and_replacement_never_block_or_read_a_pipe() {
    for kind in ["preexisting", "replace-after-open"] {
        let root = Root::new();
        root.runtime();
        if kind == "preexisting" {
            let path = root.version().join("runtime.bin");
            fs::remove_file(&path).unwrap();
            nix::unistd::mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        }
        let test = concat!(module_path!(), "::fifo_shape_child");
        let test = test.split_once("::").unwrap().1;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("SWITCH_STARTUP_EVIDENCE_FIFO_ROOT", &root.path)
            .env("SWITCH_STARTUP_EVIDENCE_FIFO_KIND", kind)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("FIFO evidence child timed out; forced cleanup is not success");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("STARTUP_EVIDENCE_FIFO_CHILD_COMPLETED")
        );
    }
}
