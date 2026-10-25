//! Durable device-SMS forwarding admission state imported from the Swift
//! notification producer. The message queue remains the delivery authority;
//! this ledger preserves first-enable/disabled-period behavior and migration
//! compatibility without storing SMS bodies.

use std::collections::BTreeSet;
use std::path::Path;

use proletarius_api_contract::SmsForwardingHandoffV1;
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};

const STORE_FILE: &str = "device-sms-forwarding-admission.json";
const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ACCEPTED_IDS: usize = 100_000;
const DELETED_ID_TAIL: usize = 2_000;

#[derive(Clone)]
pub(crate) struct Store(crate::sms_store_health::GuardedArchive);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Progress {
    schema_version: u16,
    pub(crate) handoff_id: Option<String>,
    pub(crate) accepted_notification_ids: Vec<String>,
    pub(crate) was_enabled: Option<bool>,
    pub(crate) skip_next_snapshot: bool,
}

impl Store {
    pub(crate) fn open(root: &Path) -> Result<Self, StoreError> {
        let archive = ArchiveStore::with_limits(root.join(STORE_FILE), MAX_ARCHIVE_BYTES, 1)?;
        // Previous Rust versions could persist `[]` when forwarding was first
        // enabled before the Swift checkpoint handoff. That is the valid
        // empty-ledger state, so normalize it under the archive lock before
        // GuardedArchive's fail-closed check sees it as a present empty file.
        archive.remove_if_empty()?;
        Ok(Self(archive.into()))
    }

    pub(crate) fn current(&self) -> Result<Option<Progress>, StoreError> {
        self.0.read(|items| {
            if items.len() > 1 {
                return Err(corrupt());
            }
            let Some(value) = items.first() else {
                return Ok(None);
            };
            let progress: Progress =
                serde_json::from_value(value.clone()).map_err(|_| corrupt())?;
            validate_progress(&progress)?;
            Ok(Some(progress))
        })
    }

    pub(crate) fn import_handoff(
        &self,
        handoff: &SmsForwardingHandoffV1,
    ) -> Result<Progress, StoreError> {
        handoff
            .validate()
            .map_err(|_| StoreError::Configuration("invalid_request".to_owned()))?;
        self.0.mutate(|items| {
            if items.len() > 1 {
                return Err(corrupt());
            }
            let mut progress = if let Some(value) = items.first() {
                serde_json::from_value::<Progress>(value.clone()).map_err(|_| corrupt())?
            } else {
                Progress {
                    schema_version: 1,
                    handoff_id: None,
                    accepted_notification_ids: Vec::new(),
                    was_enabled: None,
                    skip_next_snapshot: false,
                }
            };
            validate_progress(&progress)?;
            match progress.handoff_id.as_deref() {
                Some(id) if id != handoff.handoff_id => return Err(conflict()),
                Some(_) => return Ok(progress),
                None => {}
            }

            let mut accepted: BTreeSet<String> =
                progress.accepted_notification_ids.into_iter().collect();
            accepted.extend(handoff.device_sms.accepted_notification_ids.iter().cloned());
            if accepted.len() > MAX_ACCEPTED_IDS {
                return Err(too_many_ids());
            }
            progress.handoff_id = Some(handoff.handoff_id.clone());
            progress.accepted_notification_ids = accepted.into_iter().collect();
            progress.was_enabled = progress.was_enabled.or(handoff.device_sms.was_enabled);
            *items = vec![serde_json::to_value(&progress).map_err(StoreError::Json)?];
            Ok(progress)
        })
    }

    /// Configuration changes are observed even when the mailbox is empty or
    /// the UI is closed. Re-enabling arms one high-water snapshot so messages
    /// received during the disabled period are not replayed.
    pub(crate) fn note_configuration_enabled(&self, enabled: bool) -> Result<(), StoreError> {
        self.0.mutate_if_changed(|items| {
            if items.len() > 1 {
                return Err(corrupt());
            }
            if items.is_empty() {
                if enabled {
                    return Ok(((), false));
                }
                *items = vec![
                    serde_json::to_value(Progress {
                        schema_version: 1,
                        handoff_id: None,
                        accepted_notification_ids: Vec::new(),
                        was_enabled: Some(false),
                        skip_next_snapshot: false,
                    })
                    .map_err(StoreError::Json)?,
                ];
                return Ok(((), true));
            }

            let mut progress: Progress =
                serde_json::from_value(items[0].clone()).map_err(|_| corrupt())?;
            validate_progress(&progress)?;
            let original = progress.clone();
            if enabled {
                if progress.was_enabled == Some(false) {
                    progress.skip_next_snapshot = true;
                }
            } else {
                progress.was_enabled = Some(false);
                progress.skip_next_snapshot = false;
            }
            let changed = progress != original;
            items[0] = serde_json::to_value(progress).map_err(StoreError::Json)?;
            Ok(((), changed))
        })
    }

    pub(crate) fn skip_snapshot(
        &self,
        handoff_id: &str,
        current_ids: &[String],
        enabled: bool,
    ) -> Result<Progress, StoreError> {
        self.update(handoff_id, current_ids, &[], enabled, false, true, true)
    }

    pub(crate) fn accept_batch(
        &self,
        handoff_id: &str,
        accepted_ids: &[String],
        current_ids: &[String],
    ) -> Result<Progress, StoreError> {
        self.update(
            handoff_id,
            current_ids,
            accepted_ids,
            true,
            false,
            false,
            true,
        )
    }

    pub(crate) fn accept_batch_preserving(
        &self,
        handoff_id: &str,
        accepted_ids: &[String],
    ) -> Result<Progress, StoreError> {
        self.update(handoff_id, &[], accepted_ids, true, false, false, false)
    }

    fn update(
        &self,
        handoff_id: &str,
        current_ids: &[String],
        accepted_ids: &[String],
        enabled: bool,
        skip_next_snapshot: bool,
        baseline: bool,
        prune_to_current: bool,
    ) -> Result<Progress, StoreError> {
        self.0.mutate(|items| {
            if items.len() != 1 {
                return Err(corrupt());
            }
            let mut progress: Progress =
                serde_json::from_value(items[0].clone()).map_err(|_| corrupt())?;
            validate_progress(&progress)?;
            if progress.handoff_id.as_deref() != Some(handoff_id) {
                return Err(conflict());
            }
            let mut accepted: BTreeSet<String> =
                progress.accepted_notification_ids.into_iter().collect();
            if baseline {
                accepted.extend(current_ids.iter().cloned());
            }
            accepted.extend(accepted_ids.iter().cloned());
            let retained = if prune_to_current {
                let current: BTreeSet<_> = current_ids.iter().cloned().collect();
                let mut retained: BTreeSet<_> = accepted.intersection(&current).cloned().collect();
                let deleted: Vec<_> = accepted.difference(&current).cloned().collect();
                let tail_limit =
                    DELETED_ID_TAIL.min(MAX_ACCEPTED_IDS.saturating_sub(retained.len()));
                retained.extend(deleted.into_iter().rev().take(tail_limit));
                retained
            } else {
                if accepted.len() > MAX_ACCEPTED_IDS {
                    return Err(too_many_ids());
                }
                accepted
            };
            progress.accepted_notification_ids = retained.into_iter().collect();
            progress.was_enabled = Some(enabled);
            progress.skip_next_snapshot = skip_next_snapshot;
            items[0] = serde_json::to_value(&progress).map_err(StoreError::Json)?;
            Ok(progress)
        })
    }
}

fn validate_progress(progress: &Progress) -> Result<(), StoreError> {
    if progress.schema_version != 1
        || progress.accepted_notification_ids.len() > MAX_ACCEPTED_IDS
        || progress
            .accepted_notification_ids
            .iter()
            .any(|id| !is_digest(id))
        || progress
            .accepted_notification_ids
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || progress
            .handoff_id
            .as_ref()
            .is_some_and(|id| !is_canonical_uuid(id))
    {
        return Err(corrupt());
    }
    Ok(())
}

pub(crate) fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value == value.to_ascii_lowercase()
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

fn corrupt() -> StoreError {
    StoreError::Configuration("device_forwarding_ledger_corrupt".to_owned())
}

fn conflict() -> StoreError {
    StoreError::Configuration("device_forwarding_state_conflict".to_owned())
}

fn too_many_ids() -> StoreError {
    StoreError::Configuration("device_forwarding_id_limit".to_owned())
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
            let parent = switch_root.join("build/tmp/device-forwarding-admission-tests");
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

    fn handoff(was_enabled: Option<bool>) -> SmsForwardingHandoffV1 {
        serde_json::from_value(json!({
            "schema_version": 1,
            "handoff_id": "123e4567-e89b-12d3-a456-426614174000",
            "device_sms": {
                "accepted_notification_ids": ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
                "was_enabled": was_enabled
            },
            "nwait_sms": {"cursor": null, "seeded": false, "was_enabled": null}
        }))
        .unwrap()
    }

    #[test]
    fn handoff_import_merges_disabled_configuration_and_is_idempotent() {
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        store.note_configuration_enabled(false).unwrap();
        let imported = store.import_handoff(&handoff(Some(true))).unwrap();
        assert_eq!(imported.was_enabled, Some(false));
        assert_eq!(imported.accepted_notification_ids.len(), 1);
        assert_eq!(
            store.import_handoff(&handoff(Some(true))).unwrap(),
            imported
        );
    }

    #[test]
    fn enabling_an_empty_ledger_does_not_persist_an_invalid_empty_archive() {
        let root = TestRoot::new();
        let path = root.0.join(STORE_FILE);
        let store = Store::open(&root.0).unwrap();
        store.note_configuration_enabled(true).unwrap();
        assert!(
            !path.exists(),
            "a no-op enable must not create an empty ledger"
        );

        fs::write(&path, b"[]").unwrap();
        drop(store);
        let reopened = Store::open(&root.0).unwrap();
        assert!(
            !path.exists(),
            "legacy empty ledger must normalize to no state"
        );
        assert_eq!(reopened.current().unwrap(), None);
        reopened.note_configuration_enabled(false).unwrap();
        reopened.note_configuration_enabled(true).unwrap();
        let progress = reopened.current().unwrap().unwrap();
        assert_eq!(progress.was_enabled, Some(false));
        assert!(progress.skip_next_snapshot);
    }

    #[test]
    fn reenable_arms_one_high_water_snapshot_and_deduplicates_ids() {
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        store.import_handoff(&handoff(Some(false))).unwrap();
        store.note_configuration_enabled(true).unwrap();
        let high_water =
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned();
        let state = store
            .skip_snapshot(
                "123e4567-e89b-12d3-a456-426614174000",
                std::slice::from_ref(&high_water),
                true,
            )
            .unwrap();
        assert_eq!(state.was_enabled, Some(true));
        assert!(!state.skip_next_snapshot);
        assert_eq!(state.accepted_notification_ids.len(), 2);
        let next = store
            .accept_batch(
                "123e4567-e89b-12d3-a456-426614174000",
                std::slice::from_ref(&high_water),
                std::slice::from_ref(&high_water),
            )
            .unwrap();
        assert_eq!(next.accepted_notification_ids.len(), 2);
    }

    #[test]
    fn mismatched_handoff_and_corrupt_digest_fail_closed() {
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        store.import_handoff(&handoff(None)).unwrap();
        let other: SmsForwardingHandoffV1 = serde_json::from_value(json!({
            "schema_version": 1,
            "handoff_id": "123e4567-e89b-12d3-a456-426614174001",
            "device_sms": {"accepted_notification_ids": [], "was_enabled": null},
            "nwait_sms": {"cursor": null, "seeded": false, "was_enabled": null}
        }))
        .unwrap();
        assert!(store.import_handoff(&other).is_err());

        let path = root.0.join(STORE_FILE);
        let mut records: Vec<serde_json::Value> =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        records[0]["accepted_notification_ids"] = json!(["invalid"]);
        fs::write(&path, serde_json::to_vec(&records).unwrap()).unwrap();
        assert!(
            store
                .0
                .read(|items| {
                    let value = items.first().ok_or_else(corrupt)?;
                    let progress: Progress =
                        serde_json::from_value(value.clone()).map_err(|_| corrupt())?;
                    validate_progress(&progress)
                })
                .is_err()
        );
    }
}
