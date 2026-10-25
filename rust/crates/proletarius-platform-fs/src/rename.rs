//! Atomic publication relative to two actual held directories.
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

use nix::fcntl::AtFlags;
use nix::libc;
use nix::sys::stat::fstatat;

fn component(name: &str) -> io::Result<CString> {
    if name.is_empty() || name == "." || name == ".." || name.as_bytes().contains(&b'/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one directory entry",
        ));
    }
    CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "entry contains NUL"))
}

/// Move one directory without ever replacing an existing destination entry.
///
/// Both parent descriptors remain borrowed. Names are single components and
/// source symlinks are rejected. The kernel performs destination exclusion
/// atomically; an unsupported platform/filesystem returns an error, with no
/// check-then-rename fallback. This function does not fsync, grant ownership,
/// or prove that an entry still has a previously captured identity. Callers
/// must hold their resource/root rights, verify their exact source before and
/// after publication, and sync both parents before certifying durability.
pub fn rename_directory_noreplace(
    source_parent: &File,
    source_name: &str,
    destination_parent: &File,
    destination_name: &str,
) -> io::Result<()> {
    let source = component(source_name)?;
    let destination = component(destination_name)?;
    if !source_parent.metadata()?.is_dir() || !destination_parent.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "parent is not a directory",
        ));
    }
    let entry = fstatat(source_parent, source_name, AtFlags::AT_SYMLINK_NOFOLLOW)
        .map_err(io::Error::from)?;
    if entry.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source is not a directory",
        ));
    }
    #[cfg(target_os = "macos")]
    // SAFETY: both borrowed File descriptors stay live across this call, and
    // both owned C strings contain exactly one validated entry and a trailing
    // NUL. No pointer is retained. RENAME_EXCL makes the destination check and
    // namespace mutation one kernel operation, including symlink destinations.
    let result = unsafe {
        libc::renameatx_np(
            source_parent.as_raw_fd(),
            source.as_ptr(),
            destination_parent.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    // SAFETY: the descriptor/string lifetimes are identical to the macOS
    // branch; the variadic syscall arguments match renameat2's kernel ABI.
    // Use the syscall on both GNU and musl, whose libc need not export the
    // renameat2 wrapper. RENAME_NOREPLACE forbids replacing any target entry.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_parent.as_raw_fd(),
            source.as_ptr(),
            destination_parent.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (source, destination);
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename unavailable",
        ));
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Move one single-link regular file without replacing any destination entry.
///
/// Both names are single components relative to the actual borrowed parent
/// descriptors. The source is inspected without following symlinks and must
/// be a regular file with exactly one link. Destination exclusion is atomic
/// in the kernel, including when an existing destination is a symlink. No
/// copy, hard-link publication, or check-then-rename fallback is used.
///
/// This function neither locks the source namespace nor proves a previously
/// captured identity. The caller must hold its root/resource rights, exclude
/// source replacement or new links, verify the exact source before and after
/// the move, and sync both parents before certifying durability. Ownership,
/// permissions, RootProof, and recovery-ledger transitions remain the caller's
/// responsibility. An unsupported platform/filesystem returns an error.
pub fn rename_regular_noreplace(
    source_parent: &File,
    source_name: &str,
    destination_parent: &File,
    destination_name: &str,
) -> io::Result<()> {
    let source = component(source_name)?;
    let destination = component(destination_name)?;
    if !source_parent.metadata()?.is_dir() || !destination_parent.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "parent is not a directory",
        ));
    }
    let entry = fstatat(source_parent, source_name, AtFlags::AT_SYMLINK_NOFOLLOW)
        .map_err(io::Error::from)?;
    if entry.st_mode & libc::S_IFMT != libc::S_IFREG || entry.st_nlink != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source is not a single-link regular file",
        ));
    }
    #[cfg(target_os = "macos")]
    // SAFETY: the borrowed descriptors stay live and each owned C string is
    // one validated entry. RENAME_EXCL atomically refuses every existing target.
    let result = unsafe {
        libc::renameatx_np(
            source_parent.as_raw_fd(),
            source.as_ptr(),
            destination_parent.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    // SAFETY: descriptor/string lifetimes and the renameat2 kernel ABI match
    // the directory helper. No userspace destination-existence check is used.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_parent.as_raw_fd(),
            source.as_ptr(),
            destination_parent.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (source, destination);
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename unavailable",
        ));
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .unwrap()
                .join("build/runtime/iteration-592-atomic-publication")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn directory(&self, name: &str) -> File {
            fs::create_dir(self.0.join(name)).unwrap();
            File::open(self.0.join(name)).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn atomic_move_preserves_the_original_directory_and_record_inode_bytes() {
        let f = Fixture::new();
        let source = f.directory("source");
        let destination = f.directory("destination");
        fs::create_dir(f.0.join("source/active")).unwrap();
        fs::write(
            f.0.join("source/active/0001.json"),
            b"original unknown history",
        )
        .unwrap();
        let dir = File::open(f.0.join("source/active")).unwrap();
        let record = File::open(f.0.join("source/active/0001.json")).unwrap();
        let before_dir = dir.metadata().unwrap();
        let before_record = record.metadata().unwrap();
        rename_directory_noreplace(&source, "active", &destination, "journal").unwrap();
        assert!(!f.0.join("source/active").exists());
        let after_dir = fs::metadata(f.0.join("destination/journal")).unwrap();
        let after_record = fs::metadata(f.0.join("destination/journal/0001.json")).unwrap();
        assert_eq!(
            (before_dir.dev(), before_dir.ino()),
            (after_dir.dev(), after_dir.ino())
        );
        assert_eq!(
            (before_record.dev(), before_record.ino()),
            (after_record.dev(), after_record.ino())
        );
        assert_eq!(
            fs::read(f.0.join("destination/journal/0001.json")).unwrap(),
            b"original unknown history"
        );
        assert!(dir.metadata().unwrap().is_dir());
    }

    #[test]
    fn occupied_directory_file_and_dangling_symlink_are_never_replaced() {
        for kind in ["directory", "file", "symlink"] {
            let f = Fixture::new();
            let source = f.directory("source");
            let destination = f.directory("destination");
            fs::create_dir(f.0.join("source/active")).unwrap();
            fs::write(f.0.join("source/active/history"), b"source").unwrap();
            let target = f.0.join("destination/journal");
            match kind {
                "directory" => {
                    fs::create_dir(&target).unwrap();
                    fs::write(target.join("keep"), b"target").unwrap();
                }
                "file" => fs::write(&target, b"target").unwrap(),
                _ => symlink("missing-exact-target", &target).unwrap(),
            }
            let before = fs::symlink_metadata(&target).unwrap();
            assert!(
                rename_directory_noreplace(&source, "active", &destination, "journal").is_err()
            );
            let after = fs::symlink_metadata(&target).unwrap();
            assert_eq!(
                (before.dev(), before.ino(), before.mode()),
                (after.dev(), after.ino(), after.mode())
            );
            assert_eq!(
                fs::read(f.0.join("source/active/history")).unwrap(),
                b"source"
            );
            match kind {
                "directory" => assert_eq!(fs::read(target.join("keep")).unwrap(), b"target"),
                "file" => assert_eq!(fs::read(&target).unwrap(), b"target"),
                _ => assert_eq!(
                    fs::read_link(&target).unwrap(),
                    PathBuf::from("missing-exact-target")
                ),
            }
        }
    }

    #[test]
    fn only_one_concurrent_publication_wins_without_losing_the_other_source() {
        let f = Fixture::new();
        f.directory("destination");
        for name in ["a", "b"] {
            fs::create_dir(f.0.join(name)).unwrap();
            fs::write(f.0.join(name).join("owner"), name.as_bytes()).unwrap();
        }
        let barrier = Arc::new(Barrier::new(2));
        let jobs: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|name| {
                let source = File::open(&f.0).unwrap();
                let destination = File::open(f.0.join("destination")).unwrap();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (
                        name,
                        rename_directory_noreplace(&source, name, &destination, "journal"),
                    )
                })
            })
            .collect();
        let results: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
        assert_eq!(
            results.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        let winner = results.iter().find(|(_, result)| result.is_ok()).unwrap().0;
        let loser = results
            .iter()
            .find(|(_, result)| result.is_err())
            .unwrap()
            .0;
        assert_eq!(
            fs::read(f.0.join("destination/journal/owner")).unwrap(),
            winner.as_bytes()
        );
        assert_eq!(
            fs::read(f.0.join(loser).join("owner")).unwrap(),
            loser.as_bytes()
        );
        assert!(!f.0.join(winner).exists());
    }

    #[test]
    fn invalid_names_and_source_symlinks_fail_without_namespace_mutation() {
        let f = Fixture::new();
        let parent = f.directory("parent");
        fs::create_dir(f.0.join("parent/active")).unwrap();
        for name in ["", ".", "..", "../active", "active/child", "nul\0entry"] {
            assert!(rename_directory_noreplace(&parent, name, &parent, "other").is_err());
            assert!(rename_directory_noreplace(&parent, "active", &parent, name).is_err());
        }
        symlink("active", f.0.join("parent/link")).unwrap();
        assert!(rename_directory_noreplace(&parent, "link", &parent, "other").is_err());
        assert!(f.0.join("parent/active").is_dir());
        assert!(f.0.join("parent/link").is_symlink());
        assert!(!f.0.join("parent/other").exists());
    }

    #[test]
    fn renaming_the_parent_path_does_not_rebind_a_held_directory() {
        let f = Fixture::new();
        let held = f.directory("parent");
        let destination = f.directory("destination");
        fs::create_dir(f.0.join("parent/active")).unwrap();
        fs::write(f.0.join("parent/active/owner"), b"original").unwrap();
        fs::rename(f.0.join("parent"), f.0.join("original-parent")).unwrap();
        fs::create_dir(f.0.join("parent")).unwrap();
        fs::create_dir(f.0.join("parent/active")).unwrap();
        fs::write(f.0.join("parent/active/owner"), b"replacement").unwrap();
        rename_directory_noreplace(&held, "active", &destination, "journal").unwrap();
        assert_eq!(
            fs::read(f.0.join("destination/journal/owner")).unwrap(),
            b"original"
        );
        assert_eq!(
            fs::read(f.0.join("parent/active/owner")).unwrap(),
            b"replacement"
        );
    }

    #[test]
    fn regular_move_preserves_the_original_inode_raw_bytes_and_held_file() {
        use std::os::unix::fs::FileExt;

        let f = Fixture::new();
        let source = f.directory("source");
        let destination = f.directory("destination");
        let raw = b"original backup\0raw\n";
        fs::write(f.0.join("source/original.json"), raw).unwrap();
        let held = File::open(f.0.join("source/original.json")).unwrap();
        let before = held.metadata().unwrap();
        assert_eq!(before.nlink(), 1);
        rename_regular_noreplace(&source, "original.json", &destination, "retired.json").unwrap();
        assert!(fs::symlink_metadata(f.0.join("source/original.json")).is_err());
        let after = fs::symlink_metadata(f.0.join("destination/retired.json")).unwrap();
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.mode(),
                before.uid(),
                before.gid(),
                1
            ),
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.uid(),
                after.gid(),
                after.nlink()
            )
        );
        assert_eq!(fs::read(f.0.join("destination/retired.json")).unwrap(), raw);
        let mut held_raw = vec![0; raw.len()];
        assert_eq!(held.read_at(&mut held_raw, 0).unwrap(), raw.len());
        assert_eq!(held_raw.as_slice(), raw);
        assert_eq!(held.metadata().unwrap().ino(), before.ino());
    }

    #[test]
    fn regular_move_never_replaces_a_file_directory_or_dangling_symlink() {
        for kind in ["file", "directory", "symlink"] {
            let f = Fixture::new();
            let parent = f.directory("parent");
            let source = f.0.join("parent/original");
            let target = f.0.join("parent/retired");
            fs::write(&source, b"original raw").unwrap();
            match kind {
                "file" => fs::write(&target, b"foreign raw").unwrap(),
                "directory" => {
                    fs::create_dir(&target).unwrap();
                    fs::write(target.join("keep"), b"foreign child").unwrap();
                }
                _ => symlink("missing-exact-target", &target).unwrap(),
            }
            let before_source = fs::symlink_metadata(&source).unwrap();
            let before_target = fs::symlink_metadata(&target).unwrap();
            assert!(rename_regular_noreplace(&parent, "original", &parent, "retired").is_err());
            let after_source = fs::symlink_metadata(&source).unwrap();
            let after_target = fs::symlink_metadata(&target).unwrap();
            assert_eq!(
                (
                    before_source.dev(),
                    before_source.ino(),
                    before_source.nlink()
                ),
                (after_source.dev(), after_source.ino(), after_source.nlink())
            );
            assert_eq!(
                (
                    before_target.dev(),
                    before_target.ino(),
                    before_target.mode()
                ),
                (after_target.dev(), after_target.ino(), after_target.mode())
            );
            assert_eq!(fs::read(&source).unwrap(), b"original raw");
            match kind {
                "file" => assert_eq!(fs::read(&target).unwrap(), b"foreign raw"),
                "directory" => assert_eq!(fs::read(target.join("keep")).unwrap(), b"foreign child"),
                _ => assert_eq!(
                    fs::read_link(&target).unwrap(),
                    PathBuf::from("missing-exact-target")
                ),
            }
        }
    }

    #[test]
    fn regular_move_rejects_source_links_nonfiles_and_invalid_components() {
        let f = Fixture::new();
        let parent = f.directory("parent");
        fs::write(f.0.join("parent/original"), b"original raw").unwrap();
        let original = fs::metadata(f.0.join("parent/original")).unwrap();
        for name in ["", ".", "..", "../original", "original/child", "nul\0entry"] {
            assert_eq!(
                rename_regular_noreplace(&parent, name, &parent, "retired")
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                rename_regular_noreplace(&parent, "original", &parent, name)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        symlink("original", f.0.join("parent/link")).unwrap();
        fs::create_dir(f.0.join("parent/directory")).unwrap();
        for name in ["link", "directory"] {
            assert_eq!(
                rename_regular_noreplace(&parent, name, &parent, "retired")
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        fs::hard_link(f.0.join("parent/original"), f.0.join("parent/alias")).unwrap();
        for name in ["original", "alias"] {
            assert_eq!(
                rename_regular_noreplace(&parent, name, &parent, "retired")
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            let after = fs::symlink_metadata(f.0.join("parent").join(name)).unwrap();
            assert_eq!(
                (after.dev(), after.ino(), after.nlink()),
                (original.dev(), original.ino(), 2)
            );
            assert_eq!(
                fs::read(f.0.join("parent").join(name)).unwrap(),
                b"original raw"
            );
        }
        assert!(f.0.join("parent/link").is_symlink());
        assert!(f.0.join("parent/directory").is_dir());
        assert!(fs::symlink_metadata(f.0.join("parent/retired")).is_err());
    }

    #[test]
    fn regular_move_uses_the_held_parent_after_its_path_is_replaced() {
        let f = Fixture::new();
        let held = f.directory("parent");
        let destination = f.directory("destination");
        fs::write(f.0.join("parent/original"), b"original raw").unwrap();
        let original = fs::metadata(f.0.join("parent/original")).unwrap();
        fs::rename(f.0.join("parent"), f.0.join("old-parent")).unwrap();
        fs::create_dir(f.0.join("parent")).unwrap();
        fs::write(f.0.join("parent/original"), b"replacement raw").unwrap();
        rename_regular_noreplace(&held, "original", &destination, "retired").unwrap();
        let retired = fs::metadata(f.0.join("destination/retired")).unwrap();
        assert_eq!(
            (original.dev(), original.ino()),
            (retired.dev(), retired.ino())
        );
        assert_eq!(
            fs::read(f.0.join("destination/retired")).unwrap(),
            b"original raw"
        );
        assert_eq!(
            fs::read(f.0.join("parent/original")).unwrap(),
            b"replacement raw"
        );
        assert!(fs::symlink_metadata(f.0.join("old-parent/original")).is_err());
    }
}
