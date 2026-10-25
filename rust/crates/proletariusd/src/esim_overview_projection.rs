//! Rust-owned public projection for the module eSIM profile overview.
//!
//! USB and serial observations use exact-session native caches.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode, Uri};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ServerState, json_response};

const SNAPSHOT_TIMEOUT: Duration = crate::esim_refresh::WAITER_TIMEOUT;
mod native;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    result: String,
    #[serde(default)]
    euicc_unavailable: bool,
    #[serde(default)]
    operation_in_progress: bool,
    switch_allowed: bool,
    modem_available: bool,
    #[serde(default)]
    overview: Option<Value>,
    #[serde(default)]
    cached: Option<Value>,
    operation: Option<Value>,
}

pub(crate) async fn handle(
    request: &hyper::Request<hyper::body::Incoming>,
    state: &ServerState,
) -> Response<Full<Bytes>> {
    if crate::module_esim_transport::native(state) {
        return native::handle(state, refresh_requested(request.uri())).await;
    }
    coded_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "esim_unavailable",
        "eSIM 管理服务当前不可用",
    )
}

pub(crate) async fn refresh_after_mutation(state: &ServerState) {
    let _ = native::handle(state, true).await;
}

fn refresh_requested(uri: &Uri) -> bool {
    let query = uri.query().unwrap_or_default();
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "refresh")
        .is_some_and(|(_, value)| value.trim().eq_ignore_ascii_case("true") || value == "1")
}

fn project(snapshot: &Snapshot) -> Response<Full<Bytes>> {
    match snapshot.result.as_str() {
        "esim_unavailable" => coded_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_unavailable",
            "eSIM 管理服务当前不可用",
        ),
        "module_vowifi_owns_radio" => snapshot
            .cached
            .as_ref()
            .filter(|cached| has_cached_profile_snapshot(cached))
            .map_or_else(
            || {
                coded_error(
                    StatusCode::CONFLICT,
                    "module_vowifi_owns_sim_channel",
                    "模块 VoWiFi 正在使用 SIM 通道；请先关闭模块 VoWiFi，再读取 eSIM Profile。",
                )
            },
            |cached| {
                overview_response(
                    cached,
                    Some("模块 VoWiFi 正在使用 SIM 通道；当前为最近一次成功读取的 Profile 快照（只读）。关闭模块 VoWiFi 后刷新可读取最新状态。"),
                    false,
                    false,
                    snapshot.operation.clone().unwrap_or(Value::Null),
                    true,
                )
            },
        ),
        "module_channel_busy" => fallback_or_error(snapshot, StatusCode::CONFLICT, "esim_operation_busy",
            "eSIM 操作正在进行，当前显示内容暂不可刷新",
            Some("模块 SIM 通道正在使用，当前展示最近一次成功读取的 Profile 快照（只读）。")),
        "refresh_operation_busy" => coded_error(
            StatusCode::CONFLICT,
            "esim_operation_busy",
            "已有 eSIM 操作正在进行，请等待完成后重试",
        ),
        "refresh_failed" => fallback_or_error(
            snapshot,
            if snapshot.euicc_unavailable {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::BAD_GATEWAY
            },
            if snapshot.euicc_unavailable {
                "euicc_not_accessible"
            } else {
                "esim_refresh_failed"
            },
            if snapshot.euicc_unavailable {
                "模块当前未能访问 eUICC（可能是实体 SIM，也可能是模块 APDU 通道暂不可用）。请确认已启用 eSIM Profile，关闭 VoWiFi 后刷新。"
            } else {
                "刷新 eSIM 卡片信息失败，请稍后重试"
            },
            Some("本次 Profile 刷新暂时失败，当前展示最近一次成功读取的快照（只读）。请检查模块连接状态后重试刷新。"),
        ),
        "overview_failed" => fallback_or_error(
            snapshot,
            if snapshot.euicc_unavailable {
                StatusCode::SERVICE_UNAVAILABLE
            } else if snapshot.operation_in_progress {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_GATEWAY
            },
            if snapshot.euicc_unavailable {
                "euicc_not_accessible"
            } else if snapshot.operation_in_progress {
                "esim_operation_busy"
            } else {
                "esim_overview_failed"
            },
            if snapshot.euicc_unavailable {
                "模块当前未能访问 eUICC（可能是实体 SIM，也可能是模块 APDU 通道暂不可用）。请确认已启用 eSIM Profile，关闭 VoWiFi 后刷新。"
            } else if snapshot.operation_in_progress {
                "eSIM 操作正在进行，当前显示内容暂不可刷新"
            } else {
                "读取 eSIM 卡片信息失败，请重新连接模块后重试"
            },
            if snapshot.operation_in_progress || snapshot.euicc_unavailable {
                None
            } else {
                Some("eSIM 接口当前不可读，以下为最近一次成功读取的 Profile 快照（只读）。")
            },
        ),
        "ready" => match snapshot.overview.as_ref() {
            Some(overview) => overview_response(
                overview,
                None,
                snapshot.switch_allowed,
                snapshot.modem_available,
                snapshot.operation.clone().unwrap_or(Value::Null),
                false,
            ),
            None => unavailable("successful eSIM observation omitted its overview"),
        },
        _ => unavailable("eSIM hardware observation returned an unknown result"),
    }
}

fn fallback_or_error(
    snapshot: &Snapshot,
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    fallback_message: Option<&'static str>,
) -> Response<Full<Bytes>> {
    if let (Some(cached), Some(fallback_message)) = (snapshot.cached.as_ref(), fallback_message)
        && has_cached_profile_snapshot(cached)
    {
        return overview_response(
            cached,
            Some(fallback_message),
            false,
            false,
            snapshot.operation.clone().unwrap_or(Value::Null),
            true,
        );
    }
    coded_error(status, code, message)
}

fn has_cached_profile_snapshot(overview: &Value) -> bool {
    overview
        .get("chip_info")
        .and_then(|chip| chip.get("eids"))
        .and_then(Value::as_array)
        .is_some_and(|eids| !eids.is_empty())
}

fn overview_response(
    overview: &Value,
    message: Option<&str>,
    switch_allowed: bool,
    modem_available: bool,
    operation: Value,
    stale: bool,
) -> Response<Full<Bytes>> {
    if !overview.is_object() {
        return unavailable("eSIM overview has an invalid shape");
    }
    let chip_info = overview.get("chip_info").cloned().unwrap_or(Value::Null);
    let profiles = overview
        .get("profiles")
        .filter(|profiles| profiles.is_array())
        .cloned()
        .unwrap_or_else(|| json!([]));
    let updated_at = overview
        .get("updated_at")
        .cloned()
        .unwrap_or_else(|| json!("0001-01-01T00:00:00Z"));
    let available = chip_info
        .get("eids")
        .and_then(Value::as_array)
        .is_some_and(|eids| !eids.is_empty());
    let serial_available = available
        && chip_info
            .get("serial_number")
            .and_then(Value::as_str)
            .is_some_and(|serial| !serial.trim().is_empty());
    let capabilities = if stale {
        json!({
            "can_refresh": false,
            "can_download": false,
            "can_switch": false,
            "can_rename": false,
            "can_delete": false,
            "can_probe_phonebook": false,
            "vendor_serial_available": false
        })
    } else {
        json!({
            "can_refresh": available,
            "can_download": available,
            "can_switch": available && (switch_allowed || modem_available),
            "can_rename": available,
            "can_delete": available,
            "can_probe_phonebook": available,
            "vendor_serial_available": serial_available
        })
    };
    let mut body = json!({
        "card_type":"euicc",
        "profiles":profiles,
        "capabilities":capabilities,
        "updated_at":updated_at,
        "operation":operation
    });
    if !chip_info.is_null() {
        body["chip_info"] = chip_info;
    }
    if let Some(message) = message {
        body["message"] = json!(message.trim());
    }
    json_response(StatusCode::OK, &body)
}

fn coded_error(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
) -> Response<Full<Bytes>> {
    json_response(
        status,
        &json!({"error":message,"code":code,"recoverable":true}),
    )
}

fn unavailable(message: &str) -> Response<Full<Bytes>> {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &json!({"error":message,"code":"esim_overview_unavailable","retryable":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::{Snapshot, has_cached_profile_snapshot, project, refresh_requested};
    use http_body_util::BodyExt;
    use hyper::StatusCode;
    use serde_json::{Value, json};

    fn snapshot(result: &str) -> Snapshot {
        Snapshot {
            result: result.into(),
            euicc_unavailable: false,
            operation_in_progress: false,
            switch_allowed: false,
            modem_available: false,
            overview: None,
            cached: None,
            operation: None,
        }
    }

    async fn body(response: hyper::Response<http_body_util::Full<bytes::Bytes>>) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn query_flag_matches_go_refresh_semantics() {
        for query in [
            "refresh=true",
            "refresh=TRUE",
            "x=1&refresh=%20true%20",
            "refresh=1",
        ] {
            let uri = format!("/api/esim?{query}").parse().unwrap();
            assert!(refresh_requested(&uri), "{query}");
        }
        for query in ["refresh=false", "refresh=0", "refresh=2", "Refresh=true"] {
            let uri = format!("/api/esim?{query}").parse().unwrap();
            assert!(!refresh_requested(&uri), "{query}");
        }
        let uri = "/api/esim".parse().unwrap();
        assert!(!refresh_requested(&uri));
    }

    #[tokio::test]
    async fn live_projection_preserves_capability_rules_and_response_shape() {
        let mut snapshot = snapshot("ready");
        snapshot.switch_allowed = true;
        snapshot.overview = Some(json!({
            "chip_info":{"eids":[{"eid":"8901"}],"serial_number":" sn "},
            "profiles":null,
            "updated_at":"2026-10-09T01:02:03Z"
        }));
        let response = project(&snapshot);
        assert_eq!(response.status(), StatusCode::OK);
        let value = body(response).await;
        assert_eq!(value["card_type"], "euicc");
        assert_eq!(value["profiles"], json!([]));
        assert_eq!(value["capabilities"]["can_refresh"], true);
        assert_eq!(value["capabilities"]["can_switch"], true);
        assert_eq!(value["capabilities"]["vendor_serial_available"], true);
        assert_eq!(value["operation"], Value::Null);
    }

    #[tokio::test]
    async fn cached_fallback_is_read_only_and_requires_a_known_eid() {
        let mut snapshot = snapshot("overview_failed");
        snapshot.cached = Some(json!({
            "chip_info":{"eids":[{"eid":"8901"}]},
            "profiles":[],
            "updated_at":"2026-10-09T01:02:03Z"
        }));
        assert!(has_cached_profile_snapshot(
            snapshot.cached.as_ref().unwrap()
        ));
        let response = project(&snapshot);
        assert_eq!(response.status(), StatusCode::OK);
        let value = body(response).await;
        assert_eq!(
            value["message"],
            "eSIM 接口当前不可读，以下为最近一次成功读取的 Profile 快照（只读）。"
        );
        assert_eq!(value["capabilities"]["can_refresh"], false);
        assert_eq!(value["capabilities"]["can_switch"], false);

        snapshot.cached = Some(json!({"chip_info":{"eids":[]},"profiles":[]}));
        assert!(!has_cached_profile_snapshot(
            snapshot.cached.as_ref().unwrap()
        ));
        assert_eq!(project(&snapshot).status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn refresh_error_prefers_last_good_snapshot_but_physical_probe_keeps_legacy_error() {
        let mut snapshot = snapshot("refresh_failed");
        snapshot.cached = Some(json!({
            "chip_info":{"eids":[{"eid":"8901"}]},
            "profiles":[],
            "updated_at":"2026-10-09T01:02:03Z"
        }));
        let response = project(&snapshot);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body(response).await["message"],
            "本次 Profile 刷新暂时失败，当前展示最近一次成功读取的快照（只读）。请检查模块连接状态后重试刷新。"
        );

        snapshot.euicc_unavailable = true;
        assert_eq!(project(&snapshot).status(), StatusCode::OK);
        snapshot.cached = None;
        assert_eq!(project(&snapshot).status(), StatusCode::SERVICE_UNAVAILABLE);

        snapshot.euicc_unavailable = false;
        snapshot.result = "overview_failed".into();
        snapshot.operation_in_progress = true;
        let response = project(&snapshot);
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(body(response).await["code"], "esim_operation_busy");
    }

    #[tokio::test]
    async fn operation_and_vowifi_conflicts_keep_their_codes() {
        let response = project(&snapshot("refresh_operation_busy"));
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(body(response).await["code"], "esim_operation_busy");

        let response = project(&snapshot("module_vowifi_owns_radio"));
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            body(response).await["code"],
            "module_vowifi_owns_sim_channel"
        );
    }
}

#[cfg(test)]
pub(crate) async fn native_fixture(state: &ServerState, refresh: bool) -> Response<Full<Bytes>> {
    native::handle(state, refresh).await
}
