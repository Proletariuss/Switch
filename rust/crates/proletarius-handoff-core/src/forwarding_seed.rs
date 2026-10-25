//! Rust-owned first SWF1 publication for the GUI's credential bootstrap.
//! The caller must already hold the fixed credential lock, bootstrap flock,
//! and historical state.enc.lock flock. This function never replaces a file.
use nix::{
    errno::Errno,
    fcntl::{OFlag, openat},
    libc,
    sys::stat::{Mode, fchmod, fstat, fstatat},
};
use proletarius_forwarding::{StateKey, seal_empty_queue};
use std::{fs::File, io::Write, os::fd::BorrowedFd, os::unix::fs::MetadataExt};
use zeroize::Zeroizing;

const INVALID: i32 = -10;
const EXISTS: i32 = -11;
const IO: i32 = -12;
const UNSAFE: i32 = -13;
const CRYPTO: i32 = -14;
const PANIC: i32 = -15;

#[cfg(feature = "fault-injection")]
fn fail_after(phase: &str) -> bool {
    std::env::var("SWITCH_FORWARDING_SEED_TEST_FAULT").is_ok_and(|value| value == phase)
}

#[cfg(not(feature = "fault-injection"))]
fn fail_after(_phase: &str) -> bool {
    false
}

fn publish(directory_fd: i32, raw: &[u8]) -> Result<(), i32> {
    if directory_fd < 0 || raw.len() != 32 {
        return Err(INVALID);
    }
    // SAFETY: Swift owns and retains this descriptor throughout the call.
    let directory = unsafe { BorrowedFd::borrow_raw(directory_fd) };
    let dir = fstat(directory).map_err(|_| IO)?;
    if dir.st_mode & libc::S_IFMT != libc::S_IFDIR
        || dir.st_uid != unsafe { libc::geteuid() }
        || dir.st_mode & 0o022 != 0
    {
        return Err(UNSAFE);
    }
    let mut copy = Zeroizing::new([0u8; 32]);
    copy.copy_from_slice(raw);
    let key = StateKey::from_slice(copy.as_ref()).map_err(|_| INVALID)?;
    let wire = seal_empty_queue(&key).map_err(|_| CRYPTO)?;
    let descriptor = openat(
        directory,
        "state.enc",
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|error| if error == Errno::EEXIST { EXISTS } else { IO })?;
    let mut file: File = descriptor.into();
    fchmod(&file, Mode::from_bits_truncate(0o600)).map_err(|_| IO)?;
    let opened = file.metadata().map_err(|_| IO)?;
    if !opened.is_file()
        || opened.uid() != unsafe { libc::geteuid() }
        || opened.nlink() != 1
        || opened.mode() & 0o7777 != 0o600
    {
        return Err(UNSAFE);
    }
    if fail_after("partial") {
        // Test archive only: model a torn O_EXCL write. Never unlink it.
        file.write_all(b"SWF1").map_err(|_| IO)?;
        file.sync_all().map_err(|_| IO)?;
        if unsafe { libc::fsync(directory_fd) } != 0 {
            return Err(IO);
        }
        return Err(IO);
    }
    // A short write or failed sync leaves the created name in place. Recovery
    // must inspect it; automatic deletion could expose an old-key admission.
    file.write_all(&wire).map_err(|_| IO)?;
    file.sync_all().map_err(|_| IO)?;
    if fail_after("file-sync") {
        return Err(IO);
    }
    if unsafe { libc::fsync(directory_fd) } != 0 {
        return Err(IO);
    }
    if fail_after("directory-sync") {
        return Err(IO);
    }
    let named = fstatat(
        directory,
        "state.enc",
        nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(|_| UNSAFE)?;
    if named.st_mode & libc::S_IFMT != libc::S_IFREG
        || named.st_dev as u64 != opened.dev()
        || named.st_ino != opened.ino()
        || named.st_uid != unsafe { libc::geteuid() }
        || named.st_nlink != 1
        || named.st_mode & 0o7777 != 0o600
    {
        return Err(UNSAFE);
    }
    Ok(())
}

/// Return 0 only after the seed file and directory are synced. Every error
/// leaves Keychain publication to the caller's fail-closed path.
#[unsafe(no_mangle)]
pub extern "C" fn switch_forwarding_publish_empty_queue(
    directory_fd: i32,
    key_bytes: *const u8,
    key_length: usize,
) -> i32 {
    if key_bytes.is_null() || key_length != 32 {
        return INVALID;
    }
    // SAFETY: the Swift caller passes a live 32-byte Data buffer for this call.
    // No pointer is retained; the private copy is zeroized by publish().
    let raw = unsafe { std::slice::from_raw_parts(key_bytes, key_length) };
    match std::panic::catch_unwind(|| publish(directory_fd, raw)) {
        Ok(Ok(())) => 0,
        Ok(Err(status)) => status,
        Err(_) => PANIC,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proletarius_forwarding::{StateError, StateLimits, open};
    use std::{
        fs,
        os::fd::AsRawFd,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
        path::Path,
    };

    #[test]
    fn rust_seed_authenticates_with_new_key_and_refuses_replacement() {
        let report = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap();
        let parent = report.join("build/tmp/rust-seed-tests");
        fs::create_dir_all(&parent).unwrap();
        let root = parent.join(format!(
            "seed-{}-{}",
            std::process::id(),
            super::super::uuid().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&root)
            .unwrap();
        let key = [7u8; 32];
        assert_eq!(
            switch_forwarding_publish_empty_queue(directory.as_raw_fd(), key.as_ptr(), 32),
            0
        );
        let wire = fs::read(root.join("state.enc")).unwrap();
        assert_eq!(wire.len(), 56);
        let good = StateKey::from_slice(&key).unwrap();
        let document = open(&good, &wire, StateLimits::default()).unwrap();
        assert_eq!(document.as_bytes(), br#"{"seen":{},"pending":{}}"#);
        let old = StateKey::from_slice(&[3u8; 32]).unwrap();
        assert!(matches!(
            open(&old, &wire, StateLimits::default()),
            Err(StateError::AuthenticationFailed)
        ));
        assert_eq!(
            switch_forwarding_publish_empty_queue(directory.as_raw_fd(), key.as_ptr(), 32),
            EXISTS
        );
        assert_eq!(fs::read(root.join("state.enc")).unwrap(), wire);
        assert_eq!(
            switch_forwarding_publish_empty_queue(directory.as_raw_fd(), std::ptr::null(), 32),
            INVALID
        );
        assert_eq!(fs::read(root.join("state.enc")).unwrap(), wire);
        drop(directory);
        fs::remove_dir_all(root).unwrap();
    }
}
