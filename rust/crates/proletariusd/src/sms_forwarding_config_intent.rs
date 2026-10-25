//! Durable monotonic identity for SMS-forwarding configuration snapshots.
//!
//! Provider credentials remain in the GUI Keychain and the encrypted queue.
//! This ledger stores only a request identity, a client ordering sequence and
//! a one-way payload fingerprint.  It lets Rust reject a late, older GUI
//! snapshot after either side restarts without persisting any credential.

use std::path::Path;

use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};

const STORE_FILE: &str = "sms-forwarding-configuration-intent.json";

#[derive(Clone)]
pub(crate) struct Store(crate::sms_store_health::GuardedArchive);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Record {
    schema_version: u16,
    client_request_id: String,
    client_sequence: u64,
    revision: u64,
    fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Admission {
    pub(crate) revision: u64,
    pub(crate) repeated: bool,
}

impl Store {
    pub(crate) fn new(root: &Path) -> Self {
        Self(ArchiveStore::new(root.join(STORE_FILE)).into())
    }

    pub(crate) fn admit(
        &self,
        client_request_id: &str,
        client_sequence: u64,
        fingerprint: &str,
    ) -> Result<Admission, StoreError> {
        validate_request(client_request_id, client_sequence, fingerprint)?;
        self.0.mutate(|items| {
            if items.len() > 1 {
                return Err(corrupt());
            }
            let current = items
                .first()
                .map(|value| {
                    let record: Record =
                        serde_json::from_value(value.clone()).map_err(|_| corrupt())?;
                    validate_record(&record)?;
                    Ok(record)
                })
                .transpose()?;

            if let Some(current) = current {
                if current.client_request_id == client_request_id {
                    if current.client_sequence != client_sequence
                        || current.fingerprint != fingerprint
                    {
                        return Err(failure("configuration_intent_request_id_conflict"));
                    }
                    return Ok(Admission {
                        revision: current.revision,
                        repeated: true,
                    });
                }
                if client_sequence <= current.client_sequence {
                    return Err(failure("configuration_intent_stale"));
                }
                let revision = current
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| failure("configuration_intent_revision_exhausted"))?;
                let record = Record {
                    schema_version: 1,
                    client_request_id: client_request_id.to_owned(),
                    client_sequence,
                    revision,
                    fingerprint: fingerprint.to_owned(),
                };
                *items = vec![serde_json::to_value(record).map_err(StoreError::Json)?];
                return Ok(Admission {
                    revision,
                    repeated: false,
                });
            }

            let record = Record {
                schema_version: 1,
                client_request_id: client_request_id.to_owned(),
                client_sequence,
                revision: 1,
                fingerprint: fingerprint.to_owned(),
            };
            *items = vec![serde_json::to_value(record).map_err(StoreError::Json)?];
            Ok(Admission {
                revision: 1,
                repeated: false,
            })
        })
    }
}

fn validate_request(
    client_request_id: &str,
    client_sequence: u64,
    fingerprint: &str,
) -> Result<(), StoreError> {
    if client_request_id.is_empty()
        || client_request_id.len() > 128
        || client_request_id.trim() != client_request_id
        || client_request_id.chars().any(char::is_control)
        || client_sequence == 0
        || fingerprint.len() != 64
        || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(failure("invalid_configuration_intent"));
    }
    Ok(())
}

fn validate_record(record: &Record) -> Result<(), StoreError> {
    if record.schema_version != 1 || record.revision == 0 {
        return Err(corrupt());
    }
    validate_request(
        &record.client_request_id,
        record.client_sequence,
        &record.fingerprint,
    )
}

fn failure(code: &str) -> StoreError {
    StoreError::Configuration(code.to_owned())
}

fn corrupt() -> StoreError {
    failure("invalid_store_schema")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn root() -> std::path::PathBuf {
        let base = std::path::PathBuf::from(
            std::env::var_os("SWITCH_BUILD_DIR").expect("workspace build directory"),
        )
        .join("tmp/sms-forwarding-config-intent-tests");
        fs::create_dir_all(&base).unwrap();
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).unwrap();
        let path = base.join(
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        );
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn newer_sequence_wins_when_requests_arrive_in_reverse_ui_order() {
        let root = root();
        let store = Store::new(&root);
        let newer = store.admit("request-b", 2, &"b".repeat(64)).unwrap();
        assert_eq!(newer.revision, 1);
        assert!(matches!(
            store.admit("request-a", 1, &"a".repeat(64)).unwrap_err(),
            StoreError::Configuration(code) if code == "configuration_intent_stale"
        ));
        assert_eq!(
            store.admit("request-b", 2, &"b".repeat(64)).unwrap(),
            Admission {
                revision: 1,
                repeated: true
            }
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gui_and_daemon_restart_reuse_exact_request_but_reject_changed_payload() {
        let root = root();
        let first = Store::new(&root)
            .admit("durable-request", 41, &"a".repeat(64))
            .unwrap();
        drop(Store::new(&root));
        let reopened = Store::new(&root);
        assert_eq!(
            reopened
                .admit("durable-request", 41, &"a".repeat(64))
                .unwrap(),
            Admission {
                revision: first.revision,
                repeated: true
            }
        );
        assert!(matches!(
            reopened
                .admit("durable-request", 41, &"c".repeat(64))
                .unwrap_err(),
            StoreError::Configuration(code)
                if code == "configuration_intent_request_id_conflict"
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
