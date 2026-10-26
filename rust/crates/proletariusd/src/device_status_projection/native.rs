use super::*;

enum Observation {
    Device(crate::usb_at::DeviceObservation),
    Hardware(crate::module_discovery::UsbFallback),
    Changed,
}

pub(super) async fn capture(state: &ServerState) -> Result<Value, BackendFailure> {
    let owned = state.clone();
    // The workflow, not its HTTP waiter, owns RF through actual USB completion.
    // Keep its observation token across the join, not only the projected JSON.
    let task = tokio::spawn(async move { read(&owned).await });
    let observation = match tokio::time::timeout(STATUS_SNAPSHOT_TIMEOUT, task).await {
        Ok(Ok(observed)) => observed,
        _ => fallback(state).await,
    };
    if state.modem_actor.native_serial_managed() {
        return super::serial::capture(state).await;
    }
    publish(state, observation)
}

fn publish(state: &ServerState, observation: Observation) -> Result<Value, BackendFailure> {
    if let Some(status) = native_module_status(state) {
        return Ok(status);
    }
    if !crate::module_discovery::uses_native_usb(&state.module_radio, &state.usb_at) {
        return Err(BackendFailure::Io(
            "module transport changed during USB status observation".into(),
        ));
    }
    Ok(match observation {
        Observation::Device(value) if state.usb_at.device_observation_current(&value) => {
            value.status
        }
        Observation::Hardware(value)
            if state.usb_at.hardware_observation_current(value.owner_epoch) =>
        {
            let (operator, network, summary) = match &value.device {
                Some(device) => (
                    "已检测到 USB 设备",
                    device.mode.clone(),
                    format!(
                        "{} {} ({}:{})",
                        device.vendor, device.product, device.vendor_id, device.product_id
                    ),
                ),
                None => ("未连接", "不可用".into(), "未发现 AT 串口".into()),
            };
            json!({"operator":operator,"signal_dbm":null,"network_mode":network,"sim_inserted":false,
                "hardware_status":summary,"discovery_error":value.discovery_error,"usb_device":value.device})
        }
        _ => {
            json!({"operator":"未连接","signal_dbm":null,"network_mode":"不可用","sim_inserted":false,
            "hardware_status":"未发现 AT 串口","discovery_error":"USB 设备连接已变化，请刷新重试","usb_device":null})
        }
    })
}

async fn read(state: &ServerState) -> Observation {
    if native_module_status(state).is_some() {
        return Observation::Changed;
    }
    let _single = state.usb_at.esim_snapshot_guard().await;
    if native_module_status(state).is_some() {
        return Observation::Changed;
    }
    if state.module_radio.sim_channel_busy() {
        return fallback(state).await;
    }
    let Some(session) = state.module_radio.usb_transport_session() else {
        return fallback(state).await;
    };
    let reservation = match state.module_radio.reserve_esim_read().await {
        Ok(reservation) => reservation,
        Err(_) => return fallback(state).await,
    };
    let observed = state.usb_at.native_device_observation(&session).await;
    let released = state.module_radio.release_esim(reservation).await;
    match observed {
        Ok(observation)
            if released.is_ok()
                && state.module_radio.usb_transport_session().as_deref()
                    == Some(session.as_str())
                && state.usb_at.device_observation_current(&observation) =>
        {
            Observation::Device(observation)
        }
        _ => fallback(state).await,
    }
}

/// Failed observations never include historical SIM identity. The final HTTP
/// publisher rechecks this exact discovery generation after awaiting the task.
async fn fallback(state: &ServerState) -> Observation {
    match state.usb_at.native_hardware_observation().await {
        Ok(observation) => Observation::Hardware(observation),
        Err(_) => Observation::Changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb_at_transport::{Transport, TransportError};
    use std::sync::Arc;
    struct Port;
    impl Transport for Port {
        fn checked_close(
            self: Box<Self>,
        ) -> Result<
            crate::usb_at_transport::CloseDisposition,
            crate::usb_at_transport::TransportError,
        > {
            // Explicit fixture release; Drop barriers must finish before success.
            drop(self);
            Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
        }

        fn description(&self) -> String {
            "publication USB fixture".into()
        }
        fn command(
            &mut self,
            command: &str,
            _: Option<&[u8]>,
            _: Duration,
            _: usize,
        ) -> Result<String, TransportError> {
            Ok(match command {
                "AT+CFUN?" => "+CFUN: 1\r\nOK\r\n",
                "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n",
                "AT+QCCID" => "+QCCID: 8988212345678901234\r\nOK\r\n",
                "AT+CIMI" => "234150123456789\r\nOK\r\n",
                _ => "OK\r\n",
            }
            .into())
        }
    }
    #[tokio::test]
    async fn publication_rechecks_exact_device_and_descriptor_tokens_after_task_completion() {
        for descriptor in [false, true] {
            let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .unwrap()
                .join("build/runtime")
                .join(format!(
                    "publication502-{}",
                    crate::snapshot::new_instance_id()
                ));
            std::fs::create_dir_all(&root).unwrap();
            let (usb, session) =
                crate::usb_at::Owner::native_module_fixture(Arc::new(|| Ok(Box::new(Port)))).await;
            let radio = crate::module_radio::Service::native_vowifi_fixture(
                usb.clone(),
                session,
                root.clone(),
            )
            .await;
            let mut state = crate::reader_pcsc_gateway_tests::state(&root);
            state.usb_at = usb;
            state.module_radio = radio;
            let observation = if descriptor {
                fallback(&state).await
            } else {
                read(&state).await
            };
            assert!(matches!(
                (&observation, descriptor),
                (Observation::Device(_), false) | (Observation::Hardware(_), true)
            ));
            // Complete actual close after the producer task returned, while the
            // HTTP publisher still retains the old observation.
            state
                .usb_at
                .replace_producer("new-publication-producer".into())
                .await
                .unwrap();
            assert!(crate::module_discovery::uses_native_usb(
                &state.module_radio,
                &state.usb_at
            ));
            let value = publish(&state, observation).unwrap();
            assert!(value.get("iccid").is_none());
            assert_eq!(value["usb_device"], Value::Null);
            assert_eq!(value["sim_inserted"], false);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
