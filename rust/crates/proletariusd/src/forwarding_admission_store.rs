//! Durable Rust-owned admission progress for forwarding event producers.
//!
//! Cursor advancement is committed only after the forwarding queue has durably
//! accepted every eligible N-wait message in the page. The imported Swift
//! handoff is immutable and identifies the exact legacy checkpoint.

use std::path::Path;

use proletarius_api_contract::SmsForwardingHandoffV1;
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};

const STORE_FILE: &str = "sms-forwarding-admission.json";
const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Store(crate::sms_store_health::GuardedArchive);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Progress {
    schema_version: u16,
    pub(crate) handoff_id: String,
    pub(crate) cursor: u64,
    pub(crate) seeded: bool,
    pub(crate) was_enabled: Option<bool>,
}

impl Store {
    pub(crate) fn open(root: &Path) -> Result<Self, StoreError> {
        Ok(Self(
            ArchiveStore::with_limits(root.join(STORE_FILE), MAX_ARCHIVE_BYTES, 1)?.into(),
        ))
    }

    pub(crate) fn current(&self) -> Result<Option<Progress>, StoreError> {
        // Preserve the legacy never-created None state. This preliminary read
        // grants no admission; any existing progress must pass the strict raw
        // transaction before the producer can enqueue a page. The raw lock
        // selects and validates the latest generation; disappearance fails
        // closed. This does not authenticate external replacement/rollback.
        if !self.0.read(|items| Ok(!items.is_empty()))? {
            return Ok(None);
        }
        self.0
            .transaction_raw(validate_raw_progress, |bytes| {
                Ok(proletarius_store::raw::Mutation::Unchanged(Some(
                    decode_raw_progress(bytes)?,
                )))
            })
            .map_err(raw_failure)
    }

    /// Import the frozen, already-acknowledged Swift snapshot exactly once.
    /// Repeated startup calls are idempotent; a different handoff cannot
    /// silently reset a cursor after Rust has begun admission.
    pub(crate) fn import_handoff(
        &self,
        handoff: &SmsForwardingHandoffV1,
        current_revision: u64,
        forwarding_enabled: bool,
    ) -> Result<Progress, StoreError> {
        handoff
            .validate()
            .map_err(|_| StoreError::Configuration("invalid_request".to_owned()))?;
        self.0.mutate(|items| {
            if items.len() > 1 {
                return Err(corrupt());
            }
            if let Some(value) = items.first() {
                let prior: Progress =
                    serde_json::from_value(value.clone()).map_err(|_| corrupt())?;
                if prior.schema_version != 1 || prior.handoff_id != handoff.handoff_id {
                    return Err(conflict());
                }
                return Ok(prior);
            }

            // Swift cursors are decimal Rust revisions. If Swift had not
            // seeded its consumer yet, start at the current high-water mark;
            // that preserves its no-backfill-on-first-enable behavior.
            let cursor = if handoff.nwait_sms.seeded {
                handoff
                    .nwait_sms
                    .cursor
                    .as_deref()
                    .map(str::parse::<u64>)
                    .transpose()
                    .map_err(|_| StoreError::Configuration("invalid_cursor".to_owned()))?
                    .unwrap_or(current_revision)
            } else {
                current_revision
            };
            let was_enabled = handoff
                .nwait_sms
                .was_enabled
                .or_else(|| (!handoff.nwait_sms.seeded).then_some(forwarding_enabled));
            let progress = Progress {
                schema_version: 1,
                handoff_id: handoff.handoff_id.clone(),
                cursor,
                seeded: true,
                was_enabled,
            };
            *items = vec![serde_json::to_value(&progress).map_err(StoreError::Json)?];
            Ok(progress)
        })
    }

    pub(crate) fn advance(
        &self,
        handoff_id: &str,
        cursor: u64,
        was_enabled: bool,
    ) -> Result<Progress, StoreError> {
        self.0
            .transaction_raw(validate_raw_progress, |bytes| {
                // Validation is repeated in this pure decoder; it performs no
                // IO and cannot advance a cursor or enqueue an SMS itself.
                let mut progress = decode_raw_progress(bytes)?;
                if progress.handoff_id != handoff_id || cursor < progress.cursor {
                    return Err(conflict());
                }
                if progress.cursor == cursor && progress.was_enabled == Some(was_enabled) {
                    return Ok(proletarius_store::raw::Mutation::Unchanged(progress));
                }
                progress.cursor = cursor;
                progress.was_enabled = Some(was_enabled);
                let bytes = serde_json::to_vec(&[&progress]).map_err(StoreError::Json)?;
                Ok(proletarius_store::raw::Mutation::Replace {
                    bytes,
                    result: progress,
                })
            })
            .map_err(raw_failure)
    }
}

// Raw kernel checks duplicate keys, nonempty shape and limits before this
// schema decoder sees original bytes. Legacy v1 optional fields stay accepted.
fn decode_raw_progress(bytes: &[u8]) -> Result<Progress, StoreError> {
    let mut items: Vec<Progress> = serde_json::from_slice(bytes).map_err(|_| corrupt())?;
    if items.len() != 1 {
        return Err(corrupt());
    }
    let progress = items.pop().expect("one validated progress");
    if progress.schema_version != 1 || progress.handoff_id.is_empty() || !progress.seeded {
        return Err(corrupt());
    }
    Ok(progress)
}

fn validate_raw_progress(bytes: &[u8]) -> Result<(), StoreError> {
    decode_raw_progress(bytes).map(|_| ())
}

fn raw_failure(
    error: proletarius_store::raw::TransactionError<StoreError, StoreError>,
) -> StoreError {
    use proletarius_store::raw::{CommitStage, TransactionError::*};
    match error {
        Rejected(error) => error,
        // Keep the uncertain commit classification at the daemon boundary.
        // Neither code authorizes retry, delivery or cursor rollback.
        Commit(error) if error.stage == CommitStage::AfterRename => {
            StoreError::Configuration("forwarding_admission_commit_unknown".into())
        }
        Commit(_) => StoreError::Configuration("forwarding_admission_commit_failed".into()),
        _ => crate::sms_store_health::unavailable(),
    }
}

fn corrupt() -> StoreError {
    StoreError::Configuration("admission_ledger_corrupt".to_owned())
}

fn conflict() -> StoreError {
    StoreError::Configuration("admission_state_conflict".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    struct TestRoot(std::path::PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let switch_root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .expect("Switch root from Cargo manifest");
            let parent = switch_root.join("build/tmp/forwarding-admission-tests");
            fs::create_dir_all(&parent).unwrap();
            let root = parent.join(crate::multistandby_store::random_token().unwrap());
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn handoff(
        seeded: bool,
        cursor: Option<&str>,
        was_enabled: Option<bool>,
    ) -> SmsForwardingHandoffV1 {
        serde_json::from_value(json!({
            "schema_version": 1,
            "handoff_id": "123e4567-e89b-12d3-a456-426614174000",
            "device_sms": {"accepted_notification_ids": [], "was_enabled": true},
            "nwait_sms": {"cursor": cursor, "seeded": seeded, "was_enabled": was_enabled}
        }))
        .unwrap()
    }

    #[test]
    fn unseeded_handoff_starts_at_high_water_and_is_idempotent() {
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        let source = handoff(false, None, None);
        let first = store.import_handoff(&source, 42, true).unwrap();
        let reopened = Store::open(&root.0).unwrap();
        let second = reopened.import_handoff(&source, 99, false).unwrap();
        assert_eq!(first, second);
        assert_eq!(second.cursor, 42);
        assert_eq!(second.was_enabled, Some(true));
    }

    #[test]
    fn seeded_handoff_preserves_cursor_and_disabled_state() {
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        let imported = store
            .import_handoff(&handoff(true, Some("37"), Some(false)), 42, true)
            .unwrap();
        assert_eq!(imported.cursor, 37);
        assert_eq!(imported.was_enabled, Some(false));
    }

    #[test]
    fn advancement_is_monotonic_and_bound_to_the_imported_handoff() {
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        let source = handoff(true, Some("37"), Some(true));
        store.import_handoff(&source, 42, true).unwrap();
        assert!(store.advance(&source.handoff_id, 36, true).is_err());
        assert!(store.advance("other-handoff", 38, true).is_err());
        let next = store.advance(&source.handoff_id, 38, false).unwrap();
        assert_eq!(next.cursor, 38);
        assert_eq!(next.was_enabled, Some(false));
    }
}

#[cfg(test)]
#[path = "forwarding_admission_store/raw_tests.rs"]
mod raw_tests;
