use super::*;
use proletarius_store::raw::{CommitError, CommitStage};
use serde_json::json;
use std::{fs, sync::atomic::AtomicU64};

struct Fixture {
    root: std::path::PathBuf,
    archive: GuardedArchive,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::var_os("SWITCH_BUILD_DIR")
            .map(std::path::PathBuf::from)
            .expect("workspace test directory")
            .join(format!(
                "tmp/b1b-adapter-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        let archive: GuardedArchive = ArchiveStore::new(root.join("ledger.json")).into();
        archive.replace(vec![json!({"state":"prepared"})]).unwrap();
        Self { root, archive }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn valid(_: &[u8]) -> Result<(), StoreError> {
    Ok(())
}
fn refused() -> StoreError {
    StoreError::Configuration("admission_state_conflict".into())
}

#[test]
fn raw_unchanged_preserves_original_bytes_and_inode() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    let bytes = b"[ { \"state\" : \"prepared\", \"unknown\": [1,2] } ]\n";
    fs::write(f.archive.path(), bytes).unwrap();
    let inode = fs::metadata(f.archive.path()).unwrap().ino();
    assert_eq!(
        f.archive
            .transaction_raw(valid, |_| Ok(Mutation::Unchanged(7)))
            .unwrap(),
        7
    );
    assert_eq!(fs::read(f.archive.path()).unwrap(), bytes);
    assert_eq!(fs::metadata(f.archive.path()).unwrap().ino(), inode);
}

#[test]
fn raw_duplicate_evidence_closes_both_raw_and_legacy_admission_after_repair() {
    let f = Fixture::new();
    fs::write(
        f.archive.path(),
        br#"[{"state":"prepared","state":"submitting"}]"#,
    )
    .unwrap();
    assert!(matches!(
        f.archive
            .transaction_raw(valid, |_| -> Result<Mutation<()>, _> {
                panic!("invalid bytes reached planner")
            }),
        Err(TransactionError::Current(_))
    ));
    assert_eq!(f.archive.status().reason, Some("store_corrupt"));
    fs::write(f.archive.path(), br#"[{"state":"prepared"}]"#).unwrap();
    assert!(
        f.archive
            .mutate::<()>(|_| panic!("legacy path bypassed raw latch"))
            .is_err()
    );
    assert!(
        f.archive
            .transaction_raw(valid, |_| -> Result<Mutation<()>, _> {
                panic!("raw path bypassed raw latch")
            })
            .is_err()
    );
}

#[test]
fn raw_pure_business_refusal_does_not_close_latch_or_rewrite() {
    let f = Fixture::new();
    let before = fs::read(f.archive.path()).unwrap();
    assert!(matches!(
        f.archive
            .transaction_raw(valid, |_| -> Result<Mutation<()>, _> { Err(refused()) }),
        Err(TransactionError::Rejected(_))
    ));
    assert_eq!(f.archive.status().state, "ready");
    assert_eq!(fs::read(f.archive.path()).unwrap(), before);
    f.archive
        .transaction_raw(valid, |_| Ok(Mutation::Unchanged(())))
        .unwrap();
}

#[test]
fn raw_current_configuration_and_schema_errors_latch_even_unlisted_codes() {
    for schema in [false, true] {
        let f = Fixture::new();
        let result: Result<(), _> = f.archive.checked_raw(|| {
            Err(if schema {
                TransactionError::CurrentSchema(refused())
            } else {
                TransactionError::Current(StoreError::Configuration("archive_lock_replaced".into()))
            })
        });
        assert!(result.is_err());
        assert_eq!(f.archive.status().reason, Some("store_corrupt"));
    }
}

#[test]
fn raw_bad_proposal_latches_worker_fault_before_write() {
    let f = Fixture::new();
    let before = fs::read(f.archive.path()).unwrap();
    assert!(matches!(
        f.archive.transaction_raw(valid, |_| Ok(Mutation::Replace {
            bytes: b"[]".to_vec(),
            result: true
        })),
        Err(TransactionError::Proposal(_))
    ));
    assert_eq!(f.archive.status().reason, Some("store_worker_failed"));
    assert_eq!(fs::read(f.archive.path()).unwrap(), before);
}

#[test]
fn raw_proposal_schema_failure_latches_worker_fault() {
    let f = Fixture::new();
    assert!(matches!(
        f.archive.transaction_raw(
            |bytes| {
                if bytes.windows(8).any(|w| w == b"rejected") {
                    Err(refused())
                } else {
                    Ok(())
                }
            },
            |_| Ok(Mutation::Replace {
                bytes: br#"[{"state":"rejected"}]"#.to_vec(),
                result: true
            })
        ),
        Err(TransactionError::ProposalSchema(_))
    ));
    assert_eq!(f.archive.status().reason, Some("store_worker_failed"));
}

#[test]
fn raw_commit_error_keeps_stage_and_never_returns_proposed_grant() {
    for stage in [CommitStage::BeforeRename, CommitStage::AfterRename] {
        let f = Fixture::new();
        let result: Result<bool, _> = f.archive.checked_raw(|| {
            if stage == CommitStage::AfterRename {
                // Real replacement, synthetic acknowledgement loss. Raw kernel
                // syscall faults are separately exercised in proletarius-store.
                f.archive
                    .archive
                    .transaction_raw(valid, |_| -> Result<Mutation<bool>, StoreError> {
                        Ok(Mutation::Replace {
                            bytes: br#"[{"state":"submitting"}]"#.to_vec(),
                            result: true,
                        })
                    })
                    .unwrap();
            }
            Err(TransactionError::Commit(CommitError {
                stage,
                source: StoreError::Io(std::io::Error::from_raw_os_error(28)),
            }))
        });
        assert!(matches!(result, Err(TransactionError::Commit(ref e)) if e.stage == stage));
        assert_eq!(f.archive.status().reason, Some("store_io_failed"));
        assert!(
            f.archive
                .transaction_raw(valid, |_| -> Result<Mutation<()>, _> {
                    panic!("uncertain result reopened admission")
                })
                .is_err()
        );
        let persisted = String::from_utf8(fs::read(f.archive.path()).unwrap()).unwrap();
        assert!(persisted.contains(if stage == CommitStage::AfterRename {
            "submitting"
        } else {
            "prepared"
        }));
        let reopened: GuardedArchive = ArchiveStore::new(f.archive.path()).into();
        reopened
            .transaction_raw(valid, |bytes| {
                assert_eq!(bytes, persisted.as_bytes());
                Ok(Mutation::Unchanged(()))
            })
            .unwrap();
    }
}

#[test]
fn raw_failure_serializes_fault_publication_before_queued_legacy_claim() {
    let f = Fixture::new();
    let (entered, entering) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let first = f.archive.clone();
    let failing = std::thread::spawn(move || {
        first.checked_raw::<()>(|| {
            entered.send(()).unwrap();
            released.recv().unwrap();
            Err(TransactionError::Commit(CommitError {
                stage: CommitStage::AfterRename,
                source: StoreError::Io(std::io::Error::from_raw_os_error(28)),
            }))
        })
    });
    entering.recv().unwrap();
    let second = f.archive.clone();
    let queued =
        std::thread::spawn(move || second.mutate::<()>(|_| panic!("claim ran after fault")));
    release.send(()).unwrap();
    assert!(failing.join().unwrap().is_err());
    assert!(queued.join().unwrap().is_err());
}

#[test]
fn raw_callback_panic_poison_closes_legacy_admission() {
    let f = Fixture::new();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = f
                .archive
                .transaction_raw(valid, |_| -> Result<Mutation<()>, StoreError> {
                    panic!("synthetic planner panic")
                });
        }))
        .is_err()
    );
    assert_eq!(f.archive.status().reason, Some("store_worker_failed"));
    assert!(f.archive.mutate::<()>(|_| panic!("poison bypass")).is_err());
}

#[test]
fn raw_unreviewed_configuration_rejection_is_not_assumed_safe() {
    let f = Fixture::new();
    assert!(
        f.archive
            .transaction_raw(valid, |_| -> Result<Mutation<()>, _> {
                Err(StoreError::Configuration("admission_ledger_corrupt".into()))
            })
            .is_err()
    );
    assert_eq!(f.archive.status().reason, Some("store_worker_failed"));
}

#[test]
fn raw_after_rename_ack_loss_reopens_at_new_forwarding_cursor_without_old_page() {
    let f = Fixture::new();
    let archive: GuardedArchive =
        ArchiveStore::new(f.root.join("sms-forwarding-admission.json")).into();
    archive.replace(vec![json!({"schema_version":1,"handoff_id":"fixture","cursor":9,"seeded":true,"was_enabled":true})]).unwrap();
    // Producer has already durably queued revision 10 before cursor advance.
    let delivered = AtomicU64::new(1);
    let result: Result<(),_>=archive.checked_raw(|| {
        archive.archive.transaction_raw(valid, |_| -> Result<Mutation<()>,StoreError> {
            Ok(Mutation::Replace { bytes: br#"[{"schema_version":1,"handoff_id":"fixture","cursor":10,"seeded":true,"was_enabled":true}]"#.to_vec(), result: () })
        }).unwrap();
        Err(TransactionError::Commit(CommitError { stage:CommitStage::AfterRename,
            source:StoreError::Io(std::io::Error::from_raw_os_error(28)) }))
    });
    assert!(
        matches!(result,Err(TransactionError::Commit(ref e)) if e.stage==CommitStage::AfterRename)
    );
    assert!(archive.read(|_| Ok(())).is_err());
    // New process/restart uses exact visible durable progress, not an empty
    // or old-cursor fallback. The next selected page excludes revision 10.
    let reopened = crate::forwarding_admission_store::Store::open(&f.root).unwrap();
    let cursor = reopened.current().unwrap().unwrap().cursor;
    for revision in [10] {
        if revision > cursor {
            delivered.fetch_add(1, Ordering::SeqCst);
        }
    }
    assert_eq!(cursor, 10);
    assert_eq!(delivered.load(Ordering::SeqCst), 1);
    assert_eq!(reopened.advance("fixture", 10, true).unwrap().cursor, 10);
    assert!(reopened.advance("fixture", 9, true).is_err());
}
