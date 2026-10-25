//! Fixed native health observation, fenced to the overview's physical session.
use super::*;
impl Owner {
    pub(crate) async fn native_esim_health_status(
        &self,
        key: &EsimSnapshotKey,
    ) -> Result<Value, Failure> {
        if !self.esim_snapshot_current(key) {
            return Err(Failure::stale());
        }
        let deadline = Instant::now() + DEVICE_STATUS_BUDGET;
        let permit = self
            .admit(&key.producer, Some(&key.session), "", deadline)
            .await?;
        let key = key.clone();
        self.run(permit, move |inner, epoch| {
            let check = || {
                let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                check_epoch(&state, epoch)?;
                if !key.current(&state)
                    || state.lease.is_some()
                    || state.native_module_hold.is_some()
                    || state.radio_inhibit.is_some()
                    || !state.inhibits.is_empty()
                {
                    return Err(Failure::stale());
                }
                Ok(())
            };
            check()?;
            let suppressed = inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cellular_suppressed;
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let mut transport_failed = false;
            let status = crate::usb_at_status::collect(suppressed, |command, timeout, maximum| {
                check()?;
                check_cellular_command(inner, command)?;
                let result = port.as_mut().ok_or_else(Failure::stale)?.command(
                    command,
                    None,
                    remaining(deadline)?.min(timeout),
                    maximum.min(MAX_BYTES),
                );
                transport_failed |= result.is_err();
                check()?;
                result.map_err(Failure::from)
            });
            if transport_failed {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            let status = status?;
            check()?;
            if inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cellular_suppressed
                != suppressed
            {
                return Err(crate::module_radio_transport::failure(
                    "module_flight_mode_blocks_cellular_probe",
                ));
            }
            remaining(deadline)?;
            let mut value = serde_json::to_value(&status).map_err(|_| Failure::invalid())?;
            value["operator"] = json!(status.operator_name);
            Ok(value)
        })
        .await
    }
}
