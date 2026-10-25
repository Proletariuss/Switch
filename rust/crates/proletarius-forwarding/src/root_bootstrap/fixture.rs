//! Explicit synthetic foundation; local leases are not real daemon integration.
//! Never repairs/adopts an existing root, even after a fully visible witness.
use super::{BootstrapError, FailureKind as K, Stage, err};
use crate::root_preflight::{self, FreshWitness, RootIdentity, SourceIdentity};
use nix::fcntl::{Flock, FlockArg};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, Metadata},
    io::Write,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerKind {
    Producer,
    Configuration,
    SourceWriter,
    QueueIo,
    Provider,
}
impl OwnerKind {
    fn index(self) -> usize {
        self as usize
    }
}
struct OwnerState {
    accepting: bool,
    issued: bool,
    counts: [usize; 5],
}
/// A synthetic owner set. It is not registered with the real service runtime.
pub struct TrackedOwners {
    root: PathBuf,
    state: Arc<Mutex<OwnerState>>,
}
pub struct OwnerLease {
    kind: OwnerKind,
    state: Arc<Mutex<OwnerState>>,
}
/// Opaque, one-shot, path-bound local drain observation. No deserialize/clone.
pub struct DrainedOwners {
    root: PathBuf,
    _state: Arc<Mutex<OwnerState>>,
}
impl TrackedOwners {
    pub fn new(root: &Path) -> Result<Self, BootstrapError> {
        validate_target(root)?;
        Ok(Self {
            root: root.into(),
            state: Arc::new(Mutex::new(OwnerState {
                accepting: true,
                issued: false,
                counts: [0; 5],
            })),
        })
    }
    pub fn acquire(&self, kind: OwnerKind) -> Result<OwnerLease, BootstrapError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| err(Stage::OwnerGate, K::OwnerStateUnproven))?;
        if !s.accepting {
            return Err(err(Stage::OwnerGate, K::OwnerGateClosed));
        }
        s.counts[kind.index()] = s.counts[kind.index()]
            .checked_add(1)
            .ok_or_else(|| err(Stage::OwnerGate, K::OwnerStateUnproven))?;
        Ok(OwnerLease {
            kind,
            state: self.state.clone(),
        })
    }
    /// Permanent close: dropping a token/future must never reopen new work.
    pub fn begin_drain(&self) -> Result<(), BootstrapError> {
        self.state
            .lock()
            .map_err(|_| err(Stage::OwnerGate, K::OwnerStateUnproven))?
            .accepting = false;
        Ok(())
    }
    pub fn try_drained(&self) -> Result<DrainedOwners, BootstrapError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| err(Stage::OwnerGate, K::OwnerStateUnproven))?;
        if s.accepting || s.counts.iter().any(|n| *n != 0) {
            return Err(err(Stage::OwnerGate, K::OwnersNotDrained));
        }
        if s.issued {
            return Err(err(Stage::OwnerGate, K::OwnerGateClosed));
        }
        s.issued = true;
        Ok(DrainedOwners {
            root: self.root.clone(),
            _state: self.state.clone(),
        })
    }
}
impl Drop for OwnerLease {
    fn drop(&mut self) {
        if let Ok(mut s) = self.state.lock() {
            // Poison, arithmetic failure or an untracked real owner cannot issue
            // a new token. No cancellation API decrements another owner's lease.
            if let Some(n) = s.counts[self.kind.index()].checked_sub(1) {
                s.counts[self.kind.index()] = n;
            } else {
                s.accepting = false;
                s.issued = true;
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Moment {
    Before,
    After,
}
#[derive(Debug, Serialize)]
pub struct LocalCreationReport {
    pub root_id: String,
    pub source_id: String,
    pub root_identity_sha256: String,
    pub source_identity_sha256: String,
    pub witness_sha256: String,
    pub local_sync_sequence_completed: bool,
    pub local_tracked_owners_drained: bool,
    pub production_authorized: bool,
    pub daemon_owner_integration_proven: bool,
    pub historical_fresh_origin_proven: bool,
    pub reopen_can_reissue_receipt: bool,
}
/// Holds the parent/name guard and permanently closed local gate. This is only
/// a local syscall receipt; it is not a queue/progress/migration authority.
pub struct LocalCreationReceipt {
    report: LocalCreationReport,
    _guard: NamespaceGuard,
    _drained: DrainedOwners,
}
impl LocalCreationReceipt {
    pub fn report(&self) -> &LocalCreationReport {
        &self.report
    }
}
struct Parent {
    path: PathBuf,
    file: File,
    metadata: Metadata,
}
struct NamespaceGuard {
    _parent: Parent,
    _lock: Flock<File>,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn same_inode(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
fn private(m: &Metadata, directory: bool) -> bool {
    m.uid() == nix::unistd::geteuid().as_raw()
        && m.mode() & 0o077 == 0
        && if directory {
            m.is_dir()
        } else {
            m.is_file() && m.nlink() == 1
        }
}
fn validate_target(root: &Path) -> Result<(), BootstrapError> {
    let parent = root
        .parent()
        .ok_or_else(|| err(Stage::ParentOpen, K::InvalidPath))?;
    let name = root
        .file_name()
        .ok_or_else(|| err(Stage::ParentOpen, K::InvalidPath))?;
    if !root.is_absolute()
        || name.is_empty()
        || name.as_bytes().contains(&0)
        || root
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        || fs::canonicalize(parent).map_err(|_| err(Stage::ParentOpen, K::InvalidPath))? != parent
    {
        return Err(err(Stage::ParentOpen, K::InvalidPath));
    }
    let m = fs::symlink_metadata(parent).map_err(|_| err(Stage::ParentOpen, K::Io))?;
    if !private(&m, true) {
        return Err(err(Stage::ParentOpen, K::UnsafePermissions));
    }
    Ok(())
}
fn open_at(
    parent: &File,
    name: &std::ffi::OsStr,
    flags: i32,
    stage: Stage,
) -> Result<File, BootstrapError> {
    let flags = nix::fcntl::OFlag::from_bits_truncate(flags)
        | nix::fcntl::OFlag::O_CLOEXEC
        | nix::fcntl::OFlag::O_NOFOLLOW;
    let fd = nix::fcntl::openat(
        parent,
        name,
        flags,
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .map_err(|e| {
        err(
            stage,
            if e == nix::errno::Errno::EEXIST {
                K::Conflict
            } else {
                K::Io
            },
        )
    })?;
    Ok(File::from(fd))
}
impl Parent {
    fn open(root: &Path) -> Result<Self, BootstrapError> {
        validate_target(root)?;
        let path = root.parent().unwrap().to_owned();
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&path)
            .map_err(|_| err(Stage::ParentOpen, K::Io))?;
        let metadata = file.metadata().map_err(|_| err(Stage::ParentOpen, K::Io))?;
        let p = Self {
            path,
            file,
            metadata,
        };
        p.validate(Stage::ParentOpen)?;
        Ok(p)
    }
    fn validate(&self, stage: Stage) -> Result<(), BootstrapError> {
        let m =
            fs::symlink_metadata(&self.path).map_err(|_| err(stage, K::ChangedDuringCreation))?;
        if fs::canonicalize(&self.path).map_err(|_| err(stage, K::ChangedDuringCreation))?
            != self.path
            || !same_inode(&m, &self.metadata)
            || !private(&m, true)
        {
            return Err(err(stage, K::ChangedDuringCreation));
        }
        Ok(())
    }
}
fn sync_file(file: &File, stage: Stage) -> Result<(), BootstrapError> {
    file.sync_all().map_err(|_| err(stage, K::Io))?;
    // On macOS also request the drive flush. Unsupported/error is a rejection,
    // never a weaker success fallback. Directory publication uses fsync below.
    #[cfg(target_os = "macos")]
    nix::fcntl::fcntl(file, nix::fcntl::FcntlArg::F_FULLFSYNC).map_err(|_| err(stage, K::Io))?;
    Ok(())
}
fn step<T>(
    hook: &mut impl FnMut(Stage, Moment) -> Result<(), BootstrapError>,
    stage: Stage,
    f: impl FnOnce() -> Result<T, BootstrapError>,
) -> Result<T, BootstrapError> {
    hook(stage, Moment::Before)?;
    let result = f()?;
    hook(stage, Moment::After)?;
    Ok(result)
}
fn marker(
    root: &File,
    name: &str,
    bytes: &[u8],
    stages: [Stage; 4],
    hook: &mut impl FnMut(Stage, Moment) -> Result<(), BootstrapError>,
) -> Result<(), BootstrapError> {
    let [create, write, file_sync, dir_sync] = stages;
    let mut file = step(hook, create, || {
        let f = open_at(
            root,
            name.as_ref(),
            nix::libc::O_WRONLY | nix::libc::O_CREAT | nix::libc::O_EXCL,
            create,
        )?;
        if !private(&f.metadata().map_err(|_| err(create, K::Io))?, false) {
            return Err(err(create, K::UnsafePermissions));
        }
        Ok(f)
    })?;
    step(hook, write, || {
        file.write_all(bytes).map_err(|_| err(write, K::Io))
    })?;
    step(hook, file_sync, || sync_file(&file, file_sync))?;
    step(hook, dir_sync, || {
        root.sync_all().map_err(|_| err(dir_sync, K::Io))
    })?;
    Ok(())
}
/// Uses exclusive directory creation rather than replacement. Any crash leaves
/// a conflict for all later attempts, including matching complete marker bytes.
/// Hooks exist only in explicit fixture builds; no environment bypass exists.
pub fn create_fresh(
    drained: DrainedOwners,
    mut hook: impl FnMut(Stage, Moment) -> Result<(), BootstrapError>,
) -> Result<LocalCreationReceipt, BootstrapError> {
    let root_path = &drained.root;
    let parent = step(&mut hook, Stage::ParentOpen, || Parent::open(root_path))?;
    let leaf = root_path.file_name().unwrap();
    let guard_name = format!(".switch-bootstrap-{}.lock", hash(leaf.as_bytes()));
    let lock_file = step(&mut hook, Stage::GuardOpen, || {
        let path = parent.path.join(&guard_name);
        let prior = match fs::symlink_metadata(&path) {
            Ok(m) if private(&m, false) => Some(m),
            Ok(_) => return Err(err(Stage::GuardOpen, K::UnsafePermissions)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(err(Stage::GuardOpen, K::Io)),
        };
        // O_NONBLOCK also bounds an unexpected FIFO replacement between the
        // precheck and open. Creation never follows a concurrent existing name.
        let flags = nix::libc::O_RDWR
            | nix::libc::O_NONBLOCK
            | if prior.is_none() {
                nix::libc::O_CREAT | nix::libc::O_EXCL
            } else {
                0
            };
        let file = open_at(&parent.file, guard_name.as_ref(), flags, Stage::GuardOpen)?;
        let named = fs::symlink_metadata(&path).map_err(|_| err(Stage::GuardOpen, K::Io))?;
        let opened = file.metadata().map_err(|_| err(Stage::GuardOpen, K::Io))?;
        if !same_inode(&named, &opened)
            || !private(&named, false)
            || !private(&opened, false)
            || prior.as_ref().is_some_and(|m| !same_inode(m, &opened))
        {
            return Err(err(Stage::GuardOpen, K::ChangedDuringCreation));
        }
        Ok(file)
    })?;
    let lock = step(&mut hook, Stage::GuardLock, || {
        Flock::lock(lock_file, FlockArg::LockExclusiveNonblock).map_err(|(_, e)| {
            err(
                Stage::GuardLock,
                if e == nix::errno::Errno::EWOULDBLOCK || e == nix::errno::Errno::EAGAIN {
                    K::Busy
                } else {
                    K::Io
                },
            )
        })
    })?;
    step(&mut hook, Stage::GuardSync, || {
        sync_file(&lock, Stage::GuardSync)
    })?;
    step(&mut hook, Stage::GuardDirectorySync, || {
        parent
            .file
            .sync_all()
            .map_err(|_| err(Stage::GuardDirectorySync, K::Io))
    })?;
    // Check the locked name again. A guard inode replacement cannot be adopted.
    let named = fs::symlink_metadata(parent.path.join(&guard_name))
        .map_err(|_| err(Stage::GuardLock, K::Io))?;
    if !same_inode(
        &named,
        &lock.metadata().map_err(|_| err(Stage::GuardLock, K::Io))?,
    ) || !private(&named, false)
    {
        return Err(err(Stage::GuardLock, K::ChangedDuringCreation));
    }
    parent.validate(Stage::GuardLock)?;
    let (root_id, source_id, nonce) = step(&mut hook, Stage::Random, || {
        let mut bytes = [0u8; 48];
        getrandom::fill(&mut bytes).map_err(|_| err(Stage::Random, K::RandomUnavailable))?;
        let hex = |b: &[u8]| b.iter().map(|v| format!("{v:02x}")).collect::<String>();
        Ok((hex(&bytes[..16]), hex(&bytes[16..32]), hex(&bytes[32..])))
    })?;
    let root = step(&mut hook, Stage::Mkdir, || {
        parent.validate(Stage::Mkdir)?;
        nix::sys::stat::mkdirat(
            &parent.file,
            leaf,
            nix::sys::stat::Mode::from_bits_truncate(0o700),
        )
        .map_err(|e| {
            err(
                Stage::Mkdir,
                if e == nix::errno::Errno::EEXIST {
                    K::Conflict
                } else {
                    K::Io
                },
            )
        })?;
        // Retain the new directory FD before exposing the next fault boundary.
        // This anchors later marker writes even if the name is then replaced.
        let file = open_at(
            &parent.file,
            leaf,
            nix::libc::O_RDONLY | nix::libc::O_DIRECTORY,
            Stage::RootOpen,
        )?;
        if !private(
            &file.metadata().map_err(|_| err(Stage::RootOpen, K::Io))?,
            true,
        ) {
            return Err(err(Stage::RootOpen, K::UnsafePermissions));
        }
        Ok(file)
    })?;
    let root_inode = root.metadata().map_err(|_| err(Stage::RootOpen, K::Io))?;
    step(&mut hook, Stage::RootOpen, || {
        let named = fs::symlink_metadata(root_path)
            .map_err(|_| err(Stage::RootOpen, K::ChangedDuringCreation))?;
        if !same_inode(&named, &root_inode) || !private(&named, true) {
            return Err(err(Stage::RootOpen, K::ChangedDuringCreation));
        }
        Ok(())
    })?;
    step(&mut hook, Stage::RootParentSync, || {
        root.sync_all()
            .map_err(|_| err(Stage::RootParentSync, K::Io))?;
        parent
            .file
            .sync_all()
            .map_err(|_| err(Stage::RootParentSync, K::Io))
    })?;
    let source = SourceIdentity {
        schema_version: 1,
        root_id: root_id.clone(),
        source_id: source_id.clone(),
        creation_nonce: nonce.clone(),
        store_relative_path: "multistandby/store-v1.json".into(),
        contract: "nwait-source-identity-v1".into(),
    };
    let source_bytes = serde_json::to_vec(&source).map_err(|_| err(Stage::SourceWrite, K::Io))?;
    let root_identity = RootIdentity {
        schema_version: 1,
        root_id: root_id.clone(),
        creation_nonce: nonce.clone(),
        source_identity_sha256: hash(&source_bytes),
    };
    let root_bytes =
        serde_json::to_vec(&root_identity).map_err(|_| err(Stage::RootWrite, K::Io))?;
    let witness = FreshWitness {
        schema_version: 1,
        contract: "switch-fresh-root-intent-v1".into(),
        phase: "fresh_intent".into(),
        root_id,
        source_id,
        creation_nonce: nonce,
        root_identity_sha256: hash(&root_bytes),
        source_identity_sha256: hash(&source_bytes),
        inspected_archive_paths: [
            "sms-forwarding/state.enc",
            "sms-forwarding-admission.json",
            "multistandby/store-v1.json",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
    };
    let witness_bytes =
        serde_json::to_vec(&witness).map_err(|_| err(Stage::WitnessWrite, K::Io))?;
    marker(
        &root,
        root_preflight::SOURCE_FILE,
        &source_bytes,
        [
            Stage::SourceCreate,
            Stage::SourceWrite,
            Stage::SourceFileSync,
            Stage::SourceDirectorySync,
        ],
        &mut hook,
    )?;
    marker(
        &root,
        root_preflight::ROOT_FILE,
        &root_bytes,
        [
            Stage::RootCreate,
            Stage::RootWrite,
            Stage::RootFileSync,
            Stage::RootDirectorySync,
        ],
        &mut hook,
    )?;
    marker(
        &root,
        root_preflight::WITNESS_FILE,
        &witness_bytes,
        [
            Stage::WitnessCreate,
            Stage::WitnessWrite,
            Stage::WitnessFileSync,
            Stage::WitnessDirectorySync,
        ],
        &mut hook,
    )?;
    let report = step(&mut hook, Stage::FinalCheck, || {
        parent.validate(Stage::FinalCheck)?;
        let named = fs::symlink_metadata(root_path)
            .map_err(|_| err(Stage::FinalCheck, K::ChangedDuringCreation))?;
        let guard_named = fs::symlink_metadata(parent.path.join(&guard_name))
            .map_err(|_| err(Stage::FinalCheck, K::ChangedDuringCreation))?;
        if !same_inode(&named, &root_inode)
            || !private(&named, true)
            || !same_inode(
                &guard_named,
                &lock.metadata().map_err(|_| err(Stage::FinalCheck, K::Io))?,
            )
            || !private(&guard_named, false)
        {
            return Err(err(Stage::FinalCheck, K::ChangedDuringCreation));
        }
        let report = root_preflight::inspect(root_path)
            .map_err(|_| err(Stage::FinalCheck, K::ChangedDuringCreation))?;
        if !report.fresh_intent_consistent
            || report.root_id != witness.root_id
            || report.source_id != witness.source_id
            || report.root_identity_sha256 != hash(&root_bytes)
            || report.source_identity_sha256 != hash(&source_bytes)
            || report.witness_sha256 != hash(&witness_bytes)
        {
            return Err(err(Stage::FinalCheck, K::ChangedDuringCreation));
        }
        Ok(LocalCreationReport {
            root_id: report.root_id,
            source_id: report.source_id,
            root_identity_sha256: report.root_identity_sha256,
            source_identity_sha256: report.source_identity_sha256,
            witness_sha256: report.witness_sha256,
            local_sync_sequence_completed: true,
            local_tracked_owners_drained: true,
            production_authorized: false,
            daemon_owner_integration_proven: false,
            historical_fresh_origin_proven: false,
            reopen_can_reissue_receipt: false,
        })
    })?;
    Ok(LocalCreationReceipt {
        report,
        _guard: NamespaceGuard {
            _parent: parent,
            _lock: lock,
        },
        _drained: drained,
    })
}
