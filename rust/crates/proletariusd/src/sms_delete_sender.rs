//! Rust-owned sender deletion policy and proof-bound SIM/ME mutation.

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
use crate::usb_sms_receive::Service as UsbSmsService;

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_DELETE_IDS: usize = 1500;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteSenderRequest {
    sender: String,
}

#[derive(Debug)]
struct SenderDeletePlan {
    selected_ids: Vec<String>,
    physical_ids: HashSet<String>,
    native_preview_ids: Vec<String>,
    needs_physical_delete: bool,
    physical_tokens: Vec<String>,
    physical_binding: String,
}

fn response(status: StatusCode, value: &Value) -> Response<Full<Bytes>> {
    crate::json_response(status, value)
}

fn failure(status: StatusCode, code: &'static str, error: &'static str) -> Response<Full<Bytes>> {
    response(
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

fn sender_matches(item: &Value, sender: &str) -> bool {
    item.get("sender")
        .and_then(Value::as_str)
        .is_some_and(|value| value.trim() == sender)
}

fn item_id(item: &Value) -> Option<String> {
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| crate::message_archive::cache_digest(item));
    valid_sms_identity(&id).then_some(id)
}

fn build_plan(
    items: &[Value],
    usb_snapshot: &Value,
    sender: &str,
) -> Result<SenderDeletePlan, &'static str> {
    let usb_messages = usb_snapshot
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("sms_usb_snapshot_invalid")?;
    let deletion_senders = usb_snapshot
        .get("deletion_senders")
        .and_then(Value::as_array)
        .ok_or("sms_usb_snapshot_invalid")?;

    let mut selected_ids = Vec::new();
    let mut selected = HashSet::new();
    for item in items.iter().filter(|item| sender_matches(item, sender)) {
        let id = item_id(item).ok_or("sms_inbox_snapshot_invalid")?;
        if selected.insert(id.clone()) {
            selected_ids.push(id);
        }
    }
    if selected_ids.len() > MAX_DELETE_IDS {
        return Err("sms_delete_selection_too_large");
    }

    let mut physical_ids = HashSet::new();
    for item in usb_messages.iter() {
        let physical = item.get("archived").and_then(Value::as_bool) != Some(true)
            && item
                .get("module_index")
                .and_then(Value::as_i64)
                .unwrap_or_default()
                > 0;
        if physical && sender_matches(item, sender) {
            physical_ids.insert(item_id(item).ok_or("sms_inbox_snapshot_invalid")?);
        }
    }
    let hidden_physical = deletion_senders
        .iter()
        .filter_map(Value::as_str)
        .any(|value| value.trim() == sender);

    let mut native_preview_ids = Vec::new();
    let mut native_seen = HashSet::new();
    for item in usb_messages
        .iter()
        .filter(|item| sender_matches(item, sender))
    {
        let id = item_id(item).ok_or("sms_inbox_snapshot_invalid")?;
        if native_seen.insert(id.clone()) {
            native_preview_ids.push(id);
        }
    }

    let needs_physical_delete = !physical_ids.is_empty() || hidden_physical;
    Ok(SenderDeletePlan {
        selected_ids,
        physical_ids,
        native_preview_ids,
        needs_physical_delete,
        physical_tokens: Vec::new(),
        physical_binding: "local-only".into(),
    })
}

fn confirmed_ids(value: &Value) -> Option<HashSet<String>> {
    let ids = value.get("deleted_ids")?.as_array()?;
    if ids.len() > MAX_DELETE_IDS {
        return None;
    }
    let mut result = HashSet::new();
    for value in ids {
        let id = value.as_str()?;
        if !valid_sms_identity(id) || !result.insert(id.to_owned()) {
            return None;
        }
    }
    Some(result)
}

fn outcome_present(value: &Value) -> bool {
    value.get("deleted_ids").is_some() && value.get("complete").and_then(Value::as_bool).is_some()
}

fn unique_count(ids: &[String], physical: Option<&HashSet<String>>) -> usize {
    let mut all: HashSet<String> = ids.iter().cloned().collect();
    if let Some(physical) = physical {
        all.extend(physical.iter().cloned());
    }
    all.len()
}

async fn sync_selected(
    store: ArchiveStore,
    usb_sms: &UsbSmsService,
    ids: &[String],
    native_ids: &[String],
) -> Result<(), &'static str> {
    if ids.len() > MAX_DELETE_IDS {
        return Err("sms_delete_selection_too_large");
    }
    crate::message_archive::delete_ids_from_store(store, ids.to_vec())
        .await
        .map_err(|_| "sms_archive_delete_failed")?;
    if !native_ids.is_empty() {
        usb_sms
            .prune_public_ids(native_ids)
            .map_err(|_| "sms_usb_preview_prune_failed")?;
    }
    Ok(())
}

async fn execute_plan(
    store: ArchiveStore,
    usb_sms: UsbSmsService,
    plan: SenderDeletePlan,
) -> Result<Value, Response<Full<Bytes>>> {
    let mut physical_value = None;
    let mut physical_confirmed = None;
    if plan.needs_physical_delete {
        let value = usb_sms
            .execute_public_storage_operation(crate::usb_sms_receive::PublicAction::Delete {
                tokens: plan.physical_tokens.clone(),
            })
            .await?;
        if outcome_present(&value) {
            physical_confirmed = confirmed_ids(&value);
            if physical_confirmed.is_none() {
                return Err(failure(
                    StatusCode::BAD_GATEWAY,
                    "sms_delete_outcome_invalid",
                    "短信存储操作结果未确认，请刷新核对",
                ));
            }
            physical_value = Some(value.clone());
        }
        if physical_confirmed.is_none()
            || value.get("complete").and_then(Value::as_bool) != Some(true)
        {
            if let Some(confirmed) = physical_confirmed.as_ref() {
                let selected_confirmed: Vec<_> = plan
                    .selected_ids
                    .iter()
                    .filter(|id| confirmed.contains(*id))
                    .cloned()
                    .collect();
                if !selected_confirmed.is_empty()
                    && sync_selected(store, &usb_sms, &selected_confirmed, &[])
                        .await
                        .is_err()
                {
                    return Err(storage_sync_failure(physical_value.as_ref()));
                }
            }
            return Err(storage_sync_failure(physical_value.as_ref()));
        }
    }

    let approved: Vec<_> = plan
        .selected_ids
        .iter()
        .filter(|id| {
            !plan.physical_ids.contains(*id)
                || physical_confirmed
                    .as_ref()
                    .is_some_and(|confirmed| confirmed.contains(*id))
        })
        .cloned()
        .collect();
    let excluded: HashSet<_> = plan
        .physical_ids
        .iter()
        .cloned()
        .chain(
            physical_confirmed
                .iter()
                .flat_map(|ids| ids.iter().cloned()),
        )
        .collect();
    let native_ids: Vec<_> = plan
        .native_preview_ids
        .iter()
        .filter(|id| approved.contains(*id) && !excluded.contains(*id))
        .cloned()
        .collect();

    if sync_selected(store, &usb_sms, &approved, &native_ids)
        .await
        .is_err()
    {
        return Err(storage_sync_failure(physical_value.as_ref()));
    }

    let count = unique_count(&approved, physical_confirmed.as_ref());
    Ok(json!({"deleted":count > 0,"deleted_count":count}))
}

pub(crate) fn storage_sync_failure(outcome: Option<&Value>) -> Response<Full<Bytes>> {
    let Some(outcome) = outcome else {
        return response(
            StatusCode::BAD_GATEWAY,
            &json!({"error":"短信记录同步未确认，请刷新核对","code":"sms_inbox_sync_failed","recoverable":true}),
        );
    };
    let deleted = outcome
        .get("deleted_ids")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut message = format!("短信存储操作未全部完成，已确认删除 {deleted} 条。");
    if outcome.get("storage_restored").and_then(Value::as_bool) == Some(false) {
        message.push_str("存储区恢复未确认。");
    }
    if outcome.get("uncertain").and_then(Value::as_bool) == Some(true) {
        message.push_str("部分设备操作结果未知。");
    }
    for memory in ["SM", "ME"] {
        if outcome["details"][memory]["after"]
            .as_u64()
            .is_some_and(|after| after > 0)
        {
            message.push_str(&format!(
                "{memory} 仍有 {} 条。",
                outcome["details"][memory]["after"]
            ));
        }
    }
    message.push_str("请刷新核对，不会自动重新删除。");
    response(
        StatusCode::BAD_GATEWAY,
        &json!({
            "error":message,
            "code":"usb_sms_storage_unconfirmed",
            "recoverable":false,
            "complete":false,
            "uncertain":outcome.get("uncertain").and_then(Value::as_bool).unwrap_or(false),
            "storage_restored":outcome.get("storage_restored").and_then(Value::as_bool).unwrap_or(false),
            "usage":outcome.get("usage").cloned().unwrap_or_else(|| json!({})),
            "details":outcome.get("details").cloned().unwrap_or_else(|| json!({})),
            "deleted_ids":outcome.get("deleted_ids").cloned().unwrap_or_else(|| json!([])),
            "error_code":outcome.get("error_code").cloned().unwrap_or_else(|| json!("")),
        }),
    )
}

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    inbox: InboxState,
    usb_sms: UsbSmsService,
    data_root: &Path,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST || request.uri().path() != "/api/sms/delete-sender" {
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
    let mut input = match serde_json::from_slice::<DeleteSenderRequest>(&stripped) {
        Ok(input) => input,
        Err(_) => return response(StatusCode::BAD_REQUEST, &json!({"error":"请求格式错误"})),
    };
    input.sender = input.sender.trim().to_owned();
    if input.sender.is_empty() {
        return response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"sender is required"}),
        );
    }
    match crate::public_mutation_receipt::replay_async(
        data_root,
        "/api/sms/delete-sender",
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
    let plan = build_public_plan(store.clone(), inbox, &usb_sms, &input.sender).await;
    let binding = match &plan {
        Ok(plan) => {
            let material = serde_json::to_vec(&json!({
                "sender":input.sender,
                "selected_ids":plan.selected_ids,
                "physical_ids":plan.physical_ids,
                "physical_tokens":plan.physical_tokens,
                "physical_binding":plan.physical_binding
            }))
            .expect("sender deletion binding");
            format!(
                "sms-delete-sender-v1:{}",
                proletarius_sms::stable_digest_hex(&material)
            )
        }
        Err(_) => format!(
            "sms-delete-sender-preflight-v1:{}",
            proletarius_sms::stable_digest_hex(&stripped)
        ),
    };
    crate::public_mutation_receipt::run(
        data_root,
        "/api/sms/delete-sender",
        &request_id,
        &body,
        binding,
        move || async move {
            match plan {
                Ok(plan) => match execute_plan(store, usb_sms, plan).await {
                    Ok(value) => response(StatusCode::OK, &value),
                    Err(response) => response,
                },
                Err(response) => response,
            }
        },
    )
    .await
}

async fn build_public_plan(
    store: ArchiveStore,
    inbox: InboxState,
    usb_sms: &UsbSmsService,
    sender: &str,
) -> Result<SenderDeletePlan, Response<Full<Bytes>>> {
    let items = crate::message_archive::public_items_from(store, inbox)
        .await
        .map_err(|_| {
            failure(
                StatusCode::BAD_GATEWAY,
                "sms_inbox_unavailable",
                "短信列表未确认，请稍后重试",
            )
        })?;
    let usb_snapshot = usb_sms.inbox_snapshot().map_err(|_| {
        failure(
            StatusCode::BAD_GATEWAY,
            "sms_inbox_unavailable",
            "短信列表未确认，请稍后重试",
        )
    })?;
    let mut plan = build_plan(&items, &usb_snapshot, sender).map_err(|code| {
        failure(
            StatusCode::BAD_GATEWAY,
            code,
            "短信列表未确认，请刷新后重试",
        )
    })?;
    if plan.needs_physical_delete {
        let frozen = usb_sms.frozen_sender_deletion(sender)?;
        if frozen.tokens.is_empty() {
            return Err(failure(
                StatusCode::CONFLICT,
                "sms_delete_snapshot_incomplete",
                "短信物理存储快照不完整，请刷新后重试",
            ));
        }
        plan.physical_tokens = frozen.tokens;
        plan.physical_binding = frozen.binding;
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::{build_plan, confirmed_ids, storage_sync_failure, unique_count};
    use hyper::StatusCode;
    use serde_json::json;
    use std::collections::HashSet;

    fn sms(id: &str, sender: &str, index: i64, archived: bool) -> serde_json::Value {
        json!({"id":id,"sender":sender,"content":"test","timestamp":"2026-10-09T01:00:00Z","module_index":index,"archived":archived})
    }

    #[test]
    fn sender_selection_trims_values_and_detects_physical_and_hidden_rows() {
        let selected = sms(&"a".repeat(64), " peer ", 0, false);
        let physical = sms(&"b".repeat(64), "peer", 7, false);
        let archived = sms(&"c".repeat(64), "peer", 8, true);
        let usb = json!({"messages":[physical, archived],"deletion_senders":["hidden peer"]});
        let plan = build_plan(&[selected.clone(), selected], &usb, "peer").unwrap();
        assert_eq!(plan.selected_ids.len(), 1);
        assert!(plan.physical_ids.contains(&"b".repeat(64)));
        assert!(plan.needs_physical_delete);

        let hidden = build_plan(
            &[],
            &json!({"messages":[],"deletion_senders":[" hidden peer "]}),
            "hidden peer",
        )
        .unwrap();
        assert!(hidden.needs_physical_delete);
    }

    #[test]
    fn confirmed_outcome_is_validated_and_unique_counts_union_ids() {
        let ids: HashSet<_> = confirmed_ids(&json!({"deleted_ids":["a".repeat(64)]})).unwrap();
        assert_eq!(ids.len(), 1);
        assert!(confirmed_ids(&json!({"deleted_ids":["bad"]})).is_none());
        assert_eq!(unique_count(&["a".repeat(64)], Some(&ids)), 1);
    }

    #[test]
    fn sync_failure_preserves_confirmed_storage_diagnostics() {
        let result = storage_sync_failure(Some(&json!({
            "deleted_ids":["a".repeat(64)],
            "complete":false,
            "uncertain":true,
            "storage_restored":false,
            "usage":{"SM":{"used":1,"total":10}},
            "details":{"SM":{"before":1,"after":1,"delete_confirmed":false}},
            "error_code":"usb_sms_delete_uncertain"
        })));
        assert_eq!(result.status(), StatusCode::BAD_GATEWAY);
    }
}
