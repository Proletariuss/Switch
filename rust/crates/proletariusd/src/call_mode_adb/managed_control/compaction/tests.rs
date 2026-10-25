use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

struct Root {
    path: PathBuf,
    lease: Option<crate::daemon_lifetime::Lease>,
}
impl Root {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../build/runtime")
            .join(format!("mc589-{}", crate::snapshot::new_instance_id()));
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let lease = crate::daemon_lifetime::Lease::acquire(&path).unwrap();
        Self {
            path,
            lease: Some(lease),
        }
    }
    fn create(&self) -> Arc<ManagedControl> {
        ManagedControl::create(
            &self.path,
            &self.lease.as_ref().unwrap().root_proof(),
            "producer-a",
            1,
        )
        .unwrap()
    }
    fn close(&self) -> Arc<ManagedControl> {
        let control = self.create();
        let nonce = "42".repeat(32);
        let receipt = Completion {
            schema_version: 1,
            session: control.binding.session.clone(),
            producer: "producer-a".into(),
            generation: 1,
            record_sha256: control.binding.record_sha256.clone(),
            release_nonce: nonce.clone(),
            signed_release: switch_voice_supervisor::encode_response(
                &control.binding.session,
                &nonce,
                ControlCommand::Release,
                ControlState::Exited,
                42,
                switch_voice_supervisor::Exit::Code(0),
                &control.key[..],
            )
            .unwrap(),
        };
        control.persist_completion(&receipt).unwrap();
        *control.closed.lock().unwrap() = Some(receipt);
        control
    }
    fn store(&self) -> PathBuf {
        self.path.join(STORE)
    }
    fn inspect(&self) -> io::Result<ControlStoreEvidence> {
        let proof = self.lease.as_ref().unwrap().root_proof();
        proof
            .with_root(&self.path, ControlStoreEvidence::capture)
            .unwrap()
    }
    fn maintain(&self, hook: impl Fn() -> Result<(), ProbeError>) -> Result<(), ProbeError> {
        let _mutation = STORE_MUTATION.lock().unwrap();
        let proof = self.lease.as_ref().unwrap().root_proof();
        proof
            .with_root(&self.path, |root| {
                let directory = File::from(
                    openat(
                        root,
                        STORE,
                        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                        Mode::empty(),
                    )
                    .unwrap(),
                );
                maintain(root, &directory, &|| {
                    proof.with_root(&self.path, |_| ()).map_err(io_error)?;
                    hook()
                })
            })
            .unwrap()
    }
    fn batch(&self) -> Vec<String> {
        (0..BATCH)
            .map(|_| {
                let control = self.close();
                control.binding.session.clone()
            })
            .collect()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        drop(self.lease.take());
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn more_than_128_closed_cycles_remain_bounded_and_startup_verifiable() {
    let root = Root::new();
    for _ in 0..160 {
        let control = root.close();
        control.validate().unwrap();
        drop(control);
        root.inspect().unwrap();
        assert!(fs::read_dir(root.store()).unwrap().count() <= TRIGGER + 2);
    }
    assert!(root.store().join(ARCHIVE).is_file());
    let gate = crate::module_recovery::Gate::inspect_with_lifetime(root.lease.as_ref().unwrap());
    assert!(
        gate.is_quarantined(),
        "archive cannot replace pristine/history lineage"
    );
    assert!(!gate.status().unwrap().historical_recovery_confirmed);
    fs::write(root.path.join(".call-mode-pristine-v1"), b"").unwrap();
    assert!(
        !crate::module_recovery::Gate::inspect_with_lifetime(root.lease.as_ref().unwrap())
            .is_quarantined()
    );
    fs::write(
        root.path
            .join(crate::module_recovery::LEGACY_EXECUTION_PENDING),
        b"unknown history",
    )
    .unwrap();
    let gate = crate::module_recovery::Gate::inspect_with_lifetime(root.lease.as_ref().unwrap());
    assert!(gate.is_quarantined());
    assert!(
        gate.status()
            .unwrap()
            .reason_codes
            .contains(&crate::module_recovery::Reason::LegacyExecutionPending)
    );
}

#[test]
fn durable_completion_is_not_retirable_while_its_real_owner_is_pinned() {
    let root = Root::new();
    let owners: Vec<_> = (0..BATCH).map(|_| root.close()).collect();
    let retry = owners[0].clone();
    let local = retry.local_completion().unwrap().unwrap();
    let retry_payload = retry.payload.to_vec();
    let extra = root.create();
    assert!(!root.store().join(ARCHIVE).exists());
    for owner in &owners {
        owner.validate().unwrap();
    }
    drop(owners);
    let pending = root.create();
    assert!(root.store().join(ARCHIVE).exists());
    retry.validate().unwrap();
    assert_eq!(
        fs::read(root.store().join(format!("{}.json", retry.binding.session))).unwrap(),
        retry_payload
    );
    // A late local durability retry still validates its exact original record.
    let record = &retry.binding;
    let nonce = "57".repeat(32);
    let alternate = Completion {
        schema_version: 1,
        session: record.session.clone(),
        producer: record.producer.clone(),
        generation: record.generation,
        record_sha256: record.record_sha256.clone(),
        release_nonce: nonce.clone(),
        signed_release: switch_voice_supervisor::encode_response(
            &record.session,
            &nonce,
            ControlCommand::Release,
            ControlState::Exited,
            42,
            switch_voice_supervisor::Exit::Code(0),
            &retry.key[..],
        )
        .unwrap(),
    };
    assert!(
        retry.persist_completion(&alternate).is_err(),
        "different receipt is never overwritten"
    );
    retry.validate().unwrap();
    extra.validate().unwrap();
    pending.validate().unwrap();
    let retry_name = format!("{}.json", retry.binding.session);
    drop(retry);
    root.maintain(|| Ok(())).unwrap();
    assert!(
        root.store().join(&retry_name).exists(),
        "LocalCompletion alone retains the actual shared pin"
    );
    local.persist().unwrap();
    root.maintain(|| Ok(())).unwrap();
    // The remaining closed record is below the compaction trigger, so its
    // presence is permitted; the local retry succeeded without any transport.
    extra.validate().unwrap();
    pending.validate().unwrap();
}

#[test]
fn pending_unknown_and_partial_originals_are_preserved_without_early_capacity_failure() {
    let root = Root::new();
    let pending: Vec<_> = (0..BATCH).map(|_| root.create()).collect();
    fs::write(root.store().join("unknown"), b"do not discard").unwrap();
    fs::set_permissions(
        root.store().join("unknown"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let partial = root
        .store()
        .join(format!("{}.complete.json", pending[0].binding.session));
    fs::write(&partial, b"{").unwrap();
    fs::set_permissions(&partial, fs::Permissions::from_mode(0o600)).unwrap();
    let next = root.create();
    next.validate().unwrap();
    assert!(!root.store().join(ARCHIVE).exists());
    assert_eq!(fs::read(&partial).unwrap(), b"{");
    assert_eq!(
        fs::read(root.store().join("unknown")).unwrap(),
        b"do not discard"
    );
    for owner in pending {
        owner.validate().unwrap();
    }
    assert!(root.inspect().is_err());
}

#[test]
fn complete_staging_is_readable_and_retried_without_replacing_originals() {
    let root = Root::new();
    let sessions = root.batch();
    assert!(
        root.maintain(|| if root.store().join(STAGED).exists() {
            Err(failure())
        } else {
            Ok(())
        })
        .is_err()
    );
    assert!(root.store().join(STAGED).exists());
    assert!(!root.store().join(ARCHIVE).exists());
    root.inspect().unwrap();
    for session in &sessions {
        assert!(root.store().join(format!("{session}.json")).exists());
    }
    root.maintain(|| Ok(())).unwrap();
    assert!(!root.store().join(STAGED).exists());
    root.inspect().unwrap();
    for session in sessions {
        assert!(!root.store().join(format!("{session}.json")).exists());
    }
}

#[test]
fn durable_commit_and_interrupted_exact_removal_are_restart_readable() {
    for after_first_removal in [false, true] {
        let root = Root::new();
        let sessions = root.batch();
        assert!(
            root.maintain(|| {
                if root.store().join(ARCHIVE).exists()
                    && (!after_first_removal
                        || sessions
                            .iter()
                            .any(|s| !root.store().join(format!("{s}.json")).exists()))
                {
                    Err(failure())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert!(root.store().join(ARCHIVE).exists());
        assert!(!root.store().join(STAGED).exists());
        root.inspect().unwrap();
        root.maintain(|| Ok(())).unwrap();
        assert_eq!(fs::read_dir(root.store()).unwrap().count(), 1);
        root.inspect().unwrap();
    }
}

#[test]
fn residual_same_bytes_replacement_inode_cannot_be_retired_or_exempted() {
    let root = Root::new();
    let sessions = root.batch();
    assert!(
        root.maintain(|| if root.store().join(ARCHIVE).exists() {
            Err(failure())
        } else {
            Ok(())
        })
        .is_err()
    );
    let original = root.store().join(format!("{}.json", sessions[0]));
    let payload = fs::read(&original).unwrap();
    let preserved = root.path.join("preserved-record");
    fs::rename(&original, &preserved).unwrap();
    fs::write(&original, &payload).unwrap();
    fs::set_permissions(&original, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(root.inspect().is_err());
    assert!(root.maintain(|| Ok(())).is_err());
    assert_eq!(fs::read(original).unwrap(), payload);
    assert_eq!(fs::read(preserved).unwrap(), payload);
}

#[test]
fn archive_copied_into_another_actual_namespace_does_not_certify_it() {
    let original = Root::new();
    original.batch();
    original.maintain(|| Ok(())).unwrap();
    let other = Root::new();
    fs::create_dir(other.store()).unwrap();
    fs::set_permissions(other.store(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::copy(original.store().join(ARCHIVE), other.store().join(ARCHIVE)).unwrap();
    assert!(other.inspect().is_err());
    assert!(other.maintain(|| Ok(())).is_err());
    assert_eq!(fs::read_dir(other.store()).unwrap().count(), 1);
}

#[test]
fn replacing_only_the_store_inode_invalidates_an_unchanged_archive() {
    let root = Root::new();
    root.batch();
    root.maintain(|| Ok(())).unwrap();
    let old_store = root.path.join("preserved-control-store");
    fs::rename(root.store(), &old_store).unwrap();
    fs::create_dir(root.store()).unwrap();
    fs::set_permissions(root.store(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::copy(old_store.join(ARCHIVE), root.store().join(ARCHIVE)).unwrap();
    assert!(root.inspect().is_err());
    assert!(root.maintain(|| Ok(())).is_err());
    assert!(old_store.join(ARCHIVE).exists());
}

#[test]
fn raw_startup_inspection_retains_its_existing_node_and_byte_budgets() {
    let root = Root::new();
    root.batch();
    root.maintain(|| Ok(())).unwrap();
    for name in ["call-mode-operation.lock", ".module-adb-interface.lock"] {
        fs::write(root.path.join(name), b"").unwrap();
        fs::set_permissions(root.path.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let pending = root
        .path
        .join(crate::module_recovery::LEGACY_EXECUTION_PENDING);
    fs::write(&pending, b"unresolved legacy history").unwrap();
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
    let gate = crate::module_recovery::Gate::inspect_with_lifetime(root.lease.as_ref().unwrap());
    let inspection = crate::startup_recovery::Coordinator::inspect(&gate, &root.path).unwrap();
    let log = inspection.startup_logging();
    assert!(log.evidence_nodes <= 256);
    assert!(log.evidence_bytes < 8 * 1024 * 1024);
    assert!(!log.recovery_completed);
    assert!(log.ordinary_quarantine_preserved);
    inspection.verify(&gate).unwrap();
    fs::write(root.store().join("new-unknown"), b"unknown").unwrap();
    assert!(inspection.verify(&gate).is_err());
    assert!(gate.is_quarantined());
}

#[test]
fn short_staging_is_preserved_and_cannot_be_rebuilt_or_assumed_clean() {
    let root = Root::new();
    let sessions = root.batch();
    fs::write(root.store().join(STAGED), b"{").unwrap();
    fs::set_permissions(root.store().join(STAGED), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(root.inspect().is_err());
    assert!(root.maintain(|| Ok(())).is_err());
    assert_eq!(fs::read(root.store().join(STAGED)).unwrap(), b"{");
    for session in sessions {
        assert!(root.store().join(format!("{session}.json")).exists());
    }
}

#[test]
fn final_snapshot_detects_changed_archive_bytes_or_names() {
    let root = Root::new();
    root.batch();
    root.maintain(|| Ok(())).unwrap();
    let snapshot = root.inspect().unwrap();
    let path = root.store().join(ARCHIVE);
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] = b'[';
    fs::write(&path, &bytes).unwrap();
    root.lease
        .as_ref()
        .unwrap()
        .root_proof()
        .with_root(&root.path, |held| {
            assert!(snapshot.validate(held).is_err());
        })
        .unwrap();
    assert!(root.maintain(|| Ok(())).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn envelope_and_ordinary_file_limits_remain_separate() {
    let root = Root::new();
    root.batch();
    root.maintain(|| Ok(())).unwrap();
    let archive = fs::read(root.store().join(ARCHIVE)).unwrap();
    assert!(Envelope::parse(&archive).is_ok());
    assert!(archive.len() <= MAX_ENVELOPE);
    let mut format_fixture = BTreeMap::from([(ARCHIVE.to_owned(), archive.clone())]);
    crate::call_mode_adb::completed_voice_control_store_files(&format_fixture).unwrap();
    for bytes in format_fixture.values_mut() {
        bytes.zeroize();
    }
    let mut shape: serde_json::Value = serde_json::from_slice(&archive).unwrap();
    shape["domain"] = serde_json::json!("unrelated.domain");
    assert!(Envelope::parse(&serde_json::to_vec(&shape).unwrap()).is_err());
    let mut shape: serde_json::Value = serde_json::from_slice(&archive).unwrap();
    let pair = shape["pairs"][0].clone();
    shape["pairs"] = serde_json::json!(vec![pair; BATCH + 1]);
    assert!(Envelope::parse(&serde_json::to_vec(&shape).unwrap()).is_err());
    assert!(Envelope::parse(&vec![b' '; MAX_ENVELOPE + 1]).is_err());
    let ordinary = root.store().join("ordinary-too-large.json");
    fs::write(&ordinary, vec![0; 16_385]).unwrap();
    fs::set_permissions(&ordinary, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(root.inspect().is_err());
}
