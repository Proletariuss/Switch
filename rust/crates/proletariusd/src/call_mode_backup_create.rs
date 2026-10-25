//! Historical storage fixture support; compiled only for tests.
//! Durable, non-overwriting creation of a safety backup through pinned fds.
//! This is a filesystem capability, never permission to touch a physical device.
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{FileStat, Mode, fstatat, mkdirat};
use nix::unistd::{UnlinkatFlags, linkat, unlinkat};
use std::ffi::OsString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity(u64, u64);
impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self(metadata.dev(), metadata.ino())
    }
    fn entry(metadata: &FileStat) -> Self {
        Self(metadata.st_dev as u64, metadata.st_ino as u64)
    }
}

fn unsafe_storage(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn safe_mode(mode: u32) -> bool {
    mode & 0o7022 == 0
}
fn check_directory(metadata: &Metadata) -> io::Result<()> {
    if metadata.is_dir()
        && metadata.uid() == nix::unistd::geteuid().as_raw()
        && safe_mode(metadata.mode())
    {
        Ok(())
    } else {
        Err(unsafe_storage(
            "backup directory ownership or permissions are unsafe",
        ))
    }
}
fn check_directory_entry(metadata: &FileStat) -> io::Result<()> {
    if metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFDIR
        && metadata.st_uid == nix::unistd::geteuid().as_raw()
        && safe_mode(metadata.st_mode as u32)
    {
        Ok(())
    } else {
        Err(unsafe_storage("backup directory entry is unsafe"))
    }
}

struct Directory {
    file: File,
    path: PathBuf,
    identity: Identity,
}
impl Directory {
    fn open(path: PathBuf) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(
                nix::libc::O_DIRECTORY
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NONBLOCK,
            )
            .open(&path)?;
        let identity = Identity::of(&file.metadata()?);
        let directory = Self {
            file,
            path,
            identity,
        };
        directory.validate()?;
        Ok(directory)
    }
    fn from_held(requested: &Path, held: &File) -> io::Result<Self> {
        let expected = held.metadata()?;
        check_directory(&expected)?;
        let file = File::from(openat(
            held,
            ".",
            OFlag::O_RDONLY
                | OFlag::O_DIRECTORY
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
            Mode::empty(),
        )?);
        let metadata = file.metadata()?;
        check_directory(&metadata)?;
        if Identity::of(&metadata) != Identity::of(&expected) {
            return Err(unsafe_storage("held backup root identity changed"));
        }
        let directory = Self {
            file,
            path: fs::canonicalize(requested)?,
            identity: Identity::of(&metadata),
        };
        directory.validate()?;
        Ok(directory)
    }
    fn validate(&self) -> io::Result<()> {
        for metadata in [self.file.metadata()?, fs::symlink_metadata(&self.path)?] {
            if !metadata.is_dir() || Identity::of(&metadata) != self.identity {
                return Err(unsafe_storage("backup directory namespace changed"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Point {
    RootParentSync,
    FileSync,
    BeforeCommit,
    AfterCommit,
    DirectorySync,
    RootSync,
    Complete,
    FinalFileSync,
}

fn sync(
    file: &File,
    point: Point,
    hook: &mut impl FnMut(Point) -> io::Result<()>,
) -> io::Result<()> {
    // A hook can fail, never substitute a successful sync. Production always
    // executes the real syscall, including every new ancestor directory entry.
    hook(point)?;
    file.sync_all()
}

fn pin_root(
    requested: &Path,
    hook: &mut impl FnMut(Point) -> io::Result<()>,
) -> io::Result<(Directory, Vec<Directory>)> {
    let mut ancestor = requested.to_path_buf();
    let mut missing: Vec<OsString> = Vec::new();
    loop {
        match fs::symlink_metadata(&ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| unsafe_storage("invalid backup root"))?;
                missing.push(name.to_os_string());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| unsafe_storage("invalid backup root"))?
                    .to_path_buf();
            }
            Err(error) => return Err(error),
        }
    }
    let expected = Identity::of(&fs::metadata(&ancestor)?);
    let canonical = fs::canonicalize(&ancestor)?;
    let mut parent = Directory::open(canonical)?;
    if parent.identity != expected {
        return Err(unsafe_storage("backup root ancestor changed while opening"));
    }
    let mut pinned_ancestors = Vec::new();
    for name in missing.into_iter().rev() {
        parent.validate()?;
        let created = match mkdirat(&parent.file, name.as_os_str(), Mode::S_IRWXU) {
            Ok(()) => true,
            Err(nix::errno::Errno::EEXIST) => false,
            Err(error) => return Err(error.into()),
        };
        let named = fstatat(&parent.file, name.as_os_str(), AtFlags::AT_SYMLINK_NOFOLLOW)?;
        check_directory_entry(&named)?;
        let file = File::from(openat(
            &parent.file,
            name.as_os_str(),
            OFlag::O_RDONLY
                | OFlag::O_DIRECTORY
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
            Mode::empty(),
        )?);
        let metadata = file.metadata()?;
        check_directory(&metadata)?;
        if Identity::of(&metadata) != Identity::entry(&named) {
            return Err(unsafe_storage(
                "new backup root entry changed while opening",
            ));
        }
        let child = Directory {
            file,
            path: parent.path.join(&name),
            identity: Identity::of(&metadata),
        };
        child.validate()?;
        if created {
            sync(&parent.file, Point::RootParentSync, hook)?;
            parent.validate()?;
            child.validate()?;
        }
        pinned_ancestors.push(parent);
        parent = child;
    }
    check_directory(&parent.file.metadata()?)?;
    Ok((parent, pinned_ancestors))
}

struct Namespace {
    root: Directory,
    ancestors: Vec<Directory>,
    requested: PathBuf,
    backups: File,
    backup_identity: Identity,
}
impl Namespace {
    fn open(
        root: &Path,
        held: Option<&File>,
        hook: &mut impl FnMut(Point) -> io::Result<()>,
    ) -> io::Result<Self> {
        if root.as_os_str().is_empty() {
            return Err(unsafe_storage("empty backup root"));
        }
        let requested = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir()?.join(root)
        };
        let (root, ancestors) = if let Some(held) = held {
            (Directory::from_held(&requested, held)?, Vec::new())
        } else {
            pin_root(&requested, hook)?
        };
        root.validate()?;
        if Identity::of(&fs::metadata(&requested)?) != root.identity {
            return Err(unsafe_storage(
                "requested backup root changed before admission",
            ));
        }
        // mkdirat never changes an existing directory or repairs permissions.
        match mkdirat(&root.file, super::BACKUP_DIRECTORY, Mode::S_IRWXU) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
            Err(error) => return Err(error.into()),
        }
        let named = fstatat(
            &root.file,
            super::BACKUP_DIRECTORY,
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )?;
        check_directory_entry(&named)?;
        let backups = File::from(openat(
            &root.file,
            super::BACKUP_DIRECTORY,
            OFlag::O_RDONLY
                | OFlag::O_DIRECTORY
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
            Mode::empty(),
        )?);
        let metadata = backups.metadata()?;
        check_directory(&metadata)?;
        if Identity::of(&metadata) != Identity::entry(&named) {
            return Err(unsafe_storage(
                "backup directory entry changed while opening",
            ));
        }
        let namespace = Self {
            root,
            ancestors,
            requested,
            backups,
            backup_identity: Identity::of(&metadata),
        };
        namespace.validate()?;
        Ok(namespace)
    }
    fn validate(&self) -> io::Result<()> {
        for ancestor in &self.ancestors {
            ancestor.validate()?;
        }
        self.root.validate()?;
        let requested = fs::metadata(&self.requested)?;
        check_directory(&requested)?;
        check_directory(&self.root.file.metadata()?)?;
        if Identity::of(&requested) != self.root.identity {
            return Err(unsafe_storage("requested backup root changed"));
        }
        let held = self.backups.metadata()?;
        check_directory(&held)?;
        let named = fstatat(
            &self.root.file,
            super::BACKUP_DIRECTORY,
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )?;
        if Identity::of(&held) != self.backup_identity
            || Identity::entry(&named) != self.backup_identity
            || named.st_mode & nix::libc::S_IFMT != nix::libc::S_IFDIR
            || named.st_uid != nix::unistd::geteuid().as_raw()
            || !safe_mode(named.st_mode as u32)
        {
            return Err(unsafe_storage("backup subdirectory namespace changed"));
        }
        Ok(())
    }
}

struct Temporary<'a> {
    directory: &'a File,
    name: String,
    file: File,
    identity: Identity,
    keep: bool,
    removed: bool,
}
impl<'a> Temporary<'a> {
    fn create(directory: &'a File) -> io::Result<Self> {
        for _ in 0..100 {
            let mut random = [0u8; 12];
            getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
            let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            let name = format!(".usb-create-{suffix}.tmp");
            match openat(
                directory,
                name.as_str(),
                OFlag::O_RDWR
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC
                    | OFlag::O_NONBLOCK,
                Mode::S_IRUSR | Mode::S_IWUSR,
            ) {
                Ok(file) => {
                    let file = File::from(file);
                    let identity = Identity::of(&file.metadata()?);
                    return Ok(Self {
                        directory,
                        name,
                        file,
                        identity,
                        keep: false,
                        removed: false,
                    });
                }
                Err(nix::errno::Errno::EEXIST) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::other(
            "cannot create unique backup temporary file",
        ))
    }
    fn check(&self, name: &str, length: usize, links: u64) -> io::Result<()> {
        let held = self.file.metadata()?;
        let named = fstatat(self.directory, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
        if !held.is_file()
            || Identity::of(&held) != self.identity
            || Identity::entry(&named) != self.identity
            || held.nlink() != links
            || named.st_nlink as u64 != links
            || held.len() != length as u64
            || named.st_size as u64 != length as u64
            || held.uid() != nix::unistd::geteuid().as_raw()
            || named.st_uid != held.uid()
            || held.mode() & 0o7777 != 0o600
            || named.st_mode & 0o7777 != 0o600
        {
            return Err(unsafe_storage("backup file namespace or shape changed"));
        }
        Ok(())
    }
    fn check_payload(&self, name: &str, payload: &[u8], links: u64) -> io::Result<()> {
        self.check(name, payload.len(), links)?;
        let before = self.file.metadata()?;
        let mut bytes = vec![0u8; payload.len()];
        let mut offset = 0;
        while offset < bytes.len() {
            match self.file.read_at(&mut bytes[offset..], offset as u64) {
                Ok(0) => return Err(unsafe_storage("backup payload became truncated")),
                Ok(count) => offset += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        let after = self.file.metadata()?;
        self.check(name, payload.len(), links)?;
        if bytes != payload
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(unsafe_storage("backup payload changed during creation"));
        }
        Ok(())
    }
    fn remove_own(&mut self) -> io::Result<()> {
        let named = fstatat(
            self.directory,
            self.name.as_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )?;
        if Identity::entry(&named) != self.identity
            || named.st_mode & nix::libc::S_IFMT != nix::libc::S_IFREG
        {
            return Err(unsafe_storage("backup temporary name was replaced"));
        }
        unlinkat(
            self.directory,
            self.name.as_str(),
            UnlinkatFlags::NoRemoveDir,
        )?;
        self.removed = true;
        Ok(())
    }
}
impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if !self.keep && !self.removed {
            let _ = self.remove_own();
        }
    }
}

pub(super) fn write(root: &Path, id: &str, payload: &[u8]) -> io::Result<()> {
    write_with_hook(root, id, payload, |_| Ok(()))
}
pub(super) fn create_bound(
    root: &Path,
    held_root: &File,
    id: &str,
    payload: &[u8],
) -> io::Result<()> {
    write_with_root(root, Some(held_root), id, payload, |_| Ok(()))
}
fn write_with_hook(
    root: &Path,
    id: &str,
    payload: &[u8],
    hook: impl FnMut(Point) -> io::Result<()>,
) -> io::Result<()> {
    write_with_root(root, None, id, payload, hook)
}
fn write_with_root(
    root: &Path,
    held_root: Option<&File>,
    id: &str,
    payload: &[u8],
    mut hook: impl FnMut(Point) -> io::Result<()>,
) -> io::Result<()> {
    if !super::valid_backup_id(id) || payload.len() as u64 > super::MAX_BACKUP_BYTES {
        return Err(unsafe_storage("invalid created backup name"));
    }
    let namespace = Namespace::open(root, held_root, &mut hook)?;
    let mut temporary = Temporary::create(&namespace.backups)?;
    temporary.check(&temporary.name, 0, 1)?;
    temporary.file.write_all(payload)?;
    sync(&temporary.file, Point::FileSync, &mut hook)?;
    hook(Point::BeforeCommit)?;
    namespace.validate()?;
    temporary.check_payload(&temporary.name, payload, 1)?;
    // Same-directory linkat is atomic and fails if *any* destination exists.
    // Unlike rename, it cannot overwrite a backup or a replaced symlink.
    linkat(
        &namespace.backups,
        temporary.name.as_str(),
        &namespace.backups,
        id,
        AtFlags::empty(),
    )?;
    // From here uncertainty must retain the owned evidence, never remove final.
    temporary.keep = true;
    hook(Point::AfterCommit)?;
    namespace.validate()?;
    temporary.check_payload(id, payload, 2)?;
    temporary.check(&temporary.name, payload.len(), 2)?;
    temporary.remove_own()?;
    temporary.check_payload(id, payload, 1)?;
    sync(&namespace.backups, Point::DirectorySync, &mut hook)?;
    namespace.validate()?;
    temporary.check_payload(id, payload, 1)?;
    sync(&namespace.root.file, Point::RootSync, &mut hook)?;
    hook(Point::Complete)?;
    namespace.validate()?;
    temporary.check_payload(id, payload, 1)?;
    sync(&temporary.file, Point::FinalFileSync, &mut hook)?;
    namespace.validate()?;
    temporary.check_payload(id, payload, 1)?;
    Ok(())
}

#[cfg(test)]
#[path = "call_mode_backup_create_tests.rs"]
mod tests;
