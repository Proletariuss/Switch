use super::*;

impl Service {
    /// A current baseline is an observation, not consent to apply an old
    /// startup flight preference. Explicit public operations remain unchanged.
    pub(crate) fn preserve_current_startup_cfun(&self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .preserve_current_cfun = true;
    }

    /// Bind only the physical session selected by the native serial workflow.
    /// The expectation travels with the queued job; waiting cannot adopt a
    /// connection opened by an unrelated call-mode/reconnect operation.
    pub(crate) async fn configure_native_serial(&self, session: u64) -> Result<(), Failure> {
        if !self.0.modem.native_serial_managed() || !self.0.modem.call_binding_current(session) {
            return Err(Failure::stale());
        }
        let input = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopping || state.retiring {
                return Err(Failure::stale());
            }
            if state.binding
                == Some(Binding::Serial {
                    serial_session: session,
                })
            {
                return Ok(());
            }
            Configure {
                producer_id: state.producer.clone(),
                config_seq: state.config_seq.checked_add(1).ok_or_else(Failure::stale)?,
                transport: "serial".into(),
                session_id: String::new(),
            }
        };
        self.submit(Job::ConfigureNativeSerial {
            input,
            serial_session: session,
        })
        .await?;
        if self.serial_transport_session() != Some(session)
            || !self.0.modem.call_binding_current(session)
        {
            return Err(Failure::stale());
        }
        Ok(())
    }

    pub(crate) async fn bootstrap_native_serial(&self, session: u64) -> Result<(), Failure> {
        if !self.0.modem.native_serial_managed() || !self.0.modem.call_binding_current(session) {
            return Err(Failure::stale());
        }
        let producer_id = self
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .producer
            .clone();
        self.submit(Job::BootstrapNativeSerial {
            input: Producer { producer_id },
            serial_session: session,
        })
        .await?;
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.bootstrap_receipt
            != Some(Binding::Serial {
                serial_session: session,
            })
            || !self.0.modem.call_binding_current(session)
        {
            return Err(Failure::stale());
        }
        Ok(())
    }

    pub(crate) async fn configure_native_usb(&self, session: &str) -> Result<(), Failure> {
        if !self.0.usb.native_managed() {
            return Err(Failure::invalid());
        }
        let input = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopping || state.retiring {
                return Err(Failure::stale());
            }
            if matches!(&state.binding, Some(Binding::Usb { session_id, .. }) if session_id == session)
            {
                return self
                    .0
                    .usb
                    .validate_radio_binding(&state.producer, session)
                    .map_err(Failure::from);
            }
            Configure {
                producer_id: state.producer.clone(),
                config_seq: state.config_seq.checked_add(1).ok_or_else(Failure::stale)?,
                transport: "usb".into(),
                session_id: session.to_owned(),
            }
        };
        self.submit(Job::Configure(input)).await.map(|_| ())
    }

    pub(crate) async fn bootstrap_native_usb(&self) -> Result<(), Failure> {
        if !self.0.usb.native_managed() {
            return Err(Failure::invalid());
        }
        let producer_id = self
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .producer
            .clone();
        self.submit(Job::Bootstrap(Producer { producer_id }))
            .await?;
        if !self.usb_esim_route_available() {
            return Err(Failure::stale());
        }
        Ok(())
    }

    /// Best-effort read-only eSIM warmup, with the same RF reservation as the UI.
    /// A missing eUICC is not a reason to disable ordinary USB SMS.
    pub(crate) async fn warm_native_esim(&self) -> Result<(), Failure> {
        if !self.usb_esim_route_available() || self.sim_channel_busy() {
            return Err(Failure::busy());
        }
        let _single = self.0.usb.esim_snapshot_guard().await;
        let reservation = self.reserve_esim_read().await?;
        let result = self
            .0
            .usb
            .native_esim_overview(false)
            .await
            .map(|_| ())
            .map_err(Failure::from);
        self.release_esim(reservation).await.map_err(|mut error| {
            error.code = "module_startup_warm_cleanup_unconfirmed";
            error
        })?;
        result
    }
}
