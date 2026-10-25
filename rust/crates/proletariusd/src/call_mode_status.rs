//! Native call-mode inspection and readiness projection. All observations
//! come from Rust module-radio reservations, verified local artifacts, and
//! isolated Rust ADB workers; the lifecycle owns authoritative refresh state.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::module_radio_transport::{UsbComposition, VoiceConfiguration};

pub(crate) const INTERNAL_PROJECT_PATH: &str = "/api/internal/call-mode/status/project";
const MAX_REQUEST_BYTES: usize = 8 * 1024;
const MAX_DETAIL_BYTES: usize = 2 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Usb,
    UsbInterfaces,
    Voice,
    VoiceFacts,
    CallInterfaces,
    Facts,
    AdbAuthorization,
    AdbInterface,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionRequest {
    pub(crate) phase: Phase,
    pub(crate) runtime_downloaded: bool,
    pub(crate) downloaded_bytes: u64,
    pub(crate) runtime_error: String,
    #[serde(default)]
    pub(crate) usb: Option<UsbComposition>,
    #[serde(default)]
    pub(crate) usb_error: String,
    #[serde(default)]
    pub(crate) usb_unsupported: bool,
    #[serde(default)]
    pub(crate) voice: Option<VoiceConfiguration>,
    #[serde(default)]
    pub(crate) voice_error: String,
    #[serde(default)]
    pub(crate) probe_error: String,
}

#[derive(Debug, Default, Serialize)]
struct Projection {
    state: String,
    summary: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    detail: String,
    adb_enabled: bool,
    interfaces_ready: bool,
    adb_authorization_required: bool,
    adb_authorization_accepted: bool,
    uac_enabled: bool,
    voice_configured: bool,
    runtime_downloaded: bool,
    downloaded_bytes: u64,
    total_bytes: u64,
    can_enable: bool,
    can_download: bool,
    can_retry: bool,
    requires_adb_authorization_confirmation: bool,
    requires_usb_confirmation: bool,
    requires_download_confirmation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_probe: Option<&'static str>,
}

#[derive(Serialize)]
struct Failure {
    code: &'static str,
    error: &'static str,
}

pub(crate) fn valid_public_status(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let required_strings = [
        "state",
        "summary",
        "runtime_version",
        "runtime_path",
        "updated_at",
        "upstream",
    ];
    if required_strings
        .iter()
        .any(|key| !object.get(*key).is_some_and(Value::is_string))
    {
        return false;
    }
    let required_booleans = [
        "adb_enabled",
        "interfaces_ready",
        "adb_authorization_required",
        "adb_authorization_accepted",
        "uac_enabled",
        "voice_configured",
        "runtime_downloaded",
        "can_enable",
        "can_download",
        "can_retry",
        "requires_adb_authorization_confirmation",
        "requires_usb_confirmation",
        "requires_download_confirmation",
    ];
    if required_booleans
        .iter()
        .any(|key| !object.get(*key).is_some_and(Value::is_boolean))
    {
        return false;
    }
    let Some(downloaded) = object.get("downloaded_bytes").and_then(Value::as_i64) else {
        return false;
    };
    let Some(total) = object.get("total_bytes").and_then(Value::as_i64) else {
        return false;
    };
    if downloaded < 0 || total <= 0 || downloaded > total {
        return false;
    }
    if object.get("detail").is_some_and(|value| !value.is_string())
        || object.get("source").is_some_and(|value| !value.is_string())
        || object
            .get("backup_path")
            .is_some_and(|value| !value.is_string())
        || object
            .get("last_restore")
            .is_some_and(|value| !value.is_null() && !value.is_object())
    {
        return false;
    }
    let Some(sources) = object.get("sources").and_then(Value::as_array) else {
        return false;
    };
    !sources.is_empty()
        && sources.iter().all(|source| {
            source.as_object().is_some_and(|source| {
                source.get("id").is_some_and(Value::is_string)
                    && source.get("name").is_some_and(Value::is_string)
                    && source.get("detail").is_some_and(Value::is_string)
                    && source.get("trusted").is_some_and(Value::is_boolean)
            })
        })
}

pub(crate) async fn handle_public_status(
    service: crate::call_mode_lifecycle::Service,
) -> Response<Full<Bytes>> {
    crate::json_response(StatusCode::OK, &service.public_status().await)
}

/// Convert native observations with the same readiness and confirmation rules
/// as the established public DTO. Incomplete phases return the required probe.
pub(crate) fn project_native(
    input: ProjectionRequest,
) -> Result<(crate::call_mode_state::Status, Option<&'static str>), String> {
    let projection = project(input)
        .ok_or_else(|| "Rust 通话模式状态投影不可用，已停止：通话模式状态观测无效".to_owned())?;
    let status = crate::call_mode_state::Status {
        state: projection.state,
        summary: projection.summary,
        detail: projection.detail,
        adb_enabled: projection.adb_enabled,
        interfaces_ready: projection.interfaces_ready,
        adb_authorization_required: projection.adb_authorization_required,
        adb_authorization_accepted: projection.adb_authorization_accepted,
        uac_enabled: projection.uac_enabled,
        voice_configured: projection.voice_configured,
        runtime_downloaded: projection.runtime_downloaded,
        downloaded_bytes: projection.downloaded_bytes,
        total_bytes: projection.total_bytes,
        can_enable: projection.can_enable,
        can_download: projection.can_download,
        can_retry: projection.can_retry,
        requires_adb_authorization_confirmation: projection.requires_adb_authorization_confirmation,
        requires_usb_confirmation: projection.requires_usb_confirmation,
        requires_download_confirmation: projection.requires_download_confirmation,
        ..crate::call_mode_state::Status::default()
    };
    Ok((status, projection.next_probe))
}

/// Inspect under the coordinator's native reservation, so every USB, voice,
/// authorization and ADB observation belongs to the same physical operation.
pub(crate) async fn inspect_with_reservation(
    data_root: &std::path::Path,
    radio: &crate::module_radio::Service,
    reservation: &crate::module_radio::CallModeReservation,
    producer_id: &str,
    inbox: crate::message_archive::InboxState,
) -> crate::call_mode_state::Status {
    use crate::module_radio_transport::Action;
    let verification = crate::call_mode_runtime::installed_runtime_verification(data_root).await;
    let mut input = ProjectionRequest {
        phase: Phase::Usb,
        runtime_downloaded: verification.runtime_downloaded,
        downloaded_bytes: 0,
        runtime_error: verification.error,
        usb: None,
        usb_error: String::new(),
        usb_unsupported: false,
        voice: None,
        voice_error: String::new(),
        probe_error: String::new(),
    };
    let usb = radio
        .call_mode_action(reservation, Action::CallModeUsbCompositionRead)
        .await;
    match usb {
        Ok(outcome) if outcome.verified && outcome.usb_composition.is_some() => {
            input.usb = outcome.usb_composition;
            input.downloaded_bytes = verification.downloaded_bytes;
        }
        Ok(outcome) => {
            input.usb_unsupported = matches!(
                outcome.error_code.as_str(),
                "call_mode_usb_parse_failed" | "call_mode_usb_query_rejected"
            );
            input.usb_error = if outcome.response.trim().is_empty() {
                "Rust USBCFG 查询未返回已验证配置及通话目标".into()
            } else {
                outcome.response
            };
            return terminal_projection(input);
        }
        Err(error) => {
            input.usb_unsupported = matches!(
                error.code,
                "call_mode_usb_parse_failed"
                    | "call_mode_usb_query_rejected"
                    | "module_radio_outcome_unconfirmed"
            );
            input.usb_error = format!("读取模块 USB 配置：{}", error.message);
            return terminal_projection(input);
        }
    }
    input.phase = Phase::Voice;
    match radio
        .call_mode_action(reservation, Action::CallModeVoiceRead)
        .await
    {
        Ok(outcome) if outcome.verified && outcome.voice.is_some() => input.voice = outcome.voice,
        Ok(outcome) => {
            input.voice_error = if outcome.response.trim().is_empty() {
                "Rust IMS/VoLTE 查询未返回已验证状态".into()
            } else {
                outcome.response
            };
            return terminal_projection(input);
        }
        Err(error) => {
            input.voice_error = format!("读取 IMS/VoLTE 配置：{}", error.message);
            return terminal_projection(input);
        }
    }
    input.phase = Phase::Facts;
    let (status, probe) = match project_native(input.clone()) {
        Ok(result) => result,
        Err(error) => return projection_failure(error),
    };
    match probe {
        Some("adb_authorization") => {
            input.phase = Phase::AdbAuthorization;
            match radio
                .call_mode_action(reservation, Action::CallModeAdbProbe)
                .await
            {
                Ok(outcome)
                    if outcome.verified && outcome.adb_authorization_supported == Some(true) => {}
                Ok(outcome) => {
                    input.probe_error = if outcome.response.trim().is_empty() {
                        "Rust ADB 授权探测未返回已验证支持状态".into()
                    } else {
                        outcome.response
                    }
                }
                Err(error) => input.probe_error = error.message,
            }
        }
        Some("adb_interface") => {
            input.phase = Phase::AdbInterface;
            if let Err(error) = crate::call_mode_adb::probe_module_adb(
                radio.module_recovery(),
                data_root,
                producer_id,
                inbox,
            )
            .await
            {
                input.probe_error = error.message;
            }
        }
        None => return status,
        Some(_) => return projection_failure("Rust 状态投影返回未知的 ADB 探测步骤".into()),
    }
    terminal_projection(input)
}
fn terminal_projection(input: ProjectionRequest) -> crate::call_mode_state::Status {
    match project_native(input) {
        Ok((status, None)) => status,
        Ok((_, Some(_))) => projection_failure("Rust 状态投影在 ADB 探测后仍要求继续探测".into()),
        Err(error) => projection_failure(error),
    }
}
pub(crate) fn projection_failure(error: String) -> crate::call_mode_state::Status {
    let mut status = crate::call_mode_state::Status::new("unsupported", "无法确认模块通话模式状态");
    status.detail = error;
    status.can_retry = true;
    status
}

fn response<T: Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

fn bad_request() -> Response<Full<Bytes>> {
    response(
        StatusCode::BAD_REQUEST,
        &Failure {
            code: "invalid_call_mode_projection",
            error: "通话模式状态观测无效",
        },
    )
}

fn sanitize_detail(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let mut clean: String = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if clean.trim().is_empty() {
        return None;
    }
    if clean.len() > MAX_DETAIL_BYTES {
        let mut boundary = MAX_DETAIL_BYTES - '…'.len_utf8();
        while !clean.is_char_boundary(boundary) {
            boundary -= 1;
        }
        clean.truncate(boundary);
        clean.push('…');
    }
    Some(clean)
}

fn total_bytes() -> u64 {
    crate::call_mode_runtime_manifest::manifest()
        .files
        .iter()
        .map(|artifact| artifact.size)
        .sum()
}

fn base(input: &ProjectionRequest) -> Projection {
    Projection {
        runtime_downloaded: input.runtime_downloaded,
        downloaded_bytes: input.downloaded_bytes,
        total_bytes: total_bytes(),
        ..Projection::default()
    }
}

fn validate_facts(input: &ProjectionRequest) -> bool {
    input.usb.is_some_and(UsbComposition::valid)
        && input.voice.is_some_and(VoiceConfiguration::valid)
        && input.usb_error.is_empty()
        && input.voice_error.is_empty()
}

fn project(input: ProjectionRequest) -> Option<Projection> {
    let mut status = base(&input);
    if input.phase == Phase::UsbInterfaces {
        if input.runtime_downloaded
            || input.downloaded_bytes != 0
            || !input.runtime_error.is_empty()
            || !input.usb_error.is_empty()
            || input.usb_unsupported
            || input.voice.is_some()
            || !input.voice_error.is_empty()
            || !input.probe_error.is_empty()
        {
            return None;
        }
        let usb = input.usb?;
        if !usb.valid() {
            return None;
        }
        status.adb_enabled = usb.flags[5] == 1;
        status.uac_enabled = usb.flags[6] == 1;
        status.interfaces_ready = status.adb_enabled && status.uac_enabled;
        status.state = if status.interfaces_ready {
            "usb_ready"
        } else {
            "needs_usb"
        }
        .into();
        status.summary = if status.interfaces_ready {
            "模块 ADB/UAC 接口已就绪"
        } else {
            "模块 ADB/UAC 接口尚未就绪"
        }
        .into();
        return Some(status);
    }
    if input.phase == Phase::CallInterfaces {
        if input.runtime_downloaded
            || input.downloaded_bytes != 0
            || !input.runtime_error.is_empty()
            || !input.usb_error.is_empty()
            || input.usb_unsupported
            || !input.voice_error.is_empty()
            || !input.probe_error.is_empty()
        {
            return None;
        }
        let usb = input.usb?;
        let voice = input.voice?;
        if !usb.valid() || !voice.valid() {
            return None;
        }
        status.adb_enabled = usb.flags[5] == 1;
        status.uac_enabled = usb.flags[6] == 1;
        status.voice_configured =
            voice.ims == 1 && voice.volte_capability == 1 && voice.volte_disabled == 0;
        status.interfaces_ready =
            status.adb_enabled && status.uac_enabled && status.voice_configured;
        status.state = if status.interfaces_ready {
            "ready"
        } else if !status.adb_enabled || !status.uac_enabled {
            "needs_usb"
        } else {
            "needs_voice"
        }
        .into();
        status.summary = match status.state.as_str() {
            "ready" => "模块通话接口已就绪",
            "needs_usb" => "模块 ADB/UAC 接口尚未就绪",
            _ => "模块 IMS/VoLTE 尚未就绪",
        }
        .into();
        return Some(status);
    }
    if input.phase == Phase::VoiceFacts {
        if input.runtime_downloaded
            || input.downloaded_bytes != 0
            || !input.runtime_error.is_empty()
            || input.usb.is_some()
            || !input.usb_error.is_empty()
            || input.usb_unsupported
            || !input.voice_error.is_empty()
            || !input.probe_error.is_empty()
        {
            return None;
        }
        let voice = input.voice?;
        if !voice.valid() {
            return None;
        }
        status.voice_configured =
            voice.ims == 1 && voice.volte_capability == 1 && voice.volte_disabled == 0;
        status.state = if status.voice_configured {
            "ready"
        } else {
            "needs_voice"
        }
        .into();
        status.summary = if status.voice_configured {
            "模块 IMS/VoLTE 已就绪"
        } else {
            "模块 IMS/VoLTE 尚未就绪"
        }
        .into();
        return Some(status);
    }

    let total_bytes = total_bytes();
    if input.downloaded_bytes > total_bytes
        || (input.runtime_downloaded
            && input.downloaded_bytes != total_bytes
            && !(input.phase == Phase::Usb && input.downloaded_bytes == 0))
        || (!input.runtime_downloaded && input.runtime_error.is_empty())
    {
        return None;
    }

    match input.phase {
        Phase::Usb => {
            if input.usb.is_some()
                || input.usb_error.is_empty()
                || input.voice.is_some()
                || !input.voice_error.is_empty()
                || !input.probe_error.is_empty()
            {
                return None;
            }
            status.state = if input.usb_unsupported {
                "unsupported"
            } else {
                "disconnected"
            }
            .into();
            status.summary = if input.usb_unsupported {
                "无法识别模块 USB 配置"
            } else {
                "等待 4G 模块连接"
            }
            .into();
            status.detail = sanitize_detail(&input.usb_error)?;
            status.can_retry = true;
            return Some(status);
        }
        Phase::Voice => {
            let usb = input.usb?;
            if !usb.valid() || input.usb_unsupported || !input.usb_error.is_empty() {
                return None;
            }
            if input.voice.is_some() || input.voice_error.is_empty() {
                return None;
            }
            if !input.probe_error.is_empty() {
                return None;
            }
            status.state = "unsupported".into();
            status.summary = "无法确认模块 IMS/VoLTE 配置".into();
            status.detail = sanitize_detail(&input.voice_error)?;
            status.adb_enabled = usb.flags[5] == 1;
            status.uac_enabled = usb.flags[6] == 1;
            status.downloaded_bytes = input.downloaded_bytes;
            status.can_retry = true;
            return Some(status);
        }
        Phase::VoiceFacts => unreachable!("handled before general readiness validation"),
        Phase::UsbInterfaces | Phase::CallInterfaces => {
            unreachable!("handled before general readiness validation")
        }
        Phase::Facts | Phase::AdbAuthorization | Phase::AdbInterface => {
            if !validate_facts(&input) {
                return None;
            }
            if input.usb_unsupported
                || !input.voice_error.is_empty()
                || (input.phase == Phase::Facts && !input.probe_error.is_empty())
            {
                return None;
            }
            let usb = input.usb?;
            let voice = input.voice?;
            status.adb_enabled = usb.flags[5] == 1;
            status.uac_enabled = usb.flags[6] == 1;
            status.voice_configured =
                voice.ims == 1 && voice.volte_capability == 1 && voice.volte_disabled == 0;
            status.downloaded_bytes = input.downloaded_bytes;

            if !status.adb_enabled {
                match input.phase {
                    Phase::Facts => status.next_probe = Some("adb_authorization"),
                    Phase::AdbAuthorization if input.probe_error.is_empty() => {
                        status.state = "needs_adb_authorization".into();
                        status.summary = "需要持久授权并开启模块 ADB".into();
                        status.detail = "授权密码只在本机计算，不会上传或保存。USB 配置备份可以关闭 ADB 接口，但不能保证撤销模块的持久授权。".into();
                        status.adb_authorization_required = true;
                        status.can_enable = true;
                        status.requires_adb_authorization_confirmation = true;
                        status.requires_usb_confirmation = true;
                    }
                    Phase::AdbAuthorization => {
                        status.state = "unsupported".into();
                        status.summary = "无法确认模块 ADB 授权方式".into();
                        status.detail = format!(
                            "{}；为避免在未知安全状态下修改 USB 配置，已停止。",
                            sanitize_detail(&input.probe_error)?
                        );
                        status.can_retry = true;
                    }
                    _ => return None,
                }
                return Some(status);
            }

            if !status.uac_enabled {
                if input.phase != Phase::Facts {
                    return None;
                }
                status.state = "needs_usb".into();
                status.summary = "需要开启模块 USB 音频".into();
                status.detail = "会保留当前 VID/PID 和其他 USB 功能位，只开启缺少的 UAC 功能；模块随后会重启并短暂断网。".into();
                status.can_enable = true;
                status.requires_usb_confirmation = true;
                return Some(status);
            }

            if !status.voice_configured {
                if input.phase != Phase::Facts {
                    return None;
                }
                status.state = "needs_voice".into();
                status.summary = "需要启用模块 IMS/VoLTE".into();
                status.detail = format!(
                    "当前 IMS={}、VoLTE capability={}、volte_disabled={}；完成后会回读并重启模块。",
                    voice.ims, voice.volte_capability, voice.volte_disabled
                );
                status.can_enable = true;
                status.requires_usb_confirmation = true;
                return Some(status);
            }

            match input.phase {
                Phase::Facts => status.next_probe = Some("adb_interface"),
                Phase::AdbInterface if !input.probe_error.is_empty() => {
                    status.state = "needs_interface_recovery".into();
                    status.summary = "ADB 接口已配置但没有响应".into();
                    status.detail = format!(
                        "{}；可重新提交本机计算的 ADB 授权、受控重启模块并验证，不会扩大 USB 配置。",
                        sanitize_detail(&input.probe_error)?
                    );
                    status.adb_authorization_required = true;
                    status.can_enable = true;
                    status.can_retry = true;
                    status.requires_adb_authorization_confirmation = true;
                    status.requires_usb_confirmation = true;
                }
                Phase::AdbInterface if input.probe_error.is_empty() => {
                    status.interfaces_ready = true;
                    if !input.runtime_downloaded {
                        status.state = "needs_download".into();
                        status.summary = "需要下载模块侧语音运行时".into();
                        status.detail = sanitize_detail(&input.runtime_error)?;
                        status.can_download = true;
                        status.requires_download_confirmation = true;
                    } else {
                        status.state = "ready".into();
                        status.summary = "通话模式已就绪".into();
                    }
                }
                _ => return None,
            }
            Some(status)
        }
    }
}

pub(crate) async fn handle_internal(request: Request<Incoming>) -> Response<Full<Bytes>> {
    if request.method() != Method::POST
        || request.uri().path() != INTERNAL_PROJECT_PATH
        || request.uri().query().is_some()
    {
        return response(
            StatusCode::NOT_FOUND,
            &Failure {
                code: "route_not_found",
                error: "route not found",
            },
        );
    }
    let bytes = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        MAX_REQUEST_BYTES,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(bytes) => bytes,
        _ => return bad_request(),
    };
    let input = match serde_json::from_slice::<ProjectionRequest>(&bytes) {
        Ok(input) => input,
        Err(_) => return bad_request(),
    };
    match project(input) {
        Some(projection) => response(StatusCode::OK, &projection),
        None => bad_request(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn public_status_fixture() -> Value {
        json!({
            "state":"enabling_usb",
            "summary":"正在读取并备份模块 USB 配置",
            "adb_enabled":false,
            "interfaces_ready":false,
            "adb_authorization_required":false,
            "adb_authorization_accepted":false,
            "uac_enabled":false,
            "voice_configured":false,
            "runtime_downloaded":false,
            "runtime_version":"1.0.0",
            "runtime_path":"/tmp/runtime",
            "downloaded_bytes":0,
            "total_bytes":100,
            "can_enable":false,
            "can_download":false,
            "can_retry":false,
            "requires_adb_authorization_confirmation":false,
            "requires_usb_confirmation":false,
            "requires_download_confirmation":false,
            "updated_at":"2026-10-09T00:00:00Z",
            "sources":[{"id":"github","name":"GitHub 原始源","detail":"固定提交","trusted":true}],
            "upstream":"https://example.invalid/runtime"
        })
    }

    #[test]
    fn public_status_schema_requires_complete_and_bounded_runtime_projection() {
        let value = public_status_fixture();
        assert!(valid_public_status(&value));

        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove("sources");
        assert!(!valid_public_status(&missing));

        let mut unbounded = value.clone();
        unbounded["downloaded_bytes"] = json!(101);
        assert!(!valid_public_status(&unbounded));

        let mut wrong_type = value;
        wrong_type["can_enable"] = json!("false");
        assert!(!valid_public_status(&wrong_type));
    }

    fn request(phase: Phase) -> ProjectionRequest {
        ProjectionRequest {
            phase,
            runtime_downloaded: true,
            downloaded_bytes: total_bytes(),
            runtime_error: String::new(),
            usb: Some(UsbComposition {
                vendor_id: 0x2c7c,
                product_id: 0x0800,
                flags: [0, 0, 0, 0, 0, 1, 1],
            }),
            usb_error: String::new(),
            usb_unsupported: false,
            voice: Some(VoiceConfiguration {
                ims: 1,
                volte_capability: 1,
                volte_disabled: 0,
            }),
            voice_error: String::new(),
            probe_error: String::new(),
        }
    }

    #[test]
    fn native_projection_populates_authoritative_dto_without_adapter_round_trip() {
        let (status, probe) = project_native(request(Phase::AdbInterface)).unwrap();
        assert_eq!(probe, None);
        assert_eq!(status.state, "ready");
        assert!(status.interfaces_ready && status.runtime_downloaded);
        let state = crate::call_mode_state::State::new(std::path::Path::new("/fixture"));
        let (revision, _) = state.observation();
        assert!(state.publish_observation(revision, status));
        assert!(valid_public_status(
            &serde_json::to_value(state.snapshot()).unwrap()
        ));
    }

    #[test]
    fn call_mode_state_machine_preserves_action_gates_and_runtime_readiness() {
        let mut facts = request(Phase::Facts);
        let pending = project(facts).unwrap();
        assert_eq!(pending.next_probe, Some("adb_interface"));
        assert!(!pending.interfaces_ready);

        let ready = project(request(Phase::AdbInterface)).unwrap();
        assert_eq!(ready.state, "ready");
        assert!(ready.interfaces_ready);
        assert!(!ready.can_enable && !ready.can_download);

        facts = request(Phase::AdbInterface);
        facts.runtime_downloaded = false;
        facts.downloaded_bytes = 123;
        facts.runtime_error = "本地语音运行时未通过完整校验".into();
        let download = project(facts).unwrap();
        assert_eq!(download.state, "needs_download");
        assert!(download.can_download && download.requires_download_confirmation);
        assert_eq!(download.downloaded_bytes, 123);
    }

    #[test]
    fn voice_facts_projection_owns_readiness_and_rejects_unrelated_facts() {
        let mut input = request(Phase::VoiceFacts);
        input.usb = None;
        input.runtime_downloaded = false;
        input.downloaded_bytes = 0;
        input.runtime_error.clear();

        let ready = project(input.clone()).unwrap();
        assert!(ready.voice_configured);
        assert_eq!(ready.state, "ready");

        for voice in [
            VoiceConfiguration {
                ims: 0,
                volte_capability: 1,
                volte_disabled: 0,
            },
            VoiceConfiguration {
                ims: 1,
                volte_capability: 0,
                volte_disabled: 0,
            },
            VoiceConfiguration {
                ims: 1,
                volte_capability: 1,
                volte_disabled: 1,
            },
        ] {
            let mut not_ready = input.clone();
            not_ready.voice = Some(voice);
            let result = project(not_ready).unwrap();
            assert!(!result.voice_configured);
            assert_eq!(result.state, "needs_voice");
        }

        let mut invalid = input.clone();
        invalid.voice.as_mut().unwrap().ims = 3;
        assert!(project(invalid).is_none());

        let mut unrelated = input.clone();
        unrelated.usb = request(Phase::Facts).usb;
        assert!(project(unrelated).is_none());

        let mut unrelated_runtime = input.clone();
        unrelated_runtime.runtime_downloaded = true;
        assert!(project(unrelated_runtime).is_none());

        let mut unrelated_progress = input.clone();
        unrelated_progress.downloaded_bytes = 1;
        assert!(project(unrelated_progress).is_none());

        let mut unrelated_error = input.clone();
        unrelated_error.runtime_error = "unexpected runtime fact".into();
        assert!(project(unrelated_error).is_none());

        let mut unrelated_probe = input.clone();
        unrelated_probe.probe_error = "unexpected probe fact".into();
        assert!(project(unrelated_probe).is_none());
    }

    #[test]
    fn usb_interfaces_projection_owns_adb_uac_gate_and_rejects_other_facts() {
        let mut input = request(Phase::UsbInterfaces);
        input.runtime_downloaded = false;
        input.downloaded_bytes = 0;
        input.runtime_error.clear();
        input.voice = None;
        input.usb_error.clear();

        let ready = project(input.clone()).unwrap();
        assert!(ready.adb_enabled && ready.uac_enabled && ready.interfaces_ready);
        assert_eq!(ready.state, "usb_ready");

        for index in [5, 6] {
            let mut missing = input.clone();
            missing.usb.as_mut().unwrap().flags[index] = 0;
            let result = project(missing).unwrap();
            assert!(!result.interfaces_ready);
            assert_eq!(result.state, "needs_usb");
        }

        let mut unrelated_voice = input.clone();
        unrelated_voice.voice = request(Phase::Facts).voice;
        assert!(project(unrelated_voice).is_none());

        let mut unrelated_runtime = input;
        unrelated_runtime.downloaded_bytes = 1;
        assert!(project(unrelated_runtime).is_none());
    }

    #[test]
    fn call_interfaces_projection_combines_usb_and_voice_readiness() {
        let mut input = request(Phase::CallInterfaces);
        input.runtime_downloaded = false;
        input.downloaded_bytes = 0;
        input.runtime_error.clear();

        let ready = project(input.clone()).unwrap();
        assert!(ready.adb_enabled && ready.uac_enabled && ready.voice_configured);
        assert!(ready.interfaces_ready);
        assert_eq!(ready.state, "ready");

        let mut missing_adb = input.clone();
        missing_adb.usb.as_mut().unwrap().flags[5] = 0;
        let result = project(missing_adb).unwrap();
        assert!(!result.interfaces_ready);
        assert_eq!(result.state, "needs_usb");

        let mut missing_uac = input.clone();
        missing_uac.usb.as_mut().unwrap().flags[6] = 0;
        assert_eq!(project(missing_uac).unwrap().state, "needs_usb");

        let mut missing_voice = input.clone();
        missing_voice.voice.as_mut().unwrap().ims = 0;
        let result = project(missing_voice).unwrap();
        assert!(!result.interfaces_ready);
        assert_eq!(result.state, "needs_voice");

        let mut unrelated_runtime = input;
        unrelated_runtime.runtime_downloaded = true;
        assert!(project(unrelated_runtime).is_none());
    }

    #[test]
    fn missing_interfaces_voice_and_adb_authorization_keep_legacy_states() {
        let mut input = request(Phase::Facts);
        input.usb.as_mut().unwrap().flags[6] = 0;
        let usb = project(input).unwrap();
        assert_eq!(usb.state, "needs_usb");
        assert!(usb.can_enable && usb.requires_usb_confirmation);

        let mut input = request(Phase::Facts);
        input.voice.as_mut().unwrap().ims = 2;
        let voice = project(input).unwrap();
        assert_eq!(voice.state, "needs_voice");
        assert!(voice.can_enable && voice.requires_usb_confirmation);
        assert!(voice.detail.contains("IMS=2"));

        let mut input = request(Phase::Facts);
        input.usb.as_mut().unwrap().flags[5] = 0;
        let probe = project(input.clone()).unwrap();
        assert_eq!(probe.next_probe, Some("adb_authorization"));

        input.phase = Phase::AdbAuthorization;
        let auth = project(input.clone()).unwrap();
        assert_eq!(auth.state, "needs_adb_authorization");
        assert!(auth.adb_authorization_required);
        assert!(auth.requires_adb_authorization_confirmation && auth.requires_usb_confirmation);

        input.probe_error = "ADB 挑战格式不受支持".into();
        let unsupported = project(input).unwrap();
        assert_eq!(unsupported.state, "unsupported");
        assert!(unsupported.can_retry);
        assert!(unsupported.detail.contains("已停止"));
    }

    #[test]
    fn observation_failures_keep_disconnect_and_recovery_semantics() {
        let mut input = request(Phase::Usb);
        input.usb = None;
        input.voice = None;
        input.downloaded_bytes = 0;
        input.usb_error = "DJI 模块未连接".into();
        input.runtime_downloaded = false;
        input.runtime_error = "本地运行时未校验".into();
        let disconnected = project(input.clone()).unwrap();
        assert_eq!(disconnected.state, "disconnected");
        assert!(disconnected.can_retry);

        input.usb_unsupported = true;
        assert_eq!(project(input).unwrap().state, "unsupported");

        let mut input = request(Phase::Voice);
        input.voice = None;
        input.voice_error = "IMS 查询失败".into();
        let voice = project(input).unwrap();
        assert_eq!(voice.state, "unsupported");
        assert!(voice.adb_enabled && voice.uac_enabled && voice.can_retry);

        let mut input = request(Phase::AdbInterface);
        input.probe_error = "ADB root shell 不可用".into();
        let recovery = project(input).unwrap();
        assert_eq!(recovery.state, "needs_interface_recovery");
        assert!(recovery.can_enable && recovery.can_retry);
        assert!(recovery.requires_adb_authorization_confirmation);
    }

    #[test]
    fn malformed_or_inconsistent_observations_fail_closed() {
        let mut input = request(Phase::AdbInterface);
        input.downloaded_bytes = total_bytes() + 1;
        assert!(project(input).is_none());

        let mut input = request(Phase::Facts);
        input.usb.as_mut().unwrap().flags[0] = 2;
        assert!(project(input).is_none());

        let input = request(Phase::AdbAuthorization);
        assert!(project(input).is_none());

        let mut input = request(Phase::Usb);
        input.usb = None;
        input.usb_error = "\n".into();
        assert!(project(input).is_none());

        let detail = sanitize_detail(&"状态异常".repeat(900)).unwrap();
        assert!(detail.len() <= MAX_DETAIL_BYTES);
        assert!(detail.ends_with('…'));
    }
}
