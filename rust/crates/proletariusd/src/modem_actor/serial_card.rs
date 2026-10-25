//! Rust-only, whole logical-channel ownership for the compatibility serial actor.
//! The retained worker owns the permit through confirmed CCHC or exact-session
//! quarantine. Dropping an HTTP observer cannot release physical ownership.
use super::*;
use crate::module_esim_channel::AtCommandTransport;
use crate::usb_at::Failure;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SerialCardBinding {
    pub(crate) owner_epoch: String,
    pub(crate) serial_session: u64,
    pub(crate) sim_generation: u64,
}

#[derive(Default)]
pub(super) struct CardShared {
    pub(super) managed: AtomicBool,
    pub(super) binding: Mutex<Option<SerialCardBinding>>,
    pub(super) module_hold: Mutex<Option<SerialModuleHold>>,
}

pub(super) enum CardRequest {
    Module(ModuleRequest),
    BeginSimIdentity {
        hold: SerialModuleHold,
        reply: SyncSender<Result<u64, ActorError>>,
    },
    Capture {
        reply: SyncSender<Result<SerialCardBinding, ActorError>>,
    },
    Begin {
        binding: SerialCardBinding,
        reply: SyncSender<Result<u64, ActorError>>,
    },
    Command {
        permit: u64,
        command: String,
        timeout: Duration,
        maximum: usize,
        reply: SyncSender<Result<String, ActorError>>,
    },
    End {
        permit: u64,
        reply: SyncSender<Result<(), ActorError>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChannelState {
    Closed,
    Opening,
    Open(u8),
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardAction {
    Open,
    Transmit(u8),
    Close(u8),
    Health,
    SimIdentity,
    SimApdu,
}

pub(super) struct CardLease {
    pub(super) permit: u64,
    pub(super) binding: SerialCardBinding,
    pub(super) channel: ChannelState,
    pub(super) poisoned: bool,
    pub(super) module: Option<SerialModuleHold>,
}

pub(super) struct CardCompletion {
    pub(super) permit: u64,
    action: CardAction,
    maximum: usize,
    reply: Option<SyncSender<Result<String, ActorError>>>,
    end_reply: Option<SyncSender<Result<(), ActorError>>>,
}
impl CardCompletion {
    pub(super) fn fail(self, error: ActorError) {
        if let Some(reply) = self.reply {
            let _ = reply.send(Err(error.clone()));
        }
        if let Some(reply) = self.end_reply {
            let _ = reply.send(Err(error));
        }
    }
}

pub(super) fn card_stale() -> ActorError {
    ActorError::new(
        "serial_card_binding_changed",
        "Serial eSIM physical identity changed",
    )
}
pub(super) fn card_busy() -> ActorError {
    ActorError::new(
        "serial_card_busy",
        "Serial eSIM transaction requires exclusive physical ownership",
    )
}
fn card_unknown() -> ActorError {
    ActorError::new(
        "serial_card_outcome_uncertain",
        "Serial eSIM channel cleanup could not be confirmed",
    )
}
pub(super) fn as_failure(error: ActorError) -> Failure {
    let mut failure = Failure::new(
        StatusCode::CONFLICT,
        error.code,
        "Serial eSIM transaction failed",
    );
    failure.message = error.message;
    failure.uncertain = matches!(
        error.code,
        "serial_card_outcome_uncertain" | "serial_disconnected" | "serial_actor_stopped"
    );
    failure
}

impl ModemActor {
    pub(crate) fn claim_native_esim_management(&self) {
        self.client
            .card_shared
            .managed
            .store(true, Ordering::Release);
    }
    pub(crate) fn native_esim_managed(&self) -> bool {
        self.client.card_shared.managed.load(Ordering::Acquire)
    }
    pub(crate) fn cellular_suppressed(&self) -> bool {
        self.client.cellular_suppressed.load(Ordering::Acquire)
    }
    pub(crate) fn module_esim_cache(&self) -> Arc<crate::module_esim_transport::SerialCache> {
        self.client.module_esim_cache.clone()
    }
    /// No AT I/O and no requirement for an enabled subscription or SMS readiness.
    pub(crate) fn capture_serial_card_binding(&self) -> Result<SerialCardBinding, ActorError> {
        self.submit(
            |reply| Command::Card(CardRequest::Capture { reply }),
            RPC_TIMEOUT,
        )
    }
    /// Shared facts only: no command admission, actor wait, or AT I/O.
    pub(crate) fn cached_serial_card_binding(&self) -> Option<SerialCardBinding> {
        let binding = self
            .client
            .card_shared
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        self.serial_card_binding_current(&binding)
            .then_some(binding)
    }
    pub(crate) fn serial_card_binding_current(&self, binding: &SerialCardBinding) -> bool {
        self.call_binding_current(binding.serial_session)
            && self
                .client
                .card_shared
                .binding
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                == Some(binding)
    }
    /// Hold publication of SIM-generation and physical-retirement changes
    /// across one synchronous recovery-journal commit. No actor RPC inside.
    pub(crate) fn with_current_serial_card_binding<T>(
        &self,
        binding: &SerialCardBinding,
        commit: impl FnOnce() -> T,
    ) -> Option<T> {
        let published = self
            .client
            .card_shared
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        (published.as_ref() == Some(binding) && self.call_binding_current(binding.serial_session))
            .then(commit)
    }
    pub(crate) async fn esim_transaction<T, F>(
        &self,
        binding: SerialCardBinding,
        _mutating: bool,
        operation: F,
    ) -> Result<T, Failure>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a mut dyn AtCommandTransport<Error = Failure>) -> Result<T, Failure>
            + Send
            + 'static,
    {
        let actor = self.clone();
        tokio::task::spawn_blocking(move || {
            // Blocking receipt intentionally has no observer deadline: admission,
            // command settlement, and cleanup must outlive cancellation.
            let permit = actor
                .card_request(|reply| CardRequest::Begin { binding, reply })
                .map_err(as_failure)?;
            let mut transport = SerialCardTransaction {
                actor,
                permit: Some(permit),
            };
            let result = operation(&mut transport);
            let cleanup = transport.finish();
            match (result, cleanup) {
                (_, Err(error)) => Err(error),
                (result, Ok(())) => result,
            }
        })
        .await
        .map_err(|_| {
            let mut error = Failure::new(
                StatusCode::BAD_GATEWAY,
                "serial_card_worker_failed",
                "Serial eSIM retained worker failed",
            );
            error.uncertain = true;
            error
        })?
    }
    pub(super) fn card_request<T>(
        &self,
        request: impl FnOnce(SyncSender<Result<T, ActorError>>) -> CardRequest,
    ) -> Result<T, ActorError> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.client
            .sender
            .send(Command::Card(request(reply)))
            .map_err(|_| ActorError::new("serial_actor_stopped", "Serial actor stopped"))?;
        receive.recv().map_err(|_| {
            ActorError::new(
                "serial_actor_stopped",
                "Serial actor stopped before physical completion",
            )
        })?
    }
}

pub(super) struct SerialCardTransaction {
    pub(super) actor: ModemActor,
    pub(super) permit: Option<u64>,
}
impl SerialCardTransaction {
    pub(super) fn finish(&mut self) -> Result<(), Failure> {
        let Some(permit) = self.permit.take() else {
            return Ok(());
        };
        self.actor
            .card_request(|reply| CardRequest::End { permit, reply })
            .map_err(as_failure)
    }
}
impl Drop for SerialCardTransaction {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
impl AtCommandTransport for SerialCardTransaction {
    type Error = Failure;
    fn command(
        &mut self,
        command: &str,
        timeout: Duration,
        maximum: usize,
    ) -> Result<String, Failure> {
        let permit = self.permit.ok_or_else(|| as_failure(card_stale()))?;
        self.actor
            .card_request(|reply| CardRequest::Command {
                permit,
                command: command.to_owned(),
                timeout,
                maximum,
                reply,
            })
            .map_err(as_failure)
    }
}

impl ActorState {
    pub(super) fn local_card_binding(&self) -> Option<SerialCardBinding> {
        self.port.as_ref()?;
        if self.serial_session == 0
            || self.serial_sms.owner_epoch.is_empty()
            || self.serial_sms.generation_exhausted()
        {
            return None;
        }
        Some(SerialCardBinding {
            owner_epoch: self.serial_sms.owner_epoch.clone(),
            serial_session: self.serial_session,
            sim_generation: self.sms_generation,
        })
    }
    pub(super) fn publish_card_binding(&self) {
        *self
            .card_shared
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = self.local_card_binding();
    }
    pub(super) fn card_line_limit(&self) -> usize {
        if matches!(
            self.pending_command.as_ref().map(|p| &p.completion),
            Some(CommandCompletion::Card(_))
        ) {
            8_324
        } else {
            MAX_READ_BYTES
        }
    }
    pub(super) fn card_completion_current(&self, completion: &CardCompletion) -> bool {
        self.card_lease.as_ref().is_some_and(|lease| {
            lease.permit == completion.permit
                && !lease.poisoned
                && self.local_card_binding().as_ref() == Some(&lease.binding)
        })
    }
    pub(super) fn process_card_request(&mut self, request: CardRequest) {
        match request {
            CardRequest::Module(request) => self.process_module_request(request),
            CardRequest::BeginSimIdentity { hold, reply } => self.begin_sim_identity(hold, reply),
            CardRequest::Capture { reply } => {
                self.publish_card_binding();
                let _ = reply.send(
                    self.local_card_binding()
                        .ok_or_else(|| ActorError::unavailable("Serial port is not open")),
                );
            }
            CardRequest::Begin { binding, reply } => {
                if !self.card_shared.managed.load(Ordering::Acquire) {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_esim_not_managed",
                        "Native serial eSIM ownership has not been claimed",
                    )));
                } else if self.local_card_binding().as_ref() != Some(&binding) {
                    let _ = reply.send(Err(card_stale()));
                } else if self.module_hold.is_some()
                    || self.card_lease.is_some()
                    || self.native.refreshing
                    || self.pending_command.is_some()
                    || !self.pending_high_commands.is_empty()
                    || !self.pending_normal_commands.is_empty()
                    || self.deferred_sms_submit.is_some()
                    || self.deferred_radio.is_some()
                    || self.initialization.running
                    || self.sms_receive.is_active()
                {
                    let _ = reply.send(Err(card_busy()));
                } else if let Some(permit) = self.card_next_lease.checked_add(1) {
                    self.card_next_lease = permit;
                    self.card_lease = Some(CardLease {
                        permit,
                        binding,
                        channel: ChannelState::Closed,
                        poisoned: false,
                        module: None,
                    });
                    if reply.send(Ok(permit)).is_err() {
                        self.card_lease = None;
                    }
                } else {
                    let _ = reply.send(Err(card_stale()));
                }
            }
            CardRequest::Command {
                permit,
                command,
                timeout,
                maximum,
                reply,
            } => {
                // Shared USB identity readers request their larger transport
                // ceiling. Native serial retains its strict bounded response.
                let maximum = if self
                    .card_lease
                    .as_ref()
                    .is_some_and(|lease| lease.module.is_some())
                {
                    maximum.min(8_324)
                } else {
                    maximum
                };
                let action = if self
                    .card_lease
                    .as_ref()
                    .is_some_and(|lease| lease.module.is_some())
                {
                    sim_identity_action(&command)
                } else {
                    card_action(&command)
                };
                let current = self.card_lease.as_ref().is_some_and(|lease| {
                    lease.permit == permit
                        && !lease.poisoned
                        && self.local_card_binding().as_ref() == Some(&lease.binding)
                });
                if !current {
                    let _ = reply.send(Err(card_stale()));
                    return;
                }
                if self.pending_command.is_some()
                    || !self.pending_high_commands.is_empty()
                    || !self.pending_normal_commands.is_empty()
                {
                    let _ = reply.send(Err(card_busy()));
                    return;
                }
                let Some(action) = action else {
                    let _ = reply.send(Err(ActorError::invalid(
                        "Command is outside the serial eSIM capability",
                    )));
                    return;
                };
                if timeout.is_zero()
                    || timeout > MAX_COMMAND_TIMEOUT
                    || maximum == 0
                    || maximum > 8_324
                {
                    let _ = reply.send(Err(ActorError::invalid(
                        "Serial eSIM command bounds are invalid",
                    )));
                    return;
                }
                if !self.cellular_command_allowed(&command) {
                    let _ = reply.send(Err(ActorError::new(
                        "module_flight_mode_blocks_cellular_probe",
                        "Cellular telemetry is suppressed by native radio policy",
                    )));
                    return;
                }
                let lease = self.card_lease.as_mut().expect("checked lease");
                let valid = match action {
                    CardAction::Open => lease.channel == ChannelState::Closed,
                    CardAction::Transmit(channel) | CardAction::Close(channel) => {
                        lease.channel == ChannelState::Open(channel)
                    }
                    CardAction::Health | CardAction::SimIdentity | CardAction::SimApdu => {
                        lease.channel == ChannelState::Closed
                    }
                };
                if !valid {
                    let _ = reply.send(Err(ActorError::invalid(
                        "Serial eSIM logical channel does not match the capability",
                    )));
                    return;
                }
                if action == CardAction::Open {
                    lease.channel = ChannelState::Opening;
                }
                self.queue_card_command(
                    command,
                    timeout,
                    CardCompletion {
                        permit,
                        action,
                        maximum,
                        reply: Some(reply),
                        end_reply: None,
                    },
                );
            }
            CardRequest::End { permit, reply } => {
                let Some(lease) = self
                    .card_lease
                    .as_ref()
                    .filter(|lease| lease.permit == permit)
                else {
                    let _ = reply.send(Err(card_unknown()));
                    return;
                };
                let current = self.local_card_binding().as_ref() == Some(&lease.binding);
                if lease.poisoned
                    || !current
                    || matches!(lease.channel, ChannelState::Opening | ChannelState::Unknown)
                {
                    self.quarantine_card_lease(permit);
                    let _ = reply.send(Err(card_unknown()));
                } else if let ChannelState::Open(channel) = lease.channel {
                    self.queue_card_command(
                        format!("AT+CCHC={channel}"),
                        Duration::from_secs(8),
                        CardCompletion {
                            permit,
                            action: CardAction::Close(channel),
                            maximum: 1024,
                            reply: None,
                            end_reply: Some(reply),
                        },
                    );
                } else {
                    self.card_lease = None;
                    self.wake_sms_receive();
                    let _ = reply.send(Ok(()));
                }
            }
        }
    }
    pub(super) fn fail_card_completion(&mut self, completion: CardCompletion) {
        if let Some(lease) = self
            .card_lease
            .as_mut()
            .filter(|lease| lease.permit == completion.permit)
        {
            lease.poisoned = true;
            lease.channel = ChannelState::Unknown;
        }
        // A physical failure cannot leave a lease waiting for a second cleanup
        // request that its worker will never send (especially forced CCHC).
        self.quarantine_card_lease(completion.permit);
        completion.fail(card_unknown());
    }
    fn queue_card_command(
        &mut self,
        command: String,
        timeout: Duration,
        completion: CardCompletion,
    ) {
        let mut bytes = command.into_bytes();
        bytes.extend_from_slice(b"\r\n");
        self.pending_high_commands.push_back(QueuedCommand {
            bytes,
            timeout,
            follow_up: None,
            expected_sms_generation: None,
            high_priority: true,
            completion: CommandCompletion::Card(completion),
        });
    }
    fn quarantine_card_lease(&mut self, permit: u64) {
        let Some(lease) = self.card_lease.take() else {
            return;
        };
        if lease.permit != permit {
            self.card_lease = Some(lease);
            return;
        }
        if self.serial_session == lease.binding.serial_session {
            self.retire_physical_port("serial_card_outcome_uncertain");
            self.close_session();
        }
        self.publish_card_binding();
    }
    pub(super) fn finish_card_command(
        &mut self,
        completion: CardCompletion,
        result: &str,
        mut lines: Vec<String>,
        terminal: Option<String>,
    ) {
        let current = self.card_completion_current(&completion);
        let settled = matches!(result, "ok" | "error");
        if let Some(lease) = self
            .card_lease
            .as_mut()
            .filter(|lease| lease.permit == completion.permit)
        {
            if !current || !settled {
                lease.poisoned = true;
                lease.channel = ChannelState::Unknown;
            } else {
                match completion.action {
                    CardAction::Open => {
                        // A channel must be unambiguous even when a trailing ERROR
                        // causes the codec to request cleanup itself.
                        let channel_lines: Vec<&str> = lines
                            .iter()
                            .filter_map(|line| line.trim().strip_prefix("+CCHO:"))
                            .collect();
                        let channel = if channel_lines.len() == 1 {
                            channel_lines[0]
                                .trim()
                                .parse::<u8>()
                                .ok()
                                .filter(|v| (1..=19).contains(v))
                        } else {
                            None
                        };
                        lease.channel = if let Some(channel) = channel {
                            ChannelState::Open(channel)
                        } else if result == "error" && channel_lines.is_empty() {
                            ChannelState::Closed
                        } else {
                            lease.poisoned = true;
                            ChannelState::Unknown
                        };
                    }
                    CardAction::Close(_) => {
                        if result == "ok" {
                            lease.channel = ChannelState::Closed;
                        } else {
                            lease.poisoned = true;
                            lease.channel = ChannelState::Unknown;
                        }
                    }
                    CardAction::Transmit(_) | CardAction::Health | CardAction::SimIdentity => {}
                    CardAction::SimApdu => {
                        if result == "ok" {
                            let mut response = format!("{}\r\nOK", lines.join("\r\n"));
                            match crate::module_sim_identity::parse_module_csim_response(&response)
                            {
                                Ok(mut bytes) => bytes.zeroize(),
                                Err(_) => lease.poisoned = true,
                            }
                            response.zeroize();
                        }
                    }
                }
            }
        }
        let mut response = lines.join("\r\n");
        for line in &mut lines {
            line.zeroize();
        }
        if !response.is_empty() {
            response.push_str("\r\n");
        }
        response.push_str(terminal.as_deref().unwrap_or(if result == "ok" {
            "OK"
        } else {
            "ERROR"
        }));
        let too_large = response.len() > completion.maximum;
        if too_large {
            if let Some(lease) = self.card_lease.as_mut() {
                lease.poisoned = true;
            }
        }
        let poisoned = self.card_lease.as_ref().is_none_or(|lease| lease.poisoned);
        if poisoned || too_large {
            response.zeroize();
            self.quarantine_card_lease(completion.permit);
            completion.fail(card_unknown());
            return;
        }
        if let Some(reply) = completion.reply {
            let _ = reply.send(Ok(response));
        } else {
            response.zeroize();
        }
        if let Some(reply) = completion.end_reply {
            self.card_lease = None;
            self.wake_sms_receive();
            let _ = reply.send(Ok(()));
        }
    }
}

fn card_action(command: &str) -> Option<CardAction> {
    if matches!(
        command,
        "AT+QCCID" | "AT+CIMI" | "AT+CFUN?" | "AT+CSQ" | "AT+CREG?" | "AT+COPS?" | "AT+QNWINFO"
    ) {
        return Some(CardAction::Health);
    }
    if !crate::usb_at::valid_euicc_at_command(command) {
        return None;
    }
    if command.starts_with("AT+CCHO=") {
        Some(CardAction::Open)
    } else if let Some(channel) = command.strip_prefix("AT+CCHC=") {
        Some(CardAction::Close(channel.parse().ok()?))
    } else {
        Some(CardAction::Transmit(
            command
                .strip_prefix("AT+CGLA=")?
                .split(',')
                .next()?
                .parse()
                .ok()?,
        ))
    }
}

/// Preserve Go metadata's read-only CRSM operations; deny arbitrary APDU and
/// logical-channel escape hatches after Rust assumes serial eSIM authority.
pub(super) fn legacy_card_command(command: &str) -> bool {
    let compact: String = command
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .flat_map(char::to_uppercase)
        .collect();
    if ["+CCHO", "+CGLA", "+CCHC", "+CSIM"]
        .iter()
        .any(|part| compact.contains(part))
    {
        return true;
    }
    if compact.contains("+CRSM") {
        // Only the exact, single read shapes used by Go ReadSIMFile/Record.
        let text = command.trim();
        if text.contains(['\r', '\n', ';']) {
            return true;
        }
        let Some(arguments) = compact.strip_prefix("AT+CRSM=") else {
            return true;
        };
        let fields: Vec<_> = arguments.split(',').collect();
        if fields.len() != 5
            || fields
                .iter()
                .any(|v| v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()))
        {
            return true;
        }
        let values: Option<Vec<u32>> = fields.iter().map(|v| v.parse().ok()).collect();
        let Some(values) = values else {
            return true;
        };
        return !(values[1] <= 65_535
            && values[4] <= 255
            && match values[0] {
                176 => values[2] == 0 && values[3] == 0,
                178 => (1..=255).contains(&values[2]) && values[3] == 4,
                _ => false,
            });
    }
    false
}

impl ActorState {
    /// Snapshot/read/drain commands remain available while an eSIM transaction
    /// waits on network or user confirmation. All competing physical writers
    /// fail before enqueueing; close waits for the retained cleanup receipt.
    pub(super) fn admit_card_boundary(&mut self, command: Command) -> Option<Command> {
        if let Command::Radio {
            module_token: Some(token),
            serial_session,
            reply,
            ..
        } = &command
        {
            if !self.module_hold.as_ref().is_some_and(|hold| {
                hold.token() == token && hold.session().serial_session == *serial_session
            }) {
                let _ = reply.send(Err(card_stale()));
                return None;
            }
        }
        if self.card_shared.managed.load(Ordering::Acquire) {
            let forbidden = match &command {
                Command::Write { data, .. } => legacy_card_command(&String::from_utf8_lossy(data)),
                Command::Execute { command, .. } => legacy_card_command(command),
                _ => false,
            };
            if forbidden {
                let error = ActorError::new(
                    "serial_esim_native_owner",
                    "Serial card commands require native eSIM transaction ownership",
                );
                match command {
                    Command::Write { reply, .. } => {
                        let _ = reply.send(Err(error));
                    }
                    Command::Execute { reply, .. } => {
                        let _ = reply.send(Err(error));
                    }
                    _ => unreachable!(),
                }
                return None;
            }
        }
        if self.card_lease.is_none() && self.module_hold.is_none() && !self.native.refreshing {
            return Some(command);
        }
        macro_rules! reject {
            ($reply:expr) => {{
                let _ = $reply.send(Err(card_busy()));
                return None;
            }};
        }
        match command {
            Command::ScopedRadio { .. } => {
                unreachable!("scoped envelope consumed before card boundary")
            }
            Command::Native(_)
            | Command::LegacyGate(_)
            | Command::Card(_)
            | Command::Read { .. }
            | Command::Finalize { .. }
            | Command::SerialSmsObservation { .. }
            | Command::SerialSmsBindingCurrent { .. }
            | Command::CacheSerialSmsPhone { .. }
            | Command::RadioBinding { .. }
            | Command::ReadSmsGeneration { .. }
            | Command::ConfirmSmsGeneration { .. }
            | Command::FinishSmsReceive { .. } => Some(command),
            Command::Close { reply } | Command::FencedClose { reply, .. }
                if self.module_hold.is_some() =>
            {
                reject!(reply)
            }
            Command::Radio {
                ref module_token,
                serial_session,
                ..
            } if self.card_lease.is_none()
                && self.module_hold.as_ref().is_some_and(|hold| {
                    hold.session().serial_session == serial_session
                        && module_token.as_deref() == Some(hold.token())
                }) =>
            {
                Some(command)
            }
            Command::Close { .. } | Command::FencedClose { .. } => {
                // A compatibility Close was authorized for the physical
                // session whose transaction delayed it, never a later reopen.
                let command = match command {
                    Command::Close { reply } => Command::FencedClose {
                        serial_session: self
                            .card_lease
                            .as_ref()
                            .map(|lease| lease.binding.serial_session)
                            .unwrap_or(self.serial_session),
                        reply,
                    },
                    command => command,
                };
                if self.card_deferred_closes.len() < ACTOR_QUEUE_CAPACITY {
                    self.card_deferred_closes.push(command);
                } else {
                    match command {
                        Command::Close { reply } | Command::FencedClose { reply, .. } => {
                            reject!(reply)
                        }
                        _ => unreachable!(),
                    }
                }
                None
            }
            Command::NextSmsReceive { reply } => {
                let _ = reply.send(Ok(None));
                None
            }
            Command::Open { reply, .. } => reject!(reply),
            Command::DiscoverATPort { reply, .. } => reject!(reply),
            Command::Initialize { reply } | Command::CallModeRadioInitialize { reply, .. } => {
                reject!(reply)
            }
            Command::CallModeRadioReconnect { reply, .. } => reject!(reply),
            Command::Radio { reply, .. } => reject!(reply),
            Command::Write { reply, .. } => reject!(reply),
            Command::Execute { reply, .. } => reject!(reply),
            Command::SubmitSms { reply, .. } => reject!(reply),
            Command::DeleteArchivedSms { reply, .. } => reject!(reply),
            Command::ReadStoredSms { reply, .. } => reject!(reply),
            Command::ConfigureSmsReceive { reply, .. } | Command::ScanSmsReceive { reply } => {
                reject!(reply)
            }
        }
    }
}

pub(crate) fn parse_serial_esim_health_responses(
    iccid: &str,
    imsi: &str,
    csq: &str,
    creg: &str,
    cops: &str,
    qnwinfo: &str,
    suppressed: bool,
    sim_present: bool,
) -> Value {
    let lines = |text: &str| text.lines().map(str::to_owned).collect::<Vec<_>>();
    let status = if suppressed {
        0
    } else {
        parse_creg(&lines(creg)).map(|v| v.0).unwrap_or_default()
    };
    let status_text = if suppressed {
        "飞行模式 · 蜂窝射频关闭"
    } else {
        match status {
            0 => "未注册",
            1 => "已注册(本地)",
            2 => "搜索中",
            3 => "注册被拒",
            5 => "已注册(漫游)",
            _ => "未知",
        }
    };
    json!({"iccid": parse_iccid(&lines(iccid)), "imsi": parse_imsi(&lines(imsi)),
        "operator": if suppressed { String::new() } else { parse_cops_operator(&lines(cops)) },
        "sim_inserted": sim_present, "reg_status": status, "reg_status_text": status_text,
        "signal_dbm": if suppressed { -999 } else { parse_csq(&lines(csq)).map(|v| v.1).unwrap_or(-999) },
        "network_mode": if suppressed { "飞行模式".to_owned() } else { parse_qnwinfo_radio(&lines(qnwinfo)).0 }})
}

#[cfg(test)]
#[path = "serial_card_tests.rs"]
mod tests;

fn sim_identity_action(command: &str) -> Option<CardAction> {
    if matches!(command, "AT+CNUM" | "AT+CSCA?" | "AT+QCCID" | "AT+CIMI") {
        Some(CardAction::SimIdentity)
    } else if crate::module_sim_identity::valid_module_csim_command(command) {
        Some(CardAction::SimApdu)
    } else {
        None
    }
}
