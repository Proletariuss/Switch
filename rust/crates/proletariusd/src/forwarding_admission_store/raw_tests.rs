use super::*;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::var_os("SWITCH_BUILD_DIR")
            .map(std::path::PathBuf::from)
            .unwrap()
            .join(format!(
                "tmp/b1b-progress-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn seed(&self, bytes: &[u8]) -> Store {
        fs::write(self.0.join(STORE_FILE), bytes).unwrap();
        Store::open(&self.0).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
const ORIGINAL: &[u8] = b"[ { \"schema_version\":1, \"handoff_id\":\"fixture\",\"cursor\":9,\"seeded\":true,\"was_enabled\":true,\"future_metadata\":\"preserve\" } ]\n";

#[test]
fn raw_current_and_repeated_advance_preserve_v1_bytes_and_inode() {
    let f = Fixture::new();
    let store = f.seed(ORIGINAL);
    let p = f.0.join(STORE_FILE);
    let ino = fs::metadata(&p).unwrap().ino();
    assert_eq!(store.current().unwrap().unwrap().cursor, 9);
    assert_eq!(store.advance("fixture", 9, true).unwrap().cursor, 9);
    assert_eq!(fs::read(&p).unwrap(), ORIGINAL);
    assert_eq!(fs::metadata(p).unwrap().ino(), ino);
}
#[test]
fn raw_advance_keeps_monotonic_cursor_and_legacy_optional_field_compatibility() {
    let f = Fixture::new();
    let store =
        f.seed(br#"[{"schema_version":1,"handoff_id":"fixture","cursor":9,"seeded":true}]"#);
    assert_eq!(store.current().unwrap().unwrap().was_enabled, None);
    assert_eq!(store.advance("fixture", 10, true).unwrap().cursor, 10);
    assert!(
        matches!(store.advance("fixture",9,true),Err(StoreError::Configuration(code)) if code=="admission_state_conflict")
    );
    assert!(
        matches!(store.advance("other",11,true),Err(StoreError::Configuration(code)) if code=="admission_state_conflict")
    );
    assert_eq!(store.0.status().state, "ready");
    let reopen = Store::open(&f.0).unwrap();
    assert_eq!(reopen.current().unwrap().unwrap().cursor, 10);
}
#[test]
fn raw_current_rejects_duplicate_before_producer_can_obtain_progress() {
    for bytes in [br#"[{"schema_version":1,"handoff_id":"fixture","cursor":1,"cursor":9,"seeded":true}]"#.as_slice(),
        br#"[{"schema_version":1,"handoff_id":"fixture","cursor":9,"seeded":true,"meta":{"x":1,"\u0078":2}}]"#.as_slice()] {
        let f=Fixture::new();let store=f.seed(bytes);
        assert!(store.current().is_err()); assert_eq!(store.0.status().reason,Some("store_corrupt"));
        fs::write(f.0.join(STORE_FILE),ORIGINAL).unwrap();
        assert!(store.current().is_err()); assert!(store.advance("fixture",10,true).is_err());
    }
}
#[test]
fn raw_schema_failure_blocks_advance_until_restart() {
    let f = Fixture::new();
    let store =
        f.seed(br#"[{"schema_version":1,"handoff_id":"fixture","cursor":9,"seeded":false}]"#);
    assert!(store.current().is_err());
    assert_eq!(store.0.status().reason, Some("store_corrupt"));
    assert!(store.advance("fixture", 10, true).is_err());
}
#[test]
fn raw_missing_after_observation_is_not_initialized_or_retried() {
    let f = Fixture::new();
    let store = f.seed(ORIGINAL);
    store.current().unwrap();
    fs::remove_file(f.0.join(STORE_FILE)).unwrap();
    assert!(store.advance("fixture", 10, true).is_err());
    assert!(!f.0.join(STORE_FILE).exists());
    assert!(Store::open(&f.0).unwrap().current().is_err());
}
#[test]
fn raw_never_created_current_keeps_none_and_does_not_create_authority() {
    let f = Fixture::new();
    let store = Store::open(&f.0).unwrap();
    assert!(store.current().unwrap().is_none());
    assert!(!f.0.join(STORE_FILE).exists());
}
#[test]
fn raw_commit_error_mapping_preserves_before_and_after_rename_classification() {
    use proletarius_store::raw::{CommitError, CommitStage, TransactionError};
    for (stage, expected) in [
        (
            CommitStage::BeforeRename,
            "forwarding_admission_commit_failed",
        ),
        (
            CommitStage::AfterRename,
            "forwarding_admission_commit_unknown",
        ),
    ] {
        assert!(
            matches!(raw_failure(TransactionError::Commit(CommitError { stage,
            source:StoreError::Io(std::io::Error::from_raw_os_error(28)) })),StoreError::Configuration(code) if code==expected)
        );
    }
}

#[test]
fn raw_unknown_schema_never_becomes_fresh_legacy_authority() {
    let f = Fixture::new();
    let bytes = br#"[{"schema_version":2,"handoff_id":"fixture","cursor":99,"seeded":true,"was_enabled":true}]"#;
    let store = f.seed(bytes);
    assert!(store.current().is_err());
    assert_eq!(store.0.status().reason, Some("store_corrupt"));
    assert!(store.advance("fixture", 100, true).is_err());
    assert_eq!(fs::read(f.0.join(STORE_FILE)).unwrap(), bytes);
    assert!(Store::open(&f.0).unwrap().current().is_err());
}

#[test]
fn present_empty_array_is_corruption_and_cannot_reseed_import_or_expose_progress() {
    let handoff: SmsForwardingHandoffV1 = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "handoff_id": "123e4567-e89b-12d3-a456-426614174000",
        "device_sms": {"accepted_notification_ids": [], "was_enabled": true},
        "nwait_sms": {"cursor": "9", "seeded": true, "was_enabled": true}
    }))
    .unwrap();
    for after_valid_progress in [false, true] {
        let f = Fixture::new();
        let store = Store::open(&f.0).unwrap();
        if after_valid_progress {
            store.import_handoff(&handoff, 9, true).unwrap();
            assert_eq!(store.current().unwrap().unwrap().cursor, 9);
        }
        fs::write(f.0.join(STORE_FILE), b"[]").unwrap();
        // Err is distinct from the never-created None initialization state.
        assert!(store.current().is_err());
        assert_eq!(store.0.status().reason, Some("store_corrupt"));
        assert!(store.import_handoff(&handoff, 99, true).is_err());
        assert!(store.advance(&handoff.handoff_id, 99, true).is_err());
        assert_eq!(fs::read(f.0.join(STORE_FILE)).unwrap(), b"[]");
    }
}
