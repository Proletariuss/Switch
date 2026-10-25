//! Rust-owned projection for the module eSIM health endpoint.
//!
//! USB profiles and network status share a native physical-session fence.
//! Serial profiles use the retained native serial owner.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ServerState, json_response};

const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(60);
mod native;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    result: String,
    #[serde(default)]
    profiles: Vec<ProfileGroup>,
    #[serde(default)]
    operation: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ProfileGroup {
    #[serde(default)]
    read_state: String,
    #[serde(default)]
    profiles: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct ModuleStatus {
    #[serde(default)]
    iccid: String,
    #[serde(default)]
    imsi: String,
    #[serde(default)]
    operator: String,
    #[serde(default)]
    sim_inserted: bool,
    #[serde(default)]
    reg_status: i32,
    #[serde(default)]
    reg_status_text: String,
    #[serde(default)]
    signal_dbm: i32,
    #[serde(default)]
    network_mode: String,
}

pub(crate) async fn handle(state: &ServerState) -> Response<Full<Bytes>> {
    if crate::module_esim_transport::native(state) {
        return native::handle(state).await;
    }
    project_error("esim_unavailable")
}

fn project_status(
    active_profile: &Value,
    status: &ModuleStatus,
    operation: Option<Value>,
) -> Response<Full<Bytes>> {
    let registered = matches!(status.reg_status, 1 | 5);
    let profile_matches_module = normalize_sim_identifier(profile_iccid(&active_profile))
        == normalize_sim_identifier(&status.iccid);
    let mut health_state = "not_registered";
    if !profile_matches_module && !status.iccid.trim().is_empty() {
        health_state = "profile_mismatch";
    } else if status.sim_inserted && registered {
        health_state = "ready";
    }
    json_response(
        StatusCode::OK,
        &json!({
            "ok": status.sim_inserted && registered && profile_matches_module,
            "card_type": "euicc",
            "state": health_state,
            "active_profile": active_profile,
            "module_iccid": status.iccid,
            "profile_matches_module": profile_matches_module,
            "imsi": status.imsi,
            "operator": status.operator,
            "registration": status.reg_status_text,
            "registered": registered,
            "signal_dbm": status.signal_dbm,
            "network_mode": status.network_mode,
            "operation": operation
        }),
    )
}

fn project_profile_result(snapshot: &Snapshot) -> Option<Response<Full<Bytes>>> {
    let response = match snapshot.result.as_str() {
        "esim_unavailable" => Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_unavailable",
            "eSIM 管理服务当前不可用",
        )),
        "module_vowifi_owns_sim_channel" => Some((
            StatusCode::CONFLICT,
            "module_vowifi_owns_sim_channel",
            "模块 VoWiFi 正在使用 SIM 通道；关闭模块 VoWiFi 后再检查 eSIM Profile 状态。",
        )),
        "euicc_not_accessible" => Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "euicc_not_accessible",
            "模块当前未能访问 eUICC；确认已启用 eSIM Profile 后重试。",
        )),
        "esim_health_failed" => Some((
            StatusCode::BAD_GATEWAY,
            "esim_health_failed",
            "读取 eSIM 运行状态失败",
        )),
        "profiles_observed" => None,
        _ => Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_health_unavailable",
            "读取 eSIM 运行状态失败",
        )),
    };
    if let Some((status, code, message)) = response {
        return Some(json_response(
            status,
            &json!({"error":message,"code":code,"retryable":true,"recoverable":true}),
        ));
    }

    let (active, profile_read_failed) = first_active_profile(&snapshot.profiles);
    if active.is_none() {
        let (state, message) = if profile_read_failed {
            (
                "profile_read_error",
                "eSIM 卡片已识别，但 Profile 状态读取失败",
            )
        } else {
            (
                "no_active_profile",
                "eSIM 卡片已识别，但没有已启用的 Profile",
            )
        };
        return Some(json_response(
            StatusCode::OK,
            &json!({"ok":false,"card_type":"euicc","state":state,"message":message,"operation":snapshot.operation}),
        ));
    }
    None
}

fn project_error(result: &str) -> Response<Full<Bytes>> {
    let (status, code, message) = match result {
        "module_vowifi_owns_sim_channel" => (
            StatusCode::CONFLICT,
            "module_vowifi_owns_sim_channel",
            "模块 VoWiFi 正在使用 SIM 通道；关闭模块 VoWiFi 后再检查 eSIM Profile 状态。",
        ),
        "esim_unavailable" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_unavailable",
            "eSIM 管理服务当前不可用",
        ),
        "modem_unavailable" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "modem_unavailable",
            "蜂窝模块当前不可用",
        ),
        "modem_status_failed" => (
            StatusCode::BAD_GATEWAY,
            "modem_status_failed",
            "读取模块网络状态失败",
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_health_unavailable",
            "读取 eSIM 运行状态失败",
        ),
    };
    json_response(
        status,
        &json!({"error":message,"code":code,"retryable":true,"recoverable":true}),
    )
}

fn first_active_profile(groups: &[ProfileGroup]) -> (Option<&Value>, bool) {
    let mut profile_read_failed = false;
    for group in groups {
        if group.read_state == "error" {
            profile_read_failed = true;
        }
        if let Some(profile) = group
            .profiles
            .iter()
            .find(|profile| profile.get("state").and_then(Value::as_i64) == Some(1))
        {
            return (Some(profile), profile_read_failed);
        }
    }
    (None, profile_read_failed)
}

fn find_active_profile(groups: &[ProfileGroup]) -> Option<&Value> {
    first_active_profile(groups).0
}

fn profile_iccid(profile: &Value) -> &str {
    profile
        .get("iccid")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn normalize_sim_identifier(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_end_matches(['F', 'f'])
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        ProfileGroup, find_active_profile, first_active_profile, normalize_sim_identifier,
    };
    use serde_json::json;

    #[test]
    fn active_profile_uses_first_enabled_profile_and_stops_at_its_group() {
        let groups = vec![
            ProfileGroup {
                read_state: "error".into(),
                profiles: vec![json!({"iccid":"first","state":0})],
            },
            ProfileGroup {
                read_state: "ok".into(),
                profiles: vec![json!({"iccid":"enabled","state":1})],
            },
            ProfileGroup {
                read_state: "error".into(),
                profiles: vec![json!({"iccid":"later","state":1})],
            },
        ];
        let (profile, failed) = first_active_profile(&groups);
        assert_eq!(profile.unwrap()["iccid"], "enabled");
        assert!(failed);
        assert_eq!(find_active_profile(&groups).unwrap()["state"], 1);
    }

    #[test]
    fn profile_read_failure_is_distinct_from_an_empty_profile_list() {
        let groups = vec![ProfileGroup {
            read_state: "error".into(),
            profiles: Vec::new(),
        }];
        assert_eq!(first_active_profile(&groups), (None, true));
        assert_eq!(first_active_profile(&[]), (None, false));
    }

    #[test]
    fn iccid_normalization_matches_the_legacy_go_projection() {
        assert_eq!(normalize_sim_identifier("\"12345F\""), "12345");
        assert_eq!(normalize_sim_identifier(" 12345ff "), "12345");
    }
}
