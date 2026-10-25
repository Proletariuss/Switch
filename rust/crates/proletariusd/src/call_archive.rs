use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use proletarius_store::{ArchiveStore, StoreError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024;
pub(crate) const INTERNAL_PREFIX: &str = "/api/internal/call-archive/";

pub(crate) async fn handle(
    request: Request<Incoming>,
    store: ArchiveStore,
) -> Response<Full<Bytes>> {
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/calls") => match crate::archive_io::run(move || list(&store)).await {
            Ok(items) => crate::json_response(StatusCode::OK, &items),
            Err(error) => store_error(error),
        },
        (&Method::POST, "/api/calls/clear") => clear_public(request, store).await,
        (&Method::POST, "/api/calls/delete") => delete(request, store).await,
        _ => crate::json_response(
            StatusCode::NOT_FOUND,
            &json!({
                "error": "Rust call archive route not found",
                "code": "route_not_found",
                "retryable": false
            }),
        ),
    }
}

async fn clear_public(request: Request<Incoming>, store: ArchiveStore) -> Response<Full<Bytes>> {
    let raw = match collect_limited(request, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => return body_error_response(error),
    };
    let (request_id, stripped) = match crate::public_mutation_receipt::split(&raw, true) {
        Ok((Some(id), body)) => (id, body),
        Ok((None, _)) => unreachable!("required mutation identity"),
        Err(response) => return response,
    };
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Empty {}
    if serde_json::from_slice::<Empty>(&stripped).is_err() {
        return crate::public_mutation_receipt::error(
            StatusCode::BAD_REQUEST,
            "invalid_request_body",
        );
    }
    let root = store
        .path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    match crate::public_mutation_receipt::replay_async(&root, "/api/calls/clear", &request_id, &raw)
        .await
    {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return response,
    }
    let snapshot = crate::archive_io::run({
        let store = store.clone();
        move || list(&store)
    })
    .await;
    let plan = match snapshot {
        Ok(items) => {
            let ids = match frozen_call_ids(&items) {
                Ok(ids) => ids,
                Err(response) => {
                    return crate::public_mutation_receipt::run(
                        &root,
                        "/api/calls/clear",
                        &request_id,
                        &raw,
                        format!(
                            "call-archive-preflight-v1:{}",
                            proletarius_sms::stable_digest_hex(&stripped)
                        ),
                        move || async move { response },
                    )
                    .await;
                }
            };
            let encoded = serde_json::to_vec(&ids).expect("call ID snapshot");
            Ok((
                ids,
                format!(
                    "call-archive-v1:{}",
                    proletarius_sms::stable_digest_hex(&encoded)
                ),
            ))
        }
        Err(error) => Err(store_error(error)),
    };
    let binding = plan
        .as_ref()
        .map(|(_, binding)| binding.clone())
        .unwrap_or_else(|_| {
            format!(
                "call-archive-preflight-v1:{}",
                proletarius_sms::stable_digest_hex(&stripped)
            )
        });
    crate::public_mutation_receipt::run(
        &root,
        "/api/calls/clear",
        &request_id,
        &raw,
        binding,
        move || async move {
            let (ids, _) = match plan {
                Ok(plan) => plan,
                Err(response) => return response,
            };
            match crate::archive_io::run(move || delete_ids(&store, &ids)).await {
                Ok(deleted) => crate::json_response(
                    StatusCode::OK,
                    &json!({"ok":true,"deleted_count":deleted}),
                ),
                Err(error) => store_error(error),
            }
        },
    )
    .await
}

pub(crate) async fn handle_internal(
    request: Request<Incoming>,
    store: ArchiveStore,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST
        || request.uri().path() != "/api/internal/call-archive/ingest"
    {
        return crate::json_response(
            StatusCode::NOT_FOUND,
            &json!({"error":"通话归档路由不存在","code":"route_not_found","retryable":false}),
        );
    }
    let bytes = match collect_limited(request, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => return body_error_response(error),
    };
    let record: Value = match serde_json::from_slice(&bytes) {
        Ok(record) if valid_call_record(&record) => record,
        _ => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"通话记录格式错误","code":"invalid_request_body","retryable":false}),
            );
        }
    };
    match crate::archive_io::run(move || ingest(&store, record)).await {
        Ok(IngestResult::Added) => {
            crate::json_response(StatusCode::OK, &json!({"ok":true,"already_recorded":false}))
        }
        Ok(IngestResult::AlreadyRecorded) => {
            crate::json_response(StatusCode::OK, &json!({"ok":true,"already_recorded":true}))
        }
        Ok(IngestResult::Conflict) => crate::json_response(
            StatusCode::CONFLICT,
            &json!({"error":"通话记录ID已对应其他内容","code":"call_event_conflict","retryable":false}),
        ),
        Err(error) => store_error(error),
    }
}

async fn delete(request: Request<Incoming>, store: ArchiveStore) -> Response<Full<Bytes>> {
    let bytes = match collect_limited(request, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => return body_error_response(error),
    };
    let body: DeleteRequest = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(_) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({
                    "error": "请求格式错误",
                    "code": "invalid_request_body",
                    "retryable": false
                }),
            );
        }
    };
    let id = body.id.trim();
    if id.is_empty() {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({
                "error": "id is required",
                "code": "invalid_request_body",
                "retryable": false
            }),
        );
    }
    let id = id.to_owned();
    match crate::archive_io::run(move || delete_id(&store, &id).map(|_| ())).await {
        Ok(()) => crate::json_response(StatusCode::OK, &json!({"ok": true})),
        Err(error) => store_error(error),
    }
}

async fn collect_limited(
    request: Request<Incoming>,
    limit: usize,
) -> Result<Bytes, BodyCollectError> {
    crate::http_server::collect_body(request.into_body(), limit)
        .await
        .map_err(|error| match error {
            crate::http_server::BodyReadError::Timeout => BodyCollectError::Timeout,
            crate::http_server::BodyReadError::TooLarge => BodyCollectError::TooLarge,
            crate::http_server::BodyReadError::Invalid => BodyCollectError::Stream,
        })
}

enum BodyCollectError {
    Timeout,
    TooLarge,
    Stream,
}

fn body_error_response(error: BodyCollectError) -> Response<Full<Bytes>> {
    match error {
        BodyCollectError::Timeout => crate::http_server::body_timeout_response(),
        BodyCollectError::TooLarge => request_too_large(),
        BodyCollectError::Stream => crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({
                "error": "读取请求失败: invalid request body",
                "code": "invalid_request_body",
                "retryable": false
            }),
        ),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IngestResult {
    Added,
    AlreadyRecorded,
    Conflict,
}

pub(crate) fn ingest(store: &ArchiveStore, record: Value) -> Result<IngestResult, StoreError> {
    let result = store.mutate_if_changed(|items| {
        let id = record.get("id").and_then(Value::as_str).unwrap_or_default();
        if let Some(existing) = items
            .iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
        {
            return Ok((
                if existing == &record {
                    IngestResult::AlreadyRecorded
                } else {
                    IngestResult::Conflict
                },
                false,
            ));
        }
        items.insert(0, record);
        Ok((IngestResult::Added, true))
    })?;
    if result == IngestResult::AlreadyRecorded {
        // A previous atomic rename may have succeeded even if its directory
        // fsync response was lost. A byte-identical retry confirms the entry
        // before the daemon acknowledges it as durable.
        store.confirm_directory_sync()?;
    }
    Ok(result)
}

fn valid_call_record(record: &Value) -> bool {
    let Some(record) = record.as_object() else {
        return false;
    };
    let bounded_string = |key: &str, maximum: usize| {
        record
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty() && value.len() <= maximum)
    };
    bounded_string("id", 256)
        && bounded_string("started_at", 64)
        && bounded_string("ended_at", 64)
        && record
            .get("direction")
            .and_then(Value::as_str)
            .is_some_and(|value| matches!(value, "in" | "out"))
        && record
            .get("device")
            .and_then(Value::as_str)
            .is_some_and(|value| matches!(value, "module" | "reader"))
        && record.get("answered").and_then(Value::as_bool).is_some()
        && record.get("duration").and_then(Value::as_i64).is_some()
        && record
            .get("number")
            .is_none_or(|value| value.as_str().is_some_and(|text| text.len() <= 512))
}

fn list(store: &ArchiveStore) -> Result<Vec<Value>, StoreError> {
    store.load()
}

fn clear(store: &ArchiveStore) -> Result<(), StoreError> {
    store.replace(Vec::new())
}

fn frozen_call_ids(items: &[Value]) -> Result<Vec<String>, Response<Full<Bytes>>> {
    let mut seen = std::collections::HashSet::new();
    let mut ids = Vec::with_capacity(items.len());
    for item in items {
        let Some(id) = item
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty() && id.len() <= 256)
        else {
            return Err(crate::public_mutation_receipt::error(
                StatusCode::CONFLICT,
                "call_archive_snapshot_invalid",
            ));
        };
        if seen.insert(id.to_owned()) {
            ids.push(id.to_owned());
        }
    }
    Ok(ids)
}

fn delete_ids(store: &ArchiveStore, ids: &[String]) -> Result<usize, StoreError> {
    let selected: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    store.mutate(|items| {
        let before = items.len();
        items.retain(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .is_none_or(|id| !selected.contains(id))
        });
        Ok(before - items.len())
    })
}

fn delete_id(store: &ArchiveStore, id: &str) -> Result<usize, StoreError> {
    store.mutate(|items| {
        let before = items.len();
        items.retain(|item| item.get("id").and_then(Value::as_str) != Some(id));
        Ok(before - items.len())
    })
}

fn request_too_large() -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::PAYLOAD_TOO_LARGE,
        &json!({
            "error": "请求体过大",
            "code": "request_body_too_large",
            "retryable": false
        }),
    )
}

fn store_error(error: StoreError) -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        &json!({
            "error": format!("通话记录存储失败: {error}"),
            "code": "call_archive_failed",
            "retryable": false
        }),
    )
}

#[derive(serde::Deserialize)]
struct DeleteRequest {
    id: String,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use proletarius_store::ArchiveStore;
    use serde_json::{Value, json};

    use super::{
        IngestResult, clear, delete_id, delete_ids, frozen_call_ids, ingest, list,
        valid_call_record,
    };

    static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let sequence = DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "proletarius-call-archive-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn store(&self) -> ArchiveStore {
            ArchiveStore::new(self.0.join("call-log.json"))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn record(id: &str, number: &str) -> Value {
        json!({
            "id": id,
            "number": number,
            "direction": "out",
            "device": "reader",
            "future_field": {"preserve": true}
        })
    }

    fn valid_record(id: &str, number: &str) -> Value {
        json!({
            "id": id,
            "device": "reader",
            "direction": "out",
            "number": number,
            "answered": true,
            "started_at": "2026-10-04T12:00:00Z",
            "ended_at": "2026-10-04T12:01:00Z",
            "duration": 60,
            "future_field": {"preserve": true}
        })
    }

    #[test]
    fn list_preserves_order_and_unknown_fields() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let records = vec![record("new", "888"), record("old", "+441234")];
        store.replace(records.clone()).expect("seed archive");

        assert_eq!(list(&store).expect("list archive"), records);
    }

    #[test]
    fn delete_removes_only_exact_id_and_reports_count() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let retained = record("call-20", "+441234");
        store
            .replace(vec![record("call-2", "888"), retained.clone()])
            .expect("seed archive");

        assert_eq!(delete_id(&store, "call-2").expect("delete"), 1);
        assert_eq!(delete_id(&store, "missing").expect("delete missing"), 0);
        assert_eq!(list(&store).expect("reload archive"), vec![retained]);
    }

    #[test]
    fn clear_commits_an_empty_archive() {
        let directory = TestDirectory::new();
        let store = directory.store();
        store
            .replace(vec![record("call-1", "888")])
            .expect("seed archive");

        clear(&store).expect("clear archive");
        assert!(list(&store).expect("reload archive").is_empty());
    }

    #[test]
    fn frozen_clear_snapshot_never_removes_a_later_call() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let first = record("call-1", "888");
        let late = record("call-2", "999");
        store.replace(vec![first.clone()]).unwrap();
        let ids = frozen_call_ids(&list(&store).unwrap()).unwrap();
        store.replace(vec![first, late.clone()]).unwrap();
        assert_eq!(delete_ids(&store, &ids).unwrap(), 1);
        assert_eq!(list(&store).unwrap(), vec![late]);
        assert_eq!(delete_ids(&store, &ids).unwrap(), 0);
    }

    #[test]
    fn durable_ingest_is_idempotent_and_rejects_id_reuse_with_changed_content() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let incoming = valid_record("call-1", "+441234");

        assert!(valid_call_record(&incoming));
        assert_eq!(
            ingest(&store, incoming.clone()).unwrap(),
            IngestResult::Added
        );
        assert_eq!(
            ingest(&store, incoming.clone()).unwrap(),
            IngestResult::AlreadyRecorded
        );
        assert_eq!(
            ingest(&store, valid_record("call-1", "+449999")).unwrap(),
            IngestResult::Conflict
        );
        assert_eq!(list(&store).unwrap(), vec![incoming]);
    }

    #[test]
    fn ingest_validates_required_call_fields_and_preserves_unknown_fields() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let mut invalid = valid_record("call-2", "");
        invalid["direction"] = json!("sideways");
        assert!(!valid_call_record(&invalid));

        let incoming = valid_record("call-3", "");
        assert!(valid_call_record(&incoming));
        ingest(&store, incoming.clone()).unwrap();
        assert_eq!(list(&store).unwrap(), vec![incoming]);
    }

    #[test]
    fn concurrent_call_events_keep_distinct_ids_and_newest_first_order() {
        let directory = TestDirectory::new();
        let store = directory.store();
        std::thread::scope(|scope| {
            for index in 0..40 {
                let store = store.clone();
                scope.spawn(move || {
                    let id = format!("call-{index}");
                    ingest(&store, valid_record(&id, "")).expect("append call event");
                });
            }
        });
        let records = list(&store).expect("load concurrent calls");
        assert_eq!(records.len(), 40);
        let ids: std::collections::HashSet<_> = records
            .iter()
            .filter_map(|record| record.get("id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids.len(), 40);
    }
}
