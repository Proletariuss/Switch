//! Restart-safe admission and receipt replay for public call mutations.
//!
//! The live call projection still selects the physical owner.  This ledger is
//! deliberately below that decision and above the dispatcher: an exact client
//! request is durably admitted and claimed before an INVITE/answer/BYE/INFO or
//! module command can be submitted.  A claimed request is never dispatched a
//! second time, even when the daemon or HTTP waiter disappears.

use std::future::Future;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Response, StatusCode};
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const STORE_FILE: &str = "call-mutation-receipts-v1.json";
const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RECORDS: usize = 512;
const SCHEMA_VERSION: u16 = 1;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OwnerBinding<'a> {
    pub(crate) owner: &'a str,
    pub(crate) generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct MutationRequest {
    pub(crate) client_request_id: String,
    pub(crate) action: String,
    pub(crate) raw_payload_fingerprint: String,
    pub(crate) owner: String,
    pub(crate) generation: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct StoredResponse {
    status: u16,
    body: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Record {
    schema_version: u16,
    client_request_id: String,
    raw_payload_fingerprint: String,
    action: String,
    owner: String,
    generation: u64,
    phase: String,
    outcome: String,
    dispatch_count: u8,
    created_at_ms: u64,
    updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response: Option<StoredResponse>,
}

#[derive(Clone)]
struct Store {
    archive: ArchiveStore,
}

enum Lookup {
    Missing,
    Replay(StoredResponse),
    Receipt(Record),
}

enum Admission {
    Dispatch,
    Replay(StoredResponse),
    Receipt(Record),
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn failure(code: &str) -> StoreError {
    StoreError::Configuration(code.to_owned())
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_record(record: &Record) -> bool {
    record.schema_version == SCHEMA_VERSION
        && valid_request_id(&record.client_request_id)
        && valid_fingerprint(&record.raw_payload_fingerprint)
        && matches!(
            record.action.as_str(),
            "dial" | "answer" | "reject" | "hangup" | "dtmf" | "ussd"
        )
        && matches!(record.owner.as_str(), "module" | "reader" | "module-vowifi")
        && matches!(
            record.phase.as_str(),
            "admitted" | "submitted" | "unknown" | "terminal"
        )
        && matches!(
            record.outcome.as_str(),
            "pending" | "unknown" | "succeeded" | "failed" | "unknown_no_replay"
        )
        && record.dispatch_count <= 1
        && match record.phase.as_str() {
            "admitted" => {
                record.dispatch_count == 0
                    && record.outcome == "pending"
                    && record.response.is_none()
            }
            "submitted" => {
                record.dispatch_count == 1
                    && record.outcome == "unknown"
                    && record.response.is_none()
            }
            "unknown" => {
                // A module admission cannot be recovered against a proven
                // call instance. Preserve it without inventing a submit.
                (record.dispatch_count == 1 || record.owner == "module")
                    && record.outcome == "unknown"
                    && record.response.is_none()
            }
            "terminal" if record.outcome == "unknown_no_replay" => {
                record.action == "dtmf" && record.dispatch_count == 1 && record.response.is_none()
            }
            "terminal" => {
                record.dispatch_count == 1
                    && matches!(record.outcome.as_str(), "succeeded" | "failed")
                    && record
                        .response
                        .as_ref()
                        .is_some_and(|response| StatusCode::from_u16(response.status).is_ok())
            }
            _ => false,
        }
}

fn decode_records(items: &[Value]) -> Result<Vec<Record>, StoreError> {
    let records = items
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<Record>, _>>()
        .map_err(StoreError::Json)?;
    if records.iter().any(|record| !valid_record(record)) {
        return Err(failure("invalid_call_mutation_receipt_store"));
    }
    let mut ids = std::collections::BTreeSet::new();
    if records
        .iter()
        .any(|record| !ids.insert(record.client_request_id.as_str()))
    {
        return Err(failure("invalid_call_mutation_receipt_store"));
    }
    Ok(records)
}

fn encode_records(items: &mut Vec<Value>, records: Vec<Record>) -> Result<(), StoreError> {
    *items = records
        .into_iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Json)?;
    Ok(())
}

fn same_identity(
    record: &Record,
    client_request_id: &str,
    action: &str,
    fingerprint: &str,
) -> bool {
    record.client_request_id == client_request_id
        && record.action == action
        && record.raw_payload_fingerprint == fingerprint
}

fn same_binding(record: &Record, request: &MutationRequest) -> bool {
    record.owner == request.owner && record.generation == request.generation
}

fn quarantine_module_admission(record: &mut Record) -> bool {
    if record.owner != "module" || record.phase != "admitted" {
        return false;
    }
    // Radio transport sessions and module_call_state's boundary/revision are
    // process-local observations, not a durable SIM + specific-call binding.
    // Even an unsubmitted old answer/hangup must never target the next call.
    record.phase = "unknown".into();
    record.outcome = "unknown".into();
    record.updated_at_ms = now_ms();
    true
}

impl Store {
    fn new(root: &Path) -> Result<Self, StoreError> {
        let store = Self {
            archive: ArchiveStore::with_limits(
                root.join(STORE_FILE),
                MAX_STORE_BYTES,
                MAX_RECORDS,
            )?
            .require_nonempty_ledger()?,
        };
        decode_records(&store.archive.load()?)?;
        Ok(store)
    }

    fn recover(&self) -> Result<(), StoreError> {
        self.archive.mutate_if_changed(|items| {
            let mut records = decode_records(items)?;
            let mut changed = false;
            let now = now_ms();
            for record in &mut records {
                if quarantine_module_admission(record) {
                    changed = true;
                    continue;
                }
                if record.phase != "submitted" {
                    continue;
                }
                if record.action == "dtmf" {
                    record.phase = "terminal".into();
                    record.outcome = "unknown_no_replay".into();
                } else {
                    record.phase = "unknown".into();
                    record.outcome = "unknown".into();
                }
                record.updated_at_ms = now;
                changed = true;
            }
            if changed {
                encode_records(items, records)?;
            }
            Ok(((), changed))
        })
    }

    fn lookup(
        &self,
        client_request_id: &str,
        action: &str,
        fingerprint: &str,
    ) -> Result<Lookup, StoreError> {
        validate_identity(client_request_id, action, fingerprint)?;
        self.archive.mutate_if_changed(|items| {
            let mut records = decode_records(items)?;
            let Some(record) = records
                .iter_mut()
                .find(|record| record.client_request_id == client_request_id)
            else {
                return Ok((Lookup::Missing, false));
            };
            if !same_identity(record, client_request_id, action, fingerprint) {
                return Err(failure("call_mutation_request_id_conflict"));
            }
            let changed = quarantine_module_admission(record);
            let result = match record.response.clone() {
                Some(response) => Lookup::Replay(response),
                None => Lookup::Receipt(record.clone()),
            };
            if changed {
                encode_records(items, records)?;
            }
            Ok((result, changed))
        })
    }

    fn admit(&self, request: &MutationRequest) -> Result<Admission, StoreError> {
        validate_request(request)?;
        self.archive.mutate_if_changed(|items| {
            let mut records = decode_records(items)?;
            if let Some(record) = records
                .iter_mut()
                .find(|record| record.client_request_id == request.client_request_id)
            {
                if !same_identity(
                    record,
                    &request.client_request_id,
                    &request.action,
                    &request.raw_payload_fingerprint,
                ) || !same_binding(record, request)
                {
                    return Err(failure("call_mutation_request_id_conflict"));
                }
                let changed = quarantine_module_admission(record);
                let result = match record.response.clone() {
                    Some(response) => Admission::Replay(response),
                    None if record.phase == "admitted" => Admission::Dispatch,
                    None => Admission::Receipt(record.clone()),
                };
                if changed {
                    encode_records(items, records)?;
                }
                return Ok((result, changed));
            }
            if records.len() >= MAX_RECORDS {
                return Err(failure("call_mutation_receipt_store_full"));
            }
            let now = now_ms();
            records.push(Record {
                schema_version: SCHEMA_VERSION,
                client_request_id: request.client_request_id.clone(),
                raw_payload_fingerprint: request.raw_payload_fingerprint.clone(),
                action: request.action.clone(),
                owner: request.owner.clone(),
                generation: request.generation,
                phase: "admitted".into(),
                outcome: "pending".into(),
                dispatch_count: 0,
                created_at_ms: now,
                updated_at_ms: now,
                response: None,
            });
            encode_records(items, records)?;
            Ok((Admission::Dispatch, true))
        })
    }

    /// This fsync is the physical submit boundary.  Dispatch is forbidden
    /// unless the transition succeeds, and it can succeed only once.
    fn claim(&self, request: &MutationRequest) -> Result<(), StoreError> {
        self.archive.mutate(|items| {
            let mut records = decode_records(items)?;
            let record = records
                .iter_mut()
                .find(|record| record.client_request_id == request.client_request_id)
                .ok_or_else(|| failure("call_mutation_receipt_missing"))?;
            if !same_identity(
                record,
                &request.client_request_id,
                &request.action,
                &request.raw_payload_fingerprint,
            ) || !same_binding(record, request)
                || record.phase != "admitted"
                || record.dispatch_count != 0
            {
                return Err(failure("call_mutation_dispatch_already_claimed"));
            }
            record.phase = "submitted".into();
            record.outcome = "unknown".into();
            record.dispatch_count = 1;
            record.updated_at_ms = now_ms();
            encode_records(items, records)
        })
    }

    fn finish(
        &self,
        request: &MutationRequest,
        response: StoredResponse,
    ) -> Result<(), StoreError> {
        self.archive.mutate(|items| {
            let mut records = decode_records(items)?;
            let record = records
                .iter_mut()
                .find(|record| record.client_request_id == request.client_request_id)
                .ok_or_else(|| failure("call_mutation_receipt_missing"))?;
            if !same_identity(
                record,
                &request.client_request_id,
                &request.action,
                &request.raw_payload_fingerprint,
            ) || !same_binding(record, request)
                || record.phase != "submitted"
                || record.dispatch_count != 1
                || record.response.is_some()
            {
                return Err(failure("call_mutation_terminal_conflict"));
            }
            record.phase = "terminal".into();
            record.outcome = if (200..300).contains(&response.status) {
                "succeeded".into()
            } else {
                "failed".into()
            };
            record.updated_at_ms = now_ms();
            record.response = Some(response);
            encode_records(items, records)
        })
    }
}

fn validate_identity(
    client_request_id: &str,
    action: &str,
    fingerprint: &str,
) -> Result<(), StoreError> {
    if !valid_request_id(client_request_id)
        || !matches!(
            action,
            "dial" | "answer" | "reject" | "hangup" | "dtmf" | "ussd"
        )
        || !valid_fingerprint(fingerprint)
    {
        return Err(failure("invalid_call_mutation_intent"));
    }
    Ok(())
}

fn validate_request(request: &MutationRequest) -> Result<(), StoreError> {
    validate_identity(
        &request.client_request_id,
        &request.action,
        &request.raw_payload_fingerprint,
    )?;
    if !matches!(
        request.owner.as_str(),
        "module" | "reader" | "module-vowifi"
    ) {
        return Err(failure("invalid_call_mutation_intent"));
    }
    Ok(())
}

pub(crate) fn initialize(root: &Path) -> Result<(), StoreError> {
    Store::new(root)?.recover()
}

pub(crate) fn fingerprint(raw_payload: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw_payload))
}

pub(crate) fn preflight(
    root: &Path,
    client_request_id: &str,
    action: &str,
    fingerprint: &str,
) -> Option<Response<Full<Bytes>>> {
    let store = match Store::new(root) {
        Ok(store) => store,
        Err(_) => return Some(unavailable_before_dispatch()),
    };
    match store.lookup(client_request_id, action, fingerprint) {
        Ok(Lookup::Missing) => None,
        Ok(Lookup::Replay(response)) => Some(replay_response(&store, &response, client_request_id)),
        // Only a strongly bound non-module `admitted` may re-evaluate its live
        // owner and atomically claim it. Module admissions are quarantined by
        // lookup: no durable specific-call identity exists for recovery. A
        // submitted/unknown request must only return its receipt: replaying it
        // could duplicate an INVITE, answer, BYE or DTMF command.
        Ok(Lookup::Receipt(record)) if record.phase == "admitted" => None,
        Ok(Lookup::Receipt(record)) => Some(receipt_response(&record)),
        Err(StoreError::Configuration(code)) if code == "call_mutation_request_id_conflict" => {
            Some(rejection(
                StatusCode::CONFLICT,
                &code,
                "通话请求编号已绑定其他载荷",
            ))
        }
        Err(_) => Some(unavailable_before_dispatch()),
    }
}

pub(crate) async fn execute<F, Fut>(
    root: &Path,
    request: MutationRequest,
    dispatch: F,
) -> Response<Full<Bytes>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Response<Full<Bytes>>>,
{
    let store = match Store::new(root) {
        Ok(store) => store,
        Err(_) => return unavailable_before_dispatch(),
    };
    match store.admit(&request) {
        Ok(Admission::Replay(response)) => {
            replay_response(&store, &response, &request.client_request_id)
        }
        Ok(Admission::Receipt(record)) => receipt_response(&record),
        Ok(Admission::Dispatch) => {
            if store.claim(&request).is_err() {
                // An exact concurrent retry can observe `admitted` before the
                // winning request takes the durable claim.  Losing that race
                // is not an identity conflict: return the winner's receipt so
                // the client retains its original unknown fence.
                return match store.lookup(
                    &request.client_request_id,
                    &request.action,
                    &request.raw_payload_fingerprint,
                ) {
                    Ok(Lookup::Replay(response)) => {
                        replay_response(&store, &response, &request.client_request_id)
                    }
                    Ok(Lookup::Receipt(record)) if record.phase != "admitted" => {
                        receipt_response(&record)
                    }
                    _ => rejection(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "call_mutation_receipt_outcome_unknown",
                        "通话命令的提交边界暂无法确认；请使用原请求编号恢复，切勿新建请求",
                    ),
                };
            }
            let response = dispatch().await;
            let (parts, body) = response.into_parts();
            let bytes = match body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(never) => match never {},
            };
            let stored = StoredResponse {
                status: parts.status.as_u16(),
                body: String::from_utf8_lossy(&bytes).into_owned(),
            };
            if store.finish(&request, stored.clone()).is_err() {
                return rejection(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "call_mutation_receipt_outcome_unknown",
                    "通话命令已提交，但持久回执未能确认；请使用原请求编号恢复，切勿新建请求",
                );
            }
            response_from_stored(&stored, &request.client_request_id)
        }
        Err(StoreError::Configuration(code))
            if matches!(
                code.as_str(),
                "call_mutation_request_id_conflict" | "call_mutation_dispatch_already_claimed"
            ) =>
        {
            rejection(StatusCode::CONFLICT, &code, "通话请求身份或 owner 绑定冲突")
        }
        Err(_) => unavailable_before_dispatch(),
    }
}

fn unavailable_before_dispatch() -> Response<Full<Bytes>> {
    rejection(
        StatusCode::SERVICE_UNAVAILABLE,
        "call_mutation_receipt_unavailable",
        "无法读取或安全记录原通话回执；本次未提交新的物理命令",
    )
}

fn replay_response(
    store: &Store,
    response: &StoredResponse,
    client_request_id: &str,
) -> Response<Full<Bytes>> {
    // A visible terminal record may come from a failed directory fsync after
    // rename. Reading those bytes is not yet durable acknowledgement authority.
    if store.archive.confirm_directory_sync().is_err() {
        return rejection(
            StatusCode::SERVICE_UNAVAILABLE,
            "call_mutation_receipt_outcome_unknown",
            "原通话终结回执的持久化尚未确认；请保留原请求编号继续查询",
        );
    }
    response_from_stored(response, client_request_id)
}

fn response_from_stored(
    response: &StoredResponse,
    client_request_id: &str,
) -> Response<Full<Bytes>> {
    // Only durable finish/replay reaches this boundary. A preflight failure or
    // an unknown receipt must not claim that an earlier request is terminal.
    Response::builder()
        .status(response.status)
        .header("content-type", "application/json; charset=utf-8")
        .header("x-switch-call-terminal", "true")
        .header("x-switch-call-request-id", client_request_id)
        .body(Full::new(Bytes::copy_from_slice(response.body.as_bytes())))
        .unwrap_or_else(|_| {
            rejection(
                StatusCode::INTERNAL_SERVER_ERROR,
                "call_mutation_receipt_invalid",
                "通话操作回执无效",
            )
        })
}

fn receipt_response(record: &Record) -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::ACCEPTED,
        &json!({
            "client_request_id": record.client_request_id,
            "raw_payload_fingerprint": record.raw_payload_fingerprint,
            "action": record.action,
            "selected_owner": record.owner,
            "generation": record.generation,
            "phase": record.phase,
            "outcome": record.outcome,
            "dispatch_count": record.dispatch_count,
            "created_at_ms": record.created_at_ms,
            "updated_at_ms": record.updated_at_ms,
        }),
    )
}

fn rejection(status: StatusCode, code: &str, message: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":message,"code":code,"recoverable":status.is_server_error()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn root() -> std::path::PathBuf {
        let base = std::path::PathBuf::from(
            std::env::var_os("SWITCH_BUILD_DIR").expect("workspace build directory"),
        )
        .join("tmp/call-mutation-intent-tests");
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

    fn request(id: &str, action: &str, byte: u8, owner: OwnerBinding<'_>) -> MutationRequest {
        MutationRequest {
            client_request_id: id.into(),
            action: action.into(),
            raw_payload_fingerprint: fingerprint(&[byte]),
            owner: owner.owner.into(),
            generation: owner.generation,
        }
    }

    fn finish(store: &Store, request: &MutationRequest, status: u16, body: &str) {
        assert!(matches!(store.admit(request).unwrap(), Admission::Dispatch));
        store.claim(request).unwrap();
        store
            .finish(
                request,
                StoredResponse {
                    status,
                    body: body.into(),
                },
            )
            .unwrap();
    }

    #[test]
    fn durable_identity_accepts_all_public_call_alias_and_ussd_actions() {
        for action in ["dial", "answer", "reject", "hangup", "dtmf", "ussd"] {
            assert!(
                validate_identity("public-request", action, &fingerprint(action.as_bytes()))
                    .is_ok()
            );
        }
        for action in ["status", "unknown", ""] {
            assert!(
                validate_identity("public-request", action, &fingerprint(action.as_bytes()))
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn full_terminal_ledger_retains_oldest_receipt_and_refuses_new_identity() {
        let root = root();
        let store = Store::new(&root).unwrap();
        let oldest = request(
            "terminal-0",
            "dial",
            1,
            OwnerBinding {
                owner: "reader",
                generation: 1,
            },
        );
        finish(&store, &oldest, 200, r#"{"oldest":true}"#);
        store
            .archive
            .mutate(|items| {
                let mut records = decode_records(items)?;
                let template = records[0].clone();
                for index in 1..MAX_RECORDS {
                    let mut record = template.clone();
                    record.client_request_id = format!("terminal-{index}");
                    records.push(record);
                }
                encode_records(items, records)
            })
            .unwrap();
        let before = fs::read(root.join(STORE_FILE)).unwrap();
        let dispatches = AtomicUsize::new(0);
        let mut capacity_request = oldest.clone();
        capacity_request.client_request_id = "capacity-dispatch".into();
        let rejected = execute(&root, capacity_request, || async {
            dispatches.fetch_add(1, Ordering::SeqCst);
            crate::json_response(StatusCode::OK, &json!({}))
        })
        .await;
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        let mut next = oldest.clone();
        next.client_request_id = "terminal-512".into();
        assert!(
            matches!(store.admit(&next), Err(StoreError::Configuration(code)) if code == "call_mutation_receipt_store_full")
        );
        assert!(
            matches!(store.admit(&oldest).unwrap(), Admission::Replay(response) if response.body == r#"{"oldest":true}"#)
        );
        next.client_request_id = oldest.client_request_id;
        next.raw_payload_fingerprint = fingerprint(b"different");
        assert!(
            matches!(store.admit(&next), Err(StoreError::Configuration(code)) if code == "call_mutation_request_id_conflict")
        );
        assert_eq!(fs::read(root.join(STORE_FILE)).unwrap(), before);
        assert_eq!(
            decode_records(&Store::new(&root).unwrap().archive.load().unwrap())
                .unwrap()
                .len(),
            MAX_RECORDS
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn corrupt_reopen_preflight_and_execute_never_reach_dispatch() {
        for damage in [b"".as_slice(), b"null", b"[]", b"{broken", b"[{}]"] {
            let root = root();
            let store = Store::new(&root).unwrap();
            let path = root.join(STORE_FILE);
            fs::write(&path, damage).unwrap();
            assert!(Store::new(&root).is_err());
            assert!(store.recover().is_err());
            let request = request(
                "corrupt",
                "dial",
                1,
                OwnerBinding {
                    owner: "reader",
                    generation: 1,
                },
            );
            assert_eq!(
                preflight(
                    &root,
                    &request.client_request_id,
                    &request.action,
                    &request.raw_payload_fingerprint
                )
                .unwrap()
                .status(),
                StatusCode::SERVICE_UNAVAILABLE
            );
            let dispatches = AtomicUsize::new(0);
            let response = execute(&root, request, || async {
                dispatches.fetch_add(1, Ordering::SeqCst);
                crate::json_response(StatusCode::OK, &json!({}))
            })
            .await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(dispatches.load(Ordering::SeqCst), 0);
            assert_eq!(fs::read(path).unwrap(), damage);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn per_request_reopen_after_receipt_file_loss_never_redispatches() {
        let root = root();
        let original = request(
            "lost-between-requests",
            "dial",
            1,
            OwnerBinding {
                owner: "reader",
                generation: 9,
            },
        );
        let first_dispatches = AtomicUsize::new(0);
        let response = execute(&root, original.clone(), || async {
            first_dispatches.fetch_add(1, Ordering::SeqCst);
            crate::json_response(StatusCode::OK, &json!({"accepted":true}))
        })
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(first_dispatches.load(Ordering::SeqCst), 1);
        fs::remove_file(root.join(STORE_FILE)).unwrap();

        assert_eq!(
            preflight(
                &root,
                &original.client_request_id,
                &original.action,
                &original.raw_payload_fingerprint,
            )
            .unwrap()
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let retry_dispatches = AtomicUsize::new(0);
        let retry = execute(&root, original, || async {
            retry_dispatches.fetch_add(1, Ordering::SeqCst);
            crate::json_response(StatusCode::OK, &json!({"duplicate":true}))
        })
        .await;
        assert_eq!(retry.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(retry_dispatches.load(Ordering::SeqCst), 0);
        assert!(!root.join(STORE_FILE).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_independent_store_instances_claim_exact_request_once() {
        let root = root();
        let request = request(
            "concurrent",
            "dial",
            1,
            OwnerBinding {
                owner: "reader",
                generation: 1,
            },
        );
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let dispatches = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let (root, request, barrier, dispatches) = (
                    root.clone(),
                    request.clone(),
                    barrier.clone(),
                    dispatches.clone(),
                );
                std::thread::spawn(move || {
                    let store = Store::new(&root).unwrap();
                    barrier.wait();
                    if matches!(store.admit(&request).unwrap(), Admission::Dispatch)
                        && store.claim(&request).is_ok()
                    {
                        dispatches.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_receipt_reopens_and_exact_request_replays_without_dispatch() {
        let root = root();
        let request = request(
            "invite-lost-response",
            "dial",
            1,
            OwnerBinding {
                owner: "reader",
                generation: 17,
            },
        );
        let first = Store::new(&root).unwrap();
        finish(&first, &request, 200, r#"{"call":{"state":"ended"}}"#);
        drop(first);

        let reopened = Store::new(&root).unwrap();
        reopened.recover().unwrap();
        match reopened.lookup(
            &request.client_request_id,
            &request.action,
            &request.raw_payload_fingerprint,
        ) {
            Ok(Lookup::Replay(response)) => {
                assert_eq!(response.status, 200);
                assert_eq!(response.body, r#"{"call":{"state":"ended"}}"#);
            }
            _ => panic!("terminal response was not replayed"),
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_id_payload_owner_generation_and_action_drift_conflict() {
        let root = root();
        let store = Store::new(&root).unwrap();
        let original = request(
            "bound-request",
            "answer",
            2,
            OwnerBinding {
                owner: "reader",
                generation: 21,
            },
        );
        finish(&store, &original, 200, "{}");
        let changed = [
            request(
                "bound-request",
                "answer",
                3,
                OwnerBinding {
                    owner: "reader",
                    generation: 21,
                },
            ),
            request(
                "bound-request",
                "answer",
                2,
                OwnerBinding {
                    owner: "module-vowifi",
                    generation: 21,
                },
            ),
            request(
                "bound-request",
                "answer",
                2,
                OwnerBinding {
                    owner: "reader",
                    generation: 22,
                },
            ),
            request(
                "bound-request",
                "hangup",
                2,
                OwnerBinding {
                    owner: "reader",
                    generation: 21,
                },
            ),
        ];
        for changed in changed {
            assert!(matches!(
                store.admit(&changed),
                Err(StoreError::Configuration(code)) if code == "call_mutation_request_id_conflict"
            ));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fingerprint_is_bound_to_the_exact_raw_payload_bytes() {
        assert_ne!(
            fingerprint(br#"{"number":"1","generation":0}"#),
            fingerprint(br#"{"generation":0,"number":"1"}"#),
        );
    }

    #[test]
    fn every_public_call_action_has_exact_replay_and_payload_drift_rejection() {
        let root = root();
        let store = Store::new(&root).unwrap();
        for (index, action) in ["dial", "answer", "reject", "hangup", "dtmf", "ussd"]
            .into_iter()
            .enumerate()
        {
            let id = format!("all-actions-{action}");
            let original = request(
                &id,
                action,
                u8::try_from(index + 10).unwrap(),
                OwnerBinding {
                    owner: "module",
                    generation: 0,
                },
            );
            finish(&store, &original, 200, r#"{"accepted":true}"#);
            assert!(matches!(
                store.admit(&original).unwrap(),
                Admission::Replay(_)
            ));
            let changed = request(
                &id,
                action,
                u8::try_from(index + 20).unwrap(),
                OwnerBinding {
                    owner: "module",
                    generation: 0,
                },
            );
            assert!(matches!(
                store.admit(&changed),
                Err(StoreError::Configuration(code)) if code == "call_mutation_request_id_conflict"
            ));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn submitted_public_alias_and_ussd_reopen_as_receipts_without_redispatch() {
        for (index, action) in ["reject", "ussd"].into_iter().enumerate() {
            let root = root();
            let request = request(
                &format!("submitted-{action}"),
                action,
                u8::try_from(index + 40).unwrap(),
                OwnerBinding {
                    owner: "reader",
                    generation: 30 + u64::try_from(index).unwrap(),
                },
            );
            let first = Store::new(&root).unwrap();
            assert!(matches!(
                first.admit(&request).unwrap(),
                Admission::Dispatch
            ));
            first.claim(&request).unwrap();
            drop(first);

            let reopened = Store::new(&root).unwrap();
            reopened.recover().unwrap();
            let Lookup::Receipt(record) = reopened
                .lookup(
                    &request.client_request_id,
                    &request.action,
                    &request.raw_payload_fingerprint,
                )
                .unwrap()
            else {
                panic!("{action} was not recovered as an unknown receipt")
            };
            assert_eq!(record.phase, "unknown");
            assert_eq!(record.outcome, "unknown");
            assert_eq!(record.dispatch_count, 1);
            assert!(matches!(
                reopened.admit(&request).unwrap(),
                Admission::Receipt(_)
            ));
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn submitted_strict_call_reopens_unknown_and_never_dispatches_twice() {
        let root = root();
        let request = request(
            "submitted-answer",
            "answer",
            4,
            OwnerBinding {
                owner: "reader",
                generation: 30,
            },
        );
        let first = Store::new(&root).unwrap();
        assert!(matches!(
            first.admit(&request).unwrap(),
            Admission::Dispatch
        ));
        first.claim(&request).unwrap();
        drop(first);

        let reopened = Store::new(&root).unwrap();
        reopened.recover().unwrap();
        let Lookup::Receipt(record) = reopened
            .lookup(
                &request.client_request_id,
                &request.action,
                &request.raw_payload_fingerprint,
            )
            .unwrap()
        else {
            panic!("submitted request was not recovered as unknown")
        };
        assert_eq!(record.phase, "unknown");
        assert_eq!(record.outcome, "unknown");
        assert_eq!(record.dispatch_count, 1);
        assert!(matches!(
            reopened.admit(&request).unwrap(),
            Admission::Receipt(_)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dtmf_restart_is_terminal_unknown_no_replay() {
        let root = root();
        let request = request(
            "submitted-dtmf",
            "dtmf",
            5,
            OwnerBinding {
                owner: "module",
                generation: 0,
            },
        );
        let first = Store::new(&root).unwrap();
        assert!(matches!(
            first.admit(&request).unwrap(),
            Admission::Dispatch
        ));
        first.claim(&request).unwrap();
        drop(first);

        let reopened = Store::new(&root).unwrap();
        reopened.recover().unwrap();
        let Lookup::Receipt(record) = reopened
            .lookup(
                &request.client_request_id,
                &request.action,
                &request.raw_payload_fingerprint,
            )
            .unwrap()
        else {
            panic!("DTMF receipt missing")
        };
        assert_eq!(record.phase, "terminal");
        assert_eq!(record.outcome, "unknown_no_replay");
        assert_eq!(record.dispatch_count, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn physical_dispatch_claim_is_at_most_once() {
        let root = root();
        let store = Store::new(&root).unwrap();
        let request = request(
            "physical-once",
            "hangup",
            6,
            OwnerBinding {
                owner: "module",
                generation: 0,
            },
        );
        assert!(matches!(
            store.admit(&request).unwrap(),
            Admission::Dispatch
        ));
        let count = Arc::new(AtomicUsize::new(0));
        store.claim(&request).unwrap();
        count.fetch_add(1, Ordering::SeqCst);
        assert!(store.claim(&request).is_err());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_concurrent_claim_loser_observes_submitted_receipt() {
        let root = root();
        let store = Store::new(&root).unwrap();
        let request = request(
            "concurrent-exact-retry",
            "dial",
            9,
            OwnerBinding {
                owner: "reader",
                generation: 19,
            },
        );
        assert!(matches!(
            store.admit(&request).unwrap(),
            Admission::Dispatch
        ));
        assert!(matches!(
            store.admit(&request).unwrap(),
            Admission::Dispatch
        ));
        store.claim(&request).unwrap();
        assert!(store.claim(&request).is_err());
        let Lookup::Receipt(record) = store
            .lookup(
                &request.client_request_id,
                &request.action,
                &request.raw_payload_fingerprint,
            )
            .unwrap()
        else {
            panic!("concurrent claim loser did not recover the submitted receipt")
        };
        assert_eq!(record.phase, "submitted");
        assert_eq!(record.outcome, "unknown");
        assert_eq!(record.dispatch_count, 1);
        assert_eq!(receipt_response(&record).status(), StatusCode::ACCEPTED);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn exact_retry_replays_terminal_response_without_calling_dispatcher_again() {
        let root = root();
        let request = request(
            "http-replay",
            "dial",
            7,
            OwnerBinding {
                owner: "module",
                generation: 0,
            },
        );
        let count = Arc::new(AtomicUsize::new(0));
        let first_count = Arc::clone(&count);
        let first = execute(&root, request.clone(), move || async move {
            first_count.fetch_add(1, Ordering::SeqCst);
            crate::json_response(StatusCode::OK, &json!({"accepted":true}))
        })
        .await;
        assert_eq!(first.status(), StatusCode::OK);

        let retry_count = Arc::clone(&count);
        let retry = execute(&root, request, move || async move {
            retry_count.fetch_add(1, Ordering::SeqCst);
            crate::json_response(StatusCode::OK, &json!({"accepted":true}))
        })
        .await;
        assert_eq!(retry.status(), StatusCode::OK);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn strongly_bound_admitted_without_submit_can_resume_once_after_daemon_reopen() {
        let root = root();
        let request = request(
            "admitted-before-crash",
            "dial",
            8,
            OwnerBinding {
                owner: "reader",
                generation: 28,
            },
        );
        let first = Store::new(&root).unwrap();
        assert!(matches!(
            first.admit(&request).unwrap(),
            Admission::Dispatch
        ));
        drop(first);

        initialize(&root).unwrap();
        assert!(
            preflight(
                &root,
                &request.client_request_id,
                &request.action,
                &request.raw_payload_fingerprint,
            )
            .is_none()
        );

        let count = Arc::new(AtomicUsize::new(0));
        let dispatch_count = Arc::clone(&count);
        let response = execute(&root, request.clone(), move || async move {
            dispatch_count.fetch_add(1, Ordering::SeqCst);
            crate::json_response(StatusCode::OK, &json!({"accepted":true}))
        })
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        let replay_count = Arc::clone(&count);
        let replay = execute(&root, request, move || async move {
            replay_count.fetch_add(1, Ordering::SeqCst);
            crate::json_response(StatusCode::OK, &json!({"accepted":true}))
        })
        .await;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn old_module_admissions_never_dispatch_into_next_call_or_replacement_module() {
        for action in ["dial", "answer", "hangup", "dtmf"] {
            for replacement in ["next-call", "new-module", "new-sim"] {
                let root = root();
                let original = request(
                    &format!("old-{action}-{replacement}"),
                    action,
                    40,
                    OwnerBinding {
                        owner: "module",
                        generation: 0,
                    },
                );
                let first = Store::new(&root).unwrap();
                assert!(matches!(
                    first.admit(&original).unwrap(),
                    Admission::Dispatch
                ));
                drop(first); // Crash between durable admission and claim.
                initialize(&root).unwrap();

                // A new live call/module/SIM still projects module/0. That
                // coarse equality must not authorize the old user intent.
                let preflight = preflight(
                    &root,
                    &original.client_request_id,
                    action,
                    &original.raw_payload_fingerprint,
                )
                .expect("old module admission must stop before live dispatch");
                let value: Value = serde_json::from_slice(
                    &preflight.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                assert_eq!(value["phase"], "unknown");
                assert_eq!(value["dispatch_count"], 0);
                let count = Arc::new(AtomicUsize::new(0));
                for _ in 0..2 {
                    let attempts = count.clone();
                    let response = execute(&root, original.clone(), move || async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        crate::json_response(StatusCode::OK, &json!({"accepted":true}))
                    })
                    .await;
                    assert_eq!(response.status(), StatusCode::ACCEPTED);
                    initialize(&root).unwrap(); // Unknown remains durable.
                }
                assert_eq!(count.load(Ordering::SeqCst), 0, "{action}/{replacement}");
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn module_admitted_retry_is_quarantined_without_reopen_and_binding_drift_conflicts() {
        for action in ["answer", "hangup"] {
            let root = root();
            let store = Store::new(&root).unwrap();
            let original = request(
                "same-process-admission",
                action,
                41,
                OwnerBinding {
                    owner: "module",
                    generation: 0,
                },
            );
            assert!(matches!(
                store.admit(&original).unwrap(),
                Admission::Dispatch
            ));
            for (owner, generation) in [("reader", 0), ("module-vowifi", 0), ("module", 1)] {
                let mut changed = original.clone();
                changed.owner = owner.into();
                changed.generation = generation;
                assert!(
                    matches!(store.admit(&changed), Err(StoreError::Configuration(code))
                    if code == "call_mutation_request_id_conflict")
                );
            }
            let Admission::Receipt(record) = store.admit(&original).unwrap() else {
                panic!("unbound module retry was authorized to dispatch")
            };
            assert_eq!(record.phase, "unknown");
            assert_eq!(record.dispatch_count, 0);
            assert!(store.claim(&original).is_err());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn preflight_quarantines_module_admitted_before_owner_observation() {
        let root = root();
        let store = Store::new(&root).unwrap();
        let original = request(
            "preflight-module",
            "answer",
            42,
            OwnerBinding {
                owner: "module",
                generation: 0,
            },
        );
        assert!(matches!(
            store.admit(&original).unwrap(),
            Admission::Dispatch
        ));
        assert!(
            preflight(
                &root,
                &original.client_request_id,
                &original.action,
                &original.raw_payload_fingerprint
            )
            .is_some()
        );
        assert!(store.claim(&original).is_err());
        let records = decode_records(&store.archive.load().unwrap()).unwrap();
        assert_eq!(records[0].phase, "unknown");
        assert_eq!(records[0].dispatch_count, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn visible_terminal_replay_requires_confirmed_directory_durability() {
        let root = root();
        let original = request(
            "uncertain-terminal-directory-sync",
            "dial",
            54,
            OwnerBinding {
                owner: "reader",
                generation: 29,
            },
        );
        let store = Store::new(&root).unwrap();
        assert!(matches!(
            store.admit(&original).unwrap(),
            Admission::Dispatch
        ));
        store.claim(&original).unwrap();
        let terminal = StoredResponse {
            status: 409,
            body: r#"{"error":"call busy","code":"call_owner_busy"}"#.into(),
        };
        // The rename is visible, but finish has not confirmed its directory entry.
        store.archive.fail_commit_at_for_test(4);
        assert!(store.finish(&original, terminal.clone()).is_err());

        let lookup_replay = match store
            .lookup(
                &original.client_request_id,
                &original.action,
                &original.raw_payload_fingerprint,
            )
            .unwrap()
        {
            Lookup::Replay(response) => response,
            _ => panic!("visible terminal receipt must be available for replay"),
        };
        let admission_replay = match store.admit(&original).unwrap() {
            Admission::Replay(response) => response,
            _ => panic!("exact retry must not dispatch the original call again"),
        };
        for stored in [lookup_replay, admission_replay] {
            assert_eq!(stored.status, terminal.status);
            assert_eq!(stored.body, terminal.body);
            store.archive.fail_commit_at_for_test(7);
            let unknown = replay_response(&store, &stored, &original.client_request_id);
            assert_eq!(unknown.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert!(!unknown.headers().contains_key("x-switch-call-terminal"));
            assert!(!unknown.headers().contains_key("x-switch-call-request-id"));
            let body = unknown.into_body().collect().await.unwrap().to_bytes();
            let value: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["code"], "call_mutation_receipt_outcome_unknown");
        }

        let confirmed = replay_response(&store, &terminal, &original.client_request_id);
        assert_eq!(confirmed.status(), StatusCode::CONFLICT);
        assert_eq!(confirmed.headers()["x-switch-call-terminal"], "true");
        assert_eq!(
            confirmed.headers()["x-switch-call-request-id"],
            original.client_request_id
        );
        let replay = preflight(
            &root,
            &original.client_request_id,
            &original.action,
            &original.raw_payload_fingerprint,
        )
        .unwrap();
        assert_eq!(replay.headers()["x-switch-call-terminal"], "true");
        let replay = execute(&root, original, || async {
            panic!("durable terminal retry must never dispatch")
        })
        .await;
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        assert_eq!(replay.headers()["x-switch-call-terminal"], "true");
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn durable_negative_receipt_marks_original_id_but_unknown_or_unreadable_does_not() {
        let root = root();
        let original = request(
            "terminal-negative-request",
            "dial",
            52,
            OwnerBinding {
                owner: "reader",
                generation: 28,
            },
        );
        let result = execute(&root, original.clone(), || async {
            crate::json_response(
                StatusCode::CONFLICT,
                &json!({"error":"call busy","code":"call_owner_busy"}),
            )
        })
        .await;
        assert_eq!(result.status(), StatusCode::CONFLICT);
        assert_eq!(result.headers()["x-switch-call-terminal"], "true");
        assert_eq!(
            result.headers()["x-switch-call-request-id"],
            original.client_request_id
        );
        let replay = preflight(
            &root,
            &original.client_request_id,
            &original.action,
            &original.raw_payload_fingerprint,
        )
        .unwrap();
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        assert_eq!(replay.headers()["x-switch-call-terminal"], "true");
        assert_eq!(
            replay.headers()["x-switch-call-request-id"],
            original.client_request_id
        );

        let unresolved = request(
            "unknown-module-request",
            "dial",
            53,
            OwnerBinding {
                owner: "module",
                generation: 0,
            },
        );
        Store::new(&root).unwrap().admit(&unresolved).unwrap();
        let unknown = preflight(
            &root,
            &unresolved.client_request_id,
            &unresolved.action,
            &unresolved.raw_payload_fingerprint,
        )
        .unwrap();
        assert_eq!(unknown.status(), StatusCode::ACCEPTED);
        assert!(!unknown.headers().contains_key("x-switch-call-terminal"));

        let invalid_root = root.join("not-a-directory");
        fs::write(&invalid_root, b"fixture").unwrap();
        let unreadable = preflight(
            &invalid_root,
            &original.client_request_id,
            &original.action,
            &original.raw_payload_fingerprint,
        )
        .unwrap();
        assert_eq!(unreadable.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!unreadable.headers().contains_key("x-switch-call-terminal"));

        let invalid_header = response_from_stored(
            &StoredResponse {
                status: 409,
                body: "{}".into(),
            },
            "invalid\nrequest",
        );
        assert_eq!(invalid_header.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            !invalid_header
                .headers()
                .contains_key("x-switch-call-terminal")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
