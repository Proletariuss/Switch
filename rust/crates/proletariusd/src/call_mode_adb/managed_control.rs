//! Original-owner authenticated control. Stored bytes are recovery evidence,
//! never a replacement for the retained physical plan or operation authority.
use super::*;
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{Mode, fstatat, mkdirat};
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::io::Write;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use switch_voice_supervisor::{Command as ControlCommand, State as ControlState};
use zeroize::Zeroizing;

mod compaction;
#[cfg(test)]
mod composition_tests;
pub(super) mod startup;
#[cfg(test)]
mod tests;
pub(crate) use compaction::ControlStoreEvidence;

const STORE: &str = "voice-control";
const SUPERVISOR: &str = "/tmp/proletarius-call/switch-voice-supervisor.armv7";
const MAX_RECORD: usize = 4096;
// Serializes local namespace changes, not physical operation authority. The
// daemon's real root lease separately excludes older daemon versions.
static STORE_MUTATION: Mutex<()> = Mutex::new(());

pub(super) fn failure() -> ProbeError {
    ProbeError::new(
        "module_voice_control_unconfirmed",
        "原模块语音控制会话未确认，保留原清理义务",
    )
}
fn io_error(_: impl std::fmt::Display) -> ProbeError {
    failure()
}
fn lower_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(value.len() * 2);
    for byte in value {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "WireBinding")]
pub(crate) struct DurableControlBinding {
    schema_version: u8,
    session: String,
    record_sha256: String,
    producer: String,
    generation: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireBinding {
    schema_version: u8,
    session: String,
    record_sha256: String,
    producer: String,
    generation: u64,
}
impl TryFrom<WireBinding> for DurableControlBinding {
    type Error = &'static str;
    fn try_from(wire: WireBinding) -> Result<Self, Self::Error> {
        if wire.schema_version != 1
            || !lower_hex(&wire.session, 16)
            || !lower_hex(&wire.record_sha256, 32)
            || !valid_producer(&wire.producer)
            || wire.generation == 0
        {
            return Err("invalid managed voice control binding");
        }
        Ok(Self {
            schema_version: 1,
            session: wire.session,
            record_sha256: wire.record_sha256,
            producer: wire.producer,
            generation: wire.generation,
        })
    }
}
impl std::fmt::Debug for DurableControlBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableControlBinding")
            .finish_non_exhaustive()
    }
}
impl DurableControlBinding {
    pub(super) fn startup_record_sha256(&self) -> &str {
        &self.record_sha256
    }
    pub(super) fn matches_producer(&self, producer: &str) -> bool {
        self.producer == producer
    }
    pub(super) fn matches(&self, producer: &str, generation: u64) -> bool {
        self.producer == producer && self.generation == generation
    }
}

#[derive(Serialize)]
struct Record<'a> {
    schema_version: u8,
    session: &'a str,
    producer: &'a str,
    generation: u64,
    key: &'a [u8; 32],
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    links: u64,
    len: u64,
    mt: (i64, i64),
    ct: (i64, i64),
}
impl Stamp {
    fn of(m: &Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            mode: m.mode(),
            uid: m.uid(),
            links: m.nlink(),
            len: m.len(),
            mt: (m.mtime(), m.mtime_nsec()),
            ct: (m.ctime(), m.ctime_nsec()),
        }
    }
}
pub(super) struct ManagedControl {
    root: PathBuf,
    proof: crate::daemon_lifetime::RootProof,
    directory: File,
    directory_identity: (u64, u64),
    record: File,
    stamp: Stamp,
    payload: Zeroizing<Vec<u8>>,
    key: Zeroizing<[u8; 32]>,
    binding: DurableControlBinding,
    submitted: AtomicBool,
    released: AtomicBool,
    stopped: AtomicBool,
    poisoned: AtomicBool,
    closed: Mutex<Option<Completion>>,
    #[cfg(test)]
    completion_fail_before_sync: AtomicBool,
}
impl std::fmt::Debug for ManagedControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedControl").finish_non_exhaustive()
    }
}
fn directory(file: &File) -> Result<(), ProbeError> {
    let m = file.metadata().map_err(io_error)?;
    if !m.is_dir() || m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o7777 != 0o700 {
        return Err(failure());
    }
    Ok(())
}
impl ManagedControl {
    pub(super) fn create(
        root: &Path,
        proof: &crate::daemon_lifetime::RootProof,
        producer: &str,
        generation: u64,
    ) -> Result<Arc<Self>, ProbeError> {
        let _mutation = STORE_MUTATION.lock().map_err(io_error)?;
        let origin = crate::startup_recovery::reconcile::ledger::capture_normal_origin(root, proof)
            .map_err(io_error)?;
        if !valid_producer(producer) || generation == 0 {
            return Err(failure());
        }
        let mut session_bytes = [0; 16];
        getrandom::fill(&mut session_bytes).map_err(io_error)?;
        let session = hex(&session_bytes);
        let mut key = Zeroizing::new([0; 32]);
        getrandom::fill(key.as_mut()).map_err(io_error)?;
        let payload = Zeroizing::new(
            serde_json::to_vec(&Record {
                schema_version: 1,
                session: &session,
                producer,
                generation,
                key: &key,
            })
            .map_err(io_error)?,
        );
        if payload.len() > MAX_RECORD {
            return Err(failure());
        }
        let binding = DurableControlBinding {
            schema_version: 1,
            session,
            record_sha256: hex(&Sha256::digest(&payload[..])),
            producer: producer.into(),
            generation,
        };
        let (dir, record, stamp) = proof
            .with_root(root, |held| -> Result<_, ProbeError> {
                let (original_directory, created) =
                    match fstatat(held, STORE, AtFlags::AT_SYMLINK_NOFOLLOW) {
                        Err(nix::errno::Errno::ENOENT) => {
                            mkdirat(held, STORE, Mode::S_IRWXU).map_err(io_error)?;
                            held.sync_all().map_err(io_error)?;
                            (
                                fstatat(held, STORE, AtFlags::AT_SYMLINK_NOFOLLOW)
                                    .map_err(io_error)?,
                                true,
                            )
                        }
                        Err(error) => return Err(io_error(error)),
                        Ok(m) => {
                            if m.st_mode & nix::libc::S_IFMT != nix::libc::S_IFDIR
                                || m.st_uid != nix::unistd::geteuid().as_raw()
                                || m.st_mode as u32 & 0o7777 != 0o700
                            {
                                return Err(failure());
                            }
                            (m, false)
                        }
                    };
                let dir = File::from(
                    openat(
                        held,
                        STORE,
                        OFlag::O_RDONLY
                            | OFlag::O_DIRECTORY
                            | OFlag::O_NOFOLLOW
                            | OFlag::O_NONBLOCK
                            | OFlag::O_CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(io_error)?,
                );
                let dm = dir.metadata().map_err(io_error)?;
                let named = fstatat(held, STORE, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(io_error)?;
                if !dm.is_dir()
                    || dm.uid() != nix::unistd::geteuid().as_raw()
                    || original_directory.st_mode & nix::libc::S_IFMT != nix::libc::S_IFDIR
                    || original_directory.st_uid != nix::unistd::geteuid().as_raw()
                    || (dm.dev(), dm.ino()) != (named.st_dev as u64, named.st_ino as u64)
                    || (dm.dev(), dm.ino())
                        != (
                            original_directory.st_dev as u64,
                            original_directory.st_ino as u64,
                        )
                {
                    return Err(failure());
                }
                if created {
                    // umask is process-wide, so another thread may have
                    // reduced this new directory to 0600. Restore only the
                    // inode just created and verified through its held fd.
                    if dm.mode() & 0o7777 & !0o700 != 0
                        || named.st_mode as u32 & 0o7777 & !0o700 != 0
                    {
                        return Err(failure());
                    }
                    dir.set_permissions(std::fs::Permissions::from_mode(0o700))
                        .map_err(io_error)?;
                    dir.sync_all().map_err(io_error)?;
                }
                directory(&dir)?;
                let named = fstatat(held, STORE, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(io_error)?;
                if (dm.dev(), dm.ino()) != (named.st_dev as u64, named.st_ino as u64)
                    || named.st_mode as u32 & 0o7777 != 0o700
                {
                    return Err(failure());
                }
                compaction::maintain(held, &dir, &|| {
                    proof.with_root(root, |_| ()).map_err(io_error)
                })?;
                let names =
                    proletarius_platform_fs::directory_names(&dir, 256).map_err(io_error)?;
                if names.len() > 254 {
                    return Err(failure());
                }
                let name = format!("{}.json", binding.session);
                let mut record = File::from(
                    openat(
                        &dir,
                        name.as_str(),
                        OFlag::O_RDWR
                            | OFlag::O_CREAT
                            | OFlag::O_EXCL
                            | OFlag::O_NOFOLLOW
                            | OFlag::O_NONBLOCK
                            | OFlag::O_CLOEXEC,
                        Mode::S_IRUSR | Mode::S_IWUSR,
                    )
                    .map_err(io_error)?,
                );
                record.write_all(&payload).map_err(io_error)?;
                record.sync_all().map_err(io_error)?;
                // The actual owner pins this exact inode until its last Arc
                // drops, including a retained local-only completion retry.
                record.try_lock_shared().map_err(io_error)?;
                dir.sync_all().map_err(io_error)?;
                held.sync_all().map_err(io_error)?;
                let stamp = Stamp::of(&record.metadata().map_err(io_error)?);
                Ok((dir, record, stamp))
            })
            .map_err(io_error)??;
        let dm = dir.metadata().map_err(io_error)?;
        let result = Arc::new(Self {
            root: root.into(),
            proof: proof.clone(),
            directory_identity: (dm.dev(), dm.ino()),
            directory: dir,
            record,
            stamp,
            payload,
            key,
            binding,
            submitted: AtomicBool::new(false),
            released: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            poisoned: AtomicBool::new(false),
            closed: Mutex::new(None),
            #[cfg(test)]
            completion_fail_before_sync: AtomicBool::new(false),
        });
        result.validate()?;
        if let Some(origin) = origin {
            origin
                .register_voice(&result.binding.session, &result.binding.record_sha256)
                .map_err(io_error)?;
        }
        Ok(result)
    }
    pub(super) fn binding(&self) -> DurableControlBinding {
        self.binding.clone()
    }
    pub(super) fn acquire_lock(
        &self,
    ) -> Result<crate::call_mode_backups::OperationFileLock, ProbeError> {
        self.validate()?;
        crate::call_mode_backups::acquire_bound_recovery_lock(
            &self.root,
            &self.proof,
            crate::call_mode_backups::RecoveryLockKind::AdbInterface,
            false,
        )
        .map_err(io_error)
    }
    pub(super) fn local_completion(
        self: &Arc<Self>,
    ) -> Result<Option<LocalCompletion>, ProbeError> {
        let closed = self.closed.lock().map_err(io_error)?;
        Ok(closed.as_ref().map(|receipt| LocalCompletion {
            control: self.clone(),
            receipt: receipt.clone(),
        }))
    }
    pub(super) fn has_submitted(&self) -> bool {
        self.submitted.load(Ordering::Acquire)
    }
    pub(super) fn pid_path(&self) -> String {
        format!("{}/child.pid", self.remote())
    }
    pub(super) fn validate(&self) -> Result<(), ProbeError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(failure());
        }
        let result = self
            .proof
            .with_root(&self.root, |held| -> Result<(), ProbeError> {
                directory(&self.directory)?;
                let dm = self.directory.metadata().map_err(io_error)?;
                let named = fstatat(held, STORE, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(io_error)?;
                if (dm.dev(), dm.ino()) != self.directory_identity
                    || (named.st_dev as u64, named.st_ino as u64) != self.directory_identity
                {
                    return Err(failure());
                }
                let m = self.record.metadata().map_err(io_error)?;
                if !m.is_file()
                    || m.mode() & 0o7777 != 0o600
                    || m.uid() != nix::unistd::geteuid().as_raw()
                    || m.nlink() != 1
                    || Stamp::of(&m) != self.stamp
                {
                    return Err(failure());
                }
                let name = format!("{}.json", self.binding.session);
                let named = fstatat(&self.directory, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
                    .map_err(io_error)?;
                if (named.st_dev as u64, named.st_ino as u64) != (m.dev(), m.ino())
                    || named.st_mode as u32 != m.mode()
                    || named.st_nlink != 1
                {
                    return Err(failure());
                }
                let mut read = Zeroizing::new(vec![0; self.payload.len() + 1]);
                let mut off = 0;
                while off < read.len() {
                    match self.record.read_at(&mut read[off..], off as u64) {
                        Ok(0) => break,
                        Ok(n) => off += n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(io_error(e)),
                    }
                }
                if off != self.payload.len()
                    || read[..off] != self.payload[..]
                    || Stamp::of(&self.record.metadata().map_err(io_error)?) != self.stamp
                {
                    return Err(failure());
                }
                let after = fstatat(&self.directory, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
                    .map_err(io_error)?;
                if (after.st_dev as u64, after.st_ino as u64) != (self.stamp.dev, self.stamp.ino) {
                    return Err(failure());
                }
                Ok(())
            })
            .map_err(io_error)
            .and_then(|result| result);
        if result.is_err() {
            self.poisoned.store(true, Ordering::Release);
        }
        result
    }
    fn remote(&self) -> String {
        format!(
            "{MODULE_VOICE_REMOTE_DIRECTORY}/control-{}",
            self.binding.session
        )
    }
    pub(super) fn submit(
        &self,
        plan: &AdbPlan,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<(), ProbeError> {
        self.validate()?;
        current()?;
        if self.submitted.swap(true, Ordering::AcqRel) {
            return Err(failure());
        }
        plan.with_usb_checked(|usb|{
            let mut adb=AdbProbe::with_deadline(usb,Instant::now()+MODULE_VOICE_ROUTE_START_TIMEOUT);adb.connect()?;
            current()?;
            let (_,legacy)=adb.shell_checked_timeout("test ! -e /run/proletarius-voice-route.pid && test ! -L /run/proletarius-voice-route.pid",Duration::from_secs(8))?;
            if legacy!=0 {return Err(failure());}
            let remote=self.remote();
            let (_,status)=adb.shell_checked_timeout(&format!("umask 077; mkdir '{remote}'"),Duration::from_secs(8))?;
            if status!=0{return Err(failure());}
            current()?;self.validate()?;
            let key_text=Zeroizing::new(hex(&self.key[..]));
            adb.push(key_text.as_bytes(),&format!("{remote}/key"),0o100600)?;
            current()?;self.validate()?;
            let launch=format!("nohup '{SUPERVISOR}' serve --socket '{remote}/socket' --key-file '{remote}/key' --session '{}' --helper '{MODULE_VOICE_HELPER}' --child-pid-file '{remote}/child.pid' </dev/null >> '/run/proletarius-voice-route.log' 2>&1 &",self.binding.session);
            let (_,status)=adb.shell_checked_timeout(&launch,Duration::from_secs(8))?;
            if status!=0{return Err(failure());}current()?;self.validate()
        })
    }
    pub(super) fn request(
        &self,
        adb: &mut impl ModuleVoiceADB,
        command: ControlCommand,
    ) -> Result<switch_voice_supervisor::Response, ProbeError> {
        self.request_observed(adb, command).map(|v| v.0)
    }
    fn request_observed(
        &self,
        adb: &mut impl ModuleVoiceADB,
        command: ControlCommand,
    ) -> Result<(switch_voice_supervisor::Response, String, String), ProbeError> {
        self.validate()?;
        if self.released.load(Ordering::Acquire) {
            return Err(failure());
        }
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(io_error)?;
        let nonce = hex(&nonce);
        let remote = self.remote();
        let word = match command {
            ControlCommand::Status => "status",
            ControlCommand::Stop => "stop",
            ControlCommand::Release => "release",
        };
        let cmd = format!(
            "'{SUPERVISOR}' client --socket '{remote}/socket' --key-file '{remote}/key' --session '{}' --nonce '{nonce}' --command {word}",
            self.binding.session
        );
        let (output, status) = adb.shell_checked_timeout(&cmd, Duration::from_secs(15))?;
        if status != 0 || output.len() > MAX_OUTPUT_BYTES {
            return Err(failure());
        }
        // shell_checked_timeout retains exactly the wrapper's separator LF.
        // Remove that LF only; the protocol verifier still requires its own LF.
        let signed = output.strip_suffix(b"\n").ok_or_else(failure)?;
        let result = switch_voice_supervisor::verify_response(
            signed,
            &self.binding.session,
            &nonce,
            command,
            &self.key[..],
        )
        .map_err(|error| {
            self.poisoned.store(true, Ordering::Release);
            io_error(error)
        })?;
        self.validate()?;
        Ok((
            result,
            nonce,
            String::from_utf8(signed.to_vec()).map_err(io_error)?,
        ))
    }
    pub(super) fn release(
        &self,
        plan: &AdbPlan,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<(), ProbeError> {
        current()?;
        self.validate()?;
        let mut closed = self.closed.lock().map_err(io_error)?;
        if closed.is_none() {
            if !self.stopped.load(Ordering::Acquire) {
                return Err(failure());
            }
            let mut session = ModuleVoiceADBSession::checked(
                Instant::now() + MODULE_VOICE_ROUTE_STATUS_TIMEOUT,
                plan.clone(),
            );
            let mut adb = FencedModuleVoiceADB {
                adb: &mut session,
                current,
            };
            let (response, nonce, signed_release) =
                self.request_observed(&mut adb, ControlCommand::Release)?;
            if response.state() != ControlState::Exited {
                return Err(failure());
            }
            // checked session has already released the interface and dropped the actual handle.
            *closed = Some(Completion {
                schema_version: 1,
                session: self.binding.session.clone(),
                producer: self.binding.producer.clone(),
                generation: self.binding.generation,
                record_sha256: self.binding.record_sha256.clone(),
                release_nonce: nonce,
                signed_release,
            });
            self.released.store(true, Ordering::Release);
        }
        self.persist_completion(closed.as_ref().ok_or_else(failure)?)
    }
    fn persist_completion(&self, completion: &Completion) -> Result<(), ProbeError> {
        let _mutation = STORE_MUTATION.lock().map_err(io_error)?;
        self.validate()?;
        let payload = serde_json::to_vec(completion).map_err(io_error)?;
        self.proof
            .with_root(&self.root, |held| -> Result<(), ProbeError> {
                let name = format!("{}.complete.json", self.binding.session);
                let file = match openat(
                    &self.directory,
                    name.as_str(),
                    OFlag::O_RDWR
                        | OFlag::O_CREAT
                        | OFlag::O_EXCL
                        | OFlag::O_NOFOLLOW
                        | OFlag::O_NONBLOCK
                        | OFlag::O_CLOEXEC,
                    Mode::S_IRUSR | Mode::S_IWUSR,
                ) {
                    Ok(fd) => {
                        let mut file = File::from(fd);
                        file.write_all(&payload).map_err(io_error)?;
                        file
                    }
                    Err(nix::errno::Errno::EEXIST) => File::from(
                        openat(
                            &self.directory,
                            name.as_str(),
                            OFlag::O_RDONLY
                                | OFlag::O_NOFOLLOW
                                | OFlag::O_NONBLOCK
                                | OFlag::O_CLOEXEC,
                            Mode::empty(),
                        )
                        .map_err(io_error)?,
                    ),
                    Err(error) => return Err(io_error(error)),
                };
                let before = file.metadata().map_err(io_error)?;
                if !before.is_file()
                    || before.uid() != nix::unistd::geteuid().as_raw()
                    || before.mode() & 0o7777 != 0o600
                    || before.nlink() != 1
                    || before.len() != payload.len() as u64
                {
                    return Err(failure());
                }
                #[cfg(test)]
                if self
                    .completion_fail_before_sync
                    .swap(false, Ordering::AcqRel)
                {
                    return Err(failure());
                }
                file.sync_all().map_err(io_error)?;
                let mut actual = vec![0; payload.len() + 1];
                let mut count = 0;
                while count < actual.len() {
                    let n = file
                        .read_at(&mut actual[count..], count as u64)
                        .map_err(io_error)?;
                    if n == 0 {
                        break;
                    }
                    count += n;
                }
                if count != payload.len()
                    || actual[..count] != payload
                    || Stamp::of(&file.metadata().map_err(io_error)?) != Stamp::of(&before)
                {
                    return Err(failure());
                }
                let named = fstatat(&self.directory, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
                    .map_err(io_error)?;
                if (named.st_dev as u64, named.st_ino as u64) != (before.dev(), before.ino()) {
                    return Err(failure());
                }
                self.directory.sync_all().map_err(io_error)?;
                held.sync_all().map_err(io_error)?;
                let final_stamp = Stamp::of(&file.metadata().map_err(io_error)?);
                let final_named =
                    fstatat(&self.directory, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
                        .map_err(io_error)?;
                if final_stamp != Stamp::of(&before)
                    || (final_named.st_dev as u64, final_named.st_ino as u64)
                        != (before.dev(), before.ino())
                {
                    return Err(failure());
                }
                let mut final_bytes = vec![0; payload.len() + 1];
                let mut final_count = 0;
                while final_count < final_bytes.len() {
                    let n = file
                        .read_at(&mut final_bytes[final_count..], final_count as u64)
                        .map_err(io_error)?;
                    if n == 0 {
                        break;
                    }
                    final_count += n;
                }
                if final_count != payload.len()
                    || final_bytes[..final_count] != payload
                    || Stamp::of(&file.metadata().map_err(io_error)?) != final_stamp
                {
                    return Err(failure());
                }
                let final_named =
                    fstatat(&self.directory, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
                        .map_err(io_error)?;
                if (final_named.st_dev as u64, final_named.st_ino as u64)
                    != (before.dev(), before.ino())
                {
                    return Err(failure());
                }
                self.validate()
            })
            .map_err(io_error)??;
        Ok(())
    }
}

impl Drop for ManagedControl {
    fn drop(&mut self) {
        if self.record.unlock().is_err() {
            crate::diagnostics::write_line(format_args!("voice control record pin release failed"));
        }
    }
}

// This token carries only an already observed RELEASE plus checked handle-close
// fact. It can retry local durability; it cannot submit device commands.
pub(super) struct LocalCompletion {
    control: Arc<ManagedControl>,
    receipt: Completion,
}
impl LocalCompletion {
    pub(super) fn persist(self) -> Result<(), ProbeError> {
        self.control.persist_completion(&self.receipt)
    }
}

pub(super) fn route(
    adb: &mut impl ModuleVoiceADB,
    control: &ManagedControl,
    action: VoiceRouteAction,
    current: &mut dyn FnMut() -> Result<(), ProbeError>,
) -> Result<bool, ProbeError> {
    current()?;
    match action {
        VoiceRouteAction::Start => {
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                current()?;
                let status = match control.request(adb, ControlCommand::Status) {
                    Ok(status) => status,
                    Err(error) => {
                        if control.poisoned.load(Ordering::Acquire) {
                            return Err(error);
                        }
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                };
                if status.state() == ControlState::Exited {
                    return Err(failure());
                }
                if status.state() == ControlState::Running && dsp_ready(adb)? {
                    return Ok(true);
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(failure())
        }
        VoiceRouteAction::Status => {
            let status = control.request(adb, ControlCommand::Status)?;
            match status.state() {
                ControlState::Running => {
                    if dsp_ready(adb)? {
                        Ok(true)
                    } else {
                        Err(failure())
                    }
                }
                ControlState::Exited => {
                    if dsp_stopped(adb)? {
                        Ok(false)
                    } else {
                        Err(failure())
                    }
                }
                _ => Err(failure()),
            }
        }
        VoiceRouteAction::Stop => {
            if control.closed.lock().map_err(io_error)?.is_some() {
                return Ok(false);
            }
            let mut response = control.request(adb, ControlCommand::Stop)?;
            let deadline = Instant::now() + MODULE_VOICE_ROUTE_STOP_TIMEOUT;
            while response.state() != ControlState::Exited {
                if Instant::now() >= deadline {
                    return Err(failure());
                }
                current()?;
                std::thread::sleep(Duration::from_millis(100));
                response = control.request(adb, ControlCommand::Status)?;
            }
            cleanup_module_voice_audio(adb, 5, Duration::from_millis(200), &mut || current())?;
            current()?;
            if !dsp_stopped(adb)? {
                return Err(failure());
            }
            control.stopped.store(true, Ordering::Release);
            Ok(false)
        }
    }
}
fn dsp_ready(adb: &mut impl ModuleVoiceADB) -> Result<bool, ProbeError> {
    let (_,status)=adb.shell_checked_timeout("test \"$(cat /sys/class/android_usb/f_audio/audio_enable)\" = 1 && grep -q '^state: RUNNING' /proc/asound/card0/pcm4p/sub0/status && grep -q '^state: RUNNING' /proc/asound/card0/pcm4c/sub0/status",Duration::from_secs(8))?;
    Ok(status == 0)
}
fn dsp_stopped(adb: &mut impl ModuleVoiceADB) -> Result<bool, ProbeError> {
    // Procfs reports exactly "closed" once the owned child has closed each PCM.
    // An empty, unreadable, truncated or merely nonmatching state is unknown.
    let (_,status)=adb.shell_checked_timeout("a=$(head -c 3 /sys/class/android_usb/f_audio/audio_enable) || exit 2; test \"$a\" = 0 || exit 1; for p in /proc/asound/card0/pcm4p/sub0/status /proc/asound/card0/pcm4c/sub0/status; do s=$(head -c 4097 \"$p\") || exit 2; test \"$s\" = closed || exit 3; done",Duration::from_secs(8))?;
    Ok(status == 0)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    schema_version: u8,
    session: String,
    producer: String,
    generation: u64,
    record_sha256: String,
    release_nonce: String,
    signed_release: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRecord {
    schema_version: u8,
    session: String,
    producer: String,
    generation: u64,
    #[serde(deserialize_with = "read_secret_key")]
    key: Zeroizing<[u8; 32]>,
}
fn read_secret_key<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Zeroizing<[u8; 32]>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Zeroizing<[u8; 32]>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("32 key bytes")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut key = Zeroizing::new([0; 32]);
            for byte in key.iter_mut() {
                *byte = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::custom("invalid key length"))?;
            }
            if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(serde::de::Error::custom("invalid key length"));
            }
            Ok(key)
        }
    }
    decoder.deserialize_seq(Visitor)
}
pub(super) fn valid_pid_path(path: &str) -> bool {
    path.strip_prefix("/tmp/proletarius-call/control-")
        .and_then(|v| v.strip_suffix("/child.pid"))
        .is_some_and(|v| lower_hex(v, 16))
}
#[cfg(test)]
pub(super) fn completed_store_files(
    files: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    compaction::completed_payloads(files)
}

#[cfg(test)]
fn completed_ordinary_store_files(
    files: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    let invalid = || "managed voice control completion unavailable".to_owned();
    if files.is_empty()
        || files.len() > 256
        || files.len() % 2 != 0
        || files.values().map(Vec::len).sum::<usize>() > 2 * 1024 * 1024
    {
        return Err(invalid());
    }
    let mut seen = std::collections::BTreeSet::new();
    for (name, bytes) in files {
        if name.ends_with(".complete.json") {
            continue;
        }
        let session = name
            .strip_suffix(".json")
            .filter(|s| lower_hex(s, 16))
            .ok_or_else(invalid)?;
        let cname = format!("{session}.complete.json");
        let complete = files.get(&cname).ok_or_else(invalid)?;
        verify_closed_pair(session, bytes, complete)?;
        seen.insert(name.clone());
        seen.insert(cname);
    }
    if seen.len() != files.len() {
        return Err(invalid());
    }
    Ok(())
}

fn verify_closed_pair(session: &str, bytes: &[u8], complete: &[u8]) -> Result<(), String> {
    let invalid = || "managed voice control completion unavailable".to_owned();
    if bytes.len() > MAX_RECORD {
        return Err(invalid());
    }
    let record: StoredRecord = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if record.schema_version != 1
        || record.session != session
        || !valid_producer(&record.producer)
        || record.generation == 0
    {
        return Err(invalid());
    }
    if complete.len() > 16384 {
        return Err(invalid());
    }
    let receipt: Completion = serde_json::from_slice(complete).map_err(|_| invalid())?;
    if receipt.schema_version != 1
        || receipt.session != session
        || receipt.producer != record.producer
        || receipt.generation != record.generation
        || receipt.record_sha256 != hex(&Sha256::digest(bytes))
        || !lower_hex(&receipt.release_nonce, 32)
    {
        return Err(invalid());
    }
    let response = switch_voice_supervisor::verify_response(
        receipt.signed_release.as_bytes(),
        session,
        &receipt.release_nonce,
        ControlCommand::Release,
        &record.key[..],
    )
    .map_err(|_| invalid())?;
    if response.state() != ControlState::Exited {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn completed_files_fixture_for_tests() -> std::collections::BTreeMap<String, Vec<u8>> {
    let session = "12".repeat(16);
    let nonce = "34".repeat(32);
    let key = Zeroizing::new([0x56; 32]);
    let record = serde_json::to_vec(&Record {
        schema_version: 1,
        session: &session,
        producer: "producer-a",
        generation: 1,
        key: &key,
    })
    .unwrap();
    let receipt = Completion {
        schema_version: 1,
        session: session.clone(),
        producer: "producer-a".into(),
        generation: 1,
        record_sha256: hex(&Sha256::digest(&record)),
        release_nonce: nonce.clone(),
        signed_release: switch_voice_supervisor::encode_response(
            &session,
            &nonce,
            ControlCommand::Release,
            ControlState::Exited,
            42,
            switch_voice_supervisor::Exit::Code(0),
            &key[..],
        )
        .unwrap(),
    };
    [
        (format!("{session}.json"), record),
        (
            format!("{session}.complete.json"),
            serde_json::to_vec(&receipt).unwrap(),
        ),
    ]
    .into_iter()
    .collect()
}
