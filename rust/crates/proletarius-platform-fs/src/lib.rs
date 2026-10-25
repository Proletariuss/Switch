//! A small Unix FFI boundary for bounded names read from an already held directory.
//!
//! This crate does not select paths, traverse children, establish a stable
//! snapshot, or grant authority from a directory entry's name or ordering.
#![cfg(unix)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::ptr::NonNull;

use nix::errno::Errno;
use nix::fcntl::{OFlag, openat};
use nix::libc;
use nix::sys::stat::Mode;

mod process_identity;
pub use process_identity::process_identity;
pub use process_identity::{ProcessStartIdentity, process_start_identity};

mod rename;
pub use rename::{rename_directory_noreplace, rename_regular_noreplace};

/// Read at most `limit` non-dot names from an already open directory.
///
/// Opens `.` relative to `directory` to obtain an independent open-file
/// description and cursor. The caller's descriptor is borrowed, never adopted
/// or rewound. Names retain their original Unix bytes, are not sorted, and
/// neither symlinks nor child directories are opened. Only `.` and `..` are
/// filtered. Zero permits an empty directory; an additional name, duplicate,
/// invalid entry or read/close error fails the whole read instead of returning
/// a truncated list. At most `limit + 3` readdir calls are made (including dots
/// and EOF); an overflowing limit is rejected before opening anything.
///
/// Enumeration is not a snapshot. A security-sensitive caller must separately
/// check the held directory's identity and mutation stamps before and after.
pub fn directory_names(directory: &File, limit: usize) -> io::Result<Vec<OsString>> {
    let transfers = limit
        .checked_add(3)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "directory limit overflows"))?;
    let descriptor = openat(
        directory,
        ".",
        OFlag::O_RDONLY
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let mut stream = DirectoryStream::from_file(File::from(descriptor))?;
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..transfers {
        let Some(name) = stream.next_name()? else {
            stream.close()?;
            return Ok(names);
        };
        if name == "." || name == ".." {
            continue;
        }
        if names.len() == limit {
            return Err(invalid("directory entry limit exceeded"));
        }
        if !seen.insert(name.clone()) {
            return Err(invalid("directory entry repeated during enumeration"));
        }
        names.push(name);
    }
    Err(invalid(
        "directory enumeration did not finish within its bound",
    ))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn name_from_record_bytes(bytes: &[u8]) -> io::Result<OsString> {
    let length = bytes
        .iter()
        .position(|byte| *byte == 0)
        .filter(|length| *length != 0)
        .ok_or_else(|| invalid("directory name has no bounded terminator"))?;
    Ok(OsString::from_vec(bytes[..length].to_vec()))
}

struct DirectoryStream {
    pointer: Option<NonNull<libc::DIR>>,
}
impl DirectoryStream {
    fn from_file(file: File) -> io::Result<Self> {
        let descriptor: OwnedFd = file.into();
        // SAFETY: descriptor is owned and live for the call. fdopendir adopts
        // it only on success. On failure we still own and close it below.
        let pointer = unsafe { libc::fdopendir(descriptor.as_raw_fd()) };
        let Some(pointer) = NonNull::new(pointer) else {
            // Capture errno before close can replace it. Safe nix close consumes
            // our OwnedFd exactly once; never retry a possibly reused raw fd.
            let error = io::Error::last_os_error();
            let closed = nix::unistd::close(descriptor);
            #[cfg(test)]
            record_close(CloseKind::Unadopted, closed.map_err(|error| error as i32));
            #[cfg(not(test))]
            let _ = closed;
            return Err(error);
        };
        // fdopendir succeeded: transfer sole descriptor ownership to DIR.
        // DirectoryStream::close/Drop is now the only close path.
        let _ = descriptor.into_raw_fd();
        Ok(Self {
            pointer: Some(pointer),
        })
    }

    fn next_name(&mut self) -> io::Result<Option<OsString>> {
        let pointer = self.pointer.expect("directory stream is still owned");
        Errno::clear();
        // SAFETY: this object exclusively owns a live DIR. &mut self prevents
        // concurrent reads or close. The returned record is copied before the
        // next readdir; no reference or pointer escapes this private method.
        let entry = unsafe { libc::readdir(pointer.as_ptr()) };
        if entry.is_null() {
            let error = Errno::last();
            return if error == Errno::from_raw(0) {
                Ok(None)
            } else {
                Err(io::Error::from(error))
            };
        }
        let name_offset = std::mem::offset_of!(libc::dirent, d_name);
        // SAFETY: readdir returned a non-null live dirent record. The public
        // ABI supplies d_reclen; access only this field before checking bounds.
        let record_length = unsafe { (*entry).d_reclen as usize };
        let capacity = record_length
            .checked_sub(name_offset)
            .filter(|length| {
                *length > 0 && *length <= std::mem::size_of::<libc::dirent>() - name_offset
            })
            .ok_or_else(|| invalid("directory record length is invalid"))?;
        // SAFETY: the name starts at the ABI's d_name offset within the valid
        // record returned by readdir. capacity is bounded by that record and
        // the ABI structure; no unbounded C-string scan or full-struct reference
        // is made. We copy the bytes before any subsequent readdir/closedir.
        let bytes =
            unsafe { std::slice::from_raw_parts(entry.cast::<u8>().add(name_offset), capacity) };
        name_from_record_bytes(bytes).map(Some)
    }

    fn close(mut self) -> io::Result<()> {
        self.close_owned()
    }

    fn close_owned(&mut self) -> io::Result<()> {
        let Some(pointer) = self.pointer.take() else {
            return Ok(());
        };
        // SAFETY: pointer was adopted only after successful fdopendir. Taking
        // it first ensures one closedir call even if close reports an error.
        // No raw descriptor is independently closed or used after this call.
        let result = unsafe { libc::closedir(pointer.as_ptr()) };
        let closed = if result == 0 {
            Ok(())
        } else {
            Err(Errno::last())
        };
        #[cfg(test)]
        record_close(CloseKind::Stream, closed.map_err(|error| error as i32));
        closed.map_err(io::Error::from)
    }
}
impl Drop for DirectoryStream {
    fn drop(&mut self) {
        let _ = self.close_owned();
    }
}

// Test observations record the result of the actual internal close call.
// They do not replace I/O or touch a raw descriptor after it could be reused.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseKind {
    Unadopted,
    Stream,
}
#[cfg(test)]
std::thread_local! {
    static CLOSES: std::cell::RefCell<Vec<(CloseKind, Result<(), i32>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}
#[cfg(test)]
fn record_close(kind: CloseKind, result: Result<(), i32>) {
    CLOSES.with(|events| events.borrow_mut().push((kind, result)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .unwrap()
                .join("build/runtime/platform-fs-tests")
                .join(format!(
                    "{}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn directory(&self) -> File {
            File::open(&self.0).unwrap()
        }
        fn file(&self, name: impl AsRef<std::ffi::OsStr>) {
            std::fs::write(self.0.join(name.as_ref()), b"evidence").unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn take_closes() -> Vec<(CloseKind, Result<(), i32>)> {
        CLOSES.with(|events| std::mem::take(&mut *events.borrow_mut()))
    }
    fn names(values: &[&str]) -> BTreeSet<OsString> {
        values.iter().map(|value| OsString::from(*value)).collect()
    }

    #[test]
    fn empty_directory_at_zero_limit_filters_dots_and_closes_the_real_stream() {
        let fixture = Fixture::new();
        take_closes();
        assert!(directory_names(&fixture.directory(), 0).unwrap().is_empty());
        assert_eq!(take_closes(), [(CloseKind::Stream, Ok(()))]);
    }

    #[test]
    fn multiple_names_include_hidden_links_and_subdirectories_without_traversal() {
        let fixture = Fixture::new();
        fixture.file("one");
        fixture.file(".hidden");
        std::fs::create_dir(fixture.0.join("child")).unwrap();
        std::fs::write(fixture.0.join("child/not-a-top-level-name"), b"nested").unwrap();
        symlink("missing-target", fixture.0.join("unfollowed")).unwrap();
        let actual: BTreeSet<_> = directory_names(&fixture.directory(), 4)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(actual, names(&["one", ".hidden", "child", "unfollowed"]));
    }

    #[test]
    fn record_name_decoder_preserves_non_utf8_and_rejects_invalid_framing() {
        // The same bounded decoder is called by the real readdir path. APFS
        // rejects invalid UTF-8 basenames, so this case makes no native-FS claim.
        let a = name_from_record_bytes(b"name-\xff\0unused-padding").unwrap();
        let b = name_from_record_bytes(b"name-\xfe\0unused-padding").unwrap();
        assert_eq!(a.as_bytes(), b"name-\xff");
        assert_eq!(b.as_bytes(), b"name-\xfe");
        assert_ne!(a, b, "different raw names must not become lossy aliases");
        assert!(a.to_str().is_none() && b.to_str().is_none());
        assert_eq!(name_from_record_bytes(b"ok\0\xff").unwrap(), "ok");
        for bytes in [b"".as_slice(), b"\0", b"\0not-a-name", b"name-\xff"] {
            assert_eq!(
                name_from_record_bytes(bytes).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn common_name_max_basename_is_returned_whole_and_the_stream_closes_once() {
        let fixture = Fixture::new();
        // Actual 255-byte basename at the common Darwin/Linux NAME_MAX;
        // exercise the real dirent record length and trailing ABI padding.
        let name = OsString::from_vec(vec![b'n'; 255]);
        fixture.file(&name);
        take_closes();
        assert_eq!(directory_names(&fixture.directory(), 1).unwrap(), [name]);
        assert_eq!(take_closes(), [(CloseKind::Stream, Ok(()))]);
    }

    #[test]
    fn a_partially_consumed_callers_cursor_is_independent_of_every_enumeration() {
        let fixture = Fixture::new();
        for name in ["first", "second", "third"] {
            fixture.file(name);
        }
        let directory = fixture.directory();
        // try_clone intentionally shares the caller's original open-file
        // description. The public API must open a different one via openat.
        let mut original = DirectoryStream::from_file(directory.try_clone().unwrap()).unwrap();
        let mut visited = BTreeSet::new();
        while let Some(name) = original.next_name().unwrap() {
            if name != "." && name != ".." {
                visited.insert(name);
                break;
            }
        }
        assert_eq!(visited.len(), 1);
        for _ in 0..2 {
            assert_eq!(
                directory_names(&directory, 3)
                    .unwrap()
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                names(&["first", "second", "third"])
            );
        }
        while let Some(name) = original.next_name().unwrap() {
            if name != "." && name != ".." {
                assert!(visited.insert(name), "caller cursor was rewound");
            }
        }
        assert_eq!(visited, names(&["first", "second", "third"]));
        original.close().unwrap();
    }

    #[test]
    fn limit_failure_is_not_a_truncated_success_and_closes_the_stream() {
        let fixture = Fixture::new();
        fixture.file("one");
        fixture.file("two");
        for limit in [0, 1] {
            take_closes();
            let error = directory_names(&fixture.directory(), limit).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(take_closes(), [(CloseKind::Stream, Ok(()))]);
        }
        assert_eq!(directory_names(&fixture.directory(), 2).unwrap().len(), 2);
        assert_eq!(
            directory_names(&fixture.directory(), usize::MAX)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn a_regular_borrowed_file_is_rejected_without_consuming_the_callers_file() {
        let fixture = Fixture::new();
        fixture.file("regular");
        let file = File::open(fixture.0.join("regular")).unwrap();
        assert_eq!(
            directory_names(&file, 1).unwrap_err().raw_os_error(),
            Some(libc::ENOTDIR)
        );
        assert_eq!(file.metadata().unwrap().len(), 8);
    }

    #[test]
    fn actual_fdopendir_failure_closes_its_unadopted_owned_descriptor() {
        let fixture = Fixture::new();
        fixture.file("regular");
        let file = File::open(fixture.0.join("regular")).unwrap();
        take_closes();
        let error = DirectoryStream::from_file(file).err().unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR));
        // Receipt is captured inside the real nix::close path. No later close
        // or raw-fd probe risks touching a descriptor reused by another thread.
        assert_eq!(take_closes(), [(CloseKind::Unadopted, Ok(()))]);
    }
}
