//! Import one original managed-control record. This is a pinned key reader,
//! not a reconstructed normal owner and never creates local completion files.
use super::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthenticatedControlEvidence {
    pub(crate) command: String,
    pub(crate) nonce: String,
    pub(crate) signed_response: String,
    pub(crate) sha256: String,
}

pub(in crate::call_mode_adb) struct StartupControl {
    root: PathBuf,
    proof: crate::daemon_lifetime::RootProof,
    directory: File,
    identity: (u64, u64),
    record: File,
    stamp: Stamp,
    payload: Zeroizing<Vec<u8>>,
    parsed: StoredRecord,
    binding: DurableControlBinding,
    inventory: compaction::ControlStoreEvidence,
}
impl StartupControl {
    pub(in crate::call_mode_adb) fn capture(
        root: &Path,
        proof: &crate::daemon_lifetime::RootProof,
        binding: &DurableControlBinding,
    ) -> Result<Self, ProbeError> {
        let _serial = STORE_MUTATION.lock().map_err(io_error)?;
        let (directory, record, stamp, payload, parsed, inventory) = proof
            .with_root(root, |held| -> Result<_, ProbeError> {
                let inventory =
                    compaction::ControlStoreEvidence::capture_with_pending(held, binding)
                        .map_err(io_error)?;
                let dir = File::from(
                    openat(
                        held,
                        STORE,
                        OFlag::O_RDONLY
                            | OFlag::O_DIRECTORY
                            | OFlag::O_NOFOLLOW
                            | OFlag::O_CLOEXEC
                            | OFlag::O_NONBLOCK,
                        Mode::empty(),
                    )
                    .map_err(io_error)?,
                );
                directory(&dir)?;
                let name = format!("{}.json", binding.session);
                let record = File::from(
                    openat(
                        &dir,
                        name.as_str(),
                        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                        Mode::empty(),
                    )
                    .map_err(io_error)?,
                );
                record.try_lock().map_err(io_error)?;
                let m = record.metadata().map_err(io_error)?;
                if !m.is_file()
                    || m.nlink() != 1
                    || m.uid() != nix::unistd::geteuid().as_raw()
                    || m.mode() & 0o7777 != 0o600
                {
                    return Err(failure());
                }
                let stamp = Stamp::of(&m);
                let payload = read_payload(&record, stamp, MAX_RECORD)?;
                let parsed: StoredRecord = serde_json::from_slice(&payload).map_err(io_error)?;
                if parsed.schema_version != 1
                    || parsed.session != binding.session
                    || parsed.producer != binding.producer
                    || parsed.generation != binding.generation
                    || hex(&Sha256::digest(&payload[..])) != binding.record_sha256
                {
                    return Err(failure());
                }
                Ok((dir, record, stamp, payload, parsed, inventory))
            })
            .map_err(io_error)??;
        let m = directory.metadata().map_err(io_error)?;
        let value = Self {
            root: root.into(),
            proof: proof.clone(),
            identity: (m.dev(), m.ino()),
            directory,
            record,
            stamp,
            payload,
            parsed,
            binding: binding.clone(),
            inventory,
        };
        value.validate()?;
        Ok(value)
    }
    pub(in crate::call_mode_adb) fn validate(&self) -> Result<(), ProbeError> {
        self.proof
            .with_root(&self.root, |held| -> Result<(), ProbeError> {
                self.inventory.validate(held).map_err(io_error)?;
                directory(&self.directory)?;
                let dm = self.directory.metadata().map_err(io_error)?;
                let named = fstatat(held, STORE, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(io_error)?;
                let record = fstatat(
                    &self.directory,
                    format!("{}.json", self.binding.session).as_str(),
                    AtFlags::AT_SYMLINK_NOFOLLOW,
                )
                .map_err(io_error)?;
                if (dm.dev(), dm.ino()) != self.identity
                    || (named.st_dev as u64, named.st_ino as u64) != self.identity
                    || (record.st_dev as u64, record.st_ino as u64)
                        != (self.stamp.dev, self.stamp.ino)
                    || read_payload(&self.record, self.stamp, MAX_RECORD)?.as_slice()
                        != self.payload.as_slice()
                {
                    return Err(failure());
                }
                Ok(())
            })
            .map_err(io_error)?
    }
    pub(in crate::call_mode_adb) fn record_sha256(&self) -> &str {
        &self.binding.record_sha256
    }
    pub(in crate::call_mode_adb) fn pid_path(&self) -> String {
        format!(
            "{MODULE_VOICE_REMOTE_DIRECTORY}/control-{}/child.pid",
            self.binding.session
        )
    }
    pub(in crate::call_mode_adb) fn request(
        &self,
        adb: &mut impl ModuleVoiceADB,
        command: ControlCommand,
    ) -> Result<
        (
            switch_voice_supervisor::Response,
            AuthenticatedControlEvidence,
        ),
        ProbeError,
    > {
        self.validate()?;
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(io_error)?;
        let nonce = hex(&nonce);
        let word = match command {
            ControlCommand::Status => "status",
            ControlCommand::Stop => "stop",
            ControlCommand::Release => "release",
        };
        let remote = format!(
            "{MODULE_VOICE_REMOTE_DIRECTORY}/control-{}",
            self.binding.session
        );
        let cmd = format!(
            "'{SUPERVISOR}' client --socket '{remote}/socket' --key-file '{remote}/key' --session '{}' --nonce '{nonce}' --command {word}",
            self.binding.session
        );
        let (output, status) = adb.shell_checked_timeout(&cmd, Duration::from_secs(15))?;
        if status != 0 || output.len() > MAX_OUTPUT_BYTES {
            return Err(failure());
        }
        let signed = output.strip_suffix(b"\n").ok_or_else(failure)?;
        let response = switch_voice_supervisor::verify_response(
            signed,
            &self.binding.session,
            &nonce,
            command,
            &self.parsed.key[..],
        )
        .map_err(io_error)?;
        self.validate()?;
        Ok((
            response,
            AuthenticatedControlEvidence {
                command: word.into(),
                nonce,
                signed_response: String::from_utf8(signed.to_vec()).map_err(io_error)?,
                sha256: hex(&Sha256::digest(signed)),
            },
        ))
    }
    pub(in crate::call_mode_adb) fn checked_release(self) -> Result<(), ProbeError> {
        self.validate()?;
        self.record.unlock().map_err(io_error)
    }
}
impl Drop for StartupControl {
    fn drop(&mut self) {
        let _ = self.record.unlock();
    }
}
fn read_payload(file: &File, stamp: Stamp, max: usize) -> Result<Zeroizing<Vec<u8>>, ProbeError> {
    if stamp.len == 0 || stamp.len > max as u64 {
        return Err(failure());
    }
    let mut bytes = Zeroizing::new(vec![0; stamp.len as usize + 1]);
    let mut count = 0;
    while count < bytes.len() {
        match file.read_at(&mut bytes[count..], count as u64) {
            Ok(0) => break,
            Ok(n) => count += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_error(e)),
        }
    }
    if count != stamp.len as usize || Stamp::of(&file.metadata().map_err(io_error)?) != stamp {
        return Err(failure());
    }
    bytes.truncate(count);
    Ok(bytes)
}
pub(in crate::call_mode_adb) fn require_closed_inventory(
    root: &Path,
    proof: &crate::daemon_lifetime::RootProof,
) -> Result<(), ProbeError> {
    proof
        .with_root(root, |held| -> Result<(), ProbeError> {
            match fstatat(held, STORE, AtFlags::AT_SYMLINK_NOFOLLOW) {
                Err(nix::errno::Errno::ENOENT) => Ok(()),
                Err(e) => Err(io_error(e)),
                Ok(_) => {
                    let evidence =
                        compaction::ControlStoreEvidence::capture(held).map_err(io_error)?;
                    evidence.validate(held).map_err(io_error)
                }
            }
        })
        .map_err(io_error)?
}

pub(in crate::call_mode_adb) fn sessions_for_settlement(
    root: &Path,
    proof: &crate::daemon_lifetime::RootProof,
    pending: Option<&DurableControlBinding>,
) -> Result<Vec<String>, ProbeError> {
    proof
        .with_root(root, |held| -> Result<Vec<String>, ProbeError> {
            match fstatat(held, STORE, AtFlags::AT_SYMLINK_NOFOLLOW) {
                Err(nix::errno::Errno::ENOENT) if pending.is_none() => Ok(Vec::new()),
                Err(e) => Err(io_error(e)),
                Ok(_) => {
                    let evidence = match pending {
                        Some(binding) => {
                            compaction::ControlStoreEvidence::capture_with_pending(held, binding)
                        }
                        None => compaction::ControlStoreEvidence::capture(held),
                    }
                    .map_err(io_error)?;
                    let sessions = evidence.sessions().map_err(io_error)?;
                    evidence.validate(held).map_err(io_error)?;
                    Ok(sessions)
                }
            }
        })
        .map_err(io_error)?
}

pub(in crate::call_mode_adb) fn verify_archived_transcript(
    directory: &File,
    binding: &DurableControlBinding,
    pid: u32,
    evidence: &[AuthenticatedControlEvidence],
) -> Result<(), ProbeError> {
    super::directory(directory)?;
    if !(3..=4).contains(&evidence.len()) {
        return Err(failure());
    }
    let name = format!("{}.json", binding.session);
    let before =
        fstatat(directory, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW).map_err(io_error)?;
    let file = File::from(
        openat(
            directory,
            name.as_str(),
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .map_err(io_error)?,
    );
    let m = file.metadata().map_err(io_error)?;
    if !m.is_file()
        || m.mode() & 0o7777 != 0o600
        || m.uid() != nix::unistd::geteuid().as_raw()
        || m.nlink() != 1
        || (before.st_dev as u64, before.st_ino as u64) != (m.dev(), m.ino())
    {
        return Err(failure());
    }
    let stamp = Stamp::of(&m);
    let payload = read_payload(&file, stamp, MAX_RECORD)?;
    let record: StoredRecord = serde_json::from_slice(&payload).map_err(io_error)?;
    if record.schema_version != 1
        || record.session != binding.session
        || record.producer != binding.producer
        || record.generation != binding.generation
        || hex(&Sha256::digest(&payload[..])) != binding.record_sha256
    {
        return Err(failure());
    }
    let mut nonces = std::collections::BTreeSet::new();
    let mut last_exit = None;
    for (index, item) in evidence.iter().enumerate() {
        let command = match (index, item.command.as_str()) {
            (0, "status") => ControlCommand::Status,
            (1, "stop") => ControlCommand::Stop,
            (n, "release") if n == evidence.len() - 1 => ControlCommand::Release,
            (2, "status") if evidence.len() == 4 => ControlCommand::Status,
            _ => return Err(failure()),
        };
        if !lower_hex(&item.nonce, 32)
            || !nonces.insert(item.nonce.as_str())
            || item.signed_response.len() > 512
            || item.sha256 != hex(&Sha256::digest(item.signed_response.as_bytes()))
        {
            return Err(failure());
        }
        let response = switch_voice_supervisor::verify_response(
            item.signed_response.as_bytes(),
            &binding.session,
            &item.nonce,
            command,
            &record.key[..],
        )
        .map_err(io_error)?;
        if response.pid() != pid {
            return Err(failure());
        }
        if index == evidence.len() - 1 {
            if response.state() != ControlState::Exited
                || matches!(response.exit(), switch_voice_supervisor::Exit::None)
                || last_exit != Some(response.exit())
            {
                return Err(failure());
            }
        } else if index > 0 && response.state() == ControlState::Exited {
            last_exit = Some(response.exit());
        }
    }
    let after =
        fstatat(directory, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW).map_err(io_error)?;
    if (after.st_dev as u64, after.st_ino as u64) != (stamp.dev, stamp.ino)
        || read_payload(&file, stamp, MAX_RECORD)?.as_slice() != payload.as_slice()
    {
        return Err(failure());
    }
    Ok(())
}

pub(crate) fn verify_normal_origin_inventory(
    root: &File,
    origins: &[(String, String)],
) -> std::io::Result<()> {
    match fstatat(root, STORE, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Err(nix::errno::Errno::ENOENT) if origins.is_empty() => Ok(()),
        Err(e) => Err(e.into()),
        Ok(_) => {
            let evidence = compaction::ControlStoreEvidence::capture(root)?;
            evidence.verify_normal_origins(origins)?;
            evidence.validate(root)
        }
    }
}

pub(crate) fn verify_normal_origin_registration(
    root: &File,
    origins: &[(String, String)],
    new_session: &str,
    new_sha: &str,
) -> std::io::Result<()> {
    let invalid = || std::io::Error::other("normal voice origin registration is invalid");
    if !lower_hex(new_session, 16)
        || !lower_hex(new_sha, 32)
        || origins.iter().any(|(s, _)| s == new_session)
    {
        return Err(invalid());
    }
    let dir = File::from(openat(
        root,
        STORE,
        OFlag::O_RDONLY
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    directory(&dir).map_err(|_| invalid())?;
    let name = format!("{new_session}.json");
    let file = File::from(openat(
        &dir,
        name.as_str(),
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    let m = file.metadata()?;
    if !m.is_file()
        || m.mode() & 0o7777 != 0o600
        || m.uid() != nix::unistd::geteuid().as_raw()
        || m.nlink() != 1
    {
        return Err(invalid());
    }
    let bytes = read_payload(&file, Stamp::of(&m), MAX_RECORD).map_err(|_| invalid())?;
    let record: StoredRecord = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if record.schema_version != 1
        || record.session != new_session
        || !valid_producer(&record.producer)
        || record.generation == 0
        || hex(&Sha256::digest(&bytes[..])) != new_sha
    {
        return Err(invalid());
    }
    let binding = DurableControlBinding {
        schema_version: 1,
        session: record.session,
        record_sha256: new_sha.into(),
        producer: record.producer,
        generation: record.generation,
    };
    let evidence = compaction::ControlStoreEvidence::capture_with_pending(root, &binding)?;
    let mut all = origins.to_vec();
    all.push((new_session.into(), new_sha.into()));
    evidence.verify_origin_records(&all)?;
    evidence.validate(root)
}

/// Only an internal already-admitted normal writer may use this reader. It
/// does not discharge a pending session for startup or ordinary Gate coverage.
pub(crate) fn verify_normal_origin_writer_inventory(
    root: &File,
    origins: &[(String, String)],
) -> std::io::Result<()> {
    if verify_normal_origin_inventory(root, origins).is_ok() {
        return Ok(());
    }
    let invalid = || std::io::Error::other("normal voice writer inventory is invalid");
    let dir = File::from(openat(
        root,
        STORE,
        OFlag::O_RDONLY
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    directory(&dir).map_err(|_| invalid())?;
    let names = proletarius_platform_fs::directory_names(&dir, 256)?
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let mut pending = Vec::new();
    for name in &names {
        let Some(text) = name.to_str() else {
            return Err(invalid());
        };
        let Some(session) = text.strip_suffix(".json").filter(|s| lower_hex(s, 16)) else {
            continue;
        };
        if !names.contains(std::ffi::OsStr::new(&format!("{session}.complete.json"))) {
            pending.push(session.to_owned());
        }
    }
    if pending.len() != 1 {
        return Err(invalid());
    }
    let session = &pending[0];
    let hash = origins
        .iter()
        .find(|(s, _)| s == session)
        .map(|(_, h)| h)
        .ok_or_else(invalid)?;
    let file = File::from(openat(
        &dir,
        format!("{session}.json").as_str(),
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    let m = file.metadata()?;
    let bytes = read_payload(&file, Stamp::of(&m), MAX_RECORD).map_err(|_| invalid())?;
    let record: StoredRecord = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if record.schema_version != 1
        || record.session != *session
        || !valid_producer(&record.producer)
        || record.generation == 0
        || hex(&Sha256::digest(&bytes[..])) != *hash
    {
        return Err(invalid());
    }
    let binding = DurableControlBinding {
        schema_version: 1,
        session: record.session,
        record_sha256: hash.clone(),
        producer: record.producer,
        generation: record.generation,
    };
    let evidence = compaction::ControlStoreEvidence::capture_with_pending(root, &binding)?;
    evidence.verify_origin_records(origins)?;
    evidence.validate(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use switch_voice_supervisor::Exit;
    static PARENT_INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct Root {
        path: PathBuf,
        lease: Option<crate::daemon_lifetime::Lease>,
    }
    impl Root {
        fn new() -> Self {
            Self::new_in(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../../build/runtime/iteration-592-voice-metadata"),
            )
        }
        fn new_in(parent: PathBuf) -> Self {
            // A concurrent supervisor test temporarily uses umask 0177. A
            // newly created directory can therefore have mode 0600 until we
            // explicitly restore its search bit, before creating a child.
            {
                let _init = PARENT_INIT.lock().unwrap();
                std::fs::create_dir_all(&parent).unwrap();
                std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let path = parent.join(crate::snapshot::new_instance_id());
            std::fs::create_dir_all(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            let lease = Some(crate::daemon_lifetime::Lease::acquire(&path).unwrap());
            Self { path, lease }
        }
        fn proof(&self) -> crate::daemon_lifetime::RootProof {
            self.lease.as_ref().unwrap().root_proof()
        }
        fn control(&self, generation: u64) -> Arc<ManagedControl> {
            ManagedControl::create(&self.path, &self.proof(), "producer-a", generation).unwrap()
        }
        fn root_file(&self) -> File {
            File::open(&self.path).unwrap()
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            drop(self.lease.take());
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
    #[test]
    fn fixture_parent_is_searchable_after_restrictive_creation_mask() {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../build/runtime")
            .join(format!(
                "mc592-parent-{}",
                crate::snapshot::new_instance_id()
            ));
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let parent = parent.clone();
                scope.spawn(move || {
                    let root = Root::new_in(parent.clone());
                    assert_eq!(
                        std::fs::metadata(&root.path).unwrap().mode() & 0o7777,
                        0o700
                    );
                    root.control(1).validate().unwrap();
                });
            }
        });
        assert_eq!(std::fs::metadata(&parent).unwrap().mode() & 0o7777, 0o700);
        std::fs::remove_dir(&parent).unwrap();
    }
    fn transcript(
        control: &ManagedControl,
        command: ControlCommand,
        index: u8,
        state: ControlState,
    ) -> AuthenticatedControlEvidence {
        let nonce = format!("{index:02x}").repeat(32);
        let signed_response = switch_voice_supervisor::encode_response(
            &control.binding.session,
            &nonce,
            command,
            state,
            42,
            if state == ControlState::Exited {
                Exit::Code(0)
            } else {
                Exit::None
            },
            &control.key[..],
        )
        .unwrap();
        AuthenticatedControlEvidence {
            command: match command {
                ControlCommand::Status => "status",
                ControlCommand::Stop => "stop",
                ControlCommand::Release => "release",
            }
            .into(),
            nonce,
            sha256: hex(&Sha256::digest(signed_response.as_bytes())),
            signed_response,
        }
    }
    fn close_metadata(control: &ManagedControl) {
        let nonce = "91".repeat(32);
        let signed_release = switch_voice_supervisor::encode_response(
            &control.binding.session,
            &nonce,
            ControlCommand::Release,
            ControlState::Exited,
            42,
            Exit::Code(0),
            &control.key[..],
        )
        .unwrap();
        control
            .persist_completion(&Completion {
                schema_version: 1,
                session: control.binding.session.clone(),
                producer: control.binding.producer.clone(),
                generation: control.binding.generation,
                record_sha256: control.binding.record_sha256.clone(),
                release_nonce: nonce,
                signed_release,
            })
            .unwrap();
    }
    #[test]
    fn startup_import_requires_original_owner_record_pin_retired_and_preserves_raw() {
        let root = Root::new();
        let control = root.control(1);
        let binding = control.binding();
        let before = control.payload.to_vec();
        assert!(StartupControl::capture(&root.path, &root.proof(), &binding).is_err());
        drop(control);
        let imported = StartupControl::capture(&root.path, &root.proof(), &binding).unwrap();
        imported.validate().unwrap();
        imported.checked_release().unwrap();
        assert_eq!(
            std::fs::read(
                root.path
                    .join(STORE)
                    .join(format!("{}.json", binding.session))
            )
            .unwrap(),
            before
        );
        let second = root.control(2);
        drop(second);
        assert!(StartupControl::capture(&root.path, &root.proof(), &binding).is_err());
    }
    #[test]
    fn archived_transcript_verifies_actual_record_hmac_order_pid_and_exit() {
        let root = Root::new();
        let control = root.control(1);
        let mut responses = vec![
            transcript(&control, ControlCommand::Status, 1, ControlState::Running),
            transcript(&control, ControlCommand::Stop, 2, ControlState::Exited),
            transcript(&control, ControlCommand::Release, 3, ControlState::Exited),
        ];
        assert!(
            verify_archived_transcript(&control.directory, &control.binding, 42, &responses)
                .is_ok()
        );
        assert!(
            verify_archived_transcript(&control.directory, &control.binding, 43, &responses)
                .is_err()
        );
        responses.swap(0, 1);
        assert!(
            verify_archived_transcript(&control.directory, &control.binding, 42, &responses)
                .is_err()
        );
        responses.swap(0, 1);
        responses[2].nonce = "ff".repeat(32);
        assert!(
            verify_archived_transcript(&control.directory, &control.binding, 42, &responses)
                .is_err()
        );
    }
    #[test]
    fn registered_pending_is_writer_only_and_unknown_or_second_pending_is_rejected() {
        let root = Root::new();
        let control = root.control(1);
        let id = control.binding.session.clone();
        let hash = control.binding.record_sha256.clone();
        let origins = vec![(id.clone(), hash.clone())];
        let held = root.root_file();
        assert!(verify_normal_origin_registration(&held, &[], &id, &hash).is_ok());
        assert!(verify_normal_origin_writer_inventory(&held, &origins).is_ok());
        assert!(verify_normal_origin_inventory(&held, &origins).is_err());
        assert!(verify_normal_origin_writer_inventory(&held, &[]).is_err());
        close_metadata(&control);
        assert!(verify_normal_origin_inventory(&held, &origins).is_ok());
        let other = root.control(2);
        assert!(verify_normal_origin_writer_inventory(&held, &origins).is_err());
        drop(other);
    }
    #[test]
    fn normal_closed_compaction_accepts_registered_present_records_but_rejects_unknown_archive_pair()
     {
        let root = Root::new();
        let mut origins = Vec::new();
        for generation in 1..=36 {
            let control = root.control(generation);
            origins.push((
                control.binding.session.clone(),
                control.binding.record_sha256.clone(),
            ));
            close_metadata(&control);
            drop(control);
        }
        let held = root.root_file();
        assert!(root.path.join(STORE).join(".closed-pairs-v1.json").exists());
        assert!(verify_normal_origin_inventory(&held, &origins).is_ok());
        let current = File::open(root.path.join(STORE)).unwrap();
        let evidence = compaction::ControlStoreEvidence::capture(&held).unwrap();
        let visible = evidence.sessions().unwrap();
        let missing = visible.last().unwrap();
        let filtered = origins
            .iter()
            .filter(|(id, _)| id != missing)
            .cloned()
            .collect::<Vec<_>>();
        assert!(verify_normal_origin_inventory(&held, &filtered).is_err());
        drop(current);
    }
}

#[cfg(test)]
pub(in crate::call_mode_adb) mod owned_fixture;
