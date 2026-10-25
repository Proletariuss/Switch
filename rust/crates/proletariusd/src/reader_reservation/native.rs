//! Rust owns native lease identity and admission independently of the Go
//! compatibility process. The coordinator retains the binding through cancellation and
//! completes physical cleanup before calling finish_native_pin.
use super::*;
use serde::{Deserialize, Serialize};

pub(crate) const NATIVE_OWNER_PATH: &str = "/api/internal/reader-native-owner";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativePinPolicy {
    Identity,
    CardOnly,
    Mutation,
}

// Deliberately no Debug: the opaque lease capability must not enter logs.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativePinBinding {
    pub(crate) lease_id: String,
    pub(crate) lease_sequence: u64,
    pub(crate) producer_id: String,
    pub(crate) reader: String,
    pub(crate) reader_generation: u64,
    pub(crate) policy: NativePinPolicy,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct NativePreAdmission {
    token: String,
    pub(crate) producer: String,
    pub(crate) reader: String,
    pub(crate) generation: u64,
    pub(crate) policy: NativePinPolicy,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Preparing,
    Active,
    Finalizing,
}

struct NativeEntry {
    binding: NativePinBinding,
    phase: Phase,
    draining: bool,
    workers: usize,
    // The immutable initial binding remains the pin/unpin receipt key. Only
    // the exact authorized finalizer may advance its effective generation.
    generation: u64,
}

#[derive(Default)]
pub(super) struct NativeState {
    sequence: u64,
    owner: Option<NativeEntry>,
    pre: Option<NativePreAdmission>,
}

impl NativeState {
    pub(super) fn pre_matches(&self, expected: &NativePreAdmission) -> bool {
        self.pre.as_ref() == Some(expected)
    }
    pub(super) fn preparation_active(&self) -> bool {
        self.pre.is_some()
    }
    pub(super) fn owner_active(&self) -> bool {
        self.owner.is_some()
    }
    pub(super) fn active(&self) -> bool {
        self.owner.is_some() || self.pre.is_some()
    }
    pub(super) fn retire_producer(&mut self) {
        if let Some(owner) = &mut self.owner {
            owner.draining = true;
        }
    }
    pub(super) fn finalizing(&self, binding: &NativePinBinding) -> bool {
        self.matching(binding)
            .is_ok_and(|owner| owner.phase == Phase::Finalizing)
    }
    pub(super) fn matches_lifecycle(
        &self,
        producer: &str,
        reader: &str,
        token: &str,
        sequence: u64,
        role: &str,
    ) -> bool {
        self.owner.as_ref().is_some_and(|owner| {
            owner.phase == Phase::Finalizing
                && owner.binding.producer_id == producer
                && owner.binding.reader == reader
                && owner.binding.lease_id == token
                && owner.binding.lease_sequence == sequence
                && role == "native_session"
        })
    }
    pub(super) fn lifecycle_authorized(&self, binding: &NativePinBinding) -> bool {
        self.matching(binding).is_ok_and(|owner| {
            (owner.phase == Phase::Finalizing)
                || (!owner.draining
                    && owner.phase == Phase::Active
                    && binding.policy != NativePinPolicy::Identity)
        })
    }
    pub(super) fn validate_adapter_io(
        &self,
        binding: &NativePinBinding,
        purpose: &str,
        sequence: Option<u64>,
    ) -> Result<(), &'static str> {
        let owner = self.matching(binding)?;
        match purpose {
            "pin" if owner.phase == Phase::Preparing && !owner.draining && sequence.is_none() => {
                Ok(())
            }
            "finalize" if owner.phase == Phase::Finalizing && sequence == Some(1) => Ok(()),
            _ => Err("owner_expired"),
        }
    }
    pub(super) fn start_adapter_io(
        &mut self,
        binding: &NativePinBinding,
    ) -> Result<(), &'static str> {
        let owner = self.matching_mut(binding)?;
        if owner.workers != 0 {
            return Err("card_busy");
        }
        owner.workers = 1;
        Ok(())
    }
    pub(super) fn finish_adapter_io(&mut self, binding: &NativePinBinding) {
        if let Ok(owner) = self.matching_mut(binding) {
            owner.workers = owner.workers.saturating_sub(1);
        }
    }
    fn matching(&self, binding: &NativePinBinding) -> Result<&NativeEntry, &'static str> {
        self.owner
            .as_ref()
            .filter(|owner| &owner.binding == binding)
            .ok_or("owner_mismatch")
    }
    fn matching_mut(
        &mut self,
        binding: &NativePinBinding,
    ) -> Result<&mut NativeEntry, &'static str> {
        self.owner
            .as_mut()
            .filter(|owner| &owner.binding == binding)
            .ok_or("owner_mismatch")
    }
}

pub(crate) struct NativeWorkerPermit(Arc<Gate>, NativePinBinding);
impl Drop for NativeWorkerPermit {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(owner) = state.native.matching_mut(&self.1) {
            owner.workers = owner.workers.saturating_sub(1);
        }
    }
}

fn work_idle(state: &State, binding: &NativePinBinding) -> bool {
    physical_work_idle(state, binding)
        && !state
            .selection
            .borrowed_business(&binding.lease_id, binding.lease_sequence)
}

pub(super) fn physical_work_idle(state: &State, binding: &NativePinBinding) -> bool {
    state.native.matching(binding).is_ok_and(|e| e.workers == 0)
        && !state.ble_io_workers.contains_key(&binding.reader)
        && !state.workers.contains_key(&binding.reader)
        && !state
            .operation_workers
            .values()
            .any(|(r, _)| r == &binding.reader)
}

pub(super) fn native_producer_current(state: &State, producer: &str) -> bool {
    state.native_reader_producer.as_deref().map_or_else(
        || state.business.is_producer(producer),
        |native| native == producer,
    )
}

impl Gate {
    /// One retained preparation across all callers. Mutation may ask an
    /// existing IMS identity owner to stop, but cannot steal its physical lease.
    pub(crate) fn begin_native_pre_admission(
        &self,
        reader: &str,
        policy: NativePinPolicy,
    ) -> Result<NativePreAdmission, &'static str> {
        let token = crate::multistandby_store::random_token().map_err(|_| "entropy_unavailable")?;
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.native.pre.is_some()
            || state.operations.quiescing
            || state.selection.pending()
            || state.ble_control_owner.is_some()
        {
            return Err("card_busy");
        }
        if policy != NativePinPolicy::Mutation
            && (state.native.owner.is_some()
                || state.business.owner.is_some()
                || state.owner.is_some()
                || state.operations.active()
                || !state.workers.is_empty()
                || !state.ble_io_workers.is_empty()
                || !state.operation_workers.is_empty())
        {
            return Err("card_busy");
        }
        if policy == NativePinPolicy::Mutation
            && (state.owner.is_some()
                || state.operations.active()
                || !state.operation_workers.is_empty()
                || state.business.owner.is_some() && !state.business.native_active())
        {
            return Err("card_busy");
        }
        if policy == NativePinPolicy::Mutation
            && state.native.owner.as_ref().is_some_and(|owner| {
                owner.binding.policy != NativePinPolicy::Identity || owner.phase != Phase::Active
            })
        {
            return Err("card_busy");
        }
        let selection = state.selection.observed();
        if !state.selection.confirms(reader, selection.generation) {
            return Err("stale_reader_generation");
        }
        let producer = state
            .native_reader_producer
            .as_deref()
            .or_else(|| state.business.producer())
            .ok_or("stale_producer")?
            .to_owned();
        let pre = NativePreAdmission {
            token,
            producer,
            reader: reader.to_owned(),
            generation: selection.generation,
            policy,
        };
        state.native.pre = Some(pre.clone());
        Ok(pre)
    }
    pub(crate) fn finish_native_pre_admission(
        &self,
        pre: &NativePreAdmission,
    ) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        match &state.native.pre {
            Some(current) if current == pre => {
                state.native.pre = None;
                Ok(())
            }
            None => Ok(()),
            _ => Err("owner_mismatch"),
        }
    }
    pub(crate) fn begin_native_pin_from_pre(
        &self,
        pre: &NativePreAdmission,
    ) -> Result<NativePinBinding, &'static str> {
        self.begin_native_pin_inner(&pre.reader, pre.policy, Some(pre))
    }
    /// Allocate before the first physical acquisition. Producer, selection and native
    /// admission are sampled atomically; no Go-issued capability is borrowed.
    pub(crate) fn begin_native_pin(
        &self,
        reader: &str,
        policy: NativePinPolicy,
    ) -> Result<NativePinBinding, &'static str> {
        self.begin_native_pin_inner(reader, policy, None)
    }
    fn begin_native_pin_inner(
        &self,
        reader: &str,
        policy: NativePinPolicy,
        pre: Option<&NativePreAdmission>,
    ) -> Result<NativePinBinding, &'static str> {
        if reader.trim().is_empty()
            || reader.trim() != reader
            || reader.len() > 512
            || reader.contains('\0')
            || (reader.starts_with("BLE:") && !crate::reader_ble_apdu::valid_ble_reader(reader))
        {
            return Err("invalid_reader_binding");
        }
        let token = crate::multistandby_store::random_token().map_err(|_| "entropy_unavailable")?;
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.native.pre.as_ref() != pre
            || state.operations.quiescing
            || state.operations.active()
            || state.native.owner.is_some()
            || state.business.owner.is_some()
            || state.owner.is_some()
            || state.ble_control_owner.is_some()
            || !state.workers.is_empty()
            || !state.ble_io_workers.is_empty()
            || !state.operation_workers.is_empty()
            || state.selection.pending()
        {
            return Err("card_busy");
        }
        let selection = state.selection.observed();
        if !state.selection.confirms(reader, selection.generation)
            || pre.is_some_and(|pre| {
                pre.generation != selection.generation
                    || !native_producer_current(&state, &pre.producer)
            })
        {
            return Err("stale_reader_generation");
        }
        let producer = state
            .native_reader_producer
            .as_deref()
            .or_else(|| state.business.producer())
            .ok_or("stale_producer")?
            .to_owned();
        let sequence = state
            .native
            .sequence
            .checked_add(1)
            .ok_or("sequence_exhausted")?;
        let binding = NativePinBinding {
            lease_id: token,
            lease_sequence: sequence,
            producer_id: producer,
            reader: reader.to_owned(),
            reader_generation: selection.generation,
            policy,
        };
        state.native.sequence = sequence;
        state.native.pre = None;
        state.native.owner = Some(NativeEntry {
            binding: binding.clone(),
            phase: Phase::Preparing,
            draining: false,
            workers: 0,
            generation: binding.reader_generation,
        });
        Ok(binding)
    }

    pub(crate) fn activate_native_pin(
        &self,
        binding: &NativePinBinding,
    ) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !native_producer_current(&state, &binding.producer_id) {
            return Err("stale_producer");
        }
        if !state
            .selection
            .confirms(&binding.reader, binding.reader_generation)
        {
            return Err("stale_reader_generation");
        }
        let owner = state.native.matching_mut(binding)?;
        if owner.draining || owner.phase != Phase::Preparing {
            return Err("owner_expired");
        }
        owner.phase = Phase::Active;
        Ok(())
    }

    pub(crate) fn drain_native_pin(&self, binding: &NativePinBinding) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        state.native.matching_mut(binding)?.draining = true;
        Ok(())
    }

    pub(crate) fn begin_native_finalize(
        &self,
        binding: &NativePinBinding,
    ) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !native_producer_current(&state, &binding.producer_id) {
            return Err("stale_producer");
        }
        if !work_idle(&state, binding) || state.selection.pending() {
            return Err("card_busy");
        }
        let owner = state.native.matching(binding)?;
        if !state.selection.confirms(&binding.reader, owner.generation) {
            return Err("stale_reader_generation");
        }
        let owner = state.native.matching_mut(binding)?;
        if owner.draining || owner.phase != Phase::Active {
            return Err("owner_expired");
        }
        owner.phase = Phase::Finalizing;
        Ok(())
    }

    pub(crate) fn advance_native_pin_generation(
        &self,
        binding: &NativePinBinding,
        generation: u64,
    ) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !native_producer_current(&state, &binding.producer_id)
            || !state.selection.confirms(&binding.reader, generation)
        {
            return Err("stale_reader_generation");
        }
        let owner = state.native.matching_mut(binding)?;
        if owner.phase != Phase::Finalizing || generation <= owner.generation {
            return Err("owner_mismatch");
        }
        owner.generation = generation;
        Ok(())
    }
    /// Called only after the exact finalizer receipt or confirmed adapter
    /// death plus physical retirement. Cancellation alone is not completion.
    pub(crate) fn finish_native_finalize(
        &self,
        binding: &NativePinBinding,
    ) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !work_idle(&state, binding) || state.selection.pending() {
            return Err("card_busy");
        }
        let selection = state.selection.observed();
        let owner = state.native.matching_mut(binding)?;
        if owner.phase != Phase::Finalizing {
            return Err("owner_mismatch");
        }
        if selection.reader != binding.reader {
            owner.draining = true;
        } else {
            owner.generation = selection.generation;
        }
        owner.phase = Phase::Active;
        Ok(())
    }

    pub(crate) fn native_workers_idle(&self, binding: &NativePinBinding) -> bool {
        self.0.lock().is_ok_and(|state| work_idle(&state, binding))
    }

    /// The callback may have returned without a confirmed lifecycle receipt.
    /// Its logical transition must not be mistaken for an active card worker.
    /// The caller still proves PC/SC completion separately before retiring it.
    pub(crate) fn native_physical_workers_idle(&self, binding: &NativePinBinding) -> bool {
        self.0
            .lock()
            .is_ok_and(|state| physical_work_idle(&state, binding))
    }

    pub(crate) fn native_pin_producer_current(&self, binding: &NativePinBinding) -> bool {
        self.0.lock().is_ok_and(|state| {
            state.native.matching(binding).is_ok()
                && native_producer_current(&state, &binding.producer_id)
        })
    }

    pub(crate) fn track_native_worker(
        self: &Arc<Self>,
        binding: &NativePinBinding,
    ) -> Result<NativeWorkerPermit, &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !native_producer_current(&state, &binding.producer_id) {
            return Err("stale_producer");
        }
        let owner = state.native.matching(binding)?;
        if owner.draining || owner.phase != Phase::Active {
            return Err("owner_expired");
        }
        if !state.selection.confirms(&binding.reader, owner.generation) {
            return Err("stale_reader_generation");
        }
        let owner = state.native.matching_mut(binding)?;
        if owner.workers != 0 {
            return Err("card_busy");
        }
        owner.workers = 1;
        Ok(NativeWorkerPermit(self.clone(), binding.clone()))
    }

    /// Retained supplier setup/finalization workers use the same real counter
    /// as ordinary card jobs; cancellation cannot retire it early.
    pub(crate) fn track_native_supplier_worker(
        self: &Arc<Self>,
        binding: &NativePinBinding,
        finalizing: bool,
    ) -> Result<NativeWorkerPermit, &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !native_producer_current(&state, &binding.producer_id) {
            return Err("stale_producer");
        }
        let owner = state.native.matching_mut(binding)?;
        if owner.workers != 0
            || (finalizing && owner.phase != Phase::Finalizing)
            || (!finalizing && (owner.phase != Phase::Preparing || owner.draining))
        {
            return Err("card_busy");
        }
        owner.workers = 1;
        Ok(NativeWorkerPermit(self.clone(), binding.clone()))
    }
    /// The coordinator calls this only after exact physical release proof. This is never exposed as an RPC.
    pub(crate) fn finish_native_pin(&self, binding: &NativePinBinding) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        let owner = state.native.matching(binding)?;
        if !owner.draining || owner.phase == Phase::Finalizing || !work_idle(&state, binding) {
            return Err("card_busy");
        }
        state.native.owner = None;
        Ok(())
    }

    pub(crate) fn validate_native_pin(
        &self,
        binding: &NativePinBinding,
        purpose: &str,
    ) -> Result<(), &'static str> {
        let state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !native_producer_current(&state, &binding.producer_id) {
            return Err("stale_producer");
        }
        let owner = state.native.matching(binding)?;
        match purpose {
            "pin"
                if owner.phase == Phase::Preparing
                    && !owner.draining
                    && state
                        .selection
                        .confirms(&binding.reader, binding.reader_generation) =>
            {
                Ok(())
            }
            "finalize" if owner.phase == Phase::Finalizing => Ok(()),
            // A preparing pin can be tombstoned before its first request is
            // delivered. The Go ledger still guards actual adapter completion.
            "unpin"
                if owner.draining
                    && owner.phase != Phase::Finalizing
                    && work_idle(&state, binding) =>
            {
                Ok(())
            }
            "pin" | "finalize" | "unpin" => Err("card_busy"),
            _ => Err("invalid_request"),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_seed_selection(&self, reader: &str, generation: u64) {
        lifecycle::seed_test_selection(self, reader, generation);
    }

    #[cfg(test)]
    pub(crate) fn test_seed_ble_selection(
        &self,
        reader: &str,
        generation: u64,
        connection: Option<crate::reader_ble_apdu::ConnectionBinding>,
    ) {
        lifecycle::seed_test_ble_selection(self, reader, generation, connection);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidateRequest {
    action: String,
    binding: NativePinBinding,
    purpose: String,
}

pub(crate) async fn handle_native_owner(
    request: Request<Incoming>,
    stopping: bool,
    gate: &Gate,
) -> Response<Full<Bytes>> {
    if request.method() != hyper::Method::POST || request.uri().path() != NATIVE_OWNER_PATH {
        return reject("route_not_found", StatusCode::NOT_FOUND);
    }
    let bytes = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        4096,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(body) => body,
        _ => return reject("invalid_request", StatusCode::BAD_REQUEST),
    };
    let command: ValidateRequest = match serde_json::from_slice(&bytes) {
        Ok(command) => command,
        _ => return reject("invalid_request", StatusCode::BAD_REQUEST),
    };
    if command.action != "validate" {
        return reject("invalid_request", StatusCode::BAD_REQUEST);
    }
    if stopping && command.purpose == "pin" {
        return reject("backend_stopping", StatusCode::SERVICE_UNAVAILABLE);
    }
    match gate.validate_native_pin(&command.binding, &command.purpose) {
        Ok(()) => crate::json_response(StatusCode::OK, &json!({"ok":true})),
        Err(code) => reject(
            code,
            if code == "invalid_request" {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::CONFLICT
            },
        ),
    }
}

#[cfg(test)]
mod tests;
