//! Copy bounded ownership facts under the radio lock; emit only after unlocking.
use super::*;
use crate::support_diagnostics::module::{
    self as evidence, Code, Owner, OwnerFacts, ResultKind, Stage, Transport,
};

#[derive(Clone, Copy)]
pub(super) struct Snapshot {
    pub(super) transport: Transport,
    pub(super) owner: OwnerFacts,
}
impl Snapshot {
    pub(super) fn capture(state: &State) -> Self {
        let count = |value: usize| value.min(u16::MAX as usize) as u16;
        let owner = if state.intent.is_some() {
            Owner::Vowifi
        } else if let Some(reservation) = &state.reservation {
            match reservation.purpose {
                Purpose::Esim => Owner::Esim,
                Purpose::ModuleProfileNotes => Owner::ModuleProfileNotes,
                Purpose::DeviceSms => Owner::DeviceSms,
                Purpose::Call => Owner::Call,
                Purpose::CallMode => Owner::CallMode,
                Purpose::Reboot => Owner::Reboot,
                Purpose::Diagnostic => Owner::Diagnostic,
            }
        } else if state.active_shared_voice_pins != 0 {
            Owner::SharedVoice
        } else {
            Owner::None
        };
        Self {
            transport: match &state.binding {
                Some(Binding::Usb { .. }) => Transport::Usb,
                Some(Binding::Serial { .. }) => Transport::Serial,
                None => Transport::Unknown,
            },
            owner: OwnerFacts {
                owner,
                shared_voice_pins: count(state.active_shared_voice_pins),
                voice_pins: count(
                    state
                        .reservation
                        .as_ref()
                        .map_or(0, |r| r.active_voice_pins),
                ),
                reconnect_pins: count(
                    state
                        .reservation
                        .as_ref()
                        .map_or(0, |r| r.active_reconnect_pins),
                ),
                queued_jobs: count(state.jobs),
                outcome_unknown: state.observed.uncertain
                    || state.reservation.as_ref().is_some_and(|r| r.unknown),
            },
        }
    }
}

impl Service {
    #[cfg(test)]
    pub(crate) fn module_diagnostics_for_test(
        &self,
    ) -> tokio::sync::mpsc::Receiver<evidence::Event> {
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        *self.0.module_diagnostic_fixture.lock().unwrap() = Some(sender);
        receiver
    }

    pub(super) fn module_diagnostic_snapshot(&self) -> Snapshot {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        Snapshot::capture(&state)
    }

    pub(super) fn record_module_snapshot(
        &self,
        stage: Stage,
        result: ResultKind,
        code: Code,
        snapshot: Snapshot,
        elapsed: Duration,
    ) {
        evidence::record(
            stage,
            result,
            code,
            snapshot.transport,
            snapshot.owner,
            elapsed,
        );
        #[cfg(test)]
        if let Some(sender) = self.0.module_diagnostic_fixture.lock().unwrap().as_ref() {
            let _ = sender.try_send(evidence::Event {
                stage,
                result,
                code,
                transport: snapshot.transport,
                owner: snapshot.owner,
                duration_ms: elapsed.as_millis().min(u64::MAX as u128) as u64,
                suppressed_repeats: 0,
            });
        }
    }

    /// Records a completed attempt, including exact cleanup ACK replay. Owner
    /// facts are a later snapshot, not a claim about the atomic rejection cause.
    pub(super) fn record_call_result<T>(
        &self,
        stage: Stage,
        result: &Result<T, Failure>,
        elapsed: Duration,
    ) {
        let (result, code) = match result {
            Ok(_) => (ResultKind::Succeeded, Code::None),
            Err(error) => (ResultKind::Failed, Code::from_error(error.code)),
        };
        self.record_module_snapshot(stage, result, code, self.module_diagnostic_snapshot(), elapsed);
    }

    pub(super) fn record_call_query(
        &self,
        stage: Stage,
        result: &Result<CallStateSnapshot, Failure>,
        elapsed: Duration,
    ) {
        let (result, code) = match result {
            Ok(snapshot) => (ResultKind::Succeeded, if snapshot.safe_idle {
                Code::CallVoiceIdle
            } else if matches!(snapshot.display.get("state").and_then(Value::as_str),
                Some("active" | "dialing" | "alerting" | "incoming")) {
                Code::CallVoicePresent
            } else {
                Code::CallUnconfirmed
            }),
            Err(error) => (ResultKind::Failed, Code::from_error(error.code)),
        };
        self.record_module_snapshot(stage, result, code, self.module_diagnostic_snapshot(), elapsed);
    }

    pub(super) async fn submit_usbnet_with_diagnostics(
        &self,
        job: Job,
        binding: &str,
    ) -> Result<Value, Failure> {
        let started = Instant::now();
        self.record_module_snapshot(
            Stage::Admission,
            ResultKind::Started,
            Code::None,
            self.module_diagnostic_snapshot(),
            Duration::ZERO,
        );
        let result = self.submit_with_binding(job, Some(binding)).await;
        let (stage, outcome, code) = match &result {
            Ok(_) => (Stage::Physical, ResultKind::Succeeded, Code::None),
            Err(error) => (
                // This records actor admission, not proof of a physical write.
                // The error code is the returned failure; owner facts below
                // are a later observation, never a fabricated atomic cause.
                if error.admitted {
                    Stage::Physical
                } else {
                    Stage::Admission
                },
                ResultKind::Failed,
                Code::from_error(error.failure.code),
            ),
        };
        self.record_module_snapshot(
            stage,
            outcome,
            code,
            self.module_diagnostic_snapshot(),
            started.elapsed(),
        );
        result.map_err(|error| error.failure)
    }
}
