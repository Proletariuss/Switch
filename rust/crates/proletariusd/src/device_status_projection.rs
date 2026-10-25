//! Status comes from retained Rust module owners; no-device is an observable state.
use crate::ServerState;
use proletarius_api_contract::UpstreamDeviceStatus;
use proletarius_domain::BackendFailure;
use serde_json::{Value, json};
use std::time::Duration;
mod native;
mod serial;
const STATUS_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) async fn capture(state: &ServerState) -> Result<Value, BackendFailure> {
    let mut status = if state.availability.module_recovery().is_quarantined() {
        json!({
            "operator":"未连接", "signal_dbm":null, "network_mode":"不可用",
            "sim_inserted":false, "ps_attached":false,
            "hardware_status":crate::module_recovery::MESSAGE,
            "discovery_error":crate::module_recovery::CODE, "usb_device":null
        })
    } else if let Some(status) = native_module_status(state) {
        status
    } else if state.modem_actor.native_serial_managed() {
        serial::capture(state).await?
    } else if crate::module_discovery::uses_native_usb(&state.module_radio, &state.usb_at) {
        native::capture(state).await?
    } else {
        serial::capture(state).await?
    };
    attach_qmi_status(&mut status, state.qmi_enhancement.transport_status());
    Ok(status)
}

fn attach_qmi_status(status: &mut Value, qmi: Option<proletarius_api_contract::UpstreamQmiStatus>) {
    let (Some(fields), Some(qmi)) = (status.as_object_mut(), qmi) else {
        return;
    };
    let mut projected = json!({
        "enabled": qmi.enabled,
        "session_initialized": qmi.session_initialized,
    });
    if let Some(error) = qmi.initialization_error {
        projected["initialization_error"] = Value::String(error);
    }
    fields.insert("qmi".to_owned(), projected);
}

pub(crate) fn native_module_status(state: &ServerState) -> Option<Value> {
    state
        .native_vowifi_lifecycle
        .retained_module_service()
        .and_then(|service| service.metadata_observation())
}

/// Preserve the retained module's cache-only status path while exposing the
/// same QMI diagnostic projection as a full device snapshot.
pub(crate) fn cached_native_status_with_qmi(state: &ServerState) -> Option<Value> {
    let mut status = native_module_status(state)?;
    attach_qmi_status(&mut status, state.qmi_enhancement.transport_status());
    Some(status)
}

pub(crate) async fn capture_typed(
    state: &ServerState,
) -> Result<UpstreamDeviceStatus, BackendFailure> {
    let value = capture(state).await?;
    serde_json::from_value(value)
        .map_err(|error| BackendFailure::Io(format!("device status DTO: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qmi_projection_is_additive_and_does_not_promote_radio_or_sim_readiness() {
        let mut status = json!({
            "operator":"未连接",
            "sim_inserted":false,
            "hardware_status":"未发现 AT 串口"
        });
        attach_qmi_status(
            &mut status,
            Some(proletarius_api_contract::UpstreamQmiStatus {
                enabled: true,
                session_initialized: true,
                initialization_error: None,
            }),
        );
        assert_eq!(status["qmi"]["enabled"], true);
        assert_eq!(status["qmi"]["session_initialized"], true);
        assert!(status["qmi"].get("initialization_error").is_none());
        assert_eq!(status["sim_inserted"], false);
        assert_eq!(status["operator"], "未连接");
        assert_eq!(status["hardware_status"], "未发现 AT 串口");
    }

    #[test]
    fn qmi_startup_failure_is_visible_without_claiming_module_readiness() {
        let mut status = json!({
            "operator":"未连接",
            "sim_inserted":false,
            "hardware_status":"未发现 AT 串口"
        });
        attach_qmi_status(
            &mut status,
            Some(proletarius_api_contract::UpstreamQmiStatus {
                enabled: true,
                session_initialized: false,
                initialization_error: Some("QMI USB device not found".to_owned()),
            }),
        );
        assert_eq!(
            status["qmi"]["initialization_error"],
            "QMI USB device not found"
        );
        assert_eq!(status["sim_inserted"], false);
        assert_eq!(status["operator"], "未连接");
        assert_eq!(status["hardware_status"], "未发现 AT 串口");
    }

    #[test]
    fn absent_qmi_owner_preserves_the_legacy_status_shape() {
        let mut status = json!({"operator":"未连接","sim_inserted":false});
        attach_qmi_status(&mut status, None);
        assert!(status.get("qmi").is_none());
        assert_eq!(status["sim_inserted"], false);
    }
}
