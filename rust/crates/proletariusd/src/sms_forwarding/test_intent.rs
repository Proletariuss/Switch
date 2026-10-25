//! Durable at-most-once admission for a caller's fixed provider test.
//! No credentials or notification contents are stored. Claims survive waiter
//! cancellation; reopening a daemon never grants a second dispatch.
use std::collections::BTreeMap;
use std::path::Path;

use proletarius_sms::stable_digest_hex;
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};

const FILE: &str = "sms-forwarding-test-receipts-v1.json";
const LIMIT: usize = 512;

#[derive(Clone)]
pub(super) struct Store(crate::sms_store_health::GuardedArchive);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct Receipt {
    pub client_request_id: String,
    pub provider: String,
    pub expected_configuration_id: String,
    pub fingerprint: String,
    pub state: String,
    pub terminal: bool,
    pub sent: bool,
    pub dispatch_count: u8,
    pub code: String,
}

#[derive(Default, Deserialize, Serialize)]
struct Snapshot {
    schema_version: u16,
    records: BTreeMap<String, Receipt>,
}

pub(super) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn fingerprint(provider: &str, configuration: &str) -> String {
    // JSON tuple is length-delimited and independent of request key order.
    stable_digest_hex(&serde_json::to_vec(&(provider, configuration)).expect("string tuple"))
}

fn failure(code: &str) -> StoreError {
    StoreError::Configuration(code.to_owned())
}

fn decode(items: &[serde_json::Value]) -> Result<Snapshot, StoreError> {
    if items.is_empty() {
        return Ok(Snapshot {
            schema_version: 1,
            ..Snapshot::default()
        });
    }
    if items.len() != 1 {
        return Err(failure("invalid_store_schema"));
    }
    let snapshot: Snapshot =
        serde_json::from_value(items[0].clone()).map_err(|_| failure("invalid_store_schema"))?;
    if snapshot.schema_version != 1
        || snapshot.records.len() > LIMIT
        || snapshot.records.iter().any(|(id, r)| {
            id != &r.client_request_id
                || !valid_id(id)
                || !valid_id(&r.expected_configuration_id)
                || !matches!(r.provider.as_str(), "telegram" | "bark")
                || r.fingerprint != fingerprint(&r.provider, &r.expected_configuration_id)
                || r.dispatch_count > 1
                || match r.state.as_str() {
                    "dispatching" => r.terminal || r.sent || r.dispatch_count != 1,
                    "unknown_no_replay" => r.terminal || r.sent || r.dispatch_count != 1,
                    "succeeded" => !r.terminal || !r.sent || r.dispatch_count != 1,
                    "failed" => !r.terminal || r.sent,
                    _ => true,
                }
        })
    {
        return Err(failure("invalid_store_schema"));
    }
    Ok(snapshot)
}

fn encode(items: &mut Vec<serde_json::Value>, snapshot: Snapshot) -> Result<(), StoreError> {
    *items = vec![serde_json::to_value(snapshot).map_err(StoreError::Json)?];
    Ok(())
}

impl Store {
    pub(super) fn new(root: &Path) -> Self {
        let store = Self(ArchiveStore::new(root.join(FILE)).into());
        // GuardedArchive latches corrupt/uncertain IO; no failed recovery can
        // silently become a successful claim on this process instance.
        let _ = store.0.mutate(|items| {
            let mut snapshot = decode(items)?;
            for receipt in snapshot.records.values_mut() {
                if receipt.state == "dispatching" {
                    receipt.state = "unknown_no_replay".into();
                    receipt.code = "sms_forwarding_test_result_unknown".into();
                }
            }
            encode(items, snapshot)
        });
        store
    }

    pub(super) fn lookup(
        &self,
        id: &str,
        provider: &str,
        configuration: &str,
    ) -> Result<Option<Receipt>, StoreError> {
        self.0.read(|items| {
            let snapshot = decode(items)?;
            let Some(receipt) = snapshot.records.get(id) else {
                return Ok(None);
            };
            if receipt.fingerprint != fingerprint(provider, configuration) {
                return Err(failure("sms_forwarding_test_request_id_conflict"));
            }
            Ok(Some(receipt.clone()))
        })
    }

    /// `true` is returned only after the unique dispatch claim is fsynced.
    pub(super) fn claim(
        &self,
        id: &str,
        provider: &str,
        configuration: &str,
        rejection: Option<&str>,
    ) -> Result<(Receipt, bool), StoreError> {
        self.0.mutate_if_changed(|items| {
            let mut snapshot = decode(items)?;
            let digest = fingerprint(provider, configuration);
            if let Some(receipt) = snapshot.records.get(id) {
                if receipt.fingerprint != digest {
                    return Err(failure("sms_forwarding_test_request_id_conflict"));
                }
                return Ok(((receipt.clone(), false), false));
            }
            if snapshot.records.len() >= LIMIT {
                return Err(failure("sms_forwarding_test_ledger_full"));
            }
            let receipt = Receipt {
                client_request_id: id.into(),
                provider: provider.into(),
                expected_configuration_id: configuration.into(),
                fingerprint: digest,
                state: if rejection.is_some() {
                    "failed"
                } else {
                    "dispatching"
                }
                .into(),
                terminal: rejection.is_some(),
                sent: false,
                dispatch_count: u8::from(rejection.is_none()),
                code: rejection
                    .unwrap_or("sms_forwarding_test_in_progress")
                    .into(),
            };
            snapshot.records.insert(id.into(), receipt.clone());
            encode(items, snapshot)?;
            Ok(((receipt, rejection.is_none()), true))
        })
    }

    pub(super) fn finish(
        &self,
        id: &str,
        outcome: Result<(), (String, bool)>,
    ) -> Result<Receipt, StoreError> {
        self.0.mutate(|items| {
            let mut snapshot = decode(items)?;
            let receipt = snapshot
                .records
                .get_mut(id)
                .ok_or_else(|| failure("sms_forwarding_test_missing_claim"))?;
            if receipt.state != "dispatching" {
                return Ok(receipt.clone());
            }
            match outcome {
                Ok(()) => {
                    receipt.state = "succeeded".into();
                    receipt.sent = true;
                    receipt.terminal = true;
                    receipt.code = String::new();
                }
                Err((code, unknown)) => {
                    receipt.state = if unknown {
                        "unknown_no_replay"
                    } else {
                        "failed"
                    }
                    .into();
                    receipt.terminal = !unknown;
                    receipt.code = code;
                }
            }
            let result = receipt.clone();
            encode(items, snapshot)?;
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reopen_claim_is_unknown_and_never_dispatches_again() {
        let root = super::super::tests::test_root();
        let first = Store::new(&root);
        let (claimed, dispatch) = first
            .claim("request", "bark", "generation-1", None)
            .unwrap();
        assert!(dispatch);
        assert_eq!(claimed.dispatch_count, 1);
        drop(first);
        let reopened = Store::new(&root);
        let (receipt, dispatch) = reopened
            .claim("request", "bark", "generation-1", None)
            .unwrap();
        assert!(!dispatch);
        assert_eq!(receipt.state, "unknown_no_replay");
        assert!(!receipt.terminal);
        assert_eq!(receipt.dispatch_count, 1);
        assert_eq!(
            reopened.lookup("request", "bark", "generation-1").unwrap(),
            Some(receipt)
        );
        assert!(
            reopened
                .lookup("request", "telegram", "generation-1")
                .is_err()
        );
        assert!(reopened.lookup("request", "bark", "generation-2").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
