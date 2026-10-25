//! Read-only SWF1 evidence, never recovery authority. The Swift caller holds
//! credential -> bootstrap -> historical writer locks and pins their names.
//! No QueueStore admission, create, sync, unlink, plaintext export or logging.
use nix::{
    errno::Errno,
    fcntl::{AtFlags, OFlag, openat},
    libc,
    sys::stat::{Mode, fstat, fstatat},
};
use proletarius_forwarding::{MAX_PLAINTEXT_BYTES, StateKey, StateLimits, open};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::Read,
    os::{
        fd::{AsRawFd, BorrowedFd},
        unix::fs::MetadataExt,
    },
};

const INVALID: i32 = -10;
const IO: i32 = -12;
const UNSAFE: i32 = -13;
const PANIC: i32 = -15;
const MAX_KEYS: usize = 18;
const SEED: &[u8] = br#"{"seen":{},"pending":{}}"#;

struct Evidence {
    file: File,
    metadata: Metadata,
    bytes: Vec<u8>,
}
fn private_file(metadata: &Metadata) -> bool {
    metadata.is_file()
        && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.nlink() == 1
        && metadata.mode() & 0o7777 == 0o600
}
fn unchanged(a: &Metadata, b: &Metadata) -> bool {
    private_file(b)
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn capture(directory: BorrowedFd<'_>, name: &str, limit: usize) -> Result<Option<Evidence>, i32> {
    let descriptor = match openat(
        directory,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::ENOENT) => return Ok(None),
        Err(_) => return Err(UNSAFE),
    };
    let mut file: File = descriptor.into();
    let metadata = file.metadata().map_err(|_| IO)?;
    if !private_file(&metadata) || metadata.len() > limit as u64 {
        return Err(UNSAFE);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(metadata.len() as usize + 1)
        .map_err(|_| IO)?;
    (&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| IO)?;
    if bytes.len() != metadata.len() as usize
        || !unchanged(&metadata, &file.metadata().map_err(|_| IO)?)
    {
        return Err(UNSAFE);
    }
    let evidence = Evidence {
        file,
        metadata,
        bytes,
    };
    check_name(directory, name, Some(&evidence))?;
    Ok(Some(evidence))
}
fn check_name(
    directory: BorrowedFd<'_>,
    name: &str,
    evidence: Option<&Evidence>,
) -> Result<(), i32> {
    let named = fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW);
    match (evidence, named) {
        (None, Err(Errno::ENOENT)) => Ok(()),
        (Some(e), Ok(n))
            if n.st_dev as u64 == e.metadata.dev()
                && n.st_ino == e.metadata.ino()
                && n.st_mode & libc::S_IFMT == libc::S_IFREG
                && n.st_uid == unsafe { libc::geteuid() }
                && n.st_mode & 0o7777 == 0o600
                && n.st_nlink == 1
                && unchanged(&e.metadata, &e.file.metadata().map_err(|_| IO)?) =>
        {
            Ok(())
        }
        _ => Err(UNSAFE),
    }
}
fn confirm(
    directory: BorrowedFd<'_>,
    name: &str,
    evidence: Option<&Evidence>,
    limit: usize,
) -> Result<(), i32> {
    check_name(directory, name, evidence)?;
    let again = capture(directory, name, limit)?;
    match (evidence, again.as_ref()) {
        (None, None) => Ok(()),
        (Some(a), Some(b)) if unchanged(&a.metadata, &b.metadata) && a.bytes == b.bytes => Ok(()),
        _ => Err(UNSAFE),
    }
}
struct Observation {
    directory: File,
    identity: (u64, u64, u32, u32),
    queue: Option<Evidence>,
    witness: Option<Evidence>,
}
impl Observation {
    fn confirm(&self) -> Result<(), i32> {
        let directory = unsafe { BorrowedFd::borrow_raw(self.directory.as_raw_fd()) };
        confirm(
            directory,
            "state.enc",
            self.queue.as_ref(),
            MAX_PLAINTEXT_BYTES + 32,
        )?;
        confirm(directory, "state.enc.keyid", self.witness.as_ref(), 40)?;
        let after = fstat(directory).map_err(|_| IO)?;
        if (
            after.st_dev as u64,
            after.st_ino,
            after.st_uid,
            after.st_mode as u32,
        ) != self.identity
        {
            return Err(UNSAFE);
        }
        Ok(())
    }
}
fn observe(directory_fd: i32, keys: &[u8]) -> Result<(i32, Vec<u8>, Observation), i32> {
    if directory_fd < 0 || !keys.len().is_multiple_of(32) || keys.len() / 32 > MAX_KEYS {
        return Err(INVALID);
    }
    let directory = unsafe { BorrowedFd::borrow_raw(directory_fd) };
    let dir = fstat(directory).map_err(|_| IO)?;
    if dir.st_mode & libc::S_IFMT != libc::S_IFDIR
        || dir.st_uid != unsafe { libc::geteuid() }
        || dir.st_mode & 0o022 != 0
    {
        return Err(UNSAFE);
    }
    let queue = capture(directory, "state.enc", MAX_PLAINTEXT_BYTES + 32)?;
    // Oversize witness is unsafe, not a candidate match. A short witness is
    // retained and reported malformed. No missing witness is synthesized.
    let witness = capture(directory, "state.enc.keyid", 40)?;
    let kind = match queue.as_ref().map(|e| e.bytes.as_slice()) {
        None => 0, // no seed: absence is evidence, never permission to create
        Some(w) if w.len() < 32 && (w.starts_with(b"SWF1") || b"SWF1".starts_with(w)) => 1,
        Some(w) if w.len() < 32 || !w.starts_with(b"SWF1") => 2,
        Some(_) => 3,
    };
    let mut output = Vec::with_capacity(keys.len() / 32);
    for raw in keys.as_chunks::<32>().0 {
        let witness_status = match witness.as_ref().map(|e| e.bytes.as_slice()) {
            None => 0,
            Some(w) if w.len() != 40 || !w.starts_with(b"SWFKID1\0") => 3,
            Some(w) => {
                let mut hash = Sha256::new();
                hash.update(b"Switch.SMSForwarding.QueueKey.v1\0");
                hash.update(raw);
                if w[8..] == hash.finalize()[..] { 1 } else { 2 }
            }
        };
        let matched = if kind == 3 {
            let key = StateKey::from_slice(raw).map_err(|_| INVALID)?;
            match open(&key, &queue.as_ref().unwrap().bytes, StateLimits::default()) {
                Ok(document) if document.as_bytes() == SEED => 1,
                Ok(_) => 2, // any other document remains nonempty/unknown
                Err(proletarius_forwarding::StateError::AuthenticationFailed) => 0,
                Err(_) => 3, // invalid/unsupported JSON is never a safe seed
            }
        } else {
            0
        };
        output.push(matched | (witness_status << 4));
    }
    let witness_kind = match witness.as_ref().map(|e| e.bytes.as_slice()) {
        None => 0,
        Some(w) if w.len() != 40 || !w.starts_with(b"SWFKID1\0") => 3,
        Some(_) => 4,
    };
    let observation = Observation {
        directory: directory.try_clone_to_owned().map_err(|_| IO)?.into(),
        identity: (
            dir.st_dev as u64,
            dir.st_ino,
            dir.st_uid,
            dir.st_mode as u32,
        ),
        queue,
        witness,
    };
    observation.confirm()?;
    Ok((kind | (witness_kind << 4), output, observation))
}

/// Return low nibble: 0 absent, 1 truncated SWF1, 2 unsupported, 3 full SWF1.
/// Return high nibble: witness 0 missing, 3 malformed, 4 unverified.
/// Each output byte: low nibble 0 no authentication, 1 exact empty seed,
/// 2 authenticated other JSON, 3 unusable JSON; high nibble witness 0 missing,
/// 1 matching, 2 mismatched, 3 malformed. No output is published on error.
/// This result expires when locks are released and must never authorize writes.
///
/// # Safety
/// `keys` contains `key_count * 32` readable bytes; `results` contains at least
/// `key_count` writable bytes; `observation` points to a writable handle slot.
/// Keys are never retained. Caller holds locks through confirm and destroy.
/// The returned opaque handle must be destroyed once, with no concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_forwarding_diagnose_queue(
    directory_fd: i32,
    keys: *const u8,
    key_count: usize,
    results: *mut u8,
    observation: *mut *mut std::ffi::c_void,
) -> i32 {
    if observation.is_null()
        || key_count > MAX_KEYS
        || (key_count > 0 && (keys.is_null() || results.is_null()))
    {
        return INVALID;
    }
    let raw = if key_count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(keys, key_count * 32) }
    };
    match std::panic::catch_unwind(|| observe(directory_fd, raw)) {
        Ok(Ok((kind, output, evidence))) => {
            unsafe { *observation = Box::into_raw(Box::new(evidence)).cast() };
            if !output.is_empty() {
                unsafe { std::ptr::copy_nonoverlapping(output.as_ptr(), results, output.len()) };
            }
            kind
        }
        Ok(Err(code)) => code,
        Err(_) => PANIC,
    }
}

/// Final byte, inode, metadata and namespace revalidation. The handle holds
/// original read-only descriptors until Swift has rechecked Keychain evidence.
/// # Safety
/// `observation` is a live handle from diagnose_queue, with no concurrent use.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_forwarding_diagnostic_confirm(
    observation: *const std::ffi::c_void,
) -> i32 {
    if observation.is_null() {
        return INVALID;
    }
    match std::panic::catch_unwind(|| unsafe { &*observation.cast::<Observation>() }.confirm()) {
        Ok(Ok(())) => 0,
        Ok(Err(code)) => code,
        Err(_) => PANIC,
    }
}
/// # Safety
/// Destroy a live handle exactly once, after all other calls have completed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_forwarding_diagnostic_destroy(observation: *mut std::ffi::c_void) {
    if !observation.is_null() {
        drop(unsafe { Box::from_raw(observation.cast::<Observation>()) });
    }
}
#[cfg(test)]
fn diagnose(directory_fd: i32, keys: &[u8]) -> Result<(i32, Vec<u8>), i32> {
    observe(directory_fd, keys).map(|(kind, output, _)| (kind, output))
}
#[cfg(test)]
mod tests;

// Separate explicit transaction ABI; diagnostics themselves remain read-only.
mod recovery;
