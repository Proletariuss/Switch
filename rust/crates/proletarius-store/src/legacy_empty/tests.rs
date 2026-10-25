use super::*;
use crate::tests::TestDirectory;
use serde_json::Value;
use std::{
    os::unix::fs::symlink,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const REPLACEMENT: &[u8] = br#"[{"schema_version":1}]"#;
fn validate(bytes: &[u8]) -> Result<(), StoreError> {
    let values: Vec<Value> = serde_json::from_slice(bytes).map_err(StoreError::Json)?;
    if values.len() == 1 && values[0]["schema_version"] == 1 {
        Ok(())
    } else {
        Err(StoreError::Configuration("invalid_store_schema".into()))
    }
}
fn store(directory: &TestDirectory) -> ArchiveStore {
    ArchiveStore::with_limits(directory.archive("ledger.json"), 1024, 1).unwrap()
}
fn migrate(store: &ArchiveStore) -> Result<bool, CommitError> {
    store.replace_unenrolled_legacy_empty_array(REPLACEMENT, validate)
}
fn wait(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !path.exists() {
        assert!(Instant::now() < deadline, "waiting for test checkpoint");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn missing_and_valid_nonempty_remain_unchanged_but_exact_legacy_is_enrolled() {
    let directory = TestDirectory::new();
    let store = store(&directory);
    assert!(!migrate(&store).unwrap());
    assert!(!store.path().exists());
    assert!(!store.presence_path().unwrap().exists());
    fs::write(store.path(), b"[]").unwrap();
    assert!(migrate(&store).unwrap());
    assert_eq!(fs::read(store.path()).unwrap(), REPLACEMENT);
    assert_eq!(
        fs::read(store.presence_path().unwrap()).unwrap(),
        PRESENCE_CONTRACT
    );
    assert_eq!(
        fs::read(store.lock_path().unwrap()).unwrap(),
        PRESENCE_CONTRACT
    );
    let inode = fs::metadata(store.path()).unwrap().ino();
    assert!(!migrate(&store).unwrap());
    assert_eq!(fs::metadata(store.path()).unwrap().ino(), inode);
    fs::write(store.path(), b" [ {\"schema_version\": 1} ]\n").unwrap();
    let before = fs::read(store.path()).unwrap();
    assert!(!migrate(&store).unwrap());
    assert_eq!(fs::read(store.path()).unwrap(), before);
    // Existing ordinary archives retain their Value-based duplicate semantics.
    let duplicate = br#"[{"schema_version":0,"schema_version":1}]"#;
    fs::write(store.path(), duplicate).unwrap();
    assert!(!migrate(&store).unwrap());
    assert_eq!(fs::read(store.path()).unwrap(), duplicate);
}

#[test]
fn refuses_every_nonlegacy_empty_or_corrupt_form_without_enrolling_or_rewriting() {
    for bytes in [
        b"".as_slice(),
        b"null",
        b"null\n",
        b" []",
        b"[]\n",
        b"[ ]",
        b" ",
        b"{broken",
        b"{}",
        b"[null]",
        br#"[{"schema_version":2}]"#,
    ] {
        let directory = TestDirectory::new();
        let store = store(&directory);
        fs::write(store.path(), bytes).unwrap();
        assert!(migrate(&store).is_err(), "{bytes:?}");
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
        assert!(!store.presence_path().unwrap().exists());
        assert!(fs::read(store.lock_path().unwrap()).unwrap().is_empty());
    }
}

#[test]
fn any_modern_or_unknown_witness_prevents_empty_array_migration() {
    for (marker, anchor) in [
        (Some(PRESENCE_CONTRACT), b"".as_slice()),
        (None, PRESENCE_CONTRACT),
        (Some(PRESENCE_CONTRACT), PRESENCE_CONTRACT),
        (Some(b"".as_slice()), b"".as_slice()),
        (Some(b"broken".as_slice()), b"".as_slice()),
        (None, b"broken".as_slice()),
        (Some(PRESENCE_CONTRACT), b"partial".as_slice()),
    ] {
        let directory = TestDirectory::new();
        let store = store(&directory);
        fs::write(store.path(), b"[]").unwrap();
        fs::write(store.lock_path().unwrap(), anchor).unwrap();
        if let Some(bytes) = marker {
            fs::write(store.presence_path().unwrap(), bytes).unwrap();
        }
        assert!(migrate(&store).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), b"[]");
        assert_eq!(fs::read(store.lock_path().unwrap()).unwrap(), anchor);
        assert_eq!(
            fs::read(store.presence_path().unwrap()).ok().as_deref(),
            marker
        );
    }
}

#[test]
fn unsafe_archive_lock_marker_and_directory_never_follow_symlinks() {
    for which in [
        "archive",
        "lock",
        "marker",
        "parent",
        "dangling",
        "directory",
        "hardlink",
    ] {
        let directory = TestDirectory::new();
        let store = store(&directory);
        let target = directory.archive("preserved-target");
        fs::write(&target, b"[]").unwrap();
        fs::write(store.path(), b"[]").unwrap();
        match which {
            "archive" | "dangling" | "hardlink" | "directory" => {
                fs::remove_file(store.path()).unwrap();
                if which == "dangling" {
                    symlink("absent", store.path()).unwrap();
                } else if which == "directory" {
                    fs::create_dir(store.path()).unwrap();
                } else if which == "hardlink" {
                    fs::hard_link(&target, store.path()).unwrap();
                } else {
                    symlink(&target, store.path()).unwrap();
                }
            }
            "lock" => symlink(&target, store.lock_path().unwrap()).unwrap(),
            "marker" => symlink(&target, store.presence_path().unwrap()).unwrap(),
            "parent" => {
                let link = directory.archive("linked-parent");
                symlink(&directory.0, &link).unwrap();
                let alias = ArchiveStore::new(link.join("ledger.json"));
                assert!(migrate(&alias).is_err());
            }
            _ => unreachable!(),
        }
        if which != "parent" {
            assert!(migrate(&store).is_err(), "{which}");
        }
        assert_eq!(fs::read(&target).unwrap(), b"[]", "{which}");
    }
}

#[test]
fn waiting_migration_uses_latest_archive_and_witnesses_and_same_writer_lock() {
    for update in ["nonempty", "corrupt", "modern"] {
        let directory = TestDirectory::new();
        let mut store = store(&directory);
        fs::write(store.path(), b"[]").unwrap();
        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(store.lock_path().unwrap())
            .unwrap();
        lock.lock().unwrap();
        let checkpoint = directory.archive("opened");
        store.after_lock_open = Some(checkpoint.clone());
        let worker_store = store.clone();
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || tx.send(migrate(&worker_store)).unwrap());
        wait(&checkpoint);
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        let bytes: &[u8] = if update == "nonempty" {
            REPLACEMENT
        } else if update == "corrupt" {
            b"null"
        } else {
            b"[]"
        };
        fs::write(store.path(), bytes).unwrap();
        if update == "modern" {
            fs::write(store.presence_path().unwrap(), PRESENCE_CONTRACT).unwrap();
            lock.write_all(PRESENCE_CONTRACT).unwrap();
        }
        lock.unlock().unwrap();
        let result = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(result.is_ok(), update == "nonempty");
        if let Ok(changed) = result {
            assert!(!changed);
        }
        worker.join().unwrap();
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
    }
}

#[test]
fn replacing_lock_while_waiting_rejects_without_touching_archive() {
    let directory = TestDirectory::new();
    let mut store = store(&directory);
    fs::write(store.path(), b"[]").unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(store.lock_path().unwrap())
        .unwrap();
    lock.lock().unwrap();
    let checkpoint = directory.archive("opened");
    store.after_lock_open = Some(checkpoint.clone());
    let worker_store = store.clone();
    let worker = thread::spawn(move || migrate(&worker_store));
    wait(&checkpoint);
    fs::rename(store.lock_path().unwrap(), directory.archive("old-lock")).unwrap();
    fs::write(store.lock_path().unwrap(), b"").unwrap();
    lock.unlock().unwrap();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(fs::read(store.path()).unwrap(), b"[]");
    assert!(!store.presence_path().unwrap().exists());
}

#[test]
fn fault_before_rename_preserves_legacy_but_enrollment_prevents_restart_retry() {
    for point in [1, 2, 3, 6, 7] {
        let directory = TestDirectory::new();
        let store = store(&directory);
        fs::write(store.path(), b"[]").unwrap();
        store.fail_commit_stage.store(point, Ordering::SeqCst);
        let error = migrate(&store).unwrap_err();
        assert_eq!(error.stage, CommitStage::BeforeRename);
        assert_eq!(fs::read(store.path()).unwrap(), b"[]");
        assert_eq!(
            fs::read(store.presence_path().unwrap()).unwrap(),
            PRESENCE_CONTRACT
        );
        assert!(migrate(&ArchiveStore::new(store.path())).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), b"[]");
    }
}

#[test]
fn uncertain_after_rename_never_acknowledges_but_fresh_valid_state_can_reopen() {
    for fault in [0, 4, 5] {
        let directory = TestDirectory::new();
        let store = store(&directory);
        fs::write(store.path(), b"[]").unwrap();
        if fault == 0 {
            store.fail_next_directory_sync.store(true, Ordering::SeqCst);
        } else {
            store.fail_commit_stage.store(fault, Ordering::SeqCst);
        }
        let error = migrate(&store).unwrap_err();
        assert_eq!(error.stage, CommitStage::AfterRename);
        assert_eq!(fs::read(store.path()).unwrap(), REPLACEMENT);
        let reopened = ArchiveStore::new(store.path());
        assert!(!migrate(&reopened).unwrap());
        assert_eq!(reopened.track_durable_presence().load().unwrap().len(), 1);
    }
}

#[test]
fn schema_callback_cannot_return_facts_after_replacing_current_inode() {
    let directory = TestDirectory::new();
    let store = store(&directory);
    fs::write(store.path(), REPLACEMENT).unwrap();
    let calls = std::cell::Cell::new(0);
    let result = store.replace_unenrolled_legacy_empty_array(REPLACEMENT, |bytes| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            let replacement = directory.archive("replacement");
            fs::write(&replacement, REPLACEMENT).unwrap();
            fs::rename(replacement, store.path()).unwrap();
        }
        validate(bytes)
    });
    assert!(result.is_err());
    assert_eq!(fs::read(store.path()).unwrap(), REPLACEMENT);
}

#[test]
fn schema_callback_cannot_hide_presence_or_symlink_changes() {
    for update in ["anchor", "marker", "symlink"] {
        let directory = TestDirectory::new();
        let store = store(&directory);
        fs::write(store.path(), REPLACEMENT).unwrap();
        let target = directory.archive("preserved-target");
        fs::write(&target, REPLACEMENT).unwrap();
        let calls = std::cell::Cell::new(0);
        let result = store.replace_unenrolled_legacy_empty_array(REPLACEMENT, |bytes| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                match update {
                    "anchor" => fs::write(store.lock_path().unwrap(), PRESENCE_CONTRACT).unwrap(),
                    "marker" => {
                        fs::write(store.presence_path().unwrap(), PRESENCE_CONTRACT).unwrap()
                    }
                    "symlink" => {
                        fs::remove_file(store.path()).unwrap();
                        symlink(&target, store.path()).unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            validate(bytes)
        });
        assert!(result.is_err(), "{update}");
        assert_eq!(fs::read(&target).unwrap(), REPLACEMENT);
        assert_eq!(fs::read(store.path()).unwrap(), REPLACEMENT);
    }
}

#[test]
#[ignore = "subprocess helper invoked only by the migration crash test"]
fn migration_crash_worker() {
    let root = std::path::PathBuf::from(std::env::var_os("SWITCH_G1_CRASH_ROOT").unwrap());
    let point: u64 = std::env::var("SWITCH_G1_CRASH_POINT")
        .unwrap()
        .parse()
        .unwrap();
    CHECKPOINT.with(|hook| {
        let ready = root.join("ready");
        *hook.borrow_mut() = Some(Box::new(move |at| {
            if at == point {
                fs::write(&ready, b"entered").unwrap();
                loop {
                    thread::park_timeout(Duration::from_millis(50));
                }
            }
        }));
    });
    migrate(&ArchiveStore::new(root.join("ledger.json"))).unwrap();
    panic!("child returned instead of waiting at migration checkpoint");
}

#[test]
fn actual_process_kill_preserves_old_or_valid_enrolled_state_at_every_commit_prefix() {
    use std::process::{Command, Stdio};
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for point in [0, 6, 1, 2, 3, 4, 5] {
        let directory = TestDirectory::new();
        let store = store(&directory);
        fs::write(store.path(), b"[]").unwrap();
        let mut child = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "legacy_empty::tests::migration_crash_worker",
                    "--ignored",
                    "--nocapture",
                ])
                .env("SWITCH_G1_CRASH_ROOT", &directory.0)
                .env("SWITCH_G1_CRASH_POINT", point.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        wait(&directory.archive("ready"));
        child.0.kill().unwrap();
        assert!(!child.0.wait().unwrap().success());
        let bytes = fs::read(store.path()).unwrap();
        if point == 0 {
            assert_eq!(bytes, b"[]");
            assert!(!store.presence_path().unwrap().exists());
            assert!(migrate(&store).unwrap());
        } else if point == 4 || point == 5 {
            assert_eq!(bytes, REPLACEMENT);
            assert!(!migrate(&store).unwrap());
            assert_eq!(
                fs::read(store.presence_path().unwrap()).unwrap(),
                PRESENCE_CONTRACT
            );
        } else {
            assert_eq!(bytes, b"[]");
            assert!(migrate(&store).is_err());
            assert_eq!(fs::read(store.path()).unwrap(), bytes);
        }
    }
}
