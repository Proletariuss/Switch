//! At-most-once receipts for eUICC operator notification delivery.
//!
//! Every payload and the final card confirmation have their own durable
//! submitted/terminal phase.  Ambiguous delivery is fenced as unknown; a
//! later GET or retry request cannot resend an earlier successful payload.

use std::collections::BTreeMap;
use std::path::Path;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use proletarius_euicc::PendingNotification;
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const FILE: &str = "reader-esim-notification-receipts-v1.json";
const VERSION: u16 = 1;
const MAX_RECORDS: usize = 2_048;
const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub(crate) reader: String,
    pub(crate) reader_generation: u64,
    pub(crate) card_identity_hash: String,
    pub(crate) aid_hex: String,
    pub(crate) sequence_number: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Step {
    fingerprint: String,
    phase: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredResponse {
    status: u16,
    body: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    client_request_id: String,
    request_fingerprint: String,
    binding: Binding,
    phase: String,
    payloads: Vec<Step>,
    confirm: Step,
    response: StoredResponse,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u16,
    records: BTreeMap<String, Record>,
}

#[derive(Clone)]
pub(crate) struct Store {
    archive: ArchiveStore,
}

pub(crate) enum Preflight {
    New(Store),
    Replay(Response<Full<Bytes>>),
}

fn configuration(code: &str) -> StoreError {
    StoreError::Configuration(code.into())
}

fn digest(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part);
        digest.update([0]);
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn request_fingerprint(body: &[u8]) -> String {
    digest(&[b"/api/reader/esim/notifications/retry", body])
}

fn payload_fingerprint(payload: &PendingNotification) -> String {
    digest(&[
        payload.sequence_number.to_string().as_bytes(),
        payload.address.as_bytes(),
        &payload.encoded,
    ])
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_binding(binding: &Binding) -> bool {
    !binding.reader.trim().is_empty()
        && binding.reader.trim() == binding.reader
        && binding.reader.len() <= 512
        && !binding.reader.chars().any(char::is_control)
        && valid_hash(&binding.card_identity_hash)
        && !binding.aid_hex.is_empty()
        && binding.aid_hex.len() <= 512
        && binding.aid_hex.len() % 2 == 0
        && binding.aid_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        && binding.sequence_number > 0
}

fn same_notification(left: &Binding, right: &Binding) -> bool {
    left.card_identity_hash == right.card_identity_hash
        && left.aid_hex.eq_ignore_ascii_case(&right.aid_hex)
        && left.sequence_number == right.sequence_number
}

fn unknown(id: &str, binding: &Binding) -> StoredResponse {
    StoredResponse {
        status: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
        body: json!({
            "code":"notification_outcome_unknown",
            "error":"运营商通知投递或卡内确认结果未知；同一请求已被永久围栏，未再次发送",
            "retryable":false,
            "mutation_receipt":{
                "client_request_id":id,
                "outcome":"unknown",
                "reader":binding.reader,
                "reader_generation":binding.reader_generation,
                "aid_hex":binding.aid_hex,
                "sequence_number":binding.sequence_number,
            }
        }),
    }
}

fn decode(items: &[Value]) -> Result<Ledger, StoreError> {
    if items.is_empty() {
        return Ok(Ledger {
            version: VERSION,
            records: BTreeMap::new(),
        });
    }
    if items.len() != 1 {
        return Err(configuration("invalid_notification_receipt_store"));
    }
    let ledger: Ledger = serde_json::from_value(items[0].clone()).map_err(StoreError::Json)?;
    let mut notification_keys = std::collections::BTreeSet::new();
    if ledger.version != VERSION
        || ledger.records.len() > MAX_RECORDS
        || ledger.records.iter().any(|(id, record)| {
            id != &record.client_request_id
                || !crate::reader_mutation_intent::valid_client_request_id(id)
                || !valid_hash(&record.request_fingerprint)
                || !valid_binding(&record.binding)
                || !matches!(record.phase.as_str(), "active" | "unknown" | "terminal")
                || record.payloads.iter().any(|step| {
                    !valid_hash(&step.fingerprint)
                        || !matches!(
                            step.phase.as_str(),
                            "prepared" | "submitted" | "terminal" | "unknown"
                        )
                })
                || record.confirm.fingerprint != "card-confirm"
                || !matches!(
                    record.confirm.phase.as_str(),
                    "prepared" | "submitted" | "terminal" | "unknown"
                )
                || !(100..600).contains(&record.response.status)
                || (record.phase == "terminal") != (record.response.status != 503)
        })
        || ledger.records.values().any(|record| {
            !notification_keys.insert((
                record.binding.card_identity_hash.as_str(),
                record.binding.aid_hex.to_ascii_uppercase(),
                record.binding.sequence_number,
            ))
        })
    {
        return Err(configuration("invalid_notification_receipt_store"));
    }
    Ok(ledger)
}

fn encode(items: &mut Vec<Value>, ledger: &Ledger) -> Result<(), StoreError> {
    *items = vec![serde_json::to_value(ledger).map_err(StoreError::Json)?];
    Ok(())
}

fn response(stored: StoredResponse) -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::from_u16(stored.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
        &stored.body,
    )
}

impl Store {
    fn open(root: &Path) -> Result<Self, StoreError> {
        let archive = ArchiveStore::with_limits(root.join(FILE), MAX_BYTES, MAX_RECORDS)?
            .require_nonempty_ledger()?;
        let store = Self { archive };
        store.archive.mutate_if_changed(|items| {
            let mut ledger = decode(items)?;
            let mut changed = false;
            for record in ledger.records.values_mut() {
                if record.phase == "active" {
                    record.phase = "unknown".into();
                    for step in &mut record.payloads {
                        if step.phase == "submitted" {
                            step.phase = "unknown".into();
                        }
                    }
                    if record.confirm.phase == "submitted" {
                        record.confirm.phase = "unknown".into();
                    }
                    record.response = unknown(&record.client_request_id, &record.binding);
                    changed = true;
                }
            }
            if changed {
                encode(items, &ledger)?;
            }
            Ok(((), changed))
        })?;
        Ok(store)
    }

    fn lookup(&self, id: &str, fingerprint: &str) -> Result<Option<StoredResponse>, StoreError> {
        self.archive.load().and_then(|items| {
            let ledger = decode(&items)?;
            match ledger.records.get(id) {
                Some(record) if record.request_fingerprint != fingerprint => {
                    Err(configuration("notification_request_id_conflict"))
                }
                Some(record) => Ok(Some(record.response.clone())),
                None => Ok(None),
            }
        })
    }

    pub(crate) fn claim(
        &self,
        id: &str,
        fingerprint: &str,
        binding: Binding,
    ) -> Result<Option<Response<Full<Bytes>>>, StoreError> {
        if !crate::reader_mutation_intent::valid_client_request_id(id)
            || !valid_hash(fingerprint)
            || !valid_binding(&binding)
        {
            return Err(configuration("invalid_notification_request"));
        }
        self.archive.mutate_if_changed(|items| {
            let mut ledger = decode(items)?;
            if let Some(record) = ledger.records.get(id) {
                if record.request_fingerprint != fingerprint || record.binding != binding {
                    return Err(configuration("notification_request_id_conflict"));
                }
                return Ok((Some(response(record.response.clone())), false));
            }
            // The eUICC notification is the durable mutation identity.  A
            // caller cannot mint a fresh request UUID to replay the same
            // card/AID/sequence after an HTTP response is lost or the daemon
            // restarts. Reader generation is deliberately excluded: moving
            // the same card to a successor reader must not reopen delivery.
            if let Some(record) = ledger
                .records
                .values()
                .find(|record| same_notification(&record.binding, &binding))
            {
                return Ok((Some(response(record.response.clone())), false));
            }
            if ledger.records.len() >= MAX_RECORDS {
                return Err(configuration("notification_receipt_store_full"));
            }
            let initial = unknown(id, &binding);
            ledger.records.insert(
                id.into(),
                Record {
                    client_request_id: id.into(),
                    request_fingerprint: fingerprint.into(),
                    binding,
                    phase: "active".into(),
                    payloads: Vec::new(),
                    confirm: Step {
                        fingerprint: "card-confirm".into(),
                        phase: "prepared".into(),
                    },
                    response: initial,
                },
            );
            encode(items, &ledger)?;
            Ok((None, true))
        })
    }

    pub(crate) fn set_payloads(
        &self,
        id: &str,
        pending: &[PendingNotification],
    ) -> Result<Vec<String>, StoreError> {
        let fingerprints: Vec<_> = pending.iter().map(payload_fingerprint).collect();
        self.archive.mutate(|items| {
            let mut ledger = decode(items)?;
            let record = ledger
                .records
                .get_mut(id)
                .ok_or_else(|| configuration("invalid_notification_receipt_store"))?;
            if record.phase != "active"
                || pending
                    .iter()
                    .any(|payload| payload.sequence_number != record.binding.sequence_number)
            {
                return Err(configuration("notification_outcome_unknown"));
            }
            if record.payloads.is_empty() {
                record.payloads = fingerprints
                    .iter()
                    .map(|fingerprint| Step {
                        fingerprint: fingerprint.clone(),
                        phase: "prepared".into(),
                    })
                    .collect();
            } else if record
                .payloads
                .iter()
                .map(|step| &step.fingerprint)
                .ne(fingerprints.iter())
            {
                return Err(configuration("notification_payload_changed"));
            }
            encode(items, &ledger)?;
            Ok(fingerprints.clone())
        })
    }

    /// true grants exactly one external delivery; false means already terminal.
    pub(crate) fn claim_payload(&self, id: &str, fingerprint: &str) -> Result<bool, StoreError> {
        self.archive.mutate(|items| {
            let mut ledger = decode(items)?;
            let record = ledger
                .records
                .get_mut(id)
                .ok_or_else(|| configuration("invalid_notification_receipt_store"))?;
            if record.phase != "active" {
                return Err(configuration("notification_outcome_unknown"));
            }
            let step = record
                .payloads
                .iter_mut()
                .find(|step| step.fingerprint == fingerprint)
                .ok_or_else(|| configuration("notification_payload_changed"))?;
            let dispatch = match step.phase.as_str() {
                "prepared" => {
                    step.phase = "submitted".into();
                    true
                }
                "terminal" => false,
                _ => return Err(configuration("notification_outcome_unknown")),
            };
            encode(items, &ledger)?;
            Ok(dispatch)
        })
    }

    pub(crate) fn finish_payload(
        &self,
        id: &str,
        fingerprint: &str,
        delivered: bool,
    ) -> Result<(), StoreError> {
        self.archive.mutate(|items| {
            let mut ledger = decode(items)?;
            let record = ledger
                .records
                .get_mut(id)
                .ok_or_else(|| configuration("invalid_notification_receipt_store"))?;
            let step = record
                .payloads
                .iter_mut()
                .find(|step| step.fingerprint == fingerprint)
                .ok_or_else(|| configuration("notification_payload_changed"))?;
            if record.phase != "active" || step.phase != "submitted" {
                return Err(configuration("notification_outcome_unknown"));
            }
            if delivered {
                step.phase = "terminal".into();
            } else {
                step.phase = "unknown".into();
                record.phase = "unknown".into();
                record.response = unknown(id, &record.binding);
            }
            encode(items, &ledger)
        })
    }

    pub(crate) fn claim_confirm(&self, id: &str) -> Result<bool, StoreError> {
        self.archive.mutate(|items| {
            let mut ledger = decode(items)?;
            let record = ledger
                .records
                .get_mut(id)
                .ok_or_else(|| configuration("invalid_notification_receipt_store"))?;
            if record.phase != "active"
                || record.payloads.is_empty()
                || record.payloads.iter().any(|step| step.phase != "terminal")
            {
                return Err(configuration("notification_outcome_unknown"));
            }
            let dispatch = match record.confirm.phase.as_str() {
                "prepared" => {
                    record.confirm.phase = "submitted".into();
                    true
                }
                "terminal" => false,
                _ => return Err(configuration("notification_outcome_unknown")),
            };
            encode(items, &ledger)?;
            Ok(dispatch)
        })
    }

    pub(crate) fn finish_confirm(&self, id: &str, confirmed: bool) -> Result<(), StoreError> {
        self.archive.mutate(|items| {
            let mut ledger = decode(items)?;
            let record = ledger
                .records
                .get_mut(id)
                .ok_or_else(|| configuration("invalid_notification_receipt_store"))?;
            if record.phase != "active" || record.confirm.phase != "submitted" {
                return Err(configuration("notification_outcome_unknown"));
            }
            if confirmed {
                record.confirm.phase = "terminal".into();
            } else {
                record.confirm.phase = "unknown".into();
                record.phase = "unknown".into();
                record.response = unknown(id, &record.binding);
            }
            encode(items, &ledger)
        })
    }

    pub(crate) fn complete(&self, id: &str) -> Result<(), StoreError> {
        self.archive.mutate(|items| {
            let mut ledger = decode(items)?;
            let record = ledger
                .records
                .get_mut(id)
                .ok_or_else(|| configuration("invalid_notification_receipt_store"))?;
            if record.phase != "active"
                || record.confirm.phase != "terminal"
                || record.payloads.iter().any(|step| step.phase != "terminal")
            {
                return Err(configuration("notification_outcome_unknown"));
            }
            record.phase = "terminal".into();
            record.response = StoredResponse {
                status: StatusCode::OK.as_u16(),
                body: json!({
                    "message":"运营商通知发送并确认完成",
                    "sequence_number":record.binding.sequence_number,
                    "mutation_receipt":{
                        "client_request_id":id,
                        "outcome":"completed",
                        "reader":record.binding.reader,
                        "reader_generation":record.binding.reader_generation,
                        "aid_hex":record.binding.aid_hex,
                        "sequence_number":record.binding.sequence_number,
                    }
                }),
            };
            encode(items, &ledger)?;
            Ok(())
        })
    }
}

pub(crate) fn preflight(root: &Path, id: &str, body: &[u8]) -> Result<Preflight, StoreError> {
    if !crate::reader_mutation_intent::valid_client_request_id(id) {
        return Err(configuration("invalid_notification_request"));
    }
    let store = Store::open(root)?;
    match store.lookup(id, &request_fingerprint(body))? {
        Some(stored) => Ok(Preflight::Replay(response(stored))),
        None => Ok(Preflight::New(store)),
    }
}

pub(crate) fn card_identity_hash(iccid: &str) -> String {
    digest(&[b"reader-card", iccid.as_bytes()])
}

pub(crate) fn initialize(root: &Path) -> Result<(), StoreError> {
    Store::open(root).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "123e4567-e89b-42d3-a456-426614174000";

    fn area(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "switch-notification-receipt-{name}-{}-{}",
            std::process::id(),
            crate::unix_now_ms()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn binding() -> Binding {
        Binding {
            reader: "Reader A".into(),
            reader_generation: 8,
            card_identity_hash: card_identity_hash("8986001234567890123"),
            aid_hex: "A0000005591010FFFFFFFF8900000100".into(),
            sequence_number: 12,
        }
    }

    fn pending(payload: u8) -> PendingNotification {
        PendingNotification {
            sequence_number: 12,
            address: "notify.example".into(),
            encoded: vec![payload],
        }
    }

    #[test]
    fn each_payload_and_card_confirmation_dispatch_at_most_once() {
        let root = area("steps");
        let store = match preflight(&root, ID, b"body").unwrap() {
            Preflight::New(store) => store,
            _ => unreachable!(),
        };
        store
            .claim(ID, &request_fingerprint(b"body"), binding())
            .unwrap();
        let payloads = [pending(1), pending(2)];
        let fingerprints = store.set_payloads(ID, &payloads).unwrap();
        assert!(store.claim_payload(ID, &fingerprints[0]).unwrap());
        store.finish_payload(ID, &fingerprints[0], true).unwrap();
        assert!(!store.claim_payload(ID, &fingerprints[0]).unwrap());
        assert!(store.claim_payload(ID, &fingerprints[1]).unwrap());
        store.finish_payload(ID, &fingerprints[1], true).unwrap();
        assert!(store.claim_confirm(ID).unwrap());
        store.finish_confirm(ID, true).unwrap();
        assert!(!store.claim_confirm(ID).unwrap());
        store.complete(ID).unwrap();
        assert!(matches!(
            preflight(&root, ID, b"body").unwrap(),
            Preflight::Replay(response) if response.status() == StatusCode::OK
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn later_payload_unknown_survives_restart_without_replaying_prior_success() {
        let root = area("unknown");
        let store = match preflight(&root, ID, b"body").unwrap() {
            Preflight::New(store) => store,
            _ => unreachable!(),
        };
        store
            .claim(ID, &request_fingerprint(b"body"), binding())
            .unwrap();
        let payloads = [pending(1), pending(2)];
        let fingerprints = store.set_payloads(ID, &payloads).unwrap();
        store.claim_payload(ID, &fingerprints[0]).unwrap();
        store.finish_payload(ID, &fingerprints[0], true).unwrap();
        store.claim_payload(ID, &fingerprints[1]).unwrap();
        store.finish_payload(ID, &fingerprints[1], false).unwrap();
        drop(store);
        assert!(matches!(
            preflight(&root, ID, b"body").unwrap(),
            Preflight::Replay(response) if response.status() == StatusCode::SERVICE_UNAVAILABLE
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn conflict_restart_and_missing_presence_fail_closed() {
        let root = area("restart");
        let store = match preflight(&root, ID, b"body").unwrap() {
            Preflight::New(store) => store,
            _ => unreachable!(),
        };
        store
            .claim(ID, &request_fingerprint(b"body"), binding())
            .unwrap();
        drop(store);
        assert!(matches!(
            preflight(&root, ID, b"body").unwrap(),
            Preflight::Replay(response) if response.status() == StatusCode::SERVICE_UNAVAILABLE
        ));
        assert!(matches!(
            preflight(&root, ID, b"changed"),
            Err(StoreError::Configuration(code)) if code == "notification_request_id_conflict"
        ));
        std::fs::remove_file(root.join(format!(".{FILE}.presence-v1"))).unwrap();
        assert!(preflight(&root, ID, b"body").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn fresh_request_id_cannot_replay_same_card_notification_after_unknown() {
        const SECOND_ID: &str = "123e4567-e89b-42d3-a456-426614174001";
        let root = area("cross-request-object-fence");
        let store = match preflight(&root, ID, b"first-body").unwrap() {
            Preflight::New(store) => store,
            _ => unreachable!(),
        };
        store
            .claim(ID, &request_fingerprint(b"first-body"), binding())
            .unwrap();
        let fingerprints = store.set_payloads(ID, &[pending(1)]).unwrap();
        assert!(store.claim_payload(ID, &fingerprints[0]).unwrap());
        store.finish_payload(ID, &fingerprints[0], false).unwrap();

        let second = match preflight(&root, SECOND_ID, b"second-body").unwrap() {
            Preflight::New(store) => store,
            _ => unreachable!(),
        };
        let fenced = second
            .claim(
                SECOND_ID,
                &request_fingerprint(b"second-body"),
                Binding {
                    reader: "Reader B".into(),
                    reader_generation: 99,
                    ..binding()
                },
            )
            .unwrap()
            .expect("the card notification must stay fenced across request IDs");
        assert_eq!(fenced.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            second
                .lookup(SECOND_ID, &request_fingerprint(b"second-body"))
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
