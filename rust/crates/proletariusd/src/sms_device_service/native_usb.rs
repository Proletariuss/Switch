//! Cache-only device presentation. Actual cellular submission separately
//! captures and validates a fresh physical/SIM capability.
use super::*;

fn capture_ims(state: &crate::ServerState) -> Option<ModuleFacts> {
    if let Some(service) = state.native_vowifi_lifecycle.retained_module_service()
        && service.owns_session()
    {
        let binding = service.sms_binding().ok();
        let status = service.status();
        let metadata = service.metadata_observation();
        let binding = binding.filter(|b| service.resolve_sms_binding(&b.binding).is_ok());
        let phone = metadata
            .as_ref()
            .filter(|v| v["sim_inserted"] == true)
            .and_then(|v| v["phone_number"].as_str())
            .unwrap_or_default()
            .trim()
            .to_owned();
        return Some(ModuleFacts {
            usb_at_present: state.usb_at.device_observation_description().is_some(),
            session_active: true,
            session_info_present: true,
            ims_registered: status["ims_registered"] == true,
            messaging_ready: binding.is_some(),
            native_route_ready: Some(binding.is_some()),
            session_phone: phone,
            subscription_id: binding.map(|b| b.subscription_id).unwrap_or_default(),
            ..Default::default()
        });
    }
    None
}

pub(super) async fn capture(state: &crate::ServerState) -> Option<ModuleFacts> {
    if let Some(module) = capture_ims(state) {
        return Some(module);
    }
    if let Some(module) = super::native_serial::capture(state).await {
        return Some(capture_ims(state).unwrap_or(module));
    }
    if !crate::module_discovery::uses_native_usb(&state.module_radio, &state.usb_at) {
        return None;
    }
    // A descriptor is presence only; it never opens USB, establishes a SIM
    // identity or grants SMS readiness. The observer coalesces UI requests.
    let hardware = if state.usb_at.device_observation_description().is_none() {
        Some(state.usb_at.native_hardware_observation().await)
    } else {
        None
    };
    if let Some(module) = capture_ims(state) {
        return Some(module);
    }
    if let Some(module) = super::native_serial::capture(state).await {
        return Some(capture_ims(state).unwrap_or(module));
    }
    if !crate::module_discovery::uses_native_usb(&state.module_radio, &state.usb_at) {
        return None;
    }
    let present = state.usb_at.device_observation_description().is_some();
    let hardware = hardware.map(|result| {
        result
            .ok()
            .filter(|v| state.usb_at.hardware_observation_current(v.owner_epoch))
    });
    let device_present = hardware
        .as_ref()
        .and_then(|v| v.as_ref())
        .is_some_and(|v| v.device.is_some());
    let unknown = hardware.as_ref().is_some_and(|v| {
        v.as_ref()
            .is_none_or(|v| v.source == crate::module_discovery::ObservationSource::Unavailable)
    });
    let phone = state
        .usb_at
        .cached_device_observation()
        .filter(|v| v.status["sim_inserted"] == true)
        .and_then(|v| v.status["phone_number"].as_str().map(str::to_owned))
        .unwrap_or_default();
    Some(ModuleFacts {
        usb_at_present: present,
        usb_device_present: device_present,
        native_observation_unavailable: unknown,
        phone,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests;
