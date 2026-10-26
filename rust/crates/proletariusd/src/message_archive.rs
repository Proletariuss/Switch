use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use proletarius_sms::stable_digest_hex;
use proletarius_store::{ArchiveStore, StoreError};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};

const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024;
const MAX_INTERNAL_INGEST_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_VOLATILE_INBOX_MESSAGES: usize = 500;
pub(crate) const INTERNAL_PREFIX: &str = "/api/internal/sms/archive/";
/// Volatile native SMS sources are kept separate from the durable archive.
#[derive(Clone, Default)]
pub(crate) struct InboxState {
    inner: Arc<Mutex<InboxPartitions>>,
}

#[derive(Default)]
struct InboxPartitions {
    usb: Vec<Value>,
    expected_go_producer: Option<String>,
    native_module_producer: Option<String>,
}

impl InboxState {
    pub(crate) fn for_go_producer(producer_id: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(InboxPartitions {
                expected_go_producer: Some(producer_id),
                ..InboxPartitions::default()
            })),
        }
    }

    #[cfg(test)]
    pub(crate) fn clear_module_producers_for_tests(&self) {
        let mut current = self.inner.lock().unwrap();
        current.expected_go_producer = None;
        current.native_module_producer = None;
    }

    /// Public reads use durable archive rows plus the native USB preview.
    pub(crate) fn snapshot(&self) -> Result<Vec<Value>, InboxSnapshotError> {
        self.usb_snapshot()
    }

    /// Retire the old child generation so late compatibility callbacks fail.
    /// A late private request from the old process cannot confirm the new one.
    pub(crate) fn replace_go_producer(
        &self,
        producer_id: String,
    ) -> Result<(), InboxSnapshotError> {
        if producer_id.is_empty() || producer_id.len() > 128 {
            return Err(InboxSnapshotError::Invalid);
        }
        let mut current = self
            .inner
            .lock()
            .map_err(|_| InboxSnapshotError::Unavailable)?;
        if current.native_module_producer.as_ref() == Some(&producer_id) {
            return Err(InboxSnapshotError::Invalid);
        }
        current.expected_go_producer = Some(producer_id);
        Ok(())
    }

    /// Native forwarding reads only Rust-owned sources. Go's compatibility
    /// inbox is a duplicate of archive-first ingestion and must not be a
    /// runtime prerequisite for the Rust producer.
    pub(crate) fn forwarding_owned_snapshot(&self) -> Result<Vec<Value>, InboxSnapshotError> {
        let current = self
            .inner
            .lock()
            .map_err(|_| InboxSnapshotError::Unavailable)?;
        let mut messages = merge_sms(Vec::new(), current.usb.clone());
        messages.truncate(MAX_VOLATILE_INBOX_MESSAGES);
        Ok(messages)
    }

    /// Verify that a callback belongs to the current compatibility generation.
    pub(crate) fn owns_current_producer(&self, producer_id: &str) -> bool {
        self.inner
            .lock()
            .is_ok_and(|current| current.expected_go_producer.as_deref() == Some(producer_id))
    }

    pub(crate) fn current_go_producer_id(&self) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|current| current.expected_go_producer.clone())
    }

    /// Bind the Rust physical module owner before publishing native USB selection.
    /// This identity is immutable and is never a compatibility caller credential.
    pub(crate) fn bind_native_module_producer(
        &self,
        producer_id: String,
    ) -> Result<(), InboxSnapshotError> {
        if producer_id.is_empty() || producer_id.len() > 128 {
            return Err(InboxSnapshotError::Invalid);
        }
        let mut current = self
            .inner
            .lock()
            .map_err(|_| InboxSnapshotError::Unavailable)?;
        if current.expected_go_producer.as_ref() == Some(&producer_id)
            || current
                .native_module_producer
                .as_ref()
                .is_some_and(|id| id != &producer_id)
        {
            return Err(InboxSnapshotError::Invalid);
        }
        current.native_module_producer = Some(producer_id);
        Ok(())
    }

    /// Native module/call-mode work follows the physical owner. Serial-only
    /// compatibility retains its original child producer when no native owner exists.
    pub(crate) fn current_module_producer_id(&self) -> Option<String> {
        self.inner.lock().ok().and_then(|current| {
            current
                .native_module_producer
                .clone()
                .or_else(|| current.expected_go_producer.clone())
        })
    }

    pub(crate) fn owns_module_producer(&self, producer_id: &str) -> bool {
        self.inner.lock().is_ok_and(|current| {
            current
                .native_module_producer
                .as_ref()
                .or(current.expected_go_producer.as_ref())
                .is_some_and(|id| id == producer_id)
        })
    }

    /// Private Go ingress must never gain access to a bound native module owner.
    pub(crate) fn compatibility_module_allowed(&self) -> bool {
        self.inner
            .lock()
            .is_ok_and(|current| current.native_module_producer.is_none())
    }

    pub(crate) fn clear_usb(&self) -> Result<(), InboxSnapshotError> {
        self.inner
            .lock()
            .map_err(|_| InboxSnapshotError::Unavailable)?
            .usb
            .clear();
        Ok(())
    }

    pub(crate) fn usb_snapshot(&self) -> Result<Vec<Value>, InboxSnapshotError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| InboxSnapshotError::Unavailable)?
            .usb
            .clone())
    }

    /// Keep previously seen device messages, matching the legacy bounded cache.
    /// Repeated evidence keeps its first metadata. A different source token or
    /// observed sender/time replaces a reused legacy index, while absent
    /// observations retain history without retaining deletion authority.
    pub(crate) fn merge_usb(&self, messages: Vec<Value>) -> Result<usize, InboxSnapshotError> {
        if messages.iter().any(|item| !valid_sms_record(item)) {
            return Err(InboxSnapshotError::Invalid);
        }
        let mut current = self
            .inner
            .lock()
            .map_err(|_| InboxSnapshotError::Unavailable)?;
        let fresh: std::collections::HashMap<_, _> = messages
            .iter()
            .map(|item| (cache_digest(item), item))
            .collect();
        let mut previous = current.usb.clone();
        for item in &mut previous {
            if let Some(new) = fresh.get(&cache_digest(item)) {
                let changed_token = item.get("deletion_token").is_some()
                    && new.get("deletion_token").is_some()
                    && item.get("deletion_token") != new.get("deletion_token");
                let changed_source = [
                    "sender",
                    "timestamp",
                    "device",
                    "module_storage",
                    "module_index",
                    "subscription_id",
                ]
                .iter()
                .any(|key| item.get(*key) != new.get(*key));
                if changed_token || changed_source {
                    *item = (*new).clone();
                } else if let Some(token) = new.get("deletion_token") {
                    item["deletion_token"] = token.clone();
                } else if let Some(object) = item.as_object_mut() {
                    object.remove("deletion_token");
                }
            } else if let Some(object) = item.as_object_mut() {
                object.remove("deletion_token");
            }
        }
        let mut merged = merge_sms(previous, messages);
        merged.truncate(MAX_VOLATILE_INBOX_MESSAGES);
        let count = merged.len();
        current.usb = merged;
        Ok(count)
    }

    /// Prune exactly the IDs selected before a physical deletion. In particular,
    /// a concurrent arrival from the same sender is not removed by a stale clear.
    pub(crate) fn prune_usb(&self, ids: &[String]) -> Result<usize, InboxSnapshotError> {
        let selected: std::collections::HashSet<_> = ids.iter().collect();
        let mut current = self
            .inner
            .lock()
            .map_err(|_| InboxSnapshotError::Unavailable)?;
        let before = current.usb.len();
        current
            .usb
            .retain(|item| !selected.contains(&cache_digest(item)));
        Ok(before - current.usb.len())
    }

    pub(crate) fn clear_usb_deletion_tokens(&self) -> Result<(), InboxSnapshotError> {
        let mut current = self
            .inner
            .lock()
            .map_err(|_| InboxSnapshotError::Unavailable)?;
        for item in &mut current.usb {
            if let Some(object) = item.as_object_mut() {
                object.remove("deletion_token");
            }
        }
        Ok(())
    }
}

pub(crate) async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/v2/sms-archive") => {
            match crate::archive_io::run(|| archive().and_then(|store| list(&store))).await {
                Ok(items) => crate::json_response(StatusCode::OK, &items),
                Err(error) => store_error(error),
            }
        }
        (&Method::POST, "/api/v2/sms-archive/clear") => clear_archive_public(request).await,
        (&Method::POST, "/api/v2/sms-archive/delete") => {
            let body: DeleteRequest = match decode_request(request).await {
                Ok(body) => body,
                Err(error) => return error.into_response(),
            };
            let id = body.id.trim();
            if id.is_empty() {
                return invalid_request("id is required");
            }
            let id = id.to_owned();
            match crate::archive_io::run(move || archive().and_then(|store| delete_id(&store, &id)))
                .await
            {
                Ok(deleted) => crate::json_response(
                    StatusCode::OK,
                    &json!({"deleted": deleted > 0, "deleted_count": deleted}),
                ),
                Err(error) => store_error(error),
            }
        }
        (&Method::POST, "/api/v2/sms-archive/delete-sender") => {
            delete_sender_archive_public(request).await
        }
        _ => crate::json_response(
            StatusCode::NOT_FOUND,
            &json!({
                "error": "Rust SMS archive route not found",
                "code": "route_not_found",
                "retryable": false
            }),
        ),
    }
}

async fn clear_archive_public(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let raw = match collect_request_limited(request, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => return error.into_response(),
    };
    let (request_id, stripped) = match crate::public_mutation_receipt::split(&raw, true) {
        Ok((Some(id), body)) => (id, body),
        Ok((None, _)) => unreachable!("required mutation identity"),
        Err(response) => return response,
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Empty {}
    if serde_json::from_slice::<Empty>(&stripped).is_err() {
        return invalid_request("请求格式错误");
    }
    let store = match archive() {
        Ok(store) => store,
        Err(error) => return store_error(error),
    };
    let root = store
        .path()
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match crate::public_mutation_receipt::replay_async(
        &root,
        "/api/v2/sms-archive/clear",
        &request_id,
        &raw,
    )
    .await
    {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return response,
    }
    let items = crate::archive_io::run({
        let store = store.clone();
        move || list(&store)
    })
    .await;
    let plan = match items {
        Ok(items) => {
            let ids = frozen_archive_ids(&items);
            let binding = archive_snapshot_binding("sms-archive-clear-v1", &ids);
            Ok((ids, binding))
        }
        Err(error) => Err(store_error(error)),
    };
    let binding = plan
        .as_ref()
        .map(|(_, binding)| binding.clone())
        .unwrap_or_else(|_| {
            archive_snapshot_binding(
                "sms-archive-clear-preflight-v1",
                &[proletarius_sms::stable_digest_hex(&stripped)],
            )
        });
    crate::public_mutation_receipt::run(
        &root,
        "/api/v2/sms-archive/clear",
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
                    &json!({"cleared":true,"deleted_count":deleted}),
                ),
                Err(error) => store_error(error),
            }
        },
    )
    .await
}

async fn delete_sender_archive_public(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let raw = match collect_request_limited(request, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => return error.into_response(),
    };
    let (request_id, stripped) = match crate::public_mutation_receipt::split(&raw, true) {
        Ok((Some(id), body)) => (id, body),
        Ok((None, _)) => unreachable!("required mutation identity"),
        Err(response) => return response,
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PublicDeleteSender {
        sender: String,
    }
    let body: PublicDeleteSender = match serde_json::from_slice(&stripped) {
        Ok(body) => body,
        Err(_) => return invalid_request("请求格式错误"),
    };
    let sender = body.sender.trim().to_owned();
    if sender.is_empty() {
        return invalid_request("sender is required");
    }
    let store = match archive() {
        Ok(store) => store,
        Err(error) => return store_error(error),
    };
    let root = store
        .path()
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match crate::public_mutation_receipt::replay_async(
        &root,
        "/api/v2/sms-archive/delete-sender",
        &request_id,
        &raw,
    )
    .await
    {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return response,
    }
    let items = crate::archive_io::run({
        let store = store.clone();
        move || list(&store)
    })
    .await;
    let plan = match items {
        Ok(items) => {
            let ids = frozen_archive_ids(
                &items
                    .into_iter()
                    .filter(|item| string_field(item, "sender").trim() == sender)
                    .collect::<Vec<_>>(),
            );
            let binding = archive_snapshot_binding("sms-archive-sender-v1", &ids);
            Ok((ids, binding))
        }
        Err(error) => Err(store_error(error)),
    };
    let binding = plan
        .as_ref()
        .map(|(_, binding)| binding.clone())
        .unwrap_or_else(|_| {
            archive_snapshot_binding(
                "sms-archive-sender-preflight-v1",
                &[proletarius_sms::stable_digest_hex(&stripped)],
            )
        });
    crate::public_mutation_receipt::run(
        &root,
        "/api/v2/sms-archive/delete-sender",
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
                    &json!({"deleted":deleted > 0,"deleted_count":deleted}),
                ),
                Err(error) => store_error(error),
            }
        },
    )
    .await
}

fn frozen_archive_ids(items: &[Value]) -> Vec<String> {
    let mut ids: Vec<_> = items.iter().map(stable_id).collect();
    ids.sort();
    ids.dedup();
    ids
}

fn archive_snapshot_binding(prefix: &str, ids: &[String]) -> String {
    let encoded = serde_json::to_vec(ids).expect("archive ID snapshot");
    format!("{prefix}:{}", stable_digest_hex(&encoded))
}

pub(crate) async fn handle_sms_list(
    request: Request<Incoming>,
    inbox: InboxState,
) -> Response<Full<Bytes>> {
    let query = request.uri().query().unwrap_or_default().to_owned();
    let (requested_id, metadata_only, limit) = match parse_sms_query(&query) {
        Ok(parsed) => parsed,
        Err(SmsQueryError::InvalidLimit) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"limit 必须在 1 到 100 之间","code":"invalid_limit","retryable":false}),
            );
        }
    };
    let volatile = match inbox.snapshot() {
        Ok(messages) => messages,
        Err(_) => {
            return crate::json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error":"短信列表暂不可用","code":"sms_inbox_unavailable","retryable":true}),
            );
        }
    };
    match crate::archive_io::run(move || {
        archive().and_then(|store| list_sms(&store, volatile, requested_id, metadata_only, limit))
    })
    .await
    {
        Ok((status, value)) => crate::json_response(status, &value),
        Err(error) => store_error(error),
    }
}

/// Count the exact merged archive/volatile view used by GET /api/sms without
/// exposing message bodies to the status endpoint.
pub(crate) async fn public_count(inbox: InboxState) -> Result<usize, ()> {
    let volatile = inbox.snapshot().map_err(|_| ())?;
    crate::archive_io::run(move || {
        let store = archive()?;
        merged_count(&store, volatile)
    })
    .await
    .map_err(|_| ())
}

/// Return the same merged snapshot exposed by public GET /api/sms. Mutations
/// use its frozen IDs so a concurrent arrival is not erased by a bulk clear.
pub(crate) async fn public_items_from(
    store: ArchiveStore,
    inbox: InboxState,
) -> Result<Vec<Value>, StoreError> {
    let volatile = inbox
        .snapshot()
        .map_err(|_| StoreError::Configuration("sms_inbox_unavailable".into()))?;
    crate::archive_io::run(move || merged_sms(&store, volatile)).await
}

pub(crate) async fn delete_ids_from_store(
    store: ArchiveStore,
    ids: Vec<String>,
) -> Result<usize, StoreError> {
    crate::archive_io::run(move || delete_ids(&store, &ids)).await
}

/// Private archive-ingest/delete bridge for the Go hardware compatibility
/// process. Rust remains the only production writer of durable SMS records;
/// this endpoint does not publish or replace a volatile inbox snapshot.
pub(crate) async fn handle_internal(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let path = request.uri().path();
    match (request.method(), path) {
        (&Method::POST, "/api/internal/sms/archive/ingest") => {
            let body: IngestRequest =
                match decode_request_limited(request, MAX_INTERNAL_INGEST_BODY_BYTES).await {
                    Ok(body) => body,
                    Err(error) => return error.into_response(),
                };
            if body.messages.is_empty() || body.messages.len() > 200 {
                return invalid_request("messages must contain 1 to 200 records");
            }
            if body.messages.iter().any(|item| !valid_sms_record(item)) {
                return invalid_request(
                    "each message requires string sender, content, and timestamp",
                );
            }
            match crate::archive_io::run(move || {
                archive().and_then(|store| ingest(&store, body.messages))
            })
            .await
            {
                Ok(added) => crate::json_response(StatusCode::OK, &json!({"added": added})),
                Err(error) => store_error(error),
            }
        }
        (&Method::POST, "/api/internal/sms/archive/delete") => {
            let body: DeleteRequest = match decode_request(request).await {
                Ok(body) => body,
                Err(error) => return error.into_response(),
            };
            let id = body.id.trim().to_owned();
            if id.is_empty() {
                return invalid_request("id is required");
            }
            match crate::archive_io::run(move || archive().and_then(|store| delete_id(&store, &id)))
                .await
            {
                Ok(deleted) => {
                    crate::json_response(StatusCode::OK, &json!({"deleted_count": deleted}))
                }
                Err(error) => store_error(error),
            }
        }
        (&Method::POST, "/api/internal/sms/archive/delete-ids") => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct DeleteIds {
                ids: Vec<String>,
            }
            let body: DeleteIds = match decode_request_limited(request, 128 * 1024).await {
                Ok(body) => body,
                Err(error) => return error.into_response(),
            };
            if body.ids.len() > 1500
                || body.ids.iter().any(|id| {
                    id.len() != 64
                        || !id
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            {
                return invalid_request(
                    "ids must contain at most 1500 lowercase SHA-256 identifiers",
                );
            }
            match crate::archive_io::run(move || {
                archive().and_then(|store| delete_ids(&store, &body.ids))
            })
            .await
            {
                Ok(deleted) => {
                    crate::json_response(StatusCode::OK, &json!({"deleted_count":deleted}))
                }
                Err(error) => store_error(error),
            }
        }
        (&Method::POST, "/api/internal/sms/archive/delete-sender") => {
            let body: DeleteSenderRequest = match decode_request(request).await {
                Ok(body) => body,
                Err(error) => return error.into_response(),
            };
            let sender = body.sender.trim().to_owned();
            if sender.is_empty() {
                return invalid_request("sender is required");
            }
            match crate::archive_io::run(move || {
                archive().and_then(|store| delete_sender(&store, &sender))
            })
            .await
            {
                Ok(deleted) => {
                    crate::json_response(StatusCode::OK, &json!({"deleted_count": deleted}))
                }
                Err(error) => store_error(error),
            }
        }
        (&Method::POST, "/api/internal/sms/archive/clear") => {
            match crate::archive_io::run(|| archive().and_then(|store| store.replace(Vec::new())))
                .await
            {
                Ok(()) => crate::json_response(StatusCode::OK, &json!({"cleared": true})),
                Err(error) => store_error(error),
            }
        }
        _ => crate::json_response(
            StatusCode::NOT_FOUND,
            &json!({"error":"Rust internal SMS archive route not found","code":"route_not_found","retryable":false}),
        ),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InboxSnapshotError {
    Invalid,
    Unavailable,
}

/// The full list uses exactly the GET /api/sms archive + volatile projection.
/// The caller must revalidate the returned producer after the blocking read.
/// Source list for the Rust-owned device-SMS forwarding producer. Module and
/// reader messages enter the durable archive before acknowledgement; the
/// native USB SMS owner also exposes its bounded no-adopt preview here.
/// Neither source depends on a Go child snapshot.
pub(crate) async fn forwarding_owned_list(inbox: &InboxState) -> Result<Vec<Value>, &'static str> {
    let store = archive().map_err(|_| "device_sms_archive_unavailable")?;
    forwarding_owned_list_from_store(inbox, store).await
}

async fn forwarding_owned_list_from_store(
    inbox: &InboxState,
    store: ArchiveStore,
) -> Result<Vec<Value>, &'static str> {
    let native = inbox
        .forwarding_owned_snapshot()
        .map_err(|_| "device_sms_source_unavailable")?;
    crate::archive_io::run(move || merged_sms(&store, native).map(ready_for_forwarding))
        .await
        .map_err(|_| "device_sms_archive_unavailable")
}

#[cfg(test)]
pub(crate) async fn forwarding_owned_list_from_store_for_test(
    inbox: &InboxState,
    store: ArchiveStore,
) -> Result<Vec<Value>, &'static str> {
    forwarding_owned_list_from_store(inbox, store).await
}

fn ready_for_forwarding(items: Vec<Value>) -> Vec<Value> {
    items
        .into_iter()
        .filter(|item| {
            // Only the canonical assembler removes concat. A volatile source
            // cannot make a raw segment forwardable by omitting/forging status.
            item.get("concat").is_none()
                && !matches!(
                    item.get("multipart_status").and_then(Value::as_str),
                    Some("partial" | "ambiguous")
                )
        })
        .collect()
}

fn ingest(store: &ArchiveStore, incoming: Vec<Value>) -> Result<usize, StoreError> {
    ingest_guarded(store, incoming, || Ok(true))
}

fn ingest_guarded(
    store: &ArchiveStore,
    incoming: Vec<Value>,
    needed: impl FnOnce() -> Result<bool, StoreError>,
) -> Result<usize, StoreError> {
    let added = store.mutate_if_changed(|items| {
        if !needed()? {
            return Ok((0, false));
        }
        let mut identities: std::collections::HashSet<String> =
            items.iter().map(cache_digest).collect();
        let mut incoming_identities = std::collections::HashSet::new();
        let mut added = 0;
        for mut item in incoming {
            if item.get("concat").is_some() && multipart_key(&item).is_none() {
                return Err(StoreError::Configuration(
                    "incoming SMS concatenation metadata is invalid".into(),
                ));
            }
            let id = cache_digest(&item);
            if item.get("concat").is_some()
                && items.iter().any(|saved| {
                    saved
                        .get("multipart_segment_ids")
                        .and_then(Value::as_array)
                        .is_some_and(|receipts| receipts.iter().any(|receipt| receipt == &id))
                })
            {
                continue;
            }
            let collision =
                multipart_key(&item).and_then(|key| multipart_completed_collision(items, &key));
            let potential_legacy = multipart_key(&item)
                .and_then(|key| multipart_potential_legacy_collision(items, &key));
            // Go's archive recomputes an identity from message fields even
            // when a compatibility record already carries a legacy ID.
            if !identities.insert(id.clone()) {
                continue;
            }
            incoming_identities.insert(id.clone());
            let object = item.as_object_mut().ok_or(StoreError::ItemIsNotObject)?;
            object.remove("deletion_token");
            object.insert("id".into(), Value::String(id));
            object.insert("archived".into(), Value::Bool(true));
            if let Some((group_id, anchor)) = collision {
                quarantine_multipart_part(&mut item, &group_id, &anchor);
            } else if let Some(fence) = potential_legacy {
                quarantine_potential_legacy_part(&mut item, &fence);
            }
            items.push(item);
            added += 1;
        }

        // A legacy completed record may reveal its anchor only when a later
        // fragment matches its group hash. That new quarantine also supplies
        // the verified context for earlier retained, nonmatching fragments.
        let collisions: Vec<_> = items
            .iter()
            .map(|item| {
                multipart_key(item).and_then(|key| multipart_completed_collision(items, &key))
            })
            .collect();
        for (item, collision) in items.iter_mut().zip(collisions) {
            if let Some((group_id, anchor)) = collision {
                quarantine_multipart_part(item, &group_id, &anchor);
            }
        }
        let potential_legacy: Vec<_> = items
            .iter()
            .map(|item| {
                multipart_key(item)
                    .and_then(|key| multipart_potential_legacy_collision(items, &key))
            })
            .collect();
        for (item, fence) in items.iter_mut().zip(potential_legacy) {
            if item.get("multipart_conflict_group_id").is_none()
                && let Some(fence) = fence
            {
                quarantine_potential_legacy_part(item, &fence);
            }
        }

        // Reassembly and storage of the finished public message share this
        // archive transaction. The caller may ACK the SIP delivery only after
        // this transaction has durably recorded either a pending segment or
        // the completed message.
        let projected = project_multipart(items.clone());
        let completed: Vec<_> = projected
            .iter()
            .filter(|item| item["multipart_status"] == "complete")
            .filter(|item| {
                !items
                    .iter()
                    .any(|saved| saved.get("multipart_group_id") == item.get("multipart_group_id"))
            })
            .cloned()
            .collect();
        for mut message in completed {
            message["multipart_group_id"]
                .as_str()
                .expect("completed multipart message has a group id");
            let message_id = cache_digest(&message);
            if !identities.insert(message_id.clone()) {
                continue;
            }
            incoming_identities.insert(message_id.clone());
            let object = message.as_object_mut().ok_or(StoreError::ItemIsNotObject)?;
            object.remove("deletion_token");
            object.insert("id".into(), Value::String(message_id));
            object.insert("archived".into(), Value::Bool(true));
            // Keep only the canonical assembled record for a completed group.
            // Its stable group id also makes a later source retry idempotent.
            let raw_part_ids: std::collections::HashSet<String> = message
                .get("multipart_segment_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            for raw_part_id in &raw_part_ids {
                incoming_identities.remove(raw_part_id);
            }
            items.retain(|saved| !raw_part_ids.contains(&cache_digest(saved)));
            items.push(message);
            added += 1;
        }

        if added == 0 {
            return Ok((0, false));
        }
        items.sort_by_key(|item| std::cmp::Reverse(timestamp_order(item)));
        if items.len() > 1_000 {
            let pending_source_ids: std::collections::HashSet<String> =
                project_multipart(items.clone())
                    .into_iter()
                    .filter(|item| {
                        matches!(
                            item.get("multipart_status").and_then(Value::as_str),
                            Some("partial" | "ambiguous")
                        )
                    })
                    .map(|item| stable_id(&item))
                    .collect();
            let pending_count = items
                .iter()
                .filter(|item| pending_source_ids.contains(&stable_id(item)))
                .count();
            if pending_count > 1_000 {
                return Err(StoreError::Configuration(
                    "archive capacity would discard pending SMS segments".into(),
                ));
            }
            let all_items = std::mem::take(items);
            let mut pending: Vec<_> = all_items
                .iter()
                .filter(|item| pending_source_ids.contains(&stable_id(item)))
                .cloned()
                .collect();
            let remaining = 1_000 - pending.len();
            let mut recent: Vec<_> = all_items
                .into_iter()
                .filter(|item| !pending_source_ids.contains(&stable_id(item)))
                .take(remaining)
                .collect();
            pending.append(&mut recent);
            *items = pending;
            items.sort_by_key(|item| std::cmp::Reverse(timestamp_order(item)));
        }
        if incoming_identities
            .iter()
            .any(|id| !items.iter().any(|item| stable_id(item) == *id))
        {
            return Err(StoreError::Configuration(
                "archive capacity would discard an incoming SMS".into(),
            ));
        }
        Ok((added, true))
    })?;
    if added == 0 {
        // A previous request may have renamed the file successfully but
        // reported a directory-fsync error. An idempotent retry confirms that
        // the existing name is durable before acknowledging the source.
        store.confirm_directory_sync()?;
    }
    Ok(added)
}

/// Persist a module SMS snapshot through the same locked, atomic archive path
/// used by the internal compatibility API. Device records are acknowledged
/// for deletion only after this future returns successfully.
pub(crate) async fn ingest_messages_to(
    store: ArchiveStore,
    incoming: Vec<Value>,
) -> Result<usize, StoreError> {
    crate::archive_io::run(move || ingest(&store, incoming)).await
}

/// An accepted device-SMS owner survives a timed/cancelled archive observer.
/// The real archive executor retains the owner through captures and IoAck.
pub(crate) fn ingest_messages_to_retained<G: Send + 'static>(
    store: ArchiveStore,
    incoming: Vec<Value>,
    retained: G,
) -> impl std::future::Future<Output = Result<usize, StoreError>> + Send + 'static {
    let job = crate::archive_io::run_durable(
        move || retained_ingest_result(ingest(&store, incoming)),
        retained,
    );
    async move { job.await? }
}

/// Archive-only recovery checks the immutable accepted attempt under the same
/// archive lock as deletion. A deleted/confirmed receipt suppresses insertion.
pub(crate) fn reconcile_device_send_retained<G: Send + 'static>(
    store: ArchiveStore,
    ledger: crate::device_send_store::Store,
    item: Value,
    attempt: String,
    retained: G,
) -> impl std::future::Future<Output = Result<usize, StoreError>> + Send + 'static {
    let job = crate::archive_io::run_durable(
        move || {
            let proof = item.clone();
            let result = ingest_guarded(&store, vec![item], || {
                ledger.archive_receipt(&proof, Some(&attempt), false)
            })
            .and_then(|added| {
                ledger.archive_receipt(&proof, Some(&attempt), true)?;
                Ok(added)
            });
            retained_ingest_result(result)
        },
        retained,
    );
    async move { job.await? }
}

/// Persist the historical receipt before removing its only archive evidence.
/// If this fails, deletion fails closed. The lock order is archive -> ledger;
/// send/finish never hold the ledger lock while acquiring the archive lock.
fn confirm_deleted_device_sends<'a>(
    store: &ArchiveStore,
    items: impl Iterator<Item = &'a Value>,
) -> Result<(), StoreError> {
    for item in items {
        if item["direction"] == "out"
            && item["send_request_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        {
            let root = store
                .path()
                .parent()
                .ok_or_else(|| StoreError::Configuration("archive root missing".into()))?;
            match crate::device_send_store::Store::open(root)?.archive_receipt(item, None, true) {
                Ok(_) => {}
                // Imported/non-issued and retired identities cannot authorize
                // archive repair. Deleting them needs no new ledger record.
                Err(StoreError::Configuration(code))
                    if matches!(
                        code.as_str(),
                        "send_intent_not_found"
                            | "mutation_identity_retired"
                            | "mutation_identity_not_issued"
                            | "mutation_authority_mismatch"
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn retained_ingest_result(
    result: Result<usize, StoreError>,
) -> Result<Result<usize, StoreError>, StoreError> {
    match result {
        Ok(added) => Ok(Ok(added)),
        // Both exact refusals occur inside mutate_if_changed before commit.
        // Malformed input, unknown configuration, and actual I/O stay failures.
        Err(StoreError::Configuration(code))
            if matches!(
                code.as_str(),
                "archive capacity would discard pending SMS segments"
                    | "archive capacity would discard an incoming SMS"
            ) =>
        {
            Ok(Err(StoreError::Configuration(code)))
        }
        Err(error) => Err(error),
    }
}

fn timestamp_order(item: &Value) -> time::OffsetDateTime {
    item.get("timestamp")
        .and_then(Value::as_str)
        .and_then(|timestamp| {
            time::OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

fn valid_sms_record(item: &Value) -> bool {
    item.is_object()
        && item.get("sender").is_some_and(Value::is_string)
        && item.get("content").is_some_and(Value::is_string)
        && item
            .get("timestamp")
            .and_then(Value::as_str)
            .is_some_and(|timestamp| {
                time::OffsetDateTime::parse(
                    timestamp,
                    &time::format_description::well_known::Rfc3339,
                )
                .is_ok()
            })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SmsQueryError {
    InvalidLimit,
}

fn parse_sms_query(query: &str) -> Result<(Option<String>, bool, usize), SmsQueryError> {
    let pairs: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).collect();
    let value = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_ref())
    };
    let requested_id = value("id")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    // Go resolves a non-empty id before parsing metadata/limit parameters.
    if requested_id.is_some() {
        return Ok((requested_id, false, usize::MAX));
    }
    let metadata_only = value("view").is_some_and(|view| view.eq_ignore_ascii_case("metadata"));
    let limit = if let Some(raw) = value("limit") {
        match raw.trim().parse::<usize>() {
            Ok(limit) if (1..=100).contains(&limit) => limit,
            _ => return Err(SmsQueryError::InvalidLimit),
        }
    } else {
        usize::MAX
    };
    Ok((requested_id, metadata_only, limit))
}

fn list_sms(
    store: &ArchiveStore,
    volatile: Vec<Value>,
    requested_id: Option<String>,
    metadata_only: bool,
    limit: usize,
) -> Result<(StatusCode, Value), StoreError> {
    let items = merged_sms(store, volatile)?;
    if let Some(id) = requested_id {
        if let Some(item) = items.iter().find(|item| string_field(item, "id") == id) {
            return Ok((StatusCode::OK, item.clone()));
        }
        return Ok((
            StatusCode::NOT_FOUND,
            json!({"error":"未找到指定短信","code":"sms_not_found","retryable":false}),
        ));
    }
    let items: Vec<_> = items.into_iter().take(limit).collect();
    if metadata_only {
        let metadata: Vec<_> = items
            .iter()
            .map(|item| {
                json!({
                    "id": string_field(item, "id"),
                    "sender": string_field(item, "sender"),
                    "timestamp": item.get("timestamp").cloned().unwrap_or(Value::Null),
                    "direction": string_field(item, "direction"),
                    "device": string_field(item, "device"),
                    "has_content": !string_field(item, "content").is_empty(),
                })
            })
            .collect();
        Ok((StatusCode::OK, json!(metadata)))
    } else {
        Ok((StatusCode::OK, json!(items)))
    }
}

fn merged_sms(store: &ArchiveStore, volatile: Vec<Value>) -> Result<Vec<Value>, StoreError> {
    Ok(merge_sms(list(store)?, volatile))
}

fn merged_count(store: &ArchiveStore, volatile: Vec<Value>) -> Result<usize, StoreError> {
    Ok(merged_sms(store, volatile)?.len())
}

fn merge_sms(archived: Vec<Value>, volatile: Vec<Value>) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::with_capacity(archived.len() + volatile.len());
    for mut item in archived.into_iter().chain(volatile) {
        let id = cache_digest(&item);
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(object) = item.as_object_mut() {
            object.insert("id".into(), Value::String(id));
        }
        items.push(item);
    }
    items.sort_by_key(|item| std::cmp::Reverse(timestamp_order(item)));
    items
}

fn list(store: &ArchiveStore) -> Result<Vec<Value>, StoreError> {
    let mut items = project_multipart(store.load()?);
    for item in &mut items {
        let id = stable_id(item);
        if let Some(object) = item.as_object_mut() {
            object.insert("id".into(), Value::String(id));
            object.insert("archived".into(), Value::Bool(true));
        }
    }
    Ok(items)
}

fn delete_id(store: &ArchiveStore, id: &str) -> Result<usize, StoreError> {
    store.mutate(|items| {
        let before = items.len();
        let projected = project_multipart(items.clone());
        let group_id = projected
            .iter()
            .into_iter()
            .find(|item| stable_id(item) == id || cache_digest(item) == id)
            .and_then(|item| {
                item.get("multipart_group_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        let group_source_ids = multipart_source_ids(&projected, group_id.as_deref());
        // GET /api/sms intentionally recomputes the Go-compatible ID even for
        // older archive rows that still carry a legacy stored ID.
        confirm_deleted_device_sends(
            store,
            items.iter().filter(|item| {
                stable_id(item) == id
                    || cache_digest(item) == id
                    || group_source_ids.contains(&stable_id(item))
                    || group_source_ids.contains(&cache_digest(item))
            }),
        )?;
        items.retain(|item| {
            stable_id(item) != id
                && cache_digest(item) != id
                && !group_source_ids.contains(&stable_id(item))
                && !group_source_ids.contains(&cache_digest(item))
        });
        Ok(before - items.len())
    })
}

fn delete_ids(store: &ArchiveStore, ids: &[String]) -> Result<usize, StoreError> {
    let selected: std::collections::HashSet<_> = ids.iter().collect();
    store.mutate(|items| {
        let before = items.len();
        let projected = project_multipart(items.clone());
        let group_ids: std::collections::HashSet<String> = projected
            .into_iter()
            .filter(|item| {
                selected.contains(&stable_id(item)) || selected.contains(&cache_digest(item))
            })
            .filter_map(|item| {
                item.get("multipart_group_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        let projected = project_multipart(items.clone());
        let group_source_ids = multipart_source_ids_for_groups(&projected, &group_ids);
        confirm_deleted_device_sends(
            store,
            items.iter().filter(|item| {
                selected.contains(&stable_id(item))
                    || selected.contains(&cache_digest(item))
                    || group_source_ids.contains(&stable_id(item))
                    || group_source_ids.contains(&cache_digest(item))
            }),
        )?;
        items.retain(|item| {
            !selected.contains(&stable_id(item))
                && !selected.contains(&cache_digest(item))
                && !group_source_ids.contains(&stable_id(item))
                && !group_source_ids.contains(&cache_digest(item))
        });
        Ok(before - items.len())
    })
}

fn multipart_source_ids(
    projected: &[Value],
    group_id: Option<&str>,
) -> std::collections::HashSet<String> {
    let Some(group_id) = group_id else {
        return std::collections::HashSet::new();
    };
    let groups = std::collections::HashSet::from([group_id.to_owned()]);
    multipart_source_ids_for_groups(projected, &groups)
}

fn multipart_source_ids_for_groups(
    projected: &[Value],
    group_ids: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    for item in projected.iter().filter(|item| {
        item.get("multipart_group_id")
            .and_then(Value::as_str)
            .is_some_and(|group_id| group_ids.contains(group_id))
    }) {
        ids.insert(stable_id(item));
        ids.insert(cache_digest(item));
        if let Some(receipts) = item.get("multipart_segment_ids").and_then(Value::as_array) {
            ids.extend(receipts.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    ids
}

fn delete_sender(store: &ArchiveStore, sender: &str) -> Result<usize, StoreError> {
    store.mutate(|items| {
        let before = items.len();
        confirm_deleted_device_sends(
            store,
            items
                .iter()
                .filter(|item| string_field(item, "sender").trim() == sender),
        )?;
        items.retain(|item| string_field(item, "sender").trim() != sender);
        Ok(before - items.len())
    })
}

fn stable_id(item: &Value) -> String {
    if let Some(id) = item
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return id.to_owned();
    }
    cache_digest(item)
}

pub(crate) fn cache_digest(item: &Value) -> String {
    stable_digest_hex(cache_key(item).as_bytes())
}

fn cache_key(item: &Value) -> String {
    let module_index = item
        .get("module_index")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let mut key = if module_index > 0 {
        format!(
            "{}\0{}\0{}\0{}",
            string_field(item, "device"),
            string_field(item, "module_storage").to_uppercase(),
            module_index,
            string_field(item, "content")
        )
    } else {
        format!(
            "{}\0{}\0{}\0{}",
            string_field(item, "device"),
            string_field(item, "sender"),
            string_field(item, "content"),
            string_field(item, "timestamp")
        )
    };
    let subscription = string_field(item, "subscription_id");
    if subscription.len() == 69
        && subscription.starts_with("sub1_")
        && subscription[5..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        key = format!("subscription-v1\0{subscription}\0{key}");
    }
    let request = string_field(item, "send_request_id");
    if string_field(item, "direction") == "out"
        && !request.is_empty()
        && request.len() <= 128
        && request
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        key = format!("send-request-v1\0{request}\0{key}");
    }
    if let Some(group_id) = item.get("multipart_group_id").and_then(Value::as_str) {
        key = format!("multipart-message-v1\0{group_id}\0{key}");
    } else if let Some(concat) = item.get("concat") {
        key = format!(
            "concat-part-v1\0{}\0{}\0{}\0{}\0{key}",
            concat
                .get("reference")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            concat
                .get("reference_bits")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            concat
                .get("total")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            concat
                .get("sequence")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
        );
    }
    key
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MultipartKey {
    device: String,
    subscription: String,
    sender: String,
    timestamp: String,
    reference: u16,
    reference_bits: u8,
    total: u8,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MultipartBaseKey {
    device: String,
    subscription: String,
    sender: String,
    reference: u16,
    reference_bits: u8,
    total: u8,
}

fn multipart_key(item: &Value) -> Option<MultipartKey> {
    let concat = item.get("concat")?;
    let reference = u16::try_from(concat.get("reference")?.as_u64()?).ok()?;
    let reference_bits = u8::try_from(concat.get("reference_bits")?.as_u64()?).ok()?;
    let total = u8::try_from(concat.get("total")?.as_u64()?).ok()?;
    let sequence = u8::try_from(concat.get("sequence")?.as_u64()?).ok()?;
    if !matches!(reference_bits, 8 | 16)
        || (reference_bits == 8 && reference > u8::MAX.into())
        || total < 2
        || sequence == 0
        || sequence > total
    {
        return None;
    }
    Some(MultipartKey {
        device: string_field(item, "device").to_owned(),
        subscription: string_field(item, "subscription_id").to_owned(),
        sender: string_field(item, "sender").to_owned(),
        timestamp: string_field(item, "timestamp").to_owned(),
        reference,
        reference_bits,
        total,
    })
}

fn multipart_base_key(key: &MultipartKey) -> MultipartBaseKey {
    MultipartBaseKey {
        device: key.device.clone(),
        subscription: key.subscription.clone(),
        sender: key.sender.clone(),
        reference: key.reference,
        reference_bits: key.reference_bits,
        total: key.total,
    }
}

fn multipart_cluster_id(key: &MultipartBaseKey, anchor_timestamp: &str) -> String {
    let identity = format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}",
        key.device,
        key.subscription,
        key.sender,
        anchor_timestamp,
        key.reference,
        key.reference_bits,
        key.total,
    );
    stable_digest_hex(identity.as_bytes())
}

fn timestamp_millis(timestamp: &str) -> Option<i128> {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    OffsetDateTime::parse(timestamp, &Rfc3339)
        .ok()
        .map(|value| value.unix_timestamp_nanos().div_euclid(1_000_000))
}

const MULTIPART_CORRELATION_WINDOW_MS: i128 = 120_000;

#[derive(Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PotentialLegacyFence {
    legacy_id: String,
    visible_timestamp: String,
    total: u8,
}

/// Old canonical rows did not retain their cluster anchor or reference. The
/// visible timestamp V belongs to sequence 1 and may be A..A+120s; a later
/// collision within A +/-120s can therefore be anywhere in V[-240s,+120s].
/// This is potential overlap, not proof of the same original concatenation.
/// Retained raw observations carry the unchanged fence through pruning; they
/// must not move V forward and indefinitely widen the quarantine window.
fn multipart_potential_legacy_collision(
    items: &[Value],
    key: &MultipartKey,
) -> Option<PotentialLegacyFence> {
    let timestamp = timestamp_millis(&key.timestamp)?;
    for saved in items {
        if string_field(saved, "device") != key.device
            || string_field(saved, "subscription_id") != key.subscription
            || string_field(saved, "sender") != key.sender
        {
            continue;
        }
        let fence = if let Some(value) = saved.get("multipart_potential_legacy_fence") {
            let Ok(fence) = serde_json::from_value::<PotentialLegacyFence>(value.clone()) else {
                continue;
            };
            if multipart_key(saved).is_none_or(|part| {
                part.total != fence.total || multipart_base_key(&part) != multipart_base_key(key)
            }) {
                continue;
            }
            fence
        } else {
            if saved["multipart_status"] != "complete"
                || saved.get("concat").is_some()
                || saved.get("multipart_anchor_timestamp").is_some()
                || saved
                    .get("multipart_group_id")
                    .and_then(Value::as_str)
                    .is_none()
            {
                continue;
            }
            let Some(receipts) = saved.get("multipart_segment_ids").and_then(Value::as_array)
            else {
                continue;
            };
            let distinct: std::collections::HashSet<_> =
                receipts.iter().filter_map(Value::as_str).collect();
            if receipts.len() != usize::from(key.total)
                || distinct.len() != receipts.len()
                || distinct.iter().any(|id| !is_multipart_digest(id))
            {
                continue;
            }
            PotentialLegacyFence {
                legacy_id: cache_digest(saved),
                visible_timestamp: string_field(saved, "timestamp").to_owned(),
                total: key.total,
            }
        };
        if fence.total != key.total || !is_multipart_digest(&fence.legacy_id) {
            continue;
        }
        let Some(visible) = timestamp_millis(&fence.visible_timestamp) else {
            continue;
        };
        let delta = timestamp - visible;
        if (-2 * MULTIPART_CORRELATION_WINDOW_MS..=MULTIPART_CORRELATION_WINDOW_MS).contains(&delta)
        {
            return Some(fence);
        }
    }
    None
}

fn is_multipart_digest(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn quarantine_potential_legacy_part(item: &mut Value, fence: &PotentialLegacyFence) {
    item["multipart_status"] = json!("ambiguous");
    item["multipart_potential_legacy_fence"] =
        serde_json::to_value(fence).expect("bounded legacy fence");
}

/// The group digest binds the base key and exact cluster anchor. Keeping the
/// anchor lets a later fragment prove its base-key match even when its own
/// SCTS differs. These internal fields are deliberately outside cache_key, so
/// canonical and raw-fragment notification/replay identities remain stable.
fn multipart_completed_collision(items: &[Value], key: &MultipartKey) -> Option<(String, String)> {
    let base = multipart_base_key(key);
    for saved in items {
        let group_id = if saved["multipart_status"] == "complete" {
            saved.get("multipart_group_id").and_then(Value::as_str)
        } else {
            saved
                .get("multipart_conflict_group_id")
                .and_then(Value::as_str)
        };
        let Some(group_id) = group_id else { continue };
        // Old canonical rows have no retained anchor. Their visible SCTS, or
        // an incoming exact anchor, can still prove a match via the old hash.
        // If neither matches, keep the fragment visible rather than guessing.
        for anchor in [
            saved
                .get("multipart_anchor_timestamp")
                .and_then(Value::as_str),
            saved.get("timestamp").and_then(Value::as_str),
            Some(key.timestamp.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            let in_window = match (timestamp_millis(&key.timestamp), timestamp_millis(anchor)) {
                (Some(timestamp), Some(anchor)) => {
                    timestamp.abs_diff(anchor) <= MULTIPART_CORRELATION_WINDOW_MS as u128
                }
                _ => key.timestamp == anchor,
            };
            if in_window && multipart_cluster_id(&base, anchor) == group_id {
                return Some((group_id.to_owned(), anchor.to_owned()));
            }
        }
    }
    None
}

fn quarantine_multipart_part(item: &mut Value, group_id: &str, anchor: &str) {
    item["multipart_status"] = json!("ambiguous");
    item["multipart_conflict_group_id"] = json!(group_id);
    item["multipart_anchor_timestamp"] = json!(anchor);
}

/// Build the public conversation projection from durable concatenated parts.
/// Same-reference collisions with conflicting duplicate sequence numbers are
/// retained as ambiguous fragments and never guessed into a user message.
fn project_multipart(items: Vec<Value>) -> Vec<Value> {
    let mut groups: std::collections::HashMap<MultipartBaseKey, Vec<Value>> =
        std::collections::HashMap::new();
    let mut projected = Vec::with_capacity(items.len());
    for mut item in items {
        if item.get("multipart_conflict_group_id").is_some()
            || item.get("multipart_potential_legacy_fence").is_some()
        {
            // Never feed acknowledged late conflicts back into reassembly,
            // including after the original canonical row is pruned.
            item["multipart_status"] = json!("ambiguous");
            projected.push(item);
        } else if let Some(key) = multipart_key(&item) {
            groups
                .entry(multipart_base_key(&key))
                .or_default()
                .push(item);
        } else {
            projected.push(item);
        }
    }

    for (key, mut parts) in groups {
        parts.sort_by(|left, right| {
            timestamp_millis(string_field(left, "timestamp"))
                .unwrap_or(i128::MAX)
                .cmp(&timestamp_millis(string_field(right, "timestamp")).unwrap_or(i128::MAX))
                .then_with(|| string_field(left, "timestamp").cmp(string_field(right, "timestamp")))
                .then_with(|| cache_digest(left).cmp(&cache_digest(right)))
        });
        let mut clusters: Vec<Vec<Value>> = Vec::new();
        let mut current_anchor_ms = None;
        let mut current_invalid_timestamp: Option<String> = None;
        for part in parts {
            let timestamp = string_field(&part, "timestamp").to_owned();
            match timestamp_millis(&timestamp) {
                Some(millis) => {
                    if current_anchor_ms.is_some_and(|anchor| {
                        millis.saturating_sub(anchor) <= MULTIPART_CORRELATION_WINDOW_MS
                    }) {
                        clusters.last_mut().expect("active time cluster").push(part);
                    } else {
                        clusters.push(vec![part]);
                        current_anchor_ms = Some(millis);
                        current_invalid_timestamp = None;
                    }
                }
                None if current_anchor_ms.is_none()
                    && current_invalid_timestamp.as_deref() == Some(timestamp.as_str()) =>
                {
                    clusters
                        .last_mut()
                        .expect("active invalid-time cluster")
                        .push(part);
                }
                None => {
                    clusters.push(vec![part]);
                    current_anchor_ms = None;
                    current_invalid_timestamp = Some(timestamp);
                }
            }
        }

        for parts in clusters {
            project_multipart_cluster(&key, parts, &mut projected);
        }
    }
    projected
}

fn project_multipart_cluster(
    key: &MultipartBaseKey,
    parts: Vec<Value>,
    projected: &mut Vec<Value>,
) {
    let mut by_sequence: std::collections::BTreeMap<u8, Vec<&Value>> =
        std::collections::BTreeMap::new();
    for part in &parts {
        if let Some(sequence) = part["concat"]["sequence"]
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
        {
            by_sequence.entry(sequence).or_default().push(part);
        }
    }
    let conflicting = by_sequence
        .values()
        .any(|same_sequence| same_sequence.len() > 1);
    let complete = !conflicting
        && (1..=key.total).all(|sequence| {
            by_sequence
                .get(&sequence)
                .is_some_and(|parts| parts.len() == 1)
        });
    let anchor_timestamp = parts
        .first()
        .map(|part| string_field(part, "timestamp"))
        .unwrap_or_default();
    let group_id = multipart_cluster_id(key, anchor_timestamp);
    if complete {
        let mut message = by_sequence[&1][0].clone();
        let content = (1..=key.total)
            .map(|sequence| {
                by_sequence[&sequence][0]["content"]
                    .as_str()
                    .unwrap_or_default()
            })
            .collect::<String>();
        message["content"] = Value::String(content);
        if let Some(object) = message.as_object_mut() {
            object.remove("concat");
        }
        message["multipart_segment_ids"] = Value::Array(
            (1..=key.total)
                .map(|sequence| Value::String(cache_digest(by_sequence[&sequence][0])))
                .collect(),
        );
        message["multipart_status"] = Value::String("complete".into());
        message["multipart_group_id"] = Value::String(group_id);
        message["multipart_anchor_timestamp"] = json!(anchor_timestamp);
        projected.push(message);
    } else {
        for mut part in parts {
            part["multipart_status"] =
                Value::String(if conflicting { "ambiguous" } else { "partial" }.into());
            part["multipart_group_id"] = Value::String(group_id.clone());
            projected.push(part);
        }
    }
}

fn string_field<'a>(item: &'a Value, field: &str) -> &'a str {
    item.get(field).and_then(Value::as_str).unwrap_or_default()
}

async fn decode_request<T: DeserializeOwned>(
    request: Request<Incoming>,
) -> Result<T, RequestDecodeError> {
    decode_request_limited(request, MAX_REQUEST_BODY_BYTES).await
}

async fn decode_request_limited<T: DeserializeOwned>(
    request: Request<Incoming>,
    max_body_bytes: usize,
) -> Result<T, RequestDecodeError> {
    let collected = collect_request_limited(request, max_body_bytes).await?;
    serde_json::from_slice(&collected)
        .map_err(|_| RequestDecodeError::Invalid("请求格式错误".into()))
}

async fn collect_request_limited(
    request: Request<Incoming>,
    max_body_bytes: usize,
) -> Result<Bytes, RequestDecodeError> {
    crate::http_server::collect_body(request.into_body(), max_body_bytes)
        .await
        .map_err(|error| match error {
            crate::http_server::BodyReadError::Timeout => RequestDecodeError::Timeout,
            crate::http_server::BodyReadError::TooLarge => RequestDecodeError::TooLarge,
            crate::http_server::BodyReadError::Invalid => {
                RequestDecodeError::Invalid("读取请求失败".into())
            }
        })
}

enum RequestDecodeError {
    Timeout,
    Invalid(String),
    TooLarge,
}

impl RequestDecodeError {
    fn into_response(self) -> Response<Full<Bytes>> {
        match self {
            Self::Timeout => crate::http_server::body_timeout_response(),
            Self::Invalid(message) => invalid_request(&message),
            Self::TooLarge => request_too_large(),
        }
    }
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

fn invalid_request(message: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::BAD_REQUEST,
        &json!({
            "error": message,
            "code": "invalid_request_body",
            "retryable": false
        }),
    )
}

pub(crate) fn archive() -> Result<ArchiveStore, StoreError> {
    Ok(ArchiveStore::new(
        crate::data_root::resolve()
            .map_err(StoreError::Configuration)?
            .join("sms-archive.json"),
    ))
}

pub(crate) fn store_error(error: StoreError) -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        &json!({
            "error": format!("短信记录存储失败: {error}"),
            "code": "sms_archive_failed",
            "retryable": false
        }),
    )
}

#[derive(Deserialize)]
struct DeleteRequest {
    id: String,
    #[serde(flatten)]
    _future: Map<String, Value>,
}

#[derive(Deserialize)]
struct IngestRequest {
    messages: Vec<Value>,
}

#[derive(Deserialize)]
struct DeleteSenderRequest {
    sender: String,
    #[serde(flatten)]
    _future: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use proletarius_store::ArchiveStore;
    use serde_json::{Value, json};

    use super::{
        cache_digest, delete_id, delete_ids, delete_sender, frozen_archive_ids, ingest, list,
        merged_count, ready_for_forwarding, stable_id,
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
                "proletarius-message-archive-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn store(&self) -> ArchiveStore {
            ArchiveStore::new(self.0.join("sms-archive.json"))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(sender: &str, content: &str, timestamp: &str) -> Value {
        json!({
            "sender": sender,
            "content": content,
            "timestamp": timestamp,
            "direction": "in",
            "device": "reader",
            "archived": true,
            "future_field": {"preserve": true}
        })
    }

    fn concatenated_part(
        subscription: &str,
        sender: &str,
        content: &str,
        sequence: u64,
        timestamp: &str,
    ) -> Value {
        let mut item = fixture(sender, content, timestamp);
        item["subscription_id"] = json!(subscription);
        item["concat"] = json!({
            "reference": 42,
            "reference_bits": 8,
            "total": 2,
            "sequence": sequence,
        });
        item
    }

    #[test]
    fn scoped_id_matches_go_and_keeps_legacy_ids() {
        let mut item = fixture("fixture-peer", "same-body", "2026-09-01T12:00:00Z");
        item["direction"] = json!("out");
        assert_eq!(
            stable_id(&item),
            "cb228a34071f9000d7ad542ce15495fc117b0dcd404646d0ef4f160f9a057146"
        );
        item["subscription_id"] = json!(format!("sub1_{}", "a".repeat(64)));
        assert_eq!(
            stable_id(&item),
            "df60ae92ddbe92a1e59245c3d85a1b11fa21d92db8d679be86492691138b381e"
        );
        item["send_request_id"] = json!("request-1");
        assert_eq!(
            stable_id(&item),
            "a9167d3eb18659c01b3f01ffc7de913d181a46ecf7b96ba163d7042353beb5b5"
        );
        let first = stable_id(&item);
        item["send_request_id"] = json!("request-2");
        assert_ne!(stable_id(&item), first);
    }

    #[test]
    fn list_adds_stable_ids_without_rewriting_unknown_fields() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let item = fixture("888", "BALANCE", "2026-09-01T12:03:52.123456789Z");
        let expected_id = stable_id(&item);
        assert_eq!(
            expected_id,
            "2c513a5bd1137e6b4aaef5c126835b037292828d1bbc42e593f8e5d301e6ea69"
        );
        store.replace(vec![item.clone()]).expect("seed archive");

        let listed = list(&store).expect("list archive");
        assert_eq!(listed[0]["id"], expected_id);
        assert_eq!(listed[0]["future_field"], item["future_field"]);
        assert_eq!(store.load().expect("reload archive")[0], item);
    }

    #[test]
    fn status_count_matches_the_public_archive_and_volatile_projection() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let archived = fixture("peer", "archived", "2026-10-07T10:00:00Z");
        store.replace(vec![archived.clone()]).expect("seed archive");
        let latest = fixture("peer", "volatile", "2026-10-08T10:00:00Z");
        let count =
            merged_count(&store, vec![latest.clone(), archived.clone()]).expect("merged count");
        assert_eq!(count, 2, "status counts one archived and one volatile SMS");
        assert_eq!(
            merged_count(&store, vec![latest, archived]).expect("duplicate count"),
            2,
            "archive/volatile duplicate identities are counted once"
        );
    }

    #[test]
    fn internal_ingest_is_idempotent_and_preserves_go_archive_fields() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let mut first = fixture("sender", "one", "2026-10-04T14:00:00+02:00");
        first["direction"] = json!("in");
        first["deletion_token"] = json!("never-persist-physical-authority");
        let expected_id = stable_id(&first);
        assert_eq!(ingest(&store, vec![first.clone()]).expect("ingest"), 1);
        let inode_before_retry = fs::metadata(store.path()).expect("metadata").ino();
        assert_eq!(ingest(&store, vec![first]).expect("idempotent retry"), 0);
        assert_eq!(
            fs::metadata(store.path())
                .expect("metadata after retry")
                .ino(),
            inode_before_retry,
            "an unchanged poll must not rewrite the archive"
        );

        let items = store.load().expect("load");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], expected_id);
        assert_eq!(items[0]["archived"], true);
        assert_eq!(items[0]["future_field"]["preserve"], true);
        assert!(items[0].get("deletion_token").is_none());
    }

    #[test]
    fn multipart_archive_keeps_same_text_parts_distinct_and_commits_one_message() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let subscription = format!("sub1_{}", "a".repeat(64));
        let first = concatenated_part(&subscription, "888", "same", 1, "2026-10-21T10:00:00Z");
        let second = concatenated_part(&subscription, "888", "same", 2, "2026-10-21T10:00:01Z");
        assert_ne!(cache_digest(&first), cache_digest(&second));
        assert_eq!(ingest(&store, vec![first.clone()]).unwrap(), 1);
        assert_eq!(store.load().unwrap().len(), 1, "the first part is durable");
        let pending = list(&store).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["content"], "same");
        assert_eq!(pending[0]["multipart_status"], "partial");
        assert!(ready_for_forwarding(pending).is_empty());

        ingest(&store, vec![second.clone()]).expect("second part and assembled message commit");
        let stored = store.load().unwrap();
        assert_eq!(
            stored.len(),
            1,
            "raw parts are atomically replaced by one canonical message"
        );
        assert_eq!(stored[0]["content"], "samesame");
        assert_eq!(stored[0]["multipart_status"], "complete");
        assert!(stored[0].get("concat").is_none());
        let public = list(&store).unwrap();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0]["content"], "samesame");
        assert_eq!(ready_for_forwarding(public.clone()).len(), 1);

        assert_eq!(ingest(&store, vec![first, second]).unwrap(), 0);
        assert_eq!(
            list(&store).unwrap().len(),
            1,
            "source replay does not duplicate the message"
        );
        let id = public[0]["id"].as_str().unwrap().to_owned();
        assert_eq!(delete_id(&store, &id).unwrap(), 1);
        assert!(list(&store).unwrap().is_empty());
    }

    #[test]
    fn multipart_archive_quarantines_late_conflicts_after_reopen_without_reforwarding() {
        for late_second_timestamp in ["2026-10-21T10:00:00Z", "2026-10-21T10:00:02Z"] {
            let directory = TestDirectory::new();
            let subscription = format!("sub1_{}", "d".repeat(64));
            let first =
                concatenated_part(&subscription, "888", "original-", 1, "2026-10-21T10:00:00Z");
            let second = concatenated_part(&subscription, "888", "tail", 2, "2026-10-21T10:00:01Z");
            let store = directory.store();
            ingest(&store, vec![first.clone(), second.clone()]).unwrap();
            let original = store.load().unwrap().pop().unwrap();
            drop(store);

            let store = directory.store();
            let conflict_first = concatenated_part(
                &subscription,
                "888",
                "conflicting-",
                1,
                "2026-10-21T10:00:00Z",
            );
            let conflict_second =
                concatenated_part(&subscription, "888", "other-tail", 2, late_second_timestamp);
            assert_eq!(
                ingest(&store, vec![conflict_first.clone()]).unwrap(),
                1,
                "a newly acknowledged conflicting fragment must be durable"
            );
            assert_eq!(ingest(&store, vec![conflict_second.clone()]).unwrap(), 1);
            drop(store);

            let store = directory.store();
            let saved = store.load().unwrap();
            assert_eq!(saved.len(), 3);
            assert_eq!(
                saved.iter().find(|item| item["id"] == original["id"]),
                Some(&original),
                "late conflicts must not change the completed message or its identity"
            );
            for fragment in [&conflict_first, &conflict_second] {
                let retained = saved
                    .iter()
                    .find(|item| item["id"] == cache_digest(fragment))
                    .unwrap();
                assert_eq!(retained["content"], fragment["content"]);
                assert_eq!(retained["concat"], fragment["concat"]);
                assert_eq!(retained["multipart_status"], "ambiguous");
            }
            let projected = list(&store).unwrap();
            assert_eq!(projected.len(), 3);
            assert_eq!(
                ready_for_forwarding(projected),
                vec![original.clone()],
                "late conflicts must not assemble into a second forwarded message"
            );
            let merged = super::merged_sms(
                &store,
                vec![conflict_first.clone(), conflict_second.clone()],
            )
            .unwrap();
            assert_eq!(ready_for_forwarding(merged), vec![original.clone()]);
            let bytes = fs::read(store.path()).unwrap();
            assert_eq!(
                ingest(&store, vec![first, second, conflict_first, conflict_second]).unwrap(),
                0
            );
            assert_eq!(
                fs::read(store.path()).unwrap(),
                bytes,
                "exact original and quarantined fragment replays must not rewrite the archive"
            );

            ingest(
                &store,
                vec![
                    concatenated_part(&subscription, "888", "new-", 1, "2026-10-21T10:03:00Z"),
                    concatenated_part(&subscription, "888", "message", 2, "2026-10-21T10:03:01Z"),
                ],
            )
            .unwrap();
            let forwardable = ready_for_forwarding(list(&store).unwrap());
            assert_eq!(
                forwardable.len(),
                2,
                "reference reuse outside the correlation window remains valid"
            );
            assert!(forwardable.contains(&original));
            assert!(
                forwardable
                    .iter()
                    .any(|item| item["content"] == "new-message")
            );
        }
    }

    #[test]
    fn multipart_archive_legacy_completed_rows_keep_unproven_fragments_until_anchor_is_known() {
        for second_precedes_first in [false, true] {
            let directory = TestDirectory::new();
            let subscription = format!("sub1_{}", "e".repeat(64));
            let first_timestamp = if second_precedes_first {
                "2026-10-21T10:00:01Z"
            } else {
                "2026-10-21T10:00:00Z"
            };
            let second_timestamp = if second_precedes_first {
                "2026-10-21T10:00:00Z"
            } else {
                "2026-10-21T10:00:01Z"
            };
            let store = directory.store();
            ingest(
                &store,
                vec![
                    concatenated_part(&subscription, "888", "original-", 1, first_timestamp),
                    concatenated_part(&subscription, "888", "tail", 2, second_timestamp),
                ],
            )
            .unwrap();
            let mut legacy = store.load().unwrap().pop().unwrap();
            let digest = cache_digest(&legacy);
            legacy
                .as_object_mut()
                .unwrap()
                .remove("multipart_anchor_timestamp");
            assert_eq!(
                cache_digest(&legacy),
                digest,
                "the internal anchor does not participate in canonical identity"
            );
            assert_eq!(legacy["id"], digest);
            store.replace(vec![legacy.clone()]).unwrap();
            drop(store);

            let store = directory.store();
            let late_first =
                concatenated_part(&subscription, "888", "late-first", 1, first_timestamp);
            let late_second =
                concatenated_part(&subscription, "888", "late-second", 2, second_timestamp);
            assert_eq!(ingest(&store, vec![late_first.clone()]).unwrap(), 1);
            let pending = list(&store).unwrap();
            assert_eq!(
                pending.len(),
                2,
                "unproven legacy associations must never discard incoming data"
            );
            assert_eq!(ready_for_forwarding(pending.clone()), vec![legacy.clone()]);
            let fragment = pending
                .iter()
                .find(|item| item["content"] == "late-first")
                .unwrap();
            assert_eq!(fragment["multipart_status"], "ambiguous");
            assert_eq!(ingest(&store, vec![late_second.clone()]).unwrap(), 1);
            drop(store);

            let store = directory.store();
            let retained = store.load().unwrap();
            assert_eq!(retained.len(), 3);
            assert_eq!(
                retained.iter().find(|item| item["id"] == legacy["id"]),
                Some(&legacy)
            );
            assert_eq!(
                retained
                    .iter()
                    .filter(|item| item["multipart_status"] == "ambiguous")
                    .count(),
                2,
                "a proven anchor must also quarantine the earlier retained fragment"
            );
            assert_eq!(ready_for_forwarding(list(&store).unwrap()), vec![legacy]);
            let bytes = fs::read(store.path()).unwrap();
            assert_eq!(ingest(&store, vec![late_first, late_second]).unwrap(), 0);
            assert_eq!(fs::read(store.path()).unwrap(), bytes);
        }
    }

    #[test]
    fn multipart_archive_legacy_hidden_anchor_never_reassembles_shifted_late_conflicts() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let subscription = format!("sub1_{}", "9".repeat(64));
        let original_first =
            concatenated_part(&subscription, "888", "original-", 1, "2026-10-21T10:00:01Z");
        let original_second =
            concatenated_part(&subscription, "888", "tail", 2, "2026-10-21T10:00:00Z");
        ingest(&store, vec![original_first, original_second]).unwrap();
        let mut legacy = store.load().unwrap().pop().unwrap();
        assert_eq!(legacy["timestamp"], "2026-10-21T10:00:01Z");
        assert_eq!(legacy["multipart_anchor_timestamp"], "2026-10-21T10:00:00Z");
        let original_id = cache_digest(&legacy);
        legacy
            .as_object_mut()
            .unwrap()
            .remove("multipart_anchor_timestamp");
        assert_eq!(cache_digest(&legacy), original_id);
        store.replace(vec![legacy.clone()]).unwrap();
        drop(store);

        // Neither incoming SCTS is the hidden original :00 anchor. The
        // canonical row's visible :01 SCTS cannot reproduce its group hash.
        let late_first = concatenated_part(
            &subscription,
            "888",
            "conflicting-",
            1,
            "2026-10-21T10:00:02Z",
        );
        let late_second = concatenated_part(
            &subscription,
            "888",
            "other-tail",
            2,
            "2026-10-21T10:00:03Z",
        );
        let store = directory.store();
        assert_eq!(ingest(&store, vec![late_first.clone()]).unwrap(), 1);
        assert_eq!(
            ready_for_forwarding(list(&store).unwrap()),
            vec![legacy.clone()]
        );
        ingest(&store, vec![late_second.clone()]).unwrap();
        drop(store);

        let store = directory.store();
        let forwardable = ready_for_forwarding(list(&store).unwrap());
        assert_eq!(
            forwardable,
            vec![legacy.clone()],
            "unknown legacy anchor must not permit a shifted conflicting group to forward"
        );
        let retained = store.load().unwrap();
        assert_eq!(
            retained.len(),
            3,
            "retain both conflicting raw observations"
        );
        assert!(
            retained.contains(&legacy),
            "the historical canonical row is immutable"
        );
        for part in [&late_first, &late_second] {
            let saved = retained
                .iter()
                .find(|item| item["id"] == cache_digest(part))
                .unwrap();
            assert_eq!(saved["content"], part["content"]);
            assert_eq!(saved["concat"], part["concat"]);
            assert_eq!(saved["multipart_status"], "ambiguous");
            assert_eq!(
                saved["multipart_potential_legacy_fence"]["legacy_id"],
                original_id
            );
            assert_eq!(
                saved["multipart_potential_legacy_fence"]["visible_timestamp"],
                legacy["timestamp"]
            );
            assert!(saved.get("multipart_anchor_timestamp").is_none());
            assert!(saved.get("multipart_conflict_group_id").is_none());
        }
        let bytes = fs::read(store.path()).unwrap();
        assert_eq!(ingest(&store, vec![late_first, late_second]).unwrap(), 0);
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
    }

    fn seed_hidden_anchor_legacy(store: &ArchiveStore, subscription: &str) -> Value {
        ingest(
            store,
            vec![
                concatenated_part(subscription, "888", "legacy-", 1, "2026-10-21T10:02:00Z"),
                concatenated_part(subscription, "888", "tail", 2, "2026-10-21T10:00:00Z"),
            ],
        )
        .unwrap();
        let mut legacy = store.load().unwrap().pop().unwrap();
        assert_eq!(legacy["multipart_anchor_timestamp"], "2026-10-21T10:00:00Z");
        legacy
            .as_object_mut()
            .unwrap()
            .remove("multipart_anchor_timestamp");
        store.replace(vec![legacy.clone()]).unwrap();
        legacy
    }

    #[test]
    fn potential_legacy_fence_is_bounded_by_visible_time_identity_and_verified_total() {
        for (case, timestamp, quarantined, total) in [
            ("left-edge", "2026-10-21T09:58:00Z", true, 2),
            ("right-edge", "2026-10-21T10:04:00Z", true, 2),
            ("before-window", "2026-10-21T09:57:59Z", false, 2),
            ("after-window", "2026-10-21T10:04:01Z", false, 2),
            ("device", "2026-10-21T10:02:02Z", false, 2),
            ("subscription", "2026-10-21T10:02:02Z", false, 2),
            ("sender", "2026-10-21T10:02:02Z", false, 2),
            ("total", "2026-10-21T10:02:02Z", false, 3),
            ("different-reference", "2026-10-21T10:02:02Z", true, 2),
            ("unverified-total", "2026-10-21T10:02:02Z", false, 2),
        ] {
            let directory = TestDirectory::new();
            let store = directory.store();
            let subscription = format!("sub1_{}", "7".repeat(64));
            let mut legacy = seed_hidden_anchor_legacy(&store, &subscription);
            if case == "unverified-total" {
                legacy["multipart_segment_ids"][1] = legacy["multipart_segment_ids"][0].clone();
                store.replace(vec![legacy.clone()]).unwrap();
            }
            let parts: Vec<_> = (1..=total)
                .map(|sequence| {
                    let mut part = concatenated_part(
                        &subscription,
                        "888",
                        &format!("new-{sequence}"),
                        sequence,
                        timestamp,
                    );
                    part["concat"]["total"] = json!(total);
                    match case {
                        "device" => part["device"] = json!("module"),
                        "subscription" => {
                            part["subscription_id"] = json!(format!("sub1_{}", "8".repeat(64)))
                        }
                        "sender" => part["sender"] = json!("889"),
                        "different-reference" => part["concat"]["reference"] = json!(43),
                        _ => {}
                    }
                    part
                })
                .collect();
            ingest(&store, parts).unwrap();
            let forwardable = ready_for_forwarding(list(&store).unwrap());
            assert_eq!(forwardable.len(), if quarantined { 1 } else { 2 }, "{case}");
            assert!(
                forwardable.contains(&legacy),
                "historical canonical changed: {case}"
            );
            let retained = store.load().unwrap();
            assert_eq!(
                retained
                    .iter()
                    .filter(|item| item.get("multipart_potential_legacy_fence").is_some())
                    .count(),
                if quarantined { total as usize } else { 0 },
                "{case}"
            );
            // A single-part SMS in the very same scope and window stays valid.
            let mut ordinary = fixture("888", "ordinary SMS", "2026-10-21T10:02:02Z");
            ordinary["subscription_id"] = json!(subscription);
            ingest(&store, vec![ordinary]).unwrap();
            assert!(
                ready_for_forwarding(list(&store).unwrap())
                    .iter()
                    .any(|item| item["content"] == "ordinary SMS")
            );
        }
    }

    #[test]
    fn potential_legacy_fence_survives_capacity_pruning_restart_and_canonical_deletion() {
        for prune_by_capacity in [false, true] {
            let directory = TestDirectory::new();
            let store = directory.store();
            let subscription = format!("sub1_{}", "6".repeat(64));
            let legacy = seed_hidden_anchor_legacy(&store, &subscription);
            let first = concatenated_part(
                &subscription,
                "888",
                "late-first",
                1,
                "2026-10-21T10:02:02Z",
            );
            let second = concatenated_part(
                &subscription,
                "888",
                "late-second",
                2,
                "2026-10-21T10:02:03Z",
            );
            ingest(&store, vec![first.clone()]).unwrap();
            if prune_by_capacity {
                ingest(
                    &store,
                    (0..999)
                        .map(|index| {
                            fixture(
                                "other",
                                &format!("ordinary-{index}"),
                                "2026-10-21T10:10:00Z",
                            )
                        })
                        .collect(),
                )
                .unwrap();
            } else {
                assert_eq!(
                    delete_id(&store, legacy["id"].as_str().unwrap()).unwrap(),
                    1
                );
            }
            assert!(
                !store
                    .load()
                    .unwrap()
                    .iter()
                    .any(|item| item["id"] == legacy["id"])
            );
            drop(store);
            let store = directory.store();
            ingest(&store, vec![second.clone()]).unwrap();
            let retained = store.load().unwrap();
            assert_eq!(
                retained
                    .iter()
                    .filter(|item| item.get("multipart_potential_legacy_fence").is_some())
                    .count(),
                2
            );
            assert!(
                ready_for_forwarding(list(&store).unwrap())
                    .iter()
                    .all(|item| item["content"].as_str().unwrap().starts_with("ordinary-"))
            );
            let bytes = fs::read(store.path()).unwrap();
            assert_eq!(
                ingest(&store, vec![first.clone(), second.clone()]).unwrap(),
                0
            );
            assert_eq!(fs::read(store.path()).unwrap(), bytes);
            // Removing one quarantined observation must not clear the other
            // observation's inherited fence or shift its original time window.
            assert_eq!(delete_id(&store, &cache_digest(&first)).unwrap(), 1);
            let replacement = concatenated_part(
                &subscription,
                "888",
                "replacement",
                1,
                "2026-10-21T10:02:04Z",
            );
            ingest(&store, vec![replacement]).unwrap();
            assert_eq!(
                store
                    .load()
                    .unwrap()
                    .iter()
                    .filter(|item| item.get("multipart_potential_legacy_fence").is_some())
                    .count(),
                2
            );
            if prune_by_capacity {
                // The earlier capacity assertions deliberately filled the
                // archive with newer ordinary messages. Make explicit room
                // for the two older-SCTS positive controls below; otherwise
                // the unchanged incoming-retention rule correctly rejects
                // them independently of the legacy fence.
                let ordinary_ids: Vec<_> = store
                    .load()
                    .unwrap()
                    .into_iter()
                    .filter(|item| {
                        item["content"]
                            .as_str()
                            .is_some_and(|text| text.starts_with("ordinary-"))
                    })
                    .take(2)
                    .map(|item| item["id"].as_str().unwrap().to_owned())
                    .collect();
                assert_eq!(ordinary_ids.len(), 2);
                for id in ordinary_ids {
                    assert_eq!(delete_id(&store, &id).unwrap(), 1);
                }
            }
            // Once the legacy canonical is gone, only an observed raw base
            // may propagate its inherited fence. Another reference is free.
            let another_reference: Vec<_> = (1..=2)
                .map(|sequence| {
                    let mut part = concatenated_part(
                        &subscription,
                        "888",
                        &format!("other-ref-{sequence}"),
                        sequence,
                        "2026-10-21T10:02:05Z",
                    );
                    part["concat"]["reference"] = json!(43);
                    part
                })
                .collect();
            ingest(&store, another_reference).unwrap();
            assert!(
                ready_for_forwarding(list(&store).unwrap())
                    .iter()
                    .any(|item| item["content"] == "other-ref-1other-ref-2")
            );
            // Inherited fences keep V=10:02:00; they do not slide to the newly
            // observed :04 SCTS and quarantine a legitimate later group.
            ingest(
                &store,
                vec![
                    concatenated_part(&subscription, "888", "fresh-", 1, "2026-10-21T10:04:01Z"),
                    concatenated_part(&subscription, "888", "message", 2, "2026-10-21T10:04:01Z"),
                ],
            )
            .unwrap();
            assert!(
                ready_for_forwarding(list(&store).unwrap())
                    .iter()
                    .any(|item| item["content"] == "fresh-message")
            );
        }
    }

    #[test]
    fn multipart_archive_late_conflict_quarantine_survives_retention_and_canonical_pruning() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let subscription = format!("sub1_{}", "f".repeat(64));
        ingest(
            &store,
            vec![
                concatenated_part(&subscription, "888", "original-", 1, "2026-10-21T10:00:00Z"),
                concatenated_part(&subscription, "888", "tail", 2, "2026-10-21T10:00:01Z"),
            ],
        )
        .unwrap();
        let late_first = concatenated_part(
            &subscription,
            "888",
            "late-first",
            1,
            "2026-10-21T10:00:00Z",
        );
        let late_second = concatenated_part(
            &subscription,
            "888",
            "late-second",
            2,
            "2026-10-21T10:00:02Z",
        );
        ingest(&store, vec![late_first.clone(), late_second.clone()]).unwrap();
        let mut existing = store.load().unwrap();
        existing
            .extend((0..997).map(|index| {
                fixture("recent", &format!("recent-{index}"), "2026-10-21T11:00:00Z")
            }));
        store.replace(existing).unwrap();
        ingest(
            &store,
            vec![fixture("recent", "latest", "2026-10-21T12:00:00Z")],
        )
        .unwrap();
        drop(store);

        let store = directory.store();
        let retained = list(&store).unwrap();
        assert_eq!(retained.len(), 1_000);
        assert!(
            !retained
                .iter()
                .any(|item| item["content"] == "original-tail")
        );
        assert_eq!(
            retained
                .iter()
                .filter(|item| item["multipart_status"] == "ambiguous")
                .count(),
            2
        );
        assert_eq!(ready_for_forwarding(retained).len(), 998);
        let bytes = fs::read(store.path()).unwrap();
        assert_eq!(ingest(&store, vec![late_first, late_second]).unwrap(), 0);
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
        let another_late = concatenated_part(
            &subscription,
            "888",
            "another-late",
            2,
            "2026-10-21T10:00:03Z",
        );
        assert_eq!(ingest(&store, vec![another_late]).unwrap(), 1);
        assert_eq!(
            list(&store)
                .unwrap()
                .iter()
                .filter(|item| item["multipart_status"] == "ambiguous")
                .count(),
            3,
            "retained quarantine supplies correlation context after canonical pruning"
        );
    }

    #[test]
    fn multipart_archive_late_conflicts_and_canonical_have_independent_deletion_ids() {
        for delete_canonical in [false, true] {
            let directory = TestDirectory::new();
            let store = directory.store();
            let subscription = format!("sub1_{}", "a".repeat(64));
            ingest(
                &store,
                vec![
                    concatenated_part(&subscription, "888", "original-", 1, "2026-10-21T10:00:00Z"),
                    concatenated_part(&subscription, "888", "tail", 2, "2026-10-21T10:00:01Z"),
                ],
            )
            .unwrap();
            let canonical = store.load().unwrap().pop().unwrap();
            let conflict = concatenated_part(
                &subscription,
                "888",
                "late-first",
                1,
                "2026-10-21T10:00:00Z",
            );
            let conflict_id = cache_digest(&conflict);
            ingest(&store, vec![conflict]).unwrap();
            let deleted = if delete_canonical {
                canonical["id"].as_str().unwrap()
            } else {
                &conflict_id
            };
            assert_eq!(delete_id(&store, deleted).unwrap(), 1);
            let remaining = list(&store).unwrap();
            assert_eq!(remaining.len(), 1);
            if delete_canonical {
                assert_eq!(remaining[0]["id"], conflict_id);
                assert_eq!(remaining[0]["multipart_status"], "ambiguous");
                assert!(ready_for_forwarding(remaining).is_empty());
            } else {
                assert_eq!(remaining, vec![canonical]);
            }
        }
    }

    #[test]
    fn multipart_archive_reassembles_out_of_order_after_reopen_without_forwarding_a_part() {
        let directory = TestDirectory::new();
        let subscription = format!("sub1_{}", "d".repeat(64));
        let first = concatenated_part(&subscription, "888", "first-", 1, "2026-10-21T10:00:00Z");
        let second = concatenated_part(&subscription, "888", "second", 2, "2026-10-21T10:00:01Z");

        {
            let store = directory.store();
            assert_eq!(ingest(&store, vec![second.clone()]).unwrap(), 1);
            assert_eq!(
                store.load().unwrap().len(),
                1,
                "the out-of-order part is durable"
            );
            assert!(ready_for_forwarding(list(&store).unwrap()).is_empty());
        }

        // A separate store instance covers reopening within one process.
        let reopened = directory.store();
        ingest(&reopened, vec![first.clone()]).expect("complete after reopening archive");
        let archived = reopened.load().unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0]["content"], "first-second");
        assert_eq!(archived[0]["multipart_status"], "complete");
        let eligible = ready_for_forwarding(list(&reopened).unwrap());
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0]["content"], "first-second");

        assert_eq!(ingest(&reopened, vec![second, first]).unwrap(), 0);
        assert_eq!(
            reopened.load().unwrap(),
            archived,
            "source replay stays idempotent"
        );
    }

    #[test]
    fn multipart_archive_reassembles_out_of_order_after_process_exit_without_forwarding_a_part() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};

        const CHILD_ROOT: &str = "SWITCH_MULTIPART_ARCHIVE_EXIT_TEST_ROOT";
        const EXIT_CODE: i32 = 86;
        let subscription = format!("sub1_{}", "d".repeat(64));
        let first = concatenated_part(&subscription, "888", "first-", 1, "2026-10-21T10:00:00Z");
        let second = concatenated_part(&subscription, "888", "second", 2, "2026-10-21T10:00:01Z");

        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            assert!(root.is_dir() && !root.is_symlink());
            assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
            struct TeardownSentinel(PathBuf);
            impl Drop for TeardownSentinel {
                fn drop(&mut self) {
                    fs::write(&self.0, b"normal Rust teardown ran").unwrap();
                }
            }
            let _sentinel = TeardownSentinel(root.join("normal-teardown"));
            let store = ArchiveStore::new(root.join("sms-archive.json"));
            assert_eq!(ingest(&store, vec![second.clone()]).unwrap(), 1);
            assert_eq!(store.load().unwrap().len(), 1);
            assert!(ready_for_forwarding(list(&store).unwrap()).is_empty());
            let mut output = std::io::stdout().lock();
            writeln!(
                output,
                "MULTIPART_SEQ2_COMMITTED pid={}",
                std::process::id()
            )
            .unwrap();
            output.flush().unwrap();
            // Bypass Rust destructors with the store and sentinel still alive.
            // This covers process loss after durable ingest, not power loss.
            std::process::exit(EXIT_CODE);
        }

        let directory = TestDirectory::new();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "message_archive::tests::multipart_archive_reassembles_out_of_order_after_process_exit_without_forwarding_a_part",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &directory.0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn archive writer process");
        let child_pid = child.id();
        assert_ne!(child_pid, std::process::id());
        let output = child
            .wait_with_output()
            .expect("wait for archive writer exit");
        assert_eq!(
            output.status.code(),
            Some(EXIT_CODE),
            "child failed before committed exit: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains(&format!("MULTIPART_SEQ2_COMMITTED pid={child_pid}"))
        );
        assert!(!directory.0.join("normal-teardown").exists());

        // Only the exited child has touched this archive. Reload its second
        // part before any parent write and keep it ineligible for forwarding.
        let reopened = directory.store();
        let seq2_bytes = fs::metadata(reopened.path()).unwrap().len();
        let persisted = reopened
            .load()
            .expect("read archive after writer process exited");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0]["concat"]["sequence"], 2);
        assert_eq!(persisted[0]["content"], "second");
        assert!(ready_for_forwarding(list(&reopened).unwrap()).is_empty());
        ingest(&reopened, vec![first.clone()]).expect("complete after writer process exited");
        let archived = reopened.load().unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0]["content"], "first-second");
        assert_eq!(archived[0]["multipart_status"], "complete");
        let eligible = ready_for_forwarding(list(&reopened).unwrap());
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0]["content"], "first-second");

        let completed_bytes = fs::read(reopened.path()).unwrap();
        assert_eq!(ingest(&reopened, vec![second, first]).unwrap(), 0);
        assert_eq!(fs::read(reopened.path()).unwrap(), completed_bytes);
        assert_eq!(
            reopened.load().unwrap(),
            archived,
            "source replay stays idempotent"
        );
        eprintln!(
            "multipart process restart verified: writer_pid={child_pid}, exit={EXIT_CODE}, seq2_bytes={seq2_bytes}, teardown absent, partial not forwarded, completed=1, completed_bytes={}, replay_added=0",
            completed_bytes.len()
        );
    }

    #[test]
    fn multipart_archive_correlates_cross_second_parts_but_not_reused_references() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let subscription = format!("sub1_{}", "f".repeat(64));
        let first = concatenated_part(&subscription, "888", "cross-", 1, "2026-10-21T10:00:59Z");
        let second = concatenated_part(&subscription, "888", "second", 2, "2026-10-21T10:01:00Z");
        ingest(&store, vec![first, second]).unwrap();
        let public = list(&store).unwrap();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0]["content"], "cross-second");
        assert_eq!(public[0]["multipart_status"], "complete");

        let reused_reference_store = TestDirectory::new().store();
        ingest(
            &reused_reference_store,
            vec![concatenated_part(
                &subscription,
                "888",
                "old-first",
                1,
                "2026-10-21T10:00:00Z",
            )],
        )
        .unwrap();
        ingest(
            &reused_reference_store,
            vec![concatenated_part(
                &subscription,
                "888",
                "new-second",
                2,
                "2026-10-21T10:03:00Z",
            )],
        )
        .unwrap();
        let separate = list(&reused_reference_store).unwrap();
        assert_eq!(separate.len(), 2);
        assert!(
            separate
                .iter()
                .all(|item| item["multipart_status"] == "partial")
        );
    }

    #[test]
    fn multipart_archive_isolates_subscriptions_and_quarantines_conflicting_parts() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let first_subscription = format!("sub1_{}", "a".repeat(64));
        let second_subscription = format!("sub1_{}", "b".repeat(64));
        let first = concatenated_part(&first_subscription, "888", "one", 1, "2026-10-21T10:00:00Z");
        let other_profile = concatenated_part(
            &second_subscription,
            "888",
            "two",
            2,
            "2026-10-21T10:00:00Z",
        );
        ingest(&store, vec![first]).unwrap();
        ingest(&store, vec![other_profile]).unwrap();
        let isolated = list(&store).unwrap();
        assert_eq!(isolated.len(), 2);
        assert!(
            isolated
                .iter()
                .all(|item| item["multipart_status"] == "partial")
        );
        assert!(ready_for_forwarding(isolated).is_empty());

        let conflict_store = TestDirectory::new().store();
        let subscription = format!("sub1_{}", "c".repeat(64));
        ingest(
            &conflict_store,
            vec![concatenated_part(
                &subscription,
                "888",
                "first copy",
                1,
                "2026-10-21T10:00:00Z",
            )],
        )
        .unwrap();
        ingest(
            &conflict_store,
            vec![concatenated_part(
                &subscription,
                "888",
                "conflicting copy",
                1,
                "2026-10-21T10:00:00Z",
            )],
        )
        .unwrap();
        ingest(
            &conflict_store,
            vec![concatenated_part(
                &subscription,
                "888",
                "tail",
                2,
                "2026-10-21T10:00:00Z",
            )],
        )
        .unwrap();
        let ambiguous = list(&conflict_store).unwrap();
        assert_eq!(ambiguous.len(), 3);
        assert!(
            ambiguous
                .iter()
                .all(|item| item["multipart_status"] == "ambiguous")
        );
        assert!(ready_for_forwarding(ambiguous).is_empty());
    }

    #[test]
    fn archive_retention_never_prunes_an_unfinished_multipart_group() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let subscription = format!("sub1_{}", "e".repeat(64));
        let pending = concatenated_part(&subscription, "888", "first", 1, "2026-10-20T10:00:00Z");
        let mut existing = vec![pending.clone()];
        existing.extend((0..999).map(|index| {
            fixture(
                &format!("sender-{index}"),
                "old message",
                &format!("2026-10-20T10:{:02}:00Z", index % 60),
            )
        }));
        store.replace(existing).unwrap();
        ingest(
            &store,
            vec![fixture("latest", "new message", "2026-10-21T10:00:00Z")],
        )
        .expect("retention keeps pending SMS segments");
        let items = store.load().unwrap();
        assert_eq!(items.len(), 1_000);
        assert!(
            items
                .iter()
                .any(|item| cache_digest(item) == cache_digest(&pending))
        );
        let public = list(&store).unwrap();
        assert!(
            public
                .iter()
                .any(|item| item["multipart_status"] == "partial" && item["content"] == "first")
        );
    }

    #[test]
    fn batch_delete_preserves_unselected_arrival_and_propagates_corruption() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let first = fixture("888", "one", "2026-10-04T14:00:00Z");
        let second = fixture("888", "two", "2026-10-04T14:01:00Z");
        let later = fixture("888", "new arrival", "2026-10-04T14:02:00Z");
        let ids = vec![cache_digest(&first), cache_digest(&second)];
        store.replace(vec![first, second, later.clone()]).unwrap();
        assert_eq!(super::delete_ids(&store, &ids).unwrap(), 2);
        assert_eq!(store.load().unwrap(), vec![later]);
        assert_eq!(super::delete_ids(&store, &ids).unwrap(), 0);
        fs::write(store.path(), b"not valid JSON").unwrap();
        assert!(super::delete_ids(&store, &ids).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), b"not valid JSON");
    }

    #[test]
    fn internal_ingest_rejects_an_incoming_record_that_retention_would_drop() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let existing: Vec<_> = (0..1_000)
            .map(|index| {
                let timestamp =
                    time::OffsetDateTime::from_unix_timestamp(1_800_000_000 + i64::from(index))
                        .expect("timestamp")
                        .format(&time::format_description::well_known::Rfc3339)
                        .expect("RFC3339 timestamp");
                fixture(&format!("sender-{index}"), "existing", &timestamp)
            })
            .collect();
        store.replace(existing.clone()).expect("seed archive");
        let older = fixture("older", "would be discarded", "2025-01-01T00:00:00Z");
        let error = ingest(&store, vec![older]).expect_err("capacity must reject loss");
        assert!(error.to_string().contains("discard"));
        assert_eq!(store.load().expect("archive remains unchanged"), existing);
    }

    #[test]
    fn internal_ingest_sorts_by_absolute_time_across_offsets() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let earlier = fixture("a", "earlier", "2026-10-04T13:30:00+02:00");
        let later = fixture("b", "later", "2026-10-04T12:00:00Z");
        ingest(&store, vec![earlier, later]).expect("ingest");
        let items = store.load().expect("load");
        assert_eq!(items[0]["sender"], "b");
        assert_eq!(items[1]["sender"], "a");
    }

    #[test]
    fn archive_mutations_fail_closed_on_corrupt_or_non_regular_storage() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let corrupt = b"{broken archive";
        fs::write(store.path(), corrupt).expect("write corrupt fixture");

        assert!(
            ingest(
                &store,
                vec![fixture("sender", "message", "2026-10-04T12:00:00Z")]
            )
            .is_err()
        );
        assert!(delete_id(&store, "message-id").is_err());
        assert!(delete_sender(&store, "sender").is_err());
        assert_eq!(
            fs::read(store.path()).expect("read original fixture"),
            corrupt
        );

        // Clear also fails closed when the archive path has been replaced by
        // a directory; it must not recursively remove or replace that path.
        fs::remove_file(store.path()).expect("remove corrupt fixture");
        fs::create_dir(store.path()).expect("make non-regular archive path");
        assert!(store.replace(Vec::new()).is_err());
        assert!(store.path().is_dir());
    }

    #[test]
    fn archive_ingest_delete_and_clear_recover_after_store_reopen() {
        let directory = TestDirectory::new();
        let incoming = fixture("sender", "durable", "2026-10-04T12:00:00Z");
        let id = stable_id(&incoming);

        {
            let store = directory.store();
            assert_eq!(
                ingest(&store, vec![incoming.clone()]).expect("initial ingest"),
                1
            );
        }
        {
            let reopened = directory.store();
            assert_eq!(
                ingest(&reopened, vec![incoming]).expect("idempotent replay"),
                0
            );
            assert_eq!(reopened.load().expect("recovered archive").len(), 1);
            assert_eq!(delete_id(&reopened, &id).expect("delete after reopen"), 1);
        }
        {
            let reopened = directory.store();
            assert!(reopened.load().expect("recovered deletion").is_empty());
            reopened
                .replace(vec![fixture("sender", "clear me", "2026-10-04T12:01:00Z")])
                .expect("seed before clear");
            reopened.replace(Vec::new()).expect("durable clear");
        }
        assert!(
            directory
                .store()
                .load()
                .expect("recovered clear")
                .is_empty()
        );
    }

    #[test]
    fn public_list_merges_archive_and_volatile_records_by_go_stable_identity() {
        use super::{list_sms, merge_sms};

        let directory = TestDirectory::new();
        let store = directory.store();
        let archived = fixture("archived", "kept", "2026-10-04T12:00:00Z");
        store.replace(vec![archived.clone()]).expect("seed archive");
        let duplicate = archived.clone();
        let mut latest = fixture("volatile", "device only", "2026-10-04T13:00:00Z");
        latest.as_object_mut().unwrap().remove("archived");
        let merged = merge_sms(
            list(&store).expect("list archive"),
            vec![duplicate, latest.clone()],
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["sender"], "volatile");
        assert_eq!(merged[0]["archived"], Value::Null);
        assert_eq!(merged[1]["sender"], "archived");
        assert_eq!(merged[1]["archived"], true);
        assert_eq!(merged[1]["id"], stable_id(&archived));

        let (status, metadata) = list_sms(&store, vec![latest], None, true, 1).expect("metadata");
        assert_eq!(status, hyper::StatusCode::OK);
        assert_eq!(metadata[0]["sender"], "volatile");
        assert_eq!(metadata[0]["has_content"], true);
        assert!(metadata[0].get("content").is_none());
    }

    #[test]
    fn public_list_query_parsing_matches_go_bounds_and_precedence() {
        use super::parse_sms_query;

        assert_eq!(
            parse_sms_query("view=MeTaDaTa&limit=10&id=message%201").expect("valid query"),
            (Some("message 1".to_owned()), false, usize::MAX)
        );
        assert_eq!(
            parse_sms_query("id=message&limit=0").expect("id takes precedence"),
            (Some("message".to_owned()), false, usize::MAX)
        );
        assert!(parse_sms_query("limit=0").is_err());
        assert!(parse_sms_query("limit=101").is_err());
        assert_eq!(
            parse_sms_query("unknown=value").expect("unknown fields ignored"),
            (None, false, usize::MAX)
        );
    }

    #[test]
    fn compatibility_generation_changes_do_not_replace_native_usb_preview() {
        use super::InboxState;

        let inbox = InboxState::for_go_producer("first-child".into());
        let usb = fixture("usb", "USB body", "2026-10-04T14:00:00Z");
        inbox.merge_usb(vec![usb.clone()]).unwrap();
        assert!(inbox.owns_current_producer("first-child"));
        assert!(!inbox.owns_current_producer("second-child"));
        inbox.replace_go_producer("second-child".into()).unwrap();
        assert!(!inbox.owns_current_producer("first-child"));
        assert!(inbox.owns_current_producer("second-child"));
        let listed = inbox.snapshot().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["content"], usb["content"]);
    }

    #[test]
    fn native_module_identity_is_independent_and_never_authorizes_go_ingress() {
        let inbox = super::InboxState::for_go_producer("go-old".into());
        assert!(inbox.owns_module_producer("go-old"));
        assert!(inbox.bind_native_module_producer("go-old".into()).is_err());
        inbox
            .bind_native_module_producer("rust-physical".into())
            .unwrap();
        inbox
            .bind_native_module_producer("rust-physical".into())
            .unwrap();
        assert!(
            inbox
                .bind_native_module_producer("replacement".into())
                .is_err()
        );
        assert!(inbox.replace_go_producer("rust-physical".into()).is_err());
        assert!(!inbox.compatibility_module_allowed());
        assert!(!inbox.owns_current_producer("rust-physical"));
        assert!(!inbox.owns_module_producer("go-old"));
        inbox.replace_go_producer("go-new".into()).unwrap();
        assert_eq!(
            inbox.current_module_producer_id().as_deref(),
            Some("rust-physical")
        );
        assert!(inbox.owns_module_producer("rust-physical"));
        assert!(!inbox.owns_module_producer("go-new"));
        assert!(inbox.owns_current_producer("go-new"));
        assert!(!inbox.owns_current_producer("go-old"));
    }

    #[tokio::test]
    async fn public_sms_list_uses_archive_and_usb_preview_without_go_snapshot() {
        use super::{InboxState, list_sms};

        let directory = TestDirectory::new();
        let store = directory.store();
        let archived = fixture("archived", "stored", "2026-10-04T12:00:00Z");
        let usb_preview = fixture("usb", "pending USB preview", "2026-10-04T13:00:00Z");
        store.replace(vec![archived.clone()]).unwrap();
        let inbox = InboxState::default();
        inbox.merge_usb(vec![usb_preview.clone()]).unwrap();

        let visible = inbox.snapshot().unwrap();
        let (status, public) = list_sms(&store, visible, None, false, usize::MAX).unwrap();
        assert_eq!(status, hyper::StatusCode::OK);
        assert_eq!(public.as_array().unwrap().len(), 2);
        assert!(
            public
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["content"] == "stored")
        );
        assert!(
            public
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["content"] == "pending USB preview")
        );
    }

    #[tokio::test]
    async fn native_forwarding_rejects_raw_concat_preview_and_keeps_regular_and_canonical_sms() {
        use super::{InboxState, forwarding_owned_list_from_store};

        let directory = TestDirectory::new();
        let store = directory.store();
        let inbox = InboxState::default();
        let subscription = format!("sub1_{}", "a".repeat(64));
        let first = concatenated_part(&subscription, "888", "first-", 1, "2026-10-21T10:00:00Z");
        let second = concatenated_part(&subscription, "888", "second", 2, "2026-10-21T10:00:01Z");
        let mut regular = fixture("65075", "normal USB preview", "2026-10-21T11:00:00Z");
        regular["device"] = json!("dji");
        // Current USB decoding never produces this raw concat preview. This
        // deliberately synthetic source tests the final forwarding boundary
        // against future producers that omit multipart_status annotations.
        inbox
            .merge_usb(vec![first.clone(), regular.clone()])
            .unwrap();
        let pending = forwarding_owned_list_from_store(&inbox, store.clone())
            .await
            .unwrap();
        assert_eq!(
            pending.len(),
            1,
            "an unannotated raw segment must not forward"
        );
        assert_eq!(pending[0]["content"], "normal USB preview");

        ingest(&store, vec![first.clone(), second.clone()]).unwrap();
        inbox.merge_usb(vec![second]).unwrap();
        let ready = forwarding_owned_list_from_store(&inbox, store.clone())
            .await
            .unwrap();
        assert_eq!(
            ready.len(),
            2,
            "only regular USB and canonical multipart messages forward"
        );
        assert!(ready.iter().all(|item| item.get("concat").is_none()));
        assert!(
            ready
                .iter()
                .any(|item| item["content"] == "normal USB preview")
        );
        assert!(ready.iter().any(
            |item| item["content"] == "first-second" && item["multipart_status"] == "complete"
        ));
        // A forged complete label cannot turn a raw fragment into canonical
        // content. Only the archive assembler removes concatenation metadata.
        let mut misleading = first;
        misleading["multipart_status"] = json!("complete");
        assert!(ready_for_forwarding(vec![misleading]).is_empty());
    }

    #[tokio::test]
    async fn native_forwarding_reads_durable_archive_and_usb_preview_without_go_snapshot() {
        use super::{InboxState, forwarding_owned_list_from_store};

        let directory = TestDirectory::new();
        let store = directory.store();
        let archived = fixture("65075", "PAC archived first", "2026-10-04T12:00:00Z");
        store.replace(vec![archived.clone()]).unwrap();
        let inbox = InboxState::default();
        let not_adopted_yet = fixture("65075", "PAC in Rust USB preview", "2026-10-04T13:00:00Z");
        inbox.merge_usb(vec![not_adopted_yet]).unwrap();

        // An unconfirmed or absent Go child is irrelevant: archived module/IMS
        // messages and Rust's no-adopt USB preview are both native sources.
        let items = forwarding_owned_list_from_store(&inbox, store)
            .await
            .expect("Rust-owned sources should be sufficient");
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .any(|item| item["content"] == "PAC archived first")
        );
        assert!(
            items
                .iter()
                .any(|item| item["content"] == "PAC in Rust USB preview")
        );
    }

    #[test]
    fn usb_prune_only_removes_selected_native_ids_and_keeps_concurrent_arrivals() {
        use super::InboxState;
        let inbox = InboxState::default();
        let old = fixture("same", "old", "2026-10-04T13:00:00Z");
        let new = fixture("same", "new", "2026-10-04T14:00:00Z");
        inbox.merge_usb(vec![old.clone()]).unwrap();
        let selected = vec![
            inbox.usb_snapshot().unwrap()[0]["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        ];
        inbox.merge_usb(vec![new.clone()]).unwrap();
        assert_eq!(inbox.prune_usb(&selected).unwrap(), 1);
        assert_eq!(inbox.usb_snapshot().unwrap()[0]["content"], "new");
        assert_eq!(inbox.snapshot().unwrap().len(), 1);
        assert_eq!(inbox.prune_usb(&selected).unwrap(), 0);
        inbox.clear_usb().unwrap();
        assert!(inbox.snapshot().unwrap().is_empty());
    }

    #[test]
    fn usb_cache_is_validated_bounded_and_retains_first_observation_per_identity() {
        use super::{InboxSnapshotError, InboxState};
        let inbox = InboxState::default();
        let old = fixture("same", "old", "2026-10-04T13:00:00Z");
        inbox.merge_usb(vec![old.clone()]).unwrap();
        let mut duplicate = old.clone();
        duplicate["future_field"] = json!("must not replace earlier observation");
        inbox.merge_usb(vec![duplicate]).unwrap();
        assert_eq!(
            inbox.usb_snapshot().unwrap()[0]["future_field"],
            old["future_field"]
        );
        let mut bad = old;
        bad["timestamp"] = json!("invalid");
        assert_eq!(inbox.merge_usb(vec![bad]), Err(InboxSnapshotError::Invalid));
        assert_eq!(inbox.usb_snapshot().unwrap().len(), 1);
        let newer: Vec<_> = (0..501)
            .map(|i| fixture("peer", &i.to_string(), "2026-10-04T14:00:00Z"))
            .collect();
        assert_eq!(inbox.merge_usb(newer).unwrap(), 500);
        assert!(
            inbox
                .usb_snapshot()
                .unwrap()
                .iter()
                .all(|item| item["content"] != "old")
        );
    }

    #[test]
    fn delete_by_id_removes_only_the_exact_logical_message() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let first = fixture("888", "same", "2026-09-01T12:00:00Z");
        let second = fixture("888", "same", "2026-09-01T12:00:01Z");
        let first_id = stable_id(&first);
        store
            .replace(vec![first, second.clone()])
            .expect("seed archive");

        assert_eq!(delete_id(&store, &first_id).expect("delete item"), 1);
        assert_eq!(store.load().expect("reload archive"), vec![second]);
    }

    #[test]
    fn delete_by_public_sms_id_matches_legacy_archive_rows() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let mut legacy = fixture("888", "legacy-row", "2026-09-01T12:00:00Z");
        legacy["id"] = json!("legacy-app-id");
        let public_id = cache_digest(&legacy);
        store.replace(vec![legacy]).expect("seed legacy archive");
        assert_eq!(
            delete_id(&store, &public_id).expect("delete by visible id"),
            1
        );
        assert!(store.load().expect("reload archive").is_empty());
    }

    #[test]
    fn delete_sender_is_trimmed_exact_and_preserves_other_records() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let first = fixture("+447700900123", "one", "2026-09-01T12:00:00Z");
        let second = fixture("+447700900124", "two", "2026-09-01T12:00:01Z");
        store
            .replace(vec![first, second.clone()])
            .expect("seed archive");

        assert_eq!(
            delete_sender(&store, "+447700900123").expect("delete sender"),
            1
        );
        assert_eq!(store.load().expect("reload archive"), vec![second]);
    }

    #[test]
    fn frozen_archive_snapshot_never_removes_later_matching_sender_rows() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let first = fixture("888", "first", "2026-09-01T12:00:00Z");
        let late = fixture("888", "late", "2026-09-01T12:00:01Z");
        store.replace(vec![first.clone()]).unwrap();
        let ids = frozen_archive_ids(&list(&store).unwrap());
        store.replace(vec![first, late.clone()]).unwrap();
        assert_eq!(delete_ids(&store, &ids).unwrap(), 1);
        assert_eq!(store.load().unwrap(), vec![late]);
        assert_eq!(delete_ids(&store, &ids).unwrap(), 0);
    }

    #[tokio::test]
    async fn cancelled_retained_outgoing_capacity_refusal_is_business_output_not_orphan_io() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let existing: Vec<_> = (0..1_000)
            .map(|i| fixture(&format!("sender-{i}"), "existing", "2026-10-04T14:00:00Z"))
            .collect();
        store.replace(existing.clone()).unwrap();
        let before = fs::read(store.path()).unwrap();
        let older = fixture("older", "would be discarded", "2025-01-01T00:00:00Z");
        let executor = crate::archive_io::IsolatedTestExecutor::new();
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let worker_entered = entered.clone();
        let (resume, wait) = std::sync::mpsc::channel();
        let worker_store = store.clone();
        let observer=executor.run_durable(move|| {
            let result=super::retained_ingest_result(ingest(&worker_store,vec![older]));
            assert!(matches!(&result,Ok(Err(proletarius_store::StoreError::Configuration(code))) if code=="archive capacity would discard an incoming SMS"));
            worker_entered.notify_one();wait.recv().unwrap();result
        },());
        entered.notified().await;
        drop(observer);
        resume.send(()).unwrap();
        executor.finish_pending().await.unwrap();
        executor.finish_pending().await.unwrap();
        assert_eq!(fs::read(store.path()).unwrap(), before);
        assert!(
            super::retained_ingest_result(Err(proletarius_store::StoreError::Configuration(
                "unknown future error".into()
            )))
            .is_err()
        );
    }
}
