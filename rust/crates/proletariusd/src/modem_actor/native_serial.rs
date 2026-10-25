//! Native serial lifecycle facts and command capabilities. No compatibility
//! process owns this mode, and no raw event subscriber is required to drain it.
use super::*;
use std::collections::BTreeSet;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NativeSerialMetadata {
    pub(crate) binding: SerialCardBinding,
    pub(crate) status: Value,
    pub(crate) suppression_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeSerialTerminal {
    pub(crate) owner_epoch: String,
    pub(crate) serial_session: u64,
    pub(crate) code: &'static str,
    pub(crate) physical_closed: bool,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NativeSerialObservation {
    pub(crate) revision: u64,
    pub(crate) managed: bool,
    pub(crate) port: String,
    pub(crate) discovery_error: String,
    pub(crate) serial_session: Option<u64>,
    pub(crate) sim_generation: u64,
    pub(crate) initialized: bool,
    pub(crate) ready: bool,
    pub(crate) sim_present: Option<bool>,
    pub(crate) pin_blocked: bool,
    pub(crate) refresh_needed: bool,
    pub(crate) metadata: Option<NativeSerialMetadata>,
    pub(crate) terminal: Option<NativeSerialTerminal>,
}
#[derive(Clone, Debug)]
pub(crate) struct NativeSerialSession {
    pub(crate) owner_epoch: String,
    pub(crate) serial_session: u64,
    facts: Arc<Mutex<NativeSerialObservation>>,
}
impl NativeSerialSession {
    pub(crate) fn observation(&self) -> NativeSerialObservation {
        self.facts.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
#[derive(Default)]
pub(super) struct NativeShared {
    managed: AtomicBool,
    pub(super) policy_revision: std::sync::atomic::AtomicU64,
    pub(super) policy_write: Mutex<()>,
    facts: Mutex<NativeSerialObservation>,
    session: Mutex<Option<NativeSerialSession>>,
    changed: tokio::sync::Notify,
}
#[derive(Default)]
pub(super) struct NativeState {
    pub(super) shared: Arc<NativeShared>,
    pub(super) session: Option<NativeSerialSession>,
    last_port: String,
    discovery_error: String,
    terminal: Option<NativeSerialTerminal>,
    metadata: Option<NativeSerialMetadata>,
    pub(super) ready: bool,
    pub(super) refreshing: bool,
    refresh_needed: bool,
    refresh_aborted: bool,
    refresh_id: u64,
    dirty_storages: BTreeSet<String>,
    dirty_storage_overflow: bool,
    policy_revision: Option<u64>,
}
impl NativeState {
    pub(super) fn managed(&self) -> bool {
        self.shared.managed.load(Ordering::Acquire)
    }
}
#[derive(Clone, Copy)]
pub(super) enum ProbeKind {
    Status,
    Pin,
}
pub(super) struct NativeQueryCompletion {
    session: NativeSerialSession,
    binding: Option<SerialCardBinding>,
    refresh_id: u64,
    probe: Option<ProbeKind>,
    probe_seen: bool,
    pub(super) reply: SyncSender<Result<CommandResult, ActorError>>,
}
pub(super) enum NativeRequest {
    Claim {
        reply: SyncSender<Result<(), ActorError>>,
    },
    Open {
        path: String,
        baud: u32,
        reply: SyncSender<Result<NativeSerialSession, ActorError>>,
    },
    Initialize {
        session: NativeSerialSession,
        reply: SyncSender<Result<InitializationResult, ActorError>>,
    },
    Close {
        session: NativeSerialSession,
        reply: SyncSender<Result<(), ActorError>>,
    },
    BeginMetadata {
        session: NativeSerialSession,
        reply: SyncSender<Result<u64, ActorError>>,
    },
    Query {
        session: NativeSerialSession,
        binding: Option<SerialCardBinding>,
        refresh_id: u64,
        command: String,
        probe: Option<ProbeKind>,
        reply: SyncSender<Result<CommandResult, ActorError>>,
    },
    FinishMetadata {
        session: NativeSerialSession,
        refresh_id: u64,
        metadata: Option<NativeSerialMetadata>,
        reply: SyncSender<Result<(), ActorError>>,
    },
    Commit {
        session: NativeSerialSession,
        reply: SyncSender<Result<(), ActorError>>,
    },
}
pub(super) fn native_owned() -> ActorError {
    ActorError::new(
        "serial_native_owner",
        "Serial lifecycle and commands belong to the native owner",
    )
}
pub(super) fn native_stale() -> ActorError {
    ActorError::new(
        "serial_session_changed",
        "Native serial observation belongs to a retired identity",
    )
}
fn native_busy() -> ActorError {
    ActorError::new(
        "serial_command_busy",
        "Native serial physical transaction is busy",
    )
}

impl ModemActor {
    pub(crate) fn claim_native_serial_management(&self) -> Result<(), ActorError> {
        self.submit(
            |reply| Command::Native(NativeRequest::Claim { reply }),
            RPC_TIMEOUT,
        )
    }
    pub(crate) fn native_serial_managed(&self) -> bool {
        self.client.native_shared.managed.load(Ordering::Acquire)
    }
    pub(crate) fn native_serial_observation(&self) -> NativeSerialObservation {
        self.client
            .native_shared
            .facts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn native_serial_session(&self) -> Option<NativeSerialSession> {
        self.client
            .native_shared
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) async fn wait_native_serial_change(
        &self,
        after_revision: u64,
    ) -> NativeSerialObservation {
        loop {
            let changed = self.client.native_shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let facts = self.native_serial_observation();
            if facts.revision != after_revision {
                return facts;
            }
            changed.await;
        }
    }
    async fn retained_native<T: Send + 'static>(
        &self,
        request: impl FnOnce(SyncSender<Result<T, ActorError>>) -> NativeRequest + Send + 'static,
    ) -> Result<T, ActorError> {
        let actor = self.clone();
        tokio::task::spawn_blocking(move || actor.native_request(request))
            .await
            .map_err(|_| {
                ActorError::new(
                    "serial_native_worker_failed",
                    "Native serial worker failed before completion",
                )
            })?
    }
    fn native_request<T>(
        &self,
        request: impl FnOnce(SyncSender<Result<T, ActorError>>) -> NativeRequest,
    ) -> Result<T, ActorError> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.client
            .sender
            .send(Command::Native(request(reply)))
            .map_err(|_| ActorError::new("serial_actor_stopped", "Native serial actor stopped"))?;
        receive.recv().map_err(|_| {
            ActorError::new(
                "serial_actor_stopped",
                "Native serial actor stopped before completion",
            )
        })?
    }
    pub(crate) async fn open_native_serial(
        &self,
        path: String,
        baud: u32,
    ) -> Result<NativeSerialSession, ActorError> {
        if path.trim() != path
            || !path.starts_with("/dev/")
            || path.len() > MAX_PORT_NAME_BYTES
            || path.chars().any(char::is_control)
            || !(1200..=4_000_000).contains(&baud)
        {
            return Err(ActorError::invalid("Native serial path or baud is invalid"));
        }
        self.retained_native(move |reply| NativeRequest::Open { path, baud, reply })
            .await
    }
    pub(crate) async fn initialize_native_serial(
        &self,
        session: &NativeSerialSession,
    ) -> Result<NativeSerialInitialization, ActorError> {
        let session = session.clone();
        self.retained_native(move |reply| NativeRequest::Initialize { session, reply })
            .await
    }
    pub(crate) async fn close_native_serial(
        &self,
        session: &NativeSerialSession,
    ) -> Result<(), ActorError> {
        let session = session.clone();
        self.retained_native(move |reply| NativeRequest::Close { session, reply })
            .await
    }
    pub(crate) async fn commit_native_serial_ready(
        &self,
        session: &NativeSerialSession,
    ) -> Result<(), ActorError> {
        let session = session.clone();
        self.retained_native(move |reply| NativeRequest::Commit { session, reply })
            .await
    }
    pub(crate) async fn refresh_native_serial_metadata(
        &self,
        session: &NativeSerialSession,
    ) -> Result<NativeSerialMetadata, ActorError> {
        let actor = self.clone();
        let session = session.clone();
        // The entire plan and final cache receipt survive observer cancellation.
        tokio::task::spawn_blocking(move || actor.collect_native_metadata(session))
            .await
            .map_err(|_| {
                ActorError::new(
                    "serial_native_worker_failed",
                    "Native metadata worker failed before completion",
                )
            })?
    }
}

impl ActorState {
    fn native_session_current(&self, session: &NativeSerialSession) -> bool {
        self.native.managed()
            && self.port.is_some()
            && session.serial_session == self.serial_session
            && session.owner_epoch == self.serial_sms.owner_epoch
    }
    pub(super) fn native_session_opened(&mut self) {
        if !self.native.managed() {
            return;
        }
        self.native.last_port = self.current_path.clone().unwrap_or_default();
        self.native.discovery_error.clear();
        self.native.terminal = None;
        self.native.metadata = None;
        self.native.ready = false;
        self.native.refresh_needed = true;
        self.native.refreshing = false;
        self.native.refresh_aborted = false;
        self.native.dirty_storages.clear();
        self.native.dirty_storage_overflow = false;
        let session = NativeSerialSession {
            owner_epoch: self.serial_sms.owner_epoch.clone(),
            serial_session: self.serial_session,
            facts: Arc::new(Mutex::new(NativeSerialObservation::default())),
        };
        self.native.session = Some(session.clone());
        *self
            .native
            .shared
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(session);
        self.initialization.required = true;
        // Configure collection before first poll; dispatch is separately gated.
        let _ = self.configure_receive(true);
        self.last_radio_receive_enabled = true;
        self.publish_native_facts();
    }
    pub(super) fn publish_native_facts(&self) {
        if !self.native.managed() {
            return;
        }
        let sms = self.serial_sms_observation();
        let initialized = self.port.is_some()
            && matches!(&self.initialization.cached, Some(Ok(value))
            if value.completed && value.probe_succeeded);
        let metadata = self
            .native
            .metadata
            .as_ref()
            .filter(|value| {
                self.local_native_binding().as_ref() == Some(&value.binding)
                    && value.suppression_revision
                        == self.native.shared.policy_revision.load(Ordering::Acquire)
            })
            .cloned();
        let mut previous = self
            .native
            .shared
            .facts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut observation = NativeSerialObservation {
            revision: previous.revision,
            managed: true,
            port: self.native.last_port.clone(),
            discovery_error: self.native.discovery_error.clone(),
            serial_session: self.port.as_ref().map(|_| self.serial_session),
            sim_generation: self.sms_generation,
            initialized,
            ready: initialized && self.native.ready,
            sim_present: sms.sim_present,
            pin_blocked: sms.pin_blocked,
            refresh_needed: self.port.is_some() && self.native.refresh_needed,
            metadata,
            terminal: self.native.terminal.clone(),
        };
        if *previous != observation {
            observation.revision = previous.revision.saturating_add(1);
            *previous = observation.clone();
            if let Some(session) = &self.native.session {
                let mut retained = session.facts.lock().unwrap_or_else(|e| e.into_inner());
                // A later open attempt may change global port/error facts, but
                // the original session keeps its final physical-close proof.
                if retained.terminal.is_none() {
                    *retained = observation;
                }
            }
            drop(previous);
            self.native.shared.changed.notify_waiters();
        }
    }
    fn local_native_binding(&self) -> Option<SerialCardBinding> {
        self.port.as_ref()?;
        if self.serial_sms.generation_exhausted() {
            return None;
        }
        Some(SerialCardBinding {
            owner_epoch: self.serial_sms.owner_epoch.clone(),
            serial_session: self.serial_session,
            sim_generation: self.sms_generation,
        })
    }
    /// Single physical-drop point. The terminal receipt is published only
    /// after the sole port object and its OS handle have actually been dropped.
    pub(super) fn retire_physical_port(&mut self, code: &'static str) {
        // A recovery commit holds this same publication lock. Revoke the
        // shared card binding before dropping the physical port or clearing
        // its session, so retirement cannot race a journal unlock decision.
        let card_shared = self.card_shared.clone();
        let mut published = card_shared
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *published = None;
        let port = self.port.take();
        drop(port);
        self.live_serial_session.store(0, Ordering::Release);
        drop(published);
        if self.native.managed() {
            self.native.ready = false;
            self.native.metadata = None;
            self.native.refresh_aborted = true;
            self.native.refresh_needed = false;
            self.native.dirty_storages.clear();
            self.native.dirty_storage_overflow = false;
            if self.native.terminal.is_none() && self.serial_session != 0 {
                self.native.terminal = Some(NativeSerialTerminal {
                    owner_epoch: self.serial_sms.owner_epoch.clone(),
                    serial_session: self.serial_session,
                    code,
                    physical_closed: true,
                });
            }
            *self
                .native
                .shared
                .session
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.publish_native_facts();
        }
    }
    pub(super) fn observe_native_policy(&mut self) {
        if !self.native.managed() {
            return;
        }
        let revision = self.native.shared.policy_revision.load(Ordering::Acquire);
        if self.native.policy_revision != Some(revision) {
            self.native.policy_revision = Some(revision);
            self.native.metadata = None;
            if self.native.refreshing {
                self.native.refresh_aborted = true;
            }
            if self.port.is_some() {
                self.native.refresh_needed = true;
            }
        }
    }
    pub(super) fn release_idle_frames(&mut self) {
        while let Some(frame) = self.frames.pop_front() {
            if self.native.managed() {
                self.queued_bytes = self.queued_bytes.saturating_sub(frame.data.len());
            } else {
                self.command_events.push_back(frame);
            }
        }
        if self.native.managed() {
            for frame in self.command_events.drain(..) {
                self.queued_bytes = self.queued_bytes.saturating_sub(frame.data.len());
            }
        }
    }
    pub(super) fn observe_native_sms(&mut self, event: Option<&SmsStorageEvent>) {
        if !self.native.managed() || (self.native.ready && !self.native.refreshing) {
            return;
        }
        if let Some(storage) = event.and_then(|value| normalize_sms_storage(&value.storage)) {
            if self.native.dirty_storages.len() < 63
                || self.native.dirty_storages.contains(&storage)
            {
                self.native.dirty_storages.insert(storage);
            } else {
                self.native.dirty_storage_overflow = true;
            }
            // Existing queue stays bounded and the normal storage scan is
            // always scheduled; never transfer an old index to a new SIM.
        }
    }
    pub(super) fn native_probe_response(&self, event: Option<&SimEvent>) -> bool {
        let Some(event) = event else {
            return false;
        };
        self.pending_command.as_ref().is_some_and(|pending| {
            let CommandCompletion::NativeQuery(query) = &pending.completion else {
                return false;
            };
            matches!(
                (query.probe, event),
                (Some(ProbeKind::Status), SimEvent::Status { .. })
                    | (Some(ProbeKind::Pin), SimEvent::PinState { .. })
            )
        })
    }
    pub(super) fn observe_native_sim(&mut self, event: Option<&SimEvent>) {
        let Some(event) = event.filter(|_| self.native.managed()) else {
            return;
        };
        let expected_probe = self.pending_command.as_mut().is_some_and(|pending| {
            let CommandCompletion::NativeQuery(query) = &mut pending.completion else {
                return false;
            };
            let expected = matches!(
                (query.probe, event),
                (Some(ProbeKind::Status), SimEvent::Status { .. })
                    | (Some(ProbeKind::Pin), SimEvent::PinState { .. })
            );
            if !expected || query.probe_seen {
                return false;
            }
            query.probe_seen = true;
            true
        });
        self.native.metadata = None;
        self.native.ready = false;
        if !expected_probe {
            self.native.refresh_needed = true;
            if self.native.refreshing {
                self.native.refresh_aborted = true;
            }
        }
    }
    pub(super) fn native_query_current(&self, query: &NativeQueryCompletion) -> bool {
        self.native_session_current(&query.session)
            && self.native.refreshing
            && !self.native.refresh_aborted
            && self.native.refresh_id == query.refresh_id
            && query
                .binding
                .as_ref()
                .is_none_or(|binding| self.local_native_binding().as_ref() == Some(binding))
    }
    pub(super) fn finish_native_query(
        &mut self,
        query: NativeQueryCompletion,
        result: &'static str,
        lines: Vec<String>,
        terminal: Option<String>,
    ) {
        if !matches!(result, "ok" | "error") {
            if self.native_session_current(&query.session) {
                self.retire_physical_port("serial_metadata_query_uncertain");
                self.close_session();
            }
            let _ = query.reply.send(Err(ActorError::new(
                "serial_metadata_query_uncertain",
                "Native metadata query did not settle",
            )));
            return;
        }
        if !self.native_query_current(&query) {
            let _ = query.reply.send(Err(native_stale()));
            return;
        }
        let _ = query.reply.send(Ok(CommandResult {
            lines,
            result,
            terminal,
            elapsed_ms: 0,
            timeout_streak: self.timeout_streak,
            timeout_threshold: COMMAND_TIMEOUT_WATCHDOG_THRESHOLD,
            watchdog_tripped: false,
        }));
    }
    pub(super) fn process_native_request(&mut self, request: NativeRequest, opener: &PortOpener) {
        match request {
            NativeRequest::Claim { reply } => {
                if self.port.is_some() && !self.native.managed() {
                    let _ = reply.send(Err(native_busy()));
                    return;
                }
                self.native.shared.managed.store(true, Ordering::Release);
                self.card_shared.managed.store(true, Ordering::Release);
                self.publish_native_facts();
                let _ = reply.send(Ok(()));
            }
            NativeRequest::Open { path, baud, reply } => {
                if !self.native.managed() {
                    let _ = reply.send(Err(native_owned()));
                    return;
                }
                self.native.last_port = path.clone();
                self.native.discovery_error.clear();
                self.publish_native_facts();
                let (opened, result) = mpsc::sync_channel(1);
                self.process(
                    Command::Open {
                        path,
                        baud_rate: baud,
                        initialization_required: true,
                        reply: opened,
                    },
                    opener,
                );
                let result = result
                    .try_recv()
                    .map_err(|_| native_busy())
                    .and_then(|value| value)
                    .and_then(|session| {
                        self.native
                            .session
                            .clone()
                            .filter(|value| value.serial_session == session)
                            .ok_or_else(native_stale)
                    });
                if let Err(error) = &result {
                    self.native.discovery_error = error.code.to_owned();
                }
                self.publish_native_facts();
                let _ = reply.send(result);
            }
            NativeRequest::Initialize { session, reply } => {
                if !self.native_session_current(&session) {
                    let _ = reply.send(Err(native_stale()));
                    return;
                }
                self.process(
                    Command::CallModeRadioInitialize {
                        serial_session: session.serial_session,
                        deadline: Instant::now() + Duration::from_secs(60),
                        cancelled: Arc::new(AtomicBool::new(false)),
                        reply,
                    },
                    opener,
                );
            }
            NativeRequest::Close { session, reply } => {
                if !self.native_session_current(&session) {
                    let closed = session.observation().terminal.is_some_and(|value| {
                        value.physical_closed
                            && value.owner_epoch == session.owner_epoch
                            && value.serial_session == session.serial_session
                    });
                    let _ = reply.send(if closed { Ok(()) } else { Err(native_stale()) });
                    return;
                }
                self.process(
                    Command::FencedClose {
                        serial_session: session.serial_session,
                        reply,
                    },
                    opener,
                );
            }
            NativeRequest::BeginMetadata { session, reply } => {
                if !self.native_session_current(&session) {
                    let _ = reply.send(Err(native_stale()));
                    return;
                }
                if self.module_hold.is_some()
                    || self.native.refreshing
                    || self.card_lease.is_some()
                    || self.pending_command.is_some()
                    || !self.pending_high_commands.is_empty()
                    || !self.pending_normal_commands.is_empty()
                    || self.deferred_radio.is_some()
                    || self.deferred_sms_submit.is_some()
                    || self.initialization.running
                    || self.sms_receive.is_active()
                {
                    let _ = reply.send(Err(native_busy()));
                    return;
                }
                if !matches!(&self.initialization.cached, Some(Ok(value)) if value.completed && value.probe_succeeded)
                {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_initialization_unconfirmed",
                        "Native serial initialization is unconfirmed",
                    )));
                    return;
                }
                for storage in self.sms_receive.native_dirty_storages() {
                    if self.native.dirty_storages.len() < 63
                        || self.native.dirty_storages.contains(&storage)
                    {
                        self.native.dirty_storages.insert(storage);
                    } else {
                        self.native.dirty_storage_overflow = true;
                    }
                }
                let Some(id) = self.native.refresh_id.checked_add(1) else {
                    let _ = reply.send(Err(native_stale()));
                    return;
                };
                self.native.refresh_id = id;
                self.native.refreshing = true;
                self.native.refresh_aborted = false;
                self.native.refresh_needed = false;
                if reply.send(Ok(id)).is_err() {
                    self.native.refreshing = false;
                    self.wake_sms_receive();
                }
            }
            NativeRequest::Query {
                session,
                binding,
                refresh_id,
                command,
                probe,
                reply,
            } => {
                let query = NativeQueryCompletion {
                    session,
                    binding,
                    refresh_id,
                    probe,
                    probe_seen: false,
                    reply,
                };
                if !self.native_query_current(&query) {
                    let _ = query.reply.send(Err(native_stale()));
                    return;
                }
                if self.card_lease.is_some()
                    || self.pending_command.is_some()
                    || !self.pending_high_commands.is_empty()
                    || !self.pending_normal_commands.is_empty()
                {
                    let _ = query.reply.send(Err(native_busy()));
                    return;
                }
                if !native_metadata_command(&command) {
                    let _ = query.reply.send(Err(ActorError::invalid(
                        "Unsupported native metadata command",
                    )));
                    return;
                }
                if !self.cellular_command_allowed(&command) {
                    let _ = query.reply.send(Err(ActorError::new(
                        "module_flight_mode_blocks_cellular_probe",
                        "Cellular telemetry is suppressed",
                    )));
                    return;
                }
                let mut bytes = command.into_bytes();
                bytes.extend_from_slice(b"\r\n");
                self.pending_normal_commands.push_back(QueuedCommand {
                    bytes,
                    timeout: Duration::from_secs(3),
                    follow_up: None,
                    expected_sms_generation: None,
                    high_priority: false,
                    completion: CommandCompletion::NativeQuery(query),
                });
            }
            NativeRequest::FinishMetadata {
                session,
                refresh_id,
                metadata,
                reply,
            } => {
                if self.native.refresh_id != refresh_id || !self.native.refreshing {
                    let _ = reply.send(Err(native_stale()));
                    return;
                }
                let valid = self.native_session_current(&session)
                    && !self.native.refresh_aborted
                    && metadata.as_ref().is_none_or(|value| {
                        self.local_native_binding().as_ref() == Some(&value.binding)
                            && value.suppression_revision % 2 == 0
                            && value.suppression_revision
                                == self.native.shared.policy_revision.load(Ordering::Acquire)
                            && value
                                .status
                                .get("cellular_suppressed")
                                .and_then(Value::as_bool)
                                == Some(self.cellular_suppressed.load(Ordering::Acquire))
                    });
                if metadata.is_none() && self.port.is_some() {
                    self.native.refresh_needed = true;
                }
                if valid {
                    if let Some(metadata) = metadata {
                        if self.serial_sms_observation().sim_present == Some(true)
                            && !self.serial_sms_observation().pin_blocked
                        {
                            self.serial_sms.phone = metadata
                                .status
                                .get("phone_number")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                        }
                        self.native.metadata = Some(metadata);
                        self.native.refresh_needed = false;
                    }
                } else if self.port.is_some() {
                    self.native.refresh_needed = true;
                }
                self.native.refreshing = false;
                self.wake_sms_receive();
                self.publish_native_facts();
                let _ = reply.send(if valid { Ok(()) } else { Err(native_stale()) });
            }
            NativeRequest::Commit { session, reply } => {
                if !self.native_session_current(&session)
                    || self.native.refreshing
                    || self.native.metadata.as_ref().is_none_or(|value| {
                        self.local_native_binding().as_ref() != Some(&value.binding)
                            || value.suppression_revision
                                != self.native.shared.policy_revision.load(Ordering::Acquire)
                    })
                {
                    let _ = reply.send(Err(native_stale()));
                    return;
                }
                let initialized = matches!(&self.initialization.cached, Some(Ok(value)) if value.completed && value.probe_succeeded);
                if !initialized {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_initialization_unconfirmed",
                        "Native serial initialization is unconfirmed",
                    )));
                    return;
                }
                if self.native.dirty_storage_overflow {
                    let _ = reply.send(Err(ActorError::new("sms_receive_queue_full", "Native SMS storage reconciliation exceeded its bounded mailbox; sources retained")));
                    return;
                }
                let storages = self.native.dirty_storages.iter().cloned().collect();
                let result = self.sms_receive.native_rescan(
                    self.serial_session,
                    self.sms_generation,
                    storages,
                );
                if result.is_ok() {
                    self.native.ready = true;
                    self.native.dirty_storages.clear();
                }
                self.publish_native_facts();
                self.wake_sms_receive();
                let _ = reply.send(result);
            }
        }
    }
}

fn native_metadata_command(command: &str) -> bool {
    matches!(
        command,
        "AT+QSIMSTAT?"
            | "AT+CPIN?"
            | "AT+CGSN"
            | "AT+CGMR"
            | "AT+CIMI"
            | "AT+CNUM"
            | "AT+QCCID"
            | "AT+COPS=3,2"
            | "AT+COPS?"
            | "AT+CREG?"
            | "AT+CSQ"
            | "AT+QENG=\"servingcell\""
            | "AT+CGDCONT?"
            | "AT+QIMS?"
            | "AT+QNWINFO"
            | "AT+QCFG=\"usbnet\"?"
    )
}
struct MetadataGuard {
    actor: ModemActor,
    session: NativeSerialSession,
    id: u64,
    finished: bool,
}
impl MetadataGuard {
    fn query(
        &self,
        command: &str,
        binding: Option<&SerialCardBinding>,
        probe: Option<ProbeKind>,
    ) -> Result<Vec<String>, ActorError> {
        let result = self.actor.native_request(|reply| NativeRequest::Query {
            session: self.session.clone(),
            binding: binding.cloned(),
            refresh_id: self.id,
            command: command.into(),
            probe,
            reply,
        })?;
        if result.result == "timeout" || result.result == "overflow" {
            return Err(ActorError::new(
                "serial_metadata_query_failed",
                "Native metadata response did not settle",
            ));
        }
        Ok(if result.result == "ok" {
            result.lines
        } else {
            Vec::new()
        })
    }
    fn finish(&mut self, metadata: Option<NativeSerialMetadata>) -> Result<(), ActorError> {
        self.finished = true;
        self.actor
            .native_request(|reply| NativeRequest::FinishMetadata {
                session: self.session.clone(),
                refresh_id: self.id,
                metadata,
                reply,
            })
    }
}
impl Drop for MetadataGuard {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish(None);
        }
    }
}

impl ModemActor {
    fn collect_native_metadata(
        &self,
        session: NativeSerialSession,
    ) -> Result<NativeSerialMetadata, ActorError> {
        let id = self.native_request(|reply| NativeRequest::BeginMetadata {
            session: session.clone(),
            reply,
        })?;
        let mut plan = MetadataGuard {
            actor: self.clone(),
            session,
            id,
            finished: false,
        };
        let status = plan.query("AT+QSIMSTAT?", None, Some(ProbeKind::Status))?;
        let inserted = match parse_qsimstat_inserted(&status) {
            Some(value) => value,
            None => parse_cpin_inserted(&plan.query("AT+CPIN?", None, Some(ProbeKind::Pin))?)
                .unwrap_or(false),
        };
        let binding = self.capture_serial_card_binding()?;
        if binding.serial_session != plan.session.serial_session
            || binding.owner_epoch != plan.session.owner_epoch
        {
            return Err(native_stale());
        }
        let read = |command| plan.query(command, Some(&binding), None);
        let suppression_revision = self.cellular_policy_revision();
        if suppression_revision % 2 != 0 {
            return Err(native_stale());
        }
        let mut result = DeviceInfoSnapshotResult::empty(self.cellular_suppressed());
        result.imei = parse_imei(&read("AT+CGSN")?);
        result.firmware = parse_firmware(&read("AT+CGMR")?);
        result.sim_inserted = inserted;
        if inserted {
            result.imsi = parse_imsi(&read("AT+CIMI")?);
            result.phone_number = parse_msisdn(&read("AT+CNUM")?);
        }
        result.iccid = parse_iccid(&read("AT+QCCID")?);
        if !result.cellular_suppressed {
            let _ = read("AT+COPS=3,2")?;
            result.operator = crate::operator_name::resolve_serving_operator_name(
                &parse_cops_operator(&read("AT+COPS?")?),
            );
            if let Some((reg, lac, cell)) = parse_creg(&read("AT+CREG?")?) {
                result.reg_status = reg;
                result.lac = lac;
                result.cell_id = cell;
                result.reg_status_text = match reg {
                    0 => "未注册",
                    1 => "已注册(本地)",
                    2 => "搜索中",
                    3 => "注册被拒",
                    5 => "已注册(漫游)",
                    _ => "未知",
                }
                .into();
            }
            result.signal_dbm = parse_csq(&read("AT+CSQ")?)
                .map(|value| value.1)
                .unwrap_or(-999);
            if let Some((rsrp, rsrq, _, _, _, _)) =
                parse_serving_cell_lte(&read("AT+QENG=\"servingcell\"")?)
            {
                result.signal_rsrp = rsrp;
                result.signal_rsrq = rsrq;
            }
        }
        result.apn = parse_apn(&read("AT+CGDCONT?")?);
        if !result.cellular_suppressed {
            result.ims_status = parse_qims(&read("AT+QIMS?")?).unwrap_or_default();
            let (mode, duplex, _, _) = parse_qnwinfo_radio(&read("AT+QNWINFO")?);
            result.network_mode = mode;
            result.network_duplex = duplex;
        }
        result.usbnet_mode = parse_usbnet(&read("AT+QCFG=\"usbnet\"?")?).unwrap_or(-1);
        if self.cellular_suppressed() && !result.cellular_suppressed {
            // No pre-policy cellular data may escape a suppression change.
            let mut empty = DeviceInfoSnapshotResult::empty(true);
            empty.imei = result.imei;
            empty.firmware = result.firmware;
            empty.sim_inserted = result.sim_inserted;
            empty.imsi = result.imsi;
            empty.phone_number = result.phone_number;
            empty.iccid = result.iccid;
            empty.apn = result.apn;
            empty.usbnet_mode = result.usbnet_mode;
            result = empty;
        }
        let metadata = NativeSerialMetadata {
            binding,
            suppression_revision,
            status: serde_json::to_value(result).map_err(|_| {
                ActorError::new(
                    "serial_metadata_invalid",
                    "Native metadata projection failed",
                )
            })?,
        };
        plan.finish(Some(metadata.clone()))?;
        Ok(metadata)
    }
}

#[cfg(test)]
#[path = "native_serial_tests.rs"]
mod tests;
