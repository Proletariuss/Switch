//! Cache-only observation of the retained native serial lifecycle. HTTP reads
//! do not start a competing AT metadata collector or depend on a Go child.
use super::*;
use crate::modem_actor::{NativeSerialMetadata, NativeSerialObservation};

pub(super) async fn capture(state: &ServerState) -> Result<Value, BackendFailure> {
    let observation = state.modem_actor.native_serial_observation();
    let Some(metadata) = observation
        .metadata
        .as_ref()
        .filter(|value| current(state, value))
    else {
        return Ok(fallback(&observation));
    };
    let mut value = metadata.status.clone();
    // This mirrors the legacy display fallback without putting a saved phone
    // number into the physical identity cache. A SIM change while disk I/O is
    // pending invalidates both the status and the fallback number.
    if value["phone_number"]
        .as_str()
        .unwrap_or_default()
        .is_empty()
    {
        let iccid = value["iccid"].as_str().unwrap_or_default().to_owned();
        if !iccid.is_empty() {
            let store = crate::profile_notes::Store::new(state.multistandby_store.data_root());
            let phone = crate::archive_io::run(move || Ok(store.phone_for_iccid_or_empty(&iccid)))
                .await
                .unwrap_or_default();
            if !phone.is_empty() {
                value["phone_number"] = Value::String(phone);
            }
        }
    }
    publish(state, metadata, value)
}

fn current(state: &ServerState, metadata: &NativeSerialMetadata) -> bool {
    state.modem_actor.native_serial_managed()
        && state
            .modem_actor
            .serial_card_binding_current(&metadata.binding)
        && state.module_radio.serial_transport_session() == Some(metadata.binding.serial_session)
        && metadata.suppression_revision == state.modem_actor.cellular_policy_revision()
        && metadata.status["cellular_suppressed"].as_bool()
            == Some(state.modem_actor.cellular_suppressed())
}

fn publish(
    state: &ServerState,
    metadata: &NativeSerialMetadata,
    mut value: Value,
) -> Result<Value, BackendFailure> {
    if let Some(status) = native_module_status(state) {
        return Ok(status);
    }
    if !current(state, metadata) {
        return Ok(fallback(&state.modem_actor.native_serial_observation()));
    }
    let Some(fields) = value.as_object_mut() else {
        return Err(BackendFailure::Io(
            "native serial metadata schema invalid".into(),
        ));
    };
    fields.remove("cellular_suppressed");
    if fields
        .get("phone_number")
        .and_then(Value::as_str)
        .is_some_and(str::is_empty)
    {
        fields.remove("phone_number");
    }
    // The serial compatibility status never queried packet attachment.
    fields.insert("ps_attached".into(), Value::Bool(false));
    Ok(value)
}

fn fallback(observation: &NativeSerialObservation) -> Value {
    let connected = observation.serial_session.is_some() && observation.terminal.is_none();
    let error = if observation.discovery_error.is_empty() {
        observation
            .terminal
            .as_ref()
            .map_or("", |terminal| terminal.code)
    } else {
        observation.discovery_error.as_str()
    };
    json!({
        "operator":if connected { "正在初始化" } else { "未连接" },
        "signal_dbm":null,
        "network_mode":if connected { "串口 AT" } else { "不可用" },
        "sim_inserted":false,
        "ps_attached":false,
        "hardware_status":if observation.port.is_empty() { "未发现 AT 串口" } else { &observation.port },
        "discovery_error":error,
        "usb_device":null
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn old_serial_metadata_cannot_revive_after_suppression_aba() {
        let fixture = crate::module_startup::SerialTestFixture::new();
        let mut state = crate::reader_pcsc_gateway_tests::state(&fixture.root);
        state.modem_actor = fixture.modem.clone();
        state.usb_at = fixture.usb.clone();
        state.module_radio = fixture.radio.clone();
        state.sms_inbox = fixture.inbox.clone();
        let startup = fixture.start();
        tokio::time::timeout(Duration::from_secs(12), async {
            while !state.modem_actor.native_serial_observation().ready {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        let old = state
            .modem_actor
            .native_serial_observation()
            .metadata
            .unwrap();
        assert_eq!(old.status["cellular_suppressed"], true);
        assert!(current(&state, &old));
        let original = publish(&state, &old, old.status.clone()).unwrap();
        assert_eq!(original["iccid"], crate::module_startup::SERIAL_TEST_ICCID);
        assert_eq!(original["phone_number"], "+447700900123");

        // Same SIM/session and same final boolean, but a different policy
        // history. A payload captured before either change must stay invalid.
        state.modem_actor.set_cellular_suppressed(false);
        state.modem_actor.set_cellular_suppressed(true);
        assert!(state.modem_actor.cellular_suppressed());
        assert_ne!(
            state.modem_actor.cellular_policy_revision(),
            old.suppression_revision
        );
        assert!(state.modem_actor.serial_card_binding_current(&old.binding));
        assert!(!current(&state, &old));
        let result = publish(&state, &old, old.status.clone()).unwrap();
        for field in ["iccid", "imsi", "phone_number"] {
            assert!(
                result[field].as_str().unwrap_or_default().is_empty(),
                "old {field} revived after RF suppression ABA: {result}"
            );
        }
        fixture.finish(&startup).await;
    }
}
