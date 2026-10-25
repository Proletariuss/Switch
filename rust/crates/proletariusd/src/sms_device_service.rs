//! Rust owns SMS device readiness projection and route selection.
//!
//! Native reader/IMS observations come from retained session owners; cellular
//! USB facts use the exact Rust physical owner's cache without issuing AT.
//! Serial facts come from the Rust actor; unconfigured transports are unavailable.
//! The adapter does not synthesize readiness or
//! choose an SMS route; actual message submission remains in its existing
//! hardware/session executor.

use std::time::Duration;

use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode, Uri};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;

mod native_serial;
mod native_usb;

#[cfg(test)]
mod reader_authority_tests;

#[cfg(test)]
const FACTS_PATH: &str = "/api/internal/sms/devices/facts";
pub(crate) const SELECT_PATH: &str = "/api/internal/sms/devices/select";
const MAX_BODY_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct DeviceFacts {
    module: ModuleFacts,
    reader: ReaderFacts,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ModuleFacts {
    /// A captured native IMS capability already proves its SIM SMS route.
    /// This is never accepted from the compatibility wire or used for sending.
    #[serde(skip)]
    native_route_ready: Option<bool>,
    #[serde(skip)]
    native_observation_unavailable: bool,
    modem_present: bool,
    usb_at_present: bool,
    usb_device_present: bool,
    session_active: bool,
    session_info_present: bool,
    ims_registered: bool,
    messaging_ready: bool,
    smsc: String,
    target: String,
    phone: String,
    session_phone: String,
    subscription_id: String,
}

#[derive(Clone, Debug, Default, Serialize)]
struct ReaderFacts {
    connected: bool,
    session_active: bool,
    identity_valid: bool,
    ims_registered: bool,
    sms_ready: bool,
    registered_phone: String,
    sim_phone: String,
    saved_phone: String,
    subscription_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionRequest {
    requested: String,
    facts: CompatibilityFacts,
}

/// The compatibility process is not an observer of reader identity or IMS.
/// Discard an old peer's reader field without decoding it into native facts.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityFacts {
    module: ModuleFacts,
    #[serde(
        default,
        rename = "reader",
        deserialize_with = "discard_retired_reader"
    )]
    _retired_reader: (),
}

fn discard_retired_reader<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<(), D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer).map(|_| ())
}

#[derive(Clone, Debug, Serialize)]
struct DeviceList {
    devices: Vec<DeviceStatus>,
}

#[derive(Clone, Debug, Serialize)]
struct DeviceStatus {
    id: &'static str,
    name: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    phone: Option<String>,
    available: bool,
    ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subscription_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct SelectionError {
    status: u16,
    code: &'static str,
    message: String,
    recoverable: bool,
}

#[derive(Serialize)]
struct SelectionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    selected: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<SelectionError>,
}

#[derive(Serialize)]
struct PublicFailure {
    error: &'static str,
    code: &'static str,
    retryable: bool,
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

fn module_facts_valid(module: &ModuleFacts) -> bool {
    module.smsc.len() <= 1024
        && module.target.len() <= 1024
        && module.phone.len() <= 128
        && module.session_phone.len() <= 128
        && module.subscription_id.len() <= 256
}

fn facts_valid(facts: &DeviceFacts) -> bool {
    let reader = &facts.reader;
    module_facts_valid(&facts.module)
        && reader.registered_phone.len() <= 128
        && reader.sim_phone.len() <= 128
        && reader.saved_phone.len() <= 128
        && reader.subscription_id.len() <= 256
}

fn clean(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn usable_reader_phone(value: &str) -> Option<String> {
    let value = clean(value)?;
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "-" | "unknown" | "unknown number" | "n/a" | "号码未知" | "未知号码"
    ) {
        return None;
    }
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.chars().all(|digit| digit == '0') {
        return None;
    }
    Some(value)
}

fn display_name(phone: Option<&str>, suffix: &str) -> String {
    match phone.filter(|value| !value.is_empty()) {
        Some(phone) => format!("{phone}_{suffix}"),
        None => format!("未知号码_{suffix}"),
    }
}

fn project(facts: &DeviceFacts) -> Option<DeviceList> {
    if !facts_valid(facts) {
        return None;
    }

    let module = &facts.module;
    let module_phone = clean(if module.session_active && module.session_info_present {
        &module.session_phone
    } else {
        &module.phone
    });
    let module_available =
        module.modem_present || module.usb_at_present || module.usb_device_present;
    let module_ims_ready = module.session_active && module.ims_registered && module.messaging_ready;
    let module_route_ready = module
        .native_route_ready
        .unwrap_or_else(|| !module.smsc.trim().is_empty() && !module.target.trim().is_empty());
    // Outside a module VoWiFi session, a discovered modem or active USB AT
    // owner is the established direct-SMS route. During VoWiFi, use only the
    // registered IMS messaging path and its SIM-provided route.
    let module_ready = if module.session_active {
        module_ims_ready && module_route_ready
    } else {
        module
            .native_route_ready
            .unwrap_or(module.modem_present || module.usb_at_present)
    };
    let module_route_missing = !module_ready && module_ims_ready && !module_route_ready;
    let module_detail = if module_route_missing {
        Some("IMS 已注册，但当前 SIM 未提供可用 SMSC/IMS 短信目标")
    } else if module.native_observation_unavailable {
        Some("DJI 模块状态暂不可读取")
    } else if !module_available {
        Some("未检测到 DJI 模块")
    } else if module.session_active && !module_ready {
        Some("DJI 模块 VoWiFi 短信通道尚未就绪")
    } else if !module_ready {
        Some("DJI 模块短信通道正在连接")
    } else {
        None
    };
    let dji = DeviceStatus {
        id: "dji",
        kind: "dji",
        name: display_name(module_phone.as_deref(), "DJI"),
        phone: module_phone,
        available: module_available,
        ready: module_ready,
        detail: module_detail,
        subscription_id: (module.session_active)
            .then(|| clean(&module.subscription_id))
            .flatten(),
    };

    let reader = &facts.reader;
    let registered_phone = usable_reader_phone(&reader.registered_phone);
    let sim_phone = usable_reader_phone(&reader.sim_phone);
    let saved_phone = usable_reader_phone(&reader.saved_phone);
    let reader_phone = registered_phone.or(sim_phone).or(saved_phone);
    let reader_ready = reader.connected
        && reader.session_active
        && reader.identity_valid
        && reader.ims_registered
        && reader.sms_ready;
    let identity_changed = reader.session_active && !reader.identity_valid;
    let reader_detail = if !reader.connected {
        Some("读卡器未连接")
    } else if identity_changed {
        Some("读卡器或号卡身份已变化，请重新连接短信通道")
    } else if !reader.ims_registered {
        Some("读卡器 IMS 尚未注册")
    } else if !reader.sms_ready {
        Some("读卡器 IMS 短信通道尚未就绪")
    } else {
        None
    };
    let reader = DeviceStatus {
        id: "reader",
        kind: "reader",
        name: display_name(reader_phone.as_deref(), "Reader"),
        phone: reader_phone,
        available: reader.connected,
        ready: reader_ready,
        detail: reader_detail,
        subscription_id: (reader.session_active && reader.identity_valid)
            .then(|| clean(&reader.subscription_id))
            .flatten(),
    };

    Some(DeviceList {
        devices: vec![dji, reader],
    })
}

fn select(devices: &[DeviceStatus], requested: &str) -> Result<&'static str, SelectionError> {
    let requested = requested.trim().to_ascii_lowercase();
    let known = devices.iter().find(|device| device.id == requested);
    let ready: Vec<&DeviceStatus> = devices.iter().filter(|device| device.ready).collect();

    if !requested.is_empty() {
        let Some(device) = known else {
            return Err(SelectionError {
                status: 400,
                code: "invalid_sms_device",
                message: format!("未知短信发送设备：{requested}"),
                recoverable: false,
            });
        };
        if !device.ready {
            let message = match device.detail {
                Some(detail) => format!("{}：{detail}", device.name),
                None => format!("{}短信通道尚未就绪", device.name),
            };
            return Err(SelectionError {
                status: 503,
                code: "sms_device_not_ready",
                message,
                recoverable: true,
            });
        }
        return Ok(device.id);
    }

    match ready.as_slice() {
        [] => Err(SelectionError {
            status: 503,
            code: "sms_device_unavailable",
            message: "当前没有可用的短信发送设备".into(),
            recoverable: true,
        }),
        [device] => Ok(device.id),
        _ => Err(SelectionError {
            status: 409,
            code: "sms_device_required",
            message: "DJI 模块和读卡器 VoWiFi 均可发送，请先选择发送设备".into(),
            recoverable: false,
        }),
    }
}

fn is_selection_route(method: &Method, uri: &Uri) -> bool {
    *method == Method::POST && uri.path() == SELECT_PATH && uri.query().is_none()
}

fn decode_selection_input(bytes: &[u8]) -> Result<SelectionRequest, ()> {
    let input = serde_json::from_slice::<SelectionRequest>(bytes).map_err(|_| ())?;
    if input.requested.len() > 128 || !module_facts_valid(&input.facts.module) {
        return Err(());
    }
    Ok(input)
}

pub(crate) async fn handle_public(
    _request: Request<Incoming>,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    let module = if state.availability.module_recovery().is_quarantined() {
        ModuleFacts::default()
    } else {
        native_usb::capture(state).await.unwrap_or_default()
    };
    merged_facts_response(Ok(module), capture_reader(state), None)
}

fn capture_reader(state: &crate::ServerState) -> ReaderFacts {
    let Ok(lifecycle) = state.reader_lifecycle() else {
        return ReaderFacts::default();
    };
    let snapshot = lifecycle.snapshot();
    let iccid = snapshot
        .usim_info
        .as_ref()
        .and_then(|info| info["iccid"].as_str())
        .unwrap_or_default();
    let saved_phone = crate::profile_notes::Store::new(state.multistandby_store.data_root())
        .phone_for_iccid_or_empty(iccid);
    // Notes I/O may overlap a session stop or card transition. Capture the
    // retained IMS owner only after that I/O, then recheck the card generation.
    let observation = state.native_vowifi_lifecycle.reader_observation();
    let current = lifecycle.snapshot();
    if current.selection != snapshot.selection || current.usim_info != snapshot.usim_info {
        return ReaderFacts::default();
    }
    match observation {
        Some(observation) => native_reader_facts(&observation, &snapshot, saved_phone),
        None => passive_reader_facts(&snapshot, saved_phone),
    }
}

fn passive_reader_facts(
    snapshot: &crate::reader_lifecycle::Snapshot,
    saved_phone: String,
) -> ReaderFacts {
    let connected = !snapshot.selection.pending
        && !snapshot.selection.reader.is_empty()
        && snapshot.selection.generation != 0
        && snapshot.status["connected"] == true;
    ReaderFacts {
        connected,
        sim_phone: if connected {
            snapshot
                .usim_info
                .as_ref()
                .and_then(|v| v["phone_number"].as_str())
                .unwrap_or_default()
                .into()
        } else {
            String::new()
        },
        saved_phone: if connected {
            saved_phone
        } else {
            String::new()
        },
        ..Default::default()
    }
}

fn native_reader_facts(
    observation: &crate::native_vowifi_session::ReaderObservation,
    snapshot: &crate::reader_lifecycle::Snapshot,
    saved_phone: String,
) -> ReaderFacts {
    let text = |key: &str| {
        snapshot
            .usim_info
            .as_ref()
            .and_then(|info| info[key].as_str())
            .unwrap_or_default()
            .trim()
    };
    let sim_id = if text("iccid").is_empty() {
        text("imsi")
    } else {
        text("iccid")
    };
    let connected = !snapshot.selection.pending
        && !snapshot.selection.reader.is_empty()
        && snapshot.selection.generation != 0
        && snapshot.status["connected"] == true;
    let identity_valid = connected
        && observation.identity_valid
        && observation.reader == snapshot.selection.reader
        && observation.reader_generation == snapshot.selection.generation
        && !sim_id.is_empty()
        && observation.sim_id == sim_id;
    ReaderFacts {
        connected,
        session_active: observation.session_active,
        identity_valid,
        ims_registered: identity_valid && observation.status["ims_state"] == "registered",
        sms_ready: identity_valid && observation.sms_ready,
        registered_phone: if identity_valid {
            observation.status["registered_msisdn"]
                .as_str()
                .unwrap_or_default()
                .into()
        } else {
            String::new()
        },
        sim_phone: if connected {
            text("phone_number").into()
        } else {
            String::new()
        },
        saved_phone: if connected {
            saved_phone
        } else {
            String::new()
        },
        subscription_id: if identity_valid {
            observation.subscription_id.clone().unwrap_or_default()
        } else {
            String::new()
        },
    }
}

fn merged_facts_response(
    compatibility: Result<ModuleFacts, ()>,
    reader: ReaderFacts,
    module: Option<ModuleFacts>,
) -> Response<Full<Bytes>> {
    let module_unknown = compatibility.is_err() && module.is_none();
    let facts = DeviceFacts {
        module: module.unwrap_or_else(|| compatibility.unwrap_or_default()),
        reader,
    };
    match project(&facts) {
        Some(mut list) => {
            if module_unknown {
                // Unavailable compatibility observations do not prove that the
                // module is absent. The native reader remains observable.
                list.devices[0].detail = Some("DJI 模块状态暂不可读取");
            }
            json_response(StatusCode::OK, &list)
        }
        None => json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &PublicFailure {
                error: "短信通道状态暂不可读取",
                code: "sms_device_facts_invalid",
                retryable: true,
            },
        ),
    }
}

pub(crate) async fn handle_internal_for_state(
    request: Request<Incoming>,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    if !is_selection_route(request.method(), request.uri()) {
        return json_response(
            StatusCode::NOT_FOUND,
            &PublicFailure {
                error: "route not found",
                code: "route_not_found",
                retryable: false,
            },
        );
    }
    let bytes = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        MAX_BODY_BYTES,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(bytes) => bytes,
        _ => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &PublicFailure {
                    error: "无效短信设备选择请求",
                    code: "invalid_sms_device_selection",
                    retryable: false,
                },
            );
        }
    };
    let input = match decode_selection_input(&bytes) {
        Ok(input) => input,
        Err(()) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &PublicFailure {
                    error: "无效短信设备选择请求",
                    code: "invalid_sms_device_selection",
                    retryable: false,
                },
            );
        }
    };
    let module = native_usb::capture(state).await;
    let facts = DeviceFacts {
        module: module.unwrap_or_default(),
        reader: capture_reader(state),
    };
    if !facts_valid(&facts) {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &PublicFailure {
                error: "短信通道状态暂不可读取",
                code: "sms_device_facts_invalid",
                retryable: true,
            },
        );
    }
    let devices = project(&facts).expect("facts validated");
    let selection =
        if input.requested.trim().is_empty() && facts.module.native_observation_unavailable {
            Err(SelectionError {
                status: 503,
                code: "sms_device_selection_unavailable",
                message: "无法确认短信通道状态，请稍后重试".into(),
                recoverable: true,
            })
        } else {
            select(&devices.devices, &input.requested)
        };
    match selection {
        Ok(selected) => json_response(
            StatusCode::OK,
            &SelectionResponse {
                selected: Some(selected),
                error: None,
            },
        ),
        Err(error) => json_response(
            StatusCode::OK,
            &SelectionResponse {
                selected: None,
                error: Some(error),
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn response_json(response: Response<Full<Bytes>>) -> serde_json::Value {
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    #[tokio::test]
    async fn native_reader_remains_visible_without_compatibility_module_facts() {
        let response = merged_facts_response(Err(()), fixture().reader, None);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["devices"][0]["id"], "dji");
        assert_eq!(body["devices"][0]["available"], false);
        assert_eq!(body["devices"][0]["ready"], false);
        assert_eq!(body["devices"][0]["detail"], "DJI 模块状态暂不可读取");
        assert_eq!(body["devices"][1]["ready"], true);
        assert_eq!(body["devices"][1]["subscription_id"], "reader-subscription");
        assert_eq!(
            merged_facts_response(Err(()), ReaderFacts::default(), None).status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn unavailable_native_reader_preserves_compatibility_module_facts() {
        let native = ReaderFacts {
            connected: true,
            session_active: true,
            ..ReaderFacts::default()
        };
        let response = merged_facts_response(Ok(fixture().module), native, None);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["devices"][0]["ready"], true);
        assert_eq!(body["devices"][0]["subscription_id"], "module-subscription");
        assert_eq!(body["devices"][1]["ready"], false);
        assert!(body["devices"][1].get("subscription_id").is_none());
        assert!(body["devices"][1].get("phone").is_none());
        assert_eq!(
            body["devices"][1]["detail"],
            "读卡器或号卡身份已变化，请重新连接短信通道"
        );
    }

    #[tokio::test]
    async fn malformed_native_observation_fails_closed() {
        let mut native = fixture().reader;
        native.subscription_id = "x".repeat(257);
        let response = merged_facts_response(Ok(fixture().module), native, None);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response_json(response).await["code"],
            "sms_device_facts_invalid"
        );
    }

    fn native_observation() -> crate::native_vowifi_session::ReaderObservation {
        crate::native_vowifi_session::ReaderObservation {
            status: json!({"available":true,"tunnel_state":"connected","ims_state":"registered",
                "sms_ready":true,"registered_msisdn":"+447700900333"}),
            reader: "Reader A".into(),
            reader_generation: 7,
            sim_id: "894433000000000001".into(),
            session_active: true,
            identity_valid: true,
            sms_ready: true,
            subscription_id: Some("native-subscription".into()),
        }
    }

    fn native_snapshot() -> crate::reader_lifecycle::Snapshot {
        crate::reader_lifecycle::Snapshot {
            status: json!({"connected":true}),
            usim_info: Some(
                json!({"iccid":"894433000000000001","imsi":"234330000000001",
                "phone_number":"+447700900444"}),
            ),
            selection: crate::reader_reservation::SelectionSnapshot {
                reader: "Reader A".into(),
                generation: 7,
                ..Default::default()
            },
            esim_manager_available: false,
            notification_aids: Vec::new(),
            esim_profile_name: String::new(),
            pcsc_session: None,
        }
    }

    #[test]
    fn native_sms_observation_is_bound_to_current_reader_generation_and_sim() {
        for mismatch in ["reader", "generation", "sim", "pending", "owner"] {
            let mut snapshot = native_snapshot();
            let mut observation = native_observation();
            match mismatch {
                "reader" => snapshot.selection.reader = "Reader B".into(),
                "generation" => snapshot.selection.generation += 1,
                "sim" => {
                    snapshot.usim_info.as_mut().unwrap()["iccid"] = json!("894433000000000002")
                }
                "pending" => snapshot.selection.pending = true,
                "owner" => observation.identity_valid = false,
                _ => unreachable!(),
            }
            let reader = native_reader_facts(&observation, &snapshot, "+447700900555".into());
            assert!(!reader.identity_valid, "{mismatch}");
            assert!(!reader.sms_ready && !reader.ims_registered, "{mismatch}");
            assert!(reader.registered_phone.is_empty() && reader.subscription_id.is_empty());
            let projected = project(&DeviceFacts {
                reader,
                ..Default::default()
            })
            .unwrap();
            assert!(!projected.devices[1].ready);
            assert_ne!(projected.devices[1].phone.as_deref(), Some("+447700900333"));
        }
    }

    #[test]
    fn registered_phase_alone_cannot_reenable_native_sms_during_renewal_or_heartbeat_failure() {
        let mut observation = native_observation();
        let snapshot = native_snapshot();
        let ready = native_reader_facts(&observation, &snapshot, String::new());
        assert!(ready.sms_ready && ready.identity_valid && ready.ims_registered);
        observation.sms_ready = false;
        let reader = native_reader_facts(&observation, &snapshot, String::new());
        assert!(reader.ims_registered);
        assert!(!reader.sms_ready);
        let projected = project(&DeviceFacts {
            reader,
            ..Default::default()
        })
        .unwrap();
        assert!(!projected.devices[1].ready);
        assert_eq!(
            projected.devices[1].subscription_id.as_deref(),
            Some("native-subscription")
        );
    }

    fn fixture() -> DeviceFacts {
        DeviceFacts {
            module: ModuleFacts {
                native_route_ready: None,
                native_observation_unavailable: false,
                modem_present: true,
                usb_at_present: false,
                usb_device_present: false,
                session_active: true,
                session_info_present: true,
                ims_registered: true,
                messaging_ready: true,
                smsc: "+441234567890".into(),
                target: "tel:+441234567890".into(),
                phone: "+447700900111".into(),
                session_phone: "+447700900222".into(),
                subscription_id: "module-subscription".into(),
            },
            reader: ReaderFacts {
                connected: true,
                session_active: true,
                identity_valid: true,
                ims_registered: true,
                sms_ready: true,
                registered_phone: "+447700900333".into(),
                sim_phone: "+447700900444".into(),
                saved_phone: "+447700900555".into(),
                subscription_id: "reader-subscription".into(),
            },
        }
    }

    #[test]
    fn projection_preserves_device_order_names_phone_precedence_and_readiness() {
        let devices = project(&fixture()).unwrap().devices;
        assert_eq!(
            devices.iter().map(|device| device.id).collect::<Vec<_>>(),
            ["dji", "reader"]
        );
        assert_eq!(devices[0].name, "+447700900222_DJI");
        assert_eq!(devices[0].phone.as_deref(), Some("+447700900222"));
        assert_eq!(
            devices[0].subscription_id.as_deref(),
            Some("module-subscription")
        );
        assert!(devices[0].available && devices[0].ready);
        assert_eq!(devices[1].name, "+447700900333_Reader");
        assert_eq!(
            devices[1].subscription_id.as_deref(),
            Some("reader-subscription")
        );
        assert!(devices[1].available && devices[1].ready);
    }

    #[test]
    fn projection_preserves_route_missing_and_reader_identity_guard_precedence() {
        let mut facts = fixture();
        facts.module.smsc.clear();
        facts.reader.identity_valid = false;
        let devices = project(&facts).unwrap().devices;
        assert!(!devices[0].ready);
        assert_eq!(
            devices[0].detail,
            Some("IMS 已注册，但当前 SIM 未提供可用 SMSC/IMS 短信目标")
        );
        assert!(!devices[1].ready);
        assert_eq!(
            devices[1].detail,
            Some("读卡器或号卡身份已变化，请重新连接短信通道")
        );
        assert!(devices[1].subscription_id.is_none());
    }

    #[test]
    fn projection_uses_reader_phone_fallbacks_and_unavailable_details() {
        let mut facts = fixture();
        facts.module.session_active = false;
        facts.module.session_info_present = false;
        facts.module.phone.clear();
        facts.reader.connected = false;
        facts.reader.session_active = false;
        facts.reader.ims_registered = false;
        facts.reader.sms_ready = false;
        facts.reader.registered_phone = "unknown".into();
        facts.reader.sim_phone = "000000".into();
        let devices = project(&facts).unwrap().devices;
        assert_eq!(devices[0].name, "未知号码_DJI");
        assert_eq!(devices[1].name, "+447700900555_Reader");
        assert_eq!(devices[1].detail, Some("读卡器未连接"));
    }

    #[test]
    fn module_phone_uses_session_info_only_when_go_observed_it() {
        let mut facts = fixture();
        facts.module.phone = "+447700900111".into();
        facts.module.session_info_present = false;
        facts.module.session_phone = "+447700900222".into();
        let devices = project(&facts).unwrap().devices;
        assert_eq!(devices[0].phone.as_deref(), Some("+447700900111"));

        facts.module.session_info_present = true;
        facts.module.session_phone.clear();
        let devices = project(&facts).unwrap().devices;
        assert_eq!(devices[0].phone, None);
        assert_eq!(devices[0].name, "未知号码_DJI");
    }

    #[test]
    fn module_direct_sms_remains_ready_without_a_vowifi_session() {
        let mut facts = fixture();
        facts.module.session_active = false;
        facts.module.session_info_present = false;
        facts.module.smsc.clear();
        facts.module.target.clear();
        let devices = project(&facts).unwrap().devices;
        assert!(devices[0].available);
        assert!(devices[0].ready, "direct modem SMS route must remain ready");
        assert_eq!(devices[0].detail, None);

        facts.module.modem_present = false;
        facts.module.usb_at_present = true;
        let devices = project(&facts).unwrap().devices;
        assert!(devices[0].ready, "USB AT SMS route must remain ready");

        facts.module.usb_at_present = false;
        let devices = project(&facts).unwrap().devices;
        assert!(!devices[0].ready);
        assert_eq!(devices[0].detail, Some("未检测到 DJI 模块"));
    }

    #[test]
    fn selection_keeps_explicit_and_automatic_route_policy() {
        let devices = project(&fixture()).unwrap().devices;
        assert_eq!(select(&devices, "reader").unwrap(), "reader");
        let required = select(&devices, "").unwrap_err();
        assert_eq!(required.status, 409);
        assert_eq!(required.code, "sms_device_required");

        let mut only_module_ready = fixture();
        only_module_ready.reader.sms_ready = false;
        let devices = project(&only_module_ready).unwrap().devices;
        assert_eq!(select(&devices, "").unwrap(), "dji");
        let not_ready = select(&devices, "reader").unwrap_err();
        assert_eq!(not_ready.status, 503);
        assert_eq!(not_ready.code, "sms_device_not_ready");
        assert!(not_ready.recoverable);
        assert_eq!(select(&devices, "unknown").unwrap_err().status, 400);
    }

    #[test]
    fn public_device_projection_keeps_legacy_wire_shape_and_omissions() {
        let facts = fixture();
        let list = project(&facts).unwrap();
        let wire = serde_json::to_value(list).unwrap();
        assert_eq!(
            wire,
            json!({
                "devices": [
                    {
                        "id": "dji",
                        "name": "+447700900222_DJI",
                        "kind": "dji",
                        "phone": "+447700900222",
                        "available": true,
                        "ready": true,
                        "subscription_id": "module-subscription"
                    },
                    {
                        "id": "reader",
                        "name": "+447700900333_Reader",
                        "kind": "reader",
                        "phone": "+447700900333",
                        "available": true,
                        "ready": true,
                        "subscription_id": "reader-subscription"
                    }
                ]
            })
        );

        let mut unavailable = facts;
        unavailable.module.modem_present = false;
        unavailable.module.usb_at_present = false;
        unavailable.module.usb_device_present = false;
        unavailable.module.session_active = false;
        unavailable.module.session_info_present = false;
        unavailable.reader.connected = false;
        unavailable.reader.session_active = false;
        let wire = serde_json::to_value(project(&unavailable).unwrap()).unwrap();
        assert_eq!(wire["devices"][0]["detail"], "未检测到 DJI 模块");
        assert!(wire["devices"][0].get("phone").is_some());
        assert!(wire["devices"][0].get("subscription_id").is_none());
        assert!(wire["devices"][1].get("subscription_id").is_none());
    }

    #[test]
    fn selection_result_keeps_legacy_error_payload_fields() {
        let mut facts = fixture();
        facts.reader.sms_ready = false;
        let devices = project(&facts).unwrap().devices;
        let error = select(&devices, "reader").unwrap_err();
        let wire = serde_json::to_value(SelectionResponse {
            selected: None,
            error: Some(error),
        })
        .unwrap();
        assert_eq!(
            wire,
            json!({
                "error": {
                    "status": 503,
                    "code": "sms_device_not_ready",
                    "message": "+447700900333_Reader：读卡器 IMS 短信通道尚未就绪",
                    "recoverable": true
                }
            })
        );
    }

    #[test]
    fn selection_returns_unavailable_when_no_route_is_ready() {
        let mut facts = fixture();
        facts.module.messaging_ready = false;
        facts.reader.sms_ready = false;
        let devices = project(&facts).unwrap().devices;
        let error = select(&devices, "").unwrap_err();
        assert_eq!(error.status, 503);
        assert_eq!(error.code, "sms_device_unavailable");
        assert!(error.recoverable);
    }

    #[test]
    fn selection_request_decoder_and_route_guard_fail_closed() {
        let input = json!({"requested":"reader","facts":fixture()});
        let decoded = decode_selection_input(&serde_json::to_vec(&input).unwrap()).unwrap();
        assert_eq!(decoded.requested, "reader");

        let unknown = json!({"requested":"","facts":fixture(),"unexpected":true});
        assert!(decode_selection_input(&serde_json::to_vec(&unknown).unwrap()).is_err());
        let oversized_name = json!({"requested":"x".repeat(129),"facts":fixture()});
        assert!(decode_selection_input(&serde_json::to_vec(&oversized_name).unwrap()).is_err());
        let facts = fixture();
        let mut oversized = facts.clone();
        oversized.module.target = "x".repeat(1025);
        let oversized_facts = json!({"requested":"","facts":oversized});
        assert!(decode_selection_input(&serde_json::to_vec(&oversized_facts).unwrap()).is_err());

        assert!(is_selection_route(
            &Method::POST,
            &Uri::from_static("/api/internal/sms/devices/select")
        ));
        assert!(!is_selection_route(
            &Method::GET,
            &Uri::from_static("/api/internal/sms/devices/select")
        ));
        assert!(!is_selection_route(
            &Method::POST,
            &Uri::from_static("/api/internal/sms/devices/select?x=1")
        ));
    }
}
