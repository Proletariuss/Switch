//! RF transactions reserve this actor between commands. Waiting for CFUN is
//! deferred state, never a sleep on the serial thread; URCs/Close still run.
use super::*;
use crate::module_radio_transport::{
    Action, Binding, Outcome, Plan, Reply, failure, observed_call,
};
use crate::usb_at::Failure;
use zeroize::Zeroize;

// Observe actual mailbox acceptance, never admission or commit authority.
#[cfg(test)]
fn queued_radio_receipts()
-> &'static std::sync::Mutex<std::collections::HashMap<usize, SyncSender<()>>> {
    static RECEIPTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<usize, SyncSender<()>>>,
    > = std::sync::OnceLock::new();
    RECEIPTS.get_or_init(Default::default)
}

pub(super) struct RadioTransaction {
    pub(super) serial_session: u64,
    pub(super) plan: Plan,
    pub(super) write_in_flight: bool,
    pub(super) reply: SyncSender<Result<Outcome, ActorError>>,
}

impl ModemActor {
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.client, &other.client)
    }
    pub(crate) async fn call_mode_radio_initialize(
        &self,
        serial_session: u64,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), Failure> {
        self.call_mode_radio_initialize_tagged(serial_session, deadline, cancelled, None)
            .await
    }

    pub(crate) async fn call_mode_radio_initialize_with_right(
        &self,
        serial_session: u64,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        right: crate::module_recovery::OperationRight,
        producer: String,
    ) -> Result<(), Failure> {
        self.call_mode_radio_initialize_tagged(
            serial_session,
            deadline,
            cancelled,
            Some((right, producer)),
        )
        .await
    }

    async fn call_mode_radio_initialize_tagged(
        &self,
        serial_session: u64,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        scoped: Option<(crate::module_recovery::OperationRight, String)>,
    ) -> Result<(), Failure> {
        let actor = self.clone();
        tokio::task::spawn_blocking(move || {
            let (reply, receiver) = mpsc::sync_channel(1);
            actor
                .client
                .sender
                .try_send(scope_radio(
                    Command::CallModeRadioInitialize {
                        serial_session,
                        deadline,
                        cancelled,
                        reply,
                    },
                    scoped,
                ))
                .map_err(|_| failure("serial_command_busy"))?;
            actor.client.wake.notify_one();
            let result = receiver
                .recv()
                .map_err(|_| failure("serial_not_open"))?
                .map_err(|error| failure(error.code))?;
            if result.completed && result.probe_succeeded && result.sms_pdu_ready {
                Ok(())
            } else {
                Err(failure("call_mode_serial_initialization_unconfirmed"))
            }
        })
        .await
        .map_err(|_| failure("radio_worker_failed"))?
    }

    pub(crate) async fn call_mode_radio_reconnect(
        &self,
        serial_session: u64,
        expected_imei: &str,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<u64, Failure> {
        self.call_mode_radio_reconnect_tagged(
            serial_session,
            expected_imei,
            deadline,
            cancelled,
            None,
        )
        .await
    }

    pub(crate) async fn call_mode_radio_reconnect_with_right(
        &self,
        serial_session: u64,
        expected_imei: &str,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        right: crate::module_recovery::OperationRight,
        producer: String,
    ) -> Result<u64, Failure> {
        self.call_mode_radio_reconnect_tagged(
            serial_session,
            expected_imei,
            deadline,
            cancelled,
            Some((right, producer)),
        )
        .await
    }

    async fn call_mode_radio_reconnect_tagged(
        &self,
        serial_session: u64,
        expected_imei: &str,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        scoped: Option<(crate::module_recovery::OperationRight, String)>,
    ) -> Result<u64, Failure> {
        let actor = self.clone();
        let expected_imei = expected_imei.to_owned();
        tokio::task::spawn_blocking(move || {
            let (reply, receiver) = mpsc::sync_channel(1);
            actor
                .client
                .sender
                .try_send(scope_radio(
                    Command::CallModeRadioReconnect {
                        serial_session,
                        expected_imei,
                        deadline,
                        cancelled,
                        reply,
                    },
                    scoped,
                ))
                .map_err(|_| failure("serial_command_busy"))?;
            actor.client.wake.notify_one();
            receiver
                .recv()
                .map_err(|_| failure("serial_not_open"))?
                .map_err(|error| failure(error.code))
        })
        .await
        .map_err(|_| failure("radio_worker_failed"))?
    }

    pub(crate) async fn radio_binding(&self) -> Result<u64, Failure> {
        let actor = self.clone();
        tokio::task::spawn_blocking(move || {
            actor.submit(|reply| Command::RadioBinding { reply }, RPC_TIMEOUT)
        })
        .await
        .map_err(|_| failure("radio_worker_failed"))?
        .map_err(|error| failure(error.code))
    }
    #[cfg(test)]
    pub(crate) async fn radio_execute(
        &self,
        serial_session: u64,
        action: Action,
    ) -> Result<Outcome, Failure> {
        self.radio_execute_with_budget(
            serial_session,
            action,
            crate::module_radio_transport::PLAN_BUDGET,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
    }
    pub(crate) async fn radio_execute_with_budget(
        &self,
        serial_session: u64,
        action: Action,
        budget: Duration,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Outcome, Failure> {
        self.radio_execute_tagged(serial_session, None, action, budget, cancelled, None, None)
            .await
    }
    pub(crate) async fn radio_execute_module_with_budget(
        &self,
        serial_session: u64,
        token: &str,
        action: Action,
        budget: Duration,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Outcome, Failure> {
        self.radio_execute_tagged(
            serial_session,
            Some(token.to_owned()),
            action,
            budget,
            cancelled,
            None,
            None,
        )
        .await
    }
    pub(crate) async fn radio_execute_with_right(
        &self,
        serial_session: u64,
        module_token: Option<String>,
        action: Action,
        budget: Duration,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        right: crate::module_recovery::OperationRight,
        producer: String,
        commit_proof: Option<crate::module_radio::CallModeCommitProof>,
    ) -> Result<Outcome, Failure> {
        self.radio_execute_tagged(
            serial_session,
            module_token,
            action,
            budget,
            cancelled,
            Some((right, producer)),
            commit_proof,
        )
        .await
    }
    async fn radio_execute_tagged(
        &self,
        serial_session: u64,
        module_token: Option<String>,
        action: Action,
        budget: Duration,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        scoped: Option<(crate::module_recovery::OperationRight, String)>,
        commit_proof: Option<crate::module_radio::CallModeCommitProof>,
    ) -> Result<Outcome, Failure> {
        if let Some(proof) = commit_proof.as_ref() {
            proof
                .validate_for_modem_actor(self)
                .map_err(|error| failure(error.code))?;
        }
        let retained_module = module_token
            .as_deref()
            .map(|token| self.retain_module_radio_operation(serial_session, token))
            .transpose()?;
        action.validate()?;
        if budget.is_zero() {
            return Err(failure("radio_plan_timeout"));
        }
        let deadline = Instant::now() + budget.min(crate::module_radio_transport::PLAN_BUDGET);
        let actor = self.clone();
        tokio::task::spawn_blocking(move || {
            let _retained_module = retained_module;
            let (reply, receiver) = mpsc::sync_channel(1);
            // try_send makes admission bounded. Once accepted, receive the real
            // completion even if the outer HTTP waiter has disappeared.
            actor
                .client
                .sender
                .try_send(scope_radio(
                    Command::Radio {
                        module_token,
                        serial_session,
                        action,
                        deadline,
                        cancelled,
                        commit_proof,
                        reply,
                    },
                    scoped,
                ))
                .map_err(|_| failure("serial_command_busy"))?;
            #[cfg(test)]
            if let Some(receipt) = queued_radio_receipts()
                .lock()
                .unwrap()
                .remove(&(Arc::as_ptr(&actor.client) as usize))
            {
                let _ = receipt.try_send(());
            }
            actor.client.wake.notify_one();
            receiver
                .recv()
                .map_err(|_| failure("serial_not_open"))?
                .map_err(|error| failure(error.code))
        })
        .await
        .map_err(|_| failure("radio_worker_failed"))?
    }
}

impl ActorState {
    pub(super) fn call_mode_radio_reconnect(
        &mut self,
        serial_session: u64,
        expected_imei: &str,
        deadline: Instant,
        cancelled: &std::sync::atomic::AtomicBool,
        opener: &PortOpener,
    ) -> Result<u64, ActorError> {
        if expected_imei.len() != 15 || !expected_imei.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ActorError::invalid("invalid reconnect identity"));
        }
        if self.deferred_radio.is_some()
            || self.pending_command.is_some()
            || self.deferred_sms_submit.is_some()
            || self.initialization.running
            || self.sms_receive.is_active()
            || !self.pending_high_commands.is_empty()
            || !self.pending_normal_commands.is_empty()
        {
            return Err(ActorError::new(
                "serial_command_busy",
                "serial reconnect worker is busy",
            ));
        }
        if cancelled.load(std::sync::atomic::Ordering::Acquire) || Instant::now() >= deadline {
            return Err(ActorError::new(
                "radio_owner_revoked",
                "serial reconnect budget or ownership ended",
            ));
        }
        if serial_session == 0 || serial_session != self.serial_session {
            return Err(ActorError::new(
                "serial_session_changed",
                "serial reconnect generation changed",
            ));
        }
        let selected = self
            .last_radio_port
            .clone()
            .ok_or_else(|| ActorError::unavailable("no native serial selection"))?;
        let next_session = self
            .serial_session
            .checked_add(1)
            .ok_or_else(|| ActorError::unavailable("serial session identity exhausted"))?;
        self.retire_physical_port("serial_rebinding");
        self.close_session();
        let mut candidates = vec![selected.0.clone()];
        if let Ok(discovered) = enumerate_at_port_candidates() {
            for candidate in discovered {
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
        }
        for path in candidates.into_iter().take(MAX_AT_DISCOVERY_CANDIDATES) {
            if cancelled.load(std::sync::atomic::Ordering::Acquire) || Instant::now() >= deadline {
                return Err(ActorError::new(
                    "radio_owner_revoked",
                    "serial reconnect budget or ownership ended",
                ));
            }
            let Ok(mut port) = opener(path.clone(), selected.1) else {
                continue;
            };
            if call_mode_probe_serial_imei(port.as_mut(), deadline, cancelled).as_deref()
                != Some(expected_imei)
            {
                continue;
            }
            if cancelled.load(std::sync::atomic::Ordering::Acquire) || Instant::now() >= deadline {
                return Err(ActorError::new(
                    "radio_owner_revoked",
                    "serial reconnect budget or ownership ended",
                ));
            }
            self.serial_session = next_session;
            self.port = Some(port);
            self.live_serial_session
                .store(self.serial_session, std::sync::atomic::Ordering::Release);
            self.current_path = Some(path.clone());
            self.current_baud = selected.1;
            self.last_radio_port = Some((path, selected.1));
            self.initialization = InitializationState::default();
            self.sms_receive.configured = false;
            self.sms_generation = 0;
            self.serial_sms.reset_sim();
            self.native_session_opened();
            self.radio_call_active = false;
            return Ok(next_session);
        }
        Err(ActorError::new(
            "call_mode_module_identity_unconfirmed",
            "same module serial reconnect was not confirmed",
        ))
    }

    #[cfg(test)]
    pub(super) fn begin_radio(
        &mut self,
        serial_session: u64,
        action: Action,
        reply: SyncSender<Result<Outcome, ActorError>>,
    ) {
        self.begin_radio_with_deadline(
            serial_session,
            action,
            Instant::now() + crate::module_radio_transport::PLAN_BUDGET,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reply,
            None,
        );
    }
    pub(super) fn begin_radio_with_deadline(
        &mut self,
        serial_session: u64,
        action: Action,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        reply: SyncSender<Result<Outcome, ActorError>>,
        commit_proof: Option<crate::module_radio::CallModeCommitProof>,
    ) {
        if deadline <= Instant::now() {
            let _ = reply.send(Err(ActorError::new(
                "radio_plan_timeout",
                "Radio operation budget expired",
            )));
            return;
        }
        if self.port.is_none() || serial_session == 0 || serial_session != self.serial_session {
            let _ = reply.send(Err(ActorError::new(
                "serial_session_changed",
                "Radio binding is no longer current",
            )));
            return;
        }
        let init_ready = !self.initialization.required
            || matches!(&self.initialization.cached,
            Some(Ok(result)) if result.completed && result.probe_succeeded);
        if !init_ready
            || self.initialization.running
            || self.pending_command.is_some()
            || self.deferred_sms_submit.is_some()
            || self.deferred_radio.is_some()
            || !self.pending_high_commands.is_empty()
            || !self.pending_normal_commands.is_empty()
        {
            let _ = reply.send(Err(ActorError::new(
                "serial_command_busy",
                "Radio transaction admission is busy",
            )));
            return;
        }
        if let Err(error) = action.validate() {
            let _ = reply.send(Err(ActorError::new(error.code, "Radio action is invalid")));
            return;
        }
        if let Some(proof) = commit_proof.as_ref() {
            if let Err(error) = proof.validate_for_gate(&self.module_recovery) {
                let _ = reply.send(Err(ActorError::new(
                    error.code,
                    "Radio commit authority is foreign",
                )));
                return;
            }
        }
        let mut plan = Plan::new_with_cancelled(
            action,
            deadline.saturating_duration_since(Instant::now()),
            cancelled,
        );
        plan.set_call_mode_commit_proof(commit_proof);
        self.deferred_radio = Some(RadioTransaction {
            serial_session,
            plan,
            write_in_flight: false,
            reply,
        });
    }

    pub(super) fn poll_radio(&mut self) -> bool {
        let Some(mut transaction) = self.deferred_radio.take() else {
            return false;
        };
        if self.port.is_none() || transaction.serial_session != self.serial_session {
            fail_radio(transaction, "serial_session_changed");
            return true;
        }
        if transaction.plan.mutating()
            && !transaction.plan.cellular_call()
            && self.radio_call_active
        {
            transaction.plan.fail("radio_call_busy", false);
        }
        let Some(mut step) = transaction.plan.next() else {
            if transaction.plan.done() {
                self.finish_radio(transaction);
            } else {
                self.deferred_radio = Some(transaction);
            }
            return true;
        };
        // The queue envelope has already been consumed. The finite plan owns
        // its typed proof until this actual write boundary, alongside the
        // current physical serial session and original module operation pin.
        if let Err(code) = transaction.plan.prepare_commit_before_write(
            &step,
            &Binding::Serial {
                serial_session: transaction.serial_session,
            },
        ) {
            step.command.zeroize();
            fail_radio(transaction, code);
            return true;
        }
        let mut bytes = step.command.into_bytes();
        bytes.extend_from_slice(b"\r\n");
        transaction.write_in_flight = step.write;
        if step.write {
            transaction.plan.note_write_started();
            transaction.plan.mark_submitted();
        }
        let write_result = self.port.as_mut().expect("port checked").write_all(&bytes);
        bytes.zeroize();
        if write_result.is_err() {
            fail_radio(transaction, "serial_disconnected");
            self.close_radio_transport();
            return true;
        }
        while let Some(frame) = self.frames.pop_front() {
            self.command_events.push_back(frame);
        }
        self.pending_command = Some(PendingCommand {
            deadline: Instant::now() + step.timeout,
            started: Instant::now(),
            lines: Vec::new(),
            response_bytes: 0,
            overflow: false,
            follow_up: None,
            expected_sms_generation: None,
            high_priority: false,
            completion: CommandCompletion::Radio(transaction),
        });
        true
    }

    pub(super) fn advance_radio(
        &mut self,
        mut transaction: RadioTransaction,
        result: &'static str,
        mut lines: Vec<String>,
        mut terminal: Option<String>,
    ) {
        if transaction.serial_session != self.serial_session || self.port.is_none() {
            fail_radio(transaction, "serial_session_changed");
            return;
        }
        let sensitive = transaction.plan.sensitive_in_flight();
        let mut text = lines.join("\r\n");
        if result == "ok" {
            text.push_str("\r\nOK");
        }
        if let Some(terminal) = terminal.as_ref() {
            text.push_str("\r\n");
            text.push_str(&terminal);
        }
        let mut reply = match result {
            "ok" | "error" => Reply::response(text),
            "timeout" => Reply {
                text,
                ok: false,
                uncertain: true,
                fatal: false,
                code: "serial_timeout".into(),
            },
            _ => Reply {
                text,
                ok: false,
                uncertain: true,
                fatal: true,
                code: "serial_response_unconfirmed".into(),
            },
        };
        if sensitive {
            for line in &mut lines {
                line.zeroize();
            }
            if let Some(terminal) = terminal.as_mut() {
                terminal.zeroize();
            }
            reply.text.zeroize();
        }
        transaction.plan.accept(reply);
        if transaction.plan.mutating()
            && !transaction.plan.cellular_call()
            && self.radio_call_active
        {
            transaction.plan.fail("radio_call_busy", false);
        }
        transaction.write_in_flight = false;
        if transaction.plan.done() {
            self.finish_radio(transaction);
        } else {
            self.deferred_radio = Some(transaction);
        }
    }

    fn finish_radio(&mut self, transaction: RadioTransaction) {
        let retire = transaction.plan.retire_transport();
        let outcome = transaction.plan.finish();
        if retire {
            self.close_radio_transport();
        }
        let _ = transaction.reply.send(Ok(outcome));
    }
    fn close_radio_transport(&mut self) {
        let pending_read = self.pending_read.take();
        self.close_session();
        let error = ActorError::new(
            "serial_disconnected",
            "Radio command stream could not be confirmed",
        );
        let delivered =
            pending_read.is_some_and(|read| read.reply.send(Err(error.clone())).is_ok());
        if !delivered {
            self.disconnected_error = Some(error);
        }
    }
    pub(super) fn observe_radio_call(&mut self, text: &str) {
        if observed_call(text) {
            self.radio_call_active = true;
        }
        if text
            .lines()
            .map(str::trim)
            .any(|line| matches!(line, "NO CARRIER" | "NO ANSWER" | "BUSY"))
        {
            self.radio_call_active = false;
        }
    }
}
fn call_mode_probe_serial_imei(
    port: &mut dyn serialport::SerialPort,
    deadline: Instant,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Option<String> {
    let deadline = deadline.min(Instant::now() + AT_DISCOVERY_PROBE_TIMEOUT);
    let budget = deadline.saturating_duration_since(Instant::now());
    if budget.is_zero() || cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    port.set_timeout(AT_DISCOVERY_READ_TIMEOUT.min(budget))
        .ok()?;
    port.write_all(b"AT+CGSN\r\n").ok()?;
    let mut response = Vec::with_capacity(256);
    let mut buffer = [0u8; 1024];
    while Instant::now() < deadline && !cancelled.load(std::sync::atomic::Ordering::Acquire) {
        match port.read(&mut buffer) {
            Ok(0) => {}
            Ok(count) => {
                if response.len().saturating_add(count) > AT_DISCOVERY_MAX_RESPONSE_BYTES {
                    return None;
                }
                response.extend_from_slice(&buffer[..count]);
                let text = std::str::from_utf8(&response).ok()?;
                let terminal = text
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .next_back()
                    .unwrap_or("");
                if terminal == "OK" {
                    return crate::module_radio_transport::call_mode_imei(text);
                }
                if terminal == "ERROR"
                    || terminal.starts_with("+CME ERROR:")
                    || terminal.starts_with("+CMS ERROR:")
                {
                    return None;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return None,
        }
    }
    None
}
pub(super) fn fail_radio(mut transaction: RadioTransaction, code: &str) {
    let uncertain = transaction.write_in_flight || transaction.plan.unconfirmed_write();
    transaction.plan.fail(code, uncertain);
    let _ = transaction.reply.send(Ok(transaction.plan.finish()));
}

#[cfg(test)]
mod tests {
    include!("radio_tests.rs");
}

#[cfg(all(test, unix))]
mod commit_tests {
    include!("radio_commit_tests.rs");
}

fn scope_radio(
    command: Command,
    scoped: Option<(crate::module_recovery::OperationRight, String)>,
) -> Command {
    match scoped {
        Some((right, producer)) => Command::ScopedRadio {
            command: Box::new(command),
            right,
            producer,
        },
        None => command,
    }
}
