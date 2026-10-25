//! Durable caller receipts for public reader and BLE lifecycle mutations.
//!
//! A submitted record is written before device dispatch.  Terminal replies
//! are replayed byte-for-byte; an interrupted submission is fenced as unknown
//! after restart and is never dispatched again.  The immutable binding keeps
//! an old disconnect from being recaptured against a successor reader.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Response, StatusCode};
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const FILE: &str = "reader-lifecycle-mutation-receipts-v1.json";
const VERSION: u16 = 1;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RECORDS: usize = 4_096;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub(crate) reader: String,
    pub(crate) reader_generation: u64,
    pub(crate) inventory_revision: u64,
    pub(crate) instance_id: String,
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
    route: String,
    fingerprint: String,
    binding: Binding,
    phase: String,
    response: StoredResponse,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u16,
    records: BTreeMap<String, Record>,
}

#[derive(Clone)]
struct Store {
    archive: ArchiveStore,
}

enum Admission {
    Dispatch,
    Replay(StoredResponse),
}

fn configured_error(code: &str) -> StoreError {
    StoreError::Configuration(code.to_owned())
}

pub(crate) fn valid_client_request_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|index| bytes[*index] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
}

fn valid_binding(binding: &Binding) -> bool {
    !binding.reader.trim().is_empty()
        && binding.reader.trim() == binding.reader
        && binding.reader.len() <= 512
        && !binding.reader.chars().any(char::is_control)
        && binding.inventory_revision > 0
        && !binding.instance_id.trim().is_empty()
        && binding.instance_id.trim() == binding.instance_id
        && binding.instance_id.len() <= 128
        && !binding.instance_id.chars().any(char::is_control)
}

fn decode(items: &[Value]) -> Result<Ledger, StoreError> {
    if items.is_empty() {
        return Ok(Ledger {
            version: VERSION,
            records: BTreeMap::new(),
        });
    }
    if items.len() != 1 {
        return Err(configured_error("invalid_reader_mutation_store"));
    }
    let ledger: Ledger = serde_json::from_value(items[0].clone()).map_err(StoreError::Json)?;
    if ledger.version != VERSION
        || ledger.records.len() > MAX_RECORDS
        || ledger.records.iter().any(|(id, record)| {
            id != &record.client_request_id
                || !valid_client_request_id(id)
                || record.route.is_empty()
                || record.fingerprint.len() != 64
                || !record
                    .fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || !valid_binding(&record.binding)
                || !matches!(record.phase.as_str(), "submitted" | "unknown" | "terminal")
                || (record.phase == "terminal") != (record.response.status != 503)
                || !(100..600).contains(&record.response.status)
        })
    {
        return Err(configured_error("invalid_reader_mutation_store"));
    }
    Ok(ledger)
}

fn encode(items: &mut Vec<Value>, ledger: &Ledger) -> Result<(), StoreError> {
    *items = vec![serde_json::to_value(ledger).map_err(StoreError::Json)?];
    Ok(())
}

fn fingerprint(route: &str, body: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(route.as_bytes());
    digest.update([0]);
    digest.update(body);
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn unknown_response(id: &str, binding: &Binding) -> StoredResponse {
    StoredResponse {
        status: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
        body: json!({
            "code":"reader_mutation_outcome_unknown",
            "error":"原读卡器操作已提交但结果未确认；已禁止按同一请求再次作用于设备",
            "retryable":false,
            "mutation_receipt":{
                "client_request_id":id,
                "outcome":"unknown",
                "binding":binding,
            }
        }),
    }
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
                if record.phase == "submitted" {
                    record.phase = "unknown".into();
                    record.response = unknown_response(&record.client_request_id, &record.binding);
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

    fn admit(
        &self,
        id: &str,
        route: &str,
        fingerprint: &str,
        binding: &Binding,
    ) -> Result<Admission, StoreError> {
        if !valid_client_request_id(id) || route.is_empty() || !valid_binding(binding) {
            return Err(configured_error("invalid_reader_mutation_request"));
        }
        self.archive.mutate_if_changed(|items| {
            let mut ledger = decode(items)?;
            if let Some(record) = ledger.records.get(id) {
                if record.route != route
                    || record.fingerprint != fingerprint
                    || &record.binding != binding
                {
                    return Err(configured_error("reader_mutation_request_id_conflict"));
                }
                return Ok((Admission::Replay(record.response.clone()), false));
            }
            if ledger.records.len() >= MAX_RECORDS {
                return Err(configured_error("reader_mutation_store_full"));
            }
            let response = unknown_response(id, binding);
            ledger.records.insert(
                id.to_owned(),
                Record {
                    client_request_id: id.to_owned(),
                    route: route.to_owned(),
                    fingerprint: fingerprint.to_owned(),
                    binding: binding.clone(),
                    phase: "submitted".into(),
                    response,
                },
            );
            encode(items, &ledger)?;
            Ok((Admission::Dispatch, true))
        })
    }

    fn complete(
        &self,
        id: &str,
        route: &str,
        fingerprint: &str,
        binding: &Binding,
        response: StoredResponse,
    ) -> Result<StoredResponse, StoreError> {
        self.archive.mutate(|items| {
            let mut ledger = decode(items)?;
            let record = ledger
                .records
                .get_mut(id)
                .ok_or_else(|| configured_error("invalid_reader_mutation_store"))?;
            if record.route != route
                || record.fingerprint != fingerprint
                || &record.binding != binding
                || !matches!(record.phase.as_str(), "submitted" | "unknown")
            {
                return Err(configured_error("invalid_reader_mutation_store"));
            }
            record.phase = "terminal".into();
            record.response = response.clone();
            encode(items, &ledger)?;
            Ok(response)
        })
    }
}

fn response(stored: StoredResponse) -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::from_u16(stored.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
        &stored.body,
    )
}

fn error(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &json!({"code":code,"error":code,"retryable":false}))
}

pub(crate) fn initialize(root: &Path) -> Result<(), StoreError> {
    Store::open(root).map(|_| ())
}

pub(crate) async fn execute<F, Fut, V>(
    root: &Path,
    route: &str,
    id: &str,
    raw_body: &[u8],
    binding: Binding,
    validate_current: V,
    dispatch: F,
) -> Response<Full<Bytes>>
where
    V: FnOnce() -> Result<(), Response<Full<Bytes>>>,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Response<Full<Bytes>>>,
{
    if !valid_client_request_id(id) {
        return error(StatusCode::BAD_REQUEST, "client_request_id_required");
    }
    let store = match Store::open(root) {
        Ok(store) => store,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_mutation_store_unavailable",
            );
        }
    };
    let fingerprint = fingerprint(route, raw_body);
    // Exact old requests replay before consulting current inventory. This is
    // what prevents a lost disconnect reply from targeting successor B.
    let existing = store.archive.load().and_then(|items| {
        let ledger = decode(&items)?;
        Ok(ledger.records.get(id).cloned())
    });
    match existing {
        Ok(Some(record)) => {
            if record.route != route
                || record.fingerprint != fingerprint
                || record.binding != binding
            {
                return error(StatusCode::CONFLICT, "reader_mutation_request_id_conflict");
            }
            return response(record.response);
        }
        Ok(None) => {}
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_mutation_store_unavailable",
            );
        }
    }
    if let Err(rejection) = validate_current() {
        return rejection;
    }
    match store.admit(id, route, &fingerprint, &binding) {
        Ok(Admission::Replay(stored)) => return response(stored),
        Ok(Admission::Dispatch) => {}
        Err(StoreError::Configuration(code)) if code == "reader_mutation_request_id_conflict" => {
            return error(StatusCode::CONFLICT, &code);
        }
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_mutation_store_unavailable",
            );
        }
    }

    let dispatched = dispatch().await;
    let status = dispatched.status();
    let bytes = match dispatched.into_body().collect().await {
        Ok(body) => body.to_bytes(),
        Err(_) => return response(unknown_response(id, &binding)),
    };
    if status.is_server_error() {
        return response(unknown_response(id, &binding));
    }
    let mut body: Value = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(_) => return response(unknown_response(id, &binding)),
    };
    if !body.is_object() {
        body = json!({"result":body});
    }
    body["mutation_receipt"] = json!({
        "client_request_id":id,
        "outcome":"completed",
        "binding":binding,
    });
    let terminal = StoredResponse {
        status: status.as_u16(),
        body,
    };
    match store.complete(id, route, &fingerprint, &binding, terminal) {
        Ok(stored) => response(stored),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_mutation_store_unavailable",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn root(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "switch-reader-intent-{name}-{}-{}",
            std::process::id(),
            crate::unix_now_ms()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn binding(reader: &str, generation: u64) -> Binding {
        Binding {
            reader: reader.into(),
            reader_generation: generation,
            inventory_revision: 7,
            instance_id: "daemon-instance".into(),
        }
    }

    const ID: &str = "123e4567-e89b-42d3-a456-426614174000";

    #[tokio::test]
    async fn terminal_replay_precedes_successor_validation_and_conflict_is_rejected() {
        let root = root("replay");
        let calls = AtomicUsize::new(0);
        let first = execute(
            &root,
            "/api/reader/disconnect",
            ID,
            br#"{"request":"a"}"#,
            binding("Reader A", 4),
            || Ok(()),
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                crate::json_response(StatusCode::OK, &json!({"ok":true}))
            },
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let replay = execute(
            &root,
            "/api/reader/disconnect",
            ID,
            br#"{"request":"a"}"#,
            binding("Reader A", 4),
            || panic!("successor B must not be consulted for an exact replay"),
            || async { panic!("exact replay must not disconnect successor B") },
        )
        .await;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let conflict = execute(
            &root,
            "/api/reader/disconnect",
            ID,
            br#"{"request":"changed"}"#,
            binding("Reader B", 5),
            || Ok(()),
            || async { panic!("conflict dispatched") },
        )
        .await;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restart_turns_submitted_into_unknown_and_presence_damage_fails_closed() {
        let root = root("restart");
        let store = Store::open(&root).unwrap();
        let fingerprint = fingerprint("/api/reader/connect", b"body");
        assert!(matches!(
            store
                .admit(
                    ID,
                    "/api/reader/connect",
                    &fingerprint,
                    &binding("Reader A", 1)
                )
                .unwrap(),
            Admission::Dispatch
        ));
        drop(store);
        let reopened = Store::open(&root).unwrap();
        assert!(matches!(
            reopened
                .admit(
                    ID,
                    "/api/reader/connect",
                    &fingerprint,
                    &binding("Reader A", 1)
                )
                .unwrap(),
            Admission::Replay(StoredResponse { status: 503, .. })
        ));

        let marker = root.join(format!(".{FILE}.presence-v1"));
        std::fs::remove_file(marker).unwrap();
        assert!(Store::open(&root).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ids_and_bindings_are_strict() {
        assert!(valid_client_request_id(ID));
        for value in ["", "short", "123e4567-e89b-42d3-a456-42661417400z"] {
            assert!(!valid_client_request_id(value));
        }
        assert!(!valid_binding(&Binding {
            instance_id: String::new(),
            ..binding("Reader", 0)
        }));
    }
}
