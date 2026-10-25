//! Native SMS capabilities are exact actor/session/SIM identities. Legacy RPC
//! generation numbers never mint or upgrade this capability.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SerialSmsBinding {
    pub(crate) owner_epoch: String,
    pub(crate) serial_session: u64,
    pub(crate) sim_generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct SerialSmsObservation {
    pub(crate) present: bool,
    pub(crate) serial_session: Option<u64>,
    pub(crate) initialized: bool,
    pub(crate) sim_present: Option<bool>,
    pub(crate) ready: bool,
    pub(crate) unknown: bool,
    pub(crate) pin_blocked: bool,
    pub(crate) phone: String,
    pub(crate) binding: Option<SerialSmsBinding>,
}

#[derive(Default)]
pub(super) struct SerialSmsState {
    pub(super) owner_epoch: String,
    sim_present: Option<bool>,
    sim_ready: bool,
    pin_blocked: bool,
    generation_exhausted: bool,
    pub(super) phone: String,
}
impl SerialSmsState {
    pub(super) fn generation_exhausted(&self) -> bool {
        self.generation_exhausted
    }
    pub(super) fn reset_sim(&mut self) {
        self.sim_present = None;
        self.sim_ready = false;
        self.pin_blocked = false;
        self.generation_exhausted = false;
        self.phone.clear();
    }

    pub(super) fn observe_sim(&mut self, event: Option<&SimEvent>, generation: u64) {
        let Some(event) = event else { return };
        // Preserve the compatibility counter's existing semantics, but never
        // allow its wraparound to revalidate a strict native capability.
        let increment = if matches!(event, SimEvent::PinState { state } if state == "READY") {
            2
        } else {
            1
        };
        self.generation_exhausted |= generation.checked_add(increment).is_none();
        self.phone.clear();
        self.sim_ready = false;
        self.sim_present = match event {
            SimEvent::Ready => {
                self.pin_blocked = false;
                None
            }
            SimEvent::Status { inserted } => {
                if *inserted == Some(false) {
                    self.pin_blocked = false;
                }
                self.sim_ready =
                    *inserted == Some(true) && !self.pin_blocked && !self.generation_exhausted;
                *inserted
            }
            SimEvent::PinState { state } if state == "READY" => {
                self.pin_blocked = false;
                self.sim_ready = !self.generation_exhausted;
                Some(true)
            }
            SimEvent::PinState { state }
                if matches!(
                    state.as_str(),
                    "SIM PIN" | "SIM PUK" | "PH-SIM PIN" | "PH-NET PIN"
                ) =>
            {
                self.pin_blocked = true;
                Some(true)
            }
            SimEvent::PinState { .. } => {
                self.pin_blocked = true;
                None
            }
        };
    }
}

pub(super) fn new_owner_epoch() -> io::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| io::Error::other("serial owner identity unavailable"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

pub(super) fn binding_changed() -> ActorError {
    ActorError::new(
        "serial_sms_binding_changed",
        "serial SMS capability is no longer current",
    )
}

impl ModemActor {
    /// Cache-only observation; performs no serial query and accepts no Go facts.
    pub(crate) fn serial_sms_observation(&self) -> Result<SerialSmsObservation, ActorError> {
        self.submit(|reply| Command::SerialSmsObservation { reply }, RPC_TIMEOUT)
    }

    pub(crate) fn capture_serial_sms_binding(&self) -> Result<SerialSmsBinding, ActorError> {
        self.serial_sms_observation()?.binding.ok_or_else(|| {
            ActorError::new(
                "serial_sms_not_ready",
                "serial SMS initialization or SIM state is not confirmed",
            )
        })
    }

    pub(crate) fn serial_sms_binding_current(
        &self,
        binding: &SerialSmsBinding,
    ) -> Result<bool, ActorError> {
        let binding = binding.clone();
        self.submit(
            |reply| Command::SerialSmsBindingCurrent { binding, reply },
            RPC_TIMEOUT,
        )
    }

    pub(crate) fn submit_sms_bound(
        &self,
        recipient: String,
        message: String,
        encoding: String,
        binding: SerialSmsBinding,
    ) -> Result<SmsSubmitResult, ActorError> {
        self.submit_sms_with_binding(
            recipient,
            message,
            encoding,
            Some(binding.sim_generation),
            Some(binding),
        )
    }
}

impl ActorState {
    pub(super) fn strict_sms_ready(&self) -> bool {
        self.port.is_some()
            && (!self.native.managed() || self.native.ready)
            && self.serial_session != 0
            && !self.serial_sms.owner_epoch.is_empty()
            && !self.serial_sms.generation_exhausted
            && self.serial_sms.sim_ready
            && matches!(&self.initialization.cached, Some(Ok(result))
                if result.completed && result.probe_succeeded && result.sms_pdu_ready)
    }

    pub(super) fn serial_sms_binding_current(&self, binding: &SerialSmsBinding) -> bool {
        self.strict_sms_ready()
            && binding.owner_epoch == self.serial_sms.owner_epoch
            && binding.serial_session == self.serial_session
            && binding.sim_generation == self.sms_generation
    }

    /// Once CMGS has reached the wire, another AT command could be consumed
    /// as its payload. Settle this exact physical session before releasing the
    /// result; never send ESC or close a replacement session.
    pub(super) fn quarantine_invalid_sms_prompt(&mut self) -> bool {
        let invalid = self.pending_command.as_ref().is_some_and(|pending| {
            pending.follow_up.is_some() && matches!(&pending.completion,
                CommandCompletion::SubmitSms(transaction)
                    if transaction.binding.is_some() && matches!(transaction.step, SmsSubmitStep::Segment)
                        && !self.sms_submit_current(transaction))
        });
        if !invalid {
            return false;
        }
        let pending = self
            .pending_command
            .take()
            .expect("invalid SMS prompt checked");
        let CommandCompletion::SubmitSms(transaction) = pending.completion else {
            unreachable!()
        };
        let binding = transaction
            .binding
            .as_ref()
            .expect("strict SMS binding checked");
        if binding.owner_epoch == self.serial_sms.owner_epoch
            && binding.serial_session == self.serial_session
        {
            self.retire_physical_port("serial_sms_binding_changed");
            self.close_session();
        }
        let _ = transaction.reply.send(Ok(SmsSubmitResult {
            accepted_segments: transaction.accepted_segments,
            complete: false,
        }));
        true
    }

    pub(super) fn sms_submit_current(&self, transaction: &SmsSubmitTransaction) -> bool {
        transaction.expected_sms_generation == self.sms_generation
            && transaction
                .binding
                .as_ref()
                .is_none_or(|binding| self.serial_sms_binding_current(binding))
    }

    pub(super) fn serial_sms_observation(&self) -> SerialSmsObservation {
        let present = self.port.is_some();
        let initialized = present
            && matches!(&self.initialization.cached, Some(Ok(result))
            if result.completed && result.probe_succeeded && result.sms_pdu_ready);
        let ready = self.strict_sms_ready();
        let sim_present = if present {
            self.serial_sms.sim_present
        } else {
            None
        };
        SerialSmsObservation {
            pin_blocked: present && self.serial_sms.pin_blocked,
            present,
            serial_session: present.then_some(self.serial_session),
            initialized,
            sim_present,
            ready,
            unknown: present
                && (!initialized || sim_present.is_none() || (sim_present == Some(true) && !ready)),
            // Cache admission and invalidation use the same complete fence.
            phone: if ready {
                self.serial_sms.phone.clone()
            } else {
                String::new()
            },
            binding: ready.then(|| SerialSmsBinding {
                owner_epoch: self.serial_sms.owner_epoch.clone(),
                serial_session: self.serial_session,
                sim_generation: self.sms_generation,
            }),
        }
    }
}
