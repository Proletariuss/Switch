//! Serial initialization stays on the actor's ordinary command queue. Delays
//! are actor state, so URCs and shutdown remain responsive between commands.

use super::*;

const INITIALIZATION_PROBE_DELAY: Duration = Duration::from_millis(150);
const INITIALIZATION_STEP_DELAY: Duration = Duration::from_millis(100);
const INITIALIZATION_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const INITIALIZATION_STEPS: [(&str, &str); 6] = [
    ("probe", "AT"),
    ("echo", "ATE0"),
    ("pdu_mode", "AT+CMGF=0"),
    ("sms_notifications", "AT+CNMI=2,1,0,0,0"),
    ("caller_id", "AT+CLIP=1"),
    ("usb_audio", "AT+QPCMV=1,2"),
];

#[derive(Default)]
pub(super) struct InitializationState {
    pub(super) required: bool,
    pub(super) call_mode_fence: Option<(Instant, Arc<std::sync::atomic::AtomicBool>)>,
    pub(super) cached: Option<Result<InitializationResult, ActorError>>,
    pub(super) running: bool,
    pub(super) deferred: Option<DeferredInitialization>,
}

impl InitializationState {
    pub(super) fn sms_ready(&self) -> bool {
        !self.required
            || matches!(
                &self.cached,
                Some(Ok(result))
                    if result.completed && result.probe_succeeded && result.sms_pdu_ready
            )
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct InitializationResult {
    pub(crate) completed: bool,
    pub(crate) probe_succeeded: bool,
    pub(crate) sms_pdu_ready: bool,
    pub(super) steps: Vec<InitializationStepResult>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct InitializationStepResult {
    pub(super) name: &'static str,
    pub(super) result: &'static str,
}

pub(super) struct InitializationTransaction {
    pub(super) serial_session: u64,
    pub(super) step: usize,
    pub(super) result: InitializationResult,
    pub(super) reply: SyncSender<Result<InitializationResult, ActorError>>,
}

pub(super) struct DeferredInitialization {
    pub(super) transaction: InitializationTransaction,
    pub(super) ready_at: Instant,
}

impl ActorState {
    pub(super) fn begin_initialization(
        &mut self,
        reply: SyncSender<Result<InitializationResult, ActorError>>,
    ) {
        if self.port.is_none() {
            let _ = reply.send(Err(ActorError::unavailable("serial port is not open")));
            return;
        }
        if let Some(result) = &self.initialization.cached {
            let _ = reply.send(result.clone());
            return;
        }
        if self.initialization.running {
            let _ = reply.send(Err(ActorError::new(
                "serial_initialization_busy",
                "serial initialization is already running",
            )));
            return;
        }

        self.initialization.running = true;
        self.initialization.deferred = Some(DeferredInitialization {
            transaction: InitializationTransaction {
                serial_session: self.serial_session,
                step: 0,
                result: InitializationResult {
                    completed: false,
                    probe_succeeded: false,
                    sms_pdu_ready: false,
                    steps: Vec::with_capacity(INITIALIZATION_STEPS.len()),
                },
                reply,
            },
            ready_at: Instant::now() + INITIALIZATION_PROBE_DELAY,
        });
    }

    pub(super) fn advance_initialization(
        &mut self,
        mut transaction: InitializationTransaction,
        result: &'static str,
    ) {
        if let Err(error) = self.check_initialization_session(transaction.serial_session) {
            self.fail_initialization(transaction, error);
            return;
        }
        let Some(&(name, _)) = INITIALIZATION_STEPS.get(transaction.step) else {
            self.fail_initialization(
                transaction,
                ActorError::new(
                    "serial_initialization_state_invalid",
                    "serial initialization has an invalid command step",
                ),
            );
            return;
        };
        transaction
            .result
            .steps
            .push(InitializationStepResult { name, result });

        if transaction.step == 0 {
            transaction.result.probe_succeeded = result == "ok";
            if !transaction.result.probe_succeeded {
                self.finish_initialization(transaction);
                return;
            }
        } else if transaction.step == 2 {
            transaction.result.sms_pdu_ready = result == "ok";
        }

        // The probe has no trailing delay. Each of the five best-effort
        // commands does, including the final USB audio command.
        let delay = if transaction.step == 0 {
            Duration::ZERO
        } else {
            INITIALIZATION_STEP_DELAY
        };
        transaction.step += 1;
        self.initialization.deferred = Some(DeferredInitialization {
            transaction,
            ready_at: Instant::now() + delay,
        });
    }

    pub(super) fn poll_initialization(&mut self) {
        let Some(deferred) = self.initialization.deferred.take() else {
            return;
        };
        if let Err(error) = self.check_initialization_session(deferred.transaction.serial_session) {
            self.fail_initialization(deferred.transaction, error);
            return;
        }
        if Instant::now() < deferred.ready_at {
            self.initialization.deferred = Some(deferred);
            return;
        }
        if deferred.transaction.step == INITIALIZATION_STEPS.len() {
            // Completion does not need queue capacity or another AT command.
            self.finish_initialization(deferred.transaction);
            return;
        }
        let Some(&(_, command)) = INITIALIZATION_STEPS.get(deferred.transaction.step) else {
            self.fail_initialization(
                deferred.transaction,
                ActorError::new(
                    "serial_initialization_state_invalid",
                    "serial initialization has an invalid command step",
                ),
            );
            return;
        };
        if self.pending_normal_commands.len() >= NORMAL_COMMAND_QUEUE_CAPACITY {
            self.initialization.deferred = Some(deferred);
            return;
        }
        self.pending_normal_commands.push_back(QueuedCommand {
            bytes: format!("{command}\r\n").into_bytes(),
            timeout: INITIALIZATION_COMMAND_TIMEOUT,
            follow_up: None,
            expected_sms_generation: None,
            high_priority: false,
            completion: CommandCompletion::Initialize(deferred.transaction),
        });
    }

    pub(super) fn cancel_initialization(&mut self, error: ActorError) {
        let deferred = self.initialization.deferred.take();
        self.note_initialization_failure(error.clone());
        if let Some(deferred) = deferred {
            let _ = deferred.transaction.reply.send(Err(error));
        }
        // A queued or in-flight transaction belongs to the ordinary command
        // lifecycle; the caller also fails those completions during close.
    }

    pub(super) fn note_initialization_failure(&mut self, error: ActorError) {
        self.initialization.running = false;
        self.initialization.cached = Some(Err(error));
        self.initialization.deferred = None;
        self.wake_sms_receive();
    }

    fn check_initialization_session(&self, serial_session: u64) -> Result<(), ActorError> {
        if self
            .initialization
            .call_mode_fence
            .as_ref()
            .is_some_and(|(deadline, cancelled)| {
                Instant::now() >= *deadline || cancelled.load(std::sync::atomic::Ordering::Acquire)
            })
        {
            return Err(ActorError::new(
                "radio_owner_revoked",
                "native rebind initialization budget or ownership ended",
            ));
        }
        if self.port.is_none() {
            return Err(ActorError::unavailable("serial port is not open"));
        }
        if self.serial_session != serial_session {
            return Err(ActorError::new(
                "serial_session_changed",
                "serial initialization belongs to a retired serial session",
            ));
        }
        Ok(())
    }

    fn fail_initialization(&mut self, transaction: InitializationTransaction, error: ActorError) {
        // A delayed completion from a retired port may notify its own caller,
        // but must not clear a new session's initialization or cached result.
        if transaction.serial_session == self.serial_session {
            self.note_initialization_failure(error.clone());
        }
        let _ = transaction.reply.send(Err(error));
    }

    fn finish_initialization(&mut self, mut transaction: InitializationTransaction) {
        transaction.result.completed = true;
        self.initialization.running = false;
        self.initialization.cached = Some(Ok(transaction.result.clone()));
        self.initialization.deferred = None;
        if self.initialization.call_mode_fence.take().is_some()
            && transaction.result.probe_succeeded
            && transaction.result.sms_pdu_ready
        {
            // Only the already-proven module receives its previous native URC
            // policy. Source SMS records stay behind the normal receive fence.
            if let Err(error) = self.configure_receive(self.last_radio_receive_enabled) {
                self.note_initialization_failure(error.clone());
                let _ = transaction.reply.send(Err(error));
                return;
            }
        }
        self.wake_sms_receive();
        let _ = transaction.reply.send(Ok(transaction.result));
    }
}

#[cfg(test)]
mod tests {
    include!("initialize_tests.rs");
}
