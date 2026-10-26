//! One participating daemon owns a data-root inode for its entire lifetime.
//!
//! Acquire synchronously before opening stores, recovery journals or owners,
//! then retain the lease in a process-static `OnceLock<Lease>`. A guard local
//! to an async run future is insufficient: cancellation or an early return can
//! leave retained physical workers alive. The lock file is never unlinked
//! by this module. Kernel lock release after process death is not a physical
//! recovery receipt, and legacy programs which ignore this protocol are not
//! made safe by it.
use nix::{
    fcntl::{AtFlags, OFlag, openat},
    sys::stat::{Mode, fstatat},
};
use std::{
    fmt,
    fs::{self, File, Metadata, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

const LOCK_FILE: &str = ".switchd-lifetime.lock";

#[derive(Debug)]
pub(crate) enum Error {
    Busy,
    UnsafeRoot,
    UnsafeLock,
    AuthorityEnded,
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => {
                formatter.write_str("Switch data directory is already owned by another daemon")
            }
            Self::UnsafeRoot => {
                formatter.write_str("Switch data directory identity is unsafe or changed")
            }
            Self::UnsafeLock => {
                formatter.write_str("Switch daemon lifetime lock is unsafe or changed")
            }
            Self::AuthorityEnded => {
                formatter.write_str("Switch daemon lifetime root authority is no longer active")
            }
            Self::Io(error) => write!(
                formatter,
                "Switch daemon lifetime admission failed: {error}"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

pub(crate) struct Lease {
    inner: Arc<LeaseInner>,
}

struct LeaseInner {
    // Fields close in declaration order: release the file lock before the
    // directory inode lock. No descriptor is cloned or handed to a child.
    lock: File,
    directory: File,
    requested_root: PathBuf,
    root: PathBuf,
    root_identity: Identity,
    lock_identity: Identity,
    active: AtomicBool,
    // The sealed SMS adapter owner cannot be reopened to clear health.
    device_sms_adapter_claimed: AtomicBool,
}

/// Evidence for this actual lease, never a path-derived or device permit.
/// Clones retain no lease ownership and become invalid when its owner drops.
#[derive(Clone)]
pub(crate) struct RootProof(Weak<LeaseInner>);

impl fmt::Debug for RootProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("RootProof").finish_non_exhaustive()
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.inner.active.store(false, Ordering::Release);
    }
}

impl RootProof {
    /// Identity comparison only; callers must also validate the live root.
    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }

    /// Consume this actual lease's sole sealed-SMS adapter enrollment. Clones
    /// share it; dropping an adapter never resets it. This is NOT owner drain
    /// or a device grant. Old lifetime-ignorant programs remain outside it.
    pub(crate) fn claim_device_sms_adapter(&self, root: &Path) -> Result<(), Error> {
        let inner = self.0.upgrade().ok_or(Error::AuthorityEnded)?;
        inner.validate_for(root)?;
        inner
            .device_sms_adapter_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::AuthorityEnded)?;
        inner.validate_for(root)
    }

    /// Run bounded filesystem work using the real held directory descriptor.
    /// Never duplicate this descriptor into a child or retain it as authority.
    /// Both boundaries must still name the original, active root and lock.
    pub(crate) fn with_root<T>(
        &self,
        root: &Path,
        inspect: impl FnOnce(&File) -> T,
    ) -> Result<T, Error> {
        let inner = self.0.upgrade().ok_or(Error::AuthorityEnded)?;
        inner.validate_for(root)?;
        let value = inspect(&inner.directory);
        inner.validate_for(root)?;
        Ok(value)
    }
}

impl Lease {
    /// Retain the returned lease in process-static storage immediately. On
    /// failure, no stores or device owners may have been constructed yet.
    pub(crate) fn acquire(root: &Path) -> Result<Self, Error> {
        if root.as_os_str().is_empty() || !root.is_absolute() {
            return Err(Error::UnsafeRoot);
        }
        // Reject existing unsafe roots before creating or opening any entry.
        // Existing permissions are evidence; never chmod them into eligibility.
        match fs::metadata(root) {
            Ok(metadata) => check_root_metadata(&metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(root)
                    .map_err(Error::Io)?;
            }
            Err(error) => return Err(Error::Io(error)),
        }
        let requested_metadata = fs::metadata(root).map_err(Error::Io)?;
        check_root_metadata(&requested_metadata)?;
        let root_identity = Identity::of(&requested_metadata);
        let canonical = fs::canonicalize(root).map_err(Error::Io)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&canonical)
            .map_err(Error::Io)?;
        let metadata = directory.metadata().map_err(Error::Io)?;
        check_root_metadata(&metadata)?;
        if Identity::of(&metadata) != root_identity {
            return Err(Error::UnsafeRoot);
        }
        // The directory lock also excludes an attempted replacement of the
        // named lock while an older participating daemon still owns this root.
        // Unsupported directory locking fails closed, rather than falling
        // back to path-only exclusion.
        lock_exclusive(&directory)?;

        match fstatat(&directory, LOCK_FILE, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(metadata) => check_lock_entry(&metadata)?,
            Err(nix::errno::Errno::ENOENT) => {}
            Err(error) => return Err(Error::Io(error.into())),
        }
        let lock = openat(
            &directory,
            LOCK_FILE,
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .map(File::from)
        .map_err(|error| match error {
            nix::errno::Errno::ELOOP | nix::errno::Errno::EISDIR => Error::UnsafeLock,
            _ => Error::Io(error.into()),
        })?;
        let metadata = lock.metadata().map_err(Error::Io)?;
        check_lock_metadata(&metadata)?;
        lock_exclusive(&lock)?;
        let lease = Self {
            inner: Arc::new(LeaseInner {
                lock,
                directory,
                requested_root: root.to_path_buf(),
                root: canonical,
                root_identity,
                lock_identity: Identity::of(&metadata),
                active: AtomicBool::new(true),
                device_sms_adapter_claimed: AtomicBool::new(false),
            }),
        };
        lease.validate()?;
        Ok(lease)
    }

    /// Use this canonical path for all subsequent store and socket roots.
    pub(crate) fn root(&self) -> &Path {
        &self.inner.root
    }

    pub(crate) fn root_proof(&self) -> RootProof {
        RootProof(Arc::downgrade(&self.inner))
    }

    /// Bounded admission evidence read through the held root directory. Any
    /// file shape counts as pending; never follow or parse the entry. A root
    /// or lifetime-lock namespace change is not absence of evidence.
    pub(crate) fn legacy_execution_pending(&self) -> Result<bool, Error> {
        self.validate()?;
        match fstatat(
            &self.inner.directory,
            crate::module_recovery::LEGACY_EXECUTION_PENDING,
            AtFlags::AT_SYMLINK_NOFOLLOW,
        ) {
            Ok(_) => Ok(true),
            Err(nix::errno::Errno::ENOENT) => Ok(false),
            Err(error) => Err(Error::Io(error.into())),
        }
    }

    /// Detect namespace replacement without unlinking or rewriting evidence.
    /// Pinning admission does not make arbitrary later path replacement safe
    /// for path-based stores; callers must not treat this as physical recovery.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.inner.validate_for(&self.inner.root)
    }
}

impl LeaseInner {
    fn validate_for(&self, root: &Path) -> Result<(), Error> {
        if !self.active.load(Ordering::Acquire) {
            return Err(Error::AuthorityEnded);
        }
        if !root.is_absolute() {
            return Err(Error::UnsafeRoot);
        }
        let supplied = fs::metadata(root).map_err(Error::Io)?;
        let held = self.directory.metadata().map_err(Error::Io)?;
        let named = fs::symlink_metadata(&self.root).map_err(Error::Io)?;
        let requested = fs::metadata(&self.requested_root).map_err(Error::Io)?;
        for metadata in [&supplied, &held, &named, &requested] {
            check_root_metadata(metadata)?;
            if Identity::of(metadata) != self.root_identity {
                return Err(Error::UnsafeRoot);
            }
        }
        let held = self.lock.metadata().map_err(Error::Io)?;
        check_lock_metadata(&held)?;
        if Identity::of(&held) != self.lock_identity {
            return Err(Error::UnsafeLock);
        }
        let named =
            fstatat(&self.directory, LOCK_FILE, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(|error| {
                match error {
                    nix::errno::Errno::ENOENT => Error::UnsafeLock,
                    _ => Error::Io(error.into()),
                }
            })?;
        check_lock_entry(&named)?;
        if named.st_dev as u64 != self.lock_identity.device
            || named.st_ino as u64 != self.lock_identity.inode
        {
            return Err(Error::UnsafeLock);
        }
        if self.active.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(Error::AuthorityEnded)
        }
    }
}

fn lock_exclusive(file: &File) -> Result<(), Error> {
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => Error::Busy,
        fs::TryLockError::Error(error) => Error::Io(error),
    })
}

fn safe_mode(mode: u32) -> bool {
    // Keep compatible read-only group/other bits, e.g. 0755 root / 0644 lock.
    // Group/other write and all special bits cannot authorize root operations.
    mode & 0o7022 == 0
}

fn check_root_metadata(metadata: &Metadata) -> Result<(), Error> {
    if metadata.is_dir()
        && metadata.uid() == nix::unistd::geteuid().as_raw()
        && safe_mode(metadata.mode())
    {
        Ok(())
    } else {
        Err(Error::UnsafeRoot)
    }
}

fn check_lock_metadata(metadata: &Metadata) -> Result<(), Error> {
    if metadata.is_file()
        && metadata.len() == 0
        && metadata.nlink() == 1
        && metadata.uid() == nix::unistd::geteuid().as_raw()
        && safe_mode(metadata.mode())
        && metadata.mode() & 0o111 == 0
    {
        Ok(())
    } else {
        Err(Error::UnsafeLock)
    }
}

fn check_lock_entry(metadata: &nix::sys::stat::FileStat) -> Result<(), Error> {
    if metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFREG
        && metadata.st_size == 0
        && metadata.st_nlink == 1
        && metadata.st_uid == nix::unistd::geteuid().as_raw()
        && safe_mode(metadata.st_mode as u32)
        && metadata.st_mode & 0o111 == 0
    {
        Ok(())
    } else {
        Err(Error::UnsafeLock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::{fs::symlink, net::UnixListener},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            // Keep synthetic AF_UNIX fixture paths short enough for macOS SUN_LEN.
            let parent = std::env::temp_dir().join("sw-dl");
            fs::create_dir_all(&parent).unwrap();
            let parent = fs::canonicalize(parent).unwrap();
            loop {
                let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::SeqCst);
                let path = parent.join(format!("{}-{sequence}", std::process::id()));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("fixture directory: {error}"),
                }
            }
        }
        fn root(&self) -> PathBuf {
            self.0.join("data")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn fresh_root_is_private_and_empty_lock_survives_raii_release_and_reacquisition() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let lease = Lease::acquire(&root).unwrap();
        assert_eq!(lease.root(), fs::canonicalize(&root).unwrap());
        lease.validate().unwrap();
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
        let metadata = fs::symlink_metadata(root.join(LOCK_FILE)).unwrap();
        assert!(metadata.is_file());
        assert_eq!((metadata.len(), metadata.nlink()), (0, 1));
        assert_eq!(metadata.mode() & 0o777, 0o600);
        let identity = Identity::of(&metadata);
        assert!(matches!(Lease::acquire(&root), Err(Error::Busy)));
        drop(lease);
        assert_eq!(
            Identity::of(&fs::metadata(root.join(LOCK_FILE)).unwrap()),
            identity
        );
        let next = Lease::acquire(&root).unwrap();
        assert_eq!(next.inner.lock_identity, identity);
        next.validate().unwrap();
    }

    #[test]
    fn symlink_and_lexical_aliases_share_the_actual_root_inode_lease() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let lease = Lease::acquire(&root).unwrap();
        let alias = fixture.0.join("alias");
        symlink(&root, &alias).unwrap();
        assert!(matches!(Lease::acquire(&alias), Err(Error::Busy)));
        assert!(matches!(
            Lease::acquire(&root.join("../data")),
            Err(Error::Busy)
        ));
        drop(lease);
        let alias_lease = Lease::acquire(&alias).unwrap();
        assert_eq!(alias_lease.root(), fs::canonicalize(&root).unwrap());
        alias_lease.validate().unwrap();
    }

    #[test]
    fn separate_root_inodes_can_be_owned_concurrently() {
        let fixture = Fixture::new();
        let first = Lease::acquire(&fixture.root()).unwrap();
        let second = Lease::acquire(&fixture.0.join("another-data")).unwrap();
        assert_ne!(first.inner.root_identity, second.inner.root_identity);
        first.validate().unwrap();
        second.validate().unwrap();
    }

    #[test]
    fn rejects_nonempty_symlink_hardlink_directory_fifo_and_socket_lock_without_modification() {
        for kind in [
            "nonempty",
            "symlink",
            "hardlink",
            "directory",
            "fifo",
            "socket",
        ] {
            let fixture = Fixture::new();
            let root = fixture.root();
            fs::create_dir(&root).unwrap();
            let path = root.join(LOCK_FILE);
            let target = fixture.0.join("evidence");
            let contents: &[u8] = if kind == "hardlink" {
                b""
            } else {
                b"preserve existing evidence"
            };
            fs::write(&target, contents).unwrap();
            let mut listener = None;
            match kind {
                "nonempty" => fs::write(&path, b"not a lifetime lock").unwrap(),
                "symlink" => symlink(&target, &path).unwrap(),
                "hardlink" => fs::hard_link(&target, &path).unwrap(),
                "directory" => fs::create_dir(&path).unwrap(),
                "fifo" => nix::unistd::mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap(),
                "socket" => listener = Some(UnixListener::bind(&path).unwrap()),
                _ => unreachable!(),
            }
            let before = fs::symlink_metadata(&path).unwrap();
            assert!(
                matches!(Lease::acquire(&root), Err(Error::UnsafeLock)),
                "{kind}"
            );
            let after = fs::symlink_metadata(&path).unwrap();
            assert_eq!(
                (
                    before.dev(),
                    before.ino(),
                    before.len(),
                    before.mode(),
                    before.nlink()
                ),
                (
                    after.dev(),
                    after.ino(),
                    after.len(),
                    after.mode(),
                    after.nlink()
                ),
                "{kind}"
            );
            assert_eq!(fs::read(&target).unwrap(), contents);
            if kind == "nonempty" {
                assert_eq!(fs::read(&path).unwrap(), b"not a lifetime lock");
            }
            drop(listener);
        }
    }

    #[test]
    fn held_directory_inode_prevents_replacement_lock_from_admitting_a_second_daemon() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let lease = Lease::acquire(&root).unwrap();
        let old_lock = fixture.0.join("original-lock-evidence");
        fs::rename(root.join(LOCK_FILE), &old_lock).unwrap();
        fs::write(root.join(LOCK_FILE), []).unwrap();
        assert!(matches!(lease.validate(), Err(Error::UnsafeLock)));
        assert!(matches!(Lease::acquire(&root), Err(Error::Busy)));
        assert!(old_lock.is_file());
        drop(lease);
        let next = Lease::acquire(&root).unwrap();
        next.validate().unwrap();
        assert!(old_lock.is_file(), "admission never removes old evidence");
    }

    #[test]
    fn renamed_root_cannot_escape_inode_lock_and_old_namespace_fails_validation() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let lease = Lease::acquire(&root).unwrap();
        let renamed = fixture.0.join("renamed-data");
        fs::rename(&root, &renamed).unwrap();
        assert!(lease.validate().is_err());
        assert!(matches!(Lease::acquire(&renamed), Err(Error::Busy)));
        drop(lease);
        Lease::acquire(&renamed).unwrap().validate().unwrap();
    }

    #[test]
    fn preexisting_file_lock_owner_is_respected_and_failed_attempt_releases_directory_lock() {
        let fixture = Fixture::new();
        let root = fixture.root();
        fs::create_dir(&root).unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(root.join(LOCK_FILE))
            .unwrap();
        file.try_lock().unwrap();
        assert!(matches!(Lease::acquire(&root), Err(Error::Busy)));
        drop(file);
        Lease::acquire(&root).unwrap().validate().unwrap();
    }

    #[test]
    fn existing_root_lock_permissions_and_business_evidence_are_not_rewritten() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let root = fixture.root();
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o750)).unwrap();
        let path = root.join(LOCK_FILE);
        fs::write(&path, []).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let evidence = root.join(".legacy-execution-pending-v1.json");
        fs::write(&evidence, b"unknown; not recovery proof").unwrap();
        let lease = Lease::acquire(&root).unwrap();
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o750);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
        assert_eq!(fs::read(evidence).unwrap(), b"unknown; not recovery proof");
        lease.validate().unwrap();
    }

    fn evidence_metadata(path: &Path) -> (u64, u64, u64, u32, u32, u64, i64, i64, i64, i64) {
        let metadata = fs::symlink_metadata(path).unwrap();
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mode(),
            metadata.uid(),
            metadata.nlink(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    }

    #[test]
    fn compatible_0755_root_and_0644_empty_lock_keep_all_evidence_metadata() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let root = fixture.root();
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let lock = root.join(LOCK_FILE);
        fs::write(&lock, []).unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
        let before = (evidence_metadata(&root), evidence_metadata(&lock));
        let lease = Lease::acquire(&root).unwrap();
        lease.root_proof().with_root(&root, |_| ()).unwrap();
        assert_eq!((evidence_metadata(&root), evidence_metadata(&lock)), before);
        drop(lease);
        assert_eq!((evidence_metadata(&root), evidence_metadata(&lock)), before);
    }

    #[test]
    fn raw_mode_policy_rejects_each_special_or_shared_write_bit() {
        for mode in [0o700, 0o750, 0o755, 0o600, 0o640, 0o644] {
            assert!(safe_mode(mode), "compatible raw mode {mode:o}");
        }
        for bit in [0o4000, 0o2000, 0o1000, 0o020, 0o002] {
            assert!(!safe_mode(0o755 | bit), "unsafe root policy bit {bit:o}");
            assert!(!safe_mode(0o600 | bit), "unsafe lock policy bit {bit:o}");
        }
    }

    fn set_observed_mode(path: &Path, requested: u32) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(requested)).unwrap();
        let observed = fs::symlink_metadata(path).unwrap().mode() & 0o7777;
        // The mandatory macOS sandbox may strip SUID/SGID without making
        // chmod fail. Neither skip that case nor claim the stripped bit exists.
        assert!(
            observed == requested || observed == requested & !0o6000,
            "requested {requested:o}, unexpected effective mode {observed:o}"
        );
        observed
    }

    #[test]
    fn unsafe_existing_root_or_lock_modes_are_rejected_without_repair_or_creation() {
        for mode in [0o777, 0o770, 0o1755, 0o2755, 0o4755] {
            let fixture = Fixture::new();
            let root = fixture.root();
            fs::create_dir(&root).unwrap();
            let observed = set_observed_mode(&root, mode);
            let safe = observed & 0o7022 == 0;
            if safe {
                // An accepted existing root must not change its directory
                // timestamps merely because this test omitted the lock entry.
                fs::write(root.join(LOCK_FILE), []).unwrap();
            }
            let before = evidence_metadata(&root);
            let acquired = Lease::acquire(&root);
            if safe {
                let lease = acquired.unwrap();
                lease.root_proof().with_root(&root, |_| ()).unwrap();
                lease.validate().unwrap();
            } else {
                assert!(
                    matches!(acquired, Err(Error::UnsafeRoot)),
                    "requested {mode:o}, effective {observed:o}"
                );
                assert!(!root.join(LOCK_FILE).exists(), "unsafe root created lock");
            }
            assert_eq!(evidence_metadata(&root), before, "{mode:o}");
        }
        for mode in [0o666, 0o660, 0o700, 0o1644, 0o2644, 0o4644] {
            let fixture = Fixture::new();
            let root = fixture.root();
            fs::create_dir(&root).unwrap();
            let lock = root.join(LOCK_FILE);
            fs::write(&lock, []).unwrap();
            let observed = set_observed_mode(&lock, mode);
            let before = (evidence_metadata(&root), evidence_metadata(&lock));
            let acquired = Lease::acquire(&root);
            if observed & 0o7133 == 0 {
                let lease = acquired.unwrap();
                lease.root_proof().with_root(&root, |_| ()).unwrap();
                lease.validate().unwrap();
            } else {
                assert!(
                    matches!(acquired, Err(Error::UnsafeLock)),
                    "requested {mode:o}, effective {observed:o}"
                );
            }
            assert_eq!(
                (evidence_metadata(&root), evidence_metadata(&lock)),
                before,
                "{mode:o}"
            );
        }
    }

    #[test]
    fn live_root_and_lock_mode_changes_revoke_proof_before_filesystem_work() {
        use std::sync::atomic::AtomicUsize;
        for (lock_changed, mode) in [
            (false, 0o777),
            (false, 0o2755),
            (true, 0o666),
            (true, 0o700),
        ] {
            let fixture = Fixture::new();
            let root = fixture.root();
            let lease = Lease::acquire(&root).unwrap();
            let proof = lease.root_proof();
            let target = if lock_changed {
                root.join(LOCK_FILE)
            } else {
                root.clone()
            };
            let observed = set_observed_mode(&target, mode);
            let before = evidence_metadata(&target);
            let calls = AtomicUsize::new(0);
            let result = proof.with_root(&root, |_| {
                calls.fetch_add(1, Ordering::SeqCst);
            });
            let safe = observed & if lock_changed { 0o7133 } else { 0o7022 } == 0;
            if safe {
                result.unwrap();
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                lease.validate().unwrap();
            } else {
                assert!(if lock_changed {
                    matches!(result, Err(Error::UnsafeLock))
                } else {
                    matches!(result, Err(Error::UnsafeRoot))
                });
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                assert!(lease.validate().is_err());
            }
            assert_eq!(evidence_metadata(&target), before);
        }
    }

    #[test]
    fn mode_changes_during_work_cannot_authorize_result_or_repair_evidence() {
        use std::os::unix::fs::PermissionsExt;
        for lock_changed in [false, true] {
            let fixture = Fixture::new();
            let root = fixture.root();
            let lease = Lease::acquire(&root).unwrap();
            let proof = lease.root_proof();
            let target = if lock_changed {
                root.join(LOCK_FILE)
            } else {
                root.clone()
            };
            let result = proof.with_root(&root, |_| {
                fs::set_permissions(&target, fs::Permissions::from_mode(0o777)).unwrap();
                evidence_metadata(&target)
            });
            assert!(if lock_changed {
                matches!(result, Err(Error::UnsafeLock))
            } else {
                matches!(result, Err(Error::UnsafeRoot))
            });
            let before = evidence_metadata(&target);
            assert!(lease.validate().is_err());
            assert_eq!(evidence_metadata(&target), before);
            assert_eq!(before.3 & 0o777, 0o777);
        }
    }

    #[test]
    fn root_and_lock_descriptors_are_close_on_exec_and_lease_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Lease>();
        let fixture = Fixture::new();
        let lease = Lease::acquire(&fixture.root()).unwrap();
        for file in [&lease.inner.directory, &lease.inner.lock] {
            let flags = nix::fcntl::fcntl(file, nix::fcntl::FcntlArg::F_GETFD).unwrap();
            assert_ne!(flags & nix::libc::FD_CLOEXEC, 0);
        }
    }

    #[test]
    fn relative_or_empty_root_is_rejected_before_creation() {
        assert!(matches!(
            Lease::acquire(Path::new("")),
            Err(Error::UnsafeRoot)
        ));
        assert!(matches!(
            Lease::acquire(Path::new("relative-data")),
            Err(Error::UnsafeRoot)
        ));
    }

    #[test]
    fn root_proof_uses_the_actual_held_directory_and_rejects_foreign_roots_before_work() {
        use std::sync::atomic::AtomicUsize;
        let fixture = Fixture::new();
        let root = fixture.root();
        let lease = Lease::acquire(&root).unwrap();
        let proof = lease.root_proof();
        let observed = proof
            .with_root(&root, |directory| {
                Identity::of(&directory.metadata().unwrap())
            })
            .unwrap();
        assert_eq!(observed, lease.inner.root_identity);
        let foreign = fixture.0.join("foreign");
        fs::create_dir(&foreign).unwrap();
        let calls = AtomicUsize::new(0);
        assert!(matches!(
            proof.with_root(&foreign, |_| {
                calls.fetch_add(1, Ordering::SeqCst);
            }),
            Err(Error::UnsafeRoot)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn proof_clones_do_not_keep_lease_authority_or_reacquire_it_from_the_same_path() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let lease = Lease::acquire(&root).unwrap();
        let proof = lease.root_proof();
        let clone = proof.clone();
        drop(lease);
        assert!(matches!(
            proof.with_root(&root, |_| panic!("ended authority ran work")),
            Err(Error::AuthorityEnded)
        ));
        let next = Lease::acquire(&root).unwrap();
        assert!(matches!(
            clone.with_root(&root, |_| panic!("old proof adopted new lease")),
            Err(Error::AuthorityEnded)
        ));
        next.root_proof().with_root(&root, |_| ()).unwrap();
    }

    #[test]
    fn proof_revalidates_after_work_and_owner_drop_does_not_authorize_its_result() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let lease = Lease::acquire(&root).unwrap();
        let proof = lease.root_proof();
        assert!(matches!(
            proof.with_root(&root, |_| drop(lease)),
            Err(Error::AuthorityEnded)
        ));
        Lease::acquire(&root).unwrap().validate().unwrap();
    }

    #[test]
    fn proof_rejects_root_or_lifetime_lock_replacement_before_and_after_work() {
        for after_work in [false, true] {
            let fixture = Fixture::new();
            let root = fixture.root();
            let lease = Lease::acquire(&root).unwrap();
            let proof = lease.root_proof();
            let replace = || {
                fs::rename(root.join(LOCK_FILE), fixture.0.join("original-lock")).unwrap();
                fs::write(root.join(LOCK_FILE), []).unwrap();
            };
            if !after_work {
                replace();
            }
            assert!(matches!(
                proof.with_root(&root, |_| {
                    assert!(after_work, "changed namespace reached filesystem closure");
                    replace();
                }),
                Err(Error::UnsafeLock)
            ));
        }
        let fixture = Fixture::new();
        let root = fixture.root();
        let lease = Lease::acquire(&root).unwrap();
        let proof = lease.root_proof();
        fs::rename(&root, fixture.0.join("old-root")).unwrap();
        fs::create_dir(&root).unwrap();
        assert!(matches!(
            proof.with_root(&root, |_| panic!("replaced root ran work")),
            Err(Error::UnsafeRoot)
        ));
    }

    #[test]
    fn proof_accepts_same_inode_alias_but_rejects_retargeted_requested_alias() {
        let fixture = Fixture::new();
        let root = fixture.root();
        fs::create_dir(&root).unwrap();
        let alias = fixture.0.join("alias");
        symlink(&root, &alias).unwrap();
        let lease = Lease::acquire(&alias).unwrap();
        let proof = lease.root_proof();
        proof.with_root(&root, |_| ()).unwrap();
        proof.with_root(&alias, |_| ()).unwrap();
        let foreign = fixture.0.join("foreign");
        fs::create_dir(&foreign).unwrap();
        fs::remove_file(&alias).unwrap();
        symlink(foreign, &alias).unwrap();
        assert!(matches!(
            proof.with_root(&root, |_| panic!("retargeted alias ran work")),
            Err(Error::UnsafeRoot)
        ));
    }
}
