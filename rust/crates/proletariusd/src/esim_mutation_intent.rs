//! Durable identity and replay ledger for every public eSIM mutation.
//!
//! The card operation journals keep protocol-specific progress.  This ledger
//! sits immediately in front of physical dispatch and gives the four public
//! mutations one common, restart-safe request identity.  A submitted record is
//! never dispatched again, even when its HTTP response or daemon process is
//! lost.

use std::collections::BTreeMap;
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

const STORE_FILE: &str = "esim-mutation-intents-v1.json";
const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RECORDS: usize = 512;
const SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct OwnerBinding {
    pub(crate) scope: String,
    pub(crate) owner_identity: String,
    pub(crate) card_identity: String,
    pub(crate) generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct MutationRequest {
    pub(crate) client_request_id: String,
    pub(crate) action: String,
    pub(crate) fingerprint: String,
    pub(crate) owner: OwnerBinding,
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
    fingerprint: String,
    action: String,
    owner: OwnerBinding,
    phase: String,
    operation_id: String,
    dispatch_count: u32,
    created_at_ms: u64,
    updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response: Option<StoredResponse>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Snapshot {
    schema_version: u16,
    records: BTreeMap<String, Record>,
}

#[derive(Clone)]
pub(crate) struct Store {
    archive: ArchiveStore,
}

enum Admission {
    Dispatch(String),
    Replay(StoredResponse),
    Existing(Record),
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

fn read_snapshot(items: &[Value]) -> Result<Snapshot, StoreError> {
    if items.is_empty() {
        return Ok(Snapshot {
            schema_version: SCHEMA_VERSION,
            records: BTreeMap::new(),
        });
    }
    if items.len() != 1 {
        return Err(failure("invalid_esim_mutation_intent_store"));
    }
    let snapshot: Snapshot = serde_json::from_value(items[0].clone()).map_err(StoreError::Json)?;
    if snapshot.records.len() > MAX_RECORDS
        || snapshot.schema_version != SCHEMA_VERSION
        || snapshot.records.iter().any(|(id, record)| {
            id != &record.client_request_id
                || record.schema_version != SCHEMA_VERSION
                || !valid_request_id(id)
                || record.fingerprint.len() != 64
                || !record
                    .fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || record.action.is_empty()
                || record.operation_id.is_empty()
                || record.owner.scope.is_empty()
                || record.owner.owner_identity.is_empty()
                || record.owner.card_identity.is_empty()
                || !matches!(
                    record.phase.as_str(),
                    "prepared" | "submitted" | "terminal" | "unknown"
                )
                || record.dispatch_count > 1
                || (record.phase == "terminal") != record.response.is_some()
        })
    {
        return Err(failure("invalid_esim_mutation_intent_store"));
    }
    Ok(snapshot)
}

fn persist(items: &mut Vec<Value>, snapshot: Snapshot) -> Result<(), StoreError> {
    *items = vec![serde_json::to_value(snapshot).map_err(StoreError::Json)?];
    Ok(())
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn same_owner(a: &OwnerBinding, b: &OwnerBinding) -> bool {
    a.scope == b.scope
        && a.owner_identity == b.owner_identity
        && a.card_identity == b.card_identity
        && a.generation == b.generation
}

// The full binding is required to replay one request ID, but unresolved work
// fences its whole public scope. A card can move between selected readers, and
// module or reader observations can change before the first outcome is known.
fn same_mutation_domain(a: &OwnerBinding, b: &OwnerBinding) -> bool {
    a.scope == b.scope
}

fn unresolved(phase: &str) -> bool {
    matches!(phase, "prepared" | "submitted" | "unknown")
}

fn random_operation_id() -> Result<String, StoreError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| failure("esim_mutation_random_unavailable"))?;
    Ok(format!(
        "esim-{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

impl Store {
    pub(crate) fn open(root: &Path) -> Result<Self, StoreError> {
        let archive =
            ArchiveStore::with_limits(root.join(STORE_FILE), MAX_STORE_BYTES, MAX_RECORDS)?
                .require_nonempty_ledger()?;
        let store = Self { archive };
        store.recover()?;
        Ok(store)
    }

    /// Claim is durably committed before physical dispatch. A prepared record
    /// with no claim therefore proves no dispatch, even after a crash. Keep a
    /// terminal receipt for that exact ID; never run its closure on a replay.
    /// A submitted record cannot prove its physical outcome and stays fenced.
    fn recover(&self) -> Result<(), StoreError> {
        self.archive.mutate_if_changed(|items| {
            let mut snapshot = read_snapshot(items)?;
            let mut changed = false;
            let now = now_ms();
            for record in snapshot.records.values_mut() {
                if record.phase == "prepared" && record.dispatch_count == 0 {
                    record.phase = "terminal".into();
                    record.response = Some(StoredResponse {
                        status: StatusCode::CONFLICT.as_u16(),
                        body: json!({
                            "code": "esim_mutation_not_dispatched",
                            "error": "此 eSIM 请求在提交卡片前中断，未操作卡片；请核对后发起新请求",
                            "retryable": false,
                            "dispatched": false
                        })
                        .to_string(),
                    });
                    record.updated_at_ms = now;
                    changed = true;
                } else if matches!(record.phase.as_str(), "prepared" | "submitted") {
                    // Unexpected legacy prepared records with a nonzero claim
                    // count also remain unknown rather than assuming safety.
                    record.phase = "unknown".into();
                    record.updated_at_ms = now;
                    changed = true;
                }
            }
            if changed {
                persist(items, snapshot)?;
            }
            Ok(((), changed))
        })
    }

    fn admit(&self, request: &MutationRequest) -> Result<Admission, StoreError> {
        if !valid_request_id(&request.client_request_id)
            || request.action.is_empty()
            || request.fingerprint.len() != 64
            || !request
                .fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || request.owner.scope.is_empty()
            || request.owner.owner_identity.is_empty()
            || request.owner.card_identity.is_empty()
        {
            return Err(failure("invalid_esim_mutation_intent"));
        }
        self.archive.mutate_if_changed(|items| {
            let mut snapshot = read_snapshot(items)?;
            if let Some(record) = snapshot.records.get(&request.client_request_id) {
                if record.fingerprint != request.fingerprint
                    || record.action != request.action
                    || !same_owner(&record.owner, &request.owner)
                {
                    return Err(failure("esim_mutation_request_id_conflict"));
                }
                return Ok((
                    match &record.response {
                        Some(response) => Admission::Replay(response.clone()),
                        None => Admission::Existing(record.clone()),
                    },
                    false,
                ));
            }
            if snapshot.records.values().any(|record| {
                same_mutation_domain(&record.owner, &request.owner) && unresolved(&record.phase)
            }) {
                return Err(failure("esim_mutation_owner_unresolved"));
            }
            if snapshot.records.len() >= MAX_RECORDS {
                return Err(failure("esim_mutation_store_full"));
            }
            let operation_id = random_operation_id()?;
            let now = now_ms();
            snapshot.records.insert(
                request.client_request_id.clone(),
                Record {
                    schema_version: SCHEMA_VERSION,
                    client_request_id: request.client_request_id.clone(),
                    fingerprint: request.fingerprint.clone(),
                    action: request.action.clone(),
                    owner: request.owner.clone(),
                    phase: "prepared".into(),
                    operation_id: operation_id.clone(),
                    dispatch_count: 0,
                    created_at_ms: now,
                    updated_at_ms: now,
                    response: None,
                },
            );
            persist(items, snapshot)?;
            Ok((Admission::Dispatch(operation_id), true))
        })
    }

    fn claim(&self, request: &MutationRequest, operation_id: &str) -> Result<(), StoreError> {
        self.archive.mutate(|items| {
            let mut snapshot = read_snapshot(items)?;
            let record = snapshot
                .records
                .get_mut(&request.client_request_id)
                .ok_or_else(|| failure("esim_mutation_intent_missing"))?;
            if record.operation_id != operation_id
                || record.phase != "prepared"
                || record.dispatch_count != 0
                || record.fingerprint != request.fingerprint
                || record.action != request.action
                || !same_owner(&record.owner, &request.owner)
            {
                return Err(failure("esim_mutation_dispatch_already_claimed"));
            }
            record.phase = "submitted".into();
            record.dispatch_count = 1;
            record.updated_at_ms = now_ms();
            persist(items, snapshot)
        })
    }

    fn finish(
        &self,
        request: &MutationRequest,
        operation_id: &str,
        response: StoredResponse,
    ) -> Result<(), StoreError> {
        self.archive.mutate(|items| {
            let mut snapshot = read_snapshot(items)?;
            let record = snapshot
                .records
                .get_mut(&request.client_request_id)
                .ok_or_else(|| failure("esim_mutation_intent_missing"))?;
            if record.operation_id != operation_id
                || record.phase != "submitted"
                || record.dispatch_count != 1
                || record.response.is_some()
            {
                return Err(failure("esim_mutation_terminal_conflict"));
            }
            record.phase = "terminal".into();
            record.updated_at_ms = now_ms();
            record.response = Some(response);
            persist(items, snapshot)
        })
    }

    fn get(&self, client_request_id: &str) -> Result<Option<Record>, StoreError> {
        if !valid_request_id(client_request_id) {
            return Err(failure("invalid_esim_mutation_intent"));
        }
        let snapshot = read_snapshot(&self.archive.load()?)?;
        Ok(snapshot.records.get(client_request_id).cloned())
    }
}

static SERVICE: std::sync::OnceLock<Store> = std::sync::OnceLock::new();

pub(crate) fn initialize(root: &Path) -> Result<(), StoreError> {
    let store = Store::open(root)?;
    SERVICE
        .set(store)
        .map_err(|_| failure("esim_mutation_intent_already_initialized"))
}

fn service() -> Result<&'static Store, StoreError> {
    SERVICE
        .get()
        .ok_or_else(|| failure("esim_mutation_intent_unavailable"))
}

pub(crate) fn fingerprint<T: Serialize>(payload: &T) -> Result<String, Response<Full<Bytes>>> {
    let value = serde_json::to_value(payload).map_err(|_| {
        rejection(
            StatusCode::BAD_REQUEST,
            "invalid_esim_mutation_payload",
            "eSIM 操作请求格式无效",
        )
    })?;
    let canonical = canonical_json(&value);
    let bytes = serde_json::to_vec(&canonical).map_err(|_| {
        rejection(
            StatusCode::BAD_REQUEST,
            "invalid_esim_mutation_payload",
            "eSIM 操作请求格式无效",
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(crate) fn reader_request<T: Serialize>(
    state: &crate::ServerState,
    reader: &str,
    client_request_id: &str,
    action: &str,
    payload: &T,
) -> Result<MutationRequest, Response<Full<Bytes>>> {
    let Some((selected, generation)) = state.reader_gate.selected_reader_generation() else {
        return Err(rejection(
            StatusCode::CONFLICT,
            "esim_mutation_owner_unavailable",
            "读卡器归属已变化，尚未操作卡片",
        ));
    };
    if selected != reader || generation == 0 {
        return Err(rejection(
            StatusCode::CONFLICT,
            "esim_mutation_owner_unavailable",
            "读卡器归属已变化，尚未操作卡片",
        ));
    }
    let card_identity = state
        .reader_gate
        .reader_metadata(reader, generation)
        .and_then(|metadata| fingerprint(&metadata).ok())
        .map(|digest| format!("reader-metadata-sha256:{digest}"))
        .unwrap_or_else(|| format!("reader-selection:{reader}:{generation}"));
    Ok(MutationRequest {
        client_request_id: client_request_id.to_owned(),
        action: action.to_owned(),
        fingerprint: fingerprint(payload)?,
        owner: OwnerBinding {
            scope: "reader".into(),
            owner_identity: format!("reader:{reader}"),
            card_identity,
            generation,
        },
    })
}

pub(crate) fn module_request<T: Serialize>(
    state: &crate::ServerState,
    client_request_id: &str,
    action: &str,
    payload: &T,
) -> Result<MutationRequest, Response<Full<Bytes>>> {
    let Some((owner_identity, card_identity, generation)) =
        crate::module_esim_transport::mutation_identity(state)
    else {
        return Err(rejection(
            StatusCode::CONFLICT,
            "esim_mutation_owner_unavailable",
            "模块卡片归属尚未确认，尚未操作卡片",
        ));
    };
    Ok(MutationRequest {
        client_request_id: client_request_id.to_owned(),
        action: action.to_owned(),
        fingerprint: fingerprint(payload)?,
        owner: OwnerBinding {
            scope: "module".into(),
            owner_identity,
            card_identity,
            generation,
        },
    })
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut ordered = serde_json::Map::new();
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                ordered.insert(key.clone(), canonical_json(&object[key]));
            }
            Value::Object(ordered)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        _ => value.clone(),
    }
}

pub(crate) async fn execute<F, Fut>(request: MutationRequest, dispatch: F) -> Response<Full<Bytes>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Response<Full<Bytes>>>,
{
    let store = match service() {
        Ok(store) => store,
        Err(_) => {
            return rejection(
                StatusCode::SERVICE_UNAVAILABLE,
                "esim_mutation_intent_unavailable",
                "无法安全记录 eSIM 操作请求，尚未操作卡片",
            );
        }
    };
    execute_with_store(store, request, dispatch).await
}

async fn execute_with_store<F, Fut>(
    store: &Store,
    request: MutationRequest,
    dispatch: F,
) -> Response<Full<Bytes>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Response<Full<Bytes>>>,
{
    match store.admit(&request) {
        Ok(Admission::Replay(response)) => response_from_stored(&response),
        Ok(Admission::Existing(record)) => receipt_response(StatusCode::ACCEPTED, &record),
        Ok(Admission::Dispatch(operation_id)) => {
            if store.claim(&request, &operation_id).is_err() {
                return rejection(
                    StatusCode::CONFLICT,
                    "esim_mutation_dispatch_already_claimed",
                    "此 eSIM 请求已被受理；请按原请求编号查询结果",
                );
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
            if store
                .finish(&request, &operation_id, stored.clone())
                .is_err()
            {
                return rejection(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "esim_mutation_receipt_unavailable",
                    "eSIM 操作已提交，但持久回执未能确认；请按原请求编号查询，切勿重试",
                );
            }
            response_from_stored(&stored)
        }
        Err(StoreError::Configuration(code))
            if matches!(
                code.as_str(),
                "esim_mutation_request_id_conflict"
                    | "esim_mutation_owner_unresolved"
                    | "esim_mutation_dispatch_already_claimed"
            ) =>
        {
            rejection(StatusCode::CONFLICT, &code, "eSIM 请求身份或卡片归属冲突")
        }
        Err(_) => rejection(
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_mutation_intent_unavailable",
            "无法安全记录 eSIM 操作请求，尚未操作卡片",
        ),
    }
}

pub(crate) fn query(scope: &str, query: &str) -> Response<Full<Bytes>> {
    let ids = url::form_urlencoded::parse(query.as_bytes())
        .filter(|(key, _)| key == "client_request_id")
        .map(|(_, value)| value.into_owned())
        .collect::<Vec<_>>();
    if ids.len() != 1 || !valid_request_id(&ids[0]) {
        return rejection(
            StatusCode::BAD_REQUEST,
            "invalid_esim_mutation_intent",
            "client_request_id 无效",
        );
    }
    match service().and_then(|store| store.get(&ids[0])) {
        Ok(Some(record)) if record.owner.scope == scope => {
            receipt_response(StatusCode::OK, &record)
        }
        Ok(Some(_)) => rejection(
            StatusCode::CONFLICT,
            "esim_mutation_owner_conflict",
            "请求编号不属于当前 eSIM 操作范围",
        ),
        Ok(None) => rejection(
            StatusCode::NOT_FOUND,
            "esim_mutation_intent_not_found",
            "未找到此 eSIM 请求编号",
        ),
        Err(_) => rejection(
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_mutation_intent_unavailable",
            "无法读取 eSIM 操作回执",
        ),
    }
}

fn response_from_stored(response: &StoredResponse) -> Response<Full<Bytes>> {
    Response::builder()
        .status(response.status)
        .header("content-type", "application/json; charset=utf-8")
        .body(Full::new(Bytes::copy_from_slice(response.body.as_bytes())))
        .unwrap_or_else(|_| {
            rejection(
                StatusCode::INTERNAL_SERVER_ERROR,
                "esim_mutation_receipt_invalid",
                "eSIM 操作回执无效",
            )
        })
}

fn receipt_response(status: StatusCode, record: &Record) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({
            "client_request_id": record.client_request_id,
            "action": record.action,
            "scope": record.owner.scope,
            "owner_identity": record.owner.owner_identity,
            "card_identity": record.owner.card_identity,
            "generation": record.owner.generation,
            "phase": record.phase,
            "operation_id": record.operation_id,
            "dispatch_count": record.dispatch_count,
            "created_at_ms": record.created_at_ms,
            "updated_at_ms": record.updated_at_ms,
            "response_status": record.response.as_ref().map(|value| value.status),
            "response": record.response.as_ref().and_then(|value| serde_json::from_str::<Value>(&value.body).ok()),
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
        .join("tmp/esim-mutation-intent-tests");
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

    fn owner(scope: &str, card: &str, generation: u64) -> OwnerBinding {
        OwnerBinding {
            scope: scope.into(),
            owner_identity: format!("owner-{card}"),
            card_identity: card.into(),
            generation,
        }
    }

    fn bound_owner(scope: &str, physical_owner: &str, card: &str, generation: u64) -> OwnerBinding {
        OwnerBinding {
            scope: scope.into(),
            owner_identity: physical_owner.into(),
            card_identity: card.into(),
            generation,
        }
    }

    fn request(id: &str, fingerprint: char, owner: OwnerBinding) -> MutationRequest {
        MutationRequest {
            client_request_id: id.into(),
            action: "switch".into(),
            fingerprint: fingerprint.to_string().repeat(64),
            owner,
        }
    }

    fn terminal(store: &Store, request: &MutationRequest, body: &str) {
        let Admission::Dispatch(operation_id) = store.admit(request).unwrap() else {
            panic!("new request not dispatched")
        };
        store.claim(request, &operation_id).unwrap();
        store
            .finish(
                request,
                &operation_id,
                StoredResponse {
                    status: 200,
                    body: body.into(),
                },
            )
            .unwrap();
    }

    #[tokio::test]
    async fn full_terminal_ledger_retains_oldest_receipt_and_refuses_new_identity() {
        let root = root();
        let store = Store::open(&root).unwrap();
        let oldest = request("terminal-0", 'a', owner("reader", "card-a", 7));
        terminal(&store, &oldest, r#"{"oldest":true}"#);
        // Seed terminal snapshots directly; the boundary under test is the
        // next admission, not 511 repeated disk writes.
        store
            .archive
            .mutate(|items| {
                let mut snapshot = read_snapshot(items)?;
                let template = snapshot.records["terminal-0"].clone();
                for index in 1..MAX_RECORDS {
                    let mut record = template.clone();
                    record.client_request_id = format!("terminal-{index}");
                    snapshot
                        .records
                        .insert(record.client_request_id.clone(), record);
                }
                persist(items, snapshot)
            })
            .unwrap();
        let before = fs::read(root.join(STORE_FILE)).unwrap();
        let dispatches = AtomicUsize::new(0);
        let rejected = execute_with_store(
            &store,
            request("capacity-dispatch", 'a', oldest.owner.clone()),
            || async {
                dispatches.fetch_add(1, Ordering::SeqCst);
                crate::json_response(StatusCode::OK, &json!({}))
            },
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert!(
            matches!(store.admit(&request("terminal-512", 'a', oldest.owner.clone())),
            Err(StoreError::Configuration(code)) if code == "esim_mutation_store_full")
        );
        assert!(
            matches!(store.admit(&oldest).unwrap(), Admission::Replay(response) if response.body == r#"{"oldest":true}"#)
        );
        assert!(
            matches!(store.admit(&request("terminal-0", 'b', oldest.owner)),
            Err(StoreError::Configuration(code)) if code == "esim_mutation_request_id_conflict")
        );
        assert_eq!(fs::read(root.join(STORE_FILE)).unwrap(), before);
        assert_eq!(
            read_snapshot(&Store::open(&root).unwrap().archive.load().unwrap())
                .unwrap()
                .records
                .len(),
            MAX_RECORDS
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn corrupt_reopen_and_live_load_never_reach_dispatch() {
        for damage in [b"".as_slice(), b"null", b"[]", b"{broken", b"[{}]"] {
            let root = root();
            let store = Store::open(&root).unwrap();
            let path = root.join(STORE_FILE);
            fs::write(&path, damage).unwrap();
            assert!(Store::open(&root).is_err());
            assert!(store.recover().is_err());
            let dispatches = AtomicUsize::new(0);
            let response = execute_with_store(
                &store,
                request("corrupt", 'a', owner("reader", "card", 1)),
                || async {
                    dispatches.fetch_add(1, Ordering::SeqCst);
                    crate::json_response(StatusCode::OK, &json!({}))
                },
            )
            .await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(dispatches.load(Ordering::SeqCst), 0);
            assert_eq!(fs::read(path).unwrap(), damage);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn restart_before_claim_ends_original_without_dispatch_and_unblocks_successor() {
        let root = root();
        let original = request("prepared-crash", 'a', owner("reader", "card-a", 7));
        let first = Store::open(&root).unwrap();
        assert!(matches!(
            first.admit(&original).unwrap(),
            Admission::Dispatch(_)
        ));
        drop(first);

        let reopened = Store::open(&root).unwrap();
        let record = reopened.get(&original.client_request_id).unwrap().unwrap();
        assert_eq!(record.phase, "terminal");
        assert_eq!(record.dispatch_count, 0);
        let Admission::Replay(response) = reopened.admit(&original).unwrap() else {
            panic!("unclaimed original must never dispatch after recovery")
        };
        assert_eq!(response.status, 409);
        let body: Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(body["code"], "esim_mutation_not_dispatched");
        assert_eq!(body["dispatched"], false);
        assert!(reopened.claim(&original, &record.operation_id).is_err());
        assert!(matches!(
            reopened
                .admit(&request("new-after-prepared", 'b', original.owner.clone()))
                .unwrap(),
            Admission::Dispatch(_)
        ));
        drop(reopened);
        // A second restart must not turn a terminal unclaimed receipt unknown.
        let again = Store::open(&root).unwrap();
        assert!(matches!(
            again.admit(&original).unwrap(),
            Admission::Replay(_)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_after_claim_retains_unknown_and_never_dispatches_successor() {
        let root = root();
        let original = request("claimed-crash", 'a', owner("module", "card-a", 7));
        let first = Store::open(&root).unwrap();
        let Admission::Dispatch(id) = first.admit(&original).unwrap() else {
            panic!("not admitted")
        };
        first.claim(&original, &id).unwrap();
        drop(first);
        let reopened = Store::open(&root).unwrap();
        let Admission::Existing(record) = reopened.admit(&original).unwrap() else {
            panic!("claimed request redispatched")
        };
        assert_eq!(record.phase, "unknown");
        assert_eq!(record.dispatch_count, 1);
        assert!(matches!(
            reopened.admit(&request("after-claimed", 'b', original.owner.clone())),
            Err(StoreError::Configuration(code)) if code == "esim_mutation_owner_unresolved"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_exact_requests_have_one_physical_claim() {
        let root = root();
        let store = Store::open(&root).unwrap();
        let request = request("concurrent", 'a', owner("module", "card", 1));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let dispatches = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let (store, request, barrier, dispatches) = (
                    store.clone(),
                    request.clone(),
                    barrier.clone(),
                    dispatches.clone(),
                );
                std::thread::spawn(move || {
                    barrier.wait();
                    if let Admission::Dispatch(id) = store.admit(&request).unwrap() {
                        store.claim(&request, &id).unwrap();
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
    fn lost_response_reopens_and_replays_exact_receipt_without_dispatch() {
        let root = root();
        let request = request("lost-response", 'a', owner("reader", "card-a", 7));
        let first = Store::open(&root).unwrap();
        terminal(&first, &request, r#"{"message":"done"}"#);
        drop(first);
        let reopened = Store::open(&root).unwrap();
        match reopened.admit(&request).unwrap() {
            Admission::Replay(response) => {
                assert_eq!(response.status, 200);
                assert_eq!(response.body, r#"{"message":"done"}"#);
            }
            _ => panic!("terminal request was not replayed"),
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_id_drift_and_cross_owner_or_card_are_conflicts() {
        let root = root();
        let store = Store::open(&root).unwrap();
        let original = request("same-id", 'a', owner("reader", "card-a", 3));
        terminal(&store, &original, "{}");
        for changed in [
            request("same-id", 'b', owner("reader", "card-a", 3)),
            request("same-id", 'a', owner("module", "card-a", 3)),
            request("same-id", 'a', owner("reader", "card-b", 3)),
            request("same-id", 'a', owner("reader", "card-a", 4)),
        ] {
            assert!(matches!(
                store.admit(&changed),
                Err(StoreError::Configuration(code)) if code == "esim_mutation_request_id_conflict"
            ));
        }
        let module = request(
            "module-same-id",
            'c',
            bound_owner("module", "serial:old-epoch:1", "serial-card:1", 3),
        );
        terminal(&store, &module, "{}");
        let module_after_drift = request(
            "module-same-id",
            'c',
            bound_owner("module", "serial:new-epoch:2", "serial-card:2", 4),
        );
        assert!(matches!(
            store.admit(&module_after_drift),
            Err(StoreError::Configuration(code)) if code == "esim_mutation_request_id_conflict"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn different_gui_ids_wait_for_terminal_even_when_the_first_response_is_stale() {
        let root = root();
        let store = Store::open(&root).unwrap();
        let binding = bound_owner("reader", "reader:Reader A", "metadata-a", 3);
        let first = request("gui-a", 'a', binding.clone());
        let second = request("gui-b", 'b', binding);
        let Admission::Dispatch(operation_id) = store.admit(&first).unwrap() else {
            panic!("first GUI was not admitted")
        };
        store.claim(&first, &operation_id).unwrap();

        let dispatches = AtomicUsize::new(0);
        let rejected = execute_with_store(&store, second.clone(), || async {
            dispatches.fetch_add(1, Ordering::SeqCst);
            crate::json_response(StatusCode::OK, &json!({"second": true}))
        })
        .await;
        assert_eq!(rejected.status(), StatusCode::CONFLICT);
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert!(store.get(&second.client_request_id).unwrap().is_none());

        store
            .finish(
                &first,
                &operation_id,
                StoredResponse {
                    status: 409,
                    body: r#"{"code":"stale_reader_generation"}"#.into(),
                },
            )
            .unwrap();
        assert!(matches!(
            store.admit(&first).unwrap(),
            Admission::Replay(response) if response.status == 409
        ));
        let admitted = execute_with_store(&store, second.clone(), || async {
            dispatches.fetch_add(1, Ordering::SeqCst);
            crate::json_response(StatusCode::OK, &json!({"second": true}))
        })
        .await;
        assert_eq!(admitted.status(), StatusCode::OK);
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        assert!(matches!(
            store.admit(&second).unwrap(),
            Admission::Replay(response) if response.status == 200
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unresolved_reader_fences_identity_drift_and_same_card_moved_to_another_reader() {
        let root = root();
        let store = Store::open(&root).unwrap();
        let first = request(
            "reader-original",
            'a',
            bound_owner("reader", "reader:Reader A", "metadata-a", 3),
        );
        let Admission::Dispatch(operation_id) = store.admit(&first).unwrap() else {
            panic!("original reader request was not admitted")
        };
        store.claim(&first, &operation_id).unwrap();
        let moved_card = request(
            "reader-card-moved",
            'd',
            bound_owner("reader", "reader:Reader B", "metadata-a", 5),
        );
        for successor in [
            request(
                "reader-new-generation",
                'b',
                bound_owner("reader", "reader:Reader A", "metadata-a", 4),
            ),
            request(
                "reader-new-metadata",
                'c',
                bound_owner("reader", "reader:Reader A", "metadata-b", 3),
            ),
            request(
                "reader-reopened-drift",
                'e',
                bound_owner("reader", "reader:Reader A", "metadata-b", 4),
            ),
            moved_card.clone(),
        ] {
            assert!(matches!(
                store.admit(&successor),
                Err(StoreError::Configuration(code)) if code == "esim_mutation_owner_unresolved"
            ));
            assert!(store.get(&successor.client_request_id).unwrap().is_none());
        }
        store
            .finish(
                &first,
                &operation_id,
                StoredResponse {
                    status: 409,
                    body: r#"{"code":"stale_reader_generation"}"#.into(),
                },
            )
            .unwrap();
        assert!(matches!(
            store.admit(&moved_card),
            Ok(Admission::Dispatch(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn reader_restart_keeps_unknown_fence_across_reader_move_and_identity_drift() {
        let root = root();
        let first = Store::open(&root).unwrap();
        let original = request(
            "reader-before-restart",
            'a',
            bound_owner("reader", "reader:Reader A", "metadata-a", 3),
        );
        let Admission::Dispatch(operation_id) = first.admit(&original).unwrap() else {
            panic!("original reader request was not admitted")
        };
        first.claim(&original, &operation_id).unwrap();
        drop(first);

        let reopened = Store::open(&root).unwrap();
        let Admission::Existing(recovered) = reopened.admit(&original).unwrap() else {
            panic!("original reader request must remain queryable")
        };
        assert_eq!(recovered.phase, "unknown");
        assert_eq!(recovered.dispatch_count, 1);
        let dispatches = AtomicUsize::new(0);
        for successor in [
            request(
                "reader-moved-after-restart",
                'b',
                bound_owner("reader", "reader:Reader B", "metadata-a", 4),
            ),
            request(
                "reader-reopened-after-restart",
                'c',
                bound_owner("reader", "reader:Reader A", "metadata-b", 4),
            ),
        ] {
            let rejected = execute_with_store(&reopened, successor.clone(), || async {
                dispatches.fetch_add(1, Ordering::SeqCst);
                crate::json_response(StatusCode::OK, &json!({}))
            })
            .await;
            assert_eq!(rejected.status(), StatusCode::CONFLICT);
            assert!(
                reopened
                    .get(&successor.client_request_id)
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unresolved_module_survives_transport_owner_card_and_generation_drift() {
        let root = root();
        let store = Store::open(&root).unwrap();
        let first = request(
            "module-original",
            'a',
            bound_owner("module", "serial:old-epoch:1", "serial-card:1", 3),
        );
        let Admission::Dispatch(operation_id) = store.admit(&first).unwrap() else {
            panic!("original module request was not admitted")
        };
        store.claim(&first, &operation_id).unwrap();
        let successor = request(
            "module-new-session",
            'b',
            bound_owner("module", "usb:new-producer:9", "usb-session:new", 1),
        );
        assert!(matches!(
            store.admit(&successor),
            Err(StoreError::Configuration(code)) if code == "esim_mutation_owner_unresolved"
        ));
        assert!(store.get(&successor.client_request_id).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn restart_keeps_unknown_module_fence_across_new_owner_epoch() {
        let root = root();
        let first = Store::open(&root).unwrap();
        let original = request(
            "before-restart",
            'a',
            bound_owner("module", "serial:old-epoch:1", "serial-card:1", 3),
        );
        let Admission::Dispatch(operation_id) = first.admit(&original).unwrap() else {
            panic!("original module request was not admitted")
        };
        first.claim(&original, &operation_id).unwrap();
        drop(first);

        let reopened = Store::open(&root).unwrap();
        let Admission::Existing(recovered) = reopened.admit(&original).unwrap() else {
            panic!("original request must remain queryable")
        };
        assert_eq!(recovered.phase, "unknown");
        assert_eq!(recovered.dispatch_count, 1);
        let successor = request(
            "after-restart",
            'b',
            bound_owner("module", "serial:new-epoch:1", "serial-card:1", 3),
        );
        let dispatches = AtomicUsize::new(0);
        let rejected = execute_with_store(&reopened, successor.clone(), || async {
            dispatches.fetch_add(1, Ordering::SeqCst);
            crate::json_response(StatusCode::OK, &json!({}))
        })
        .await;
        assert_eq!(rejected.status(), StatusCode::CONFLICT);
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert!(
            reopened
                .get(&successor.client_request_id)
                .unwrap()
                .is_none()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn submitted_request_becomes_unknown_after_reopen_and_blocks_successor() {
        let root = root();
        let first = Store::open(&root).unwrap();
        let old = request("old", 'a', owner("reader", "card-a", 9));
        let Admission::Dispatch(operation_id) = first.admit(&old).unwrap() else {
            panic!("not dispatched")
        };
        first.claim(&old, &operation_id).unwrap();
        drop(first);
        let reopened = Store::open(&root).unwrap();
        let Admission::Existing(recovered) = reopened.admit(&old).unwrap() else {
            panic!("unknown request was not recovered")
        };
        assert_eq!(recovered.phase, "unknown");
        assert_eq!(recovered.dispatch_count, 1);
        let successor = request("successor", 'b', owner("reader", "card-a", 9));
        assert!(matches!(
            reopened.admit(&successor),
            Err(StoreError::Configuration(code)) if code == "esim_mutation_owner_unresolved"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dispatcher_can_be_claimed_at_most_once() {
        let root = root();
        let store = Store::open(&root).unwrap();
        let request = request("once", 'a', owner("module", "card-a", 1));
        let Admission::Dispatch(operation_id) = store.admit(&request).unwrap() else {
            panic!("not dispatched")
        };
        let dispatches = Arc::new(AtomicUsize::new(0));
        store.claim(&request, &operation_id).unwrap();
        dispatches.fetch_add(1, Ordering::SeqCst);
        assert!(store.claim(&request, &operation_id).is_err());
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
