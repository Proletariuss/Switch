//! The module eSIM authority and complete physical transaction boundary.
//! Serial snapshots are scoped to one actor instance, physical session and SIM
//! generation. SMS initialization/readiness is deliberately not an eUICC gate.
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use hyper::StatusCode;
use serde_json::Value;

use crate::modem_actor::SerialCardBinding;
use crate::module_esim_channel::AtCommandTransport;
use crate::{ServerState, reader_esim_download, usb_at};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotKey {
    Usb(usb_at::EsimSnapshotKey),
    Serial {
        binding: SerialCardBinding,
        revision: u64,
    },
}
#[derive(Clone)]
pub(crate) enum RefreshBinding {
    Usb(crate::usb_at::EsimRefreshBinding),
    Serial(SerialCardBinding),
}
pub(crate) fn capture_refresh_binding(state: &ServerState) -> Option<RefreshBinding> {
    if state.modem_actor.native_esim_managed() {
        capture(state).ok().map(RefreshBinding::Serial)
    } else {
        state
            .usb_at
            .capture_esim_refresh_binding()
            .map(RefreshBinding::Usb)
    }
}
pub(crate) fn refresh_binding_current(state: &ServerState, binding: &RefreshBinding) -> bool {
    match binding {
        RefreshBinding::Usb(binding) => {
            !state.modem_actor.native_esim_managed()
                && state.usb_at.esim_refresh_binding_current(binding)
        }
        RefreshBinding::Serial(binding) => binding_current(state, binding),
    }
}

#[derive(Clone)]
struct Entry {
    key: SnapshotKey,
    overview: Value,
}
#[derive(Default)]
struct CacheState {
    revision: u64,
    live: Option<Entry>,
    last_good: Option<Entry>,
}
impl CacheState {
    fn invalidate(&mut self, keep_last_good: bool) {
        self.revision = self.revision.saturating_add(1);
        self.live = None;
        if !keep_last_good {
            self.last_good = None;
        }
    }
}

#[derive(Default)]
pub(crate) struct SerialCache {
    state: Mutex<CacheState>,
    gate: Arc<tokio::sync::Mutex<()>>,
}

// Both variants retain their lock until the projection finishes publishing.
#[allow(dead_code)]
pub(crate) enum SnapshotGuard<'a> {
    Usb(tokio::sync::MutexGuard<'a, ()>),
    Serial(tokio::sync::OwnedMutexGuard<()>),
}

pub(crate) fn native(state: &ServerState) -> bool {
    state.modem_actor.native_esim_managed()
        || crate::module_discovery::uses_native_usb(&state.module_radio, &state.usb_at)
}
pub(crate) fn native_writer(state: &ServerState) -> bool {
    state.modem_actor.native_esim_managed() || state.usb_at.native_managed()
}
fn stale() -> usb_at::Failure {
    usb_at::Failure::new(
        StatusCode::CONFLICT,
        "serial_esim_session_stale",
        "模块物理会话已变化，请刷新后重试",
    )
}
fn capture(state: &ServerState) -> Result<SerialCardBinding, usb_at::Failure> {
    let session = state
        .module_radio
        .serial_transport_session()
        .ok_or_else(stale)?;
    let binding = state
        .modem_actor
        .capture_serial_card_binding()
        .map_err(|_| stale())?;
    if binding.serial_session != session || !binding_current(state, &binding) {
        return Err(stale());
    }
    Ok(binding)
}
fn binding_current(state: &ServerState, binding: &SerialCardBinding) -> bool {
    state.modem_actor.native_esim_managed()
        && state.module_radio.serial_transport_session() == Some(binding.serial_session)
        && (!state.modem_actor.native_serial_managed()
            || state
                .module_radio
                .native_serial_bootstrapped(binding.serial_session))
        && state.modem_actor.serial_card_binding_current(binding)
        && state.module_radio.serial_transport_session() == Some(binding.serial_session)
}
pub(crate) fn serial_route_available(state: &ServerState) -> bool {
    state.modem_actor.native_esim_managed() && capture(state).is_ok()
}
pub(crate) async fn snapshot_guard(state: &ServerState) -> SnapshotGuard<'_> {
    if state.modem_actor.native_esim_managed() {
        SnapshotGuard::Serial(
            state
                .modem_actor
                .module_esim_cache()
                .gate
                .clone()
                .lock_owned()
                .await,
        )
    } else {
        SnapshotGuard::Usb(state.usb_at.esim_snapshot_guard().await)
    }
}
pub(crate) fn snapshot_current(state: &ServerState, key: &SnapshotKey) -> bool {
    match key {
        SnapshotKey::Usb(key) => {
            !state.modem_actor.native_esim_managed() && state.usb_at.esim_snapshot_current(key)
        }
        SnapshotKey::Serial { binding, revision } => {
            if !binding_current(state, binding) {
                return false;
            }
            let cache = state.modem_actor.module_esim_cache();
            let revision_current = {
                let cached = cache.state.lock().unwrap_or_else(|e| e.into_inner());
                *revision != u64::MAX && cached.revision == *revision
            };
            revision_current && binding_current(state, binding)
        }
    }
}
pub(crate) fn cached_overview(state: &ServerState, stale: bool) -> Option<(SnapshotKey, Value)> {
    if !state.modem_actor.native_esim_managed() {
        return state
            .usb_at
            .cached_esim_overview(stale)
            .map(|(key, value)| (SnapshotKey::Usb(key), value));
    }
    let cache = state.modem_actor.module_esim_cache();
    let entry = {
        let cached = cache.state.lock().unwrap_or_else(|e| e.into_inner());
        if stale {
            cached.last_good.clone()
        } else {
            cached.live.clone()
        }
    }?;
    let current = match &entry.key {
        SnapshotKey::Serial { binding, .. } if stale => binding_current(state, binding),
        _ => snapshot_current(state, &entry.key),
    };
    current.then_some((entry.key, entry.overview))
}
pub(crate) fn invalidate(state: &ServerState) {
    if state.modem_actor.native_esim_managed() {
        state
            .modem_actor
            .module_esim_cache()
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .invalidate(false);
    } else {
        state.usb_at.invalidate_esim_overview();
    }
}
pub(crate) async fn overview(
    state: &ServerState,
    refresh: bool,
) -> Result<(SnapshotKey, Value), usb_at::Failure> {
    if !state.modem_actor.native_esim_managed() {
        let (key, value) = state
            .usb_at
            .native_esim_overview(refresh)
            .await
            .map(|(key, value)| (SnapshotKey::Usb(key), value))?;
        if !snapshot_current(state, &key) {
            return Err(stale());
        }
        return Ok((key, value));
    }
    if !refresh && let Some(cached) = cached_overview(state, false) {
        return Ok(cached);
    }
    serial_overview(state, capture(state)?, refresh).await
}
async fn serial_overview(
    state: &ServerState,
    binding: SerialCardBinding,
    refresh: bool,
) -> Result<(SnapshotKey, Value), usb_at::Failure> {
    if !binding_current(state, &binding) {
        return Err(stale());
    }
    let cache = state.modem_actor.module_esim_cache();
    let revision = {
        let mut cached = cache.state.lock().unwrap_or_else(|e| e.into_inner());
        if refresh {
            cached.invalidate(true);
        }
        if cached.revision == u64::MAX {
            return Err(stale());
        }
        cached.revision
    };
    let key = SnapshotKey::Serial {
        binding: binding.clone(),
        revision,
    };
    let read = state
        .modem_actor
        .esim_transaction(
            binding,
            false,
            crate::module_esim::read_overview_with_identities,
        )
        .await?;
    if !snapshot_current(state, &key) {
        return Err(stale());
    }
    let value = read.value;
    let identities = read.complete_card_identities;
    let commit_state = state.clone();
    let commit_key = key.clone();
    let commit_value = value.clone();
    serial_journal_pool()
        .run(move || {
            commit_serial_overview(
                &commit_state,
                &commit_key,
                &commit_value,
                &identities,
                refresh,
            )
        })
        .await
        .map_err(|_| recovery_journal_failure())??;
    if !snapshot_current(state, &key) {
        return Err(stale());
    }
    Ok((key, value))
}

fn serial_journal_pool() -> &'static crate::blocking_pool::BlockingPool {
    static POOL: OnceLock<crate::blocking_pool::BlockingPool> = OnceLock::new();
    POOL.get_or_init(|| crate::blocking_pool::BlockingPool::new(1))
}

/// Lock order is radio route -> actor card publication -> overview revision ->
/// recovery service journal. Neither this callback nor its callees wait on an
/// actor RPC or an async task. A stale A overview cannot commit after B has
/// replaced its SIM generation, physical session, route, or cache revision.
fn commit_serial_overview(
    state: &ServerState,
    key: &SnapshotKey,
    value: &Value,
    identities: &[(Vec<u8>, Vec<u8>)],
    refresh: bool,
) -> Result<(), usb_at::Failure> {
    let SnapshotKey::Serial { binding, revision } = key else {
        return Err(stale());
    };
    let cache = state.modem_actor.module_esim_cache();
    state
        .module_radio
        .with_current_serial_transport(binding.serial_session, || {
            state
                .modem_actor
                .with_current_serial_card_binding(binding, || {
                    let mut cached = cache.state.lock().unwrap_or_else(|e| e.into_inner());
                    if *revision == u64::MAX || cached.revision != *revision {
                        return Err(stale());
                    }
                    if refresh
                        && !reader_esim_download::reconcile_module_overview_at_commit(
                            value, identities,
                        )
                    {
                        return Err(recovery_journal_failure());
                    }
                    let entry = Entry {
                        key: key.clone(),
                        overview: value.clone(),
                    };
                    cached.live = Some(entry.clone());
                    cached.last_good = Some(entry);
                    Ok(())
                })
        })
        .ok_or_else(stale)?
        .ok_or_else(stale)??;
    Ok(())
}

fn recovery_journal_failure() -> usb_at::Failure {
    usb_at::Failure::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "esim_operation_journal_failed",
        "卡片信息已读取，但无法安全保存上次操作的核对状态",
    )
}

pub(crate) async fn transaction_fenced<T, F>(
    state: &ServerState,
    fence: &MutationFence,
    operation: F,
) -> Result<T, usb_at::Failure>
where
    T: Send + 'static,
    F: for<'a> FnOnce(
            &'a mut dyn AtCommandTransport<Error = usb_at::Failure>,
        ) -> Result<T, usb_at::Failure>
        + Send
        + 'static,
{
    match fence {
        MutationFence::Usb if fence.current(state) => {
            state.usb_at.esim_transaction(operation).await
        }
        MutationFence::Serial(Some(binding)) if binding_current(state, binding) => {
            invalidate(state);
            let result = state
                .modem_actor
                .esim_transaction(binding.clone(), true, operation)
                .await;
            if !binding_current(state, binding) {
                return Err(stale());
            }
            result
        }
        _ => Err(stale()),
    }
}

// Switch recovery may observe a deliberately changed SIM, but it cannot
// follow a replacement physical session or lend that new key to a write.
pub(crate) async fn observe_same_transport<T, F>(
    state: &ServerState,
    fence: &MutationFence,
    operation: F,
) -> Result<T, usb_at::Failure>
where
    T: Send + 'static,
    F: for<'a> FnOnce(
            &'a mut dyn AtCommandTransport<Error = usb_at::Failure>,
        ) -> Result<T, usb_at::Failure>
        + Send
        + 'static,
{
    match fence {
        MutationFence::Usb if fence.current(state) => {
            state.usb_at.esim_transaction(operation).await
        }
        MutationFence::Serial(Some(old)) => {
            let binding = capture(state)?;
            if binding.owner_epoch != old.owner_epoch
                || binding.serial_session != old.serial_session
            {
                return Err(stale());
            }
            let result = state
                .modem_actor
                .esim_transaction(binding.clone(), false, operation)
                .await;
            if !binding_current(state, &binding) {
                return Err(stale());
            }
            result
        }
        _ => Err(stale()),
    }
}

pub(crate) async fn health_status(
    state: &ServerState,
    key: &SnapshotKey,
) -> Result<Value, usb_at::Failure> {
    match key {
        SnapshotKey::Usb(key) if !state.modem_actor.native_esim_managed() => {
            state.usb_at.native_esim_health_status(key).await
        }
        SnapshotKey::Serial { binding, .. } => {
            if !snapshot_current(state, key) {
                return Err(stale());
            }
            let suppressed = state.modem_actor.cellular_suppressed();
            let observed = state
                .modem_actor
                .serial_sms_observation()
                .map_err(|_| stale())?;
            if observed.pin_blocked {
                return Err(usb_at::Failure::new(
                    StatusCode::CONFLICT,
                    "serial_sim_state_unconfirmed",
                    "SIM 解锁状态未确认，请刷新模块状态",
                ));
            }
            let sim_presence = observed.sim_present;
            let value = state
                .modem_actor
                .esim_transaction(binding.clone(), false, move |transport| {
                    let iccid = transport.command("AT+QCCID", Duration::from_secs(3), 4096)?;
                    let imsi = transport.command("AT+CIMI", Duration::from_secs(3), 4096)?;
                    let cfun = transport.command("AT+CFUN?", Duration::from_secs(3), 4096)?;
                    let suppressed = suppressed || flight_mode(&cfun);
                    let (csq, creg, cops, qnwinfo) = if suppressed {
                        (String::new(), String::new(), String::new(), String::new())
                    } else {
                        (
                            transport.command("AT+CSQ", Duration::from_secs(3), 4096)?,
                            transport.command("AT+CREG?", Duration::from_secs(3), 4096)?,
                            transport.command("AT+COPS?", Duration::from_secs(3), 4096)?,
                            transport.command("AT+QNWINFO", Duration::from_secs(3), 4096)?,
                        )
                    };
                    let mut value = crate::modem_actor::parse_serial_esim_health_responses(
                        &iccid,
                        &imsi,
                        &csq,
                        &creg,
                        &cops,
                        &qnwinfo,
                        suppressed,
                        sim_presence == Some(true),
                    );
                    if sim_presence.is_none() {
                        // Successful identities from this fenced physical session
                        // prove presence without manufacturing SMS readiness. A
                        // missing observation is never projected as confirmed absence.
                        if !identity_proves_presence(&value) {
                            return Err(usb_at::Failure::new(
                                StatusCode::CONFLICT,
                                "serial_sim_state_unconfirmed",
                                "SIM 身份状态未确认，请刷新模块状态",
                            ));
                        }
                        value["sim_inserted"] = Value::Bool(true);
                    }
                    Ok(value)
                })
                .await?;
            if !snapshot_current(state, key)
                || state.modem_actor.cellular_suppressed() != suppressed
            {
                return Err(stale());
            }
            Ok(value)
        }
        _ => Err(stale()),
    }
}

fn identity_proves_presence(value: &Value) -> bool {
    let iccid = value["iccid"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .trim_end_matches(['f', 'F']);
    let imsi = value["imsi"].as_str().unwrap_or_default().trim();
    (18..=22).contains(&iccid.len())
        && iccid.bytes().all(|b| b.is_ascii_digit())
        && (14..=15).contains(&imsi.len())
        && imsi.bytes().all(|b| b.is_ascii_digit())
}

// Preserve the established CFUN=4 radio suppression rule. Duplicate or
// malformed observations do not manufacture a confirmed flight-mode state.
fn flight_mode(response: &str) -> bool {
    let mut values = response
        .lines()
        .filter_map(|line| line.trim().strip_prefix("+CFUN:"))
        .map(str::trim);
    let first = values.next();
    values.next().is_none() && first.and_then(|value| value.parse::<u32>().ok()) == Some(4)
}

pub(crate) enum MutationFence {
    Usb,
    Serial(Option<SerialCardBinding>),
}
pub(crate) fn mutation_fence(state: &ServerState) -> MutationFence {
    if state.modem_actor.native_esim_managed() {
        MutationFence::Serial(capture(state).ok())
    } else {
        MutationFence::Usb
    }
}

/// Stable physical owner used by the durable public-mutation ledger before
/// any RF reservation or APDU can be submitted.
pub(crate) fn mutation_identity(state: &ServerState) -> Option<(String, String, u64)> {
    if state.modem_actor.native_esim_managed() {
        capture(state).ok().map(|binding| {
            (
                format!("serial:{}:{}", binding.owner_epoch, binding.serial_session),
                format!("serial-card:{}", binding.serial_session),
                binding.sim_generation,
            )
        })
    } else {
        state.usb_at.esim_mutation_identity()
    }
}
impl MutationFence {
    pub(crate) fn current(&self, state: &ServerState) -> bool {
        match self {
            Self::Usb => !state.modem_actor.native_esim_managed(),
            Self::Serial(Some(binding)) => binding_current(state, binding),
            Self::Serial(None) => false,
        }
    }
    // Switching may deliberately produce a new SIM generation. Only the same
    // physical actor/session can receive its later identity observation.
    pub(crate) fn same_transport(&self, state: &ServerState) -> bool {
        match self {
            Self::Usb => !state.modem_actor.native_esim_managed(),
            Self::Serial(Some(old)) => capture(state).is_ok_and(|new| {
                old.owner_epoch == new.owner_epoch && old.serial_session == new.serial_session
            }),
            Self::Serial(None) => false,
        }
    }
}

// Complete the cache refresh inside the original write reservation. The caller
// keeps its operation gate active through this read and physical RF release, so
// a terminal operation never schedules another reservation behind the caller.
pub(crate) async fn refresh_reserved(
    state: &ServerState,
    fence: &MutationFence,
    _reservation: &crate::module_radio::EsimReservation,
) -> Option<SnapshotKey> {
    let _single = snapshot_guard(state).await;
    if !fence.current(state) {
        return None;
    }
    let observation = match fence {
        MutationFence::Usb => state
            .usb_at
            .native_esim_overview(true)
            .await
            .map(|(key, value)| (SnapshotKey::Usb(key), value)),
        MutationFence::Serial(Some(binding)) => serial_overview(state, binding.clone(), true).await,
        MutationFence::Serial(None) => return None,
    };
    observation
        .ok()
        .map(|(key, _)| key)
        .filter(|key| snapshot_current(state, key))
}

pub(crate) fn discard_unreleased_refresh(state: &ServerState, key: Option<&SnapshotKey>) {
    if key.is_some_and(|key| snapshot_current(state, key)) {
        invalidate(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serial_cache_refresh_retains_last_good_but_mutation_revokes_it() {
        let key = SnapshotKey::Serial {
            binding: SerialCardBinding {
                owner_epoch: "owner".into(),
                serial_session: 1,
                sim_generation: 0,
            },
            revision: 0,
        };
        let entry = Entry {
            key,
            overview: serde_json::json!({"profiles":[]}),
        };
        let mut cache = CacheState {
            revision: 0,
            live: Some(entry.clone()),
            last_good: Some(entry),
        };
        cache.invalidate(true);
        assert!(cache.live.is_none());
        assert!(cache.last_good.is_some());
        assert_eq!(cache.revision, 1);
        cache.invalidate(false);
        assert!(cache.last_good.is_none());
        cache.revision = u64::MAX;
        cache.invalidate(false);
        assert_eq!(cache.revision, u64::MAX);
    }
    #[test]
    fn identity_presence_requires_both_valid_card_identifiers() {
        assert!(identity_proves_presence(
            &serde_json::json!({"iccid":"8986012345678901234F", "imsi":"460001234567890"})
        ));
        assert!(!identity_proves_presence(
            &serde_json::json!({"iccid":"8986012345678901234F", "imsi":""})
        ));
        assert!(!identity_proves_presence(
            &serde_json::json!({"iccid":"ERROR", "imsi":"460001234567890"})
        ));
    }
    #[test]
    fn flight_mode_uses_one_unambiguous_cfun_observation() {
        assert!(flight_mode("\r\n+CFUN: 04\r\nOK\r\n"));
        assert!(!flight_mode("+CFUN: 1\nOK"));
        assert!(!flight_mode("+CFUN: 4\n+CFUN: 1\nOK"));
        assert!(!flight_mode("ERROR"));
    }
}
