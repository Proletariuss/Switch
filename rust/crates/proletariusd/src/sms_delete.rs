//! Rust-owned local and physical SIM/ME SMS deletion policy.

use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use proletarius_store::ArchiveStore;
use serde::Deserialize;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::message_archive::InboxState;
use crate::usb_sms_receive::Service as UsbSmsService;

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const FALLBACK_MATCH_WINDOW_NS: i128 = 2_000_000_000;
const GO_ZERO_TIME_UNIX_SECONDS: i64 = -62_135_596_800;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deletion_token: Option<String>,
}

fn failure(status: StatusCode, code: &'static str, error: &'static str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":error,"code":code,"retryable":status.is_server_error()}),
    )
}

fn valid_sms_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parsed_timestamp(value: Option<&str>) -> Result<Option<OffsetDateTime>, ()> {
    value
        .map(|value| OffsetDateTime::parse(value, &Rfc3339).map_err(|_| ()))
        .transpose()
}

fn resolve_local_id(
    items: &[Value],
    input: &DeleteRequest,
) -> Result<Option<String>, &'static str> {
    let id = input.id.as_deref().unwrap_or_default().trim();
    if !id.is_empty() {
        return valid_sms_identity(id)
            .then(|| Some(id.to_owned()))
            .ok_or("invalid SMS identity");
    }
    let sender = input.sender.as_deref().unwrap_or_default();
    let content = input.content.as_deref().unwrap_or_default();
    if sender.trim().is_empty() && content.trim().is_empty() {
        return Err("index is required");
    }
    let Some(timestamp) =
        parsed_timestamp(input.timestamp.as_deref()).map_err(|_| "invalid timestamp")?
    else {
        return Ok(None);
    };
    if timestamp.unix_timestamp() == GO_ZERO_TIME_UNIX_SECONDS && timestamp.nanosecond() == 0 {
        return Ok(None);
    }

    let target_ns = timestamp.unix_timestamp_nanos();
    let mut best_delta = FALLBACK_MATCH_WINDOW_NS;
    let mut best = None;
    for item in items {
        if item.get("sender").and_then(Value::as_str) != Some(sender)
            || item.get("content").and_then(Value::as_str) != Some(content)
        {
            continue;
        }
        let Some(item_time) = item
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
        else {
            continue;
        };
        let delta = item_time.unix_timestamp_nanos().abs_diff(target_ns) as i128;
        if delta < best_delta {
            best_delta = delta;
            best = item.get("id").and_then(Value::as_str).map(str::to_owned);
        }
    }
    match best {
        Some(id) if valid_sms_identity(&id) => Ok(Some(id)),
        Some(_) => Err("invalid SMS identity"),
        None => Ok(None),
    }
}

async fn delete_local(
    store: ArchiveStore,
    inbox: InboxState,
    usb_sms: UsbSmsService,
    input: &DeleteRequest,
) -> Result<bool, &'static str> {
    let items = crate::message_archive::public_items_from(store.clone(), inbox)
        .await
        .map_err(|_| "sms_inbox_unavailable")?;
    let Some(id) = resolve_local_id(&items, input)? else {
        return Ok(false);
    };
    let ids = vec![id.clone()];
    let archived = crate::message_archive::delete_ids_from_store(store, ids.clone())
        .await
        .map_err(|_| "sms_archive_delete_failed")?;
    let usb_removed = usb_sms
        .prune_public_ids(&ids)
        .map_err(|_| "sms_usb_preview_prune_failed")?;
    Ok(archived + usb_removed > 0)
}

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    inbox: InboxState,
    usb_sms: UsbSmsService,
    data_root: &Path,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST || request.uri().path() != "/api/sms/delete" {
        return failure(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "请求方法不受支持",
        );
    }
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        MAX_REQUEST_BYTES,
        Duration::from_secs(5),
    )
    .await
    {
        Ok(body) => body,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return failure(StatusCode::BAD_REQUEST, "invalid_request", "请求格式错误"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(
                StatusCode::REQUEST_TIMEOUT,
                "request_timeout",
                "读取请求超时",
            );
        }
    };
    let input = match serde_json::from_slice::<DeleteRequest>(&body) {
        Ok(input) => input,
        Err(_) => {
            return crate::json_response(StatusCode::BAD_REQUEST, &json!({"error":"请求格式错误"}));
        }
    };
    if parsed_timestamp(input.timestamp.as_deref()).is_err() {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"invalid timestamp"}),
        );
    }
    let physical = input.index.unwrap_or_default() > 0
        && !input
            .storage
            .as_deref()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("LOCAL");
    if physical {
        if !input
            .deletion_token
            .as_deref()
            .is_some_and(valid_sms_identity)
        {
            return crate::json_response(
                StatusCode::CONFLICT,
                &json!({"error":"短信删除凭据缺失或已失效，请刷新短信列表后再操作","code":"usb_sms_deletion_refresh_required","retryable":true}),
            );
        }
        let token = input.deletion_token.clone().unwrap_or_default();
        let outcome = match usb_sms
            .execute_public_storage_operation(crate::usb_sms_receive::PublicAction::Delete {
                tokens: vec![token],
            })
            .await
        {
            Ok(value) => value,
            Err(response) => return response,
        };
        let Some(ids) = outcome
            .get("deleted_ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
        else {
            return failure(
                StatusCode::BAD_GATEWAY,
                "sms_delete_outcome_invalid",
                "短信存储操作结果未确认，请刷新核对",
            );
        };
        if !ids.is_empty() {
            if crate::message_archive::delete_ids_from_store(
                ArchiveStore::new(data_root.join("sms-archive.json")),
                ids.clone(),
            )
            .await
            .is_err()
            {
                return failure(
                    StatusCode::BAD_GATEWAY,
                    "sms_inbox_sync_failed",
                    "短信记录同步未确认，请刷新核对",
                );
            }
        }
        if outcome.get("complete").and_then(Value::as_bool) != Some(true) {
            return failure(
                StatusCode::BAD_GATEWAY,
                "usb_sms_storage_unconfirmed",
                "短信存储操作未全部确认，请刷新核对；不会自动重试设备删除",
            );
        }
        let requested_id = input.id.as_deref().unwrap_or_default();
        return crate::json_response(
            StatusCode::OK,
            &json!({"deleted":!ids.is_empty() && (requested_id.is_empty() || ids.iter().any(|id| id == requested_id)),"deleted_count":ids.len()}),
        );
    }
    let store = ArchiveStore::new(data_root.join("sms-archive.json"));
    match delete_local(store, inbox, usb_sms, &input).await {
        Ok(deleted) => crate::json_response(StatusCode::OK, &json!({"deleted":deleted})),
        Err(code @ ("index is required" | "invalid SMS identity")) => {
            crate::json_response(StatusCode::BAD_REQUEST, &json!({"error":code}))
        }
        Err("sms_inbox_unavailable") => crate::json_response(
            StatusCode::BAD_GATEWAY,
            &json!({"error":"短信列表未确认，请稍后重试","code":"sms_inbox_unavailable","retryable":true}),
        ),
        Err(_) => crate::json_response(
            StatusCode::BAD_GATEWAY,
            &json!({"error":"短信记录同步未确认，请刷新核对","code":"sms_inbox_sync_failed","retryable":true}),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{DeleteRequest, delete_local, resolve_local_id};
    use crate::message_archive::{InboxState, cache_digest};
    use crate::usb_sms_receive::Service;
    use proletarius_store::ArchiveStore;
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn temp_store() -> ArchiveStore {
        let root = std::path::PathBuf::from(
            std::env::var_os("SWITCH_BUILD_DIR").expect("workspace-env build directory"),
        );
        let path = root.join("tmp/sms-delete-tests").join(format!(
            "archive-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&path);
        ArchiveStore::new(path)
    }

    fn id_for(mut item: serde_json::Value) -> serde_json::Value {
        item["id"] = json!(cache_digest(&item));
        item
    }

    #[test]
    fn local_delete_selection_matches_the_legacy_identity_and_nearest_time_rules() {
        let first = id_for(
            json!({"sender":"peer","content":"same","timestamp":"2026-10-09T01:00:00.900Z"}),
        );
        let second = id_for(
            json!({"sender":"peer","content":"same","timestamp":"2026-10-09T01:00:01.500Z"}),
        );
        let input = DeleteRequest {
            id: None,
            storage: None,
            index: None,
            sender: Some("peer".into()),
            content: Some("same".into()),
            timestamp: Some("2026-10-09T01:00:01.400Z".into()),
            deletion_token: None,
        };
        assert_eq!(
            resolve_local_id(&[first.clone(), second.clone()], &input).unwrap(),
            second["id"].as_str().map(str::to_owned)
        );
        let explicit = DeleteRequest {
            id: Some(first["id"].as_str().unwrap().into()),
            ..input
        };
        assert_eq!(resolve_local_id(&[], &explicit).unwrap(), explicit.id);
    }

    #[tokio::test]
    async fn local_delete_removes_selected_archive_and_usb_preview_only() {
        let store = temp_store();
        let inbox = InboxState::default();
        let archived = id_for(
            json!({"sender":"archive","content":"one","timestamp":"2026-10-09T01:00:00Z","device":"reader"}),
        );
        let usb_item = id_for(
            json!({"sender":"usb","content":"three","timestamp":"2026-10-09T01:00:02Z","device":"dji","module_storage":"SM","module_index":7}),
        );
        store.replace(vec![archived.clone()]).unwrap();
        inbox.merge_usb(vec![usb_item.clone()]).unwrap();
        let usb_sms = Service::new(
            store.clone(),
            inbox.clone(),
            crate::usb_at::Owner::unavailable_for_tests("sms-delete-test".into()),
            crate::availability::BackendAvailability::new(),
            "sms-delete-test".into(),
        );
        let input = DeleteRequest {
            id: archived["id"].as_str().map(str::to_owned),
            storage: None,
            index: Some(0),
            sender: None,
            content: None,
            timestamp: None,
            deletion_token: None,
        };
        let result = delete_local(store.clone(), inbox.clone(), usb_sms.clone(), &input).await;
        assert_eq!(result, Ok(true));
        let remaining_after_delete =
            crate::message_archive::public_items_from(store.clone(), inbox.clone())
                .await
                .unwrap();
        assert!(
            !remaining_after_delete
                .iter()
                .any(|item| item["sender"] == "archive")
        );
        assert!(
            remaining_after_delete
                .iter()
                .any(|item| item["sender"] == "usb")
        );

        let usb_input = DeleteRequest {
            id: usb_item["id"].as_str().map(str::to_owned),
            storage: Some("LOCAL".into()),
            index: Some(0),
            sender: None,
            content: None,
            timestamp: None,
            deletion_token: None,
        };
        let usb_result = delete_local(store.clone(), inbox.clone(), usb_sms, &usb_input).await;
        assert_eq!(usb_result, Ok(true));
        let remaining = crate::message_archive::public_items_from(store.clone(), inbox.clone())
            .await
            .unwrap();
        assert!(!remaining.iter().any(|item| item["sender"] == "archive"));
        assert!(!remaining.iter().any(|item| item["sender"] == "usb"));
        assert!(store.load().unwrap().is_empty());
    }
}
