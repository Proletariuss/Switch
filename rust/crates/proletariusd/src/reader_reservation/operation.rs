//! Explicit workflow capabilities. A caller's HTTP lifetime never determines
//! physical ownership: retained cleanup waits for queued jobs and PC/SC proof.
use super::*;
use crate::reader_pcsc::{Binding, LifecycleBinding, Owner};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(120);

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Authority {
    Idle {},
    Business {
        token: String,
        sequence: u64,
        role: String,
    },
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperationBinding {
    pub(crate) producer_id: String,
    pub(crate) operation_token: String,
    pub(crate) operation_sequence: u64,
    pub(crate) reader: String,
    pub(crate) reader_generation: u64,
    pub(crate) purpose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target_reader: Option<String>,
    pub(crate) authority: Authority,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CardTransaction {
    Operation {
        binding: OperationBinding,
    },
    Native {
        binding: NativePinBinding,
        purpose: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_sequence: Option<u64>,
    },
}
// Capabilities must not accidentally appear in transport diagnostics.
impl std::fmt::Debug for CardTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CardTransaction([redacted])")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Active,
    Draining,
    Released,
}
impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Released => "released",
        }
    }
}
pub(super) struct Entry {
    pub(super) binding: OperationBinding,
    pub(super) reader: String,
    pub(super) generation: u64,
    pub(super) phase: Phase,
    pub(super) deadline: Instant,
    pub(super) jobs: usize,
    pub(super) pcsc: Vec<Binding>,
    pub(super) sessions: Vec<LifecycleBinding>,
    // Only native_multi can mint this; wire clients cannot select an issuer.
    pub(super) native: bool,
    pub(super) native_completed: Option<tokio::sync::watch::Sender<bool>>,
}
#[derive(Default)]
pub(super) struct OperationState {
    pub(super) entries: HashMap<String, Entry>,
    terminal: VecDeque<String>,
    high_water: u64,
    pub(super) quiescing: bool,
}
impl OperationState {
    pub(super) fn workflow_running(&self, token: &str) -> bool {
        self.entries.get(token).is_some_and(|entry| entry.jobs == 1)
    }
    pub(super) fn active(&self) -> bool {
        self.entries
            .values()
            .any(|entry| entry.phase != Phase::Released)
    }
    pub(super) fn retire_producer(&mut self) {
        self.high_water = 0;
        self.quiescing = false;
        for e in self.entries.values_mut() {
            if !e.native && e.phase == Phase::Active {
                e.phase = Phase::Draining;
            }
        }
    }
    pub(super) fn advance(&mut self, token: &str, snapshot: &SelectionSnapshot) {
        if let Some(e) = self.entries.get_mut(token) {
            e.reader = snapshot.reader.clone();
            e.generation = snapshot.generation;
        }
    }
    pub(super) fn lifecycle_allowed(&self, token: &str, kind: &str, reader: &str) -> bool {
        self.entries.get(token).is_some_and(|e| {
            e.phase == Phase::Active
                && e.jobs == 0
                && Instant::now() < e.deadline
                && match kind {
                    "connect" => {
                        e.binding.purpose == "lifecycle"
                            && e.binding.target_reader.as_deref() == Some(reader)
                    }
                    "disconnect" => {
                        matches!(e.binding.purpose.as_str(), "lifecycle" | "presence_probe")
                            && e.reader == reader
                    }
                    "identity" | "rebuild" => e.reader == reader,
                    _ => false,
                }
        })
    }
    pub(super) fn contains(&self, token: &str) -> bool {
        self.entries.contains_key(token)
    }
    pub(super) fn borrows_business(&self, token: &str, sequence: u64) -> bool {
        self.entries.values().any(|e| {
            e.phase != Phase::Released
                && matches!(&e.binding.authority,
            Authority::Business { token: t, sequence: s, .. } if t == token && *s == sequence)
        })
    }
}

pub(crate) struct CardIoPermit(Arc<Gate>, CardTransaction);
impl Drop for CardIoPermit {
    fn drop(&mut self) {
        let mut s = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
        match &self.1 {
            CardTransaction::Operation { binding } => {
                if let Some(e) = s.operations.entries.get_mut(&binding.operation_token)
                    && &e.binding == binding
                {
                    e.jobs = e.jobs.saturating_sub(1);
                }
            }
            CardTransaction::Native { binding, .. } => s.native.finish_adapter_io(binding),
        }
    }
}

fn valid_reader(reader: &str, empty: bool) -> bool {
    (empty && reader.is_empty() || !reader.is_empty() && reader.trim() == reader)
        && reader.len() <= 512
        && !reader.contains('\0')
        && (!reader.starts_with("BLE:") || crate::reader_ble_apdu::valid_ble_reader(reader))
}
fn validate(b: &OperationBinding) -> Result<(), &'static str> {
    if b.producer_id.is_empty()
        || b.producer_id.len() > 128
        || !valid_operation_token(&b.operation_token)
        || b.operation_sequence == 0
        || !matches!(b.purpose.as_str(), "card" | "lifecycle" | "presence_probe")
        || !valid_reader(&b.reader, b.purpose == "lifecycle")
        || !b.reader.is_empty() && b.reader_generation == 0
        || b.target_reader
            .as_ref()
            .is_some_and(|r| b.purpose != "lifecycle" || !valid_reader(r, false))
    {
        return Err("invalid_reader_binding");
    }
    if let Authority::Business {
        token,
        sequence,
        role,
    } = &b.authority
        && (!valid_operation_token(token)
            || *sequence == 0
            || !matches!(role.as_str(), "operation" | "multistandby"))
    {
        return Err("invalid_request");
    }
    Ok(())
}
fn parent_current(s: &State, b: &OperationBinding) -> bool {
    match &b.authority {
        Authority::Idle {} => s.business.owner.is_none() && s.owner.is_none(),
        Authority::Business {
            token,
            sequence,
            role,
        } => {
            s.business
                .matches_operation(token, *sequence, role, &b.reader, b.reader_generation)
                || s.native_multi.matches(b)
        }
    }
}
fn receipt(e: &Entry) -> Value {
    json!({"binding":e.binding,"phase":e.phase.name(),"effective_reader":e.reader,"effective_generation":e.generation})
}

pub(super) fn validate_io(
    s: &State,
    tx: &CardTransaction,
    producer: &str,
    reader: &str,
) -> Result<(), &'static str> {
    match tx {
        CardTransaction::Operation { binding } => {
            if binding.producer_id != producer
                && s.native_reader_producer.as_deref() != Some(producer)
            {
                return Err("owner_mismatch");
            }
            let e = s
                .operations
                .entries
                .get(&binding.operation_token)
                .filter(|e| &e.binding == binding)
                .ok_or("owner_mismatch")?;
            if if e.native {
                s.native_reader_producer.as_deref() != Some(binding.producer_id.as_str())
                    || !s.native_multi.matches(binding)
            } else {
                !s.business.is_producer(&binding.producer_id)
            } {
                return Err("stale_producer");
            }
            if e.phase != Phase::Active || Instant::now() >= e.deadline {
                return Err("owner_expired");
            }
            if !parent_current(s, binding) {
                return Err("owner_mismatch");
            }
            let scoped = if s.selection.pending() {
                s.selection.operation_io(&binding.operation_token, reader)
            } else {
                e.reader == reader && s.selection.confirms(reader, e.generation)
            };
            if !scoped {
                return Err("stale_reader_generation");
            }
        }
        CardTransaction::Native {
            binding,
            purpose,
            operation_sequence,
        } => {
            if !super::native::native_producer_current(s, &binding.producer_id)
                || binding.producer_id != producer
                || binding.reader != reader
            {
                return Err("owner_mismatch");
            }
            s.native
                .validate_adapter_io(binding, purpose, *operation_sequence)?;
        }
    }
    Ok(())
}

impl Gate {
    pub(crate) fn track_card_io(
        self: &Arc<Self>,
        tx: &CardTransaction,
        producer: &str,
        reader: &str,
    ) -> Result<CardIoPermit, &'static str> {
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        validate_io(&s, tx, producer, reader)?;
        match tx {
            CardTransaction::Operation { binding } => {
                let e = s
                    .operations
                    .entries
                    .get_mut(&binding.operation_token)
                    .ok_or("owner_mismatch")?;
                if e.jobs != 0 {
                    return Err("card_busy");
                }
                e.jobs = 1;
            }
            CardTransaction::Native { binding, .. } => s.native.start_adapter_io(binding)?,
        }
        Ok(CardIoPermit(self.clone(), tx.clone()))
    }

    pub(crate) fn register_card_pcsc(
        &self,
        tx: &CardTransaction,
        binding: &Binding,
    ) -> Result<(), &'static str> {
        binding
            .validate_for_reader(&binding.reader)
            .map_err(|_| "invalid_reader_binding")?;
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        validate_io(&s, tx, &binding.producer_id, &binding.reader)?;
        if s.native_reader_producer.as_deref() == Some(binding.producer_id.as_str())
            && !s.selection.physical.as_ref().is_some_and(|session| {
                session.producer == binding.producer_id
                    && session.reader == binding.reader
                    && session.session_id == binding.session_id
            })
        {
            return Err("stale_reader_session");
        }
        if let CardTransaction::Operation { binding: operation } = tx {
            let e = s
                .operations
                .entries
                .get_mut(&operation.operation_token)
                .ok_or("owner_mismatch")?;
            if !e.pcsc.contains(binding) {
                e.pcsc.push(binding.clone());
            }
        }
        Ok(())
    }

    pub(crate) fn register_card_pcsc_session(
        &self,
        tx: &CardTransaction,
        binding: &LifecycleBinding,
    ) -> Result<(), &'static str> {
        if !valid_operation_token(&binding.session_id) || binding.session_sequence == 0 {
            return Err("invalid_reader_binding");
        }
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        validate_io(&s, tx, &binding.producer, &binding.reader)?;
        let CardTransaction::Operation { binding: operation } = tx else {
            return Err("owner_mismatch");
        };
        if operation.purpose != "lifecycle" {
            return Err("owner_mismatch");
        }
        let e = s
            .operations
            .entries
            .get_mut(&operation.operation_token)
            .ok_or("owner_mismatch")?;
        if !e.sessions.contains(binding) {
            e.sessions.push(binding.clone());
        }
        Ok(())
    }

    fn operation_acquire(&self, b: &OperationBinding) -> Result<(Value, bool), &'static str> {
        validate(b)?;
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !s.business.is_producer(&b.producer_id) {
            return Err("stale_producer");
        }
        if let Some(e) = s.operations.entries.get(&b.operation_token) {
            return if &e.binding == b {
                Ok((receipt(e), false))
            } else {
                Err("owner_mismatch")
            };
        }
        if s.operations.quiescing {
            return Err("backend_stopping");
        }
        if b.operation_sequence <= s.operations.high_water {
            return Err("owner_expired");
        }
        if !parent_current(&s, b) {
            return Err("card_busy");
        }
        if s.native.active()
            || s.ble_control_owner.is_some()
            || s.selection.pending()
            || !s.workers.is_empty()
            || !s.ble_io_workers.is_empty()
            || !s.operation_workers.is_empty()
        {
            return Err("card_busy");
        }
        let observed = s.selection.observed();
        if observed.reader != b.reader || observed.generation != b.reader_generation {
            // A fresh producer's first connect may start from an empty Go mirror.
            if !(b.purpose == "lifecycle"
                && b.reader.is_empty()
                && b.reader_generation == 0
                && observed.reader.is_empty())
            {
                return Err("stale_reader_generation");
            }
        }
        let e = Entry {
            binding: b.clone(),
            reader: b.reader.clone(),
            generation: b.reader_generation,
            phase: Phase::Active,
            deadline: Instant::now() + TTL,
            jobs: 0,
            pcsc: Vec::new(),
            sessions: Vec::new(),
            native: false,
            native_completed: None,
        };
        let answer = receipt(&e);
        s.operations.high_water = b.operation_sequence;
        s.operation_workers
            .insert(b.operation_token.clone(), (e.reader.clone(), e.generation));
        s.operations.entries.insert(b.operation_token.clone(), e);
        Ok((answer, true))
    }

    fn operation_command(&self, b: &OperationBinding, action: &str) -> Result<Value, &'static str> {
        validate(b)?;
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !s.business.is_producer(&b.producer_id) {
            return Err("stale_producer");
        }
        let Some(e) = s.operations.entries.get_mut(&b.operation_token) else {
            if action == "release" {
                s.operations.high_water = s.operations.high_water.max(b.operation_sequence);
                let e = Entry {
                    binding: b.clone(),
                    reader: b.reader.clone(),
                    generation: b.reader_generation,
                    phase: Phase::Released,
                    deadline: Instant::now(),
                    jobs: 0,
                    pcsc: Vec::new(),
                    sessions: Vec::new(),
                    native: false,
                    native_completed: None,
                };
                let answer = receipt(&e);
                s.operations.entries.insert(b.operation_token.clone(), e);
                record_terminal(&mut s.operations, &b.operation_token);
                return Ok(answer);
            }
            return Err("operation_unknown");
        };
        if &e.binding != b {
            return Err("owner_mismatch");
        }
        if e.phase == Phase::Active && Instant::now() >= e.deadline {
            e.phase = Phase::Draining;
        }
        match action {
            "status" => (),
            "release" => {
                if e.phase == Phase::Active {
                    e.phase = Phase::Draining;
                }
            }
            "renew" => {
                if e.phase != Phase::Active {
                    return Err("owner_expired");
                }
                e.deadline = Instant::now() + TTL;
            }
            _ => return Err("invalid_request"),
        }
        Ok(receipt(e))
    }

    fn quiesce_operations(&self, producer: &str) -> Result<Value, &'static str> {
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !s.business.is_producer(producer) {
            return Err("stale_producer");
        }
        s.operations.quiescing = true;
        let quiet = s.operation_workers.is_empty()
            && s.workers.is_empty()
            && s.ble_io_workers.is_empty()
            && !s.native.active()
            && !s.selection.pending();
        Ok(json!({"producer_id":producer,"phase":if quiet {"quiescent"} else {"pending"}}))
    }

    #[cfg(test)]
    pub(crate) fn test_operation_command(
        &self,
        action: &str,
        value: Value,
    ) -> Result<Value, &'static str> {
        let b: OperationBinding = serde_json::from_value(value).map_err(|_| "invalid_request")?;
        if action == "acquire" {
            self.operation_acquire(&b).map(|(v, _)| v)
        } else {
            self.operation_command(&b, action)
        }
    }
}

fn record_terminal(s: &mut OperationState, token: &str) {
    s.terminal.push_back(token.to_owned());
    while s.terminal.len() > 128 {
        if let Some(t) = s.terminal.pop_front() {
            s.entries.remove(&t);
        }
    }
}

pub(super) async fn drive(gate: Arc<Gate>, owner: Owner, binding: OperationBinding) {
    loop {
        let work = {
            let mut s = gate.0.lock().unwrap_or_else(|e| e.into_inner());
            let Some(e) = s.operations.entries.get_mut(&binding.operation_token) else {
                return;
            };
            if e.binding != binding || e.phase == Phase::Released {
                return;
            }
            if e.phase == Phase::Active && Instant::now() >= e.deadline {
                e.phase = Phase::Draining;
            }
            (e.phase == Phase::Draining && e.jobs == 0)
                .then(|| (e.pcsc.clone(), e.sessions.clone()))
        };
        if let Some((leases, sessions)) = work {
            // One retained blocking cleanup at a time. Cancellation never
            // spawns another physical end/reset or treats timeout as proof.
            let physical = owner.clone();
            let producer = binding.producer_id.clone();
            let logical = gate.clone();
            let idle = tokio::task::spawn_blocking(move || {
                // Each physical binding has its own native producer. A Go
                // process exit is not proof that those workers ended.
                if leases.is_empty()
                    && sessions.is_empty()
                    && physical.producer_physically_idle(&producer)
                {
                    return true;
                }
                for b in leases {
                    if !physical.retire_operation_binding(&b).unwrap_or(false) {
                        return false;
                    }
                }
                for session in sessions {
                    let committed = logical
                        .0
                        .lock()
                        .is_ok_and(|s| s.selection.physical_matches(&session));
                    if !committed
                        && physical
                            .close(
                                &session.producer,
                                &session.reader,
                                &session.session_id,
                                session.session_sequence,
                                None,
                            )
                            .is_err()
                    {
                        return false;
                    }
                }
                true
            })
            .await
            .unwrap_or(false);
            if idle {
                let mut s = gate.0.lock().unwrap_or_else(|e| e.into_inner());
                if s.operations
                    .entries
                    .get(&binding.operation_token)
                    .is_some_and(|e| {
                        e.binding == binding && e.jobs == 0 && e.phase == Phase::Draining
                    })
                    && s.ble_io_workers.is_empty()
                {
                    if s.selection
                        .retire_operation(&binding.operation_token)
                        .is_err()
                    {
                        continue;
                    }
                    if let Some(e) = s.operations.entries.get_mut(&binding.operation_token) {
                        e.phase = Phase::Released;
                        if let Some(completed) = &e.native_completed {
                            completed.send_replace(true);
                        }
                    }
                    s.operation_workers.remove(&binding.operation_token);
                    record_terminal(&mut s.operations, &binding.operation_token);
                    super::business::finish_retired_business(&mut s);
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod legacy_wire_oracle;
#[cfg(test)]
pub(super) use legacy_wire_oracle::handle as handle_legacy_wire;
