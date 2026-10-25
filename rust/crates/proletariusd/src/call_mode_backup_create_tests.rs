use super::*;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicU64, Ordering};

const ID: &str = "usb-before-call-mode-20261019-123456.000000001-2345.json";
const PAYLOAD: &[u8] = b"literal original backup bytes\n";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "b581c-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn backup_dir(&self) -> PathBuf {
        self.0.join(super::super::BACKUP_DIRECTORY)
    }
    fn backup(&self) -> PathBuf {
        self.backup_dir().join(ID)
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
fn temporary(directory: &Path) -> PathBuf {
    let candidates: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".usb-create-")
        })
        .collect();
    assert_eq!(candidates.len(), 1);
    candidates[0].clone()
}

#[test]
fn actual_sync_path_preserves_bytes_and_syncs_each_new_ancestor() {
    let fixture = Fixture::new();
    let root = fixture.0.join("one/two/data");
    let mut points = Vec::new();
    write_with_hook(&root, ID, PAYLOAD, |point| {
        points.push(point);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        points
            .iter()
            .filter(|point| **point == Point::RootParentSync)
            .count(),
        3
    );
    assert_eq!(
        &points[3..],
        &[
            Point::FileSync,
            Point::BeforeCommit,
            Point::AfterCommit,
            Point::DirectorySync,
            Point::RootSync,
            Point::Complete,
            Point::FinalFileSync
        ]
    );
    let directory = root.join(super::super::BACKUP_DIRECTORY);
    assert_eq!(fs::read(directory.join(ID)).unwrap(), PAYLOAD);
    assert_eq!(fs::metadata(directory.join(ID)).unwrap().nlink(), 1);
    assert_eq!(
        fs::metadata(directory.join(ID)).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::metadata(&directory).unwrap().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
    assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
}

#[test]
fn existing_directory_permissions_are_preserved() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.backup_dir()).unwrap();
    fs::set_permissions(fixture.backup_dir(), fs::Permissions::from_mode(0o755)).unwrap();
    let directory_inode = evidence(&fixture.backup_dir()).1;
    write(&fixture.0, ID, PAYLOAD).unwrap();
    assert_eq!(
        fs::metadata(fixture.backup_dir()).unwrap().mode() & 0o777,
        0o755
    );
    assert_eq!(evidence(&fixture.backup_dir()).1, directory_inode);
}

#[test]
fn unsafe_root_and_backup_permissions_are_rejected_without_chmod() {
    for unsafe_root in [false, true] {
        let fixture = Fixture::new();
        fs::create_dir(fixture.backup_dir()).unwrap();
        let path = if unsafe_root {
            fixture.0.clone()
        } else {
            fixture.backup_dir()
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();
        let before = evidence(&path);
        assert!(write(&fixture.0, ID, PAYLOAD).is_err());
        assert_eq!(evidence(&path), before);
        assert!(!fixture.backup().exists());
    }
}

#[test]
fn existing_backup_is_never_overwritten_including_identical_id_retry() {
    let fixture = Fixture::new();
    write(&fixture.0, ID, PAYLOAD).unwrap();
    let before = evidence(&fixture.backup());
    assert_eq!(
        write(&fixture.0, ID, b"different bytes")
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(evidence(&fixture.backup()), before);
    assert_eq!(fs::read(fixture.backup()).unwrap(), PAYLOAD);
    assert_eq!(fs::read_dir(fixture.backup_dir()).unwrap().count(), 1);
}

#[test]
fn fixed_file_sync_failure_prevents_commit_and_cleans_only_own_temp() {
    let fixture = Fixture::new();
    let result = write_with_hook(&fixture.0, ID, PAYLOAD, |point| {
        if point == Point::FileSync {
            Err(io::Error::other("injected file sync failure"))
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert!(!fixture.backup().exists());
    assert_eq!(fs::read_dir(fixture.backup_dir()).unwrap().count(), 0);
}

#[test]
fn fixed_directory_or_root_sync_failure_retains_committed_backup_and_returns_error() {
    for failed in [Point::DirectorySync, Point::RootSync, Point::FinalFileSync] {
        let fixture = Fixture::new();
        let result = write_with_hook(&fixture.0, ID, PAYLOAD, |point| {
            if point == failed {
                Err(io::Error::other("injected directory sync failure"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(fs::read(fixture.backup()).unwrap(), PAYLOAD);
        assert_eq!(fs::metadata(fixture.backup()).unwrap().nlink(), 1);
        assert_eq!(fs::read_dir(fixture.backup_dir()).unwrap().count(), 1);
    }
}

#[test]
fn failed_new_root_parent_sync_never_commits_backup() {
    let fixture = Fixture::new();
    let root = fixture.0.join("new/data");
    let result = write_with_hook(&root, ID, PAYLOAD, |point| {
        if point == Point::RootParentSync {
            Err(io::Error::other("injected parent sync failure"))
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert!(!root.join(super::super::BACKUP_DIRECTORY).join(ID).exists());
}

#[test]
fn root_or_backup_directory_replaced_before_commit_is_rejected() {
    for replace_root in [false, true] {
        let fixture = Fixture::new();
        let root = fixture.0.join("data");
        fs::create_dir(&root).unwrap();
        let directory = root.join(super::super::BACKUP_DIRECTORY);
        let target = if replace_root {
            root.clone()
        } else {
            directory.clone()
        };
        let moved = fixture.0.join("original");
        let result = write_with_hook(&root, ID, PAYLOAD, |point| {
            if point == Point::BeforeCommit {
                fs::rename(&target, &moved).unwrap();
                fs::create_dir(&target).unwrap();
                fs::write(target.join("replacement-evidence"), b"keep").unwrap();
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read(target.join("replacement-evidence")).unwrap(),
            b"keep"
        );
        assert!(!directory.join(ID).exists());
        let original_directory = if replace_root {
            moved.join(super::super::BACKUP_DIRECTORY)
        } else {
            moved
        };
        assert_eq!(fs::read_dir(original_directory).unwrap().count(), 0);
    }
}

#[test]
fn cleanup_never_unlinks_replacement_temp_name() {
    let fixture = Fixture::new();
    let saved = fixture.0.join("saved-original-temp");
    let mut replacement = None;
    let result = write_with_hook(&fixture.0, ID, PAYLOAD, |point| {
        if point == Point::BeforeCommit {
            let path = temporary(&fixture.backup_dir());
            fs::rename(&path, &saved).unwrap();
            fs::write(&path, b"foreign replacement").unwrap();
            replacement = Some((path.clone(), evidence(&path)));
        }
        Ok(())
    });
    assert!(result.is_err());
    let (path, before) = replacement.unwrap();
    assert_eq!(evidence(&path), before);
    assert_eq!(fs::read(path).unwrap(), b"foreign replacement");
    assert_eq!(fs::read(saved).unwrap(), PAYLOAD);
    assert!(!fixture.backup().exists());
}

#[test]
fn destination_created_during_commit_cannot_be_overwritten() {
    let fixture = Fixture::new();
    let mut before = None;
    let result = write_with_hook(&fixture.0, ID, PAYLOAD, |point| {
        if point == Point::BeforeCommit {
            fs::write(fixture.backup(), b"other backup").unwrap();
            before = Some(evidence(&fixture.backup()));
        }
        Ok(())
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(evidence(&fixture.backup()), before.unwrap());
    assert_eq!(fs::read(fixture.backup()).unwrap(), b"other backup");
}

#[test]
fn postcommit_namespace_uncertainty_returns_error_and_retains_evidence() {
    for point in [Point::AfterCommit, Point::Complete] {
        let fixture = Fixture::new();
        let root = fixture.0.join("data");
        fs::create_dir(&root).unwrap();
        let moved = fixture.0.join("original");
        let result = write_with_hook(&root, ID, PAYLOAD, |observed| {
            if observed == point {
                fs::rename(&root, &moved).unwrap();
                fs::create_dir(&root).unwrap();
                fs::write(root.join("replacement-evidence"), b"keep").unwrap();
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read(moved.join(super::super::BACKUP_DIRECTORY).join(ID)).unwrap(),
            PAYLOAD
        );
        assert_eq!(
            fs::read(root.join("replacement-evidence")).unwrap(),
            b"keep"
        );
    }
}

#[test]
fn replaced_final_name_never_becomes_success_or_is_cleaned_as_ours() {
    let fixture = Fixture::new();
    let saved = fixture.backup_dir().join("saved-original.json");
    let mut replacement_before = None;
    let result = write_with_hook(&fixture.0, ID, PAYLOAD, |point| {
        if point == Point::AfterCommit {
            fs::rename(fixture.backup(), &saved).unwrap();
            fs::write(fixture.backup(), b"replacement").unwrap();
            replacement_before = Some(evidence(&fixture.backup()));
        }
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(evidence(&fixture.backup()), replacement_before.unwrap());
    assert_eq!(fs::read(fixture.backup()).unwrap(), b"replacement");
    assert_eq!(fs::read(saved).unwrap(), PAYLOAD);
    assert!(temporary(&fixture.backup_dir()).is_file());
}

#[test]
fn same_inode_same_length_payload_mutation_is_never_accepted() {
    for point in [
        Point::BeforeCommit,
        Point::AfterCommit,
        Point::Complete,
        Point::FinalFileSync,
    ] {
        let fixture = Fixture::new();
        let mut changed_path = None;
        let result = write_with_hook(&fixture.0, ID, PAYLOAD, |observed| {
            if point == observed {
                let path = if point == Point::BeforeCommit {
                    temporary(&fixture.backup_dir())
                } else {
                    fixture.backup()
                };
                let inode = evidence(&path).1;
                fs::write(&path, vec![b'X'; PAYLOAD.len()]).unwrap();
                assert_eq!(evidence(&path).1, inode);
                changed_path = Some(path);
            }
            Ok(())
        });
        assert!(result.is_err());
        if point == Point::BeforeCommit {
            assert!(!fixture.backup().exists());
        } else {
            assert_eq!(
                fs::read(changed_path.unwrap()).unwrap(),
                vec![b'X'; PAYLOAD.len()]
            );
        }
    }
}

#[test]
fn abnormal_backup_directory_shapes_are_rejected_without_changing_entries() {
    for shape in ["symlink", "fifo", "socket", "file"] {
        let fixture = Fixture::new();
        let directory = fixture.backup_dir();
        let mut socket = None;
        match shape {
            "symlink" => {
                fs::create_dir(fixture.0.join("target")).unwrap();
                symlink("target", &directory).unwrap();
            }
            "fifo" => nix::unistd::mkfifo(&directory, Mode::S_IRUSR | Mode::S_IWUSR).unwrap(),
            "socket" => {
                // Darwin sun_path has 104 bytes, including its terminator.
                assert!(directory.as_os_str().as_bytes().len() < 104);
                socket = Some(
                    UnixListener::bind(&directory)
                        .expect("fixture must create an actual Unix socket"),
                );
                assert!(
                    fs::symlink_metadata(&directory)
                        .unwrap()
                        .file_type()
                        .is_socket()
                );
            }
            "file" => fs::write(&directory, b"evidence").unwrap(),
            _ => unreachable!(),
        }
        let before = evidence(&directory);
        assert!(write(&fixture.0, ID, PAYLOAD).is_err());
        assert_eq!(evidence(&directory), before);
        drop(socket);
    }
}

#[test]
fn bound_creation_uses_actual_lease_root_descriptor() {
    let fixture = Fixture::new();
    let lease = crate::daemon_lifetime::Lease::acquire(&fixture.0).unwrap();
    lease
        .root_proof()
        .with_root(&fixture.0, |held| {
            let owned = Directory::from_held(&fixture.0, held).unwrap();
            assert_eq!(owned.identity, Identity::of(&held.metadata().unwrap()));
            let flags = nix::fcntl::fcntl(&owned.file, nix::fcntl::FcntlArg::F_GETFD).unwrap();
            assert_ne!(flags & nix::libc::FD_CLOEXEC, 0);
            create_bound(&fixture.0, held, ID, PAYLOAD)
        })
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(fixture.backup()).unwrap(), PAYLOAD);
    assert_eq!(fs::metadata(fixture.backup()).unwrap().nlink(), 1);
    lease.root_proof().with_root(&fixture.0, |_| ()).unwrap();
}

#[test]
fn bound_creation_rejects_foreign_path_before_creating_any_entry() {
    let fixture = Fixture::new();
    let foreign = Fixture::new();
    let lease = crate::daemon_lifetime::Lease::acquire(&fixture.0).unwrap();
    let before = evidence(&foreign.0);
    let result = lease
        .root_proof()
        .with_root(&fixture.0, |held| {
            create_bound(&foreign.0, held, ID, PAYLOAD)
        })
        .unwrap();
    assert!(result.is_err());
    assert_eq!(evidence(&foreign.0), before);
    assert!(!fixture.backup_dir().exists());
    assert!(!foreign.backup_dir().exists());
}

#[test]
fn bound_creation_refuses_foreign_path_even_when_outer_proof_sees_aba_restored() {
    let fixture = Fixture::new();
    let root = fixture.0.join("data");
    let foreign = fixture.0.join("foreign");
    let original = fixture.0.join("original");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&foreign).unwrap();
    let lease = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
    let result = lease
        .root_proof()
        .with_root(&root, |held| {
            fs::rename(&root, &original).unwrap();
            fs::rename(&foreign, &root).unwrap();
            let result = create_bound(&root, held, ID, PAYLOAD);
            fs::rename(&root, &foreign).unwrap();
            fs::rename(&original, &root).unwrap();
            result
        })
        .unwrap(); // The outer proof sees its original inode at both boundaries.
    assert!(result.is_err());
    assert!(!root.join(super::super::BACKUP_DIRECTORY).exists());
    assert!(!foreign.join(super::super::BACKUP_DIRECTORY).exists());
}

#[test]
fn bound_creation_does_not_create_a_missing_foreign_root() {
    let fixture = Fixture::new();
    let missing = fixture.0.join("not-the-held-root");
    let lease = crate::daemon_lifetime::Lease::acquire(&fixture.0).unwrap();
    let result = lease
        .root_proof()
        .with_root(&fixture.0, |held| create_bound(&missing, held, ID, PAYLOAD))
        .unwrap();
    assert!(result.is_err());
    assert!(!missing.exists());
}
