//! One process-wide ordinary IMS owner. RF restoration is not retirement proof.
//! Claims never release on Drop; callers release only after actual worker joins.
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

pub(crate) const PATH: &str = "/api/internal/ordinary-vowifi/admission";
#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Native(String, u64),
    NativeModule(String, u64),
    Go(String, String, String, u64),
}
#[derive(Default)]
struct State {
    owner: Option<Arc<Claim>>,
    producer: String,
    stopping: bool,
    water: HashMap<String, u64>,
    maintenance: Option<Arc<Maintenance>>,
    maintenance_sequence: u64,
    legacy_business: Option<Arc<LegacyReaderBusiness>>,
    legacy_sequence: u64,
}
#[derive(Default)]
struct Inner {
    state: Mutex<State>,
}
#[derive(Clone, Default)]
pub(crate) struct Gate(Arc<Inner>);
pub(crate) struct Claim {
    inner: Weak<Inner>,
    key: Key,
    reader: Option<(String, u64)>,
}
pub(crate) struct LegacyReaderBusiness {
    inner: Weak<Inner>,
    sequence: u64,
    eligibility: Mutex<LegacyEligibility>,
}
struct LegacyEligibility {
    may_stop_ordinary_reader: bool,
    reader_claim: Option<Key>,
}
impl LegacyReaderBusiness {
    pub(crate) fn is_current(&self) -> bool {
        self.inner.upgrade().is_some_and(|inner| {
            inner
                .state
                .lock()
                .unwrap()
                .legacy_business
                .as_ref()
                .is_some_and(|guard| guard.sequence == self.sequence)
        })
    }
    pub(crate) fn release_confirmed(&self) -> Result<(), Failure> {
        let inner = self.inner.upgrade().ok_or_else(stale)?;
        let mut state = inner.state.lock().unwrap();
        if state
            .legacy_business
            .as_ref()
            .is_some_and(|guard| guard.sequence == self.sequence)
        {
            state.legacy_business = None;
        }
        Ok(())
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaintenancePolicy {
    // CardOnly and N-wait preserve independent module-session coexistence.
    Reject,
    // Selection and identity have the legacy global ordinary busy predicate.
    RejectAllOrdinary,
    StopAndDrain,
}
#[derive(Clone)]
pub(crate) struct GoBinding {
    pub(crate) producer: String,
    pub(crate) source: String,
    pub(crate) token: String,
    pub(crate) sequence: u64,
}

/// A maintenance capability stays owned after waiter cancellation or Drop.
/// Its holder must retain it until actual physical work and leases retire.
pub(crate) struct Maintenance {
    inner: Weak<Inner>,
    sequence: u64,
    reader: String,
    generation: u64,
    policy: MaintenancePolicy,
    captured: Option<Arc<Claim>>,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Failure {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
    pub(crate) status: StatusCode,
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for Failure {}
fn failure(code: &'static str, message: &'static str, status: StatusCode) -> Failure {
    Failure {
        code,
        message,
        status,
    }
}
fn stale() -> Failure {
    failure(
        "ordinary_vowifi_claim_stale",
        "VoWiFi 启动意图已失效",
        StatusCode::CONFLICT,
    )
}
fn valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
}
impl Key {
    fn water_key(&self) -> String {
        match self {
            Self::Native(epoch, _) => format!("n:{epoch}"),
            Self::NativeModule(epoch, _) => format!("m:{epoch}"),
            Self::Go(producer, source, _, _) => format!("g:{producer}:{source}"),
        }
    }
    fn uses_reader(&self) -> bool {
        matches!(self, Self::Native(..))
            || matches!(self, Self::Go(_, source, _, _) if source == "reader")
    }
    fn sequence(&self) -> u64 {
        match self {
            Self::Native(_, seq) | Self::NativeModule(_, seq) | Self::Go(_, _, _, seq) => *seq,
        }
    }
}
impl Gate {
    #[cfg(test)]
    pub(crate) fn poison_for_test(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _state = self.0.state.lock().unwrap();
            panic!("controlled ordinary owner lock poison");
        }));
    }
    #[cfg(test)]
    pub(crate) fn clear_test_poison(&self) {
        self.0.state.clear_poison();
    }

    pub(crate) fn has_ordinary_claim(&self) -> bool {
        self.0
            .state
            .lock()
            .map_or(true, |state| state.owner.is_some())
    }
    pub(crate) fn current_go_claim(&self) -> Result<Option<Arc<Claim>>, Failure> {
        let state = self.0.state.lock().map_err(|_| stale())?;
        Ok(state
            .owner
            .as_ref()
            .filter(|claim| matches!(claim.key, Key::Go(..)))
            .cloned())
    }
    pub(crate) fn has_go_ordinary_claim(&self) -> bool {
        self.0.state.lock().map_or(true, |state| {
            state
                .owner
                .as_ref()
                .is_some_and(|claim| matches!(claim.key, Key::Go(..)))
        })
    }

    pub(crate) fn reader_maintenance_active(&self) -> bool {
        self.0.state.lock().map_or(true, |state| {
            state.maintenance.is_some() || state.legacy_business.is_some()
        })
    }

    pub(crate) fn set_go_producer(&self, producer: &str) -> Result<(), Failure> {
        if !valid(producer) {
            return Err(stale());
        }
        let mut s = self.0.state.lock().unwrap();
        if !s.producer.is_empty() && s.producer != producer {
            return Err(stale());
        }
        s.producer = producer.into();
        Ok(())
    }
    pub(crate) fn retire_go_producer(&self, old: &str, next: &str) -> Result<(), Failure> {
        if !valid(next) || old == next {
            return Err(stale());
        }
        let mut s = self.0.state.lock().unwrap();
        if s.producer != old {
            return Err(stale());
        }
        if s.owner
            .as_ref()
            .is_some_and(|c| matches!(&c.key,Key::Go(p,_,_,_) if p==old))
        {
            s.owner = None;
        }
        s.water.retain(|k, _| !k.starts_with(&format!("g:{old}:")));
        s.producer = next.into();
        Ok(())
    }
    pub(crate) fn begin_shutdown(&self) {
        self.0.state.lock().unwrap().stopping = true;
    }
    pub(crate) fn acquire_native(&self, epoch: &str, intent: u64) -> Result<Arc<Claim>, Failure> {
        if !valid(epoch) || intent == 0 {
            return Err(stale());
        }
        self.acquire(Key::Native(epoch.into(), intent), None)
    }
    pub(crate) fn acquire_native_module(
        &self,
        epoch: &str,
        intent: u64,
    ) -> Result<Arc<Claim>, Failure> {
        if !valid(epoch) || intent == 0 {
            return Err(stale());
        }
        self.acquire(Key::NativeModule(epoch.into(), intent), None)
    }
    pub(crate) fn acquire_native_reader(
        &self,
        epoch: &str,
        intent: u64,
        reader: &str,
        generation: u64,
    ) -> Result<Arc<Claim>, Failure> {
        if !valid(epoch) || intent == 0 || !valid_reader(reader, generation) {
            return Err(stale());
        }
        self.acquire(
            Key::Native(epoch.into(), intent),
            Some((reader.into(), generation)),
        )
    }
    pub(crate) fn validate_reader_claim(
        &self,
        claim: &Arc<Claim>,
        reader: &str,
        generation: u64,
    ) -> Result<(), Failure> {
        if !claim
            .inner
            .upgrade()
            .is_some_and(|inner| Arc::ptr_eq(&inner, &self.0))
        {
            return Err(stale());
        }
        claim.validate_reader_start(reader, generation)
    }
    pub(crate) fn validate_reader_maintenance(
        &self,
        maintenance: &Arc<Maintenance>,
        reader: &str,
        generation: u64,
        policy: MaintenancePolicy,
    ) -> Result<(), Failure> {
        if !maintenance
            .inner
            .upgrade()
            .is_some_and(|inner| Arc::ptr_eq(&inner, &self.0))
            || maintenance.reader != reader
            || maintenance.generation != generation
            || maintenance.policy != policy
        {
            return Err(stale());
        }
        let state = self.0.state.lock().map_err(|_| stale())?;
        if state.stopping
            || !state
                .maintenance
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, maintenance))
        {
            return Err(stale());
        }
        if policy == MaintenancePolicy::RejectAllOrdinary && state.owner.is_some() {
            return Err(maintenance_busy());
        }
        Ok(())
    }
    pub(crate) fn begin_legacy_reader_business(
        &self,
        may_stop_ordinary_reader: bool,
    ) -> Result<Arc<LegacyReaderBusiness>, Failure> {
        let mut state = self.0.state.lock().unwrap();
        if state.stopping
            || state.maintenance.is_some()
            || state.legacy_business.is_some()
            || state
                .owner
                .as_ref()
                .is_some_and(|claim| matches!(claim.key, Key::Native(..)))
        {
            return Err(maintenance_busy());
        }
        let sequence = state.legacy_sequence.checked_add(1).ok_or_else(stale)?;
        let reader_claim = state
            .owner
            .as_ref()
            .filter(|claim| matches!(&claim.key, Key::Go(_,source,_,_) if source=="reader"))
            .map(|claim| claim.key.clone());
        let guard = Arc::new(LegacyReaderBusiness {
            inner: Arc::downgrade(&self.0),
            sequence,
            eligibility: Mutex::new(LegacyEligibility {
                may_stop_ordinary_reader,
                reader_claim,
            }),
        });
        state.legacy_sequence = sequence;
        state.legacy_business = Some(guard.clone());
        Ok(guard)
    }
    pub(crate) fn promote_legacy_reader_business(
        &self,
        legacy: &Arc<LegacyReaderBusiness>,
    ) -> Result<(), Failure> {
        let state = self.0.state.lock().unwrap();
        if !legacy
            .inner
            .upgrade()
            .is_some_and(|inner| Arc::ptr_eq(&inner, &self.0))
            || !state
                .legacy_business
                .as_ref()
                .is_some_and(|guard| Arc::ptr_eq(guard, legacy))
        {
            return Err(stale());
        }
        if state.stopping || state.maintenance.is_some() {
            return Err(maintenance_busy());
        }
        let mut eligible = legacy.eligibility.lock().unwrap();
        eligible.may_stop_ordinary_reader = true;
        eligible.reader_claim = state
            .owner
            .as_ref()
            .filter(|claim| matches!(&claim.key, Key::Go(_,source,_,_) if source=="reader"))
            .map(|claim| claim.key.clone());
        Ok(())
    }
    pub(crate) fn transfer_to_maintenance(
        &self,
        legacy: &Arc<LegacyReaderBusiness>,
        reader: &str,
        generation: u64,
    ) -> Result<Arc<Maintenance>, Failure> {
        if !valid_reader(reader, generation) {
            return Err(stale());
        }
        let mut state = self.0.state.lock().unwrap();
        if !legacy
            .inner
            .upgrade()
            .is_some_and(|inner| Arc::ptr_eq(&inner, &self.0))
            || !state
                .legacy_business
                .as_ref()
                .is_some_and(|guard| Arc::ptr_eq(guard, legacy))
        {
            return Err(stale());
        }
        if state.stopping
            || state.maintenance.is_some()
            || state
                .owner
                .as_ref()
                .is_some_and(|claim| claim.key.uses_reader())
        {
            return Err(maintenance_busy());
        }
        let sequence = state
            .maintenance_sequence
            .checked_add(1)
            .ok_or_else(stale)?;
        let maintenance = Arc::new(Maintenance {
            inner: Arc::downgrade(&self.0),
            sequence,
            reader: reader.into(),
            generation,
            policy: MaintenancePolicy::Reject,
            captured: None,
        });
        state.maintenance_sequence = sequence;
        state.maintenance = Some(maintenance.clone());
        state.legacy_business = None;
        Ok(maintenance)
    }
    pub(crate) fn begin_reader_maintenance(
        &self,
        reader: &str,
        generation: u64,
        policy: MaintenancePolicy,
    ) -> Result<Arc<Maintenance>, Failure> {
        if !valid_reader(reader, generation)
            && !(policy != MaintenancePolicy::StopAndDrain
                && valid_reader(reader, 1)
                && generation == 0)
        {
            return Err(stale());
        }
        let mut state = self.0.state.lock().unwrap();
        if state.stopping {
            return Err(failure(
                "backend_stopping",
                "Switch 正在退出，未启动读卡器操作",
                StatusCode::SERVICE_UNAVAILABLE,
            ));
        }
        if state.maintenance.is_some() {
            return Err(maintenance_busy());
        }
        let captured = state
            .owner
            .as_ref()
            .filter(|claim| {
                policy == MaintenancePolicy::RejectAllOrdinary || claim.key.uses_reader()
            })
            .cloned();
        if let Some(legacy) = &state.legacy_business {
            // Preserve stop-and-drain of the ordinary Go reader's own
            // captured business lease. Unrelated LPA/N-wait jobs never qualify.
            let eligible = legacy.eligibility.lock().unwrap();
            if policy != MaintenancePolicy::StopAndDrain
                || !eligible.may_stop_ordinary_reader
                || captured
                    .as_ref()
                    .is_none_or(|claim| eligible.reader_claim.as_ref() != Some(&claim.key))
            {
                return Err(maintenance_busy());
            }
        }
        if let Some(claim) = &captured {
            if policy != MaintenancePolicy::StopAndDrain {
                return Err(maintenance_busy());
            }
            if matches!(claim.key, Key::Native(..))
                && claim.reader.as_ref().map(|(r, g)| (r.as_str(), *g))
                    != Some((reader, generation))
            {
                return Err(stale());
            }
        }
        let sequence = state
            .maintenance_sequence
            .checked_add(1)
            .ok_or_else(stale)?;
        let maintenance = Arc::new(Maintenance {
            inner: Arc::downgrade(&self.0),
            sequence,
            reader: reader.into(),
            generation,
            policy,
            captured,
        });
        state.maintenance_sequence = sequence;
        state.maintenance = Some(maintenance.clone());
        Ok(maintenance)
    }
    pub(crate) fn acquire_go(
        &self,
        producer: &str,
        source: &str,
        token: &str,
        sequence: u64,
    ) -> Result<Arc<Claim>, Failure> {
        if !valid(producer)
            || !valid(token)
            || !matches!(source, "reader" | "module")
            || sequence == 0
        {
            return Err(stale());
        }
        self.acquire(
            Key::Go(producer.into(), source.into(), token.into(), sequence),
            None,
        )
    }
    fn acquire(&self, key: Key, reader: Option<(String, u64)>) -> Result<Arc<Claim>, Failure> {
        let mut s = self.0.state.lock().unwrap();
        if matches!(&key,Key::Go(p,_,_,_) if p!=&s.producer) {
            return Err(stale());
        }
        if s.stopping {
            return Err(failure(
                "backend_stopping",
                "Switch 正在退出，未启动 VoWiFi",
                StatusCode::SERVICE_UNAVAILABLE,
            ));
        }
        if s.maintenance.as_ref().is_some_and(|maintenance| {
            key.uses_reader() || maintenance.policy == MaintenancePolicy::RejectAllOrdinary
        }) || matches!(key, Key::Native(..)) && s.legacy_business.is_some()
        {
            return Err(maintenance_busy());
        }
        if let Some(c) = &s.owner {
            if c.key == key {
                if c.reader != reader {
                    return Err(stale());
                }
                return Ok(c.clone());
            }
            return Err(failure(
                "ordinary_vowifi_busy",
                "已有 VoWiFi 会话正在运行，请先关闭后再切换设备",
                StatusCode::CONFLICT,
            ));
        }
        if s.water
            .get(&key.water_key())
            .is_some_and(|seq| *seq >= key.sequence())
        {
            return Err(stale());
        }
        let claim = Arc::new(Claim {
            inner: Arc::downgrade(&self.0),
            key,
            reader,
        });
        s.water.insert(claim.key.water_key(), claim.key.sequence());
        s.owner = Some(claim.clone());
        Ok(claim)
    }
    fn go_status(&self, key: &Key, release: bool) -> Result<bool, Failure> {
        let mut s = self.0.state.lock().unwrap();
        if !matches!(key,Key::Go(p,_,_,_) if p==&s.producer) {
            return Err(stale());
        }
        if release {
            // Cleanup can arrive before its timed-out acquire. Retain the
            // sequence even when no owner exists, so that late acquire cannot
            // resurrect a startup that already returned without doing IO.
            let water = s.water.entry(key.water_key()).or_default();
            *water = (*water).max(key.sequence());
        }
        if s.owner.as_ref().is_some_and(|c| &c.key == key) {
            if release {
                s.owner = None;
            }
            return Ok(!release);
        }
        // A request known never to have won cannot release another owner. A
        // lost acquire ACK may safely be followed by exact release without IO.
        Ok(false)
    }
}
fn valid_reader(reader: &str, generation: u64) -> bool {
    !reader.is_empty()
        && reader.trim() == reader
        && reader.len() <= 512
        && !reader.contains('\0')
        && generation != 0
}
fn maintenance_busy() -> Failure {
    failure(
        "reader_maintenance_busy",
        "读卡器或 VoWiFi 会话正在运行或释放资源",
        StatusCode::CONFLICT,
    )
}
impl Maintenance {
    pub(crate) fn reader(&self) -> &str {
        &self.reader
    }
    pub(crate) fn reader_generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn policy(&self) -> MaintenancePolicy {
        self.policy
    }
    pub(crate) fn native_claim(&self) -> Option<Arc<Claim>> {
        self.captured
            .as_ref()
            .filter(|claim| matches!(claim.key, Key::Native(..)))
            .cloned()
    }
    pub(crate) fn is_current(&self) -> bool {
        self.inner.upgrade().is_some_and(|inner| {
            inner
                .state
                .lock()
                .unwrap()
                .maintenance
                .as_ref()
                .is_some_and(|m| m.sequence == self.sequence)
        })
    }
    pub(crate) fn ordinary_drained(&self) -> bool {
        self.inner.upgrade().is_some_and(|inner| {
            let state = inner.state.lock().unwrap();
            state
                .maintenance
                .as_ref()
                .is_some_and(|m| m.sequence == self.sequence)
                && state.legacy_business.is_none()
                && !state.owner.as_ref().is_some_and(|c| {
                    self.policy == MaintenancePolicy::RejectAllOrdinary || c.key.uses_reader()
                })
        })
    }
    /// Only used for a definite rejection before any stop/physical dispatch.
    pub(crate) fn abort_undispatched(&self) -> Result<(), Failure> {
        let inner = self.inner.upgrade().ok_or_else(stale)?;
        let mut state = inner.state.lock().unwrap();
        if state
            .maintenance
            .as_ref()
            .is_some_and(|m| m.sequence == self.sequence)
        {
            state.maintenance = None;
        }
        Ok(())
    }
    /// The caller additionally owes actual physical worker/lease release proof.
    pub(crate) fn release_confirmed(&self) -> Result<(), Failure> {
        let inner = self.inner.upgrade().ok_or_else(stale)?;
        let mut state = inner.state.lock().unwrap();
        if state
            .maintenance
            .as_ref()
            .is_some_and(|m| m.sequence == self.sequence)
        {
            if state.legacy_business.is_some()
                || state.owner.as_ref().is_some_and(|c| {
                    self.policy == MaintenancePolicy::RejectAllOrdinary || c.key.uses_reader()
                })
            {
                return Err(maintenance_busy());
            }
            state.maintenance = None;
        }
        Ok(())
    }
}
impl Claim {
    pub(crate) fn go_binding(&self) -> Option<GoBinding> {
        match &self.key {
            Key::Go(producer_id, source, token, sequence) => Some(GoBinding {
                producer: producer_id.clone(),
                source: source.clone(),
                token: token.clone(),
                sequence: *sequence,
            }),
            Key::Native(..) | Key::NativeModule(..) => None,
        }
    }
    pub(crate) fn validate_reader_start(
        &self,
        reader: &str,
        generation: u64,
    ) -> Result<(), Failure> {
        if !matches!(self.key, Key::Native(..))
            || self.reader.as_ref().map(|(r, g)| (r.as_str(), *g)) != Some((reader, generation))
        {
            return Err(stale());
        }
        let inner = self.inner.upgrade().ok_or_else(stale)?;
        let state = inner.state.lock().unwrap();
        if !state
            .owner
            .as_ref()
            .is_some_and(|c| std::ptr::eq(c.as_ref(), self))
        {
            return Err(stale());
        }
        if state.stopping || state.maintenance.is_some() || state.legacy_business.is_some() {
            return Err(maintenance_busy());
        }
        Ok(())
    }
    pub(crate) fn try_is_current(&self) -> Result<bool, Failure> {
        let Some(inner) = self.inner.upgrade() else {
            return Ok(false);
        };
        let state = inner.state.lock().map_err(|_| stale())?;
        Ok(state.owner.as_ref().is_some_and(|c| c.key == self.key))
    }
    pub(crate) fn is_current(&self) -> bool {
        // An unavailable lock cannot prove the captured owner retired.
        self.try_is_current().unwrap_or(true)
    }
    pub(crate) fn release_confirmed(&self) -> Result<(), Failure> {
        let inner = self.inner.upgrade().ok_or_else(stale)?;
        let mut s = inner.state.lock().unwrap();
        if s.owner.as_ref().is_some_and(|c| c.key == self.key) {
            s.owner = None;
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    action: String,
    producer_id: String,
    source: String,
    token: String,
    sequence: u64,
    finished: Option<bool>,
}
pub(crate) async fn handle<B>(
    request: Request<B>,
    gate: Arc<Gate>,
    stopping: bool,
) -> Response<Full<Bytes>>
where
    B: hyper::body::Body<Data = Bytes> + Send,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    if request.method() != hyper::Method::POST {
        return crate::json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            &json!({"error":"method not allowed"}),
        );
    }
    let ingress = std::pin::pin!(request.into_body());
    let data = match crate::http_server::collect_body_with_timeout(
        ingress,
        4096,
        std::time::Duration::from_secs(5),
    )
    .await
    {
        Ok(data) => data,
        _ => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"invalid admission request","code":"invalid_request"}),
            );
        }
    };
    let w: Wire = match serde_json::from_slice(&data) {
        Ok(w) => w,
        Err(_) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"invalid admission request","code":"invalid_request"}),
            );
        }
    };
    if !valid(&w.producer_id)
        || !valid(&w.token)
        || !matches!(w.source.as_str(), "reader" | "module")
        || w.sequence == 0
        || !matches!(w.action.as_str(), "acquire" | "release" | "status")
        || (w.action == "release" && w.finished != Some(true))
        || (w.action != "release" && w.finished.is_some())
    {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"invalid admission request","code":"invalid_request"}),
        );
    }
    if stopping && w.action == "acquire" {
        return crate::json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"error":"Switch 正在退出，未启动 VoWiFi","code":"backend_stopping"}),
        );
    }
    let key = Key::Go(
        w.producer_id.clone(),
        w.source.clone(),
        w.token.clone(),
        w.sequence,
    );
    let result = if w.action == "acquire" {
        gate.acquire_go(&w.producer_id, &w.source, &w.token, w.sequence)
            .map(|_| true)
    } else {
        gate.go_status(&key, w.action == "release")
    };
    match result {
        Ok(acquired) => crate::json_response(
            StatusCode::OK,
            &json!({"ok":true,"producer_id":w.producer_id,"source":w.source,"token":w.token,"sequence":w.sequence,"acquired":acquired,"released":!acquired}),
        ),
        Err(e) => crate::json_response(
            e.status,
            &json!({"error":e.message,"code":e.code,"recoverable":true}),
        ),
    }
}
#[cfg(test)]
mod tests;
