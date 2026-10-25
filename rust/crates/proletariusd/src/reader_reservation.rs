//! Rust owns business, workflow and physical admission. Go adapters carry
//! exact capabilities; only actual worker completion releases card I/O guards.
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

#[cfg(test)]
pub(crate) const PREFIX: &str = "/api/internal/multistandby/reader-reservation/";
mod business;
mod lifecycle;
mod native;
mod native_multi;
mod operation;
pub(crate) use business::{BUSINESS_PATH, handle_business};
pub(crate) use lifecycle::{
    LIFECYCLE_PATH, NativeAuthority, NativeReaderFacts, NativeTransition, NativeTransitionKind,
    SelectionSnapshot, handle_lifecycle,
};
pub(crate) use native::{
    NATIVE_OWNER_PATH, NativePinBinding, NativePinPolicy, NativePreAdmission, NativeWorkerPermit,
    handle_native_owner,
};
pub(crate) use native_multi::NativeMultiBinding;
pub(crate) use operation::CardTransaction;
#[derive(Default)]
struct State {
    business: business::BusinessState,
    ordinary_admission: Option<Arc<crate::ordinary_vowifi_admission::Gate>>,
    multistandby_maintenance: Option<business::MultistandbyMaintenance>,
    legacy_business_guard: Option<business::LegacyBusinessGuard>,
    operations: operation::OperationState,
    selection: lifecycle::SelectionState,
    reader_metadata: Option<(String, u64, Value)>,
    // Rust-owned native leases survive a Go producer replacement. They are
    // retired only by the retained coordinator after physical cleanup proof.
    native: native::NativeState,
    native_multi: native_multi::NativeMultiState,
    native_reader_producer: Option<String>,
    native_session_supplier:
        Option<std::sync::Weak<dyn crate::reader_session_lease::NativeSessionSupplier>>,
    workers: HashMap<String, usize>,
    // Actual bridge jobs survive the Go process that admitted their caller.
    // This counter is separate from Go capabilities and never retired by TTL.
    ble_io_workers: HashMap<String, usize>,
    operation_workers: HashMap<String, (String, u64)>,
    #[cfg(test)]
    released_operations: HashMap<String, (String, u64)>,
    #[cfg(test)]
    released_operation_order: VecDeque<String>,
    owner: Option<String>,
    last_released: Option<String>,
    owner_reader: Option<String>,
    owner_generation: Option<u64>,
    ble_control_owner: Option<String>,
    last_released_ble_control: Option<String>,
}
#[derive(Default)]
pub(crate) struct Gate(Mutex<State>);
pub(crate) struct WorkerPermit(Arc<Gate>, String);
pub(crate) struct BleIoWorkerPermit(Arc<Gate>, String);
impl std::fmt::Debug for BleIoWorkerPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BleIoWorkerPermit")
    }
}
impl Drop for BleIoWorkerPermit {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(active) = state.ble_io_workers.get_mut(&self.1) {
            *active = active.saturating_sub(1);
            if *active == 0 {
                state.ble_io_workers.remove(&self.1);
            }
        }
        business::finish_retired_business(&mut state);
    }
}
pub(crate) struct BleControlPermit(Arc<Gate>, Option<String>);
impl BleControlPermit {
    pub(crate) fn release(mut self) -> Result<(), &'static str> {
        let Some(token) = self.1.as_deref() else {
            return Ok(());
        };
        self.0.release_ble_control(token)?;
        self.1 = None;
        Ok(())
    }
}
impl Drop for BleControlPermit {
    fn drop(&mut self) {
        if let Some(token) = self.1.take() {
            let _ = self.0.release_ble_control(&token);
        }
    }
}
impl Drop for WorkerPermit {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(active) = state.workers.get_mut(&self.1) {
            *active = active.saturating_sub(1);
            if *active == 0 {
                state.workers.remove(&self.1);
            }
        }
        business::finish_retired_business(&mut state);
    }
}
impl Gate {
    pub(crate) fn set_ordinary_admission(
        &self,
        admission: Arc<crate::ordinary_vowifi_admission::Gate>,
    ) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if let Some(current) = &state.ordinary_admission {
            if !Arc::ptr_eq(current, &admission) {
                return Err("reader_ordinary_admission_conflict");
            }
        } else {
            state.ordinary_admission = Some(admission);
        }
        Ok(())
    }
    pub(crate) fn initialize_native_reader_producer(&self, producer: &str) {
        assert!(!producer.is_empty() && producer.len() <= 128);
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            state.native_reader_producer.is_none(),
            "native reader producer already initialized"
        );
        state.native_reader_producer = Some(producer.to_owned());
    }
    pub(crate) fn ensure_native_reader_producer(&self, producer: &str) -> Result<(), &'static str> {
        if producer.is_empty() || producer.len() > 128 || producer.chars().any(char::is_control) {
            return Err("invalid_native_producer");
        }
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        match &state.native_reader_producer {
            Some(current) if current != producer => Err("native_reader_producer_conflict"),
            Some(_) => Ok(()),
            None => {
                state.native_reader_producer = Some(producer.to_owned());
                Ok(())
            }
        }
    }
    pub(crate) fn native_reader_producer(&self) -> Option<String> {
        let state = self.0.lock().ok()?;
        state
            .native_reader_producer
            .clone()
            .or_else(|| state.business.producer().map(str::to_owned))
    }
    pub(crate) fn set_native_session_supplier(
        &self,
        supplier: std::sync::Weak<dyn crate::reader_session_lease::NativeSessionSupplier>,
    ) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .native_session_supplier = Some(supplier);
    }
    pub(crate) fn native_session_supplier(
        &self,
    ) -> Result<Arc<dyn crate::reader_session_lease::NativeSessionSupplier>, &'static str> {
        self.0
            .lock()
            .map_err(|_| "reader_gate_failed")?
            .native_session_supplier
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or("reader_native_supplier_unavailable")
    }
    pub(crate) fn reader_metadata(&self, reader: &str, generation: u64) -> Option<Value> {
        let state = self.0.lock().ok()?;
        if !state.selection.confirms(reader, generation) {
            return None;
        }
        let (stored_reader, stored_generation, metadata) = state.reader_metadata.as_ref()?;
        (stored_reader == reader && *stored_generation == generation).then(|| metadata.clone())
    }

    /// Hotplug monitoring must not accumulate absence while a native reader
    /// session owns the card. The monitor queries this state in Rust instead
    /// of accepting a Go-local lease assertion.
    pub(crate) fn native_pre_admission_active(&self) -> bool {
        self.0
            .lock()
            .map_or(true, |state| state.native.preparation_active())
    }

    pub(crate) fn native_pre_admission_current(&self, expected: &NativePreAdmission) -> bool {
        self.0
            .lock()
            .map_or(true, |state| state.native.pre_matches(expected))
    }
    pub(crate) fn native_reader_session_active(&self) -> bool {
        self.0.lock().map_or(true, |state| {
            state.native.active() || state.business.native_active()
        })
    }

    /// A bridge status probe may be queued behind card I/O on Swift's serial
    /// BLE executor. Treat all Rust-visible work for this reader as an
    /// indeterminate sample instead of letting the probe time out and
    /// incorrectly confirm a disconnect.
    pub(crate) fn reader_work_active(&self, reader: &str) -> bool {
        self.0.lock().map_or(true, |state| {
            state.native.active()
                || state.business.owner.is_some()
                || state.business.native_active()
                || state.owner_reader.as_deref() == Some(reader)
                || state.workers.get(reader).copied().unwrap_or_default() != 0
                || state.ble_io_workers.contains_key(reader)
                || state
                    .operation_workers
                    .values()
                    .any(|(active_reader, _)| active_reader == reader)
        })
    }

    /// Validate the producer and register queued work atomically with Go exit.
    /// The Go adapter still supplies transaction admission during migration.
    pub(crate) fn track_go_ble_io(
        self: &Arc<Self>,
        reader: &str,
        producer: &str,
    ) -> Result<BleIoWorkerPermit, &'static str> {
        if !crate::reader_ble_apdu::valid_ble_reader(reader) {
            return Err("invalid_reader_binding");
        }
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if producer.is_empty() || !state.business.is_producer(producer) {
            return Err("stale_producer");
        }
        self.register_ble_io(&mut state, reader)
    }

    fn register_ble_io(
        self: &Arc<Self>,
        state: &mut State,
        reader: &str,
    ) -> Result<BleIoWorkerPermit, &'static str> {
        let active = state.ble_io_workers.entry(reader.to_owned()).or_default();
        *active = active.checked_add(1).ok_or("reader_gate_failed")?;
        Ok(BleIoWorkerPermit(self.clone(), reader.to_owned()))
    }

    #[cfg(test)]
    pub(crate) fn test_confirm_go_exit(self: &Arc<Self>, next_producer: &str) {
        confirmed_go_exit_on(self, next_producer);
    }

    /// Record a physical bridge job after its caller's existing admission.
    /// Do not recursively acquire business admission: native/N-wait/finalize
    /// callers already hold it. The real transport owns this guard, including
    /// while polling an uncertain submitted operation for its terminal proof.
    pub(crate) fn track_ble_io(
        self: &Arc<Self>,
        reader: &str,
    ) -> Result<BleIoWorkerPermit, &'static str> {
        if !crate::reader_ble_apdu::valid_ble_reader(reader) {
            return Err("invalid_reader_binding");
        }
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        self.register_ble_io(&mut state, reader)
    }
    /// Borrow only the already-bound N待 owner for its shutdown restoration.
    /// Counting a real worker prevents release/replacement racing cleanup I/O.
    #[cfg(test)]
    pub(crate) fn admit_shutdown_cleanup(
        self: &Arc<Self>,
        reader: &str,
    ) -> Result<WorkerPermit, ()> {
        let mut state = self.0.lock().map_err(|_| ())?;
        if state.owner.is_none()
            || state.owner_reader.as_deref() != Some(reader)
            || state
                .owner_generation
                .is_none_or(|generation| generation == 0)
            || state.ble_control_owner.is_some()
        {
            return Err(());
        }
        let active = state.workers.entry(reader.to_owned()).or_default();
        *active = active.checked_add(1).ok_or(())?;
        Ok(WorkerPermit(self.clone(), reader.to_owned()))
    }

    pub(crate) fn admit(self: &Arc<Self>, reader: &str) -> Result<WorkerPermit, ()> {
        if reader.trim().is_empty() || reader.len() > 512 {
            return Err(());
        }
        let mut state = self.0.lock().map_err(|_| ())?;
        if state.operations.quiescing
            || state.ble_control_owner.is_some()
            || state.native.active()
            || state.selection.pending()
            || state.ble_io_workers.contains_key(reader)
        {
            return Err(());
        }
        if state
            .owner_reader
            .as_deref()
            .is_some_and(|owner| owner == reader)
            || (state.owner.is_some() && state.owner_reader.is_none())
        {
            return Err(());
        }
        if state
            .operation_workers
            .values()
            .any(|(active_reader, _)| active_reader == reader)
        {
            return Err(());
        }
        let active = state.workers.entry(reader.to_owned()).or_default();
        *active = active.checked_add(1).ok_or(())?;
        Ok(WorkerPermit(self.clone(), reader.to_owned()))
    }
    #[cfg(test)]
    pub(crate) fn acquire(
        &self,
        retry: Option<&str>,
        reader: Option<&str>,
        generation: Option<u64>,
    ) -> Result<String, &'static str> {
        if reader.is_some() != generation.is_some()
            || reader.is_some_and(|value| value.trim().is_empty() || value.len() > 512)
            || generation == Some(0)
        {
            return Err("invalid_reader_binding");
        }
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.ble_control_owner.is_some()
            || state.business.owner.is_some()
            || state.native.active()
        {
            return Err("card_busy");
        }
        if let Some(owner) = state.owner.as_deref() {
            return if Some(owner) == retry
                && state.owner_reader.as_deref() == reader
                && state.owner_generation == generation
            {
                Ok(owner.to_owned())
            } else {
                Err("card_busy")
            };
        }
        let active_workers = match reader {
            Some(reader) => {
                state.workers.get(reader).copied().unwrap_or_default()
                    + state
                        .operation_workers
                        .values()
                        .filter(|(active_reader, _)| active_reader == reader)
                        .count()
            }
            None => state.workers.values().copied().sum::<usize>() + state.operation_workers.len(),
        };
        if active_workers != 0 {
            return Err("card_busy");
        }
        if retry.is_some() && retry == state.last_released.as_deref() {
            return Err("owner_expired");
        }
        let owner = match retry {
            Some(value) => value.to_owned(),
            None => crate::multistandby_store::random_token().map_err(|_| "entropy_unavailable")?,
        };
        state.owner = Some(owner.clone());
        state.owner_reader = reader.map(str::to_owned);
        state.owner_generation = generation;
        Ok(owner)
    }
    #[cfg(test)]
    pub(crate) fn release(&self, owner: &str) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        // Business reservations are released together with their explicit
        // capability. The legacy physical-only route cannot split ownership.
        if state.business.owner.is_some() {
            return Err("owner_mismatch");
        }
        if state.owner.as_deref() != Some(owner) {
            return if state.owner.is_none() && state.last_released.as_deref() == Some(owner) {
                Ok(())
            } else {
                Err("owner_mismatch")
            };
        }
        let active_workers = match state.owner_reader.as_deref() {
            Some(reader) => {
                state.workers.get(reader).copied().unwrap_or_default()
                    + state
                        .operation_workers
                        .values()
                        .filter(|(active_reader, _)| active_reader == reader)
                        .count()
            }
            None => state.workers.values().copied().sum::<usize>() + state.operation_workers.len(),
        };
        if active_workers != 0 {
            return Err("card_busy");
        }
        state.owner = None;
        state.last_released = Some(owner.to_owned());
        state.owner_reader = None;
        state.owner_generation = None;
        Ok(())
    }

    #[cfg(test)]
    fn acquire_operation(
        &self,
        token: &str,
        reader: &str,
        generation: u64,
    ) -> Result<(), &'static str> {
        if !valid_operation_binding(token, reader, generation) {
            return Err("invalid_reader_binding");
        }
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.ble_control_owner.is_some() || state.native.active() {
            return Err("card_busy");
        }
        if let Some(existing) = state.operation_workers.get(token) {
            return if existing == &(reader.to_owned(), generation) {
                Ok(())
            } else {
                Err("owner_mismatch")
            };
        }
        if state.released_operations.contains_key(token) {
            return Err("owner_expired");
        }
        if state.selection.pending() {
            return Err("card_busy");
        }
        // Existing exact capabilities remain retryable across a transition
        // they borrowed; a new request must name the current Rust selection.
        if !state.selection.confirms(reader, generation) {
            return Err("stale_reader_generation");
        }
        if state
            .owner_reader
            .as_deref()
            .is_some_and(|active| active == reader)
            || (state.owner.is_some() && state.owner_reader.is_none())
            || state.workers.get(reader).copied().unwrap_or_default() != 0
            || state.ble_io_workers.contains_key(reader)
            || state
                .operation_workers
                .values()
                .any(|(active, _)| active == reader)
        {
            return Err("card_busy");
        }
        state
            .operation_workers
            .insert(token.to_owned(), (reader.to_owned(), generation));
        Ok(())
    }

    #[cfg(test)]
    fn release_operation(
        &self,
        token: &str,
        reader: &str,
        generation: u64,
    ) -> Result<(), &'static str> {
        if !valid_operation_binding(token, reader, generation) {
            return Err("invalid_reader_binding");
        }
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if let Some(existing) = state.operation_workers.get(token) {
            if existing != &(reader.to_owned(), generation) {
                return Err("owner_mismatch");
            }
            if state.selection.borrowed_operation(token) {
                return Err("card_busy");
            }
            if state.ble_io_workers.contains_key(reader) {
                return Err("card_busy");
            }
            state.operation_workers.remove(token);
            state
                .released_operations
                .insert(token.to_owned(), (reader.to_owned(), generation));
            state.released_operation_order.push_back(token.to_owned());
            while state.released_operation_order.len() > 128 {
                if let Some(expired) = state.released_operation_order.pop_front() {
                    state.released_operations.remove(&expired);
                }
            }
            return Ok(());
        }
        if state.released_operations.get(token) == Some(&(reader.to_owned(), generation)) {
            Ok(())
        } else {
            Err("owner_mismatch")
        }
    }

    pub(crate) fn acquire_ble_control(
        self: &Arc<Self>,
        token: &str,
    ) -> Result<BleControlPermit, &'static str> {
        if !valid_operation_token(token) {
            return Err("invalid_request");
        }
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if let Some(owner) = state.ble_control_owner.as_deref() {
            return if owner == token {
                Err("owner_mismatch")
            } else {
                Err("card_busy")
            };
        }
        if state.selection.pending()
            || state.native.active()
            || state.business.owner.is_some()
            || state.owner.is_some()
            || !state.workers.is_empty()
            || !state.ble_io_workers.is_empty()
            || !state.operation_workers.is_empty()
        {
            return Err("card_busy");
        }
        if state.last_released_ble_control.as_deref() == Some(token) {
            return Err("owner_expired");
        }
        state.ble_control_owner = Some(token.to_owned());
        drop(state);
        Ok(BleControlPermit(self.clone(), Some(token.to_owned())))
    }

    pub(crate) fn release_ble_control(&self, token: &str) -> Result<(), &'static str> {
        if !valid_operation_token(token) {
            return Err("invalid_request");
        }
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.ble_control_owner.as_deref() == Some(token) {
            state.ble_control_owner = None;
            state.last_released_ble_control = Some(token.to_owned());
            return Ok(());
        }
        if state.ble_control_owner.is_none()
            && state.last_released_ble_control.as_deref() == Some(token)
        {
            return Ok(());
        }
        Err("owner_mismatch")
    }
}

fn valid_operation_token(token: &str) -> bool {
    token.len() >= 32
        && token.len() <= 128
        && token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}

fn valid_operation_binding(token: &str, reader: &str, generation: u64) -> bool {
    token.len() >= 32
        && token.len() <= 128
        && token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        && !reader.trim().is_empty()
        && reader.len() <= 512
        && generation != 0
}
pub(crate) fn gate() -> Arc<Gate> {
    static GATE: OnceLock<Arc<Gate>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(Gate::default())).clone()
}

/// Called only after the supervisor confirms that its exact Go child exited.
pub(crate) fn confirmed_go_exit(next_producer: &str) {
    confirmed_go_exit_on(&gate(), next_producer);
}

pub(crate) fn confirmed_go_exit_on(gate: &Arc<Gate>, next_producer: &str) {
    let mut state = gate.0.lock().unwrap_or_else(|e| e.into_inner());
    if !state.native_multi.active()
        && let Some(maintenance) = &mut state.multistandby_maintenance
    {
        maintenance.retired = true;
    }
    if let Some(legacy) = &mut state.legacy_business_guard {
        legacy.retired = true;
    }
    state.business = business::BusinessState::for_producer(next_producer);
    if state.native_reader_producer.is_none() && !state.selection.native_owned() {
        state.native.retire_producer();
        state.selection.retire();
        state.reader_metadata = None;
    }
    if !state.native_multi.active()
        && let Some(owner) = state.owner.take()
    {
        state.last_released = Some(owner);
        state.owner_reader = None;
        state.owner_generation = None;
    }
    state.operations.retire_producer();
    let retained: Vec<_> = state
        .operation_workers
        .keys()
        .filter(|t| state.operations.contains(t))
        .cloned()
        .collect();
    state.operation_workers.retain(|t, _| retained.contains(t));
    business::finish_retired_business(&mut state);
}
#[cfg(test)]
pub(crate) async fn handle_legacy_operation_wire(
    request: Request<Incoming>,
    owner: crate::reader_pcsc::Owner,
    gate: Arc<Gate>,
) -> Response<Full<Bytes>> {
    operation::handle_legacy_wire(request, owner, gate).await
}
#[cfg(test)]
fn parse_reader_binding(body: &Value) -> Option<(&str, u64)> {
    let reader = body.get("reader")?.as_str()?;
    let generation = body.get("reader_generation")?.as_u64()?;
    if reader.trim().is_empty() || reader.len() > 512 || generation == 0 {
        return None;
    }
    Some((reader, generation))
}
fn reject(code: &str, status: StatusCode) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"读卡器占用请求未完成","code":code,"retryable":code=="card_busy"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ble_control_lease_excludes_reader_workers_and_reservations_until_drop() {
        let gate = Arc::new(Gate::default());
        let token = "ble-control-token-0000000000000000000000000000";
        let permit = gate.acquire_ble_control(token).unwrap();
        assert!(gate.admit("reader-a").is_err());
        assert_eq!(
            gate.acquire_operation("operation-token-00000000000000000000", "reader-a", 1),
            Err("card_busy")
        );
        assert_eq!(gate.acquire(None, None, None), Err("card_busy"));
        assert!(matches!(
            gate.acquire_ble_control("other-token-0000000000000000000000000000"),
            Err("card_busy")
        ));
        permit.release().unwrap();
        assert!(gate.admit("reader-a").is_ok());
    }

    #[test]
    fn ble_control_lease_is_released_when_request_scope_is_dropped() {
        let gate = Arc::new(Gate::default());
        {
            let _permit = gate
                .acquire_ble_control("ble-control-token-0000000000000000000000000000")
                .unwrap();
            assert!(gate.admit("reader-a").is_err());
        }
        assert!(gate.admit("reader-a").is_ok());
    }

    #[test]
    fn ble_control_lease_rejects_invalid_tokens() {
        let gate = Arc::new(Gate::default());
        assert!(matches!(
            gate.acquire_ble_control("short"),
            Err("invalid_request")
        ));
    }

    #[test]
    fn hotplug_presence_is_blocked_while_ble_card_io_is_in_flight() {
        let gate = Arc::new(Gate::default());
        let reader = "BLE:12345678-1234-1234-1234-123456789abc";
        assert!(!gate.reader_work_active(reader));
        let permit = gate.track_ble_io(reader).unwrap();
        assert!(gate.reader_work_active(reader));
        drop(permit);
        assert!(!gate.reader_work_active(reader));
    }

    #[test]
    fn reservation_and_workers_are_atomic_and_owner_scoped() {
        let gate = Arc::new(Gate::default());
        let permit = gate.admit("reader-a").unwrap();
        assert_eq!(gate.acquire(None, None, None), Err("card_busy"));
        let other_reader = gate.admit("reader-b").unwrap();
        drop(other_reader);
        assert_eq!(
            gate.acquire(None, Some("reader-a"), Some(1)),
            Err("card_busy")
        );
        drop(permit);
        let owner = gate.acquire(None, None, None).unwrap();
        assert!(gate.admit("reader-a").is_err());
        assert_eq!(gate.acquire(Some(&owner), None, None).unwrap(), owner);
        assert_eq!(gate.release("old-owner"), Err("owner_mismatch"));
        assert!(gate.admit("reader-a").is_err());
        gate.release(&owner).unwrap();
        gate.release(&owner).unwrap();
        assert!(gate.admit("reader-a").is_ok());
        let next = gate.acquire(None, None, None).unwrap();
        assert_ne!(next, owner);
        assert_eq!(gate.release(&owner), Err("owner_mismatch"));
    }

    #[test]
    fn reader_owner_is_bound_to_identity_and_generation() {
        let gate = Arc::new(Gate::default());
        let reader = "BLE:12345678-1234-1234-1234-123456789abc";
        let owner = gate.acquire(None, Some(reader), Some(7)).unwrap();
        assert_eq!(
            gate.acquire(Some(&owner), Some(reader), Some(8)),
            Err("card_busy")
        );
        assert_eq!(
            gate.acquire(Some(&owner), Some("Reader B"), Some(7)),
            Err("card_busy")
        );
        assert_eq!(
            gate.acquire(Some(&owner), Some(reader), Some(7)).unwrap(),
            owner
        );
        assert_eq!(gate.acquire(None, Some(reader), Some(7)), Err("card_busy"));
        gate.release(&owner).unwrap();
        assert!(
            !gate
                .acquire(None, Some(reader), Some(7))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unrelated_reader_worker_does_not_block_reservation_release() {
        let gate = Arc::new(Gate::default());
        let owner = gate
            .acquire(None, Some("reader-a"), Some(3))
            .expect("reader A reservation should succeed");
        let other_reader = gate
            .admit("reader-b")
            .expect("reader B work should be independent");
        gate.release(&owner)
            .expect("reader B work must not pin reader A reservation");
        drop(other_reader);

        let reader_a_worker = gate.admit("reader-a").unwrap();
        let owner = gate.acquire(None, Some("reader-a"), Some(4));
        assert_eq!(owner, Err("card_busy"));
        drop(reader_a_worker);
    }

    #[test]
    fn external_operation_lease_is_reader_generation_and_token_bound() {
        let gate = Arc::new(Gate::default());
        lifecycle::seed_test_selection(&gate, "reader-a", 4);
        let token = "operation-token-0123456789abcdef0123456789";
        gate.acquire_operation(token, "reader-a", 4).unwrap();
        assert_eq!(gate.admit("reader-a").err(), Some(()));
        assert!(gate.admit("reader-b").is_ok());
        assert_eq!(
            gate.acquire(None, Some("reader-a"), Some(4)),
            Err("card_busy")
        );
        assert_eq!(
            gate.acquire_operation(token, "reader-a", 5),
            Err("owner_mismatch")
        );
        assert_eq!(
            gate.release_operation(token, "reader-a", 5),
            Err("owner_mismatch")
        );
        gate.release_operation(token, "reader-a", 4).unwrap();
        gate.release_operation(token, "reader-a", 4).unwrap();
        assert!(gate.admit("reader-a").is_ok());
    }

    #[test]
    fn confirmed_go_exit_releases_only_external_operation_leases() {
        let gate = Arc::new(Gate::default());
        lifecycle::seed_test_selection(&gate, "reader-a", 1);
        let token = "operation-token-0123456789abcdef0123456789";
        gate.acquire_operation(token, "reader-a", 1).unwrap();
        let reservation = gate.acquire(None, Some("reader-b"), Some(1)).unwrap();
        confirmed_go_exit_on(&gate, "next-go-producer");
        assert!(gate.admit("reader-a").is_ok());
        assert_eq!(gate.release(&reservation), Ok(()));
    }

    #[test]
    fn incomplete_or_zero_reader_binding_is_rejected() {
        let gate = Gate::default();
        assert_eq!(
            gate.acquire(None, Some("Reader A"), None),
            Err("invalid_reader_binding")
        );
        assert_eq!(
            gate.acquire(None, Some("Reader A"), Some(0)),
            Err("invalid_reader_binding")
        );
        assert_eq!(
            gate.acquire(None, None, Some(1)),
            Err("invalid_reader_binding")
        );
    }
    #[test]
    fn acquire_wire_binding_rejects_missing_types_and_invalid_values() {
        for body in [
            json!({"owner_token":"owner-token"}),
            json!({"reader":7,"reader_generation":1}),
            json!({"reader":"reader-a","reader_generation":"1"}),
            json!({"reader":"reader-a","reader_generation":0}),
            json!({"reader":"  ","reader_generation":1}),
            json!({"reader":"x".repeat(513),"reader_generation":1}),
        ] {
            assert!(
                parse_reader_binding(&body).is_none(),
                "accepted invalid binding: {body}"
            );
        }
        assert_eq!(
            parse_reader_binding(&json!({"reader":"reader-a","reader_generation":7})),
            Some(("reader-a", 7))
        );
    }
    #[tokio::test]
    async fn timed_out_blocking_worker_retains_admission_until_actual_return() {
        let gate = Arc::new(Gate::default());
        let worker_gate = gate.clone();
        let (send, recv) = std::sync::mpsc::channel();
        let (ready, started) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(crate::reader_task::run_with_gate(
            worker_gate,
            "reader-a",
            move || {
                ready.send(()).unwrap();
                recv.recv().unwrap();
            },
            std::time::Duration::from_millis(10),
        ));
        started.await.unwrap();
        assert_eq!(
            worker.await.unwrap(),
            Err(crate::reader_task::ReaderTaskError::Timeout)
        );
        assert_eq!(
            gate.acquire(None, Some("reader-a"), Some(1)),
            Err("card_busy")
        );
        let other_reader = gate.admit("reader-b").unwrap();
        drop(other_reader);
        send.send(()).unwrap();
        for _ in 0..100 {
            if gate.acquire(None, None, None).is_ok() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("worker should release admission after finishing");
    }
}
