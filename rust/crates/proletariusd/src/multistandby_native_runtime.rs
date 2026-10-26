//! Retained native N-wait manager. Durable desired and SMS claims stay in the
//! existing N-wait store; network generations and short card work stay in Rust.
use crate::{
    multistandby_native_card::{Pool, Profile},
    multistandby_runtime_report::{Journal, RegistrationDue, RetryDue},
    native_vowifi_session::{
        PlanRequest,
        multi::{NativeMultiSession, Runtime},
    },
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
mod lifecycle;
mod projection;
mod sending;
mod worker;
pub(crate) type Failure = &'static str;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DueDelivery {
    Accepted,
    Stale,
    Busy,
}
#[derive(Clone)]
pub(crate) struct ProfileSpec {
    pub(crate) profile_id: String,
    pub(crate) card_id: String,
    pub(crate) eid: String,
    pub(crate) iccid: String,
    pub(crate) aid: String,
    pub(crate) display_name: String,
    pub(crate) phone_number: String,
    pub(crate) physical_enabled: bool,
}
#[derive(Clone)]
pub(crate) struct Inventory {
    pub(crate) reader: String,
    pub(crate) generation: u64,
    pub(crate) profiles: Vec<ProfileSpec>,
}
#[derive(Clone, Default)]
pub(crate) struct ConnectionOptions {
    pub(crate) proxy: String,
    pub(crate) epdg: String,
    pub(crate) epdg_port: Option<u16>,
}
pub(crate) struct Resources {
    pub(crate) owner: crate::reader_pcsc::Owner,
    pub(crate) gate: Arc<crate::reader_reservation::Gate>,
    pub(crate) card_service: crate::multistandby_card::Service,
    pub(crate) data_root: PathBuf,
    pub(crate) store: crate::multistandby_store::Store,
    pub(crate) journal: Arc<Mutex<Journal>>,
    pub(crate) transactions: crate::ims_sms_transactions::Service,
    pub(crate) connection: ConnectionOptions,
}
#[derive(Clone, Copy)]
pub(crate) enum Action {
    Start,
    Stop,
    Retry,
}
pub(crate) struct Ticket(oneshot::Receiver<Result<Value, Failure>>);
impl Ticket {
    pub(crate) async fn wait(self) -> Result<Value, Failure> {
        self.0
            .await
            .map_err(|_| "native_multi_command_unconfirmed")?
    }
}
#[derive(Clone, Copy)]
pub(crate) struct SendReadiness {
    pub(crate) session_current: bool,
    pub(crate) channel_ready: bool,
}
struct Entry {
    spec: ProfileSpec,
    stop: watch::Sender<bool>,
    wake: mpsc::Sender<Wake>,
    done: watch::Receiver<Option<Result<(), Failure>>>,
    session: Option<Arc<NativeMultiSession>>,
    generation: u64,
    reason: String,
    error: String,
    failures: u32,
    auth: &'static str,
    health: &'static str,
    attempt_stop: Option<watch::Sender<bool>>,
    retry_requested: bool,
    maintenance_pending: bool,
    registration_pending: bool,
    retry: Option<(String, u64, Option<i64>)>,
    // Set before the journal can publish this unique attempt's deadline.
    installing_retry: Option<String>,
}
#[derive(Clone, Copy)]
enum Wake {
    Maintenance,
    Retry,
    Registration(u64),
    RetryDue(u64),
}
#[derive(Default)]
struct State {
    inventory: Option<Inventory>,
    runtime: Option<(String, Arc<Runtime>)>,
    entries: HashMap<String, Entry>,
    desired: HashMap<String, bool>,
}
pub(crate) struct Manager {
    resources: Resources,
    boot_id: String,
    state: Mutex<State>,
    commands: tokio::sync::Mutex<()>,
    slots: Arc<Semaphore>,
    stopping: AtomicBool,
    startup_gate: tokio::sync::Mutex<()>,
    #[cfg(test)]
    event_hub: Option<Arc<crate::safe_events::Hub>>,
}
fn token() -> Result<String, Failure> {
    crate::multistandby_store::random_token().map_err(|_| "native_multi_random_failed")
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
impl Manager {
    pub(crate) fn new(resources: Resources) -> Result<Arc<Self>, Failure> {
        Ok(Arc::new(Self {
            resources,
            boot_id: token()?,
            state: Mutex::new(State::default()),
            commands: tokio::sync::Mutex::new(()),
            slots: Arc::new(Semaphore::new(64)),
            stopping: AtomicBool::new(false),
            startup_gate: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            event_hub: None,
        }))
    }
    fn diagnostic_hub(&self) -> &crate::safe_events::Hub {
        #[cfg(test)]
        if let Some(hub) = &self.event_hub {
            return hub;
        }
        crate::safe_events::global()
    }
    #[cfg(test)]
    pub(crate) fn new_with_events(
        resources: Resources,
        hub: Arc<crate::safe_events::Hub>,
    ) -> Result<Arc<Self>, Failure> {
        let mut manager = Self::new(resources)?;
        Arc::get_mut(&mut manager)
            .expect("new manager has one owner")
            .event_hub = Some(hub);
        Ok(manager)
    }
    #[cfg(test)]
    pub(crate) fn runtime_for_test(&self) -> Option<Arc<Runtime>> {
        self.state
            .lock()
            .unwrap()
            .runtime
            .as_ref()
            .map(|(_, runtime)| runtime.clone())
    }
    pub(crate) fn boot_id(&self) -> &str {
        &self.boot_id
    }
    pub(crate) async fn install_inventory(&self, inventory: Inventory) -> Result<(), Failure> {
        let _serial = self.commands.lock().await;
        if inventory.profiles.len() > 512
            || inventory.reader.is_empty()
            || self.resources.gate.selected_reader_generation()
                != Some((inventory.reader.clone(), inventory.generation))
        {
            return Err("native_multi_inventory_stale");
        }
        let store = self.resources.store.clone();
        let catalog = crate::archive_io::run(move || store.runtime_catalog())
            .await
            .map_err(|_| "native_multi_store_unavailable")?;
        let mut ids = std::collections::HashSet::new();
        for p in &inventory.profiles {
            if p.aid.is_empty()
                || !ids.insert(&p.profile_id)
                || !catalog["profiles"].as_array().is_some_and(|rows| {
                    rows.iter().any(|r| {
                        r["profile_id"] == p.profile_id
                            && r["card_id"] == p.card_id
                            && r["eid"] == p.eid
                            && r["iccid"] == p.iccid
                            && r["tombstone"] != true
                    })
                })
            {
                return Err("native_multi_inventory_identity_mismatch");
            }
        }
        let mut state = self.state.lock().unwrap();
        if state.runtime.is_some() {
            return Err("native_multi_inventory_busy");
        };
        state.desired = catalog["desired"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| {
                Some((
                    row["profile_id"].as_str()?.to_owned(),
                    row["desired_state"] == "keep_online",
                ))
            })
            .collect();
        state.inventory = Some(inventory);
        Ok(())
    }
    pub(crate) fn cached_inventory(&self) -> Option<Inventory> {
        self.state.lock().unwrap().inventory.clone()
    }
    pub(crate) fn execute(
        self: &Arc<Self>,
        action: Action,
        card_id: String,
        profile_ids: Vec<String>,
    ) -> Result<Ticket, Failure> {
        if self.stopping.load(Ordering::Acquire) {
            return Err("native_multi_stopping");
        }
        if profile_ids.len() > 32 {
            return Err("native_multi_profile_limit");
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "native_multi_queue_full")?;
        let manager = self.clone();
        let (send, wait) = oneshot::channel();
        tokio::spawn(async move {
            let _permit = permit;
            let _serial = manager.commands.lock().await;
            let result = manager.execute_inner(action, card_id, profile_ids).await;
            let _ = send.send(result);
        });
        Ok(Ticket(wait))
    }
    async fn apply(&self, action: &'static str, body: Value) -> Result<Value, Failure> {
        let store = self.resources.store.clone();
        crate::archive_io::run(move || store.apply(action, body))
            .await
            .map_err(|_| "native_multi_store_unavailable")
    }
    async fn execute_inner(
        self: &Arc<Self>,
        action: Action,
        card_id: String,
        ids: Vec<String>,
    ) -> Result<Value, Failure> {
        if self.stopping.load(Ordering::Acquire) {
            return Err("native_multi_stopping");
        }
        let inventory = self
            .state
            .lock()
            .unwrap()
            .inventory
            .clone()
            .ok_or("native_multi_inventory_unavailable")?;
        let mut unique = std::collections::HashSet::new();
        let specs = if ids.is_empty() {
            inventory
                .profiles
                .iter()
                .filter(|p| card_id.is_empty() || p.card_id == card_id)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            ids.iter()
                .filter(|id| unique.insert((*id).clone()))
                .map(|id| {
                    inventory
                        .profiles
                        .iter()
                        .find(|p| p.profile_id == *id && p.card_id == card_id)
                        .cloned()
                        .ok_or("profile_not_found")
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        if specs.is_empty() || specs.len() > 32 {
            return Err("profile_not_found");
        }
        if matches!(action, Action::Stop) {
            let mut failure = None;
            // Stop every selected live generation even when a durable write fails.
            for spec in &specs {
                let known_stopped =
                    self.state.lock().unwrap().desired.get(&spec.profile_id) == Some(&false);
                if !known_stopped {
                    let persisted=match token(){Ok(event)=>self.apply("desired",json!({"event_id":event,"profile_id":spec.profile_id,"desired_state":"stopped"})).await,Err(error)=>Err(error)};
                    match persisted {
                        Ok(_) => {
                            self.state
                                .lock()
                                .unwrap()
                                .desired
                                .insert(spec.profile_id.clone(), false);
                        }
                        Err(error) => {
                            failure.get_or_insert(error);
                        }
                    }
                }
                self.cancel_profile(&spec.profile_id);
            }
            for spec in &specs {
                if let Err(error) = self.stop_profile(&spec.profile_id).await {
                    failure.get_or_insert(error);
                }
            }
            if let Err(error) = self.release_empty().await {
                failure.get_or_insert(error);
            }
            return failure.map_or(Ok(json!({"status":"completed"})), Err);
        }
        if self.resources.gate.selected_reader_generation()
            != Some((inventory.reader.clone(), inventory.generation))
        {
            return Err("native_multi_inventory_stale");
        }
        if matches!(action, Action::Retry) {
            let mut state = self.state.lock().unwrap();
            if specs.iter().any(|p| {
                state
                    .entries
                    .get(&p.profile_id)
                    .is_none_or(|e| *e.stop.borrow() || e.done.borrow().is_some())
            }) {
                return Err("profile_stopped");
            }
            for spec in specs {
                let entry = state.entries.get_mut(&spec.profile_id).unwrap();
                entry.retry = None;
                entry.retry_requested = true;
                entry.registration_pending = false;
                if let Some(cancel) = &entry.attempt_stop {
                    cancel.send_replace(true);
                }
                let _ = entry.wake.try_send(Wake::Retry);
            }
            return Ok(json!({"status":"accepted"}));
        }
        {
            let state = self.state.lock().unwrap();
            // accepted means a retained worker can still own the request, not
            // that registration is guaranteed. Reject already terminal/lost
            // workers before any selected profile writes intent or acquires a
            // runtime; their uncertain cleanup must retain the existing owner.
            if specs.iter().any(|p| {
                state.entries.get(&p.profile_id).is_some_and(|e| {
                    *e.stop.borrow() || e.done.borrow().is_some() || e.done.has_changed().is_err()
                })
            }) {
                return Err("native_multi_cleanup_unconfirmed");
            }
            let additional = specs
                .iter()
                .filter(|p| !state.entries.contains_key(&p.profile_id))
                .count();
            if state.entries.len() + additional > 32 {
                return Err("native_multi_profile_limit");
            }
            if state
                .runtime
                .as_ref()
                .is_some_and(|(card, _)| specs.iter().any(|p| p.card_id != *card))
            {
                return Err("card_busy");
            }
            if specs.iter().any(|p| p.card_id != specs[0].card_id) {
                return Err("card_busy");
            }
        }
        self.runtime_for(&inventory, &specs[0])?;
        for spec in specs {
            if self
                .state
                .lock()
                .unwrap()
                .entries
                .contains_key(&spec.profile_id)
            {
                continue;
            }
            if let Err(error)=self.apply("desired",json!({"event_id":token()?,"profile_id":spec.profile_id,"desired_state":"keep_online"})).await{self.release_empty().await?;return Err(error)}
            self.state
                .lock()
                .unwrap()
                .desired
                .insert(spec.profile_id.clone(), true);
            self.start_profile(inventory.clone(), spec).await?;
        }
        Ok(json!({"status":"accepted"}))
    }
    fn runtime_for(
        &self,
        inventory: &Inventory,
        spec: &ProfileSpec,
    ) -> Result<Arc<Runtime>, Failure> {
        let runtime = {
            self.state
                .lock()
                .unwrap()
                .runtime
                .as_ref()
                .map(|(_, r)| r.clone())
        };
        let runtime = match runtime {
            Some(r) => r,
            None => {
                let pool = Pool::acquire(
                    self.resources.owner.clone(),
                    self.resources.gate.clone(),
                    self.resources.card_service.clone(),
                    inventory.reader.clone(),
                    inventory.generation,
                )
                .map_err(|_| "native_multi_card_admission_failed")?;
                let r = Runtime::new(
                    pool,
                    self.resources.gate.clone(),
                    self.resources.data_root.clone(),
                    self.resources.transactions.clone(),
                )?;
                self.state.lock().unwrap().runtime = Some((spec.card_id.clone(), r.clone()));
                r
            }
        };
        Ok(runtime)
    }
    async fn start_profile(
        self: &Arc<Self>,
        inventory: Inventory,
        spec: ProfileSpec,
    ) -> Result<(), Failure> {
        {
            let s = self.state.lock().unwrap();
            if s.entries.contains_key(&spec.profile_id) {
                return Ok(());
            }
            if s.entries.len() >= 32 {
                return Err("native_multi_profile_limit");
            }
            if s.runtime
                .as_ref()
                .is_some_and(|(card, _)| card != &spec.card_id)
            {
                return Err("card_busy");
            }
        }
        let runtime = self.runtime_for(&inventory, &spec)?;
        let (stop, stop_rx) = watch::channel(false);
        let (wake, rx) = mpsc::channel(4);
        let (done, done_rx) = watch::channel(None);
        self.state.lock().unwrap().entries.insert(
            spec.profile_id.clone(),
            Entry {
                spec: spec.clone(),
                stop,
                wake,
                done: done_rx,
                session: None,
                generation: 0,
                reason: "starting".into(),
                error: String::new(),
                failures: 0,
                auth: "queued",
                health: "starting",
                attempt_stop: None,
                retry_requested: false,
                maintenance_pending: false,
                registration_pending: false,
                retry: None,
                installing_retry: None,
            },
        );
        let manager = self.clone();
        tokio::spawn(async move {
            let id = spec.profile_id.clone();
            let result = manager
                .clone()
                .run_profile(spec, inventory, runtime, stop_rx, rx)
                .await;
            if let Err(error) = result {
                if let Some(entry) = manager.state.lock().unwrap().entries.get_mut(&id) {
                    entry.error = error.into();
                }
            }
            done.send_replace(Some(result));
        });
        Ok(())
    }
    async fn stop_profile(&self, id: &str) -> Result<(), Failure> {
        let mut done = {
            let s = self.state.lock().unwrap();
            let Some(e) = s.entries.get(id) else {
                return Ok(());
            };
            e.stop.send_replace(true);
            if let Some(stop) = &e.attempt_stop {
                stop.send_replace(true);
            }
            e.done.clone()
        };
        loop {
            let result = done.borrow().clone();
            if let Some(result) = result {
                result?;
                self.state.lock().unwrap().entries.remove(id);
                return Ok(());
            }
            done.changed()
                .await
                .map_err(|_| "native_multi_cleanup_unconfirmed")?;
        }
    }
    async fn release_empty(&self) -> Result<(), Failure> {
        let runtime = {
            let s = self.state.lock().unwrap();
            if s.entries.is_empty() {
                s.runtime.as_ref().map(|(_, r)| r.clone())
            } else {
                None
            }
        };
        if let Some(r) = runtime {
            r.retry_shutdown_receipt()
                .await
                .map_err(|_| "native_multi_cleanup_unconfirmed")?;
            self.state.lock().unwrap().runtime = None;
        }
        Ok(())
    }
    fn cancel_profile(&self, id: &str) {
        if let Some(e) = self.state.lock().unwrap().entries.get_mut(id) {
            e.retry = None;
            e.installing_retry = None;
            e.registration_pending = false;
            e.stop.send_replace(true);
            if let Some(stop) = &e.attempt_stop {
                stop.send_replace(true);
            }
        }
    }
    pub(crate) fn has_active_runtime(&self) -> bool {
        self.state.lock().unwrap().runtime.is_some()
    }
    pub(crate) fn maintenance(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        let mut queued = false;
        for e in state.entries.values_mut() {
            if !*e.stop.borrow()
                && e.session.is_some()
                && !e.maintenance_pending
                && e.wake.try_send(Wake::Maintenance).is_ok()
            {
                e.maintenance_pending = true;
                queued = true;
            }
        }
        queued
    }
    pub(crate) fn retry_due(&self, due: RetryDue) -> bool {
        self.dispatch_retry_due(due) == DueDelivery::Accepted
    }
    pub(crate) fn registration_due(&self, due: RegistrationDue) -> bool {
        self.dispatch_registration_due(due) == DueDelivery::Accepted
    }
    pub(crate) fn dispatch_retry_due(&self, due: RetryDue) -> DueDelivery {
        if due.owner_id != self.boot_id {
            return DueDelivery::Stale;
        }
        let mut state = self.state.lock().unwrap();
        let Some(e) = state.entries.get_mut(&due.profile_id) else {
            return DueDelivery::Stale;
        };
        if *e.stop.borrow() {
            return DueDelivery::Stale;
        }
        if !e.retry.as_ref().is_some_and(|(attempt, generation, _)| {
            attempt == &due.attempt_id && *generation == due.generation
        }) {
            // The journal can become visible before spawn_blocking returns.
            // Retain only this exact installing attempt, not arbitrary absent
            // entries or old tokens. The next tick retries after installation.
            return if e.installing_retry.as_deref() == Some(due.attempt_id.as_str()) {
                DueDelivery::Busy
            } else {
                DueDelivery::Stale
            };
        }
        match e.wake.try_send(Wake::RetryDue(due.generation)) {
            Ok(()) => {
                e.retry = None;
                DueDelivery::Accepted
            }
            Err(mpsc::error::TrySendError::Full(_)) => DueDelivery::Busy,
            Err(_) => DueDelivery::Stale,
        }
    }
    pub(crate) fn dispatch_registration_due(&self, due: RegistrationDue) -> DueDelivery {
        if due.owner_id != self.boot_id {
            return DueDelivery::Stale;
        }
        let mut state = self.state.lock().unwrap();
        let Some(e) = state.entries.get_mut(&due.profile_id) else {
            return DueDelivery::Stale;
        };
        if *e.stop.borrow()
            || e.generation != due.generation
            || e.session.is_none()
            || e.registration_pending
        {
            return DueDelivery::Stale;
        }
        match e.wake.try_send(Wake::Registration(due.generation)) {
            Ok(()) => {
                e.registration_pending = true;
                DueDelivery::Accepted
            }
            Err(mpsc::error::TrySendError::Full(_)) => DueDelivery::Busy,
            Err(_) => DueDelivery::Stale,
        }
    }
    pub(crate) fn send_readiness(&self, id: &str) -> SendReadiness {
        let state = self.state.lock().unwrap();
        let pair = self.resources.gate.selected_reader_generation();
        let current = state
            .entries
            .get(id)
            .filter(|e| {
                !*e.stop.borrow()
                    && !e.retry_requested
                    && !e.registration_pending
                    && e.attempt_stop.as_ref().is_none_or(|stop| !*stop.borrow())
            })
            .and_then(|e| e.session.as_ref())
            .map(|s| s.snapshot());
        let session_current = current.as_ref().is_some_and(|s| {
            pair == Some((s.binding.reader.clone(), s.binding.reader_generation))
                && s.termination.is_none()
        });
        SendReadiness {
            session_current,
            channel_ready: session_current
                && current.is_some_and(|s| {
                    s.tunnel_ready && s.registered && s.sms_ready && s.sms_route_ready
                }),
        }
    }
    pub(crate) fn begin_shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        for e in self.state.lock().unwrap().entries.values() {
            e.stop.send_replace(true);
            if let Some(stop) = &e.attempt_stop {
                stop.send_replace(true);
            }
        }
    }
    pub(crate) async fn shutdown(self: &Arc<Self>) -> Result<(), Failure> {
        self.begin_shutdown();
        let manager = self.clone();
        let (send, wait) = oneshot::channel();
        tokio::spawn(async move {
            let _serial = manager.commands.lock().await;
            let ids = manager
                .state
                .lock()
                .unwrap()
                .entries
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            for id in ids {
                let _ = manager.stop_profile(&id).await;
            }
            let runtime = manager
                .state
                .lock()
                .unwrap()
                .runtime
                .as_ref()
                .map(|(_, r)| r.clone());
            let result = match runtime {
                Some(runtime) => runtime
                    .retry_shutdown_receipt()
                    .await
                    .map(|_| ())
                    .map_err(|_| "native_multi_cleanup_unconfirmed"),
                None => Ok(()),
            };
            if result.is_ok() {
                let mut state = manager.state.lock().unwrap();
                state.entries.clear();
                state.runtime = None;
            }
            let _ = send.send(result);
        });
        wait.await.map_err(|_| "native_multi_cleanup_unconfirmed")?
    }
}

#[cfg(test)]
mod start_admission_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn due_preserves_busy_attempt_and_fences_stale_identity_and_stop() {
        let physical = crate::multistandby_card::production::NativeFixture::new();
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap())
            .join(format!("runtime/manager-due-497-{}", token().unwrap()));
        let manager = Manager::new(Resources {
            owner: physical.owner.clone(),
            gate: physical.gate.clone(),
            card_service: crate::multistandby_card::Service::new(
                physical.owner.clone(),
                physical.gate.clone(),
                None,
                None,
            ),
            data_root: root.clone(),
            store: crate::multistandby_store::Store::open(&root).unwrap(),
            journal: Arc::new(Mutex::new(Journal::default())),
            transactions: Default::default(),
            connection: Default::default(),
        })
        .unwrap();
        let (wake, mut received) = mpsc::channel(1);
        wake.try_send(Wake::Maintenance).unwrap();
        let (stop, _) = watch::channel(false);
        let (_done, done) = watch::channel(None);
        let attempt = "a".repeat(48);
        manager.state.lock().unwrap().entries.insert(
            "profile".into(),
            Entry {
                spec: ProfileSpec {
                    profile_id: "profile".into(),
                    card_id: "card".into(),
                    eid: "eid".into(),
                    iccid: "iccid".into(),
                    aid: "aid".into(),
                    display_name: String::new(),
                    phone_number: String::new(),
                    physical_enabled: false,
                },
                stop,
                wake,
                done,
                session: None,
                generation: 3,
                reason: "ims_lost".into(),
                error: String::new(),
                failures: 1,
                auth: "backoff",
                health: "degraded",
                attempt_stop: None,
                retry_requested: false,
                maintenance_pending: false,
                registration_pending: false,
                retry: Some((attempt.clone(), 7, Some(1000))),
                installing_retry: None,
            },
        );
        let due = RetryDue {
            profile_id: "profile".into(),
            owner_id: manager.boot_id().into(),
            generation: 7,
            attempt_id: attempt,
        };
        for stale in [
            RetryDue {
                owner_id: "old-boot".into(),
                ..due.clone()
            },
            RetryDue {
                generation: due.generation + 1,
                ..due.clone()
            },
            RetryDue {
                attempt_id: "b".repeat(48),
                ..due.clone()
            },
        ] {
            assert_eq!(manager.dispatch_retry_due(stale), DueDelivery::Stale);
            assert_eq!(
                manager.state.lock().unwrap().entries["profile"]
                    .retry
                    .as_ref()
                    .map(|(attempt, generation, _)| (attempt.clone(), *generation)),
                Some((due.attempt_id.clone(), due.generation)),
                "stale boot/attempt/schedule generation cannot consume current retry"
            );
        }
        assert_eq!(manager.dispatch_retry_due(due.clone()), DueDelivery::Busy);
        assert!(
            manager.state.lock().unwrap().entries["profile"]
                .retry
                .is_some()
        );
        received.try_recv().unwrap();
        assert_eq!(
            manager.dispatch_retry_due(due.clone()),
            DueDelivery::Accepted
        );
        assert_eq!(manager.dispatch_retry_due(due.clone()), DueDelivery::Stale);
        assert!(matches!(received.try_recv(), Ok(Wake::RetryDue(7))));
        manager
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut("profile")
            .unwrap()
            .retry = Some((due.attempt_id.clone(), due.generation, Some(1000)));
        manager.cancel_profile("profile");
        assert_eq!(manager.dispatch_retry_due(due), DueDelivery::Stale);
        assert!(
            matches!(received.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "a stop concurrent with retry delivery must not wake a new attempt"
        );
        // Pause the worker in the exact interval after durable scheduling and
        // before e.retry installation; drive the production timer completion.
        let installing = "c".repeat(48);
        {
            let mut state = manager.state.lock().unwrap();
            let e = state.entries.get_mut("profile").unwrap();
            e.stop.send_replace(false);
            e.installing_retry = Some(installing.clone());
        }
        let schedule = manager.resources.journal.lock().unwrap()
            .schedule_native_retry("profile".into(), manager.boot_id().into(), installing.clone(), 1, 0)
            .unwrap();
        let pending = manager.resources.journal.lock().unwrap().next_due_retry(i64::MAX).unwrap();
        let delivery = manager.dispatch_retry_due(pending.clone());
        assert_eq!(delivery, DueDelivery::Busy);
        crate::multistandby_retry::complete_retry(&manager.resources.journal, &pending, delivery, 1000);
        assert!(manager.resources.journal.lock().unwrap().next_due_retry(i64::MAX).is_some());
        assert_eq!(manager.dispatch_retry_due(RetryDue {
            attempt_id: "d".repeat(48), ..pending.clone()
        }), DueDelivery::Stale);
        {
            let mut state = manager.state.lock().unwrap();
            let e = state.entries.get_mut("profile").unwrap();
            e.retry = Some((installing, schedule.generation, schedule.due_at_ms));
            e.installing_retry = None;
        }
        let delivery = manager.dispatch_retry_due(pending.clone());
        assert_eq!(delivery, DueDelivery::Accepted);
        crate::multistandby_retry::complete_retry(&manager.resources.journal, &pending, delivery, 1000);
        assert!(matches!(received.try_recv(), Ok(Wake::RetryDue(g)) if g == schedule.generation));
        assert!(manager.resources.journal.lock().unwrap().next_due_retry(i64::MAX).is_none());
        // A genuinely orphaned journal deadline still retires as stale.
        manager.resources.journal.lock().unwrap()
            .schedule_native_retry("profile".into(), manager.boot_id().into(), "e".repeat(48), 1, 0)
            .unwrap();
        let stale = manager.resources.journal.lock().unwrap().next_due_retry(i64::MAX).unwrap();
        let delivery = manager.dispatch_retry_due(stale.clone());
        assert_eq!(delivery, DueDelivery::Stale);
        crate::multistandby_retry::complete_retry(&manager.resources.journal, &stale, delivery, 1000);
        assert!(manager.resources.journal.lock().unwrap().next_due_retry(i64::MAX).is_none());
        manager.state.lock().unwrap().entries.clear();
        manager.shutdown().await.unwrap();
        physical.finish().await;
    }
}
