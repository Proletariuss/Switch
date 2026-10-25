use super::*;
use crate::tests::TestDirectory;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    process::{Command, Stdio},
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

fn valid(_: &[u8]) -> Result<(), &'static str> {
    Ok(())
}
fn keep(bytes: &[u8]) -> Result<Mutation<Vec<u8>>, &'static str> {
    Ok(Mutation::Unchanged(bytes.to_vec()))
}
fn replace(bytes: &[u8]) -> Mutation<()> {
    Mutation::Replace {
        bytes: bytes.to_vec(),
        result: (),
    }
}
fn wait(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "waiting for {}", path.display());
        thread::sleep(Duration::from_millis(5));
    }
}
fn lock(path: &std::path::Path) -> File {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    file.lock().unwrap();
    file
}

#[test]
fn current_duplicates_at_every_depth_reject_before_callbacks_and_preserve_bytes() {
    let dir = TestDirectory::new();
    let path = dir.archive("raw.json");
    let store = ArchiveStore::new(&path);
    for bytes in [
        br#"[{"a":1,"a":2}]"#.as_slice(),
        br#"[{"a":1,"\u0061":2}]"#,
        br#"[{"nested":{"x":1,"x":2}}]"#,
        br#"[{"nested":[{"x":1,"\u0078":2}]}]"#,
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(
            store.load().is_ok(),
            "legacy Value duplicate semantics remain accepted"
        );
        let error = store
            .transaction_raw::<(), (), ()>(|_| panic!("schema called"), |_| panic!("plan called"))
            .unwrap_err();
        assert!(matches!(
            error,
            TransactionError::Current(StoreError::Json(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn strict_absence_empty_shape_limits_and_legacy_defaults_are_separate() {
    let dir = TestDirectory::new();
    let path = dir.archive("raw.json");
    let store = ArchiveStore::with_limits(&path, 32, 2).unwrap();
    assert!(store.load().unwrap().is_empty());
    assert!(matches!(
        store.transaction_raw(valid, keep),
        Err(TransactionError::Current(_))
    ));
    assert!(!path.exists());
    for bytes in [
        b"".as_slice(),
        b"null",
        b"[]",
        b"{}",
        b"[1]",
        b"[{}, {}, {}]",
        &[b'x'; 33],
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            store.transaction_raw(valid, keep),
            Err(TransactionError::Current(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    for bytes in [b"".as_slice(), b"null", b"[]"] {
        fs::write(&path, bytes).unwrap();
        assert!(store.load().unwrap().is_empty());
    }
    store
        .replace(vec![json!({"x":1}), json!({"x":2}), json!({"x":3})])
        .unwrap();
    assert_eq!(store.load().unwrap().len(), 2);
}

#[test]
fn proposals_and_schema_conflicts_are_phase_separate_without_commit_or_truncation() {
    let dir = TestDirectory::new();
    let path = dir.archive("raw.json");
    let original = b" [ { \"x\" : 1 } ]\n";
    fs::write(&path, original).unwrap();
    let store = ArchiveStore::with_limits(&path, 40, 2).unwrap();
    for bytes in [
        br#"[{"x":1,"x":2}]"#.as_slice(),
        b"[]",
        b"[1]",
        b"[{}, {}, {}]",
        &[b'x'; 41],
    ] {
        assert!(matches!(
            store.transaction_raw(valid, |_| Ok::<_, ()>(replace(bytes))),
            Err(TransactionError::Proposal(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            fs::read_dir(&dir.0).unwrap().count(),
            2,
            "only data and lock, no temp or witness"
        );
    }
    assert!(matches!(
        store.transaction_raw(valid, |_| Err::<Mutation<()>, _>("conflict")),
        Err(TransactionError::Rejected("conflict"))
    ));
    assert!(matches!(
        store.transaction_raw(|_| Err::<(), _>("bad current"), keep),
        Err(TransactionError::CurrentSchema("bad current"))
    ));
    let schema = |bytes: &[u8]| {
        if bytes == original {
            Ok(())
        } else {
            Err("bad proposed")
        }
    };
    assert!(matches!(
        store.transaction_raw(schema, |_| Ok::<_, ()>(replace(b"[{}]"))),
        Err(TransactionError::ProposalSchema("bad proposed"))
    ));
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn unchanged_preserves_bytes_inode_and_success_only_after_commit() {
    let dir = TestDirectory::new();
    let path = dir.archive("raw.json");
    let original = b" [ { \"x\" : 1, \"nested\" : [true, null, -1, 1.5, \"ok\"] } ]\n";
    fs::write(&path, original).unwrap();
    let store = ArchiveStore::new(&path);
    let inode = fs::metadata(&path).unwrap().ino();
    assert!(!store.presence_path().unwrap().exists());
    assert_eq!(store.transaction_raw(valid, keep).unwrap(), original);
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(
        fs::read(store.presence_path().unwrap()).unwrap(),
        crate::PRESENCE_CONTRACT
    );
    assert_eq!(
        fs::read(store.lock_path().unwrap()).unwrap(),
        crate::PRESENCE_CONTRACT
    );
    store
        .transaction_raw(valid, |_| Ok::<_, ()>(replace(b"[{\"x\":2}]")))
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"[{\"x\":2}]");
}

#[test]
fn common_kernel_fault_boundaries_never_return_result_and_keep_old_or_new() {
    for stage in 1..=5 {
        let dir = TestDirectory::new();
        let path = dir.archive("raw.json");
        let old = b"[{\"x\":1}]";
        let new = b"[{\"x\":2}]";
        fs::write(&path, old).unwrap();
        let store = ArchiveStore::new(&path);
        // Enroll presence before isolating the actual shared commit kernel.
        store.transaction_raw(valid, keep).unwrap();
        store.fail_commit_stage.store(stage, Ordering::SeqCst);
        let error = store
            .transaction_raw(valid, |_| Ok::<_, ()>(replace(new)))
            .unwrap_err();
        match error {
            TransactionError::Commit(error) => assert_eq!(
                error.stage,
                if stage <= 3 {
                    CommitStage::BeforeRename
                } else {
                    CommitStage::AfterRename
                }
            ),
            _ => panic!("wrong phase: {error:?}"),
        }
        assert_eq!(
            fs::read(&path).unwrap(),
            if stage <= 3 {
                old.as_slice()
            } else {
                new.as_slice()
            }
        );
        assert!(!fs::read_dir(&dir.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        // Fresh reopen only inspects visible data, not a retrospective commit ACK.
        let reopened = ArchiveStore::new(&path);
        assert_eq!(
            reopened.transaction_raw(valid, keep).unwrap(),
            fs::read(&path).unwrap()
        );
    }
}

#[test]
fn actual_current_or_lock_replacement_during_callback_fails_closed() {
    for swap_lock in [false, true] {
        let dir = TestDirectory::new();
        let path = dir.archive("raw.json");
        fs::write(&path, b"[{}]").unwrap();
        let store = ArchiveStore::new(&path);
        let error = store
            .transaction_raw(valid, |_| {
                // Deliberately violate purity to exercise external replacement defense.
                let target = if swap_lock {
                    store.lock_path().unwrap()
                } else {
                    path.clone()
                };
                fs::rename(&target, target.with_extension("old")).unwrap();
                fs::write(&target, if swap_lock { b"".as_slice() } else { b"[{}]" }).unwrap();
                Ok::<_, ()>(replace(b"[{\"new\":true}]"))
            })
            .unwrap_err();
        assert!(matches!(error, TransactionError::Current(_)));
        assert_eq!(fs::read(&path).unwrap(), b"[{}]");
    }
}

fn worker(path: &std::path::Path, mode: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "raw::tests::process_worker",
            "--nocapture",
        ])
        .env("STORE_RAW_WORKER_PATH", path)
        .env("STORE_RAW_WORKER_MODE", mode)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}
#[test]
#[ignore = "subprocess helper, invoked by process tests"]
fn process_worker() {
    let Ok(path) = std::env::var("STORE_RAW_WORKER_PATH") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let mode = std::env::var("STORE_RAW_WORKER_MODE").unwrap();
    let mut store = ArchiveStore::new(&path);
    store.after_lock_open = Some(path.with_extension(format!("{}.opened", std::process::id())));
    let result = store.transaction_raw(valid, |bytes| {
        let mut items: Vec<Value> = serde_json::from_slice(bytes).unwrap();
        let count = items[0]["count"].as_u64().unwrap();
        items[0]["count"] = json!(count + 1);
        Ok::<_, ()>(Mutation::Replace {
            bytes: serde_json::to_vec(&items).unwrap(),
            result: (),
        })
    });
    if mode == "reject" {
        assert!(matches!(result, Err(TransactionError::Current(_))));
    } else {
        result.unwrap();
    }
}

#[test]
fn independent_processes_serialize_without_lost_update() {
    let dir = TestDirectory::new();
    let path = dir.archive("raw.json");
    fs::write(&path, br#"[{"count":0}]"#).unwrap();
    let mut children: Vec<_> = (0..6).map(|_| worker(&path, "increment")).collect();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    let items: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(items[0]["count"], 6);
}

#[test]
fn blocked_process_rereads_current_and_detects_named_lock_replacement() {
    for replacement in [false, true] {
        let dir = TestDirectory::new();
        let path = dir.archive("raw.json");
        fs::write(&path, br#"[{"count":0}]"#).unwrap();
        let store = ArchiveStore::new(&path);
        let lock_path = store.lock_path().unwrap();
        let held = lock(&lock_path);
        let mut child = worker(&path, if replacement { "reject" } else { "increment" });
        wait(&path.with_extension(format!("{}.opened", child.id())));
        assert!(child.try_wait().unwrap().is_none());
        if replacement {
            fs::rename(&lock_path, dir.archive("old.lock")).unwrap();
            fs::write(&lock_path, b"").unwrap();
        } else {
            fs::write(&path, br#"[{"count":10}]"#).unwrap();
        }
        held.unlock().unwrap();
        assert!(child.wait().unwrap().success());
        let items: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(items[0]["count"], if replacement { 0 } else { 11 });
    }
}

#[test]
fn presence_loss_after_wait_and_current_capacity_are_current_errors() {
    let dir = TestDirectory::new();
    let path = dir.archive("raw.json");
    fs::write(&path, b"[{}]").unwrap();
    let mut store = ArchiveStore::with_limits(&path, 8, 1).unwrap();
    store.transaction_raw(valid, keep).unwrap();
    let held = lock(&store.lock_path().unwrap());
    let opened = dir.archive("opened");
    store.after_lock_open = Some(opened.clone());
    let join = thread::spawn(move || store.transaction_raw(valid, keep));
    wait(&opened);
    fs::remove_file(&path).unwrap();
    held.unlock().unwrap();
    assert!(matches!(
        join.join().unwrap(),
        Err(TransactionError::Current(_))
    ));
    // Different fresh path: current overflow is never classified as a proposal.
    let path = dir.archive("large.json");
    fs::write(&path, b"[{},{}]").unwrap();
    let store = ArchiveStore::with_limits(&path, 8, 1).unwrap();
    assert!(matches!(
        store.transaction_raw(valid, keep),
        Err(TransactionError::Current(StoreError::TooManyItems { .. }))
    ));
}

#[test]
fn exact_byte_and_item_bounds_pass_and_plus_one_remains_phase_specific() {
    let dir = TestDirectory::new();
    let path = dir.archive("raw.json");
    let exact = br#"[{"x":"123456"}]"#;
    let store = ArchiveStore::with_limits(&path, exact.len() as u64, 1).unwrap();
    fs::write(&path, exact).unwrap();
    assert_eq!(store.transaction_raw(valid, keep).unwrap(), exact);
    let mut over = exact.to_vec();
    over.push(b' ');
    assert!(matches!(
        store.transaction_raw(valid, |_| Ok::<_, ()>(replace(&over))),
        Err(TransactionError::Proposal(StoreError::TooLarge { .. }))
    ));
    assert_eq!(fs::read(&path).unwrap(), exact);
    fs::write(&path, &over).unwrap();
    assert!(matches!(
        store.transaction_raw(valid, keep),
        Err(TransactionError::Current(StoreError::TooLarge { .. }))
    ));
}

#[test]
fn invalid_presence_and_symlink_are_current_failures_before_callbacks() {
    let dir = TestDirectory::new();
    let path = dir.archive("raw.json");
    fs::write(&path, b"[{}]").unwrap();
    let store = ArchiveStore::new(&path);
    store.transaction_raw(valid, keep).unwrap();
    fs::write(store.presence_path().unwrap(), b"bad").unwrap();
    assert!(matches!(
        store.transaction_raw::<(), (), ()>(|_| panic!("schema"), |_| panic!("plan")),
        Err(TransactionError::Current(StoreError::Configuration(_)))
    ));
    let other = dir.archive("target.json");
    fs::write(&other, b"[{}]").unwrap();
    let link = dir.archive("link.json");
    std::os::unix::fs::symlink(&other, &link).unwrap();
    assert!(matches!(
        ArchiveStore::new(&link).transaction_raw(valid, keep),
        Err(TransactionError::Current(StoreError::SymlinkRejected))
    ));
    assert_eq!(fs::read(&other).unwrap(), b"[{}]");
}

#[test]
fn interrupted_presence_enrollment_repairs_empty_anchor_before_returning_success() {
    let dir = TestDirectory::new();
    let path = dir.archive("raw.json");
    let original = b" [ {} ]\n";
    fs::write(&path, original).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    let store = ArchiveStore::new(&path);
    store.fail_commit_stage.store(6, Ordering::SeqCst);
    assert!(matches!(
        store.transaction_raw(valid, keep),
        Err(TransactionError::Current(StoreError::Io(_)))
    ));
    assert_eq!(
        fs::read(store.presence_path().unwrap()).unwrap(),
        crate::PRESENCE_CONTRACT
    );
    assert!(fs::read(store.lock_path().unwrap()).unwrap().is_empty());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    let reopened = ArchiveStore::new(&path);
    assert_eq!(reopened.transaction_raw(valid, keep).unwrap(), original);
    assert_eq!(
        fs::read(reopened.lock_path().unwrap()).unwrap(),
        crate::PRESENCE_CONTRACT
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
}

#[test]
fn family_inspection_rejects_duplicate_siblings_before_schema_and_preserves_data() {
    let dir = TestDirectory::new();
    let path = dir.archive("active.json");
    fs::write(&path, b"[{\"v\":1}]").unwrap();
    let store = ArchiveStore::with_limits(&path, 1024, 1).unwrap();
    for bad in [
        br#"[{"x":1,"x":2}]"#.as_slice(),
        br#"[{"nested":{"x":1,"\u0078":2}}]"#,
    ] {
        fs::write(dir.archive("frozen.json"), bad).unwrap();
        assert!(matches!(
            store.inspect_raw_family::<(), ()>(&["frozen.json"], |_, _| panic!(
                "schema saw duplicate"
            )),
            Err(FamilyInspectionError::Storage(StoreError::Json(_)))
        ));
        assert_eq!(fs::read(dir.archive("frozen.json")).unwrap(), bad);
        assert_eq!(fs::read(&path).unwrap(), b"[{\"v\":1}]");
        assert!(
            !store.presence_path().unwrap().exists(),
            "invalid family never enrolled"
        );
    }
}

#[test]
fn family_inspection_exposes_explicit_absence_and_rejects_disappeared_active() {
    let dir = TestDirectory::new();
    let path = dir.archive("active.json");
    fs::write(&path, b"[{\"v\":1}]").unwrap();
    let store = ArchiveStore::with_limits(&path, 64, 1).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    assert_eq!(
        store
            .inspect_raw_family(&["frozen.json"], |bytes, family| {
                assert_eq!(bytes, b"[{\"v\":1}]");
                assert_eq!(family, &[None]);
                Ok::<_, ()>(7)
            })
            .unwrap(),
        7
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    fs::remove_file(&path).unwrap();
    assert!(
        store
            .inspect_raw_family::<(), ()>(&["frozen.json"], |_, _| panic!("missing active"))
            .is_err()
    );
    assert!(!path.exists());
}

#[test]
fn family_inspection_rechecks_all_members_and_named_lock_after_pure_callback() {
    for target in ["active.json", "frozen.json", "active.json.lock"] {
        for same_bytes in [false, true] {
            let dir = TestDirectory::new();
            let path = dir.archive("active.json");
            fs::write(&path, b"[{\"v\":1}]").unwrap();
            fs::write(dir.archive("frozen.json"), b"[{\"v\":2}]").unwrap();
            let store = ArchiveStore::with_limits(&path, 1024, 1).unwrap();
            // Deliberately violate the pure-callback contract to verify defenses.
            let result = store.inspect_raw_family(&["frozen.json", "prepared.json"], |_, _| {
                let moved = dir.archive("replacement");
                let bytes = if same_bytes {
                    fs::read(dir.archive(target)).unwrap()
                } else {
                    b"[{\"v\":3}]".to_vec()
                };
                fs::write(&moved, bytes).unwrap();
                fs::rename(moved, dir.archive(target)).unwrap();
                Ok::<_, ()>(())
            });
            assert!(
                matches!(result, Err(FamilyInspectionError::Storage(_))),
                "{target} same_bytes={same_bytes}"
            );
        }
    }
    let dir = TestDirectory::new();
    let path = dir.archive("active.json");
    fs::write(&path, b"[{}]").unwrap();
    let store = ArchiveStore::new(&path);
    assert!(
        store
            .inspect_raw_family(&["absent.json"], |_, _| {
                fs::write(dir.archive("absent.json"), b"[{}]").unwrap();
                Ok::<_, ()>(())
            })
            .is_err()
    );
}

#[test]
fn family_inspection_applies_shape_bytes_and_item_limits_to_every_present_member() {
    let dir = TestDirectory::new();
    let path = dir.archive("active.json");
    fs::write(&path, b"[{}]").unwrap();
    let store = ArchiveStore::with_limits(&path, 16, 1).unwrap();
    for bytes in [
        b"".as_slice(),
        b"null",
        b"[]",
        b"[1]",
        b"[{},{}]",
        &[b' '; 17],
    ] {
        fs::write(dir.archive("frozen.json"), bytes).unwrap();
        assert!(
            store
                .inspect_raw_family::<(), ()>(&["frozen.json"], |_, _| panic!("invalid sibling"))
                .is_err()
        );
    }
}
