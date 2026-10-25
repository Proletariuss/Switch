//! Native per-source call aggregation. Ended records are committed directly;
//! an unconfirmed terminal remains in its source slot, never in a lossy queue.
use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, MutexGuard};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(crate) const OBSERVE_PATH: &str = "/api/internal/call-archive/observe";
pub(crate) const HISTORY_ERROR_PATH: &str = "/api/internal/call-archive/history-error";
const MAX_BODY_BYTES: usize = 16 * 1024;
mod native;
pub(crate) use native::{NativeFact, NativeFactAction, NativeHistory, NativeSink};

#[derive(Clone)]
pub(crate) struct CallFacts {
    store: ArchiveStore,
    sources: Arc<[SourceSlot; 3]>,
    native_startup_errors: Arc<Mutex<Vec<std::sync::Weak<str>>>>,
    #[cfg(test)]
    before_ingest: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
}

struct SourceSlot {
    // Only the worker takes the operation gate. Producer replacement uses
    // the short state lock, so it never waits for archive fsync to complete.
    gate: Mutex<()>,
    state: Mutex<SourceState>,
}

#[derive(Clone)]
struct SourceState {
    producer_id: String,
    owner_id: Option<String>,
    owner_epoch: u64,
    owner_retired: bool,
    last: Option<Observation>,
    active: Option<ActiveCall>,
    frozen: Option<FrozenTerminal>,
    last_closed_call: Option<String>,
    history_error: Option<String>,
    native_pending_error: Option<String>,
    native_claimed: bool,
}

#[derive(Clone)]
struct ActiveCall {
    id: String,
    direction: Direction,
    number: String,
    answered: bool,
    started_at: OffsetDateTime,
    call_id: Option<String>,
}

#[derive(Clone)]
struct FrozenTerminal {
    fact: Observation,
    record: Value,
    call_id: Option<String>,
    retire_owner: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Source {
    ModuleCs,
    ModuleIms,
    ReaderIms,
}

impl Source {
    fn index(self) -> usize {
        match self {
            Self::ModuleCs => 0,
            Self::ModuleIms => 1,
            Self::ReaderIms => 2,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::ModuleCs => "module_cs",
            Self::ModuleIms => "module_ims",
            Self::ReaderIms => "reader_ims",
        }
    }
    fn device(self) -> &'static str {
        if self == Self::ReaderIms {
            "reader"
        } else {
            "module"
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    Begin,
    Number,
    Answered,
    End,
    Ims,
    Retire,
    Snapshot,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
enum Direction {
    #[serde(rename = "in")]
    Incoming,
    #[serde(rename = "out")]
    Outgoing,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Observation {
    producer_id: String,
    source: Source,
    sequence: u64,
    owner_id: String,
    owner_epoch: u64,
    observed_at: String,
    action: Action,
    direction: Option<Direction>,
    number: Option<String>,
    call_id: Option<String>,
    state: Option<String>,
}

impl Observation {
    fn ims_live(&self) -> bool {
        self.action == Action::Ims
            && matches!(
                self.state.as_deref(),
                Some("incoming" | "outgoing" | "early" | "connecting" | "active")
            )
    }
    fn terminal(&self) -> bool {
        matches!(self.action, Action::End | Action::Retire)
            || (self.action == Action::Snapshot && self.state.as_deref() == Some("idle"))
            || (self.action == Action::Ims
                && matches!(
                    self.state.as_deref(),
                    Some("idle" | "ended" | "failed" | "unavailable")
                ))
    }
    fn validate(&self) -> Result<OffsetDateTime, FactError> {
        if !valid_identity(&self.producer_id)
            || !valid_identity(&self.owner_id)
            || self.sequence == 0
            || self.owner_epoch == 0
            || self.observed_at.len() > 64
            || self.number.as_ref().is_some_and(|s| s.len() > 512)
            || self.call_id.as_ref().is_some_and(|s| s.len() > 512)
            || self.state.as_ref().is_some_and(|s| s.len() > 32)
            || match self.source {
                Source::ModuleCs => {
                    self.action == Action::Ims
                        || (self.action == Action::Snapshot
                            && !matches!(
                                self.state.as_deref(),
                                Some("incoming" | "dialing" | "alerting" | "active" | "idle")
                            ))
                }
                Source::ModuleIms | Source::ReaderIms => {
                    !matches!(self.action, Action::Ims | Action::Retire)
                }
            }
            || (self.action == Action::Begin && self.direction.is_none())
            || (self.action == Action::Ims && self.state.as_ref().is_none_or(|s| s.is_empty()))
            || (self.action == Action::Snapshot
                && (self.source != Source::ModuleCs
                    || self.direction.is_some()
                    || self.call_id.is_some()))
            || (self.ims_live()
                && (self.direction.is_none()
                    || self.call_id.as_ref().is_none_or(|s| s.trim().is_empty())))
        {
            return Err(FactError::invalid());
        }
        OffsetDateTime::parse(&self.observed_at, &Rfc3339).map_err(|_| FactError::invalid())
    }
}

fn valid_identity(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 96 && !value.chars().any(char::is_control)
}

#[derive(Debug)]
struct FactError {
    code: &'static str,
    message: String,
    status: StatusCode,
    retryable: bool,
}
impl FactError {
    fn conflict(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
            status: StatusCode::CONFLICT,
            retryable: false,
        }
    }
    fn invalid() -> Self {
        Self {
            code: "invalid_call_fact",
            message: "通话事实格式错误".into(),
            status: StatusCode::BAD_REQUEST,
            retryable: false,
        }
    }
    fn storage(error: impl std::fmt::Display) -> Self {
        Self {
            code: "call_archive_failed",
            message: format!("通话事实尚未获得耐久确认: {error}"),
            status: StatusCode::INTERNAL_SERVER_ERROR,
            retryable: true,
        }
    }
    fn response(self) -> Response<Full<Bytes>> {
        crate::json_response(
            self.status,
            &json!({"error":self.message,"code":self.code,"retryable":self.retryable}),
        )
    }
}

fn state_lock(slot: &SourceSlot) -> Result<MutexGuard<'_, SourceState>, FactError> {
    slot.state
        .lock()
        .map_err(|_| FactError::storage("call source state lock poisoned"))
}

impl CallFacts {
    #[cfg(test)]
    pub(crate) fn set_before_ingest_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.before_ingest.lock().unwrap() = hook;
    }

    pub(crate) fn new(store: ArchiveStore, producer_id: String) -> Self {
        Self {
            store,
            native_startup_errors: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            before_ingest: Arc::new(Mutex::new(None)),
            sources: Arc::new(std::array::from_fn(|_| SourceSlot {
                gate: Mutex::new(()),
                state: Mutex::new(SourceState {
                    producer_id: producer_id.clone(),
                    owner_id: None,
                    owner_epoch: 0,
                    owner_retired: false,
                    last: None,
                    active: None,
                    frozen: None,
                    last_closed_call: None,
                    history_error: None,
                    native_pending_error: None,
                    native_claimed: false,
                }),
            })),
        }
    }

    /// Called only after the supervisor confirms that the old Go child exited.
    /// Lock all short states together, without waiting on the I/O gates. A
    /// worker already writing may finish, but cannot overwrite this new epoch.
    pub(crate) fn replace_producer(&self, producer_id: String) -> Result<(), StoreError> {
        if !valid_identity(&producer_id) {
            return Err(StoreError::Configuration("invalid call producer ID".into()));
        }
        let mut guards = self
            .sources
            .iter()
            .skip(1)
            .map(|slot| {
                slot.state.lock().map_err(|_| {
                    StoreError::Configuration("call source state lock poisoned".into())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for state in &mut guards {
            if state.native_claimed || state.producer_id == producer_id {
                continue;
            }
            state.producer_id = producer_id.clone();
            state.owner_id = None;
            state.owner_epoch = 0;
            state.owner_retired = false;
            state.last = None;
            state.active = None;
            state.last_closed_call = None;
            state.history_error = None;
            // Frozen terminal identity and contents survive producer changes.
        }
        Ok(())
    }

    pub(crate) fn history_error(&self) -> Option<String> {
        let mut messages: Vec<_> = self
            .sources
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let state = slot.state.lock().ok()?;
                source_error(&state).map(|error| {
                    format!(
                        "{}: {error}",
                        ["module_cs", "module_ims", "reader_ims"][index]
                    )
                })
            })
            .collect();
        messages.extend(self.native_startup_errors());
        (!messages.is_empty()).then(|| messages.join("; "))
    }

    fn native_history_error(&self, source: Source) -> Option<String> {
        self.sources[source.index()]
            .state
            .lock()
            .ok()
            .and_then(|state| source_error(&state))
    }

    fn native_delivery_error(&self, source: Source, producer: &str, error: Option<String>) {
        if let Ok(mut state) = self.sources[source.index()].state.lock()
            && state.producer_id == producer
        {
            state.native_pending_error = error;
        }
    }

    fn latch_error(&self, source: Source, producer: &str, error: String) {
        if let Ok(mut state) = self.sources[source.index()].state.lock()
            && state.producer_id == producer
            && !error.is_empty()
        {
            state.history_error.get_or_insert(error);
        }
    }

    fn start_native_source(&self, source: Source, producer: &str) -> Result<(), String> {
        if !valid_identity(producer) {
            return Err("invalid native call producer ID".into());
        }
        let slot = &self.sources[source.index()];
        let mut state = slot
            .state
            .lock()
            .map_err(|_| "native call state poisoned")?;
        if state.native_claimed
            || state.active.is_some()
            || state.frozen.is_some()
            || (state.last.is_some() && !state.owner_retired)
        {
            return Err("native call producer has not drained".into());
        }
        state.producer_id = producer.into();
        state.owner_id = None;
        state.owner_epoch = 0;
        state.owner_retired = false;
        state.last = None;
        state.last_closed_call = None;
        state.history_error = None;
        state.native_pending_error = None;
        state.native_claimed = true;
        Ok(())
    }

    fn release_native_source(&self, source: Source, producer: &str) {
        if let Ok(mut state) = self.sources[source.index()].state.lock()
            && state.producer_id == producer
        {
            state.native_claimed = false;
        }
    }

    /// Wait for each actual worker and retry its terminal before shutdown.
    /// An active call without an observed terminal is never invented as ended.
    pub(crate) fn finish_pending(&self) -> Result<(), StoreError> {
        let mut first_error = None;
        for slot in self.sources.iter() {
            let result = (|| {
                let _gate = slot
                    .gate
                    .lock()
                    .map_err(|_| FactError::storage("call source gate poisoned"))?;
                let frozen = state_lock(slot)?.frozen.clone();
                if let Some(frozen) = frozen {
                    self.commit_frozen(slot, &frozen)?;
                }
                Ok::<_, FactError>(())
            })();
            if let Err(error) = result {
                first_error.get_or_insert(StoreError::Configuration(format!(
                    "{}: {}",
                    error.code, error.message
                )));
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn commit_frozen(&self, slot: &SourceSlot, frozen: &FrozenTerminal) -> Result<(), FactError> {
        #[cfg(test)]
        {
            let hook = self.before_ingest.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        match crate::call_archive::ingest(&self.store, frozen.record.clone())
            .map_err(FactError::storage)?
        {
            crate::call_archive::IngestResult::Conflict => {
                return Err(FactError::conflict(
                    "call_event_conflict",
                    "通话记录ID已对应其他内容",
                ));
            }
            crate::call_archive::IngestResult::Added
            | crate::call_archive::IngestResult::AlreadyRecorded => {}
        }
        let mut state = state_lock(slot)?;
        // Replacement never changes frozen; the per-source gate excludes a
        // second commit. Keep the check explicit for lifecycle safety.
        if state
            .frozen
            .as_ref()
            .is_some_and(|current| current.fact == frozen.fact)
        {
            if state.producer_id == frozen.fact.producer_id {
                if frozen.retire_owner {
                    retire_current_owner(&mut state);
                }
                state.active = None;
                state.last_closed_call = frozen.call_id.clone();
                state.last = Some(frozen.fact.clone());
            }
            state.frozen = None;
        }
        Ok(())
    }

    fn observe(&self, fact: Observation, stopping: bool) -> Result<Value, FactError> {
        self.observe_with_origin(fact, stopping, false)
    }

    fn observe_go(&self, fact: Observation, stopping: bool) -> Result<Value, FactError> {
        self.observe_with_origin(fact, stopping, true)
    }

    fn observe_with_origin(
        &self,
        fact: Observation,
        _stopping: bool,
        from_go: bool,
    ) -> Result<Value, FactError> {
        // These are already-observed historical facts, never physical call
        // commands. Drain every trusted producer sequence during shutdown;
        // rejecting a queued begin/answered would strand its later terminal.
        let observed_at = fact.validate()?;
        let slot = &self.sources[fact.source.index()];
        let _gate = slot
            .gate
            .lock()
            .map_err(|_| FactError::storage("call source gate poisoned"))?;
        {
            let state = state_lock(slot)?;
            check_origin(&state, &fact, from_go)?;
            if let Some(last) = &state.last
                && last.sequence == fact.sequence
            {
                return if last == &fact {
                    Ok(ack(fact.sequence))
                } else {
                    Err(sequence_conflict())
                };
            }
            if let Some(frozen) = &state.frozen {
                if frozen.fact.producer_id == fact.producer_id && frozen.fact != fact {
                    return Err(if frozen.fact.sequence == fact.sequence {
                        sequence_conflict()
                    } else {
                        FactError::conflict(
                            "call_terminal_pending",
                            "上一通话终态尚未确认，请重试原事实",
                        )
                    });
                }
            }
        }
        let pending = state_lock(slot)?.frozen.clone();
        if let Some(frozen) = pending {
            self.commit_frozen(slot, &frozen)?;
            if frozen.fact == fact {
                check_origin(&*state_lock(slot)?, &fact, from_go)?;
                return Ok(ack(fact.sequence));
            }
        }
        let frozen = {
            let mut state = state_lock(slot)?;
            check_origin(&state, &fact, from_go)?;
            let expected = state
                .last
                .as_ref()
                .map_or(Some(1), |last| last.sequence.checked_add(1));
            if expected != Some(fact.sequence) {
                return Err(FactError::conflict(
                    "call_sequence_out_of_order",
                    "通话事实序号不连续",
                ));
            }
            // Invalid observations cannot partly switch the owner or mutate
            // an active call before reporting a rejected sequence.
            let mut candidate = state.clone();
            let applied = apply_fact(&mut candidate, &fact, observed_at)?;
            *state = candidate;
            if let Some(frozen) = applied {
                state.frozen = Some(frozen.clone());
                Some(frozen)
            } else {
                state.last = Some(fact.clone());
                None
            }
        };
        if let Some(frozen) = frozen {
            self.commit_frozen(slot, &frozen)?;
            check_origin(&*state_lock(slot)?, &fact, from_go)?;
        }
        Ok(ack(fact.sequence))
    }
}

fn check_origin(state: &SourceState, fact: &Observation, from_go: bool) -> Result<(), FactError> {
    if from_go && state.native_claimed {
        return Err(FactError::conflict(
            "native_call_source_owned",
            "通话事实由原生服务持有",
        ));
    }
    check_producer(state, fact)
}

fn source_error(state: &SourceState) -> Option<String> {
    match (&state.history_error, &state.native_pending_error) {
        (Some(sticky), Some(pending)) => Some(format!("{sticky}; {pending}")),
        (Some(error), None) | (None, Some(error)) => Some(error.clone()),
        (None, None) => None,
    }
}

fn check_producer(state: &SourceState, fact: &Observation) -> Result<(), FactError> {
    if state.producer_id != fact.producer_id {
        Err(FactError::conflict(
            "call_producer_retired",
            "通话事实属于已退出的后端",
        ))
    } else {
        Ok(())
    }
}
fn sequence_conflict() -> FactError {
    FactError::conflict("call_sequence_conflict", "同一通话事实序号不可对应不同内容")
}
fn ack(sequence: u64) -> Value {
    json!({"ok":true,"sequence":sequence})
}

fn retire_current_owner(state: &mut SourceState) {
    state.owner_retired = true;
    state.last_closed_call = None;
}

fn apply_fact(
    state: &mut SourceState,
    fact: &Observation,
    at: OffsetDateTime,
) -> Result<Option<FrozenTerminal>, FactError> {
    if fact.action == Action::Snapshot {
        let mut normalized = fact.clone();
        normalized.action = match fact.state.as_deref() {
            Some("incoming") => {
                if state.active.is_some() {
                    Action::Number
                } else {
                    normalized.direction = Some(Direction::Incoming);
                    Action::Begin
                }
            }
            Some("dialing" | "alerting") => {
                if state.active.is_some() {
                    Action::Number
                } else {
                    normalized.direction = Some(Direction::Outgoing);
                    Action::Begin
                }
            }
            Some("active") => Action::Answered,
            Some("idle") => Action::End,
            _ => return Ok(None),
        };
        let mut terminal = apply_fact(state, &normalized, at)?;
        if let Some(terminal) = &mut terminal {
            // Idempotency is keyed by the exact admitted observation, while
            // the generated record reflects the normalized lifecycle action.
            terminal.fact = fact.clone();
        }
        return Ok(terminal);
    }
    if fact.owner_epoch < state.owner_epoch {
        return Ok(None);
    }
    if fact.owner_epoch == state.owner_epoch {
        if state.owner_id.as_deref() != Some(fact.owner_id.as_str()) {
            return Err(FactError::conflict(
                "call_owner_epoch_conflict",
                "同一来源代次不可对应不同身份",
            ));
        }
        if state.owner_retired {
            return Ok(None);
        }
    } else {
        if state.active.is_some() {
            return Err(FactError::conflict("call_owner_busy", "旧来源仍有活动通话"));
        }
        // Even idle/unknown establishes the newer service generation. A late
        // live snapshot from an older service must never create a new call.
        state.owner_id = Some(fact.owner_id.clone());
        state.owner_epoch = fact.owner_epoch;
        state.owner_retired = false;
        state.last_closed_call = None;
    }
    match fact.action {
        Action::Begin => {
            if state.active.is_none() {
                state.active = Some(new_call(fact, at)?);
            }
        }
        Action::Number => {
            if let Some(active) = &mut state.active {
                if fact.source == Source::ModuleCs || active.call_id == fact.call_id {
                    update_number(active, fact);
                }
            }
        }
        Action::Answered => {
            if let Some(active) = &mut state.active {
                if fact.source == Source::ModuleCs || active.call_id == fact.call_id {
                    active.answered = true;
                }
            }
        }
        Action::Ims if fact.ims_live() => {
            if state.last_closed_call.as_deref() == fact.call_id.as_deref() {
                return Ok(None);
            }
            if let Some(active) = &mut state.active {
                if active.call_id != fact.call_id {
                    return Err(FactError::conflict(
                        "call_id_conflict",
                        "当前活动通话与观察到的Call-ID不一致",
                    ));
                }
                update_number(active, fact);
                active.answered |= fact.state.as_deref() == Some("active");
            } else {
                state.active = Some(new_call(fact, at)?);
            }
        }
        Action::End | Action::Retire | Action::Ims if fact.terminal() => {
            if let Some(active) = &state.active {
                if fact.action != Action::Retire
                    && fact.source != Source::ModuleCs
                    && active.call_id != fact.call_id
                {
                    return Ok(None);
                }
                let mut record = json!({
                    "id":active.id,"device":fact.source.device(),"direction":active.direction,
                    "number":active.number,"answered":active.answered,
                    "started_at":active.started_at.format(&Rfc3339).map_err(FactError::storage)?,
                    "ended_at":at.format(&Rfc3339).map_err(FactError::storage)?,
                    "duration":(at-active.started_at).whole_seconds().max(0),
                });
                // Preserve the legacy optional-number contract so anonymous
                // callers retain the UI's unknown-number fallback.
                if active.number.is_empty() {
                    record
                        .as_object_mut()
                        .expect("record object")
                        .remove("number");
                }
                return Ok(Some(FrozenTerminal {
                    fact: fact.clone(),
                    record,
                    call_id: active.call_id.clone(),
                    retire_owner: fact.action == Action::Retire,
                }));
            }
            if fact.action == Action::Retire {
                retire_current_owner(state);
            }
        }
        Action::Snapshot => unreachable!("module snapshot is normalized before application"),
        _ => {} // Unknown IMS state never implies a completed call.
    }
    Ok(None)
}

fn new_call(fact: &Observation, at: OffsetDateTime) -> Result<ActiveCall, FactError> {
    let direction = fact.direction.ok_or_else(FactError::invalid)?;
    Ok(ActiveCall {
        // Length prefixes make opaque identities unambiguous even with ':'.
        id: format!(
            "cf:{}:{}:{}:{}:{}:{}",
            fact.producer_id.len(),
            fact.producer_id,
            fact.source.name(),
            fact.owner_id.len(),
            fact.owner_id,
            fact.sequence
        ),
        direction,
        number: fact.number.clone().unwrap_or_default(),
        answered: fact.action == Action::Ims && fact.state.as_deref() == Some("active"),
        started_at: at,
        call_id: fact.call_id.clone(),
    })
}
fn update_number(active: &mut ActiveCall, fact: &Observation) {
    if let Some(number) = &fact.number
        && !number.is_empty()
    {
        active.number = number.clone();
    }
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    facts: CallFacts,
    stopping: bool,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST
        || !matches!(request.uri().path(), OBSERVE_PATH | HISTORY_ERROR_PATH)
    {
        return crate::json_response(
            StatusCode::NOT_FOUND,
            &json!({"code":"route_not_found","error":"通话事实路由不存在","retryable":false}),
        );
    }
    let history_error = request.uri().path() == HISTORY_ERROR_PATH;
    let bytes = match crate::http_server::collect_body(request.into_body(), MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Err(crate::http_server::BodyReadError::TooLarge) => {
            return crate::json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                &json!({"code":"request_body_too_large","error":"通话事实请求体过大","retryable":false}),
            );
        }
        Err(crate::http_server::BodyReadError::Invalid) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"code":"invalid_call_fact","error":"读取通话事实失败: invalid request body","retryable":false}),
            );
        }
    };
    if history_error {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ErrorReport {
            producer_id: String,
            source: Source,
            error: String,
        }
        let report: ErrorReport = match serde_json::from_slice(&bytes) {
            Ok(report) => report,
            Err(_) => return FactError::invalid().response(),
        };
        if report.source == Source::ModuleCs
            || !valid_identity(&report.producer_id)
            || report.error.len() > 4096
        {
            return FactError::invalid().response();
        }
        let mut state = match state_lock(&facts.sources[report.source.index()]) {
            Ok(state) => state,
            Err(error) => return error.response(),
        };
        if state.native_claimed {
            return FactError::conflict("native_call_source_owned", "通话事实由原生服务持有")
                .response();
        }
        if state.producer_id != report.producer_id {
            return FactError::conflict("call_producer_retired", "通话事实属于已退出的后端")
                .response();
        }
        state.history_error = (!report.error.is_empty()).then_some(report.error);
        return crate::json_response(
            StatusCode::OK,
            &json!({"ok":true,"source":report.source.name()}),
        );
    }
    let fact: Observation = match serde_json::from_slice(&bytes) {
        Ok(fact) => fact,
        Err(_) => return FactError::invalid().response(),
    };
    if fact.source == Source::ModuleCs {
        return FactError::conflict("native_call_source_owned", "模块蜂窝通话事实由原生服务持有")
            .response();
    }
    let errors = facts.clone();
    let source = fact.source;
    let producer = fact.producer_id.clone();
    // The pool keeps running work alive after its HTTP waiter disappears.
    // The source gate and frozen record remain owned by that actual worker.
    match crate::archive_io::run(move || Ok(facts.observe_go(fact, stopping))).await {
        Ok(Ok(ack)) => crate::json_response(StatusCode::OK, &ack),
        Ok(Err(error)) => {
            errors.latch_error(source, &producer, error.message.clone());
            error.response()
        }
        Err(error) => {
            let error = FactError::storage(error);
            errors.latch_error(source, &producer, error.message.clone());
            error.response()
        }
    }
}

#[cfg(test)]
mod tests {
    include!("call_facts_tests.rs");
}
