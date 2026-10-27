//! Metadata-only upgrade of the exact pair of idle locks created by old
//! startup/status code. This records lock provenance, never physical recovery.
//! The immutable namespace prevents a missing/partial receipt from restarting
//! migration; physical intent durably adds a revocation tombstone.
use crate::call_mode_backups::{RecoveryLockKind, acquire_bound_existing_recovery_lock};
use crate::daemon_lifetime::RootProof;
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{Mode, fstatat, mkdirat};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub(super) const DIRECTORY: &str = "module-legacy-locks-v1";
const RECEIPT: &str = "provenance.json";
const COMMITTED: &str = "committed";
const REVOKED: &str = "revoked";
const LOCKS: [&str; 2] = ["call-mode-operation.lock", ".module-adb-interface.lock"];
const ABSENT: [&str; 9] = [
    super::LEGACY_EXECUTION_PENDING,
    ".call-mode-pristine-v1",
    "module-backups",
    "voice-runtime",
    "voice-control",
    "module-recovery-v1",
    crate::call_mode_restore_journal::ACTIVE_NAME,
    crate::call_mode_restore_journal::ARCHIVE_NAME,
    "backup-lifecycle-v1",
];
const POLICY: &str = "idle-legacy-lock-pair-v1";

fn invalid() -> io::Error {
    io::Error::other("legacy lock provenance unavailable or changed")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    size: i64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
fn stamp(directory: &File, name: &str) -> io::Result<Option<Stamp>> {
    match fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(s) => Ok(Some(Stamp {
            device: s.st_dev as u64,
            inode: s.st_ino as u64,
            mode: s.st_mode as u32,
            uid: s.st_uid,
            gid: s.st_gid,
            links: s.st_nlink as u64,
            size: s.st_size,
            mtime: (s.st_mtime, s.st_mtime_nsec),
            ctime: (s.st_ctime, s.st_ctime_nsec),
        })),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn safe_regular(s: &Stamp) -> bool {
    s.mode & nix::libc::S_IFMT as u32 == nix::libc::S_IFREG as u32
        && s.mode & 0o7133 == 0
        && s.links == 1
        && s.uid == nix::unistd::geteuid().as_raw()
}
fn open_directory(parent: &File, name: &str) -> io::Result<File> {
    let before = stamp(parent, name)?.ok_or_else(invalid)?;
    if before.mode & nix::libc::S_IFMT as u32 != nix::libc::S_IFDIR as u32
        || before.mode & 0o7777 != 0o700
        || before.uid != nix::unistd::geteuid().as_raw()
    {
        return Err(invalid());
    }
    let file = File::from(openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let held = file.metadata()?;
    if held.dev() != before.device
        || held.ino() != before.inode
        || stamp(parent, name)? != Some(before)
    {
        return Err(invalid());
    }
    Ok(file)
}
fn empty_file(directory: &File, name: &str) -> io::Result<bool> {
    Ok(stamp(directory, name)?.is_some_and(|s| safe_regular(&s) && s.size == 0))
}
fn write_new(directory: &File, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::from(openat(
        directory,
        name,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?);
    file.write_all(bytes)?;
    file.sync_all()?;
    directory.sync_all()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    policy: String,
    root_device: u64,
    root_inode: u64,
    locks: [Stamp; 2],
    historical_recovery_confirmed: bool,
}
fn capture(directory: &File) -> io::Result<Provenance> {
    capture_with_runtime(directory, false)
}
fn capture_with_runtime(directory: &File, allow_runtime: bool) -> io::Result<Provenance> {
    for name in ABSENT {
        if allow_runtime && name == "voice-runtime" {
            continue;
        }
        if stamp(directory, name)?.is_some() {
            return Err(invalid());
        }
    }
    let first = stamp(directory, LOCKS[0])?.ok_or_else(invalid)?;
    let second = stamp(directory, LOCKS[1])?.ok_or_else(invalid)?;
    if [&first, &second]
        .into_iter()
        .any(|s| !safe_regular(s) || s.size != 0)
    {
        return Err(invalid());
    }
    let root = directory.metadata()?;
    Ok(Provenance {
        policy: POLICY.into(),
        root_device: root.dev(),
        root_inode: root.ino(),
        locks: [first, second],
        historical_recovery_confirmed: false,
    })
}

/// Covers only the two immutable lock slots and only while all other recovery
/// evidence is absent. A complete JSON file without the commit is insufficient.
pub(super) fn covered(directory: &File) -> io::Result<bool> {
    covered_download_source(directory, false)
}

/// The separate authenticated download receipt may explain only voice-runtime.
/// This still checks the original receipt, both exact locks and every other
/// absent slot, including all revocation and recovery evidence.
pub(super) fn covered_download_source(directory: &File, allow_runtime: bool) -> io::Result<bool> {
    let store_stamp = stamp(directory, DIRECTORY)?;
    if store_stamp.is_none() {
        return Ok(false);
    }
    let store = open_directory(directory, DIRECTORY)?;
    if stamp(&store, REVOKED)?.is_some() {
        return Ok(false);
    }
    let names = proletarius_platform_fs::directory_names(&store, 3)?;
    if names.len() != 2
        || !names.iter().any(|n| n == RECEIPT)
        || !names.iter().any(|n| n == COMMITTED)
        || !empty_file(&store, COMMITTED)?
    {
        return Err(invalid());
    }
    let before = stamp(&store, RECEIPT)?.ok_or_else(invalid)?;
    if !safe_regular(&before) || !(1..=4096).contains(&before.size) {
        return Err(invalid());
    }
    let file = File::from(openat(
        &store,
        RECEIPT,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let held = file.metadata()?;
    if held.dev() != before.device || held.ino() != before.inode {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    let receipt: Provenance = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if bytes.len() > 4096 || stamp(&store, RECEIPT)? != Some(before) {
        return Err(invalid());
    }
    // New journals/backup/runtime/control evidence always revoke this narrow
    // exception. They retain the existing recovery paths and classifications.
    let current = match capture_with_runtime(directory, allow_runtime) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    Ok(receipt == current
        && stamp(&store, REVOKED)?.is_none()
        && stamp(directory, DIRECTORY)? == store_stamp)
}

/// Called once before Gate construction and before any producer exists. The
/// genuine lifetime and both real kernel locks are mandatory; no hardware I/O.
pub(crate) fn migrate(root: &Path, proof: &RootProof) -> io::Result<bool> {
    let initial = proof
        .with_root(root, |directory| -> io::Result<Option<Provenance>> {
            if stamp(directory, DIRECTORY)?.is_some() {
                return Ok(None);
            }
            // Inapplicable roots are left entirely unchanged.
            Ok(capture(directory).ok())
        })
        .map_err(|_| invalid())??;
    let Some(initial) = initial else {
        return Ok(false);
    };
    let operation =
        acquire_bound_existing_recovery_lock(root, proof, RecoveryLockKind::Operation, false)?;
    let adb = match acquire_bound_existing_recovery_lock(
        root,
        proof,
        RecoveryLockKind::AdbInterface,
        false,
    ) {
        Ok(lock) => lock,
        Err(error) => {
            operation.release()?;
            return Err(error);
        }
    };
    let publication = proof
        .with_root(root, |directory| -> io::Result<()> {
            operation.validate()?;
            adb.validate()?;
            if capture(directory)? != initial || stamp(directory, DIRECTORY)?.is_some() {
                return Err(invalid());
            }
            mkdirat(directory, DIRECTORY, Mode::from_bits_truncate(0o700))?;
            directory.sync_all()?;
            let store = open_directory(directory, DIRECTORY)?;
            write_new(
                &store,
                RECEIPT,
                &serde_json::to_vec(&initial).map_err(|_| invalid())?,
            )
        })
        .map_err(|_| invalid());
    // Always attempt both real releases, even when publication failed.
    let adb_released = adb.release();
    let operation_released = operation.release();
    publication??;
    let adb_released = adb_released?;
    let operation_released = operation_released?;
    proof
        .with_root(root, |directory| -> io::Result<()> {
            adb_released.validate()?;
            operation_released.validate()?;
            if capture(directory)? != initial {
                return Err(invalid());
            }
            let store = open_directory(directory, DIRECTORY)?;
            write_new(&store, COMMITTED, &[])?;
            if !covered(directory)? {
                return Err(invalid());
            }
            adb_released.validate()?;
            operation_released.validate()
        })
        .map_err(|_| invalid())??;
    Ok(true)
}

/// Durable intent fence. The containing namespace remains, so removal of the
/// receipt or interruption of its first write cannot allow migration again.
pub(crate) fn invalidate(root: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(root)?;
    if stamp(&directory, DIRECTORY)?.is_none() {
        return Ok(());
    }
    let store = open_directory(&directory, DIRECTORY)?;
    if stamp(&store, REVOKED)?.is_some() {
        if !empty_file(&store, REVOKED)? {
            return Err(invalid());
        }
        return store.sync_all();
    }
    write_new(&store, REVOKED, &[])
}

/// Validate provenance as immutable input to a separate physical current-state
/// inspection. Revocation is retained, and this function never grants coverage.
pub(crate) fn legacy_current_source(root: &File) -> io::Result<bool> {
    let Some(before) = stamp(root, DIRECTORY)? else {
        return Ok(false);
    };
    let store = open_directory(root, DIRECTORY)?;
    let names = proletarius_platform_fs::directory_names(&store, 3)?;
    if names.len() < 2
        || names
            .iter()
            .any(|n| n != RECEIPT && n != COMMITTED && n != REVOKED)
        || !empty_file(&store, COMMITTED)?
    {
        return Err(invalid());
    }
    if stamp(&store, REVOKED)?.is_some() && !empty_file(&store, REVOKED)? {
        return Err(invalid());
    }
    let original = stamp(&store, RECEIPT)?.ok_or_else(invalid)?;
    if !safe_regular(&original) || !(1..=4096).contains(&original.size) {
        return Err(invalid());
    }
    let file = File::from(openat(
        &store,
        RECEIPT,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let held = file.metadata()?;
    if held.dev() != original.device || held.ino() != original.inode {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    let receipt: Provenance = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let root_meta = root.metadata()?;
    let locks = [
        stamp(root, LOCKS[0])?.ok_or_else(invalid)?,
        stamp(root, LOCKS[1])?.ok_or_else(invalid)?,
    ];
    if receipt.policy != POLICY
        || receipt.historical_recovery_confirmed
        || receipt.root_device != root_meta.dev()
        || receipt.root_inode != root_meta.ino()
        || receipt.locks != locks
        || locks.iter().any(|s| !safe_regular(s) || s.size != 0)
        || stamp(&store, RECEIPT)? != Some(original)
        || stamp(root, DIRECTORY)? != Some(before)
    {
        return Err(invalid());
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
