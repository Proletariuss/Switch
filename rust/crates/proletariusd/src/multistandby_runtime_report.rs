//! Native owner for the N-wait runtime journal and report projection.
//!
//! Go sends only bounded session observations over the authenticated internal
//! socket. Rust owns normalization, deduplication, atomic persistence, and the
//! public duration/recovery report while preserving the existing disk format.

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const EVENT_LIMIT: usize = 4_096;
const UNCERTAINTY_LIMIT: usize = 256;
const OBSERVATION_GAP_MS: i64 = 60_000;
const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
const DEFAULT_WINDOW_MS: i64 = 12 * 60 * 60 * 1_000;
#[cfg(test)]
const BODY_LIMIT: usize = 4_096;
const MAX_DISK_BYTES: u64 = 2 * 1024 * 1024;
const CHECKPOINT_INTERVAL_MS: i64 = 30_000;
const MAX_RECOVERY_PROFILES: usize = 2_048;
const REASONS: &[&str] = &[
    "start_requested",
    "starting",
    "registered",
    "readiness",
    "heartbeat",
    "planned_renewal",
    "planned_ike_reauth",
    "manual_retry",
    "startup_failed",
    "tunnel_lost",
    "ims_lost",
    "reader_lost",
    "manual_stop",
    "backend_exit",
    "backend_restart",
    "observation_gap",
    "clock_adjustment",
];
pub(crate) mod native;

#[cfg(test)]
mod legacy_http;
#[cfg(test)]
pub(crate) use legacy_http::{
    INTERNAL_OBSERVE_PATH, INTERNAL_REGISTRATION_SCHEDULE_PATH, handle_internal,
    handle_registration_schedule,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Event {
    at_ms: i64,
    profile_id: String,
    generation: u64,
    state: String,
    reason: String,
    #[serde(default, skip_serializing_if = "is_false")]
    recovery: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservationInput {
    profile_id: String,
    generation: u64,
    reason: String,
    desired: bool,
    tunnel_connected: bool,
    ims_registered: bool,
    messaging_ready: bool,
    sim_sms_route_ready: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryPlanRequest {
    profile_id: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryScheduleRequest {
    profile_id: String,
    owner_id: String,
    attempt_id: String,
    delay_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
struct RegistrationScheduleRequest {
    profile_id: String,
    owner_id: String,
    generation: u64,
    delay_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RetrySchedule {
    owner_id: String,
    generation: u64,
    delay_ms: u64,
    #[serde(default)]
    attempt_id: String,
    #[serde(default)]
    due_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RetryDue {
    pub(crate) profile_id: String,
    pub(crate) owner_id: String,
    pub(crate) generation: u64,
    pub(crate) attempt_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RegistrationDue {
    pub(crate) profile_id: String,
    pub(crate) owner_id: String,
    pub(crate) generation: u64,
}

#[derive(Serialize)]
struct RecoveryPlan {
    reason: String,
    delay_ms: u64,
    failures: u32,
    stable_ready_ms: u64,
    reset_ready_since: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct RecoveryState {
    failures: u32,
    stable_ready_ms: u64,
    #[serde(default)]
    ready_since_ms: Option<i64>,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Uncertainty {
    profile_id: String,
    start_ms: i64,
    end_ms: i64,
}

#[derive(Default, Deserialize, Serialize)]
struct Disk {
    #[serde(default)]
    events: Vec<Event>,
    #[serde(default)]
    observed: BTreeMap<String, Event>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    uncertain: Vec<Uncertainty>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    recovery_states: BTreeMap<String, RecoveryState>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    retry_schedules: BTreeMap<String, RetrySchedule>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    retry_generation_counters: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    registration_schedules: BTreeMap<String, RetrySchedule>,
}

#[derive(Default)]
pub(crate) struct Journal {
    path: Option<PathBuf>,
    events: Vec<Event>,
    observed: BTreeMap<String, Event>,
    uncertain: Vec<Uncertainty>,
    recovery_states: BTreeMap<String, RecoveryState>,
    retry_schedules: BTreeMap<String, RetrySchedule>,
    retry_generation_counters: BTreeMap<String, u64>,
    registration_schedules: BTreeMap<String, RetrySchedule>,
    storage_issue: String,
    last_saved_ms: i64,
}

#[derive(Serialize)]
struct ProfileReport {
    profile_id: String,
    online_ms: i64,
    not_ready_ms: i64,
    stopped_ms: i64,
    unknown_ms: i64,
    planned_renewals: u64,
    planned_ike_reauths: u64,
    unexpected_failures: u64,
    manual_retries: u64,
    slowest_recovery_ms: i64,
    state: String,
    since_ms: i64,
    last_observed_ms: i64,
}

#[derive(Serialize)]
struct Report {
    since_ms: i64,
    until_ms: i64,
    coverage_start_ms: i64,
    retention_days: u8,
    event_limit: usize,
    events: Vec<Event>,
    profiles: Vec<ProfileReport>,
    #[serde(skip_serializing_if = "String::is_empty")]
    storage_issue: String,
}

pub(crate) async fn handle<B>(
    request: Request<B>,
    store: crate::multistandby_store::Store,
    journal: Arc<Mutex<Journal>>,
) -> Response<Full<Bytes>> {
    if request.method() != Method::GET || request.uri().path() != "/api/multistandby/report" {
        return failure(StatusCode::NOT_FOUND, "route_not_found", "route not found");
    }
    let now_ms = now_ms();
    let params = query_params(request.uri().query().unwrap_or_default());
    let since_ms = match report_window(params.get("since_ms"), now_ms) {
        Ok(value) => value,
        Err(()) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"请选择最近7天以内的有效统计窗口"}),
            );
        }
    };
    let card_id = params.get("card_id").cloned().unwrap_or_default();
    let runtime_store = store.clone();
    let ids = crate::archive_io::run(move || {
        let catalog = runtime_store.runtime_catalog()?;
        Ok(profile_ids(&catalog, &card_id))
    })
    .await
    .unwrap_or_default();
    let report = journal
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .report(since_ms, now_ms, &ids);
    crate::json_response(StatusCode::OK, &report)
}

fn observation_event(input: ObservationInput, at_ms: i64) -> Option<Event> {
    if at_ms <= 0 || !valid_profile_id(&input.profile_id) || !valid_reason_candidate(&input.reason)
    {
        return None;
    };
    let reason = normalize_reason(&input.reason);
    let recovery = is_recovery_reason(&reason);
    let mut state = "not_ready";
    if !input.desired {
        state = "stopped";
    }
    if reason == "reader_lost" {
        state = "not_ready";
    }
    if reason == "backend_exit" {
        state = "unknown";
    }
    let ready = input.tunnel_connected
        && input.ims_registered
        && input.messaging_ready
        && input.sim_sms_route_ready;
    if ready && input.desired && !recovery {
        state = "online";
    }
    Some(Event {
        at_ms,
        profile_id: input.profile_id,
        generation: input.generation,
        state: state.to_owned(),
        reason,
        recovery,
    })
}

fn valid_reason_candidate(reason: &str) -> bool {
    !reason.is_empty()
        && reason.len() <= 48
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn normalize_reason(reason: &str) -> String {
    if REASONS.contains(&reason) {
        reason.to_owned()
    } else {
        "startup_failed".to_owned()
    }
}

fn is_recovery_reason(reason: &str) -> bool {
    matches!(
        reason,
        "planned_renewal"
            | "planned_ike_reauth"
            | "manual_retry"
            | "startup_failed"
            | "tunnel_lost"
            | "ims_lost"
    )
}

fn recovery_plan(
    input: RecoveryPlanRequest,
    previous: RecoveryState,
    now: i64,
    latest_observation: Option<&Event>,
) -> Option<(RecoveryPlan, RecoveryState)> {
    const STABLE_WINDOW_MS: u64 = 5 * 60 * 1_000;
    const MAX_DURATION_MS: u64 = i64::MAX as u64 / 1_000_000;
    if !valid_profile_id(&input.profile_id)
        || input.reason.is_empty()
        || input.reason.len() > 48
        || !input
            .reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        || now <= 0
    {
        return None;
    }
    let reason = normalize_reason(&input.reason);

    let observation_gap = latest_observation.is_some_and(|event| {
        event.state != "stopped"
            && (now < event.at_ms || now.saturating_sub(event.at_ms) > OBSERVATION_GAP_MS)
    });
    let ready_since_valid = previous
        .ready_since_ms
        .is_some_and(|since| !observation_gap && now >= since && since > 0);
    let reset_ready_since = observation_gap
        || (previous.ready_since_ms.is_some() && !ready_since_valid)
        || (previous.stable_ready_ms > 0 && previous.ready_since_ms.is_none());
    let elapsed = if ready_since_valid {
        now.saturating_sub(previous.ready_since_ms.expect("validated ready timestamp")) as u64
    } else {
        0
    };
    let mut stable_ready_ms = if reset_ready_since {
        0
    } else {
        previous
            .stable_ready_ms
            .saturating_add(elapsed)
            .min(MAX_DURATION_MS)
    };
    let mut failures = previous.failures;
    if stable_ready_ms >= STABLE_WINDOW_MS {
        failures = 0;
    }
    let (delay_ms, keep_stable_window) = match reason.as_str() {
        "manual_retry" => (0, false),
        "planned_renewal" | "planned_ike_reauth" => (2_000, true),
        _ => {
            failures = failures.saturating_add(1).clamp(1, 30);
            (u64::from(failures) * 2_000, false)
        }
    };
    if !keep_stable_window {
        stable_ready_ms = 0;
    }
    let ready_since_ms = if keep_stable_window && ready_since_valid {
        Some(now)
    } else {
        None
    };
    let state = RecoveryState {
        failures,
        stable_ready_ms,
        ready_since_ms,
    };
    Some((
        RecoveryPlan {
            reason,
            delay_ms,
            failures,
            stable_ready_ms,
            reset_ready_since,
        },
        state,
    ))
}

fn query_params(query: &str) -> BTreeMap<String, String> {
    let mut params = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        params
            .entry(key.into_owned())
            .or_insert_with(|| value.into_owned());
    }
    params
}

fn report_window(value: Option<&String>, now_ms: i64) -> Result<i64, ()> {
    let Some(value) = value else {
        return Ok(now_ms.saturating_sub(DEFAULT_WINDOW_MS));
    };
    let since = value.parse::<i64>().map_err(|_| ())?;
    let cut = now_ms.saturating_sub(WINDOW_MS);
    if since <= 0 || since > now_ms || since < cut.saturating_sub(60_000) {
        return Err(());
    }
    Ok(since.max(cut))
}

fn profile_ids(catalog: &Value, card_id: &str) -> Vec<String> {
    let mut ids = catalog["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|profile| card_id.is_empty() || profile["card_id"].as_str() == Some(card_id))
        .filter_map(|profile| profile["profile_id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

impl Journal {
    pub(crate) fn open(path: &Path, now_ms: i64) -> Self {
        let mut journal = Self::load(path);
        journal.path = Some(path.to_path_buf());
        if journal.storage_issue.is_empty() && path.exists() {
            for event in journal.observed.values_mut() {
                if event.state != "stopped" && event.state != "unknown" {
                    event.state = "unknown".to_owned();
                    event.reason = "backend_restart".to_owned();
                    event.recovery = false;
                    journal.events.push(event.clone());
                }
            }
            for state in journal.recovery_states.values_mut() {
                state.stable_ready_ms = 0;
            }
            journal.events.sort_by_key(|event| event.at_ms);
            journal.prune(now_ms);
            journal.save(now_ms);
            journal.last_saved_ms = now_ms;
        }
        journal
    }

    fn load(path: &Path) -> Self {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(_) => {
                return Self {
                    path: Some(path.to_path_buf()),
                    storage_issue: "runtime_history_unavailable".to_owned(),
                    ..Self::default()
                };
            }
        };
        let Ok(metadata) = file.metadata() else {
            return Self::invalid();
        };
        if !metadata.is_file() || metadata.len() > MAX_DISK_BYTES {
            return Self::invalid();
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        if file
            .take(MAX_DISK_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_DISK_BYTES
        {
            return Self::invalid();
        }
        let Ok(disk) = serde_json::from_slice::<Disk>(&bytes) else {
            return Self::invalid();
        };
        if disk.events.len() > EVENT_LIMIT
            || disk.observed.len() > 2_048
            || disk.uncertain.len() > UNCERTAINTY_LIMIT
            || disk.recovery_states.len() > MAX_RECOVERY_PROFILES
            || disk.retry_schedules.len() > MAX_RECOVERY_PROFILES
            || disk.retry_generation_counters.len() > MAX_RECOVERY_PROFILES
            || disk.registration_schedules.len() > MAX_RECOVERY_PROFILES
            || disk.events.iter().any(|event| !valid_event(event))
            || disk
                .observed
                .iter()
                .any(|(id, event)| id != &event.profile_id || !valid_event(event))
            || disk.uncertain.iter().any(|gap| {
                gap.start_ms <= 0 || gap.end_ms < gap.start_ms || !valid_profile_id(&gap.profile_id)
            })
            || disk.recovery_states.iter().any(|(id, state)| {
                !valid_profile_id(id)
                    || state.failures > 30
                    || state.stable_ready_ms > i64::MAX as u64 / 1_000_000
                    || state.ready_since_ms.is_some_and(|at| at <= 0)
            })
            || disk.retry_schedules.iter().any(|(id, schedule)| {
                !valid_profile_id(id)
                    || !valid_owner_id(&schedule.owner_id)
                    || schedule.generation == 0
                    || schedule.delay_ms > 60_000
                    || (!schedule.attempt_id.is_empty()
                        && !valid_retry_attempt_id(&schedule.attempt_id))
                    || schedule
                        .due_at_ms
                        .is_some_and(|due_at_ms| due_at_ms <= 0 || schedule.delay_ms == 0)
            })
            || disk
                .retry_generation_counters
                .iter()
                .any(|(id, generation)| !valid_profile_id(id) || *generation == 0)
            || disk.registration_schedules.iter().any(|(id, schedule)| {
                !valid_profile_id(id)
                    || !valid_owner_id(&schedule.owner_id)
                    || schedule.generation == 0
                    || schedule.delay_ms > 7 * 24 * 60 * 60 * 1_000
                    || schedule
                        .due_at_ms
                        .is_some_and(|due_at_ms| due_at_ms <= 0 || schedule.delay_ms == 0)
            })
        {
            return Self::invalid();
        }
        let mut retry_generation_counters = disk.retry_generation_counters;
        // Older journals did not retain the counter after a schedule was
        // acknowledged. Seed from any surviving schedule before accepting
        // new Rust-issued attempt generations.
        for (profile_id, schedule) in &disk.retry_schedules {
            retry_generation_counters
                .entry(profile_id.clone())
                .and_modify(|generation| *generation = (*generation).max(schedule.generation))
                .or_insert(schedule.generation);
        }
        if retry_generation_counters.len() > MAX_RECOVERY_PROFILES {
            return Self::invalid();
        }
        let mut journal = Self {
            path: Some(path.to_path_buf()),
            events: disk.events,
            observed: BTreeMap::new(),
            uncertain: disk.uncertain,
            recovery_states: disk.recovery_states,
            retry_schedules: disk.retry_schedules,
            retry_generation_counters,
            registration_schedules: disk.registration_schedules,
            storage_issue: String::new(),
            last_saved_ms: 0,
        };
        for event in &journal.events {
            journal
                .observed
                .insert(event.profile_id.clone(), event.clone());
        }
        journal.observed.extend(disk.observed);

        // The Go event producer records a backend-restart boundary when its
        // journal is opened. This handler is read-only: fabricating that
        // transition here would mark every live online profile unknown on
        // every report request. If the producer has actually stopped
        // observing, report() applies the existing observation-gap rule.
        journal.events.sort_by_key(|event| event.at_ms);
        journal
    }

    fn invalid() -> Self {
        Self {
            storage_issue: "runtime_history_invalid".to_owned(),
            ..Self::default()
        }
    }

    fn plan_recovery(
        &mut self,
        input: RecoveryPlanRequest,
        now: i64,
    ) -> Result<Option<RecoveryPlan>, ()> {
        if !self.storage_issue.is_empty() {
            return Err(());
        }
        let profile_id = input.profile_id.clone();
        let previous_exists = self.recovery_states.contains_key(&profile_id);
        let previous = self
            .recovery_states
            .get(&profile_id)
            .copied()
            .unwrap_or_default();
        if !self.recovery_states.contains_key(&profile_id)
            && self.recovery_states.len() >= MAX_RECOVERY_PROFILES
        {
            return Err(());
        }
        let latest = self.observed.get(&profile_id).cloned();
        let Some((plan, next)) = recovery_plan(input, previous, now, latest.as_ref()) else {
            return Ok(None);
        };
        self.recovery_states.insert(profile_id.clone(), next);
        if self.path.is_some() {
            self.save(now);
            if !self.storage_issue.is_empty() {
                if previous_exists {
                    self.recovery_states.insert(profile_id, previous);
                } else {
                    self.recovery_states.remove(&profile_id);
                }
                return Err(());
            }
        }
        Ok(Some(plan))
    }

    fn schedule_retry(
        &mut self,
        input: RetryScheduleRequest,
        now: i64,
    ) -> Result<Option<RetrySchedule>, ()> {
        if !self.storage_issue.is_empty() {
            return Err(());
        }
        if !valid_profile_id(&input.profile_id)
            || !valid_owner_id(&input.owner_id)
            || !valid_retry_attempt_id(&input.attempt_id)
            || input.delay_ms > 60_000
            || now <= 0
        {
            return Ok(None);
        }
        if !self
            .retry_generation_counters
            .contains_key(&input.profile_id)
            && self.retry_generation_counters.len() >= MAX_RECOVERY_PROFILES
        {
            return Err(());
        }
        if let Some(previous) = self.retry_schedules.get(&input.profile_id)
            && previous.owner_id == input.owner_id
            && previous.attempt_id == input.attempt_id
        {
            return if previous.delay_ms == input.delay_ms {
                Ok(Some(previous.clone()))
            } else {
                Err(())
            };
        }
        let generation = self
            .retry_generation_counters
            .get(&input.profile_id)
            .copied()
            .unwrap_or_default()
            .checked_add(1)
            .ok_or(())?;
        let due_at_ms = if input.delay_ms == 0 {
            None
        } else {
            Some(
                now.saturating_add(input.delay_ms as i64)
                    .saturating_add(retry_jitter_ms(&input.profile_id, generation)),
            )
        };
        let next = RetrySchedule {
            owner_id: input.owner_id.clone(),
            generation,
            delay_ms: input.delay_ms,
            attempt_id: input.attempt_id,
            due_at_ms,
        };
        let previous = self
            .retry_schedules
            .insert(input.profile_id.clone(), next.clone());
        let previous_counter = self
            .retry_generation_counters
            .insert(input.profile_id.clone(), generation);
        if self.path.is_some() {
            self.save(now);
            if !self.storage_issue.is_empty() {
                if let Some(previous) = previous {
                    self.retry_schedules
                        .insert(input.profile_id.clone(), previous);
                } else {
                    self.retry_schedules.remove(&input.profile_id);
                }
                if let Some(previous_counter) = previous_counter {
                    self.retry_generation_counters
                        .insert(input.profile_id.clone(), previous_counter);
                } else {
                    self.retry_generation_counters.remove(&input.profile_id);
                }
                return Err(());
            }
        }
        Ok(Some(next))
    }

    #[cfg(test)]
    fn schedule_registration(
        &mut self,
        input: RegistrationScheduleRequest,
        now: i64,
    ) -> Result<Option<RetrySchedule>, ()> {
        const MAX_DELAY_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
        if !self.storage_issue.is_empty() {
            return Err(());
        }
        if !valid_profile_id(&input.profile_id)
            || !valid_owner_id(&input.owner_id)
            || input.generation == 0
            || input.delay_ms == 0
            || input.delay_ms > MAX_DELAY_MS
            || now <= 0
        {
            return Ok(None);
        }
        if !self.registration_schedules.contains_key(&input.profile_id)
            && self.registration_schedules.len() >= MAX_RECOVERY_PROFILES
        {
            return Err(());
        }
        let next = RetrySchedule {
            owner_id: input.owner_id.clone(),
            generation: input.generation,
            delay_ms: input.delay_ms,
            attempt_id: String::new(),
            // Registration must renew before the negotiated lease expires. Do
            // not add reconnect jitter here: it could consume the safety margin.
            due_at_ms: Some(now.saturating_add(input.delay_ms as i64)),
        };
        if let Some(previous) = self.registration_schedules.get(&input.profile_id) {
            if previous.owner_id == input.owner_id {
                if input.generation < previous.generation {
                    return Err(());
                }
                if input.generation == previous.generation {
                    return if previous.delay_ms == input.delay_ms {
                        Ok(Some(previous.clone()))
                    } else {
                        Err(())
                    };
                }
            }
        }
        let previous = self
            .registration_schedules
            .insert(input.profile_id.clone(), next.clone());
        if self.path.is_some() {
            self.save(now);
            if !self.storage_issue.is_empty() {
                if let Some(previous) = previous {
                    self.registration_schedules
                        .insert(input.profile_id, previous);
                } else {
                    self.registration_schedules.remove(&input.profile_id);
                }
                return Err(());
            }
        }
        Ok(Some(next))
    }

    pub(crate) fn next_due_retry(&self, now: i64) -> Option<RetryDue> {
        if !self.storage_issue.is_empty() {
            return None;
        }
        self.retry_schedules
            .iter()
            .filter_map(|(profile_id, schedule)| {
                schedule
                    .due_at_ms
                    .filter(|due| *due <= now)
                    .map(|due| (due, profile_id, schedule))
            })
            .min_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)))
            .map(|(_, profile_id, schedule)| RetryDue {
                profile_id: profile_id.clone(),
                owner_id: schedule.owner_id.clone(),
                generation: schedule.generation,
                attempt_id: schedule.attempt_id.clone(),
            })
    }

    pub(crate) fn next_due_registration(&self, now: i64) -> Option<RegistrationDue> {
        if !self.storage_issue.is_empty() {
            return None;
        }
        self.registration_schedules
            .iter()
            .filter_map(|(profile_id, schedule)| {
                schedule
                    .due_at_ms
                    .filter(|due| *due <= now)
                    .map(|due| (due, profile_id, schedule))
            })
            .min_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)))
            .map(|(_, profile_id, schedule)| RegistrationDue {
                profile_id: profile_id.clone(),
                owner_id: schedule.owner_id.clone(),
                generation: schedule.generation,
            })
    }

    pub(crate) fn complete_retry(&mut self, due: &RetryDue, now: i64) -> Result<(), ()> {
        if !self.storage_issue.is_empty() {
            return Err(());
        }
        let Some(schedule) = self.retry_schedules.get(&due.profile_id) else {
            return Ok(());
        };
        if schedule.owner_id != due.owner_id
            || schedule.generation != due.generation
            || schedule.attempt_id != due.attempt_id
        {
            return Ok(());
        }
        if schedule.due_at_ms.is_none() {
            return Ok(());
        }
        let previous = schedule.clone();
        self.retry_schedules
            .get_mut(&due.profile_id)
            .expect("validated retry schedule")
            .due_at_ms = None;
        if self.path.is_some() {
            self.save(now);
            if !self.storage_issue.is_empty() {
                self.retry_schedules
                    .insert(due.profile_id.clone(), previous);
                return Err(());
            }
        }
        Ok(())
    }

    pub(crate) fn complete_registration(
        &mut self,
        due: &RegistrationDue,
        now: i64,
    ) -> Result<(), ()> {
        if !self.storage_issue.is_empty() {
            return Err(());
        }
        let Some(schedule) = self.registration_schedules.get(&due.profile_id) else {
            return Ok(());
        };
        if schedule.owner_id != due.owner_id || schedule.generation != due.generation {
            return Ok(());
        }
        if schedule.due_at_ms.is_none() {
            return Ok(());
        }
        let previous = schedule.clone();
        self.registration_schedules
            .get_mut(&due.profile_id)
            .expect("validated registration schedule")
            .due_at_ms = None;
        if self.path.is_some() {
            self.save(now);
            if !self.storage_issue.is_empty() {
                self.registration_schedules
                    .insert(due.profile_id.clone(), previous);
                return Err(());
            }
        }
        Ok(())
    }

    fn record(&mut self, event: Event) {
        self.record_at(event, now_ms());
    }

    fn record_at(&mut self, mut event: Event, now: i64) {
        if !valid_event(&event) {
            return;
        }
        let mut recovery_state_changed = false;
        if event.state == "online"
            && !self.recovery_states.contains_key(&event.profile_id)
            && self.recovery_states.len() < MAX_RECOVERY_PROFILES
        {
            self.recovery_states
                .insert(event.profile_id.clone(), RecoveryState::default());
        }
        if let Some(state) = self.recovery_states.get_mut(&event.profile_id) {
            if matches!(event.reason.as_str(), "start_requested" | "manual_stop") {
                recovery_state_changed = *state != RecoveryState::default();
                *state = RecoveryState::default();
            } else if event.state != "online"
                && !matches!(
                    event.reason.as_str(),
                    "planned_renewal" | "planned_ike_reauth" | "manual_retry"
                )
            {
                recovery_state_changed =
                    state.stable_ready_ms != 0 || state.ready_since_ms.is_some();
                state.stable_ready_ms = 0;
                state.ready_since_ms = None;
            }
        }
        let old = self.observed.get(&event.profile_id).cloned();
        let exists = old.is_some();
        let mut clock_back = false;
        let mut already_uncertain = false;
        if let Some(old) = &old
            && event.at_ms < old.at_ms
        {
            clock_back = true;
            self.add_uncertainty(Uncertainty {
                profile_id: event.profile_id.clone(),
                start_ms: event.at_ms,
                end_ms: old.at_ms,
            });
            event.at_ms = old.at_ms;
            event.state = "unknown".to_owned();
            event.reason = "clock_adjustment".to_owned();
            event.recovery = false;
            already_uncertain = old.state == "unknown"
                && old.reason == "clock_adjustment"
                && old.generation == event.generation;
        }
        let gap = old.as_ref().is_some_and(|old| {
            old.state != "stopped" && event.at_ms.saturating_sub(old.at_ms) > OBSERVATION_GAP_MS
        });
        if let Some(state) = self.recovery_states.get_mut(&event.profile_id) {
            if gap || clock_back {
                recovery_state_changed |=
                    state.stable_ready_ms != 0 || state.ready_since_ms.is_some();
                state.stable_ready_ms = 0;
                state.ready_since_ms = None;
            }
            if event.state == "online" && state.ready_since_ms.is_none() {
                state.ready_since_ms = Some(event.at_ms);
                recovery_state_changed = true;
            }
        }
        if gap {
            let mut boundary = old.clone().expect("gap requires an old observation");
            boundary.state = "unknown".to_owned();
            boundary.reason = "observation_gap".to_owned();
            boundary.recovery = false;
            self.events.push(boundary);
        }
        if !exists && self.observed.len() >= 2_048 {
            if let Some(oldest) = self
                .observed
                .iter()
                .min_by_key(|(_, event)| event.at_ms)
                .map(|(id, _)| id.clone())
            {
                self.observed.remove(&oldest);
            }
        }
        self.observed
            .insert(event.profile_id.clone(), event.clone());
        if let Some(old) = &old
            && !event.recovery
            && old.state == event.state
            && old.generation == event.generation
            && !gap
            && (!clock_back || already_uncertain)
        {
            if recovery_state_changed
                || now.saturating_sub(self.last_saved_ms) >= CHECKPOINT_INTERVAL_MS
                || now < self.last_saved_ms
            {
                self.prune(now);
                self.save(now);
                self.last_saved_ms = now;
            }
            return;
        }
        self.events.push(event.clone());
        self.events.sort_by_key(|event| event.at_ms);
        self.prune(event.at_ms);
        self.save(now);
        self.last_saved_ms = now;
    }

    fn add_uncertainty(&mut self, gap: Uncertainty) {
        if let Some(old) = self.uncertain.iter_mut().find(|old| {
            old.profile_id == gap.profile_id
                && gap.start_ms <= old.end_ms
                && gap.end_ms >= old.start_ms
        }) {
            old.start_ms = old.start_ms.min(gap.start_ms);
            old.end_ms = old.end_ms.max(gap.end_ms);
            return;
        }
        self.uncertain.push(gap);
        if self.uncertain.len() > UNCERTAINTY_LIMIT {
            let remove = self.uncertain.len() - UNCERTAINTY_LIMIT;
            self.uncertain.drain(..remove);
        }
    }

    fn prune(&mut self, now_ms: i64) {
        let cut = now_ms.saturating_sub(RETENTION_MS);
        let first_by_time = self.events.partition_point(|event| event.at_ms < cut);
        let first_by_limit = self.events.len().saturating_sub(EVENT_LIMIT);
        let first = first_by_time.max(first_by_limit);
        if first > 0 {
            self.events.drain(..first);
        }
        self.uncertain.retain(|gap| gap.end_ms >= cut);
    }

    fn save(&mut self, now_ms: i64) {
        let Some(path) = &self.path else {
            return;
        };
        let result = self.save_to(path);
        self.storage_issue = if result.is_ok() {
            String::new()
        } else {
            "runtime_history_save_failed".to_owned()
        };
        self.last_saved_ms = now_ms;
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let data = serde_json::to_vec(&Disk {
            events: self.events.clone(),
            observed: self.observed.clone(),
            uncertain: self.uncertain.clone(),
            recovery_states: self.recovery_states.clone(),
            retry_schedules: self.retry_schedules.clone(),
            retry_generation_counters: self.retry_generation_counters.clone(),
            registration_schedules: self.registration_schedules.clone(),
        })
        .map_err(std::io::Error::other)?;
        if data.len() as u64 > MAX_DISK_BYTES {
            return Err(std::io::Error::other("runtime journal exceeds size limit"));
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let nonce = crate::multistandby_store::random_token()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let temporary = parent.join(format!(".multi-runtime-{nonce}.tmp"));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            let mut file = options.open(&temporary)?;
            file.write_all(&data)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn report(&self, since_ms: i64, until_ms: i64, ids: &[String]) -> Report {
        let mut result = Report {
            since_ms,
            until_ms,
            coverage_start_ms: self.events.first().map_or(0, |event| event.at_ms),
            retention_days: (RETENTION_MS / (24 * 60 * 60 * 1_000)) as u8,
            event_limit: EVENT_LIMIT,
            events: Vec::new(),
            profiles: Vec::new(),
            storage_issue: self.storage_issue.clone(),
        };
        let wanted = ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
        let mut by_profile: BTreeMap<String, Vec<Event>> = BTreeMap::new();
        for event in &self.events {
            if !wanted.contains(event.profile_id.as_str()) || event.at_ms > until_ms {
                continue;
            }
            by_profile
                .entry(event.profile_id.clone())
                .or_default()
                .push(event.clone());
            if event.at_ms >= since_ms {
                result.events.push(event.clone());
            }
        }
        for (id, observed) in &self.observed {
            if !wanted.contains(id.as_str()) || observed.at_ms > until_ms {
                continue;
            }
            let events = by_profile.entry(id.clone()).or_default();
            if events.is_empty() || observed.at_ms > events.last().map_or(0, |event| event.at_ms) {
                let mut observed = observed.clone();
                observed.recovery = false;
                events.push(observed.clone());
            }
            if observed.state != "stopped"
                && until_ms.saturating_sub(observed.at_ms) > OBSERVATION_GAP_MS
            {
                let mut stale = observed.clone();
                stale.state = "unknown".to_owned();
                stale.reason = "observation_gap".to_owned();
                stale.recovery = false;
                events.push(stale);
            }
        }
        for id in ids {
            let events = by_profile.get(id).map(Vec::as_slice).unwrap_or_default();
            let mut profile = ProfileReport {
                profile_id: id.clone(),
                online_ms: 0,
                not_ready_ms: 0,
                stopped_ms: 0,
                unknown_ms: 0,
                planned_renewals: 0,
                planned_ike_reauths: 0,
                unexpected_failures: 0,
                manual_retries: 0,
                slowest_recovery_ms: 0,
                state: "unknown".to_owned(),
                since_ms: result.since_ms,
                last_observed_ms: 0,
            };
            let mut cursor = result.since_ms;
            let mut state = "unknown";
            let mut recovering_at = 0i64;
            for event in events {
                if event.at_ms < result.since_ms {
                    state = event.state.as_str();
                    profile.last_observed_ms = event.at_ms;
                    if event.recovery && recovering_at == 0 {
                        recovering_at = event.at_ms;
                    }
                    if matches!(event.state.as_str(), "online" | "unknown" | "stopped") {
                        recovering_at = 0;
                    }
                    continue;
                }
                add_duration(
                    &mut profile,
                    id,
                    cursor,
                    event.at_ms,
                    state,
                    &self.uncertain,
                );
                cursor = event.at_ms;
                if event.recovery
                    && uncertain_duration(
                        &self.uncertain,
                        id,
                        event.at_ms,
                        event.at_ms.saturating_add(1),
                    ) == 0
                {
                    match event.reason.as_str() {
                        "planned_renewal" => profile.planned_renewals += 1,
                        "planned_ike_reauth" => profile.planned_ike_reauths += 1,
                        "manual_retry" => profile.manual_retries += 1,
                        _ => profile.unexpected_failures += 1,
                    }
                    if recovering_at == 0 {
                        recovering_at = event.at_ms;
                    }
                }
                if event.state == "online" && recovering_at > 0 {
                    profile.slowest_recovery_ms = profile
                        .slowest_recovery_ms
                        .max(event.at_ms.saturating_sub(recovering_at));
                    recovering_at = 0;
                }
                if matches!(event.state.as_str(), "stopped" | "unknown") {
                    recovering_at = 0;
                }
                if state != event.state {
                    profile.since_ms = event.at_ms;
                }
                state = event.state.as_str();
                profile.last_observed_ms = event.at_ms;
            }
            add_duration(
                &mut profile,
                id,
                cursor,
                result.until_ms,
                state,
                &self.uncertain,
            );
            if uncertain_duration(
                &self.uncertain,
                id,
                result.until_ms.saturating_sub(1),
                result.until_ms,
            ) > 0
            {
                state = "unknown";
            }
            profile.state = state.to_owned();
            result.profiles.push(profile);
        }
        result
    }
}

// Spread profiles that fail together over a stable one-second window. The
// per-profile/generation value is deterministic so an idempotent schedule
// request always observes the same durable deadline.
fn retry_jitter_ms(profile_id: &str, generation: u64) -> i64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in profile_id.bytes().chain(generation.to_be_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % 1_000) as i64
}

fn valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_owner_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_retry_attempt_id(id: &str) -> bool {
    id.len() == 48
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_event(event: &Event) -> bool {
    event.at_ms > 0
        && valid_profile_id(&event.profile_id)
        && matches!(
            event.reason.as_str(),
            "start_requested"
                | "starting"
                | "registered"
                | "readiness"
                | "heartbeat"
                | "planned_renewal"
                | "planned_ike_reauth"
                | "manual_retry"
                | "startup_failed"
                | "tunnel_lost"
                | "ims_lost"
                | "reader_lost"
                | "manual_stop"
                | "backend_exit"
                | "backend_restart"
                | "observation_gap"
                | "clock_adjustment"
        )
        && matches!(
            event.state.as_str(),
            "online" | "not_ready" | "stopped" | "unknown"
        )
}

fn add_duration(
    report: &mut ProfileReport,
    id: &str,
    start: i64,
    end: i64,
    state: &str,
    uncertain: &[Uncertainty],
) {
    if end < start {
        return;
    }
    let mut duration = end - start;
    if state != "unknown" {
        let unknown = uncertain_duration(uncertain, id, start, end);
        duration -= unknown;
        report.unknown_ms += unknown;
    }
    match state {
        "online" => report.online_ms += duration,
        "not_ready" => report.not_ready_ms += duration,
        "stopped" => report.stopped_ms += duration,
        _ => report.unknown_ms += duration,
    }
}

fn uncertain_duration(gaps: &[Uncertainty], id: &str, start: i64, end: i64) -> i64 {
    let mut intervals = gaps
        .iter()
        .filter(|gap| gap.profile_id == id && gap.start_ms < end && gap.end_ms > start)
        .map(|gap| (gap.start_ms.max(start), gap.end_ms.min(end)))
        .collect::<Vec<_>>();
    intervals.sort_by_key(|interval| interval.0);
    let (mut total, mut through) = (0i64, i64::MIN);
    for (mut left, right) in intervals {
        if left < through {
            left = through;
        }
        if right > left {
            total = total.saturating_add(right - left);
        }
        through = through.max(right);
    }
    total
}

fn failure(status: StatusCode, code: &str, message: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":message,"code":code,"retryable":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn rust_normalizes_runtime_observation_state_and_recovery() {
        let observe = |reason: &str, desired, ready| {
            observation_event(
                ObservationInput {
                    profile_id: "profile_a".into(),
                    generation: 4,
                    reason: reason.into(),
                    desired,
                    tunnel_connected: ready,
                    ims_registered: ready,
                    messaging_ready: ready,
                    sim_sms_route_ready: ready,
                },
                now_ms(),
            )
            .unwrap()
        };
        let online = observe("readiness", true, true);
        assert_eq!(online.state, "online");
        assert!(!online.recovery);

        let stopped = observe("manual_stop", false, false);
        assert_eq!(stopped.state, "stopped");
        assert!(!stopped.recovery);

        let unknown = observe("backend_exit", true, false);
        assert_eq!(unknown.state, "unknown");

        let planned = observe("planned_ike_reauth", true, true);
        assert_eq!(planned.state, "not_ready");
        assert!(planned.recovery);

        let normalized = observe("carrier_specific_failure", true, true);
        assert_eq!(normalized.reason, "startup_failed");
        assert_eq!(normalized.state, "not_ready");
        assert!(normalized.recovery);

        assert!(
            observation_event(
                ObservationInput {
                    profile_id: "profile_a".into(),
                    generation: 4,
                    reason: "raw error text".into(),
                    desired: true,
                    tunnel_connected: false,
                    ims_registered: false,
                    messaging_ready: false,
                    sim_sms_route_ready: false,
                },
                now_ms()
            )
            .is_none()
        );
    }

    #[test]
    fn online_requires_every_profile_readiness_fact() {
        let fact_sets = [
            (true, true, true, true),
            (false, true, true, true),
            (true, false, true, true),
            (true, true, false, true),
            (true, true, true, false),
        ];
        for (index, (tunnel, ims, messaging, sim_route)) in fact_sets.into_iter().enumerate() {
            let event = observation_event(
                ObservationInput {
                    profile_id: "profile_a".into(),
                    generation: index as u64,
                    reason: "readiness".into(),
                    desired: true,
                    tunnel_connected: tunnel,
                    ims_registered: ims,
                    messaging_ready: messaging,
                    sim_sms_route_ready: sim_route,
                },
                now_ms(),
            )
            .unwrap();
            assert_eq!(event.state, if index == 0 { "online" } else { "not_ready" });
        }
    }

    #[test]
    fn recovery_policy_matches_planned_manual_and_unexpected_rules() {
        let planned = recovery_plan(
            RecoveryPlanRequest {
                profile_id: "profile_a".into(),
                reason: "planned_renewal".into(),
            },
            RecoveryState {
                failures: 20,
                stable_ready_ms: 240_000,
                ready_since_ms: Some(1_800_000_000_000),
            },
            1_800_000_060_000,
            Some(&event(1_800_000_059_000, "online", "readiness", false)),
        )
        .unwrap();
        assert_eq!(planned.0.reason, "planned_renewal");
        assert_eq!(planned.0.delay_ms, 2_000);
        assert_eq!(planned.0.failures, 0);
        assert_eq!(planned.0.stable_ready_ms, 300_000);
        assert_eq!(planned.1.failures, 0);

        let retry = recovery_plan(
            RecoveryPlanRequest {
                profile_id: "profile_a".into(),
                reason: "manual_retry".into(),
            },
            RecoveryState {
                failures: 20,
                stable_ready_ms: 0,
                ready_since_ms: None,
            },
            1_800_000_000_000,
            None,
        )
        .unwrap();
        assert_eq!(retry.0.delay_ms, 0);
        assert_eq!(retry.0.failures, 20);
        assert_eq!(retry.0.stable_ready_ms, 0);

        let unexpected = recovery_plan(
            RecoveryPlanRequest {
                profile_id: "profile_a".into(),
                reason: "ims_lost".into(),
            },
            RecoveryState::default(),
            1_800_000_000_000,
            None,
        )
        .unwrap();
        assert_eq!(unexpected.0.delay_ms, 2_000);
        assert_eq!(unexpected.0.failures, 1);

        let capped = recovery_plan(
            RecoveryPlanRequest {
                profile_id: "profile_a".into(),
                reason: "tunnel_lost".into(),
            },
            RecoveryState {
                failures: 30,
                stable_ready_ms: 0,
                ready_since_ms: None,
            },
            1_800_000_000_000,
            None,
        )
        .unwrap();
        assert_eq!(capped.0.delay_ms, 60_000);
        assert_eq!(capped.0.failures, 30);
    }

    #[test]
    fn recovery_policy_resets_stable_window_after_observation_gap() {
        let plan = recovery_plan(
            RecoveryPlanRequest {
                profile_id: "profile_a".into(),
                reason: "planned_ike_reauth".into(),
            },
            RecoveryState {
                failures: 20,
                stable_ready_ms: 299_000,
                ready_since_ms: Some(1_800_000_000_000 - 300_000),
            },
            1_800_000_060_000,
            Some(&event(
                1_800_000_000_000 - 60_001,
                "online",
                "readiness",
                false,
            )),
        )
        .unwrap();
        assert!(plan.0.reset_ready_since);
        assert_eq!(plan.0.stable_ready_ms, 0);
        assert_eq!(plan.0.failures, 20);
        assert_eq!(plan.0.delay_ms, 2_000);
    }

    #[test]
    fn legacy_recovery_state_without_rust_clock_fails_closed() {
        let legacy: RecoveryState =
            serde_json::from_str(r#"{"failures":7,"stable_ready_ms":240000}"#).unwrap();
        assert_eq!(legacy.ready_since_ms, None);
        let (plan, state) = recovery_plan(
            RecoveryPlanRequest {
                profile_id: "profile_a".into(),
                reason: "planned_renewal".into(),
            },
            legacy,
            1_800_000_060_000,
            Some(&event(1_800_000_059_000, "online", "readiness", false)),
        )
        .unwrap();
        assert_eq!(plan.stable_ready_ms, 0);
        assert!(plan.reset_ready_since);
        assert_eq!(state.ready_since_ms, None);
    }

    #[test]
    fn recovery_policy_measures_stable_online_time_from_rust_observations() {
        let start = 1_800_000_000_000;
        let mut journal = Journal::default();
        journal.recovery_states.insert(
            "profile_a".into(),
            RecoveryState {
                failures: 8,
                ..RecoveryState::default()
            },
        );
        for step in 0..=10 {
            let at = start + step * 30_000;
            journal.record_at(event(at, "online", "readiness", false), at);
        }
        let plan = journal
            .plan_recovery(
                RecoveryPlanRequest {
                    profile_id: "profile_a".into(),
                    reason: "planned_renewal".into(),
                },
                start + 301_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(plan.failures, 0);
        assert_eq!(plan.stable_ready_ms, 301_000);
        assert_eq!(
            journal.recovery_states["profile_a"].ready_since_ms,
            Some(start + 301_000)
        );
    }

    #[test]
    fn recovery_policy_normalizes_unknown_reason_and_rejects_unbounded_state() {
        let unknown = recovery_plan(
            RecoveryPlanRequest {
                profile_id: "profile_a".into(),
                reason: "carrier_specific_failure".into(),
            },
            RecoveryState::default(),
            1_800_000_000_000,
            None,
        )
        .unwrap();
        assert_eq!(unknown.0.reason, "startup_failed");
        assert_eq!(unknown.0.delay_ms, 2_000);

        for input in [
            RecoveryPlanRequest {
                profile_id: "profile_a".into(),
                reason: "raw error text".into(),
            },
            RecoveryPlanRequest {
                profile_id: "profile_a".into(),
                reason: "a".repeat(49),
            },
            RecoveryPlanRequest {
                profile_id: "".into(),
                reason: "ims_lost".into(),
            },
        ] {
            assert!(
                recovery_plan(input, RecoveryState::default(), 1_800_000_000_000, None).is_none()
            );
        }
    }

    #[test]
    fn recovery_state_is_durable_and_start_intent_resets_failure_budget() {
        let root =
            std::env::temp_dir().join(format!("switch-recovery-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("multi-runtime-events.json");
        let mut journal = Journal::open(&path, now_ms());
        let request = |reason: &str| RecoveryPlanRequest {
            profile_id: "profile_a".into(),
            reason: reason.into(),
        };
        let first = journal
            .plan_recovery(request("ims_lost"), now_ms())
            .unwrap()
            .unwrap();
        assert_eq!(first.failures, 1);
        drop(journal);

        let mut recovered = Journal::open(&path, now_ms());
        let second = recovered
            .plan_recovery(request("ims_lost"), now_ms())
            .unwrap()
            .unwrap();
        assert_eq!(second.failures, 2);
        recovered.record_at(
            event(now_ms(), "not_ready", "start_requested", false),
            now_ms(),
        );
        let restarted = recovered
            .plan_recovery(request("ims_lost"), now_ms())
            .unwrap()
            .unwrap();
        assert_eq!(restarted.failures, 1);
        drop(recovered);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retry_generation_is_allocated_and_persisted_by_rust() {
        let root =
            std::env::temp_dir().join(format!("switch-retry-schedule-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("multi-runtime-events.json");
        let mut journal = Journal::open(&path, 1_800_000_000_000);
        let scheduled = journal
            .schedule_retry(
                RetryScheduleRequest {
                    profile_id: "profile_a".into(),
                    owner_id: "go-owner-one".into(),
                    attempt_id: "a".repeat(48),
                    delay_ms: 2_000,
                },
                1_800_000_000_000,
            )
            .unwrap()
            .unwrap();
        let scheduled_due = scheduled.due_at_ms.unwrap();
        assert!((1_800_000_002_000..=1_800_000_002_999).contains(&scheduled_due));
        assert!(journal.next_due_retry(scheduled_due - 1).is_none());
        let due = journal.next_due_retry(scheduled_due).unwrap();
        assert_eq!(due.generation, 1);
        assert_eq!(due.attempt_id, "a".repeat(48));

        // Retrying an IPC request with the same identity must not move the
        // already-persisted deadline.
        let repeated = journal
            .schedule_retry(
                RetryScheduleRequest {
                    profile_id: "profile_a".into(),
                    owner_id: "go-owner-one".into(),
                    attempt_id: "a".repeat(48),
                    delay_ms: 2_000,
                },
                1_800_000_001_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(repeated.due_at_ms, scheduled.due_at_ms);
        assert!(
            journal
                .schedule_retry(
                    RetryScheduleRequest {
                        profile_id: "profile_a".into(),
                        owner_id: "go-owner-one".into(),
                        attempt_id: "a".repeat(48),
                        delay_ms: 3_000,
                    },
                    1_800_000_001_000,
                )
                .is_err()
        );

        drop(journal);
        let mut recovered = Journal::open(&path, scheduled_due);
        assert_eq!(recovered.next_due_retry(scheduled_due), Some(due.clone()));
        recovered.complete_retry(&due, scheduled_due).unwrap();
        drop(recovered);

        let mut recovered = Journal::open(&path, 1_800_000_002_100);
        assert!(recovered.next_due_retry(1_800_000_003_000).is_none());
        let canceled = recovered
            .schedule_retry(
                RetryScheduleRequest {
                    profile_id: "profile_a".into(),
                    owner_id: "go-owner-one".into(),
                    attempt_id: "b".repeat(48),
                    delay_ms: 0,
                },
                1_800_000_003_000,
            )
            .unwrap()
            .unwrap();
        assert!(canceled.due_at_ms.is_none());
        assert_eq!(canceled.generation, 2);
        let replaced = recovered
            .schedule_retry(
                RetryScheduleRequest {
                    profile_id: "profile_a".into(),
                    owner_id: "go-owner-two".into(),
                    attempt_id: "c".repeat(48),
                    delay_ms: 1_000,
                },
                1_800_000_004_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            replaced.due_at_ms,
            Some(1_800_000_005_000 + retry_jitter_ms("profile_a", 3))
        );
        assert_eq!(replaced.generation, 3);
        assert_ne!(
            retry_jitter_ms("profile_a", 3),
            retry_jitter_ms("profile_b", 3)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retry_generation_migrates_from_legacy_schedule_schema() {
        let root = std::env::temp_dir().join(format!(
            "switch-retry-schedule-legacy-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("multi-runtime-events.json");
        fs::write(
            &path,
            br#"{"retry_schedules":{"profile_a":{"owner_id":"go-owner-one","generation":9,"delay_ms":2000,"due_at_ms":1800000002000}}}"#,
        )
        .unwrap();

        let mut journal = Journal::open(&path, 1_800_000_003_000);
        assert!(journal.storage_issue.is_empty());
        let next = journal
            .schedule_retry(
                RetryScheduleRequest {
                    profile_id: "profile_a".into(),
                    owner_id: "go-owner-one".into(),
                    attempt_id: "f".repeat(48),
                    delay_ms: 2_000,
                },
                1_800_000_003_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(next.generation, 10);
        assert_eq!(next.attempt_id, "f".repeat(48));
        drop(journal);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn registration_lease_is_durable_distinct_and_never_jittered() {
        let root = std::env::temp_dir().join(format!(
            "switch-registration-schedule-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("multi-runtime-events.json");
        let mut journal = Journal::open(&path, 1_800_000_000_000);
        let input = || RegistrationScheduleRequest {
            profile_id: "profile_a".into(),
            owner_id: "go-owner-one".into(),
            generation: 8,
            delay_ms: 450_000,
        };
        let scheduled = journal
            .schedule_registration(input(), 1_800_000_000_000)
            .unwrap()
            .unwrap();
        let scheduled_due = scheduled.due_at_ms.unwrap();
        assert_eq!(scheduled_due, 1_800_000_450_000);
        assert!(journal.next_due_registration(scheduled_due - 1).is_none());
        let due = journal.next_due_registration(scheduled_due).unwrap();
        assert_eq!(due.generation, 8);

        // A REGISTER lease and a reconnect backoff for the same Profile are
        // different protocol clocks and must coexist in durable storage.
        journal
            .schedule_retry(
                RetryScheduleRequest {
                    profile_id: "profile_a".into(),
                    owner_id: "go-owner-one".into(),
                    attempt_id: "d".repeat(48),
                    delay_ms: 2_000,
                },
                1_800_000_000_000,
            )
            .unwrap();
        let retry_due = journal.next_due_retry(1_800_000_002_999).unwrap();
        assert_eq!(retry_due.generation, 1);
        assert_eq!(
            journal.next_due_registration(scheduled_due),
            Some(due.clone())
        );

        let repeated = journal
            .schedule_registration(input(), 1_800_000_100_000)
            .unwrap()
            .unwrap();
        assert_eq!(repeated.due_at_ms, scheduled.due_at_ms);
        drop(journal);

        let mut recovered = Journal::open(&path, scheduled_due);
        assert_eq!(
            recovered.next_due_registration(scheduled_due),
            Some(due.clone())
        );
        recovered
            .complete_registration(&due, scheduled_due)
            .unwrap();
        drop(recovered);

        let recovered = Journal::open(&path, scheduled_due + 1);
        assert!(
            recovered
                .next_due_registration(scheduled_due + 10_000)
                .is_none()
        );
        drop(recovered);
        fs::remove_dir_all(root).unwrap();
    }

    fn event(at_ms: i64, state: &str, reason: &str, recovery: bool) -> Event {
        Event {
            at_ms,
            profile_id: "profile_a".to_owned(),
            generation: 1,
            state: state.to_owned(),
            reason: reason.to_owned(),
            recovery,
        }
    }

    fn fixture() -> Journal {
        Journal {
            path: None,
            events: vec![
                event(1_000, "online", "registered", false),
                event(11_000, "not_ready", "planned_renewal", true),
                event(14_000, "online", "registered", false),
                event(16_000, "not_ready", "ims_lost", true),
                event(20_000, "online", "registered", false),
            ],
            observed: BTreeMap::from([(
                "profile_a".to_owned(),
                event(20_000, "online", "registered", false),
            )]),
            uncertain: Vec::new(),
            recovery_states: BTreeMap::new(),
            retry_schedules: BTreeMap::new(),
            retry_generation_counters: BTreeMap::new(),
            registration_schedules: BTreeMap::new(),
            storage_issue: String::new(),
            last_saved_ms: 0,
        }
    }

    #[test]
    fn report_preserves_event_counts_uptime_and_recovery_duration() {
        let report = fixture().report(1_000, 21_000, &["profile_a".to_owned()]);
        let profile = &report.profiles[0];
        assert_eq!(report.events.len(), 5);
        assert_eq!(profile.online_ms, 13_000);
        assert_eq!(profile.not_ready_ms, 7_000);
        assert_eq!(profile.planned_renewals, 1);
        assert_eq!(profile.unexpected_failures, 1);
        assert_eq!(profile.slowest_recovery_ms, 4_000);
        assert_eq!(profile.state, "online");
    }

    #[test]
    fn uncertain_clock_intervals_are_removed_from_known_uptime() {
        let mut journal = fixture();
        journal.uncertain.push(Uncertainty {
            profile_id: "profile_a".to_owned(),
            start_ms: 5_000,
            end_ms: 8_000,
        });
        let report = journal.report(1_000, 21_000, &["profile_a".to_owned()]);
        let profile = &report.profiles[0];
        assert_eq!(profile.online_ms, 10_000);
        assert_eq!(profile.unknown_ms, 3_000);
    }

    #[test]
    fn unknown_observations_and_stale_profiles_remain_explicit() {
        let journal = Journal {
            path: None,
            events: vec![event(1_000, "unknown", "backend_restart", false)],
            observed: BTreeMap::from([(
                "profile_a".to_owned(),
                event(1_000, "unknown", "backend_restart", false),
            )]),
            uncertain: Vec::new(),
            recovery_states: BTreeMap::new(),
            retry_schedules: BTreeMap::new(),
            retry_generation_counters: BTreeMap::new(),
            registration_schedules: BTreeMap::new(),
            storage_issue: String::new(),
            last_saved_ms: 0,
        };
        let report = journal.report(
            1_000,
            62_000,
            &["profile_a".to_owned(), "profile_b".to_owned()],
        );
        assert_eq!(report.profiles[0].state, "unknown");
        assert_eq!(report.profiles[0].unknown_ms, 61_000);
        assert_eq!(report.profiles[1].unknown_ms, 61_000);
    }

    #[test]
    fn only_valid_report_windows_are_accepted_and_small_boundary_skew_is_clamped() {
        let now = 1_800_000_000_000;
        assert_eq!(report_window(None, now).unwrap(), now - DEFAULT_WINDOW_MS);
        assert!(report_window(Some(&"bad".to_owned()), now).is_err());
        assert!(report_window(Some(&"0".to_owned()), now).is_err());
        assert!(report_window(Some(&(now + 1).to_string()), now).is_err());
        assert!(report_window(Some(&(now - WINDOW_MS - 60_001).to_string()), now).is_err());
        assert_eq!(
            report_window(Some(&(now - WINDOW_MS - 30_000).to_string()), now).unwrap(),
            now - WINDOW_MS
        );
    }

    #[test]
    fn report_profile_scope_is_sorted_and_card_filtered() {
        let catalog = json!({"profiles":[
            {"profile_id":"z","card_id":"card-b"},
            {"profile_id":"a","card_id":"card-a"},
            {"profile_id":"b","card_id":"card-b"}
        ]});
        assert_eq!(profile_ids(&catalog, "card-b"), vec!["b", "z"]);
        assert_eq!(profile_ids(&catalog, ""), vec!["a", "b", "z"]);
    }

    #[test]
    fn invalid_legacy_journal_is_reported_without_exposing_its_contents() {
        let root =
            std::env::temp_dir().join(format!("switch-runtime-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("multi-runtime-events.json");
        std::fs::write(
            &path,
            br#"{"events":[{"profile_id":"bad/secret","state":"online"}]}"#,
        )
        .unwrap();
        let journal = Journal::load(&path);
        assert_eq!(journal.storage_issue, "runtime_history_invalid");
        let value = serde_json::to_value(journal.report(1, 2, &["bad/secret".to_owned()])).unwrap();
        assert!(value["events"].as_array().unwrap().is_empty());
        assert_eq!(value["storage_issue"], "runtime_history_invalid");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn valid_go_legacy_journal_round_trips_into_native_projection() {
        let root =
            std::env::temp_dir().join(format!("switch-runtime-compat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("multi-runtime-events.json");
        let online = serde_json::to_value(event(10_000, "online", "readiness", false)).unwrap();
        let offline =
            serde_json::to_value(event(15_000, "not_ready", "planned_renewal", true)).unwrap();
        std::fs::write(
            &path,
            json!({"events":[online,offline],"observed":{},"uncertain":[]}).to_string(),
        )
        .unwrap();
        let journal = Journal::load(&path);
        let report = journal.report(10_000, 20_000, &["profile_a".to_owned()]);
        assert!(report.storage_issue.is_empty());
        assert_eq!(report.events.len(), 2);
        assert_eq!(report.profiles[0].online_ms, 5_000);
        assert_eq!(report.profiles[0].not_ready_ms, 5_000);
        assert_eq!(report.profiles[0].planned_renewals, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_only_report_does_not_turn_a_recent_online_checkpoint_into_restart() {
        let root = std::env::temp_dir().join(format!("switch-runtime-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("multi-runtime-events.json");
        let online = event(10_000, "online", "heartbeat", false);
        std::fs::write(
            &path,
            json!({"events":[online.clone()],"observed":{"profile_a":online},"uncertain":[]})
                .to_string(),
        )
        .unwrap();
        let journal = Journal::load(&path);
        let report = journal.report(10_000, 20_000, &["profile_a".to_owned()]);
        assert_eq!(report.events.len(), 1);
        assert_eq!(report.profiles[0].state, "online");
        assert_eq!(report.profiles[0].online_ms, 10_000);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn public_handler_rejects_invalid_window_and_reads_native_journal() {
        let root =
            std::env::temp_dir().join(format!("switch-report-handler-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let store = crate::multistandby_store::Store::open(&root).unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/multistandby/report?since_ms=invalid")
            .body(())
            .unwrap();
        let journal = Arc::new(Mutex::new(Journal::default()));
        let response = handle(request, store.clone(), journal.clone()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        journal
            .lock()
            .unwrap()
            .record(event(now_ms(), "online", "registered", false));
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/multistandby/report")
            .body(())
            .unwrap();
        let response = handle(request, store, journal).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let report: Value = serde_json::from_slice(&body).unwrap();
        assert!(report["events"].as_array().unwrap().is_empty());
        assert!(report["profiles"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn internal_observation_is_validated_persisted_and_recovered_as_unknown() {
        let root =
            std::env::temp_dir().join(format!("switch-runtime-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("multi-runtime-events.json");
        let journal = Arc::new(Mutex::new(Journal::open(&path, now_ms())));
        let body = serde_json::to_vec(&json!({
            "profile_id": "profile_a",
            "generation": 1,
            "reason": "readiness",
            "desired": true,
            "tunnel_connected": true,
            "ims_registered": true,
            "messaging_ready": true,
            "sim_sms_route_ready": true
        }))
        .unwrap();
        let request = Request::builder()
            .method(Method::POST)
            .uri(INTERNAL_OBSERVE_PATH)
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let response = handle_internal(request, journal).await;
        assert_eq!(response.status(), StatusCode::OK);
        let permissions = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(permissions, 0o600);

        let restarted = Journal::open(&path, now_ms());
        assert_eq!(restarted.observed["profile_a"].state, "unknown");
        assert_eq!(restarted.observed["profile_a"].reason, "backend_restart");
        assert_eq!(restarted.events.last().unwrap().reason, "backend_restart");

        let invalid = Request::builder()
            .method(Method::POST)
            .uri(INTERNAL_OBSERVE_PATH)
            .body(Full::new(Bytes::from_static(
                br#"{"profile_id":"bad/id","generation":1,"reason":"readiness","desired":true,"tunnel_connected":false,"ims_registered":false,"messaging_ready":false,"sim_sms_route_ready":false}"#,
            )))
            .unwrap();
        let response = handle_internal(invalid, Arc::new(Mutex::new(Journal::default()))).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn journal_deduplicates_steady_observations_and_marks_long_gaps_unknown() {
        let mut journal = Journal::default();
        let initial = event(1_000, "online", "readiness", false);
        journal.record_at(initial.clone(), 1_000);
        let mut checkpoint = initial.clone();
        checkpoint.at_ms = 30_000;
        checkpoint.reason = "heartbeat".to_owned();
        journal.record_at(checkpoint, 30_000);
        assert_eq!(journal.events.len(), 1);
        assert_eq!(journal.observed["profile_a"].at_ms, 30_000);

        let mut after_gap = initial;
        after_gap.at_ms = 91_000;
        after_gap.reason = "readiness".to_owned();
        journal.record_at(after_gap, 91_000);
        assert!(
            journal
                .events
                .iter()
                .any(|event| event.reason == "observation_gap")
        );
        assert_eq!(journal.events.len(), 3);
    }

    #[test]
    fn backward_wall_clock_adjustment_is_persistently_reported_as_unknown() {
        let mut journal = Journal::default();
        let online = event(10_000, "online", "readiness", false);
        journal.record_at(online.clone(), 10_000);
        let mut checkpoint = online.clone();
        checkpoint.at_ms = 40_000;
        checkpoint.reason = "heartbeat".to_owned();
        journal.record_at(checkpoint, 40_000);
        let mut corrected = online.clone();
        corrected.at_ms = 20_000;
        journal.record_at(corrected.clone(), 20_000);
        let mut report = journal.report(10_000, 50_000, &["profile_a".to_owned()]);
        assert_eq!(report.profiles[0].online_ms, 10_000);
        assert_eq!(report.profiles[0].unknown_ms, 30_000);
        assert_eq!(report.profiles[0].state, "unknown");

        corrected.at_ms = 41_000;
        journal.record_at(corrected, 41_000);
        report = journal.report(10_000, 42_000, &["profile_a".to_owned()]);
        assert_eq!(report.profiles[0].online_ms, 11_000);
        assert_eq!(report.profiles[0].unknown_ms, 21_000);
        assert_eq!(report.profiles[0].state, "online");
    }
}
