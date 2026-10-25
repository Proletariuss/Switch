//! Public USB device status on the exact physical owner. No Go observation or
//! replacement open is allowed once a status transaction has been captured.
use super::*;

#[derive(Clone)]
pub(crate) struct DeviceObservation {
    producer: String,
    epoch: u64,
    session: String,
    identity_generation: u64,
    config_sequence: u64,
    suppressed: bool,
    pub(crate) status: Value,
}
impl DeviceObservation {
    fn current(&self, state: &State) -> bool {
        !state.stopping
            && !state.revoking
            && state.producer == self.producer
            && state.epoch == self.epoch
            && state.session == self.session
            && !state.session.is_empty()
            && state.identity_generation == self.identity_generation
            && state.config_high_water == self.config_sequence
            && state.cellular_suppressed == self.suppressed
    }
}
impl Owner {
    /// Describes only a presently owned physical stream, never a remembered
    /// ICCID or the descriptor of a previously closed/reconfigured device.
    pub(crate) fn device_observation_description(&self) -> Option<String> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        (!state.stopping && !state.revoking && !state.session.is_empty())
            .then(|| state.description.clone())
            .filter(|s| !s.is_empty())
    }
    /// Same-physical-session last-known-good display data. It is never a
    /// capability to submit SMS and has no arbitrary display TTL.
    pub(crate) fn cached_device_observation(&self) -> Option<DeviceObservation> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .device_observation
            .as_ref()
            .filter(|value| value.current(&state))
            .cloned()
    }
    pub(crate) fn device_observation_current(&self, observation: &DeviceObservation) -> bool {
        observation.current(&self.inner.state.lock().unwrap_or_else(|e| e.into_inner()))
    }
    pub(crate) async fn native_device_observation(
        &self,
        session: &str,
    ) -> Result<DeviceObservation, Failure> {
        let captured = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check(&state, &state.producer)?;
            if session.is_empty() || state.session != session {
                return Err(Failure::stale());
            }
            DeviceObservation {
                producer: state.producer.clone(),
                epoch: state.epoch,
                session: session.into(),
                identity_generation: state.identity_generation,
                config_sequence: state.config_high_water,
                suppressed: state.cellular_suppressed,
                status: Value::Null,
            }
        };
        let deadline = Instant::now() + DEVICE_STATUS_BUDGET;
        let permit = self
            .admit(&captured.producer, Some(&captured.session), "", deadline)
            .await?;
        let notes = self.profile_notes.clone();
        self.run(permit, move |inner, epoch| {
            let fence = || {
                let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                check_epoch(&state, epoch)?;
                if !captured.current(&state) {
                    return Err(Failure::stale());
                }
                if state.lease.is_some()
                    || state.native_module_hold.is_some()
                    || !state.inhibits.is_empty()
                    || state.radio_inhibit.is_some()
                {
                    return Err(Failure::busy());
                }
                Ok(())
            };
            fence()?;
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let mut failed = false;
            let mut sim_unavailable = false;
            let mut identity_iccid = None;
            let mut identity_imsi = None;
            let status =
                crate::usb_at_status::collect(captured.suppressed, |command, timeout, maximum| {
                    fence()?;
                    check_cellular_command(inner, command)?;
                    let result = port.as_mut().ok_or_else(Failure::stale)?.command(
                        command,
                        None,
                        remaining(deadline)?.min(timeout),
                        maximum.min(MAX_BYTES),
                    );
                    failed |= result.is_err();
                    if let Ok(response) = &result
                        && complete_ok(response)
                    {
                        if command == "AT+QCCID" {
                            identity_iccid = crate::usb_at_storage::card_identity(response)
                                .filter(|value| valid_module_sim_identifier(value, 10, 20));
                        } else if command == "AT+CIMI" {
                            identity_imsi = response
                                .split(['\r', '\n'])
                                .map(str::trim)
                                .find(|value| valid_module_sim_identifier(value, 5, 20))
                                .map(str::to_owned);
                        }
                    }
                    if command == "AT+CPIN?"
                        && let Ok(response) = &result
                    {
                        sim_unavailable = native_sms_route::explicit_sim_absent(response)
                            || (complete_ok(response)
                                && response.split(['\r', '\n']).any(|line| {
                                    line.trim()
                                        .strip_prefix("+CPIN:")
                                        .is_some_and(|state| state.trim() != "READY")
                                }));
                    }
                    fence()?;
                    result.map_err(Failure::from)
                });
            if failed {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            let mut status = status?;
            fence()?;
            remaining(deadline)?;
            if crate::profile_notes::is_missing_phone(&status.phone_number) {
                status.phone_number = notes
                    .as_ref()
                    .map(|n| n.phone_for_iccid_or_empty(&status.iccid))
                    .unwrap_or_default()
                    .trim()
                    .into();
            }
            fence()?;
            let mut observation = DeviceObservation {
                status: public_status(status),
                ..captured
            };
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !observation.current(&state) {
                return Err(Failure::stale());
            }
            // Display reads cannot grant a send capability, but actual evidence
            // contradicting the previously captured SIM must retire it. A card
            // removed and reinserted later cannot revive an old prepared send.
            if state.module_identity.as_ref().is_some_and(|identity| {
                sim_unavailable
                    || identity_iccid
                        .as_deref()
                        .is_some_and(|iccid| iccid != identity.iccid.as_str())
                    || identity_imsi
                        .as_deref()
                        .is_some_and(|imsi| imsi != identity.imsi.as_str())
            }) {
                native_sms_route::invalidate_identity(&mut state);
                if state.identity_generation == 0 {
                    return Err(Failure::stopped());
                }
                observation.identity_generation = state.identity_generation;
            }
            state.device_observation = Some(observation.clone());
            Ok(observation)
        })
        .await
    }
}

/// Match Go modem.DeviceStatus's JSON defaults and omitempty fields, rather
/// than leaking the private USB wire's operator identifier/name split.
fn public_status(status: crate::usb_at_status::DeviceStatus) -> Value {
    let mut value = json!({"imei":status.imei,"firmware":status.firmware,"iccid":status.iccid,"imsi":status.imsi,
        "operator":status.operator_name,"sim_inserted":status.sim_inserted,"signal_dbm":status.signal_dbm,
        "signal_rsrp":0,"signal_rsrq":0,"reg_status":status.reg_status,"reg_status_text":status.reg_status_text,
        "ps_attached":false,"lac":"","cell_id":"","apn":"","ims_status":0,
        "network_mode":status.network_mode,"network_duplex":status.network_duplex,"usbnet_mode":status.usbnet_mode});
    if !status.phone_number.is_empty() {
        value["phone_number"] = json!(status.phone_number);
    }
    if !status.radio_band.is_empty() {
        value["radio_band"] = json!(status.radio_band);
    }
    if status.radio_channel != 0 {
        value["radio_channel"] = json!(status.radio_channel);
    }
    value
}

#[cfg(test)]
mod tests;
