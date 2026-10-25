//! Health is observed from retained Rust physical owners, including startup
//! and disconnected states. A successful observation does not imply SIM readiness.
use crate::ServerState;
use proletarius_api_contract::HealthResponse;
use proletarius_domain::BackendFailure;

pub(crate) async fn capture(state: &ServerState) -> Result<HealthResponse, BackendFailure> {
    if state.availability.module_recovery().is_quarantined() {
        return Ok(HealthResponse {
            ok: true,
            port: "模块等待旧版恢复确认".into(),
            esim_available: false,
            usb_device: None,
            discovery_error: crate::module_recovery::CODE.into(),
        });
    }
    if state.modem_actor.native_serial_managed() {
        return Ok(capture_native_serial(state));
    }
    if crate::module_discovery::uses_native_usb(&state.module_radio, &state.usb_at) {
        return capture_native_usb(state).await;
    }
    Ok(capture_native_serial(state))
}

fn capture_native_serial(state: &ServerState) -> HealthResponse {
    let observation = state.modem_actor.native_serial_observation();
    let available = state
        .modem_actor
        .cached_serial_card_binding()
        .is_some_and(|binding| {
            state.modem_actor.native_esim_managed()
                && observation.serial_session == Some(binding.serial_session)
                && state
                    .module_radio
                    .native_serial_bootstrapped(binding.serial_session)
                && state.modem_actor.serial_card_binding_current(&binding)
        });
    let after = state.modem_actor.native_serial_observation();
    HealthResponse {
        ok: true,
        port: if after.port.is_empty() {
            "未发现 AT 串口".into()
        } else {
            after.port
        },
        esim_available: available && observation.revision == after.revision,
        usb_device: None,
        discovery_error: if after.discovery_error.is_empty() {
            after
                .terminal
                .map_or_else(String::new, |terminal| terminal.code.into())
        } else {
            after.discovery_error
        },
    }
}

async fn capture_native_usb(state: &ServerState) -> Result<HealthResponse, BackendFailure> {
    let observation = state.usb_at.native_hardware_observation().await;
    if state.modem_actor.native_serial_managed() {
        return Ok(capture_native_serial(state));
    }
    let observation = observation
        .map_err(|error| BackendFailure::Io(format!("USB hardware observation: {}", error.code)))?;
    if !crate::module_discovery::uses_native_usb(&state.module_radio, &state.usb_at) {
        return Err(BackendFailure::Io(
            "module transport changed during health observation".into(),
        ));
    }
    let esim_available = state.module_radio.usb_esim_route_available();
    if !state
        .usb_at
        .hardware_observation_current(observation.owner_epoch)
    {
        return Err(BackendFailure::Io(
            "USB device changed during health observation".into(),
        ));
    }
    Ok(HealthResponse {
        ok: true,
        port: observation.port,
        esim_available,
        usb_device: observation.device,
        discovery_error: observation.discovery_error,
    })
}
