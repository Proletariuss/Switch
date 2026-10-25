//! Cross-process business admission lives in the same mutex as physical
//! reservations. Capabilities are explicit and immutable at each Go callsite.
//! Sequence fencing prevents a delayed acquire from reviving a released owner.
use super::*;
use serde::Deserialize;

pub(crate) const BUSINESS_PATH: &str = "/api/internal/reader-business";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Role {
    Operation,
    NativeSession,
    Multistandby,
}
impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Operation => "operation",
            Self::NativeSession => "native_session",
            Self::Multistandby => "multistandby",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Capability {
    token: String,
    sequence: u64,
    role: Role,
    binding: Option<(String, u64)>,
}

#[derive(Default)]
pub(super) struct BusinessState {
    producer: Option<String>,
    high_water: u64,
    pub(super) owner: Option<Capability>,
}

pub(super) struct MultistandbyMaintenance {
    guard: Arc<crate::ordinary_vowifi_admission::Maintenance>,
    pub(super) retired: bool,
}

pub(super) struct LegacyBusinessGuard {
    guard: Arc<crate::ordinary_vowifi_admission::LegacyReaderBusiness>,
    pub(super) retired: bool,
}

// Go exit retires its workflow, but submitted native BLE/PCSC work can outlive
// that process. Only the real completion paths call this under the physical
// gate; a timer or dropped HTTP waiter cannot release this maintenance slot.
pub(super) fn finish_retired_business(state: &mut State) {
    if let Some(legacy) = &state.legacy_business_guard {
        if legacy.retired
            && !state.selection.pending()
            && physical_workers(state, None) == 0
            && legacy.guard.release_confirmed().is_ok()
        {
            state.legacy_business_guard = None;
        }
    }
    let Some(maintenance) = &state.multistandby_maintenance else {
        return;
    };
    if maintenance.retired
        && !state.selection.pending()
        && physical_workers(state, Some(maintenance.guard.reader())) == 0
        && maintenance.guard.release_confirmed().is_ok()
    {
        state.multistandby_maintenance = None;
    }
}
impl BusinessState {
    pub(super) fn producer(&self) -> Option<&str> {
        self.producer.as_deref()
    }
    pub(super) fn native_active(&self) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|c| c.role == Role::NativeSession)
    }
    pub(super) fn is_producer(&self, producer: &str) -> bool {
        self.producer.as_deref() == Some(producer)
    }
    pub(super) fn matches_lifecycle(&self, token: &str, sequence: u64, role: &str) -> bool {
        self.owner.as_ref().is_some_and(|c| {
            c.token == token
                && c.sequence == sequence
                && c.role.name() == role
                && c.role != Role::Multistandby
        })
    }
    pub(super) fn matches_operation(
        &self,
        token: &str,
        sequence: u64,
        role: &str,
        reader: &str,
        generation: u64,
    ) -> bool {
        self.owner.as_ref().is_some_and(|c| {
            c.token == token
                && c.sequence == sequence
                && c.role.name() == role
                && match c.role {
                    Role::Operation => c.binding.is_none(),
                    Role::Multistandby => {
                        c.binding.as_ref() == Some(&(reader.to_owned(), generation))
                    }
                    _ => false,
                }
        })
    }
    pub(super) fn for_producer(producer: &str) -> Self {
        Self {
            producer: Some(producer.to_owned()),
            ..Self::default()
        }
    }
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    action: String,
    producer_id: String,
    token: Option<String>,
    sequence: Option<u64>,
    role: Option<Role>,
    from_role: Option<Role>,
    reader: Option<String>,
    reader_generation: Option<u64>,
}
#[cfg(test)]
impl Command {
    fn capability(&self) -> Result<Capability, &'static str> {
        let token = self.token.as_deref().ok_or("invalid_request")?;
        let sequence = self.sequence.filter(|s| *s != 0).ok_or("invalid_request")?;
        let role = self.role.ok_or("invalid_request")?;
        if !valid_operation_token(token) {
            return Err("invalid_request");
        }
        let binding = match (role, self.reader.as_deref(), self.reader_generation) {
            (Role::Multistandby, Some(reader), Some(generation))
                if valid_operation_binding(token, reader, generation) =>
            {
                Some((reader.to_owned(), generation))
            }
            (Role::Operation | Role::NativeSession, None, None) => None,
            _ => return Err("invalid_request"),
        };
        Ok(Capability {
            token: token.to_owned(),
            sequence,
            role,
            binding,
        })
    }
}

impl Gate {
    #[cfg(test)]
    pub(crate) fn test_business_command(
        &self,
        value: Value,
        stopping: bool,
    ) -> Result<Value, &'static str> {
        let command: Command = serde_json::from_value(value).map_err(|_| "invalid_request")?;
        self.business_command(&command, stopping)
    }
    /// Before the first Go spawn. Producer replacement is performed only by
    /// confirmed_go_exit under this same mutex after the old child has exited.
    pub(crate) fn initialize_business_producer(&self, producer: &str) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            state.business.producer.is_none(),
            "reader producer already initialized"
        );
        state.business = BusinessState::for_producer(producer);
    }

    // Frozen legacy mutation oracle; production only exposes business_status.
    #[cfg(test)]
    fn business_command(&self, command: &Command, stopping: bool) -> Result<Value, &'static str> {
        if command.producer_id.is_empty() || command.producer_id.len() > 128 {
            return Err("invalid_request");
        }
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.business.producer.as_deref() != Some(command.producer_id.as_str()) {
            return Err("stale_producer");
        }
        if stopping && !matches!(command.action.as_str(), "release" | "status") {
            return Err("backend_stopping");
        }
        if command.action == "status" {
            if command.token.is_some()
                || command.sequence.is_some()
                || command.role.is_some()
                || command.from_role.is_some()
                || command.reader.is_some()
                || command.reader_generation.is_some()
            {
                return Err("invalid_request");
            }
            let role = state.business.owner.as_ref().map(|owner| owner.role);
            return Ok(json!({
                "ok":true,
                "operation_active":role == Some(Role::Operation) || state.selection.pending() || !state.ble_io_workers.is_empty(),
                "native_session_active":role == Some(Role::NativeSession) || state.native.active(),
                "multistandby_active":role == Some(Role::Multistandby) || state.owner.is_some(),
                "ble_control_active":state.ble_control_owner.is_some(),
            }));
        }
        let cap = command.capability()?;
        if command.action != "transfer" && command.from_role.is_some() {
            return Err("invalid_request");
        }
        match command.action.as_str() {
            "acquire" => {
                if state.business.owner.as_ref() == Some(&cap) {
                    return Ok(ack(&cap));
                }
                if cap.sequence <= state.business.high_water {
                    return Err("owner_expired");
                }
                // A rejected acquire is terminal too: retrying a previously
                // busy request later must never unexpectedly gain admission.
                state.business.high_water = cap.sequence;
                if state.operations.quiescing
                    || state.selection.pending()
                    || !state.operation_workers.is_empty()
                    || state.native.active()
                    || !state.ble_io_workers.is_empty()
                    || state.business.owner.is_some()
                    || state.ble_control_owner.is_some()
                    || state.owner.is_some()
                    || state.legacy_business_guard.is_some()
                {
                    return Err("card_busy");
                }
                if cap.role == Role::Multistandby {
                    install_physical_owner(&mut state, &cap)?;
                } else if let Some(admission) = &state.ordinary_admission {
                    let guard = admission
                        .begin_legacy_reader_business(cap.role == Role::NativeSession)
                        .map_err(|_| "card_busy")?;
                    state.legacy_business_guard = Some(LegacyBusinessGuard {
                        guard,
                        retired: false,
                    });
                }
                state.business.owner = Some(cap.clone());
            }
            "transfer" => {
                if command.from_role != Some(Role::Operation)
                    || !matches!(cap.role, Role::NativeSession | Role::Multistandby)
                {
                    return Err("invalid_request");
                }
                // Reply loss is safe: an identical transfer is idempotent.
                if state.business.owner.as_ref() == Some(&cap) {
                    return Ok(ack(&cap));
                }
                if state.selection.pending()
                    || !state.operation_workers.is_empty()
                    || !state.ble_io_workers.is_empty()
                {
                    return Err("card_busy");
                }
                let expected = Capability {
                    token: cap.token.clone(),
                    sequence: cap.sequence,
                    role: Role::Operation,
                    binding: None,
                };
                if state.business.owner.as_ref() != Some(&expected) {
                    return Err("owner_mismatch");
                }
                if cap.role == Role::Multistandby {
                    install_physical_owner(&mut state, &cap)?;
                } else if let (Some(admission), Some(legacy)) =
                    (&state.ordinary_admission, &state.legacy_business_guard)
                {
                    admission
                        .promote_legacy_reader_business(&legacy.guard)
                        .map_err(|_| "card_busy")?;
                }
                state.business.owner = Some(cap.clone());
            }
            "release" => {
                if let Some(owner) = state.business.owner.as_ref() {
                    if owner.token == cap.token || owner.sequence == cap.sequence {
                        // In particular, a late operation defer cannot clear
                        // the same token after transfer to native/N-wait.
                        if owner != &cap {
                            return Err("owner_mismatch");
                        }
                        if state.selection.borrowed_business(&cap.token, cap.sequence)
                            || state.operations.borrows_business(&cap.token, cap.sequence)
                        {
                            return Err("card_busy");
                        }
                        if !state.ble_io_workers.is_empty() {
                            return Err("card_busy");
                        }
                        if cap.role == Role::Multistandby {
                            if state.owner.as_deref() != Some(cap.token.as_str())
                                || cap.binding.as_ref()
                                    != state
                                        .owner_reader
                                        .as_ref()
                                        .zip(state.owner_generation)
                                        .map(|(r, g)| (r.clone(), g))
                                        .as_ref()
                            {
                                return Err("owner_mismatch");
                            }
                            if physical_workers(
                                &state,
                                cap.binding.as_ref().map(|(r, _)| r.as_str()),
                            ) != 0
                            {
                                return Err("card_busy");
                            }
                            if let Some(maintenance) = &state.multistandby_maintenance {
                                maintenance
                                    .guard
                                    .release_confirmed()
                                    .map_err(|_| "card_busy")?;
                            }
                            state.multistandby_maintenance = None;
                            state.owner = None;
                            state.owner_reader = None;
                            state.owner_generation = None;
                            state.last_released = Some(cap.token.clone());
                        } else if let Some(legacy) = &state.legacy_business_guard {
                            if physical_workers(&state, None) != 0 {
                                return Err("card_busy");
                            }
                            legacy.guard.release_confirmed().map_err(|_| "card_busy")?;
                            state.legacy_business_guard = None;
                        }
                        state.business.owner = None;
                    }
                }
                // Also revoke requests which have not arrived yet. A late
                // acquire cannot resurrect a capability after cleanup.
                state.business.high_water = state.business.high_water.max(cap.sequence);
            }
            _ => return Err("invalid_request"),
        }
        Ok(ack(&cap))
    }
}

pub(super) fn physical_workers(state: &State, reader: Option<&str>) -> usize {
    match reader {
        Some(reader) => {
            state.workers.get(reader).copied().unwrap_or_default()
                + state
                    .operation_workers
                    .values()
                    .filter(|(r, _)| r == reader)
                    .count()
                + state
                    .ble_io_workers
                    .get(reader)
                    .copied()
                    .unwrap_or_default()
        }
        None => {
            state.workers.values().copied().sum::<usize>()
                + state.operation_workers.len()
                + state.ble_io_workers.values().copied().sum::<usize>()
        }
    }
}
#[cfg(test)]
fn install_physical_owner(state: &mut State, cap: &Capability) -> Result<(), &'static str> {
    let Some((reader, generation)) = cap.binding.as_ref() else {
        return Err("invalid_request");
    };
    install_multistandby_owner(state, &cap.token, reader, *generation)
}
pub(super) fn install_multistandby_owner(
    state: &mut State,
    token: &str,
    reader: &str,
    generation: u64,
) -> Result<(), &'static str> {
    if state.owner.is_some()
        || state.ble_control_owner.is_some()
        || state.multistandby_maintenance.is_some()
        || physical_workers(state, Some(reader)) != 0
    {
        return Err("card_busy");
    }
    if let Some(admission) = &state.ordinary_admission {
        // Taking this slot and the physical owner is one operation. It blocks
        // an ordinary intent even during backoff, when it owns no card lease.
        let guard = if let Some(legacy) = &state.legacy_business_guard {
            if legacy.retired {
                return Err("card_busy");
            }
            admission.transfer_to_maintenance(&legacy.guard, reader, generation)
        } else {
            admission.begin_reader_maintenance(
                reader,
                generation,
                crate::ordinary_vowifi_admission::MaintenancePolicy::Reject,
            )
        }
        .map_err(|_| "card_busy")?;
        state.legacy_business_guard = None;
        state.multistandby_maintenance = Some(MultistandbyMaintenance {
            guard,
            retired: false,
        });
    }
    state.owner = Some(token.to_owned());
    state.owner_reader = Some(reader.to_owned());
    state.owner_generation = Some(generation);
    Ok(())
}
pub(super) fn release_multistandby_owner(
    state: &mut State,
    token: &str,
    reader: &str,
    generation: u64,
) -> Result<(), &'static str> {
    if state.owner.as_deref() != Some(token)
        || state.owner_reader.as_deref() != Some(reader)
        || state.owner_generation != Some(generation)
    {
        return Err("owner_mismatch");
    }
    if state.selection.pending() || physical_workers(state, None) != 0 {
        return Err("card_busy");
    }
    if let Some(maintenance) = &state.multistandby_maintenance {
        maintenance
            .guard
            .release_confirmed()
            .map_err(|_| "card_busy")?;
    }
    state.multistandby_maintenance = None;
    state.owner = None;
    state.owner_reader = None;
    state.owner_generation = None;
    state.last_released = Some(token.to_owned());
    Ok(())
}
#[cfg(test)]
fn ack(cap: &Capability) -> Value {
    json!({"ok":true,"token":cap.token,"role":cap.role.name()})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusRequest {
    action: String,
    producer_id: String,
}

impl Gate {
    /// Compatibility observer for the remaining Go module eSIM admission.
    /// No capability can be acquired, transferred, or released through HTTP.
    fn business_status(&self, producer: &str) -> Result<Value, &'static str> {
        if producer.is_empty() || producer.len() > 128 {
            return Err("invalid_request");
        }
        let state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.business.producer.as_deref() != Some(producer) {
            return Err("stale_producer");
        }
        let role = state.business.owner.as_ref().map(|owner| owner.role);
        Ok(json!({
            "ok":true,
            "operation_active":role == Some(Role::Operation) || state.selection.pending() || !state.ble_io_workers.is_empty(),
            "native_session_active":role == Some(Role::NativeSession) || state.native.active(),
            "multistandby_active":role == Some(Role::Multistandby) || state.owner.is_some(),
            "ble_control_active":state.ble_control_owner.is_some(),
        }))
    }
}

pub(crate) async fn handle_business(
    request: Request<Incoming>,
    _stopping: bool,
    _store_status: proletarius_api_contract::SmsStoreStatus,
) -> Response<Full<Bytes>> {
    handle_status(request, &gate()).await
}

async fn handle_status(request: Request<Incoming>, gate: &Gate) -> Response<Full<Bytes>> {
    if request.method() != hyper::Method::POST || request.uri().path() != BUSINESS_PATH {
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
    let command: StatusRequest = match serde_json::from_slice(&bytes) {
        Ok(command) => command,
        Err(_) => return reject("invalid_request", StatusCode::BAD_REQUEST),
    };
    if command.action != "status" {
        return reject("invalid_request", StatusCode::BAD_REQUEST);
    }
    match gate.business_status(&command.producer_id) {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(code) => reject(
            code,
            match code {
                "invalid_request" => StatusCode::BAD_REQUEST,
                "reader_gate_failed" => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::CONFLICT,
            },
        ),
    }
}

#[cfg(test)]
mod status_tests;
#[cfg(test)]
mod tests;
