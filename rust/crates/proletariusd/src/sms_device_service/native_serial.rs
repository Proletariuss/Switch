//! Cache-only actor facts; a configured but unready serial route remains authoritative.
use super::*;

pub(super) async fn capture(state: &crate::ServerState) -> Option<ModuleFacts> {
    if !state.modem_actor.native_serial_managed()
        && state.module_radio.configured_usb_transport() != Some(false)
    {
        return None;
    }
    let session = state.module_radio.serial_transport_session();
    let actor = state.modem_actor.clone();
    let observation = tokio::task::spawn_blocking(move || actor.serial_sms_observation())
        .await
        .ok()
        .and_then(|v| v.ok());
    // Do not return compatibility facts across a configuration transition.
    // The next request may observe the replacement transport; this one is unknown.
    if session.is_none() || session != state.module_radio.serial_transport_session() {
        return Some(unknown());
    }
    let Some(observation) = observation else {
        return Some(unknown());
    };
    if observation.present && observation.serial_session != session {
        return Some(unknown());
    }
    let current = if let Some(binding) = observation.binding.clone() {
        let actor = state.modem_actor.clone();
        tokio::task::spawn_blocking(move || actor.serial_sms_binding_current(&binding))
            .await
            .ok()
            .and_then(|v| v.ok())
            .unwrap_or(false)
    } else {
        false
    };
    if observation.ready && !current || session != state.module_radio.serial_transport_session() {
        return Some(unknown());
    }
    let ready = observation.ready && current && !state.module_radio.vowifi_owns_sim_channel();
    Some(ModuleFacts {
        modem_present: observation.present,
        native_route_ready: Some(ready),
        native_observation_unavailable: observation.unknown,
        phone: observation.phone,
        ..Default::default()
    })
}
fn unknown() -> ModuleFacts {
    ModuleFacts {
        native_route_ready: Some(false),
        native_observation_unavailable: true,
        ..Default::default()
    }
}
