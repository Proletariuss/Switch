//! Durable network-recovery owner.
//!
//! HTTP and GUI waiters only observe a Rust-owned operation. Every wake,
//! manual recovery and explicit module reboot is identified before it can
//! reach `CFUN=1,1`. The durable arm record deliberately says `unknown`
//! before the physical dispatcher is entered, so a lost reply or daemon
//! crash can never authorize a second reboot.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use proletarius_sms::stable_digest_hex;
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const STORE_FILE: &str = "network-recovery-operations.json";
const SCHEMA_VERSION: u16 = 1;
const OPERATION_LIMIT: usize = 4_096;
// Candidate-only default: one active operation plus one admission/replay owner.
// This finite limit requires measured release memory/fairness acceptance.
const COORDINATOR_LIMIT: usize = 2;
#[path = "network_recovery/coordinator_owner.rs"]
mod coordinator_owner;
use coordinator_owner::{CoordinatorOwners, CoordinatorTicket, Finish as OwnerFinish};
const WAKE_DELAY: Duration = Duration::from_secs(12);
const REBOOT_COOLDOWN: Duration = Duration::from_secs(600);
const READBACK_DELAY: Duration = Duration::from_secs(2);
const READBACK_LIMIT: u8 = 30;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Trigger {
    Wake,
    Manual,
    Reboot,
}

impl Trigger {
    fn name(self) -> &'static str {
        match self {
            Self::Wake => "wake",
            Self::Manual => "manual",
            Self::Reboot => "reboot",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Submission {
    NotSubmitted,
    Unknown,
    Submitted,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Operation {
    accepted: bool,
    operation_id: String,
    client_request_id: String,
    payload_fingerprint: String,
    trigger: Trigger,
    phase: String,
    outcome: Option<String>,
    reason: Option<String>,
    submission: Submission,
    owner_identity: String,
    owner_generation: u64,
    created_at_ms: u64,
    updated_at_ms: u64,
    cooldown_until_ms: Option<u64>,
    sample_count: u8,
    poll_after_ms: u64,
}

impl Operation {
    fn done(&self) -> bool {
        self.outcome.is_some()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RequestIndex {
    operation_id: String,
    fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Snapshot {
    schema_version: u16,
    sequence: u64,
    operations: BTreeMap<String, Operation>,
    requests: BTreeMap<String, RequestIndex>,
    cooldown_until_ms: Option<u64>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            sequence: 0,
            operations: BTreeMap::new(),
            requests: BTreeMap::new(),
            cooldown_until_ms: None,
        }
    }
}

#[derive(Clone)]
enum Journal {
    Durable(crate::sms_store_health::GuardedArchive),
    Memory(Arc<Mutex<Vec<Value>>>),
}

impl Journal {
    fn read<T>(
        &self,
        decode: impl FnOnce(&[Value]) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        match self {
            Self::Durable(store) => store.read(decode),
            Self::Memory(items) => {
                let items = items
                    .lock()
                    .map_err(|_| failure("network_store_unavailable"))?;
                decode(&items)
            }
        }
    }

    fn mutate<T>(
        &self,
        mutation: impl FnOnce(&mut Vec<Value>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        match self {
            Self::Durable(store) => store.mutate(mutation),
            Self::Memory(items) => {
                let mut items = items
                    .lock()
                    .map_err(|_| failure("network_store_unavailable"))?;
                mutation(&mut items)
            }
        }
    }

    fn mutate_if_changed<T>(
        &self,
        mutation: impl FnOnce(&mut Vec<Value>) -> Result<(T, bool), StoreError>,
    ) -> Result<T, StoreError> {
        match self {
            Self::Durable(store) => store.mutate_if_changed(mutation),
            Self::Memory(items) => {
                let mut items = items
                    .lock()
                    .map_err(|_| failure("network_store_unavailable"))?;
                let mut candidate = items.clone();
                let (result, changed) = mutation(&mut candidate)?;
                if changed {
                    *items = candidate;
                }
                Ok(result)
            }
        }
    }
}

#[derive(Default)]
struct RuntimeState {
    active_operation_id: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Service {
    journal: Journal,
    runtime: Arc<Mutex<RuntimeState>>,
    owners: CoordinatorOwners,
    #[cfg(test)]
    terminal_gate: Arc<Mutex<Option<Arc<tests::TerminalGate>>>>,
}

impl Default for Service {
    fn default() -> Self {
        let service = Self {
            journal: Journal::Memory(Arc::new(Mutex::new(Vec::new()))),
            runtime: Arc::new(Mutex::new(RuntimeState::default())),
            #[cfg(test)]
            terminal_gate: Arc::default(),
            owners: CoordinatorOwners::new(std::num::NonZeroUsize::new(COORDINATOR_LIMIT).unwrap())
                .unwrap(),
        };
        service.recover().expect("memory recovery journal");
        service
    }
}

#[derive(Clone, Copy)]
enum Observation {
    Connected,
    Disconnected,
    Unrecoverable,
}

struct RebootFailure {
    reason: String,
    submitted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnerBinding {
    identity: String,
    generation: u64,
    radio_binding: String,
}

trait Runtime: Send + Sync + 'static {
    fn now_ms(&self) -> u64;
    fn binding(&self) -> OwnerBinding;
    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send;
    fn sample(&self) -> impl Future<Output = Observation> + Send;
    /// This is the authoritative radio reservation/USB-mode/call-safe commit,
    /// not a client-side status check followed by a separate mutation.
    fn reboot(
        &self,
        direct: bool,
        binding: &OwnerBinding,
    ) -> impl Future<Output = Result<(), RebootFailure>> + Send;
}

struct Admission {
    operation: Operation,
    dispatch: bool,
}

impl Service {
    pub(crate) fn new(root: &Path) -> Self {
        let service = Self {
            journal: Journal::Durable(ArchiveStore::new(root.join(STORE_FILE)).into()),
            runtime: Arc::new(Mutex::new(RuntimeState::default())),
            #[cfg(test)]
            terminal_gate: Arc::default(),
            owners: CoordinatorOwners::new(std::num::NonZeroUsize::new(COORDINATOR_LIMIT).unwrap())
                .unwrap(),
        };
        // GuardedArchive latches any failure. Keeping the service installed is
        // fail-closed: later requests return unavailable and dispatch nothing.
        let _ = service.recover();
        service
    }

    fn recover(&self) -> Result<(), StoreError> {
        self.journal.mutate(|items| {
            let mut snapshot = read_snapshot(items)?;
            let now = wall_clock_ms();
            for operation in snapshot.operations.values_mut() {
                if operation.done() {
                    continue;
                }
                let might_have_submitted = operation.submission != Submission::NotSubmitted
                    || matches!(operation.phase.as_str(), "submitting" | "readback");
                operation.phase = "finished".to_owned();
                operation.updated_at_ms = now;
                operation.poll_after_ms = 0;
                if might_have_submitted {
                    operation.submission = Submission::Unknown;
                    operation.outcome = Some("unknown".to_owned());
                    operation.reason = Some("daemon_restarted_after_reboot_arm".to_owned());
                    if let Some(until) = operation.cooldown_until_ms {
                        snapshot.cooldown_until_ms = Some(
                            snapshot
                                .cooldown_until_ms
                                .map_or(until, |current| current.max(until)),
                        );
                    }
                } else {
                    operation.outcome = Some("interrupted_before_submit".to_owned());
                    operation.reason = Some("daemon_restarted_before_reboot_submit".to_owned());
                }
            }
            persist(items, snapshot)
        })
    }

    fn snapshot(&self, id: &str) -> Result<Option<Operation>, StoreError> {
        self.journal.read(|items| {
            let snapshot = read_snapshot(items)?;
            Ok(snapshot.operations.get(id).cloned())
        })
    }

    fn admit(
        &self,
        client_request_id: &str,
        trigger: Trigger,
        binding: &OwnerBinding,
        now: u64,
    ) -> Result<Admission, StoreError> {
        validate_request_id(client_request_id)?;
        validate_binding(binding)?;
        let fingerprint = fingerprint(trigger);
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| failure("network_store_unavailable"))?;
        let admission = self.journal.mutate(|items| {
            let mut snapshot = read_snapshot(items)?;
            if let Some(prior) = snapshot.requests.get(client_request_id) {
                if prior.fingerprint != fingerprint {
                    return Err(failure("network_request_id_conflict"));
                }
                let operation = snapshot
                    .operations
                    .get(&prior.operation_id)
                    .cloned()
                    .ok_or_else(corrupt)?;
                return Ok(Admission {
                    operation,
                    dispatch: false,
                });
            }
            if snapshot.operations.len() >= OPERATION_LIMIT {
                return Err(failure("network_operation_limit_reached"));
            }
            let cooldown = snapshot.cooldown_until_ms.filter(|until| *until > now);
            // Existing-ID replay grants no new dispatch. A new dispatch must
            // pass the runtime-owner check before sequence/records/persist.
            // Terminal cooldown responses retain their existing no-dispatch behavior.
            // This mutex stays held across the complete journal mutation.
            if cooldown.is_none() && runtime.active_operation_id.is_some() {
                return Err(failure("network_operation_in_progress"));
            }
            if snapshot
                .operations
                .values()
                .any(|operation| !operation.done())
            {
                return Err(failure("network_operation_in_progress"));
            }
            snapshot.sequence = snapshot
                .sequence
                .checked_add(1)
                .ok_or_else(|| failure("network_operation_sequence_exhausted"))?;
            let digest = stable_digest_hex(
                format!("network-operation-v1\0{client_request_id}\0{fingerprint}").as_bytes(),
            );
            let operation_id = format!(
                "network-{}-{}",
                snapshot.sequence,
                &digest[..16.min(digest.len())]
            );
            let operation = Operation {
                accepted: true,
                operation_id: operation_id.clone(),
                client_request_id: client_request_id.to_owned(),
                payload_fingerprint: fingerprint.clone(),
                trigger,
                phase: if cooldown.is_some() {
                    "finished".to_owned()
                } else if trigger == Trigger::Wake {
                    "waiting".to_owned()
                } else {
                    "checking".to_owned()
                },
                outcome: cooldown.map(|_| "cooldown".to_owned()),
                reason: cooldown.map(|_| "reboot_cooldown_active".to_owned()),
                submission: Submission::NotSubmitted,
                owner_identity: binding.identity.clone(),
                owner_generation: binding.generation,
                created_at_ms: now,
                updated_at_ms: now,
                cooldown_until_ms: cooldown,
                sample_count: 0,
                poll_after_ms: if cooldown.is_some() { 0 } else { 500 },
            };
            snapshot.requests.insert(
                client_request_id.to_owned(),
                RequestIndex {
                    operation_id: operation_id.clone(),
                    fingerprint,
                },
            );
            snapshot.operations.insert(operation_id, operation.clone());
            persist(items, snapshot)?;
            Ok(Admission {
                dispatch: !operation.done(),
                operation,
            })
        })?;
        if admission.dispatch {
            // All refusal checks preceded persist; the held runtime guard
            // permits only this infallible identity assignment after commit.
            runtime.active_operation_id = Some(admission.operation.operation_id.clone());
        }
        Ok(admission)
    }

    pub(crate) fn begin_shutdown(&self) -> coordinator_owner::Snapshot {
        self.owners.begin_shutdown();
        self.owners.snapshot()
    }
    pub(crate) fn owner_status(&self) -> coordinator_owner::Snapshot {
        self.owners.snapshot()
    }
    pub(crate) async fn finish_pending(&self) -> Result<(), String> {
        self.owners
            .finish_pending()
            .await
            .map_err(|error| error.to_string())
    }
    fn try_reserve_start(&self, request_id: &str) -> Result<CoordinatorTicket, StoreError> {
        validate_request_id(request_id)?;
        self.owners.try_reserve().map_err(|error| {
            failure(match error {
                coordinator_owner::Refusal::Full => "network_recovery_admission_full",
                coordinator_owner::Refusal::Closed => "network_recovery_stopping",
                coordinator_owner::Refusal::Unavailable => "network_recovery_owner_unavailable",
            })
        })
    }
    // Existing unit callers retain their API; production constructs NativeRuntime
    // only after try_reserve_start succeeds at the HTTP producer entry.
    #[cfg(test)]
    async fn start<R: Runtime>(
        &self,
        request_id: &str,
        trigger: Trigger,
        runtime: R,
    ) -> Result<Operation, StoreError> {
        let ticket = self.try_reserve_start(request_id)?;
        self.start_reserved(ticket, request_id, trigger, runtime)
            .await
    }
    // Synchronous envelope construction and spawn: the returned response future
    // owns only the receiver. Not polling/dropping it cannot detach real owner accounting.
    fn start_reserved<R: Runtime>(
        &self,
        ticket: CoordinatorTicket,
        request_id: &str,
        trigger: Trigger,
        runtime: R,
    ) -> impl Future<Output = Result<Operation, StoreError>> + Send + 'static + use<R> {
        let owner = self.clone();
        let request_id = request_id.to_owned();
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(ticket.into_runner(async move {
            let binding = runtime.binding();
            let now = runtime.now_ms();
            let admission = crate::archive_io::run({
                let owner = owner.clone();
                let binding = binding.clone();
                move || owner.admit(&request_id, trigger, &binding, now)
            }).await;
            match admission {
                Ok(admission) => {
                    let operation = admission.operation;
                    let id = operation.operation_id.clone();
                    let _ = send.send(Ok(operation));
                    if admission.dispatch {
                        match owner.run(id, trigger, runtime, binding).await {
                            Ok(()) => OwnerFinish::Clean,
                            Err(_) => OwnerFinish::Uncertain,
                        }
                    } else { OwnerFinish::Clean }
                }
                Err(error) => {
                    let known_refusal = matches!(&error, StoreError::Configuration(code)
                        if matches!(code.as_str(), "invalid_network_request_id" | "invalid_network_owner_binding"
                            | "network_request_id_conflict" | "network_operation_in_progress"
                            | "network_operation_limit_reached"));
                    let _ = send.send(Err(error));
                    if known_refusal { OwnerFinish::Clean } else { OwnerFinish::Uncertain }
                }
            }
        }));
        async move {
            receive.await.map_err(|_| {
                StoreError::Io(std::io::Error::other(
                    "network recovery coordinator stopped",
                ))
            })?
        }
    }

    async fn update(
        &self,
        id: &str,
        update: impl FnOnce(&mut Operation, &mut Snapshot) -> Result<(), StoreError> + Send + 'static,
    ) -> Result<bool, StoreError> {
        let owner = self.clone();
        let id = id.to_owned();
        crate::archive_io::run(move || {
            // Admission takes runtime -> journal in this same order. Keep the
            // lock through commit and exact-ID retirement: a terminal snapshot
            // may be visible, but admission cannot see its obsolete runtime pin.
            // This worker completes even if the async observer is cancelled.
            let mut runtime = owner
                .runtime
                .lock()
                .map_err(|_| failure("network_store_unavailable"))?;
            let (applied, terminal) = owner.journal.mutate_if_changed(|items| {
                let mut snapshot = read_snapshot(items)?;
                let mut operation = snapshot.operations.remove(&id).ok_or_else(corrupt)?;
                if operation.done() {
                    return Ok(((false, true), false));
                }
                update(&mut operation, &mut snapshot)?;
                let terminal = operation.done();
                snapshot.operations.insert(id.clone(), operation);
                persist(items, snapshot)?;
                Ok(((true, terminal), true))
            })?;
            #[cfg(test)]
            if applied && terminal {
                let gate = owner.terminal_gate.lock().unwrap().take();
                if let Some(gate) = gate {
                    gate.pause();
                }
            }
            // Only successful writes/reads retire. A failed or uncertain
            // commit retains the pin and GuardedArchive health fence. Terminal
            // updates follow the final awaited physical step (or stale-owner
            // rejection before dispatch); they never grant a new reboot.
            if terminal && runtime.active_operation_id.as_deref() == Some(id.as_str()) {
                runtime.active_operation_id = None;
            }
            Ok(applied)
        })
        .await
    }

    async fn finish(
        &self,
        id: &str,
        outcome: &str,
        reason: Option<String>,
    ) -> Result<(), StoreError> {
        let now = wall_clock_ms();
        let outcome = outcome.to_owned();
        let result = self
            .update(id, move |operation, _| {
                operation.phase = "finished".to_owned();
                operation.outcome = Some(outcome);
                operation.reason = reason;
                operation.updated_at_ms = now;
                operation.poll_after_ms = 0;
                Ok(())
            })
            .await;
        result?;
        self.release_runtime(id);
        Ok(())
    }

    fn release_runtime(&self, id: &str) {
        let mut runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if runtime.active_operation_id.as_deref() == Some(id) {
            runtime.active_operation_id = None;
        }
    }

    async fn arm(&self, id: &str, binding: OwnerBinding, now: u64) -> Result<bool, StoreError> {
        let applied = self
            .update(id, move |operation, snapshot| {
                if operation.owner_identity != binding.identity
                    || operation.owner_generation != binding.generation
                {
                    operation.phase = "finished".to_owned();
                    operation.outcome = Some("stale_owner".to_owned());
                    operation.reason = Some("module_owner_changed_before_reboot".to_owned());
                    operation.updated_at_ms = now;
                    operation.poll_after_ms = 0;
                    return Ok(());
                }
                let until = now.saturating_add(REBOOT_COOLDOWN.as_millis() as u64);
                operation.phase = "submitting".to_owned();
                operation.submission = Submission::Unknown;
                operation.updated_at_ms = now;
                operation.cooldown_until_ms = Some(until);
                operation.poll_after_ms = 500;
                snapshot.cooldown_until_ms = Some(until);
                Ok(())
            })
            .await?;
        if !applied {
            return Ok(false);
        }
        Ok(self
            .snapshot(id)?
            .is_some_and(|operation| !operation.done() && operation.phase == "submitting"))
    }

    async fn reboot_result(
        &self,
        id: &str,
        result: Result<(), RebootFailure>,
        now: u64,
    ) -> Result<bool, StoreError> {
        let was_submitted = result.is_ok();
        let applied = self
            .update(id, move |operation, snapshot| {
                match result {
                    Ok(()) => {
                        operation.submission = Submission::Submitted;
                        operation.phase = "readback".to_owned();
                        operation.updated_at_ms = now;
                        operation.poll_after_ms = READBACK_DELAY.as_millis() as u64;
                    }
                    Err(error) if error.submitted => {
                        operation.submission = Submission::Unknown;
                        operation.phase = "finished".to_owned();
                        operation.outcome = Some("unknown".to_owned());
                        operation.reason = Some(error.reason);
                        operation.updated_at_ms = now;
                        operation.poll_after_ms = 0;
                    }
                    Err(error) => {
                        operation.submission = Submission::NotSubmitted;
                        operation.phase = "finished".to_owned();
                        operation.outcome = Some("blocked".to_owned());
                        operation.reason = Some(error.reason);
                        operation.updated_at_ms = now;
                        operation.poll_after_ms = 0;
                        if snapshot.cooldown_until_ms == operation.cooldown_until_ms {
                            snapshot.cooldown_until_ms = None;
                        }
                        operation.cooldown_until_ms = None;
                    }
                }
                Ok(())
            })
            .await?;
        Ok(applied && was_submitted)
    }

    async fn run<R: Runtime>(
        &self,
        id: String,
        trigger: Trigger,
        runtime: R,
        admitted_binding: OwnerBinding,
    ) -> Result<(), StoreError> {
        if trigger == Trigger::Wake {
            runtime.sleep(WAKE_DELAY).await;
        }
        if trigger != Trigger::Reboot {
            let now = runtime.now_ms();
            match self
                .update(&id, move |operation, _| {
                    operation.phase = "checking".to_owned();
                    operation.updated_at_ms = now;
                    Ok(())
                })
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    self.release_runtime(&id);
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
            match runtime.sample().await {
                Observation::Connected => {
                    self.finish(&id, "already_connected", None).await?;
                    return Ok(());
                }
                Observation::Unrecoverable => {
                    self.finish(
                        &id,
                        "not_recoverable",
                        Some("traffic_not_recoverable".into()),
                    )
                    .await?;
                    return Ok(());
                }
                Observation::Disconnected => {}
            }
        }

        // This write is the irreversible boundary. `unknown` and the absolute
        // cooldown are durable before module_radio may submit CFUN.
        let binding = runtime.binding();
        let now = runtime.now_ms();
        match self.arm(&id, binding, now).await {
            Ok(true) => {}
            Ok(false) => {
                self.release_runtime(&id);
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        // The durable arm may yield while A is replaced by B. Submit only
        // against the radio identity captured by the original admission.
        let reboot = runtime
            .reboot(trigger == Trigger::Reboot, &admitted_binding)
            .await;
        match self.reboot_result(&id, reboot, runtime.now_ms()).await {
            Ok(true) => {}
            Ok(false) => {
                self.release_runtime(&id);
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        for count in 1..=READBACK_LIMIT {
            runtime.sleep(READBACK_DELAY).await;
            let observation = runtime.sample().await;
            let now = runtime.now_ms();
            match self
                .update(&id, move |operation, _| {
                    operation.sample_count = count;
                    operation.updated_at_ms = now;
                    Ok(())
                })
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    self.release_runtime(&id);
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
            if matches!(observation, Observation::Connected) {
                self.finish(&id, "recovered", None).await?;
                return Ok(());
            }
        }
        self.finish(&id, "timed_out", Some("network_readback_exhausted".into()))
            .await
    }
}

#[derive(Clone)]
struct NativeRuntime(crate::ServerState);

impl Runtime for NativeRuntime {
    fn now_ms(&self) -> u64 {
        wall_clock_ms()
    }

    fn binding(&self) -> OwnerBinding {
        let description = self
            .0
            .usb_at
            .active_description()
            .unwrap_or_else(|| "module-missing".to_owned());
        OwnerBinding {
            identity: stable_digest_hex(format!("network-owner-v1\0{description}").as_bytes()),
            generation: self.0.availability.snapshot().generation,
            radio_binding: self.0.module_radio.public_mutation_binding(),
        }
    }

    async fn sleep(&self, delay: Duration) {
        tokio::time::sleep(delay).await;
    }

    async fn sample(&self) -> Observation {
        let product =
            crate::network_diagnostic::discover_module_product(self.0.usb_at.active_description())
                .await;
        let snapshot = crate::network_traffic::capture(
            &self.0.network_traffic,
            product.as_deref(),
            self.0.availability.snapshot().generation,
        )
        .await;
        if snapshot.available {
            Observation::Connected
        } else if snapshot.recovery_retryable {
            Observation::Disconnected
        } else {
            Observation::Unrecoverable
        }
    }

    async fn reboot(&self, direct: bool, binding: &OwnerBinding) -> Result<(), RebootFailure> {
        let result = if direct {
            self.0
                .module_radio
                .reboot_module(&binding.radio_binding)
                .await
        } else {
            self.0
                .module_radio
                .recover_network_reboot(&binding.radio_binding)
                .await
        };
        result.map_err(|error| RebootFailure {
            reason: error.code.to_owned(),
            submitted: error.command_submitted(),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    client_request_id: String,
    trigger: Trigger,
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    let owner = &state.network_traffic.recovery;
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    match method {
        Method::POST => {
            let bytes = match crate::http_server::collect_body(request.into_body(), 4096).await {
                Ok(bytes) => bytes,
                Err(crate::http_server::BodyReadError::Timeout) => {
                    return crate::http_server::body_timeout_response();
                }
                Err(_) => {
                    return error(StatusCode::BAD_REQUEST, "invalid_network_recovery_request");
                }
            };
            let Ok(input) = serde_json::from_slice::<StartRequest>(&bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_network_recovery_request");
            };
            let path_trigger = if path == "/api/network/reboot-module" {
                Trigger::Reboot
            } else {
                input.trigger
            };
            if (path == "/api/network/reboot-module" && input.trigger != Trigger::Reboot)
                || (path == "/api/network/recovery" && input.trigger == Trigger::Reboot)
            {
                return error(StatusCode::BAD_REQUEST, "invalid_network_recovery_trigger");
            }
            let ticket = match owner.try_reserve_start(&input.client_request_id) {
                Ok(ticket) => ticket,
                Err(StoreError::Configuration(code))
                    if matches!(
                        code.as_str(),
                        "network_recovery_admission_full" | "network_recovery_stopping"
                    ) =>
                {
                    return crate::json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &serde_json::json!({"error":code,"code":code,"retryable":true,
                            "retry_same_request_id":true,"client_request_id":input.client_request_id,
                            "attempt_started":false}),
                    );
                }
                Err(error) => return store_error(error),
            };
            match owner
                .start_reserved(
                    ticket,
                    &input.client_request_id,
                    path_trigger,
                    NativeRuntime(state.clone()),
                )
                .await
            {
                Ok(operation) => crate::json_response(StatusCode::ACCEPTED, &operation),
                Err(error_value) => store_error(error_value),
            }
        }
        Method::GET if path == "/api/network/recovery" => {
            let id = request.uri().query().and_then(|query| {
                let values: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).collect();
                (values.len() == 1 && values[0].0 == "operation_id")
                    .then(|| values[0].1.to_string())
            });
            let Some(id) = id else {
                return error(StatusCode::BAD_REQUEST, "invalid_network_recovery_query");
            };
            let lookup = owner.clone();
            match crate::archive_io::run(move || lookup.snapshot(&id)).await {
                Ok(Some(operation)) => crate::json_response(StatusCode::OK, &operation),
                Ok(None) => error(
                    StatusCode::NOT_FOUND,
                    "network_recovery_operation_not_found",
                ),
                Err(error_value) => store_error(error_value),
            }
        }
        _ => error(
            StatusCode::METHOD_NOT_ALLOWED,
            "network_recovery_method_not_allowed",
        ),
    }
}

fn read_snapshot(items: &[Value]) -> Result<Snapshot, StoreError> {
    if items.is_empty() {
        return Ok(Snapshot::default());
    }
    if items.len() != 1 {
        return Err(corrupt());
    }
    let snapshot: Snapshot = serde_json::from_value(items[0].clone()).map_err(StoreError::Json)?;
    if snapshot.schema_version != SCHEMA_VERSION
        || snapshot.operations.len() > OPERATION_LIMIT
        || snapshot.requests.len() != snapshot.operations.len()
        || snapshot
            .operations
            .values()
            .filter(|value| !value.done())
            .count()
            > 1
    {
        return Err(corrupt());
    }
    for (request_id, index) in &snapshot.requests {
        validate_request_id(request_id).map_err(|_| corrupt())?;
        if index.fingerprint.len() != 64
            || !index
                .fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(corrupt());
        }
        let Some(operation) = snapshot.operations.get(&index.operation_id) else {
            return Err(corrupt());
        };
        if operation.client_request_id != *request_id
            || operation.payload_fingerprint != index.fingerprint
        {
            return Err(corrupt());
        }
    }
    Ok(snapshot)
}

fn persist(items: &mut Vec<Value>, snapshot: Snapshot) -> Result<(), StoreError> {
    *items = vec![serde_json::to_value(snapshot).map_err(StoreError::Json)?];
    Ok(())
}

fn fingerprint(trigger: Trigger) -> String {
    stable_digest_hex(format!("network-recovery-v1\0{}", trigger.name()).as_bytes())
}

fn validate_request_id(value: &str) -> Result<(), StoreError> {
    if value.is_empty()
        || value.len() > 128
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(failure("invalid_network_request_id"));
    }
    Ok(())
}

fn validate_binding(binding: &OwnerBinding) -> Result<(), StoreError> {
    if binding.identity.len() != 64
        || !binding
            .identity
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(failure("invalid_network_owner_binding"));
    }
    Ok(())
}

fn wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn store_error(error_value: StoreError) -> Response<Full<Bytes>> {
    let code = match &error_value {
        StoreError::Configuration(code) => code.as_str(),
        _ => "network_store_unavailable",
    };
    let status = match code {
        "invalid_network_request_id" | "invalid_network_owner_binding" => StatusCode::BAD_REQUEST,
        "network_request_id_conflict" | "network_operation_in_progress" => StatusCode::CONFLICT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    error(status, code)
}

fn failure(code: &str) -> StoreError {
    StoreError::Configuration(code.to_owned())
}

fn corrupt() -> StoreError {
    failure("invalid_store_schema")
}

fn error(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &serde_json::json!({"error": code, "code": code}))
}

#[cfg(test)]
mod tests;
