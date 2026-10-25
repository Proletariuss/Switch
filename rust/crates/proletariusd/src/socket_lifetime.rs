//! Participating daemons serialize one Unix listener pathname across data roots.
//!
//! Hold Claim across stale-path preparation, bind, guard capture and listener
//! teardown. The empty companion file is never unlinked. Parent directories are
//! pinned for openat/identity checks but are deliberately not flocked: main may
//! already hold its independent data-root directory lease. This protocol does
//! not prevent an unrelated program from maliciously unlinking a held lock.
use nix::{
    fcntl::{AtFlags, OFlag, openat},
    sys::stat::{Mode, fstatat},
};
use std::{
    ffi::OsString,
    fmt,
    fs::{self, File, Metadata, OpenOptions},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

const SUFFIX: &str = ".switch-listener.lock";

#[derive(Debug)]
pub(crate) enum Error {
    Busy,
    SocketInUse,
    ProbeUnconfirmed,
    InvalidPath,
    UnsafeParent,
    UnsafeLock,
    Io {
        stage: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("backend listener path is already claimed"),
            Self::SocketInUse => formatter.write_str("backend_socket_in_use"),
            Self::ProbeUnconfirmed => {
                formatter.write_str("backend listener ownership probe was not confirmed")
            }
            Self::InvalidPath => formatter.write_str("backend listener path is invalid"),
            Self::UnsafeParent => {
                formatter.write_str("backend listener parent identity is unsafe or changed")
            }
            Self::UnsafeLock => formatter.write_str("backend listener lock is unsafe or changed"),
            Self::Io {
                stage,
                path,
                source,
            } => write!(
                formatter,
                "backend listener admission failed at {stage} ({}): {source}",
                path.display()
            ),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
fn io_at(stage: &'static str, path: &Path, source: io::Error) -> Error {
    Error::Io {
        stage,
        path: path.to_path_buf(),
        source,
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

pub(crate) struct Claim {
    lock: File,
    parent: File,
    requested_parent: PathBuf,
    canonical_parent: PathBuf,
    path: PathBuf,
    companion: OsString,
    parent_identity: Identity,
    lock_identity: Identity,
}
impl Claim {
    pub(crate) fn acquire(path: &Path) -> Result<Self, Error> {
        let bytes = path.as_os_str().as_bytes();
        // Preserve the final filename exactly, including non-UTF8 Unix bytes;
        // reject spellings whose final component denotes a directory instead.
        if bytes.contains(&0)
            || matches!(
                bytes.rsplit(|byte| *byte == b'/').next(),
                None | Some(b"" | b"." | b"..")
            )
        {
            return Err(Error::InvalidPath);
        }
        let filename = path.file_name().ok_or(Error::InvalidPath)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let requested_parent = if parent.is_absolute() {
            parent.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| io_at("current directory", path, error))?
                .join(parent)
        };
        let requested = fs::metadata(&requested_parent)
            .map_err(|error| io_at("requested parent metadata", &requested_parent, error))?;
        if !requested.is_dir() {
            return Err(Error::UnsafeParent);
        }
        let parent_identity = Identity::of(&requested);
        let canonical_parent = fs::canonicalize(&requested_parent)
            .map_err(|error| io_at("canonicalize parent", &requested_parent, error))?;
        let parent = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&canonical_parent)
            .map_err(|error| io_at("open canonical parent", &canonical_parent, error))?;
        let held_parent = parent
            .metadata()
            .map_err(|error| io_at("opened parent metadata", &canonical_parent, error))?;
        if !held_parent.is_dir() || Identity::of(&held_parent) != parent_identity {
            return Err(Error::UnsafeParent);
        }
        let companion = companion_name(filename);
        match fstatat(&parent, companion.as_os_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(metadata) => check_entry(&metadata)?,
            Err(nix::errno::Errno::ENOENT) => {}
            Err(error) => {
                return Err(io_at(
                    "inspect companion lock",
                    &canonical_parent.join(&companion),
                    error.into(),
                ));
            }
        }
        // Darwin may return ENOENT when two participants openat(O_CREAT) the
        // same initially absent companion at once. Retry only this transient
        // creation race. The held parent descriptor and later identity checks
        // still reject a moved parent or replaced companion.
        let mut missing_retries = 0;
        let lock = loop {
            match openat(
                &parent,
                companion.as_os_str(),
                OFlag::O_RDWR
                    | OFlag::O_CREAT
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC
                    | OFlag::O_NONBLOCK,
                Mode::S_IRUSR | Mode::S_IWUSR,
            ) {
                Ok(descriptor) => break File::from(descriptor),
                Err(nix::errno::Errno::ENOENT) if missing_retries < 2 => {
                    missing_retries += 1;
                    std::thread::yield_now();
                }
                Err(nix::errno::Errno::ELOOP | nix::errno::Errno::EISDIR) => {
                    return Err(Error::UnsafeLock);
                }
                Err(error) => {
                    return Err(io_at(
                        "open companion lock",
                        &canonical_parent.join(&companion),
                        error.into(),
                    ));
                }
            }
        };
        let metadata = lock.metadata().map_err(|error| {
            io_at(
                "opened companion lock metadata",
                &canonical_parent.join(&companion),
                error,
            )
        })?;
        check_metadata(&metadata)?;
        lock.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => Error::Busy,
            fs::TryLockError::Error(error) => {
                io_at("lock companion", &canonical_parent.join(&companion), error)
            }
        })?;
        let claim = Self {
            lock,
            parent,
            requested_parent,
            path: canonical_parent.join(filename),
            canonical_parent,
            companion,
            parent_identity,
            lock_identity: Identity::of(&metadata),
        };
        claim.validate()?;
        Ok(claim)
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let held = self
            .parent
            .metadata()
            .map_err(|error| io_at("validate opened parent", &self.canonical_parent, error))?;
        let canonical = fs::symlink_metadata(&self.canonical_parent)
            .map_err(|error| io_at("validate canonical parent", &self.canonical_parent, error))?;
        let requested = fs::metadata(&self.requested_parent)
            .map_err(|error| io_at("validate requested parent", &self.requested_parent, error))?;
        if [&held, &canonical, &requested]
            .into_iter()
            .any(|metadata| !metadata.is_dir() || Identity::of(metadata) != self.parent_identity)
        {
            return Err(Error::UnsafeParent);
        }
        let held = self.lock.metadata().map_err(|error| {
            io_at(
                "validate opened companion lock",
                &self.canonical_parent.join(&self.companion),
                error,
            )
        })?;
        check_metadata(&held)?;
        if Identity::of(&held) != self.lock_identity {
            return Err(Error::UnsafeLock);
        }
        let named = fstatat(
            &self.parent,
            self.companion.as_os_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|error| match error {
            nix::errno::Errno::ENOENT => Error::UnsafeLock,
            _ => io_at(
                "validate named companion lock",
                &self.canonical_parent.join(&self.companion),
                error.into(),
            ),
        })?;
        check_entry(&named)?;
        if named.st_dev as u64 != self.lock_identity.device
            || named.st_ino as u64 != self.lock_identity.inode
        {
            return Err(Error::UnsafeLock);
        }
        Ok(())
    }
}
impl Drop for Claim {
    fn drop(&mut self) {
        // Darwin inherits flock state across fork. O_CLOEXEC only closes the
        // inherited descriptor after exec, so relying on File::drop alone can
        // leave a listener claim transiently busy while a child is between
        // fork and exec. Explicitly unlock the shared open-file description
        // before closing our descriptor so daemon restart is deterministic.
        if let Err(error) = self.lock.unlock() {
            crate::diagnostics::write_line(format_args!(
                "backend listener lock release failed: {error}"
            ));
        }
    }
}
fn companion_name(filename: &std::ffi::OsStr) -> OsString {
    let mut companion = filename.to_os_string();
    companion.push(SUFFIX);
    companion
}

// Darwin can report ECONNREFUSED for a live AF_UNIX listener whose accept
// queue is full. Refusal alone is therefore never stale evidence. This extra
// check depends on the installed /usr/sbin/lsof and same-UID process visibility
// in a trusted directory. It does not promise detection of malicious root,
// hidden owners or concurrent pathname replacement; callers still hold Claim
// and recheck the original socket inode immediately before removal.
const PROBE_LIMIT: usize = 16 * 1024;
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

pub(crate) async fn verify_stale_socket(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path).map_err(|_| Error::ProbeUnconfirmed)?;
    if !visible_socket(&metadata, nix::unistd::geteuid().as_raw()) {
        return Err(Error::ProbeUnconfirmed);
    }
    let child = tokio::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-U", "-Fp", "--"])
        .arg(path)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| Error::ProbeUnconfirmed)?;
    let (status, stdout, stderr) = collect_probe(child).await?;
    let classified = classify_probe(status.code(), &stdout, &stderr);
    #[cfg(test)]
    if classified.is_err() {
        eprintln!(
            "socket ownership probe: path={path:?} status={:?} stdout={:?} stderr={:?}",
            status.code(),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }
    classified
}
fn visible_socket(metadata: &Metadata, uid: u32) -> bool {
    metadata.file_type().is_socket() && metadata.uid() == uid
}
async fn bounded_output(input: impl tokio::io::AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut output = Vec::new();
    input
        .take((PROBE_LIMIT + 1) as u64)
        .read_to_end(&mut output)
        .await?;
    if output.len() > PROBE_LIMIT {
        return Err(io::Error::other(
            "listener ownership probe output exceeded limit",
        ));
    }
    Ok(output)
}
async fn collect_probe(
    mut child: tokio::process::Child,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), Error> {
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(Error::ProbeUnconfirmed);
    };
    let result = tokio::time::timeout(PROBE_TIMEOUT, async {
        tokio::try_join!(child.wait(), bounded_output(stdout), bounded_output(stderr))
    })
    .await;
    match result {
        Ok(Ok(output)) => Ok(output),
        _ => {
            // Kill and reap this exact child on timeout/read/output failure.
            // A cancelled caller also retains kill_on_drop(true).
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(Error::ProbeUnconfirmed)
        }
    }
}
fn classify_probe(status: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<(), Error> {
    if stdout.len() > PROBE_LIMIT || stderr.len() > PROBE_LIMIT || !stderr.is_empty() {
        return Err(Error::ProbeUnconfirmed);
    }
    if status == Some(1) && stdout.is_empty() {
        return Ok(());
    }
    if status == Some(0) && normal_owner_fields(stdout) {
        return Err(Error::SocketInUse);
    }
    Err(Error::ProbeUnconfirmed)
}
fn normal_owner_fields(output: &[u8]) -> bool {
    if !output.ends_with(b"\n") {
        return false;
    }
    let mut process = false;
    let mut descriptor = false;
    for line in output[..output.len() - 1].split(|byte| *byte == b'\n') {
        match line.split_first() {
            Some((b'p', digits)) if positive_decimal(digits) => {
                if process && !descriptor {
                    return false;
                }
                process = true;
                descriptor = false;
            }
            Some((b'f', digits))
                if process && !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) =>
            {
                descriptor = true
            }
            _ => return false,
        }
    }
    process && descriptor
}
fn positive_decimal(digits: &[u8]) -> bool {
    !digits.is_empty()
        && digits.iter().all(u8::is_ascii_digit)
        && digits.iter().any(|byte| *byte != b'0')
}

fn check_metadata(metadata: &Metadata) -> Result<(), Error> {
    if metadata.is_file() && metadata.len() == 0 && metadata.nlink() == 1 {
        Ok(())
    } else {
        Err(Error::UnsafeLock)
    }
}
fn check_entry(metadata: &nix::sys::stat::FileStat) -> Result<(), Error> {
    if metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFREG
        && metadata.st_size == 0
        && metadata.st_nlink == 1
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
        os::unix::{
            ffi::OsStringExt,
            fs::{PermissionsExt, symlink},
            net::UnixListener,
        },
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, AtomicU64, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            // Keep Unix-domain socket fixture paths below Darwin's sun_path limit.
            let base = std::env::temp_dir().join("sw-sl");
            fs::create_dir_all(&base).unwrap();
            let base = fs::canonicalize(base).unwrap();
            loop {
                let path = base.join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::SeqCst)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("listener claim fixture: {error}"),
                }
            }
        }
        fn path(&self) -> PathBuf {
            self.0.join("s")
        }
        fn companion(&self) -> PathBuf {
            self.0.join(format!("s{SUFFIX}"))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn same_path_is_exclusive_until_drop_and_empty_companion_is_never_unlinked() {
        let fixture = Fixture::new();
        let claim = Claim::acquire(&fixture.path()).unwrap();
        claim.validate().unwrap();
        assert_eq!(claim.path(), fixture.path());
        let before = fs::symlink_metadata(fixture.companion()).unwrap();
        assert!(before.is_file());
        assert_eq!((before.len(), before.nlink()), (0, 1));
        assert_eq!(before.mode() & 0o777, 0o600);
        assert!(matches!(Claim::acquire(&fixture.path()), Err(Error::Busy)));
        drop(claim);
        assert_eq!(
            Identity::of(&before),
            Identity::of(&fs::symlink_metadata(fixture.companion()).unwrap())
        );
        Claim::acquire(&fixture.path()).unwrap().validate().unwrap();
    }
    #[test]
    fn concurrent_claims_create_one_companion_and_one_holder() {
        // Open the initially absent companion from two independent file
        // descriptions at once. A competing daemon must see Busy, not a
        // transient filesystem error or a second successful claim.
        for _ in 0..32 {
            let fixture = Fixture::new();
            let path = fixture.path();
            let gate = Arc::new(Barrier::new(3));
            let release = Arc::new(AtomicBool::new(false));
            let (sender, receiver) = mpsc::channel();
            thread::scope(|scope| {
                let mut workers = Vec::new();
                for _ in 0..2 {
                    let (gate, release, sender, path) = (
                        Arc::clone(&gate),
                        Arc::clone(&release),
                        sender.clone(),
                        path.clone(),
                    );
                    workers.push(scope.spawn(move || {
                        gate.wait();
                        let claim = Claim::acquire(&path);
                        let result = match &claim {
                            Ok(_) => Ok(true),
                            Err(Error::Busy) => Ok(false),
                            Err(error) => Err(error.to_string()),
                        };
                        sender.send(result).unwrap();
                        let deadline = Instant::now() + Duration::from_secs(5);
                        while !release.load(Ordering::Acquire) && Instant::now() < deadline {
                            thread::sleep(Duration::from_millis(1));
                        }
                        drop(claim);
                    }));
                }
                gate.wait();
                let first = receiver.recv_timeout(Duration::from_secs(5));
                let second = receiver.recv_timeout(Duration::from_secs(5));
                release.store(true, Ordering::Release);
                for worker in workers {
                    worker.join().unwrap();
                }
                let results = [first.unwrap().unwrap(), second.unwrap().unwrap()];
                assert_eq!(results.into_iter().filter(|won| *won).count(), 1);
            });
            let companion = fs::symlink_metadata(fixture.companion()).unwrap();
            assert!(companion.is_file());
            assert_eq!((companion.len(), companion.nlink()), (0, 1));
        }
    }
    #[test]
    fn missing_parent_io_identifies_operation_and_path() {
        let fixture = Fixture::new();
        let missing = fixture.0.join("missing");
        match Claim::acquire(&missing.join("s")) {
            Err(Error::Io {
                stage,
                path,
                source,
            }) => {
                assert_eq!(stage, "requested parent metadata");
                assert_eq!(path, missing);
                assert_eq!(source.kind(), io::ErrorKind::NotFound);
            }
            _ => panic!("missing parent should identify the failing admission operation"),
        }
    }
    #[test]
    fn drop_explicitly_unlocks_a_fork_inherited_open_file_description() {
        let fixture = Fixture::new();
        let claim = Claim::acquire(&fixture.path()).unwrap();
        let inherited = claim.lock.try_clone().unwrap();
        drop(claim);

        // A forked child inherits the same open-file description before
        // CLOEXEC takes effect. Claim::drop must explicitly unlock it so a
        // daemon restart does not wait for that unrelated child to exec/exit.
        Claim::acquire(&fixture.path()).unwrap().validate().unwrap();
        drop(inherited);
    }
    #[test]
    fn parent_symlink_and_lexical_aliases_share_the_same_listener_claim() {
        let fixture = Fixture::new();
        let parent = fixture.0.join("p");
        fs::create_dir(&parent).unwrap();
        let alias = fixture.0.join("alias");
        symlink(&parent, &alias).unwrap();
        let claim = Claim::acquire(&parent.join("s")).unwrap();
        assert!(matches!(Claim::acquire(&alias.join("s")), Err(Error::Busy)));
        assert!(matches!(
            Claim::acquire(&parent.join("../p/s")),
            Err(Error::Busy)
        ));
        drop(claim);
        let alias_claim = Claim::acquire(&alias.join("s")).unwrap();
        assert_eq!(alias_claim.path(), parent.join("s"));
    }
    #[test]
    fn distinct_paths_can_coexist_even_when_main_already_flocks_parent_directory() {
        let fixture = Fixture::new();
        let parent = File::open(&fixture.0).unwrap();
        parent.try_lock().unwrap();
        let first = Claim::acquire(&fixture.path()).unwrap();
        let second = Claim::acquire(&fixture.0.join("other")).unwrap();
        first.validate().unwrap();
        second.validate().unwrap();
        assert_ne!(first.lock_identity, second.lock_identity);
    }
    #[test]
    fn unsafe_companion_types_are_rejected_without_changing_existing_evidence() {
        for kind in [
            "nonempty",
            "symlink",
            "hardlink",
            "directory",
            "fifo",
            "socket",
        ] {
            let fixture = Fixture::new();
            let path = fixture.companion();
            let target = fixture.0.join("evidence");
            let data: &[u8] = if kind == "hardlink" {
                b""
            } else {
                b"preserve evidence"
            };
            fs::write(&target, data).unwrap();
            let mut listener = None;
            match kind {
                "nonempty" => fs::write(&path, b"not a claim").unwrap(),
                "symlink" => symlink(&target, &path).unwrap(),
                "hardlink" => fs::hard_link(&target, &path).unwrap(),
                "directory" => fs::create_dir(&path).unwrap(),
                "fifo" => nix::unistd::mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap(),
                "socket" => listener = Some(UnixListener::bind(&path).unwrap()),
                _ => unreachable!(),
            }
            let before = fs::symlink_metadata(&path).unwrap();
            assert!(
                matches!(Claim::acquire(&fixture.path()), Err(Error::UnsafeLock)),
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
            assert_eq!(fs::read(&target).unwrap(), data);
            drop(listener);
        }
    }
    #[test]
    fn existing_companion_permissions_and_live_listener_are_not_rewritten() {
        let fixture = Fixture::new();
        fs::write(fixture.companion(), []).unwrap();
        fs::set_permissions(fixture.companion(), fs::Permissions::from_mode(0o640)).unwrap();
        let listener = UnixListener::bind(fixture.path()).unwrap();
        let socket = fs::symlink_metadata(fixture.path()).unwrap();
        let claim = Claim::acquire(&fixture.path()).unwrap();
        claim.validate().unwrap();
        assert_eq!(
            fs::metadata(fixture.companion()).unwrap().mode() & 0o777,
            0o640
        );
        assert_eq!(
            Identity::of(&socket),
            Identity::of(&fs::symlink_metadata(fixture.path()).unwrap())
        );
        drop(claim);
        assert!(fixture.path().exists());
        drop(listener);
    }
    #[test]
    fn validate_detects_named_lock_replacement_and_does_not_delete_it() {
        let fixture = Fixture::new();
        let claim = Claim::acquire(&fixture.path()).unwrap();
        let old = fixture.0.join("original-lock");
        fs::rename(fixture.companion(), &old).unwrap();
        fs::write(fixture.companion(), []).unwrap();
        assert!(matches!(claim.validate(), Err(Error::UnsafeLock)));
        drop(claim);
        assert!(old.is_file());
        assert!(fixture.companion().is_file());
        // Unlinking a held lock is outside the participating-claim protocol;
        // validation detects it, without claiming inode-locks prevent it.
    }
    #[test]
    fn renamed_parent_keeps_original_claim_busy_and_invalidates_old_namespace() {
        let fixture = Fixture::new();
        let parent = fixture.0.join("p");
        fs::create_dir(&parent).unwrap();
        let claim = Claim::acquire(&parent.join("s")).unwrap();
        let renamed = fixture.0.join("renamed");
        fs::rename(&parent, &renamed).unwrap();
        assert!(claim.validate().is_err());
        assert!(matches!(
            Claim::acquire(&renamed.join("s")),
            Err(Error::Busy)
        ));
        drop(claim);
        Claim::acquire(&renamed.join("s"))
            .unwrap()
            .validate()
            .unwrap();
    }
    #[test]
    fn descriptors_are_close_on_exec_and_claim_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Claim>();
        let fixture = Fixture::new();
        let claim = Claim::acquire(&fixture.path()).unwrap();
        for file in [&claim.lock, &claim.parent] {
            assert_ne!(
                nix::fcntl::fcntl(file, nix::fcntl::FcntlArg::F_GETFD).unwrap()
                    & nix::libc::FD_CLOEXEC,
                0
            );
        }
    }
    #[test]
    fn unicode_filename_and_fixed_suffix_use_the_actual_filesystem() {
        let fixture = Fixture::new();
        let filename = std::ffi::OsStr::new("通话.sock");
        let path = fixture.0.join(filename);
        let claim = Claim::acquire(&path).unwrap();
        assert_eq!(claim.path(), path);
        assert!(fixture.0.join(companion_name(filename)).is_file());
    }
    #[test]
    fn raw_filename_suffix_preservation_is_a_pure_unix_bytes_rule() {
        // Darwin sandbox/APFS may reject these bytes: this tests only the
        // production naming function, not filesystem support for raw names.
        let filename = OsString::from_vec(b"s\xff".to_vec());
        assert_eq!(
            companion_name(&filename).as_bytes(),
            b"s\xff.switch-listener.lock"
        );
    }
    #[test]
    fn empty_directory_terminal_and_nul_paths_fail_before_creating_any_companion() {
        let fixture = Fixture::new();
        for path in [
            PathBuf::new(),
            PathBuf::from("/"),
            fixture.0.join("s/"),
            fixture.0.join("."),
            fixture.0.join(".."),
            fixture.0.join(OsString::from_vec(b"nul\0path".to_vec())),
        ] {
            assert!(matches!(Claim::acquire(&path), Err(Error::InvalidPath)));
        }
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    }

    #[test]
    fn probe_parser_accepts_only_empty_exit_one_as_confirmed_stale() {
        assert!(classify_probe(Some(1), b"", b"").is_ok());
        for output in [
            b"p123\nf4\n".as_slice(),
            b"p1\nf0\nf12\np456\nf9\n".as_slice(),
        ] {
            assert!(matches!(
                classify_probe(Some(0), output, b""),
                Err(Error::SocketInUse)
            ));
        }
        for (code, stdout, stderr) in [
            (Some(0), b"".as_slice(), b"".as_slice()),
            (None, b"", b""),
            (Some(2), b"", b""),
            (Some(1), b"\n", b""),
            (Some(1), b"", b"permission denied"),
            (Some(0), b"p1\nf4\n", b"warning"),
            (Some(0), b"p0\nf4\n", b""),
            (Some(0), b"f4\np1\n", b""),
            (Some(0), b"p1\nf4", b""),
            (Some(0), b"p1\n", b""),
            (Some(0), b"p1\nf4\np2\n", b""),
            (Some(0), b"p1\nfwat\n", b""),
            (Some(0), b"p1\nf4\nextra\n", b""),
            (Some(1), b"p1\nf4\n", b""),
        ] {
            assert!(matches!(
                classify_probe(code, stdout, stderr),
                Err(Error::ProbeUnconfirmed)
            ));
        }
        assert!(matches!(
            classify_probe(Some(1), &vec![0; PROBE_LIMIT + 1], b""),
            Err(Error::ProbeUnconfirmed)
        ));
        assert!(matches!(
            classify_probe(Some(1), b"", &vec![0; PROBE_LIMIT + 1]),
            Err(Error::ProbeUnconfirmed)
        ));
    }
    #[tokio::test]
    async fn actual_live_socket_probe_preserves_listener_and_reports_in_use() {
        let fixture = Fixture::new();
        let listener = UnixListener::bind(fixture.path()).unwrap();
        let before = fs::symlink_metadata(fixture.path()).unwrap();
        assert!(matches!(
            verify_stale_socket(&fixture.path()).await,
            Err(Error::SocketInUse)
        ));
        assert_eq!(
            Identity::of(&before),
            Identity::of(&fs::symlink_metadata(fixture.path()).unwrap())
        );
        drop(listener);
    }
    #[tokio::test]
    async fn actual_saturated_socket_probe_reports_in_use_even_when_darwin_refuses_connect() {
        let fixture = Fixture::new();
        let address = socket2::SockAddr::unix(fixture.path()).unwrap();
        let listener =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        listener.bind(&address).unwrap();
        listener.listen(1).unwrap();
        let mut clients = vec![];
        let mut saturated = false;
        for _ in 0..32 {
            let client =
                socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
            client.set_nonblocking(true).unwrap();
            match client.connect(&address) {
                Ok(()) => clients.push(client),
                Err(_) => {
                    saturated = true;
                    break;
                }
            }
        }
        assert!(
            saturated,
            "test must actually saturate the listener accept queue"
        );
        let before = fs::symlink_metadata(fixture.path()).unwrap();
        assert!(matches!(
            verify_stale_socket(&fixture.path()).await,
            Err(Error::SocketInUse)
        ));
        assert_eq!(
            Identity::of(&before),
            Identity::of(&fs::symlink_metadata(fixture.path()).unwrap())
        );
        drop(clients);
        drop(listener);
    }
    #[tokio::test]
    async fn actual_same_uid_stale_socket_is_confirmed_without_removing_evidence() {
        let fixture = Fixture::new();
        let listener = UnixListener::bind(fixture.path()).unwrap();
        drop(listener);
        let before = fs::symlink_metadata(fixture.path()).unwrap();
        verify_stale_socket(&fixture.path()).await.unwrap();
        assert_eq!(
            Identity::of(&before),
            Identity::of(&fs::symlink_metadata(fixture.path()).unwrap())
        );
    }
    #[tokio::test]
    async fn ownership_probe_refuses_nonsocket_symlink_and_other_uid_metadata() {
        let fixture = Fixture::new();
        fs::write(fixture.path(), b"preserve").unwrap();
        assert!(matches!(
            verify_stale_socket(&fixture.path()).await,
            Err(Error::ProbeUnconfirmed)
        ));
        fs::remove_file(fixture.path()).unwrap();
        let target = fixture.0.join("target");
        let listener = UnixListener::bind(&target).unwrap();
        symlink(&target, fixture.path()).unwrap();
        assert!(matches!(
            verify_stale_socket(&fixture.path()).await,
            Err(Error::ProbeUnconfirmed)
        ));
        let metadata = fs::symlink_metadata(&target).unwrap();
        assert!(!visible_socket(&metadata, metadata.uid().wrapping_add(1)));
        assert!(visible_socket(&metadata, metadata.uid()));
        drop(listener);
    }
    #[tokio::test]
    async fn subprocess_output_is_bounded_and_timeout_reaps_the_exact_child() {
        assert!(bounded_output(&vec![b'x'; PROBE_LIMIT][..]).await.is_ok());
        assert!(
            bounded_output(&vec![b'x'; PROBE_LIMIT + 1][..])
                .await
                .is_err()
        );
        let child = tokio::process::Command::new("/bin/sleep")
            .arg("10")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        assert!(matches!(
            collect_probe(child).await,
            Err(Error::ProbeUnconfirmed)
        ));
        assert_eq!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None),
            Err(nix::errno::Errno::ESRCH),
            "timed-out exact child must have been killed and waited"
        );
    }
}
