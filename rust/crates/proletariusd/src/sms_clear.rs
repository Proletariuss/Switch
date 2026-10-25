//! Rust-owned SMS clear policy and proof-bound SIM/ME storage transactions.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use proletarius_store::ArchiveStore;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::message_archive::InboxState;
use crate::usb_sms_receive::{FrozenDeletionSelection, Service as UsbSmsService};

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_CLEAR_IDS: usize = 1500;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClearRequest {
    #[serde(default)]
    sm: bool,
    #[serde(default)]
    me: bool,
    #[serde(default)]
    local: bool,
}

struct ClearPlan {
    local_ids: Vec<String>,
    physical: FrozenDeletionSelection,
    clear_local: bool,
    sm: bool,
    me: bool,
}

fn failure(status: StatusCode, code: &'static str, error: &'static str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":error,"code":code,"retryable":status.is_server_error()}),
    )
}

fn selected_ids(items: &[Value]) -> Option<Vec<String>> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for item in items {
        let id = item.get("id")?.as_str()?.trim();
        if !is_sms_identity(id) {
            return None;
        }
        if seen.insert(id.to_owned()) {
            ids.push(id.to_owned());
        }
    }
    (ids.len() <= MAX_CLEAR_IDS).then_some(ids)
}

fn is_sms_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn clear_ids(
    store: ArchiveStore,
    usb_sms: UsbSmsService,
    ids: Vec<String>,
) -> Result<(), &'static str> {
    if ids.len() > MAX_CLEAR_IDS || ids.iter().any(|id| !is_sms_identity(id)) {
        return Err("sms_inbox_snapshot_invalid");
    }
    if ids.is_empty() {
        return Ok(());
    }
    crate::message_archive::delete_ids_from_store(store, ids.clone())
        .await
        .map_err(|_| "sms_archive_delete_failed")?;
    usb_sms
        .prune_public_ids(&ids)
        .map_err(|_| "sms_usb_preview_prune_failed")?;
    Ok(())
}

#[cfg(test)]
async fn clear_local(
    store: ArchiveStore,
    inbox: InboxState,
    usb_sms: UsbSmsService,
) -> Result<(), &'static str> {
    let items = crate::message_archive::public_items_from(store.clone(), inbox)
        .await
        .map_err(|_| "sms_inbox_snapshot_unavailable")?;
    let ids = selected_ids(&items).ok_or("sms_inbox_snapshot_invalid")?;
    clear_ids(store, usb_sms, ids).await
}

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    inbox: InboxState,
    usb_sms: UsbSmsService,
    data_root: &Path,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST || request.uri().path() != "/api/sms/clear" {
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
    let (request_id, stripped) = match crate::public_mutation_receipt::split(&body, true) {
        Ok((Some(id), body)) => (id, body),
        Ok((None, _)) => unreachable!("required mutation identity"),
        Err(response) => return response,
    };
    let input = match serde_json::from_slice::<ClearRequest>(&stripped) {
        Ok(input) => input,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_request", "请求格式错误"),
    };
    if !input.sm && !input.me && !input.local {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"至少选择一项要清空的存储"}),
        );
    }
    match crate::public_mutation_receipt::replay_async(
        data_root,
        "/api/sms/clear",
        &request_id,
        &body,
    )
    .await
    {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return response,
    }

    let store = ArchiveStore::new(data_root.join("sms-archive.json"));
    let plan = build_plan(&input, store.clone(), inbox, &usb_sms).await;
    let binding = match &plan {
        Ok(plan) => {
            let material = serde_json::to_vec(&json!({
                "local_ids":plan.local_ids,
                "physical_tokens":plan.physical.tokens,
                "physical_binding":plan.physical.binding,
                "sm":plan.sm,"me":plan.me,"local":plan.clear_local
            }))
            .expect("clear snapshot binding");
            format!(
                "sms-clear-v1:{}",
                proletarius_sms::stable_digest_hex(&material)
            )
        }
        Err(_) => format!(
            "sms-clear-preflight-v1:{}",
            proletarius_sms::stable_digest_hex(&stripped)
        ),
    };
    crate::public_mutation_receipt::run(
        data_root,
        "/api/sms/clear",
        &request_id,
        &body,
        binding,
        move || async move {
            let plan = match plan {
                Ok(plan) => plan,
                Err(response) => return response,
            };
            execute_plan(store, usb_sms, plan).await
        },
    )
    .await
}

async fn build_plan(
    input: &ClearRequest,
    store: ArchiveStore,
    inbox: InboxState,
    usb_sms: &UsbSmsService,
) -> Result<ClearPlan, Response<Full<Bytes>>> {
    let local_ids = if input.local {
        let items = crate::message_archive::public_items_from(store, inbox)
            .await
            .map_err(|_| {
                failure(
                    StatusCode::BAD_GATEWAY,
                    "sms_inbox_snapshot_unavailable",
                    "短信列表未确认，请稍后重试",
                )
            })?;
        selected_ids(&items).ok_or_else(|| {
            failure(
                StatusCode::BAD_GATEWAY,
                "sms_inbox_snapshot_invalid",
                "短信列表未确认，请刷新后重试",
            )
        })?
    } else {
        Vec::new()
    };
    let storages = [
        (input.sm, crate::usb_at_storage::Storage::SM),
        (input.me, crate::usb_at_storage::Storage::ME),
    ]
    .into_iter()
    .filter_map(|(selected, storage)| selected.then_some(storage))
    .collect::<Vec<_>>();
    let physical = if storages.is_empty() {
        FrozenDeletionSelection {
            tokens: Vec::new(),
            binding: "local-only".into(),
        }
    } else {
        usb_sms.frozen_storage_deletion(&storages)?
    };
    Ok(ClearPlan {
        local_ids,
        physical,
        clear_local: input.local,
        sm: input.sm,
        me: input.me,
    })
}

async fn execute_plan(
    store: ArchiveStore,
    usb_sms: UsbSmsService,
    plan: ClearPlan,
) -> Response<Full<Bytes>> {
    let mut details = serde_json::Map::new();
    if !plan.physical.tokens.is_empty() {
        let storages = [
            (plan.sm, crate::usb_at_storage::Storage::SM),
            (plan.me, crate::usb_at_storage::Storage::ME),
        ]
        .into_iter()
        .filter_map(|(selected, storage)| selected.then_some(storage))
        .collect();
        let outcome = match usb_sms
            .execute_public_storage_operation(crate::usb_sms_receive::PublicAction::DeleteScoped {
                tokens: plan.physical.tokens.clone(),
                storages,
            })
            .await
        {
            Ok(value) => value,
            Err(response) => return response,
        };
        let Some(deleted_ids) = validated_deleted_ids(&outcome) else {
            return failure(
                StatusCode::BAD_GATEWAY,
                "sms_clear_outcome_invalid",
                "短信存储操作结果未确认，请刷新核对",
            );
        };
        if outcome.get("complete").and_then(Value::as_bool) != Some(true) {
            return crate::sms_delete_sender::storage_sync_failure(Some(&outcome));
        }
        let detail = json!({"ok":true,"deleted_count":deleted_ids.len()});
        if plan.sm {
            details.insert("sm".into(), detail.clone());
        }
        if plan.me {
            details.insert("me".into(), detail);
        }
    } else {
        if plan.sm {
            details.insert("sm".into(), json!({"ok":true,"deleted_count":0}));
        }
        if plan.me {
            details.insert("me".into(), json!({"ok":true,"deleted_count":0}));
        }
    }
    if plan.clear_local {
        if let Err(code) = clear_ids(store, usb_sms, plan.local_ids).await {
            return failure(
                StatusCode::BAD_GATEWAY,
                code,
                "短信记录同步未确认，请刷新核对",
            );
        }
        details.insert("local".into(), json!({"ok":true}));
    }
    crate::json_response(StatusCode::OK, &json!({"cleared":true,"details":details}))
}

fn validated_deleted_ids(outcome: &Value) -> Option<Vec<String>> {
    let values = outcome.get("deleted_ids")?.as_array()?;
    let ids = values
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (ids.len() <= MAX_CLEAR_IDS
        && ids.iter().all(|id| is_sms_identity(id))
        && ids.iter().collect::<HashSet<_>>().len() == ids.len())
    .then_some(ids)
}

#[cfg(test)]
mod tests {
    use super::{clear_local, selected_ids};
    use crate::message_archive::InboxState;
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
        let path = root.join("tmp/sms-clear-tests").join(format!(
            "archive-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&path);
        ArchiveStore::new(path)
    }

    #[test]
    fn clear_snapshot_selects_unique_lowercase_content_ids_only() {
        let item = json!({"sender":"peer","content":"body","timestamp":"2026-10-09T00:00:00Z"});
        let item = {
            let mut value = item;
            value["id"] = json!(crate::message_archive::cache_digest(&value));
            value
        };
        assert_eq!(
            selected_ids(&[item.clone(), item.clone()]).unwrap().len(),
            1
        );
        assert!(selected_ids(&[json!({"sender":"peer"})]).is_none());
        assert!(selected_ids(&[json!({"id":"legacy-id"})]).is_none());
    }

    #[tokio::test]
    async fn local_clear_removes_frozen_archive_and_usb_preview_without_go() {
        let store = temp_store();
        let inbox = InboxState::default();
        let archive_item = json!({"sender":"archived","content":"one","timestamp":"2026-10-09T00:00:00Z","device":"reader"});
        let usb_item = json!({"sender":"usb","content":"three","timestamp":"2026-10-09T00:00:02Z","device":"dji","module_storage":"SM","module_index":7});
        store.replace(vec![archive_item]).unwrap();
        inbox.merge_usb(vec![usb_item]).unwrap();
        let usb_sms = Service::new(
            store.clone(),
            inbox.clone(),
            crate::usb_at::Owner::unavailable_for_tests("sms-clear-test".into()),
            crate::availability::BackendAvailability::new(),
            "sms-clear-test".into(),
        );
        let result = clear_local(store.clone(), inbox.clone(), usb_sms.clone()).await;
        assert!(result.is_ok());
        assert!(store.load().unwrap().is_empty());
        assert!(
            crate::message_archive::public_items_from(store, inbox.clone())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(inbox.usb_snapshot().unwrap().is_empty());
    }
}
