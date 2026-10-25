use crate::Error;
use crate::protocol::decode_hex;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::io::Read;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

static CWD: Mutex<()> = Mutex::new(());
#[derive(Clone, Copy, Eq, PartialEq)]
struct Id {
    dev: u64,
    ino: u64,
}
impl Id {
    fn of(m: &fs::Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
        }
    }
}
fn cstr(value: &OsStr) -> Result<CString, Error> {
    CString::new(value.as_bytes()).map_err(|_| Error::UnsafePath)
}
fn openat(parent: &File, name: &OsStr, flags: i32, mode: u32) -> Result<File, Error> {
    let name = cstr(name)?;
    // SAFETY: live borrowed parent fd and a bounded NUL-terminated basename.
    // Only the returned owned descriptor is adopted; failure adopts nothing.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(Error::Io);
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn directory(m: &fs::Metadata, private: bool) -> bool {
    let uid = unsafe { libc::geteuid() };
    m.is_dir()
        && (m.uid() == uid || m.uid() == 0)
        && m.mode() & 0o7000 == 0
        && if private {
            m.uid() == uid && m.mode() & 0o777 == 0o700
        } else {
            m.mode() & 0o022 == 0
        }
}
fn regular(m: &fs::Metadata) -> bool {
    m.is_file() && m.uid() == unsafe { libc::geteuid() } && m.nlink() == 1 && m.mode() & 0o7022 == 0
}
struct Link {
    parent: File,
    name: OsString,
    id: Id,
}
pub(crate) struct Directory {
    file: File,
    path: PathBuf,
    links: Vec<Link>,
    private: bool,
}
impl Directory {
    pub(crate) fn open(path: &Path, private: bool) -> Result<Self, Error> {
        if !path.is_absolute() {
            return Err(Error::UnsafePath);
        }
        let mut file = File::open("/")?;
        let mut links = Vec::new();
        let names = path
            .components()
            .filter_map(|part| match part {
                Component::RootDir => None,
                Component::Normal(n) => Some(Ok(n)),
                _ => Some(Err(Error::UnsafePath)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if names.is_empty() {
            return Err(Error::UnsafePath);
        }
        for name in names {
            let next = openat(
                &file,
                name,
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
                0,
            )?;
            let m = next.metadata()?;
            if !m.is_dir() {
                return Err(Error::UnsafePath);
            }
            links.push(Link {
                parent: file,
                name: name.to_owned(),
                id: Id::of(&m),
            });
            file = next;
        }
        let result = Self {
            file,
            path: path.to_owned(),
            links,
            private,
        };
        result.validate()?;
        Ok(result)
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if !directory(&self.file.metadata()?, self.private) {
            return Err(Error::UnsafePath);
        }
        for link in &self.links {
            let name = cstr(&link.name)?;
            // SAFETY: initialized stat is written by fstatat; never follows a
            // named symlink, and the original parent fd remains held.
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::fstatat(
                    link.parent.as_raw_fd(),
                    name.as_ptr(),
                    &mut stat,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
                || stat.st_mode & libc::S_IFMT != libc::S_IFDIR
                || (Id {
                    dev: stat.st_dev as u64,
                    ino: stat.st_ino as u64,
                }) != link.id
            {
                return Err(Error::UnsafePath);
            }
        }
        if Id::of(&fs::symlink_metadata(&self.path)?) != Id::of(&self.file.metadata()?) {
            return Err(Error::UnsafePath);
        }
        Ok(())
    }
    fn stat(&self, name: &OsStr) -> Result<libc::stat, Error> {
        let name = cstr(name)?;
        let mut result: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                &mut result,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(Error::Io);
        }
        Ok(result)
    }
    fn at<T>(&self, run: impl FnOnce(&Path) -> std::io::Result<T>) -> Result<T, Error> {
        self.validate()?;
        #[cfg(target_os = "linux")]
        {
            let path = PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()));
            let value = run(&path)?;
            self.validate()?;
            Ok(value)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _lock = CWD.lock().map_err(|_| Error::Io)?;
            let previous = File::open(".")?;
            struct Restore(File);
            impl Drop for Restore {
                fn drop(&mut self) {
                    unsafe {
                        libc::fchdir(self.0.as_raw_fd());
                    }
                }
            }
            let restore = Restore(previous);
            if unsafe { libc::fchdir(self.file.as_raw_fd()) } != 0 {
                return Err(Error::Io);
            }
            let value = run(Path::new("."));
            if unsafe { libc::fchdir(restore.0.as_raw_fd()) } != 0 {
                return Err(Error::Io);
            }
            drop(restore);
            self.validate()?;
            Ok(value?)
        }
    }
}
fn split(path: &Path, private: bool) -> Result<(Directory, OsString), Error> {
    let parent = path.parent().ok_or(Error::UnsafePath)?;
    let name = path.file_name().ok_or(Error::UnsafePath)?;
    if name.as_bytes().is_empty() || name.as_bytes().len() > 200 {
        return Err(Error::UnsafePath);
    }
    Ok((Directory::open(parent, private)?, name.to_owned()))
}
pub(crate) struct PinnedFile {
    directory: Directory,
    name: OsString,
    file: File,
    id: Id,
}
impl PinnedFile {
    fn open(path: &Path, private: bool) -> Result<Self, Error> {
        let (directory, name) = split(path, private)?;
        let before = directory.stat(&name)?;
        if before.st_mode & libc::S_IFMT != libc::S_IFREG || before.st_nlink != 1 {
            return Err(Error::UnsafePath);
        }
        let file = openat(
            &directory.file,
            &name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        )?;
        let id = Id::of(&file.metadata()?);
        if id
            != (Id {
                dev: before.st_dev as u64,
                ino: before.st_ino as u64,
            })
        {
            return Err(Error::UnsafePath);
        }
        let result = Self {
            directory,
            name,
            file,
            id,
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<(), Error> {
        self.directory.validate()?;
        let metadata = self.file.metadata()?;
        let stat = self.directory.stat(&self.name)?;
        if !regular(&metadata)
            || Id::of(&metadata) != self.id
            || stat.st_mode & libc::S_IFMT != libc::S_IFREG
            || stat.st_nlink != 1
            || (Id {
                dev: stat.st_dev as u64,
                ino: stat.st_ino as u64,
            }) != self.id
        {
            return Err(Error::UnsafePath);
        }
        Ok(())
    }
}
pub(crate) struct Key {
    pin: PinnedFile,
    bytes: [u8; 32],
    encoded: [u8; 64],
}
impl Key {
    pub(crate) fn open(path: &Path) -> Result<Self, Error> {
        let pin = PinnedFile::open(path, true)?;
        if pin.file.metadata()?.mode() & 0o777 != 0o600 || pin.file.metadata()?.len() != 64 {
            return Err(Error::UnsafePath);
        }
        let mut encoded = [0; 64];
        pin.file.read_exact_at(&mut encoded, 0)?;
        let text = std::str::from_utf8(&encoded).map_err(|_| Error::UnsafePath)?;
        let bytes = decode_hex(text).map_err(|_| Error::UnsafePath)?;
        let key = Self {
            pin,
            bytes,
            encoded,
        };
        key.validate()?;
        Ok(key)
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.pin.validate()?;
        if self.pin.file.metadata()?.len() != 64
            || self.pin.file.metadata()?.mode() & 0o777 != 0o600
        {
            return Err(Error::UnsafePath);
        }
        let mut bytes = [0; 64];
        self.pin.file.read_exact_at(&mut bytes, 0)?;
        let changed = bytes
            .iter()
            .zip(self.encoded)
            .fold(0u8, |a, (x, y)| a | (*x ^ y));
        bytes.fill(0);
        if changed != 0 {
            return Err(Error::UnsafePath);
        }
        Ok(())
    }
}
impl Drop for Key {
    fn drop(&mut self) {
        for b in &mut self.bytes {
            unsafe {
                std::ptr::write_volatile(b, 0);
            }
        }
        for b in &mut self.encoded {
            unsafe {
                std::ptr::write_volatile(b, 0);
            }
        }
    }
}

pub(crate) struct Socket {
    directory: Directory,
    name: OsString,
    id: Id,
}
impl Socket {
    pub(crate) fn bind(path: &Path) -> Result<(Self, UnixListener), Error> {
        let (directory, name) = split(path, true)?;
        if path.as_os_str().as_bytes().len() > 100 {
            return Err(Error::UnsafePath);
        }
        // Set the creation mode, never chmod an existing named socket. This
        // standalone server is single threaded; the lock also serializes tests.
        let _lock = CWD.lock().map_err(|_| Error::Io)?;
        let old = unsafe { libc::umask(0o177) };
        struct Mask(libc::mode_t);
        impl Drop for Mask {
            fn drop(&mut self) {
                unsafe {
                    libc::umask(self.0);
                }
            }
        }
        let mask = Mask(old);
        // On macOS at() uses CWD's lock too; bind directly through the held
        // directory below rather than acquiring the mutex recursively.
        #[cfg(target_os = "linux")]
        let listener = UnixListener::bind(
            PathBuf::from(format!("/proc/self/fd/{}", directory.file.as_raw_fd())).join(&name),
        );
        #[cfg(not(target_os = "linux"))]
        let listener = {
            let previous = File::open(".")?;
            if unsafe { libc::fchdir(directory.file.as_raw_fd()) } != 0 {
                return Err(Error::Io);
            }
            let bound = UnixListener::bind(Path::new(".").join(&name));
            if unsafe { libc::fchdir(previous.as_raw_fd()) } != 0 {
                return Err(Error::Io);
            }
            bound
        };
        drop(mask);
        drop(_lock);
        let listener = listener?;
        let stat = directory.stat(&name)?;
        let socket = Self {
            directory,
            name,
            id: Id {
                dev: stat.st_dev as u64,
                ino: stat.st_ino as u64,
            },
        };
        socket.validate()?;
        Ok((socket, listener))
    }
    pub(crate) fn existing(path: &Path) -> Result<Self, Error> {
        let (directory, name) = split(path, true)?;
        let stat = directory.stat(&name)?;
        let result = Self {
            directory,
            name,
            id: Id {
                dev: stat.st_dev as u64,
                ino: stat.st_ino as u64,
            },
        };
        result.validate()?;
        Ok(result)
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.directory.validate()?;
        let stat = self.directory.stat(&self.name)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
            || stat.st_mode & 0o7777 != 0o600
            || stat.st_nlink != 1
            || stat.st_uid != unsafe { libc::geteuid() }
            || (Id {
                dev: stat.st_dev as u64,
                ino: stat.st_ino as u64,
            }) != self.id
        {
            return Err(Error::UnsafePath);
        }
        Ok(())
    }
    pub(crate) fn connect(&self) -> Result<UnixStream, Error> {
        self.validate()?;
        let stream = self.directory.at(|root| {
            connect_timeout(&root.join(&self.name), std::time::Duration::from_secs(1))
        })?;
        self.validate()?;
        Ok(stream)
    }
}

pub(crate) struct Helper {
    pin: PinnedFile,
    path: PathBuf,
}
impl Helper {
    pub(crate) fn open(path: &Path) -> Result<Self, Error> {
        let pin = PinnedFile::open(path, false)?;
        let m = pin.file.metadata()?;
        if m.mode() & 0o100 == 0
            || m.mode() & 0o7000 != 0
            || m.len() == 0
            || m.len() > 64 * 1024 * 1024
        {
            return Err(Error::UnsafePath);
        }
        #[cfg(target_os = "linux")]
        {
            let mut magic = [0; 4];
            pin.file.read_exact_at(&mut magic, 0)?;
            if magic != *b"\x7fELF" {
                return Err(Error::UnsafePath);
            }
            let attr = CString::new("security.capability").map_err(|_| Error::UnsafePath)?;
            let size = unsafe {
                libc::fgetxattr(pin.file.as_raw_fd(), attr.as_ptr(), std::ptr::null_mut(), 0)
            };
            if size >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ENODATA) {
                return Err(Error::UnsafePath);
            }
        }
        Ok(Self {
            pin,
            path: path.to_owned(),
        })
    }
    pub(crate) fn command(&self) -> Result<Command, Error> {
        use std::os::unix::process::CommandExt;
        self.pin.validate()?;
        #[cfg(target_os = "linux")]
        let executable = PathBuf::from(format!("/proc/self/fd/{}", self.pin.file.as_raw_fd()));
        #[cfg(not(target_os = "linux"))]
        let executable = self.path.clone();
        let mut command = Command::new(executable);
        command
            .arg0(&self.path)
            .args(["--voice-route-session", "--verbose"])
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        Ok(command)
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.pin.validate()
    }
}

pub(crate) struct PidFile {
    directory: Directory,
    name: OsString,
    file: File,
    id: Id,
}
impl PidFile {
    pub(crate) fn reserve(path: &Path) -> Result<Self, Error> {
        let (directory, name) = split(path, true)?;
        let file = openat(
            &directory.file,
            &name,
            libc::O_WRONLY
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | libc::O_CLOEXEC,
            0o600,
        )?;
        let id = Id::of(&file.metadata()?);
        Ok(Self {
            directory,
            name,
            file,
            id,
        })
    }
    pub(crate) fn write(&mut self, pid: u32) -> Result<(), Error> {
        self.directory.validate()?;
        #[cfg(target_os = "linux")]
        let start = {
            let file = File::open(format!("/proc/{pid}/stat"))?;
            let mut bytes = Vec::new();
            file.take(4097).read_to_end(&mut bytes)?;
            if bytes.len() > 4096 {
                return Err(Error::Child);
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| Error::Child)?;
            let end = text.rfind(") ").ok_or(Error::Child)?;
            let fields: Vec<_> = text[end + 2..].split_ascii_whitespace().collect();
            fields
                .get(19)
                .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .ok_or(Error::Child)?
                .to_string()
        };
        // Host tests must never pretend this is Linux /proc birth identity.
        #[cfg(not(target_os = "linux"))]
        let start = "host-unavailable".to_owned();
        let stat = self.directory.stat(&self.name)?;
        if (Id {
            dev: stat.st_dev as u64,
            ino: stat.st_ino as u64,
        }) != self.id
            || !regular(&self.file.metadata()?)
        {
            return Err(Error::UnsafePath);
        }
        self.file.write_all(format!("{pid} {start}\n").as_bytes())?;
        self.file.sync_all()?;
        self.directory.file.sync_all()?;
        Ok(())
    }
}

// A full UDS backlog must not block the sole child owner. Adopt each fd once;
// connect/poll has one absolute deadline and never uses a network/proxy path.
fn connect_timeout(path: &Path, budget: std::time::Duration) -> std::io::Result<UnixStream> {
    use std::os::fd::OwnedFd;
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    #[cfg(not(target_os = "linux"))]
    {
        address.sun_len = std::mem::size_of::<libc::sockaddr_un>() as u8;
    }
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let deadline = std::time::Instant::now() + budget;
    let connected = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            &address as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if connected != 0 {
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EINPROGRESS) | Some(libc::EAGAIN)
        ) {
            return Err(error);
        }
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
            }
            let mut poll = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            let millis = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            let result = unsafe { libc::poll(&mut poll, 1, millis) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result == 0 {
                continue;
            }
            let mut error: libc::c_int = 0;
            let mut length = std::mem::size_of_val(&error) as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut error as *mut _ as *mut libc::c_void,
                    &mut length,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if error != 0 {
                return Err(std::io::Error::from_raw_os_error(error));
            }
            break;
        }
    }
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, 0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(UnixStream::from(fd))
}
