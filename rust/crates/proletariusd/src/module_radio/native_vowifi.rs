//! In-process capability for the exact configured RF owner. Never deserialized.
use super::*;

#[derive(Clone)]
pub(crate) struct NativeVoWiFiIntent {
    service: Service,
    producer: String,
    epoch: u64,
    binding: Binding,
    identity: Identity,
    serial: Option<crate::modem_actor::SerialCardBinding>,
    preparation: Arc<tokio::sync::Mutex<PrepareAdmission>>,
}
/// Shared by every clone of one captured intent. A rejected first attempt
/// permanently closes prepare before its NotAdmitted receipt escapes. Keeping
/// InFlight on cancellation prevents a later clone claiming it was unaccepted.
enum PrepareAdmission {
    Fresh,
    InFlight,
    Accepted,
    TerminalNotAdmitted,
}
#[derive(Debug)]
pub(crate) enum NativePrepareFailure {
    NotAdmitted(Failure),
    AcceptedOrUnknown(Failure),
}
impl NativePrepareFailure {
    fn into_failure(self) -> Failure {
        match self {
            Self::NotAdmitted(failure) | Self::AcceptedOrUnknown(failure) => failure,
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) struct NativeVoWiFiStatus {
    pub(crate) cfun_mode: Option<u8>,
    pub(crate) original_mode: Option<u8>,
    pub(crate) active: bool,
    pub(crate) verified: bool,
}
pub(super) struct Request {
    pub(super) input: IntentRequest,
    epoch: u64,
    binding: Binding,
}
impl Request {
    pub(super) fn validate(&self, service: &Service, epoch: u64) -> Result<(), Failure> {
        let state = service.0.state.lock().unwrap();
        self.validate_locked(&state, epoch)
    }
    pub(super) fn validate_locked(&self, state: &State, epoch: u64) -> Result<(), Failure> {
        if epoch != self.epoch
            || state.epoch != self.epoch
            || state.producer != self.input.producer_id
            || state.binding.as_ref() != Some(&self.binding)
            || state.retiring
        {
            return Err(Failure::stale());
        }
        Ok(())
    }
}
impl Service {
    #[cfg(test)]
    pub(crate) fn pin_esim_release_for_test(&self, pinned: bool) {
        let mut state = self.0.state.lock().unwrap();
        let reservation = state.reservation.as_mut().expect("live eSIM reservation");
        assert_eq!(reservation.purpose, Purpose::Esim);
        // Exercise the real release rejection while an existing owner pin is live.
        reservation.active_reconnect_pins = usize::from(pinned);
    }

    #[cfg(test)]
    pub(crate) async fn native_vowifi_fixture(
        usb: crate::usb_at::Owner,
        session: String,
        root: PathBuf,
    ) -> Self {
        let gate = crate::module_recovery::Gate::default();
        let service = Self::new(
            root,
            usb.with_recovery_gate_for_tests(gate.clone()),
            crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap(),
            BackendAvailability::with_module_recovery(gate),
            "native-module-test".into(),
        );
        service
            .0
            .store
            .save(Preferences {
                enabled: false,
                restore_cfun: 1,
                user_configured: true,
            })
            .unwrap();
        service
            .submit(Job::Configure(Configure {
                producer_id: "native-module-test".into(),
                config_seq: 1,
                transport: "usb".into(),
                session_id: session,
            }))
            .await
            .unwrap();
        service
    }
    /// Capture before prepare so the caller retains rollback authority even if
    /// the submitted prepare acknowledgement is lost with its waiter.
    pub(crate) fn native_vowifi_intent(&self) -> Result<NativeVoWiFiIntent, Failure> {
        self.require_new_module_admission()?;
        let state = self.0.state.lock().unwrap();
        if state.stopping || state.retiring {
            return Err(Failure::stopped());
        }
        if state.intent.is_some() || state.reservation.is_some() {
            return Err(Failure::busy());
        }
        let binding = state.binding.clone().ok_or_else(Failure::unavailable)?;
        let serial = match &binding {
            Binding::Usb { .. } if !self.0.modem.native_serial_managed() => None,
            Binding::Serial { serial_session }
                if self.0.modem.native_serial_managed()
                    && state.bootstrap_receipt.as_ref() == Some(&binding) =>
            {
                Some(
                    self.0
                        .modem
                        .cached_serial_card_binding()
                        .filter(|card| card.serial_session == *serial_session)
                        .ok_or_else(Failure::unavailable)?,
                )
            }
            _ => return Err(Failure::unavailable()),
        };
        Ok(NativeVoWiFiIntent {
            service: self.clone(),
            producer: state.producer.clone(),
            epoch: state.epoch,
            binding,
            serial,
            preparation: Arc::new(tokio::sync::Mutex::new(PrepareAdmission::Fresh)),
            identity: Identity {
                token: format!("native-module-{}", crate::snapshot::new_instance_id()),
                sequence: state
                    .intent_water
                    .sequence
                    .checked_add(1)
                    .ok_or_else(Failure::stale)?,
            },
        })
    }
}
impl NativeVoWiFiIntent {
    pub(crate) fn serial_binding(&self) -> Option<crate::modem_actor::SerialCardBinding> {
        self.serial.clone()
    }
    /// Borrow only the actual accepted RF intent's operation. Merely capturing
    /// this capability before prepare does not admit a module hold, and a
    /// released or replaced intent cannot reopen one through its old token.
    pub(crate) fn retained_operation_right(
        &self,
    ) -> Result<crate::module_recovery::OperationRight, Failure> {
        let state = self.service.0.state.lock().unwrap();
        if state.retiring
            || state.epoch != self.epoch
            || state.producer != self.producer
            || state.binding.as_ref() != Some(&self.binding)
        {
            return Err(Failure::stale());
        }
        let owner = state
            .intent
            .as_ref()
            .filter(|owner| owner.identity == self.identity)
            .ok_or_else(Failure::stale)?;
        let right = owner.operation.right();
        if !right.is_active_for(self.service.module_recovery(), &self.producer) {
            return Err(Service::admission_failure());
        }
        Ok(right)
    }
    pub(crate) fn binding_current(&self) -> bool {
        let state = self.service.0.state.lock().unwrap();
        state.epoch == self.epoch
            && state.producer == self.producer
            && !state.retiring
            && !state.stopping
            && state.binding.as_ref() == Some(&self.binding)
    }
    pub(crate) async fn retire_confirmed_usb_loss(&self) -> Result<bool, Failure> {
        let _gate = self.service.0.gate.lock().await;
        if !self.service.0.usb.native_module_closed(self) {
            return Ok(false);
        }
        let mut state = self.service.0.state.lock().unwrap();
        if state.epoch != self.epoch
            || state.producer != self.producer
            || state.binding.as_ref() != Some(&self.binding)
            || !state
                .intent
                .as_ref()
                .is_some_and(|intent| intent.identity == self.identity)
        {
            return Err(Failure::stale());
        }
        self.service
            .0
            .usb
            .release_native_module(self)
            .map_err(Failure::from)?;
        state.intent = None;
        state.intent_water.released = true;
        state.observed = Outcome::default();
        state.observed.error_code = "module_vowifi_radio_lost".into();
        self.service.sync_suppression(&state);
        self.service.0.wake.notify_one();
        Ok(true)
    }

    /// Only the retained actor capability can prove physical closure. A changed
    /// SIM generation or missing current route is not permission to skip RF cleanup.
    pub(crate) async fn retire_confirmed_serial_loss(
        &self,
        hold: &crate::modem_actor::SerialModuleHold,
    ) -> Result<bool, Failure> {
        let _gate = self.service.0.gate.lock().await;
        if self.serial.as_ref() != Some(&hold.binding())
            || !self
                .service
                .0
                .modem
                .serial_module_hold_matches(hold.binding().serial_session, &self.identity.token)
            || !self.service.0.modem.serial_module_closed(hold)
        {
            return Ok(false);
        }
        let mut state = self.service.0.state.lock().unwrap();
        if state.epoch != self.epoch
            || state.producer != self.producer
            || state.binding.as_ref() != Some(&self.binding)
            || state
                .intent
                .as_ref()
                .is_some_and(|intent| intent.identity != self.identity)
            || state.intent_water.sequence > self.identity.sequence
            || state.intent_water.sequence == self.identity.sequence
                && state.intent_water.token != self.identity.token
        {
            return Err(Failure::stale());
        }
        state.intent = None;
        state.intent_water = Water {
            sequence: self.identity.sequence,
            token: self.identity.token.clone(),
            released: true,
        };
        state.observed = Outcome::default();
        state.observed.error_code = "module_vowifi_radio_lost".into();
        self.service.sync_suppression(&state);
        self.service.0.wake.notify_one();
        Ok(true)
    }

    pub(crate) fn usb_inhibit(&self) -> &str {
        &self.identity.token
    }
    pub(crate) fn status(&self) -> NativeVoWiFiStatus {
        let state = self.service.0.state.lock().unwrap();
        let intent = state.intent.as_ref().filter(|v| {
            v.identity == self.identity
                && state.epoch == self.epoch
                && state.producer == self.producer
                && state.binding.as_ref() == Some(&self.binding)
                && !state.retiring
        });
        NativeVoWiFiStatus {
            cfun_mode: if intent.is_some()
                || (state.epoch == self.epoch
                    && state.producer == self.producer
                    && !state.retiring
                    && state.binding.as_ref() == Some(&self.binding)
                    && state.intent.is_none()
                    && state.intent_water.released
                    && state.intent_water.sequence == self.identity.sequence
                    && state.intent_water.token == self.identity.token)
            {
                state.observed.cfun
            } else {
                None
            },
            original_mode: intent.and_then(|v| v.restore_mode),
            active: intent.is_some_and(|v| v.active),
            verified: intent.is_some() && state.observed.complete && state.observed.verified,
        }
    }
    pub(crate) fn usb_binding(&self) -> (&str, &str) {
        match &self.binding {
            Binding::Usb {
                producer_id,
                session_id,
            } => (producer_id, session_id),
            _ => unreachable!("native module capture requires USB"),
        }
    }
    pub(crate) fn current(&self) -> bool {
        let state = self.service.0.state.lock().unwrap();
        state.epoch == self.epoch
            && state.producer == self.producer
            && !state.retiring
            && !state.stopping
            && state.binding.as_ref() == Some(&self.binding)
            && state
                .intent
                .as_ref()
                .is_some_and(|v| v.identity == self.identity)
    }
    fn request_job(&self, action: &str) -> Job {
        Job::NativeIntent(Request {
            input: IntentRequest {
                producer_id: self.producer.clone(),
                action: action.into(),
                token: self.identity.token.clone(),
                sequence: self.identity.sequence,
            },
            epoch: self.epoch,
            binding: self.binding.clone(),
        })
    }
    async fn request(&self, action: &str) -> Result<Value, Failure> {
        self.service.submit(self.request_job(action)).await
    }
    pub(crate) async fn prepare(&self) -> Result<(), Failure> {
        self.prepare_with_receipt()
            .await
            .map_err(NativePrepareFailure::into_failure)
    }
    /// Only the first, definitely unqueued attempt can prove no RF prepare was
    /// admitted. Clones serialize here; once this receipt is issued the shared
    /// capability can never submit prepare again. Accepted failures and a lost
    /// waiter retain the existing restore obligation conservatively.
    pub(crate) async fn prepare_with_receipt(&self) -> Result<(), NativePrepareFailure> {
        let mut preparation = self.preparation.lock().await;
        if matches!(*preparation, PrepareAdmission::TerminalNotAdmitted) {
            return Err(NativePrepareFailure::AcceptedOrUnknown(Failure::stale()));
        }
        let first = matches!(*preparation, PrepareAdmission::Fresh);
        *preparation = PrepareAdmission::InFlight;
        match self
            .service
            .submit_with_receipt(self.request_job("prepare"))
            .await
        {
            Ok(_) => {
                *preparation = PrepareAdmission::Accepted;
                Ok(())
            }
            Err(error) if first && !error.admitted => {
                *preparation = PrepareAdmission::TerminalNotAdmitted;
                Err(NativePrepareFailure::NotAdmitted(error.failure))
            }
            Err(error) => {
                *preparation = PrepareAdmission::Accepted;
                Err(NativePrepareFailure::AcceptedOrUnknown(error.failure))
            }
        }
    }
    pub(crate) async fn enter(&self) -> Result<(), Failure> {
        self.request("enter").await.map(|_| ())
    }
    pub(crate) async fn validate(&self) -> Result<(), Failure> {
        let value = self.request("observe").await?;
        if value["verified"] != true {
            return Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "module_vowifi_radio_unconfirmed",
                "模块射频状态读取未确认",
            ));
        }
        if value["active"] != true {
            return Err(Failure::stale());
        }
        if value["cfun_mode"] != 4 {
            return Err(Failure::new(
                StatusCode::CONFLICT,
                "module_vowifi_radio_changed",
                "模块未处于已确认的 VoWiFi 射频模式",
            ));
        }
        Ok(())
    }
    pub(crate) async fn restore(&self) -> Result<(), Failure> {
        let value = self.request("restore").await?;
        if value["restored"] != true || value["active"] != false {
            return Err(Failure::stale());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn captured_old_epoch_never_prepares_or_restores_replacement() {
        let root = std::env::temp_dir().join(format!(
            "native-radio-{}",
            crate::snapshot::new_instance_id()
        ));
        struct Port;
        impl crate::usb_at_transport::Transport for Port {
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

            fn command(
                &mut self,
                command: &str,
                _: Option<&[u8]>,
                _: Duration,
                _: usize,
            ) -> Result<String, crate::usb_at_transport::TransportError> {
                Ok(if command == "AT+CFUN?" {
                    "+CFUN: 1\r\nOK"
                } else {
                    "OK"
                }
                .into())
            }
            fn description(&self) -> String {
                "native RF epoch test".into()
            }
        }
        let (usb, session) =
            crate::usb_at::Owner::native_module_fixture(Arc::new(|| Ok(Box::new(Port)))).await;
        let service =
            Service::native_vowifi_fixture(usb.clone(), session.clone(), root.clone()).await;
        let old = service.native_vowifi_intent().unwrap();
        old.prepare().await.unwrap();
        service
            .retire_producer("native-module-test".into())
            .await
            .unwrap();
        service
            .submit(Job::Configure(Configure {
                producer_id: "native-module-test".into(),
                config_seq: 1,
                transport: "usb".into(),
                session_id: session,
            }))
            .await
            .unwrap();
        let replacement = service.native_vowifi_intent().unwrap();
        replacement.prepare().await.unwrap();
        assert!(!old.current());
        assert!(old.prepare().await.is_err());
        assert!(old.restore().await.is_err());
        assert!(replacement.current());
        assert!(service.vowifi_owns_sim_channel());
        replacement.restore().await.unwrap();
        assert!(!service.vowifi_owns_sim_channel());
        service.finish_shutdown().await.unwrap();
        usb.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

pub(super) struct IdleObservation {
    pub(super) producer: String,
    epoch: u64,
    binding: Binding,
}
impl IdleObservation {
    pub(super) fn validate(&self, service: &Service, epoch: u64) -> Result<(), Failure> {
        let state = service.0.state.lock().unwrap();
        if epoch != self.epoch
            || state.epoch != self.epoch
            || state.producer != self.producer
            || state.binding.as_ref() != Some(&self.binding)
        {
            return Err(Failure::stale());
        }
        if state.intent.is_some() || state.reservation.is_some() {
            return Err(Failure::busy());
        }
        Ok(())
    }
}
impl Service {
    /// The configured native route is authoritative. A serial selection with
    /// no current session must not borrow an unrelated USB session.
    pub(crate) fn native_vowifi_route_available(&self) -> bool {
        let state = self.0.state.lock().unwrap();
        if state.stopping || state.retiring {
            return false;
        }
        match state.binding.as_ref() {
            Some(Binding::Usb {
                producer_id,
                session_id,
            }) => {
                !self.0.modem.native_serial_managed()
                    && self
                        .0
                        .usb
                        .validate_radio_binding(producer_id, session_id)
                        .is_ok()
            }
            Some(Binding::Serial { serial_session }) => {
                self.0.modem.native_serial_managed()
                    && state.bootstrap_receipt == state.binding
                    && self.0.modem.call_binding_current(*serial_session)
            }
            None => false,
        }
    }
    pub(crate) async fn native_vowifi_idle_mode(&self) -> Result<u8, Failure> {
        let request = {
            let state = self.0.state.lock().unwrap();
            if state.stopping || state.retiring {
                return Err(Failure::stopped());
            }
            let binding = state.binding.clone().ok_or_else(Failure::unavailable)?;
            match &binding {
                Binding::Usb { .. } if !self.0.modem.native_serial_managed() => {}
                Binding::Serial { serial_session }
                    if self.0.modem.native_serial_managed()
                        && state.bootstrap_receipt.as_ref() == Some(&binding)
                        && self.0.modem.call_binding_current(*serial_session) => {}
                _ => return Err(Failure::unavailable()),
            }
            IdleObservation {
                producer: state.producer.clone(),
                epoch: state.epoch,
                binding,
            }
        };
        let value = self.submit(Job::NativeIdleObserve(request)).await?;
        value["cfun_mode"]
            .as_u64()
            .and_then(|v| u8::try_from(v).ok())
            .ok_or_else(Failure::unavailable)
    }
}
