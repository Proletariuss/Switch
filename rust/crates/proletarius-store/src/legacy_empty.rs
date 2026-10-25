//! Opt-in migration of an exact, unenrolled legacy `[]`. Ordinary archive
//! decoding (including the legacy null/zero-byte compatibility) is unchanged.
use crate::{
    ArchiveStore, PRESENCE_CONTRACT, StoreError,
    raw::{CommitError, CommitStage},
};
use nix::{
    fcntl::{AtFlags, OFlag, openat, renameat},
    sys::stat::{Mode, fstatat},
    unistd::{UnlinkatFlags, unlinkat},
};
use std::{
    ffi::OsStr,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    sync::atomic::Ordering,
};

impl ArchiveStore {
    /// Replace only exact legacy `[]` without either durable-presence witness.
    /// The pure schema validator also checks an existing nonlegacy archive;
    /// missing and valid nonempty archives remain byte-for-byte unchanged.
    /// Enrollment precedes atomic replacement, so an interrupted migration can
    /// never be confused with a never-created ledger. An AfterRename error is
    /// not an acknowledgement; a fresh opener must validate the visible state.
    ///
    /// All decisions use the existing named writer lock and held directory.
    /// Opens do not follow final-component symlinks. Like other archive APIs,
    /// this requires writers to honor that lock; it does not exclude a hostile
    /// process replacing names between a final identity check and rename.
    pub fn replace_unenrolled_legacy_empty_array(
        &self,
        replacement: &[u8],
        validate: impl Fn(&[u8]) -> Result<(), StoreError>,
    ) -> Result<bool, CommitError> {
        let mut stage = CommitStage::BeforeRename;
        let result = (|| {
            self.validate_raw_bytes(replacement)?;
            validate(replacement)?;
            let parent = self.parent()?;
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(
                    nix::libc::O_DIRECTORY
                        | nix::libc::O_NOFOLLOW
                        | nix::libc::O_CLOEXEC
                        | nix::libc::O_NONBLOCK,
                )
                .open(parent)
                .map_err(StoreError::Io)?;
            check_directory(parent, &directory)?;
            let name = self.path.file_name().ok_or(StoreError::MissingFileName)?;
            let lock_path = self.lock_path()?;
            let lock_name = lock_path.file_name().ok_or(StoreError::MissingFileName)?;
            let marker_path = self.presence_path()?;
            let marker_name = marker_path.file_name().ok_or(StoreError::MissingFileName)?;
            let mut lock = open_regular(&directory, lock_name, true, true)?.ok_or_else(changed)?;
            #[cfg(test)]
            if let Some(path) = &self.after_lock_open {
                fs::write(path, b"opened").map_err(StoreError::Io)?;
            }
            lock.lock().map_err(StoreError::Io)?;
            check_directory(parent, &directory)?;
            check_named(
                &directory,
                lock_name,
                &lock,
                &lock.metadata().map_err(StoreError::Io)?,
            )?;
            let anchor = read_bounded(&mut lock, PRESENCE_CONTRACT.len() as u64 + 1)?;
            let anchored = match anchor.as_slice() {
                [] => false,
                bytes if bytes == PRESENCE_CONTRACT => true,
                _ => return Err(invalid_presence()),
            };
            let marker = read_marker(&directory, marker_name)?;
            if anchored && marker.is_none() {
                return Err(invalid_presence());
            }
            let Some(mut current) = open_regular(&directory, name, false, false)? else {
                check_directory(parent, &directory)?;
                check_named(
                    &directory,
                    lock_name,
                    &lock,
                    &lock.metadata().map_err(StoreError::Io)?,
                )?;
                if read_bounded(&mut lock, PRESENCE_CONTRACT.len() as u64 + 1)? != anchor
                    || read_marker(&directory, marker_name)? != marker
                {
                    return Err(invalid_presence());
                }
                return if anchored || marker.is_some() {
                    Err(invalid_presence())
                } else {
                    Ok(false)
                };
            };
            let identity = current.metadata().map_err(StoreError::Io)?;
            let bytes = read_bounded(&mut current, self.max_bytes)?;
            check_named(&directory, name, &current, &identity)?;
            if bytes != b"[]" {
                // No normalization of zero/null/whitespace-empty is permitted.
                // Preserve ordinary archive duplicate-key semantics for existing
                // data. The replacement alone uses the strict raw validator.
                let values: Vec<serde_json::Value> =
                    serde_json::from_slice(&bytes).map_err(StoreError::Json)?;
                if values.is_empty() {
                    return Err(StoreError::Configuration("invalid_store_schema".into()));
                }
                if values.len() > self.max_items {
                    return Err(StoreError::TooManyItems {
                        actual: values.len(),
                        limit: self.max_items,
                    });
                }
                if values.iter().any(|value| !value.is_object()) {
                    return Err(StoreError::ItemIsNotObject);
                }
                validate(&bytes)?;
                check_directory(parent, &directory)?;
                check_named(&directory, name, &current, &identity)?;
                check_named(
                    &directory,
                    lock_name,
                    &lock,
                    &lock.metadata().map_err(StoreError::Io)?,
                )?;
                if read_bounded(&mut lock, PRESENCE_CONTRACT.len() as u64 + 1)? != anchor
                    || read_marker(&directory, marker_name)? != marker
                {
                    return Err(invalid_presence());
                }
                return Ok(false);
            }
            if anchored || marker.is_some() {
                return Err(invalid_presence());
            }
            // Only this exact raw form, with both witnesses absent, is eligible.
            check_directory(parent, &directory)?;
            check_named(&directory, name, &current, &identity)?;
            check_named(
                &directory,
                lock_name,
                &lock,
                &lock.metadata().map_err(StoreError::Io)?,
            )?;
            lock.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
            if !read_bounded(&mut lock, PRESENCE_CONTRACT.len() as u64 + 1)?.is_empty()
                || read_marker(&directory, marker_name)?.is_some()
            {
                return Err(invalid_presence());
            }
            // Ordinary presence grammar, durable before any archive rename.
            checkpoint(0);
            let mut witness = create_regular(&directory, marker_name)?;
            witness
                .write_all(PRESENCE_CONTRACT)
                .and_then(|_| witness.sync_all())
                .map_err(StoreError::Io)?;
            self.commit_fault(7)?;
            directory.sync_all().map_err(StoreError::Io)?;
            lock.set_len(0).map_err(StoreError::Io)?;
            self.commit_fault(6)?;
            checkpoint(6);
            lock.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
            lock.write_all(PRESENCE_CONTRACT)
                .and_then(|_| lock.sync_all())
                .map_err(StoreError::Io)?;
            let temp_path = self.temp_path()?;
            let temp_name = temp_path.file_name().ok_or(StoreError::MissingFileName)?;
            let mut temp = create_regular(&directory, temp_name)?;
            let persisted = (|| {
                self.commit_fault(1)?;
                checkpoint(1);
                temp.write_all(replacement).map_err(StoreError::Io)?;
                self.commit_fault(2)?;
                checkpoint(2);
                temp.sync_all().map_err(StoreError::Io)?;
                self.commit_fault(3)?;
                checkpoint(3);
                check_directory(parent, &directory)?;
                check_named(&directory, name, &current, &identity)?;
                check_named(
                    &directory,
                    lock_name,
                    &lock,
                    &lock.metadata().map_err(StoreError::Io)?,
                )?;
                if read_marker(&directory, marker_name)?.as_deref() != Some(PRESENCE_CONTRACT) {
                    return Err(invalid_presence());
                }
                if read_bounded(&mut lock, PRESENCE_CONTRACT.len() as u64 + 1)? != PRESENCE_CONTRACT
                {
                    return Err(invalid_presence());
                }
                check_named(
                    &directory,
                    temp_name,
                    &temp,
                    &temp.metadata().map_err(StoreError::Io)?,
                )?;
                renameat(&directory, temp_name, &directory, name).map_err(io_error)?;
                stage = CommitStage::AfterRename;
                self.observed_present.store(true, Ordering::Release);
                self.commit_fault(4)?;
                checkpoint(4);
                #[cfg(test)]
                if self.fail_next_directory_sync.swap(false, Ordering::SeqCst) {
                    return Err(StoreError::Io(std::io::Error::other(
                        "injected directory sync failure after archive rename",
                    )));
                }
                directory.sync_all().map_err(StoreError::Io)?;
                self.commit_fault(5)?;
                checkpoint(5);
                check_directory(parent, &directory)?;
                check_named(
                    &directory,
                    name,
                    &temp,
                    &temp.metadata().map_err(StoreError::Io)?,
                )?;
                check_named(
                    &directory,
                    lock_name,
                    &lock,
                    &lock.metadata().map_err(StoreError::Io)?,
                )?;
                if read_marker(&directory, marker_name)?.as_deref() != Some(PRESENCE_CONTRACT)
                    || read_bounded(&mut lock, PRESENCE_CONTRACT.len() as u64 + 1)?
                        != PRESENCE_CONTRACT
                {
                    return Err(invalid_presence());
                }
                Ok(true)
            })();
            if persisted.is_err() {
                let _ = unlinkat(&directory, temp_name, UnlinkatFlags::NoRemoveDir);
            }
            persisted
        })();
        result.map_err(|source| CommitError { stage, source })
    }
}

fn io_error(error: nix::errno::Errno) -> StoreError {
    match error {
        nix::errno::Errno::ELOOP => StoreError::SymlinkRejected,
        _ => StoreError::Io(error.into()),
    }
}
fn changed() -> StoreError {
    StoreError::Configuration("archive_current_replaced".into())
}
fn invalid_presence() -> StoreError {
    StoreError::Configuration("invalid_durable_presence_store".into())
}
fn check_directory(path: &std::path::Path, directory: &File) -> Result<(), StoreError> {
    let held = directory.metadata().map_err(StoreError::Io)?;
    let named = fs::symlink_metadata(path).map_err(StoreError::Io)?;
    if !named.is_dir() || held.dev() != named.dev() || held.ino() != named.ino() {
        return Err(changed());
    }
    Ok(())
}
fn open_regular(
    directory: &File,
    name: &OsStr,
    writable: bool,
    create: bool,
) -> Result<Option<File>, StoreError> {
    let flags = (if writable {
        OFlag::O_RDWR
    } else {
        OFlag::O_RDONLY
    }) | OFlag::O_NOFOLLOW
        | OFlag::O_CLOEXEC
        | OFlag::O_NONBLOCK
        | if create {
            OFlag::O_CREAT
        } else {
            OFlag::empty()
        };
    let file = match openat(directory, name, flags, Mode::S_IRUSR | Mode::S_IWUSR) {
        Ok(fd) => File::from(fd),
        Err(nix::errno::Errno::ENOENT) if !create => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let metadata = file.metadata().map_err(StoreError::Io)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(StoreError::NotRegularFile);
    }
    check_named(directory, name, &file, &metadata)?;
    Ok(Some(file))
}
fn create_regular(directory: &File, name: &OsStr) -> Result<File, StoreError> {
    openat(
        directory,
        name,
        OFlag::O_WRONLY
            | OFlag::O_CREAT
            | OFlag::O_EXCL
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK,
        Mode::S_IRUSR | Mode::S_IWUSR,
    )
    .map(File::from)
    .map_err(io_error)
}
fn check_named(
    directory: &File,
    name: &OsStr,
    file: &File,
    original: &Metadata,
) -> Result<(), StoreError> {
    let named = fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(io_error)?;
    let held = file.metadata().map_err(StoreError::Io)?;
    // Darwin dev_t is signed; MetadataExt exposes its value as u64.
    #[allow(clippy::unnecessary_cast)]
    let named_device = named.st_dev as u64;
    if named.st_mode & nix::libc::S_IFMT != nix::libc::S_IFREG
        || named_device != held.dev()
        || named.st_ino != held.ino()
        || original.dev() != held.dev()
        || original.ino() != held.ino()
        || original.len() != held.len()
        || original.mtime() != held.mtime()
        || original.mtime_nsec() != held.mtime_nsec()
        || original.ctime() != held.ctime()
        || original.ctime_nsec() != held.ctime_nsec()
        || held.nlink() != 1
    {
        return Err(changed());
    }
    Ok(())
}
fn read_bounded(file: &mut File, limit: u64) -> Result<Vec<u8>, StoreError> {
    let length = file.metadata().map_err(StoreError::Io)?.len();
    if length > limit {
        return Err(StoreError::TooLarge {
            actual: length,
            limit,
        });
    }
    file.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(StoreError::Io)?;
    if bytes.len() as u64 > limit {
        return Err(StoreError::TooLarge {
            actual: bytes.len() as u64,
            limit,
        });
    }
    Ok(bytes)
}
fn read_marker(directory: &File, name: &OsStr) -> Result<Option<Vec<u8>>, StoreError> {
    let Some(mut marker) = open_regular(directory, name, false, false)? else {
        return Ok(None);
    };
    let identity = marker.metadata().map_err(StoreError::Io)?;
    let bytes = read_bounded(&mut marker, PRESENCE_CONTRACT.len() as u64 + 1)?;
    check_named(directory, name, &marker, &identity)?;
    if bytes != PRESENCE_CONTRACT {
        return Err(invalid_presence());
    }
    Ok(Some(bytes))
}

#[cfg(test)]
type CheckpointHook = Option<Box<dyn Fn(u64)>>;
#[cfg(test)]
thread_local! {
    static CHECKPOINT: std::cell::RefCell<CheckpointHook> = const { std::cell::RefCell::new(None) };
}
fn checkpoint(point: u64) {
    #[cfg(test)]
    CHECKPOINT.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook(point);
        }
    });
    let _ = point;
}

#[cfg(test)]
mod tests;
