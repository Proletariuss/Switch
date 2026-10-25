//! Legacy-compatible kernel locks, without rewriting historical evidence.
//!
//! The directory and lock descriptors stay owned by the operation guard. A
//! caller crossing another physical-operation boundary can revalidate the
//! namespace; retaining a descriptor alone cannot prevent an external rename.

use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{FileStat, Mode, fstatat};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Operation,
    AdbInterface,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Operation => "call-mode-operation.lock",
            Self::AdbInterface => ".module-adb-interface.lock",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug)]
struct RootBinding {
    proof: crate::daemon_lifetime::RootProof,
    root: PathBuf,
}
impl RootBinding {
    fn validate(&self) -> io::Result<()> {
        self.proof
            .with_root(&self.root, |_| ())
            .map_err(|_| unsafe_lock("bound operation lifetime root authority changed"))
    }
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn matches(self, metadata: &FileStat) -> bool {
        self.device == metadata.st_dev as u64 && self.inode == metadata.st_ino as u64
    }

    fn entry(metadata: &FileStat) -> Self {
        Self {
            device: metadata.st_dev as u64,
            inode: metadata.st_ino as u64,
        }
    }
}

/// Share through Arc when a detached physical worker outlives its waiter. The
/// last real owner explicitly unlocks, independently of incidental dup fds.
#[derive(Debug)]
pub(crate) struct OperationFileLock {
    // Only the parent module's existing lock-lifetime tests may duplicate it.
    pub(super) file: File,
    directory: File,
    requested_root: PathBuf,
    canonical_root: PathBuf,
    root_identity: Identity,
    lock_identity: Identity,
    name: &'static str,
    binding: Option<RootBinding>,
    explicitly_released: bool,
}

/// Read-only namespace evidence for a result waiting on actual release. This
/// retains a directory and independently opened read-only evidence descriptor,
/// never the locked open-file description or its Arc. It cannot extend kernel
/// exclusion or grant operation authority. The evidence descriptor pins the
/// original inode against reuse; only metadata is exposed.
#[derive(Debug)]
pub(crate) struct OperationNamespaceWitness {
    directory: File,
    entry: File,
    requested_root: PathBuf,
    canonical_root: PathBuf,
    root_identity: Identity,
    lock_identity: Identity,
    name: &'static str,
    binding: Option<RootBinding>,
}

impl OperationNamespaceWitness {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if let Some(binding) = &self.binding {
            binding.validate()?;
        }
        let held = self.directory.metadata()?;
        let canonical = fs::symlink_metadata(&self.canonical_root)?;
        let requested = fs::metadata(&self.requested_root)?;
        for metadata in [&held, &canonical, &requested] {
            check_root(metadata)?;
            if Identity::of(metadata) != self.root_identity {
                return Err(unsafe_lock("settled operation root identity changed"));
            }
        }
        let entry = self.entry.metadata()?;
        check_file(&entry)?;
        if Identity::of(&entry) != self.lock_identity {
            return Err(unsafe_lock("settled operation evidence identity changed"));
        }
        let named = fstatat(&self.directory, self.name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
        check_entry(&named)?;
        if !self.lock_identity.matches(&named) {
            return Err(unsafe_lock("settled operation lock name identity changed"));
        }
        if let Some(binding) = &self.binding {
            binding.validate()?;
        }
        Ok(())
    }
}

impl OperationFileLock {
    pub(crate) fn namespace_witness(&self) -> io::Result<OperationNamespaceWitness> {
        self.validate()?;
        let witness = OperationNamespaceWitness {
            directory: self.directory.try_clone()?,
            entry: File::from(openat(
                &self.directory,
                self.name,
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                Mode::empty(),
            )?),
            requested_root: self.requested_root.clone(),
            canonical_root: self.canonical_root.clone(),
            root_identity: self.root_identity,
            lock_identity: self.lock_identity,
            name: self.name,
            binding: self.binding.clone(),
        };
        witness.validate()?;
        Ok(witness)
    }
    /// Metadata only: do not read lock contents or change any existing inode.
    pub(crate) fn validate(&self) -> io::Result<()> {
        if let Some(binding) = &self.binding {
            binding.validate()?;
        }
        let held = self.directory.metadata()?;
        let canonical = fs::symlink_metadata(&self.canonical_root)?;
        let requested = fs::metadata(&self.requested_root)?;
        for metadata in [&held, &canonical, &requested] {
            check_root(metadata)?;
            if Identity::of(metadata) != self.root_identity {
                return Err(unsafe_lock("legacy lock root identity changed"));
            }
        }
        let held = self.file.metadata()?;
        check_file(&held)?;
        if Identity::of(&held) != self.lock_identity {
            return Err(unsafe_lock("legacy lock descriptor identity changed"));
        }
        let named = fstatat(&self.directory, self.name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
        check_entry(&named)?;
        if !self.lock_identity.matches(&named) {
            return Err(unsafe_lock("legacy lock name identity changed"));
        }
        if let Some(binding) = &self.binding {
            binding.validate()?;
        }
        Ok(())
    }

    /// Confirm the actual unlock and close before issuing metadata-only
    /// namespace evidence. Namespace failure never prevents original cleanup.
    /// Unlike Drop's best-effort diagnostics, either syscall error is returned.
    pub(crate) fn release(mut self) -> io::Result<OperationNamespaceWitness> {
        let witness = self.namespace_witness();
        // Preserve the legacy File field API without ManuallyDrop/unsafe. This
        // is only a held-directory duplicate, never the locked OFD. Drop skips
        // unlock after the original file has been extracted below.
        let placeholder = self.directory.try_clone()?;
        let unlocked = self.file.unlock();
        let original = std::mem::replace(&mut self.file, placeholder);
        self.explicitly_released = true;
        // nix consumes File via IntoRawFd and returns the real close result.
        // Never retry close after EINTR: its descriptor number may be reused.
        let closed = nix::unistd::close(original).map_err(io::Error::from);
        unlocked?;
        closed?;
        let witness = witness?;
        witness.validate()?;
        Ok(witness)
    }
}

impl Drop for OperationFileLock {
    fn drop(&mut self) {
        if self.explicitly_released {
            return;
        }
        if let Err(error) = self.file.unlock() {
            crate::diagnostics::write_line(format_args!(
                "call-mode operation lock release failed: {error}"
            ));
        }
    }
}

pub(super) fn acquire(root: &Path, kind: Kind, shared: bool) -> io::Result<OperationFileLock> {
    acquire_with_hook(root, kind, shared, || {})
}

/// A filesystem ownership prerequisite for a separate recovery owner. The
/// supplied proof must belong to the actual current lifetime. No directory is
/// reopened through a path and no ordinary operation right is minted here.
pub(crate) fn acquire_bound(
    root: &Path,
    proof: &crate::daemon_lifetime::RootProof,
    kind: Kind,
    shared: bool,
) -> io::Result<OperationFileLock> {
    acquire_bound_with_hook(root, proof, kind, shared, || {})
}

/// Inspectors must preserve a missing historical lock as missing. This entry
/// never creates a lock, including if an existing name disappears before open.
pub(crate) fn acquire_bound_existing(
    root: &Path,
    proof: &crate::daemon_lifetime::RootProof,
    kind: Kind,
    shared: bool,
) -> io::Result<OperationFileLock> {
    acquire_bound_with_hooks(root, proof, kind, shared, false, || {}, || {})
}

fn acquire_bound_with_hook(
    root: &Path,
    proof: &crate::daemon_lifetime::RootProof,
    kind: Kind,
    shared: bool,
    before_lock: impl FnOnce(),
) -> io::Result<OperationFileLock> {
    acquire_bound_with_hooks(root, proof, kind, shared, true, || {}, before_lock)
}

fn acquire_bound_with_hooks(
    root: &Path,
    proof: &crate::daemon_lifetime::RootProof,
    kind: Kind,
    shared: bool,
    allow_create: bool,
    before_open: impl FnOnce(),
    before_lock: impl FnOnce(),
) -> io::Result<OperationFileLock> {
    proof
        .with_root(root, |held| {
            let metadata = held.metadata()?;
            check_root(&metadata)?;
            let root_identity = Identity::of(&metadata);
            let directory = File::from(openat(
                held,
                ".",
                OFlag::O_RDONLY
                    | OFlag::O_DIRECTORY
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC
                    | OFlag::O_NONBLOCK,
                Mode::empty(),
            )?);
            let pinned = directory.metadata()?;
            check_root(&pinned)?;
            if Identity::of(&pinned) != root_identity {
                return Err(unsafe_lock("bound operation held root changed"));
            }
            acquire_in_directory(
                directory,
                root.to_path_buf(),
                fs::canonicalize(root)?,
                root_identity,
                kind,
                shared,
                allow_create,
                Some(RootBinding {
                    proof: proof.clone(),
                    root: root.to_path_buf(),
                }),
                before_open,
                before_lock,
            )
        })
        .map_err(|_| unsafe_lock("bound operation lifetime root authority changed"))?
}

fn acquire_with_hook(
    root: &Path,
    kind: Kind,
    shared: bool,
    before_lock: impl FnOnce(),
) -> io::Result<OperationFileLock> {
    if root.as_os_str().is_empty() {
        return Err(unsafe_lock("legacy lock root is empty"));
    }
    let requested_root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let requested = fs::metadata(&requested_root)?;
    check_root(&requested)?;
    let root_identity = Identity::of(&requested);
    let canonical_root = fs::canonicalize(&requested_root)?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(
            nix::libc::O_DIRECTORY
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NONBLOCK,
        )
        .open(&canonical_root)?;
    let held_root = directory.metadata()?;
    check_root(&held_root)?;
    if Identity::of(&held_root) != root_identity {
        return Err(unsafe_lock("legacy lock root changed while opening"));
    }
    acquire_in_directory(
        directory,
        requested_root,
        canonical_root,
        root_identity,
        kind,
        shared,
        true,
        None,
        || {},
        before_lock,
    )
}

fn acquire_in_directory(
    directory: File,
    requested_root: PathBuf,
    canonical_root: PathBuf,
    root_identity: Identity,
    kind: Kind,
    shared: bool,
    allow_create: bool,
    binding: Option<RootBinding>,
    before_open: impl FnOnce(),
    before_lock: impl FnOnce(),
) -> io::Result<OperationFileLock> {
    let name = kind.name();
    let flags = OFlag::O_RDWR | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK;
    let existing = match fstatat(&directory, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(metadata) => {
            check_entry(&metadata)?;
            Some(Identity::entry(&metadata))
        }
        Err(nix::errno::Errno::ENOENT) if allow_create => None,
        Err(error) => return Err(error.into()),
    };
    before_open();
    let (file, expected) = if let Some(identity) = existing {
        (
            File::from(openat(&directory, name, flags, Mode::empty())?),
            Some(identity),
        )
    } else {
        match openat(
            &directory,
            name,
            flags | OFlag::O_CREAT | OFlag::O_EXCL,
            Mode::S_IRUSR | Mode::S_IWUSR,
        ) {
            Ok(file) => (File::from(file), None),
            Err(nix::errno::Errno::EEXIST) => {
                // Another legitimate first opener may have won creation. It
                // must pass the same checks; never remove or replace its inode.
                let metadata = fstatat(&directory, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
                check_entry(&metadata)?;
                (
                    File::from(openat(&directory, name, flags, Mode::empty())?),
                    Some(Identity::entry(&metadata)),
                )
            }
            Err(error) => return Err(error.into()),
        }
    };
    let metadata = file.metadata()?;
    check_file(&metadata)?;
    if expected.is_some_and(|identity| identity != Identity::of(&metadata)) {
        return Err(unsafe_lock("legacy lock entry changed while opening"));
    }
    let guard = OperationFileLock {
        file,
        directory,
        requested_root,
        canonical_root,
        root_identity,
        lock_identity: Identity::of(&metadata),
        name,
        binding,
        explicitly_released: false,
    };
    // Each acquisition has a distinct open file description, so unlocking an
    // unsuccessful claimant cannot release another worker's kernel lock.
    before_lock();
    let locked = if shared {
        guard.file.try_lock_shared()
    } else {
        guard.file.try_lock()
    };
    locked.map_err(|error| match error {
        fs::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
        fs::TryLockError::Error(error) => error,
    })?;
    // Recheck after flock: path replacement must not manufacture permission to
    // operate on a new root or a new lock inode.
    guard.validate()?;
    Ok(guard)
}

#[cfg(test)]
#[path = "call_mode_file_lock_bound_tests.rs"]
mod bound_tests;

fn unsafe_lock(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn safe_mode(mode: u32) -> bool {
    // Legacy read-only group/other bits (e.g. 0644) do not grant lock mutation.
    // Never chmod historical files to make an unsafe mode appear acceptable.
    mode & 0o7022 == 0
}

fn check_root(metadata: &Metadata) -> io::Result<()> {
    if metadata.is_dir()
        && metadata.uid() == nix::unistd::geteuid().as_raw()
        && safe_mode(metadata.mode())
    {
        Ok(())
    } else {
        Err(unsafe_lock("legacy lock root is unsafe"))
    }
}

fn check_file(metadata: &Metadata) -> io::Result<()> {
    if metadata.is_file()
        && metadata.len() == 0
        && metadata.nlink() == 1
        && metadata.uid() == nix::unistd::geteuid().as_raw()
        && safe_mode(metadata.mode())
        && metadata.mode() & 0o111 == 0
    {
        Ok(())
    } else {
        Err(unsafe_lock("legacy lock file is unsafe"))
    }
}

fn check_entry(metadata: &FileStat) -> io::Result<()> {
    if metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFREG
        && metadata.st_size == 0
        && metadata.st_nlink == 1
        && metadata.st_uid == nix::unistd::geteuid().as_raw()
        && safe_mode(metadata.st_mode as u32)
        && metadata.st_mode & 0o111 == 0
    {
        Ok(())
    } else {
        Err(unsafe_lock("legacy lock entry is unsafe"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "switch-legacy-lock-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self, kind: Kind) -> PathBuf {
            self.0.join(kind.name())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn evidence(path: &Path) -> (u64, u64, u64, u32, i64, i64, i64, i64) {
        let metadata = fs::symlink_metadata(path).unwrap();
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mode(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    }

    #[test]
    fn compatible_existing_readable_lock_is_never_chmodded_or_replaced() {
        for kind in [Kind::Operation, Kind::AdbInterface] {
            let fixture = Fixture::new();
            let path = fixture.path(kind);
            fs::write(&path, []).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            let before = evidence(&path);
            let guard = acquire(&fixture.0, kind, false).unwrap();
            guard.validate().unwrap();
            assert_eq!(evidence(&path), before);
            drop(guard);
            assert_eq!(evidence(&path), before);
        }
    }

    #[test]
    fn unsafe_existing_evidence_is_rejected_without_repair() {
        for kind in [Kind::Operation, Kind::AdbInterface] {
            for shape in [
                "payload",
                "writable",
                "executable",
                "hardlink",
                "symlink",
                "directory",
            ] {
                let fixture = Fixture::new();
                let path = fixture.path(kind);
                match shape {
                    "payload" => fs::write(&path, b"unknown legacy evidence").unwrap(),
                    "writable" => {
                        fs::write(&path, []).unwrap();
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
                    }
                    "executable" => {
                        fs::write(&path, []).unwrap();
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                    }
                    "hardlink" => {
                        fs::write(&path, []).unwrap();
                        fs::hard_link(&path, fixture.0.join("other")).unwrap();
                    }
                    "symlink" => {
                        fs::write(fixture.0.join("other"), []).unwrap();
                        symlink("other", &path).unwrap();
                    }
                    "directory" => fs::create_dir(&path).unwrap(),
                    _ => unreachable!(),
                }
                let before = evidence(&path);
                assert!(acquire(&fixture.0, kind, false).is_err(), "{shape}");
                assert_eq!(evidence(&path), before, "{shape}");
                if shape == "payload" {
                    assert_eq!(fs::read(&path).unwrap(), b"unknown legacy evidence");
                }
            }
        }
    }

    #[test]
    fn opened_name_replacement_does_not_grant_a_different_inode() {
        let fixture = Fixture::new();
        let path = fixture.path(Kind::Operation);
        fs::write(&path, []).unwrap();
        let original = fixture.0.join("original");
        let result = acquire_with_hook(&fixture.0, Kind::Operation, false, || {
            fs::rename(&path, &original).unwrap();
            fs::write(&path, []).unwrap();
        });
        assert!(result.is_err());
        assert_ne!(evidence(&path).1, evidence(&original).1);
        let original = OpenOptions::new()
            .read(true)
            .write(true)
            .open(original)
            .unwrap();
        original.try_lock().unwrap(); // Rejected claim released only its original inode.
    }

    #[test]
    fn retained_guard_detects_rebased_root_without_releasing_original_lock() {
        let fixture = Fixture::new();
        let root = fixture.0.join("data");
        let moved = fixture.0.join("moved");
        fs::create_dir(&root).unwrap();
        let guard = acquire(&root, Kind::Operation, false).unwrap();
        fs::rename(&root, &moved).unwrap();
        fs::create_dir(&root).unwrap();
        assert!(guard.validate().is_err());
        let error = acquire(&moved, Kind::Operation, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(guard);
        acquire(&moved, Kind::Operation, false).unwrap();
    }

    #[test]
    fn retained_guard_detects_replaced_name_and_keeps_original_inode_locked() {
        let fixture = Fixture::new();
        let guard = acquire(&fixture.0, Kind::Operation, false).unwrap();
        let path = fixture.path(Kind::Operation);
        let original = fixture.0.join("original");
        fs::rename(&path, &original).unwrap();
        fs::write(&path, []).unwrap();
        assert!(guard.validate().is_err());
        let old_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&original)
            .unwrap();
        assert!(matches!(
            old_file.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(guard);
        old_file.try_lock().unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn namespace_witness_survives_release_without_retaining_kernel_exclusion() {
        for kind in [Kind::Operation, Kind::AdbInterface] {
            let fixture = Fixture::new();
            let guard = acquire(&fixture.0, kind, false).unwrap();
            let witness = guard.namespace_witness().unwrap();
            drop(guard);
            let next = acquire(&fixture.0, kind, false).unwrap();
            witness.validate().unwrap();
            drop(next);
            let path = fixture.path(kind);
            fs::rename(&path, fixture.0.join("retained-original")).unwrap();
            fs::write(&path, []).unwrap();
            assert!(witness.validate().is_err());
            // Retaining evidence never keeps the old inode locked either.
            let old = OpenOptions::new()
                .read(true)
                .write(true)
                .open(fixture.0.join("retained-original"))
                .unwrap();
            old.try_lock().unwrap();
            old.unlock().unwrap();
        }
    }

    #[test]
    fn fresh_locks_are_private_noninheritable_and_valid() {
        let fixture = Fixture::new();
        for kind in [Kind::Operation, Kind::AdbInterface] {
            let guard = acquire(&fixture.0, kind, false).unwrap();
            guard.validate().unwrap();
            assert_eq!(guard.file.metadata().unwrap().mode() & 0o777, 0o600);
            for file in [&guard.file, &guard.directory] {
                let flags = nix::fcntl::fcntl(file, nix::fcntl::FcntlArg::F_GETFD).unwrap();
                assert_ne!(flags & nix::libc::FD_CLOEXEC, 0);
            }
        }
    }

    #[test]
    fn root_rebased_during_acquisition_is_rejected() {
        let fixture = Fixture::new();
        let root = fixture.0.join("data");
        let moved = fixture.0.join("moved");
        fs::create_dir(&root).unwrap();
        let result = acquire_with_hook(&root, Kind::AdbInterface, false, || {
            fs::rename(&root, &moved).unwrap();
            fs::create_dir(&root).unwrap();
        });
        assert!(result.is_err());
        assert!(!root.join(Kind::AdbInterface.name()).exists());
        assert!(moved.join(Kind::AdbInterface.name()).is_file());
    }

    #[test]
    fn unsafe_root_permissions_are_not_repaired() {
        let fixture = Fixture::new();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(acquire(&fixture.0, Kind::Operation, false).is_err());
        assert_eq!(fs::metadata(&fixture.0).unwrap().mode() & 0o777, 0o777);
        assert!(!fixture.path(Kind::Operation).exists());
    }

    #[test]
    fn abnormal_entry_child() {
        let Some(root) = std::env::var_os("SWITCH_TEST_LEGACY_LOCK_ABNORMAL_ROOT") else {
            return;
        };
        for kind in [Kind::Operation, Kind::AdbInterface] {
            assert!(acquire(Path::new(&root), kind, false).is_err());
        }
        assert!(acquire(Path::new(&root), Kind::Operation, true).is_err());
        fs::write(Path::new(&root).join("child-completed"), []).unwrap();
    }

    #[test]
    fn occupied_entry_child() {
        let Some(root) = std::env::var_os("SWITCH_TEST_LEGACY_LOCK_ABNORMAL_ROOT") else {
            return;
        };
        for kind in [Kind::Operation, Kind::AdbInterface] {
            assert_eq!(
                acquire(Path::new(&root), kind, false).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
        assert_eq!(
            acquire(Path::new(&root), Kind::Operation, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        fs::write(Path::new(&root).join("child-completed"), []).unwrap();
    }

    fn child_probe(fixture: &Fixture, test: &str) {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test])
            .env("SWITCH_TEST_LEGACY_LOCK_ABNORMAL_ROOT", &fixture.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("legacy lock child probe blocked");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success());
        assert!(fixture.0.join("child-completed").is_file());
    }

    #[test]
    fn occupied_locks_fail_promptly_in_a_real_child() {
        let fixture = Fixture::new();
        let operation = acquire(&fixture.0, Kind::Operation, false).unwrap();
        let interface = acquire(&fixture.0, Kind::AdbInterface, false).unwrap();
        child_probe(
            &fixture,
            "call_mode_backups::file_lock::tests::occupied_entry_child",
        );
        operation.validate().unwrap();
        interface.validate().unwrap();
    }

    #[test]
    fn fifo_and_socket_are_rejected_in_a_bounded_real_child() {
        for shape in ["fifo", "socket"] {
            let fixture = Fixture::new();
            let mut sockets = Vec::new();
            for kind in [Kind::Operation, Kind::AdbInterface] {
                let path = fixture.path(kind);
                if shape == "fifo" {
                    nix::unistd::mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
                } else {
                    sockets.push(UnixListener::bind(&path).unwrap());
                }
            }
            let before =
                [Kind::Operation, Kind::AdbInterface].map(|kind| evidence(&fixture.path(kind)));
            child_probe(
                &fixture,
                "call_mode_backups::file_lock::tests::abnormal_entry_child",
            );
            assert_eq!(
                before,
                [Kind::Operation, Kind::AdbInterface].map(|kind| evidence(&fixture.path(kind)))
            );
        }
    }
}
