//! Authoritative logical selection; Go projects receipts and performs physical
//! adapter work only between a prepared transition and its terminal receipt.
use super::*;
use crate::reader_pcsc::{LifecycleBinding, Owner};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const LIFECYCLE_PATH: &str = "/api/internal/reader-lifecycle";
mod native;
pub(super) use native::NativeReceipt;
pub(crate) use native::{
    NativeAuthority, NativeReaderFacts, NativeTransition, NativeTransitionKind,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct SelectionSnapshot {
    pub reader: String,
    pub generation: u64,
    pub connected_at_ms: i64,
    pub pending: bool,
}
#[derive(Default)]
pub(super) struct SelectionState {
    snapshot: SelectionSnapshot,
    pub(super) physical: Option<LifecycleBinding>,
    high_water: u64,
    pending: Option<Intent>,
    terminal: VecDeque<Receipt>,
    bootstrap_used: bool,
    native_producer: Option<String>,
    native_pending: Option<native::NativeIntent>,
    native_terminal: VecDeque<native::NativeReceipt>,
    native_high_water: u64,
    native_workers: usize,
    native_unconfirmed: bool,
    native_facts: Option<native::NativeReaderFacts>,
    // Advances only on a new physical metadata publication, under Gate's
    // snapshot lock. Phone enrichment, clearing and receipt replay are not reads.
    physical_metadata_epoch: u64,
}
impl SelectionState {
    pub(super) fn ble_connection(&self) -> Option<crate::reader_ble_apdu::ConnectionBinding> {
        self.native_facts
            .as_ref()
            .and_then(|f| f.ble_connection.clone())
    }
    pub(super) fn physical_matches(&self, binding: &LifecycleBinding) -> bool {
        self.physical.as_ref() == Some(binding) && !self.pending()
    }
    pub(super) fn operation_io(&self, token: &str, reader: &str) -> bool {
        if self.native_pending.as_ref().is_some_and(|p| matches!(&p.authority, native::NativeAuthority::Transaction(CardTransaction::Operation { binding }) if binding.operation_token==token && binding.reader==reader)) {return true;}
        self.pending.as_ref().is_some_and(|p| {
            p.authority.kind == "operation"
                && p.authority.token.as_deref() == Some(token)
                && (p.reader == reader || p.expected_reader == reader)
        })
    }
    pub(super) fn retire_operation(&mut self, token: &str) -> Result<(), &'static str> {
        if self.native_pending.as_ref().is_some_and(|p|matches!(&p.authority,NativeAuthority::Transaction(CardTransaction::Operation{binding}) if binding.operation_token==token)) {
            if self.native_workers!=0 || !self.native_unconfirmed {return Err("reader_lifecycle_pending");}
            let intent=self.native_pending.take().unwrap();
            self.snapshot=SelectionSnapshot{generation:self.snapshot.generation.checked_add(1).ok_or("generation_exhausted")?,..Default::default()};
            self.physical=None;self.native_facts=None;self.native_unconfirmed=false;
            self.native_terminal.push_back(native::NativeReceipt{intent,snapshot:self.snapshot.clone(),facts:NativeReaderFacts::default(),physical:None,aborted:true});
            while self.native_terminal.len()>64 {self.native_terminal.pop_front();}
            return Ok(());
        }
        let Some(intent) = self
            .pending
            .as_ref()
            .filter(|p| {
                p.authority.kind == "operation" && p.authority.token.as_deref() == Some(token)
            })
            .cloned()
        else {
            return Ok(());
        };
        let generation = self
            .snapshot
            .generation
            .checked_add(1)
            .ok_or("generation_exhausted")?;
        self.snapshot = SelectionSnapshot {
            generation,
            ..Default::default()
        };
        self.physical = None;
        finish(self, intent, "retired").map(|_| ())
    }
    pub(super) fn confirms(&self, reader: &str, generation: u64) -> bool {
        !self.pending()
            && !reader.is_empty()
            && self.snapshot.reader == reader
            && self.snapshot.generation == generation
            && generation != 0
    }
    pub(super) fn pending(&self) -> bool {
        self.pending.is_some() || self.native_pending.is_some()
    }
    pub(super) fn native_owned(&self) -> bool {
        self.native_producer.is_some()
    }
    pub(super) fn borrowed_business(&self, token: &str, sequence: u64) -> bool {
        if self.native_pending.as_ref().is_some_and(|p|match &p.authority {
            NativeAuthority::Transaction(CardTransaction::Native{binding,..})=>binding.lease_id==token && binding.lease_sequence==sequence,
            NativeAuthority::Transaction(CardTransaction::Operation{binding})=>matches!(&binding.authority,super::operation::Authority::Business{token:t,sequence:s,..} if t==token && *s==sequence),_=>false,
        }) {return true;}
        self.pending.as_ref().is_some_and(|p| {
            p.authority.kind == "business"
                && p.authority.token.as_deref() == Some(token)
                && p.authority.sequence == Some(sequence)
        })
    }
    pub(super) fn borrowed_operation(&self, token: &str) -> bool {
        if self.native_pending.as_ref().is_some_and(|p|matches!(&p.authority,NativeAuthority::Transaction(CardTransaction::Operation{binding}) if binding.operation_token==token)) {return true;}
        self.pending.as_ref().is_some_and(|p| {
            p.authority.kind == "operation" && p.authority.token.as_deref() == Some(token)
        })
    }
    pub(super) fn retire(&mut self) {
        let generation = self.snapshot.generation.saturating_add(1);
        *self = Self {
            snapshot: SelectionSnapshot {
                generation,
                ..Default::default()
            },
            ..Default::default()
        };
    }
    pub(super) fn observed(&self) -> SelectionSnapshot {
        SelectionSnapshot {
            pending: self.pending(),
            ..self.snapshot.clone()
        }
    }
}
#[cfg(test)]
pub(crate) fn clear_test_native_receipts(gate: &Gate) {
    gate.0.lock().unwrap().selection.native_terminal.clear();
}
#[cfg(test)]
pub(crate) fn seed_test_selection(gate: &Gate, reader: &str, generation: u64) {
    gate.0.lock().unwrap().selection.snapshot = SelectionSnapshot {
        reader: reader.into(),
        generation,
        ..Default::default()
    };
}

#[cfg(test)]
pub(crate) fn seed_test_ble_selection(
    gate: &Gate,
    reader: &str,
    generation: u64,
    connection: Option<crate::reader_ble_apdu::ConnectionBinding>,
) {
    let mut state = gate.0.lock().unwrap();
    state.selection.snapshot = SelectionSnapshot {
        reader: reader.into(),
        generation,
        ..Default::default()
    };
    state.selection.native_facts = Some(native::NativeReaderFacts {
        ble_connection: connection,
        ..Default::default()
    });
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Authority {
    kind: String,
    token: Option<String>,
    sequence: Option<u64>,
    role: Option<String>,
    reader: Option<String>,
    reader_generation: Option<u64>,
    session_id: Option<String>,
    session_sequence: Option<u64>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    action: String,
    producer_id: String,
    token: Option<String>,
    sequence: Option<u64>,
    kind: Option<String>,
    expected_reader: Option<String>,
    expected_generation: Option<u64>,
    reader: Option<String>,
    authority: Option<Authority>,
    previous_iccid: Option<String>,
    current_iccid: Option<String>,
    metadata: Option<serde_json::Value>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Intent {
    producer: String,
    token: String,
    sequence: u64,
    kind: String,
    expected_reader: String,
    expected_generation: u64,
    reader: String,
    authority: Authority,
    previous_iccid: Option<String>,
    current_iccid: Option<String>,
    metadata: Option<serde_json::Value>,
}
#[derive(Clone)]
struct Receipt {
    intent: Intent,
    phase: &'static str,
    snapshot: SelectionSnapshot,
}
fn valid_reader(reader: &str, empty: bool) -> bool {
    (empty && reader.is_empty() || !reader.trim().is_empty())
        && reader.len() <= 512
        && !reader.contains('\0')
        && (!reader.starts_with("BLE:") || crate::reader_ble_apdu::valid_ble_reader(reader))
}
impl Command {
    fn intent(&self) -> Result<Intent, &'static str> {
        let i = Intent {
            producer: self.producer_id.clone(),
            token: self
                .token
                .clone()
                .filter(|t| valid_operation_token(t))
                .ok_or("invalid_request")?,
            sequence: self.sequence.filter(|s| *s != 0).ok_or("invalid_request")?,
            kind: self.kind.clone().ok_or("invalid_request")?,
            expected_reader: self.expected_reader.clone().ok_or("invalid_request")?,
            expected_generation: self.expected_generation.ok_or("invalid_request")?,
            reader: self.reader.clone().ok_or("invalid_request")?,
            authority: self.authority.clone().ok_or("invalid_request")?,
            previous_iccid: self.previous_iccid.clone(),
            current_iccid: self.current_iccid.clone(),
            metadata: self.metadata.clone(),
        };
        if !valid_reader(&i.expected_reader, true)
            || !valid_reader(&i.reader, false)
            || (!i.expected_reader.is_empty() && i.expected_generation == 0)
        {
            return Err("invalid_request");
        }
        match i.kind.as_str() {
            "connect" if i.expected_reader.is_empty() => (),
            "disconnect" | "rebuild" | "identity"
                if !i.expected_reader.is_empty() && i.reader == i.expected_reader =>
            {
                ()
            }
            _ => return Err("invalid_request"),
        }
        if i.kind != "identity" && (i.previous_iccid.is_some() || i.current_iccid.is_some()) {
            return Err("invalid_request");
        }
        if i.kind == "identity" {
            let metadata = i.metadata.as_ref().ok_or("invalid_reader_metadata")?;
            let fields = metadata.as_object().ok_or("invalid_reader_metadata")?;
            let iccid = fields
                .get("iccid")
                .and_then(serde_json::Value::as_str)
                .ok_or("invalid_reader_metadata")?;
            let current = i
                .current_iccid
                .as_deref()
                .ok_or("invalid_reader_metadata")?;
            if !(10..=20).contains(&iccid.len())
                || !iccid.bytes().all(|value| value.is_ascii_digit())
                || iccid != current
                || serde_json::to_vec(metadata)
                    .map_err(|_| "invalid_reader_metadata")?
                    .len()
                    > 16 * 1024
            {
                return Err("invalid_reader_metadata");
            }
        } else if i.metadata.is_some() {
            return Err("invalid_reader_metadata");
        }
        if [&i.previous_iccid, &i.current_iccid].iter().any(|v| {
            v.as_ref()
                .is_some_and(|s| s.len() > 128 || s.contains('\0'))
        }) {
            return Err("invalid_request");
        }
        let a = &i.authority;
        let no_business = a.token.is_none() && a.sequence.is_none() && a.role.is_none();
        let no_binding = a.reader.is_none() && a.reader_generation.is_none();
        let no_session = a.session_id.is_none() && a.session_sequence.is_none();
        match a.kind.as_str() {
            "idle" if no_business && no_binding && no_session => (),
            "business"
                if a.token.as_deref().is_some_and(valid_operation_token)
                    && a.sequence.is_some_and(|s| s != 0)
                    && matches!(a.role.as_deref(), Some("operation" | "native_session"))
                    && no_binding
                    && no_session =>
            {
                ()
            }
            "operation"
                if a.token.as_deref().is_some_and(valid_operation_token)
                    && a.sequence.is_none()
                    && a.role.is_none()
                    && a.reader.as_deref().unwrap_or_default() == i.expected_reader
                    && a.reader_generation.unwrap_or_default() == i.expected_generation
                    && no_session =>
            {
                ()
            }
            "physical_loss"
                if i.kind == "disconnect"
                    && no_business
                    && a.reader.as_deref() == Some(i.expected_reader.as_str())
                    && a.reader_generation == Some(i.expected_generation)
                    && a.session_id.as_deref().is_some_and(valid_operation_token)
                    && a.session_sequence.is_some_and(|s| s != 0)
                    && !i.reader.starts_with("BLE:") =>
            {
                ()
            }
            _ => return Err("invalid_request"),
        }
        Ok(i)
    }
    fn is_plain_snapshot(&self) -> bool {
        self.token.is_none()
            && self.sequence.is_none()
            && self.kind.is_none()
            && self.expected_reader.is_none()
            && self.expected_generation.is_none()
            && self.reader.is_none()
            && self.authority.is_none()
            && self.previous_iccid.is_none()
            && self.current_iccid.is_none()
            && self.metadata.is_none()
    }
}
impl Intent {
    fn loss_binding(&self) -> LifecycleBinding {
        LifecycleBinding {
            producer: self.producer.clone(),
            reader: self.reader.clone(),
            session_id: self.authority.session_id.clone().unwrap_or_default(),
            session_sequence: self.authority.session_sequence.unwrap_or_default(),
        }
    }
}
fn receipt(intent: &Intent, phase: &str, snapshot: &SelectionSnapshot) -> Value {
    json!({"ok":true,"token":intent.token,"sequence":intent.sequence,"phase":phase,"snapshot":snapshot})
}
impl Gate {
    #[cfg(test)]
    pub(crate) fn test_lifecycle_command(&self, value: Value) -> Result<Value, &'static str> {
        let command = serde_json::from_value(value).map_err(|_| "invalid_request")?;
        self.lifecycle_command(&command, false, None)
    }

    /// Only the retained native coordinator calls this after the exact Go
    /// finalizer callback has ended and actual BLE/PCSC work is proven idle.
    /// A lost commit reply may already have committed; leave that receipt
    /// intact. An unresolved prepare is retired without claiming rollback or
    /// successful card identity: invalidate selection and advance generation.
    /// A late commit can only observe the immutable retired receipt. If the
    /// finalizer never prepared, an unchanged selection is still stale: the
    /// EnableProfile command already ran before this finalizer was requested.
    pub(crate) fn retire_native_finalizer_selection(
        &self,
        binding: &NativePinBinding,
    ) -> Result<bool, &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !state.native.finalizing(binding) {
            return Err("owner_mismatch");
        }
        if !super::native::physical_work_idle(&state, binding) {
            return Err("card_busy");
        }
        if state.selection.native_pending.as_ref().is_some_and(|p|matches!(&p.authority,NativeAuthority::Transaction(CardTransaction::Native{binding:b,..}) if b==binding)) {
            if state.selection.native_workers!=0 {return Err("reader_physical_work_pending");}
            let intent=state.selection.native_pending.take().unwrap();
            state.selection.snapshot=SelectionSnapshot{generation:state.selection.snapshot.generation.checked_add(1).ok_or("generation_exhausted")?,..Default::default()};
            state.selection.physical=None;state.selection.native_facts=None;state.selection.native_unconfirmed=false;state.reader_metadata=None;
            let snapshot=state.selection.snapshot.clone();
            state.selection.native_terminal.push_back(native::NativeReceipt{intent,snapshot,facts:NativeReaderFacts::default(),physical:None,aborted:true});
            while state.selection.native_terminal.len()>64 {state.selection.native_terminal.pop_front();}
            return Ok(true);
        }
        let Some(intent) = state.selection.pending.clone() else {
            if !state
                .selection
                .confirms(&binding.reader, binding.reader_generation)
            {
                return Ok(false);
            }
            let generation = state
                .selection
                .snapshot
                .generation
                .checked_add(1)
                .ok_or("generation_exhausted")?;
            state.selection.snapshot = SelectionSnapshot {
                generation,
                ..Default::default()
            };
            state.selection.physical = None;
            state.reader_metadata = None;
            return Ok(true);
        };
        if intent.producer != binding.producer_id
            || intent.expected_reader != binding.reader
            || intent.authority.kind != "business"
            || intent.authority.role.as_deref() != Some("native_session")
            || intent.authority.token.as_deref() != Some(binding.lease_id.as_str())
            || intent.authority.sequence != Some(binding.lease_sequence)
        {
            return Err("owner_mismatch");
        }
        let generation = state
            .selection
            .snapshot
            .generation
            .checked_add(1)
            .ok_or("generation_exhausted")?;
        state.selection.snapshot = SelectionSnapshot {
            generation,
            ..Default::default()
        };
        state.selection.physical = None;
        state.reader_metadata = None;
        finish(&mut state.selection, intent, "retired")?;
        Ok(true)
    }

    pub(crate) fn selection_snapshot(&self) -> SelectionSnapshot {
        match self.0.lock() {
            Ok(s) => s.selection.observed(),
            Err(_) => SelectionSnapshot {
                pending: true,
                ..Default::default()
            },
        }
    }
    pub(crate) fn selected_reader_generation(&self) -> Option<(String, u64)> {
        let s = self.selection_snapshot();
        (!s.pending && !s.reader.is_empty() && s.generation != 0)
            .then_some((s.reader, s.generation))
    }

    /// Publish a completed observation only while its selection is current.
    /// Callers run bounded synchronous persistence on a blocking worker. The
    /// callback must not re-enter this gate or start physical card operations.
    /// Keeping the lock through publication closes the check-then-write race
    /// with a disconnect, new card, or identity transition after lease release.
    pub(crate) fn with_selected_generation<T>(
        &self,
        reader: &str,
        generation: u64,
        publish: impl FnOnce() -> T,
    ) -> Result<T, &'static str> {
        let state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !state.selection.confirms(reader, generation) || state.operations.quiescing {
            return Err("reader_generation_changed");
        }
        Ok(publish())
    }

    pub(crate) fn selected_reader(&self) -> Option<String> {
        self.selected_reader_generation().map(|(r, _)| r)
    }

    /// Read-only fast path must precede physical proof lookup: a confirmed
    /// disconnect remains replayable after its PC/SC handoff has been ACKed.
    fn replay(&self, command: &Command) -> Result<Option<Value>, &'static str> {
        let state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        check_producer(&state, command)?;
        if command.action == "snapshot" {
            return if command.is_plain_snapshot() {
                Ok(Some(
                    json!({"ok":true,"snapshot":state.selection.observed()}),
                ))
            } else {
                Err("invalid_request")
            };
        }
        let intent = command.intent()?;
        replay_locked(&state.selection, &intent, &command.action)
    }
    fn lifecycle_command(
        &self,
        command: &Command,
        stopping: bool,
        physical: Option<&LifecycleBinding>,
    ) -> Result<Value, &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.selection.native_owned() && command.action != "snapshot" {
            return Err("native_reader_lifecycle_owned");
        }
        check_producer(&state, command)?;
        let intent = command.intent()?;
        if !matches!(command.action.as_str(), "prepare" | "commit" | "abort") {
            return Err("invalid_request");
        }
        if let Some(value) = replay_locked(&state.selection, &intent, &command.action)? {
            return Ok(value);
        }
        match command.action.as_str() {
            "prepare" => {
                if stopping || state.operations.quiescing {
                    return Err("backend_stopping");
                }
                if intent.sequence <= state.selection.high_water {
                    return Err("transition_expired");
                }
                state.selection.high_water = intent.sequence;
                if state.selection.pending() {
                    return Err("card_busy");
                }
                let snapshot = &state.selection.snapshot;
                let bootstrap = !state.selection.bootstrap_used
                    && snapshot.reader.is_empty()
                    && intent.expected_reader.is_empty()
                    && intent.expected_generation == 0;
                if snapshot.reader != intent.expected_reader
                    || (!bootstrap && snapshot.generation != intent.expected_generation)
                {
                    return Err("stale_reader_generation");
                }
                if snapshot.generation == u64::MAX {
                    return Err("generation_exhausted");
                }
                authorize(&state, &intent, physical)?;
                state.selection.bootstrap_used = true;
                state.selection.pending = Some(intent.clone());
                Ok(receipt(&intent, "prepared", &state.selection.observed()))
            }
            "commit" => {
                if state.selection.pending.as_ref() != Some(&intent) {
                    return Err("transition_mismatch");
                }
                if !state.ble_io_workers.is_empty() {
                    return Err("card_busy");
                }
                if intent.kind != "disconnect" && !intent.reader.starts_with("BLE:") {
                    let binding = physical.ok_or("physical_binding_unconfirmed")?;
                    if binding.producer != intent.producer || binding.reader != intent.reader {
                        return Err("physical_binding_unconfirmed");
                    }
                    if intent.kind != "connect"
                        && state.selection.physical.as_ref() != Some(binding)
                    {
                        return Err("stale_physical_binding");
                    }
                }
                if intent.authority.kind == "physical_loss"
                    && physical != state.selection.physical.as_ref()
                {
                    return Err("stale_physical_binding");
                }
                authorize(&state, &intent, physical)?;
                let changed_identity = intent
                    .previous_iccid
                    .as_deref()
                    .zip(intent.current_iccid.as_deref())
                    .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty() && a != b);
                let generation = if intent.kind != "identity" || changed_identity {
                    state
                        .selection
                        .snapshot
                        .generation
                        .checked_add(1)
                        .ok_or("generation_exhausted")?
                } else {
                    state.selection.snapshot.generation
                };
                let connected_at = if intent.kind == "connect" {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|_| "clock_unavailable")?
                        .as_millis()
                        .try_into()
                        .map_err(|_| "clock_unavailable")?
                } else {
                    state.selection.snapshot.connected_at_ms
                };
                state.selection.snapshot.generation = generation;
                state.reader_metadata = intent
                    .metadata
                    .as_ref()
                    .map(|metadata| (intent.reader.clone(), generation, metadata.clone()));
                match intent.kind.as_str() {
                    "connect" => {
                        state.selection.snapshot.reader = intent.reader.clone();
                        state.selection.snapshot.connected_at_ms = connected_at;
                        state.selection.physical = physical.cloned();
                    }
                    "disconnect" => {
                        state.selection.snapshot.reader.clear();
                        state.selection.snapshot.connected_at_ms = 0;
                        state.selection.physical = None;
                    }
                    _ => (),
                }
                if intent.authority.kind == "operation"
                    && state
                        .operations
                        .contains(intent.authority.token.as_deref().unwrap_or_default())
                {
                    let token = intent.authority.token.as_deref().unwrap_or_default();
                    let snapshot = state.selection.observed();
                    state.operations.advance(token, &snapshot);
                    if let Some(scope) = state.operation_workers.get_mut(token) {
                        *scope = (snapshot.reader, snapshot.generation);
                    }
                }
                finish(&mut state.selection, intent, "committed")
            }
            "abort" => {
                if state
                    .selection
                    .pending
                    .as_ref()
                    .is_some_and(|p| p != &intent)
                {
                    return Err("transition_mismatch");
                }
                // An abort arriving before its delayed prepare is a terminal
                // fence, not permission for that prepare to execute later.
                if state.selection.pending.is_none()
                    && intent.sequence <= state.selection.high_water
                {
                    return Err("transition_expired");
                }
                state.selection.high_water = state.selection.high_water.max(intent.sequence);
                finish(&mut state.selection, intent, "aborted")
            }
            _ => unreachable!(),
        }
    }
}
fn check_producer(state: &State, command: &Command) -> Result<(), &'static str> {
    if command.producer_id.is_empty() || command.producer_id.len() > 128 {
        return Err("invalid_request");
    }
    if !state.business.is_producer(&command.producer_id) {
        return Err("stale_producer");
    }
    Ok(())
}
fn replay_locked(
    selection: &SelectionState,
    intent: &Intent,
    action: &str,
) -> Result<Option<Value>, &'static str> {
    if let Some(p) = selection.pending.as_ref() {
        if p.token == intent.token || p.sequence == intent.sequence {
            if p != intent {
                return Err("transition_mismatch");
            }
            if action == "prepare" {
                return Ok(Some(receipt(p, "prepared", &selection.observed())));
            }
        }
    }
    if let Some(r) = selection
        .terminal
        .iter()
        .find(|r| r.intent.token == intent.token || r.intent.sequence == intent.sequence)
    {
        if &r.intent != intent {
            return Err("transition_mismatch");
        }
        if action != "prepare"
            && !matches!(
                (action, r.phase),
                ("commit", "committed") | ("abort", "aborted")
            )
        {
            return Err("transition_finished");
        }
        // A historical receipt is never permission to publish an older card
        // over a replacement or to hide another transition's pending state.
        if selection.pending() || selection.snapshot != r.snapshot {
            return Err("transition_superseded");
        }
        return Ok(Some(receipt(intent, r.phase, &r.snapshot)));
    }
    Ok(None)
}
fn finish(
    selection: &mut SelectionState,
    intent: Intent,
    phase: &'static str,
) -> Result<Value, &'static str> {
    selection.pending = None;
    let snapshot = selection.observed();
    let result = receipt(&intent, phase, &snapshot);
    selection.terminal.push_back(Receipt {
        intent,
        phase,
        snapshot,
    });
    while selection.terminal.len() > 64 {
        selection.terminal.pop_front();
    }
    Ok(result)
}
fn authorize(
    state: &State,
    intent: &Intent,
    physical: Option<&LifecycleBinding>,
) -> Result<(), &'static str> {
    if !state.ble_io_workers.is_empty() {
        return Err("card_busy");
    }
    let a = &intent.authority;
    match a.kind.as_str() {
        "idle"
            if state.business.owner.is_none()
                && !state.native.active()
                && state.owner.is_none()
                && state.workers.is_empty()
                && state.operation_workers.is_empty()
                && state.ble_control_owner.is_none() =>
        {
            Ok(())
        }
        "business"
            if state.native.matches_lifecycle(
                &intent.producer,
                &intent.expected_reader,
                a.token.as_deref().unwrap_or_default(),
                a.sequence.unwrap_or_default(),
                a.role.as_deref().unwrap_or_default(),
            ) && state.ble_control_owner.is_none()
                && state.owner.is_none()
                && state.business.owner.is_none()
                && state.workers.is_empty()
                && state.operation_workers.is_empty() =>
        {
            Ok(())
        }
        "business"
            if state.business.matches_lifecycle(
                a.token.as_deref().unwrap_or_default(),
                a.sequence.unwrap_or_default(),
                a.role.as_deref().unwrap_or_default(),
            ) && !state.native.active()
                && state.ble_control_owner.is_none()
                && state.owner.is_none()
                && state.operation_workers.is_empty()
                && match a.role.as_deref() {
                    Some("operation") => state.workers.is_empty(),
                    Some("native_session") => {
                        !intent.expected_reader.is_empty()
                            && state.workers.keys().all(|r| r == &intent.expected_reader)
                    }
                    _ => false,
                } =>
        {
            Ok(())
        }
        "operation"
            if state
                .operation_workers
                .get(a.token.as_deref().unwrap_or_default())
                == Some(&(intent.expected_reader.clone(), intent.expected_generation))
                && (state.operations.lifecycle_allowed(
                    a.token.as_deref().unwrap_or_default(),
                    &intent.kind,
                    &intent.reader,
                ) || cfg!(test)
                    && !state
                        .operations
                        .contains(a.token.as_deref().unwrap_or_default()))
                && state.ble_control_owner.is_none()
                && !state.native.active()
                && !state.business.native_active() =>
        {
            Ok(())
        }
        "physical_loss"
            if physical == Some(&intent.loss_binding())
                && physical == state.selection.physical.as_ref() =>
        {
            Ok(())
        }
        _ => Err("card_busy"),
    }
}

pub(crate) async fn handle_lifecycle(
    request: Request<Incoming>,
    stopping: bool,
    owner: &Owner,
) -> Response<Full<Bytes>> {
    if request.method() != hyper::Method::POST || request.uri().path() != LIFECYCLE_PATH {
        return reject("route_not_found", StatusCode::NOT_FOUND);
    }
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        32 * 1024,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(v) => v,
        _ => return reject("invalid_request", StatusCode::BAD_REQUEST),
    };
    let command: Command = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(_) => return reject("invalid_request", StatusCode::BAD_REQUEST),
    };
    let gate = gate();
    let result = (|| {
        if let Some(replayed) = gate.replay(&command)? {
            return Ok(replayed);
        }
        let intent = command.intent()?;
        if intent.authority.kind == "physical_loss"
            && matches!(command.action.as_str(), "prepare" | "commit")
        {
            let binding = intent.loss_binding();
            return owner
                .with_confirmed_presence_loss(&binding, || {
                    gate.lifecycle_command(&command, stopping, Some(&binding))
                })
                .map_err(|_| "physical_loss_unconfirmed")?;
        }
        if command.action == "commit"
            && intent.kind != "disconnect"
            && !intent.reader.starts_with("BLE:")
        {
            return owner
                .with_live_lifecycle_binding(&intent.producer, &intent.reader, |binding| {
                    gate.lifecycle_command(&command, stopping, Some(binding))
                })
                .map_err(|_| "physical_binding_unconfirmed")?;
        }
        gate.lifecycle_command(&command, stopping, None)
    })();
    match result {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(code) => reject(
            code,
            match code {
                "invalid_request" => StatusCode::BAD_REQUEST,
                "backend_stopping"
                | "reader_gate_failed"
                | "generation_exhausted"
                | "clock_unavailable"
                | "physical_binding_unconfirmed"
                | "physical_loss_unconfirmed" => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::CONFLICT,
            },
        ),
    }
}
#[cfg(test)]
mod tests;
