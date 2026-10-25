//! Typed SIM capability, bound to the physical USB session and identity epoch.
use super::*;
use crate::reader_session_lease::ReaderAkaApplication;
use proletarius_vowifi::SimAkaResult;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(crate) struct NativeModuleIdentity {
    producer: String,
    session: String,
    generation: u64,
    metadata: Value,
    pub(super) radio_permit: String,
}
impl NativeModuleIdentity {
    pub(crate) fn producer_id(&self) -> &str {
        &self.producer
    }
    pub(crate) fn session_id(&self) -> &str {
        &self.session
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn metadata(&self) -> Value {
        self.metadata.clone()
    }
}
impl Owner {
    #[cfg(test)]
    pub(crate) async fn native_module_fixture(factory: Factory) -> (Self, String) {
        let owner = Self::with_factory_and_adopt("native-module-test".into(), factory, false);
        owner.set_cellular_suppressed(false);
        let response = owner
            .open(
                OpenRequest {
                    producer_id: "native-module-test".into(),
                    lease_id: String::new(),
                },
                false,
            )
            .await
            .unwrap();
        let session = response["session_id"].as_str().unwrap().to_owned();
        (owner, session)
    }
    pub(crate) fn native_module_current(&self, binding: &NativeModuleIdentity) -> bool {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        !state.revoking
            && !state.stopping
            && state.producer == binding.producer
            && state.session == binding.session
            && state.identity_generation == binding.generation
            && state.radio_inhibit.as_deref() == Some(binding.radio_permit.as_str())
            && state
                .module_identity
                .as_ref()
                .is_some_and(|i| i.generation == binding.generation)
    }
    pub(super) fn native_module_session(
        &self,
        producer: &str,
        session: &str,
    ) -> Result<(), Failure> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.revoking
            || state.stopping
            || session.is_empty()
            || state.producer != producer
            || state.session != session
        {
            return Err(Failure::stale());
        }
        Ok(())
    }
    pub(crate) async fn native_module_identity(
        &self,
        intent: &crate::module_radio::NativeVoWiFiIntent,
    ) -> Result<NativeModuleIdentity, Failure> {
        if !intent.current() {
            return Err(Failure::stale());
        }
        let (producer, session) = intent.usb_binding();
        let radio_permit = intent.usb_inhibit().to_owned();
        self.native_module_session(producer, session)?;
        let response = self
            .sim_identity_bound(
                false,
                Some((producer.into(), session.into())),
                Some(radio_permit.clone()),
            )
            .await?;
        self.native_module_session(producer, session)?;
        let generation = response["session_generation"]
            .as_u64()
            .filter(|v| *v != 0)
            .ok_or_else(Failure::stale)?;
        let mut metadata = response["metadata"].clone();
        if metadata["phone_number"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty())
        {
            metadata["phone_number"] = response["phone_number"].clone();
        }
        let binding = NativeModuleIdentity {
            producer: producer.into(),
            session: session.into(),
            generation,
            metadata,
            radio_permit,
        };
        if !self.native_module_current(&binding) {
            return Err(Failure::stale());
        }
        Ok(binding)
    }
    pub(crate) async fn native_module_validate(
        &self,
        binding: &NativeModuleIdentity,
    ) -> Result<(), Failure> {
        if !self.native_module_current(binding) {
            return Err(Failure::stale());
        }
        let result = self
            .sim_profile_check_bound(
                SimProfileCheckRequest {
                    session_generation: binding.generation,
                },
                false,
                Some(binding.clone()),
            )
            .await?;
        if !self.native_module_current(binding) {
            return Err(Failure::stale());
        }
        match result["outcome"].as_str() {
            Some("identity_matches") => Ok(()),
            Some("identity_changed") => Err(Failure::new(
                StatusCode::CONFLICT,
                "module_sim_identity_changed",
                "模块 SIM 身份已变更",
            )),
            _ => Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "module_sim_identity_failed",
                "模块 SIM 身份读取失败",
            )),
        }
    }
    pub(crate) async fn native_module_authenticate(
        &self,
        binding: NativeModuleIdentity,
        application: ReaderAkaApplication,
        rand: [u8; 16],
        autn: [u8; 16],
    ) -> Result<SimAkaResult, Failure> {
        let rand = Zeroizing::new(rand);
        let autn = Zeroizing::new(autn);
        if !self.native_module_current(&binding) {
            return Err(Failure::stale());
        }
        let (iccid, imsi) = self.module_identity_for_generation(binding.generation)?;
        let radio_permit = binding.radio_permit.clone();
        let expected = binding.clone();
        let owner = self.clone();
        let result = self
            .sim_identity_transaction_with_radio(
                move |command, generation| {
                    if generation != expected.generation || !owner.native_module_current(&expected)
                    {
                        return Err(Failure::stale());
                    }
                    crate::module_sim_identity::authenticate(
                        command,
                        application,
                        &iccid,
                        &imsi,
                        &rand,
                        &autn,
                    )
                },
                Some(radio_permit),
            )
            .await?;
        if !self.native_module_current(&binding) {
            return Err(Failure::stale());
        }
        Ok(result)
    }
}

// Created only from an in-process typed RF intent; wire callers cannot mint it.
pub(super) struct Hold {
    producer: String,
    pub(super) session: String,
    pub(super) token: String,
    // Host resources closed only; RF/DSP/settings need independent proof.
    closed: bool,
}
pub(super) fn record_close(
    inner: &Inner,
    identity: PortIdentity,
    closed: Result<CloseDisposition, Failure>,
) {
    if let Err(error) = &closed {
        if let Some(anchor) = identity
            .reconnect
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        {
            anchor.note_close_failure(error.clone());
        }
    }
    let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(error) = &closed {
        if !state.close_failures.iter().any(|(old, _)| {
            old.producer == identity.producer
                && old.epoch == identity.epoch
                && old.connection == identity.connection
        }) {
            state.close_failures.push((identity.clone(), error.clone()));
        }
    } else if let Some(hold) = &mut state.native_module_hold {
        if hold.producer == identity.producer
            && hold.session == identity.session
            && !identity.session.is_empty()
        {
            hold.closed = true;
        }
    }
    state.last_close_result = Some((identity, closed));
    inner.changed.notify_waiters();
}
pub(super) fn retire_port(
    inner: &Inner,
    identity: PortIdentity,
    port: Box<dyn Transport>,
) -> Result<(), Failure> {
    let closed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| port.checked_close()))
        .unwrap_or_else(|_| Err(usb_at_transport::close_unconfirmed()))
        .map_err(Failure::from);
    record_close(inner, identity, closed.clone());
    closed.map(|_| ())
}
pub(super) fn close_port(
    inner: &Inner,
    port: &mut Option<Box<dyn Transport>>,
) -> Result<(), Failure> {
    let Some(port) = port.take() else {
        let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        return state
            .last_close_result
            .as_ref()
            .map_or(Ok(()), |(_, result)| result.clone().map(|_| ()));
    };
    // Pin the identity belonging to this actual port before the consuming call.
    // Revoke may already have retired its session or changed the producer.
    let identity = inner
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .port_identity
        .take();
    match identity {
        Some(identity) => retire_port(inner, identity, port),
        None => {
            // An unbound port cannot prove anybody's close, even if its release
            // succeeds. This is not a production installation path.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| port.checked_close()));
            let identity =
                PortIdentity::current(&inner.state.lock().unwrap_or_else(|e| e.into_inner()));
            let error = Failure::from(usb_at_transport::close_unconfirmed());
            record_close(inner, identity, Err(error.clone()));
            Err(error)
        }
    }
}

impl Owner {
    pub(crate) fn retain_native_module(
        &self,
        intent: &crate::module_radio::NativeVoWiFiIntent,
    ) -> Result<(), Failure> {
        if !intent.binding_current() {
            return Err(Failure::stale());
        }
        let (producer, session) = intent.usb_binding();
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check(&state, producer)?;
        if state.session != session || session.is_empty() {
            return Err(Failure::stale());
        }
        if let Some(hold) = &state.native_module_hold {
            return if hold.producer == producer
                && hold.session == session
                && hold.token == intent.usb_inhibit()
            {
                Ok(())
            } else {
                Err(Failure::busy())
            };
        }
        if state.lease.is_some() || !state.inhibits.is_empty() || state.radio_inhibit.is_some() {
            return Err(Failure::busy());
        }
        // A captured identity is not an accepted operation. New holds require
        // live admission; releasing the exact existing hold remains available.
        let _admission = new_io_authority(&state, producer)?;
        state.native_module_hold = Some(Hold {
            producer: producer.into(),
            session: session.into(),
            token: intent.usb_inhibit().into(),
            closed: false,
        });
        Ok(())
    }
    pub(crate) fn native_module_closed(
        &self,
        intent: &crate::module_radio::NativeVoWiFiIntent,
    ) -> bool {
        let (producer, session) = intent.usb_binding();
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        !state.busy
            && !state.revoking
            && state.native_module_hold.as_ref().is_some_and(|hold| {
                hold.producer == producer
                    && hold.session == session
                    && hold.token == intent.usb_inhibit()
                    && hold.closed
            })
    }
    pub(crate) fn release_native_module(
        &self,
        intent: &crate::module_radio::NativeVoWiFiIntent,
    ) -> Result<(), Failure> {
        let (producer, session) = intent.usb_binding();
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.busy || state.revoking {
            return Err(Failure::busy());
        }
        if let Some(error) = close_failure_for(&state, producer, session) {
            return Err(error);
        }
        if state.native_module_hold.as_ref().is_some_and(|hold| {
            hold.producer == producer
                && hold.session == session
                && hold.token == intent.usb_inhibit()
        }) {
            if state.radio_inhibit.as_deref() == Some(intent.usb_inhibit()) {
                state.radio_inhibit = None;
            }
            state.native_module_hold = None;
            self.inner.changed.notify_waiters();
            Ok(())
        } else if state.native_module_hold.is_none() {
            Ok(())
        } else {
            Err(Failure::stale())
        }
    }
}
