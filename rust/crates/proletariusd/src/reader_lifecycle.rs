//! Native reader selection, metadata and session supplier. Physical work is
//! retained by the service; an HTTP cancellation cannot complete a transition.
mod presence;

use crate::reader_pcsc::{Binding, LifecycleBinding, Owner, ResetAction};
use crate::reader_reservation::{
    CardTransaction, Gate, NativeAuthority, NativePinBinding, NativePinPolicy, NativeReaderFacts,
    NativeTransitionKind as Kind, SelectionSnapshot,
};
use crate::reader_session_lease::{
    AdmissionFuture, EsimSwitchFinalizeResponse, NativeCardContext, NativeSessionSupplier,
    PreAdmissionFuture, SupplierFuture,
};
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

/// A producer of retained cleanup jobs, not a disposable status waiter. Stop
/// and join this loop before the final lifecycle jobs/transport drain.
pub(crate) async fn run_ble_presence_observer(
    service: Service,
    mut stopping: tokio::sync::watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_failure = None;
    loop {
        // A preexisting stop must win even if the first tick is also ready.
        if *stopping.borrow() {
            break;
        }
        tokio::select! {
            biased;
            changed=stopping.changed()=>{if changed.is_err()||*stopping.borrow(){break;}}
            _=interval.tick()=>{
                // Only the exact authenticated bridge receipt proves loss.
                match service.reconcile_ble_presence().await {
                    Ok(_)=>last_failure=None,
                    Err(error)=>if last_failure!=Some(error.code) {
                        crate::diagnostics::write_line(format_args!("switchd-rs BLE loss observation: {}",error.code));
                        last_failure=Some(error.code);
                    },
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) recoverable: bool,
}
impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message: message.into(),
            recoverable: true,
        }
    }
    fn gate(code: &'static str) -> Self {
        Self::new(code, "读卡器状态尚未确认，请刷新后重试")
    }
    fn physical(error: crate::reader_pcsc::Failure) -> Self {
        Self {
            status: error.status,
            code: error.code,
            message: error.message,
            recoverable: !error.uncertain,
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Snapshot {
    pub(crate) status: Value,
    pub(crate) usim_info: Option<Value>,
    pub(crate) selection: SelectionSnapshot,
    pub(crate) esim_manager_available: bool,
    pub(crate) notification_aids: Vec<String>,
    pub(crate) esim_profile_name: String,
    pub(crate) pcsc_session: Option<LifecycleBinding>,
}
impl Snapshot {
    fn from_facts(
        selection: SelectionSnapshot,
        facts: Option<NativeReaderFacts>,
        session: Option<LifecycleBinding>,
    ) -> Self {
        let facts = facts.unwrap_or_default();
        let connected = !selection.reader.is_empty();
        // Logical selection/session and metadata do not prove current card
        // presence. The asynchronous public observation adds OS/bridge facts.
        let mut status = json!({"connected":connected,"has_card":false,
            "card_state":"unknown","presence_source":"none",
            "card_muted":null,"card_unpowered":null,"card_ready":false,
            "session_ready":session.is_some() || facts.ble_connection.is_some(),
            "metadata_ready":facts.metadata.is_some(),
            "card_type":if facts.has_esim {"euicc"} else if facts.metadata.is_some() {"physical_sim"} else {"unknown"},
            "has_usim_info":facts.metadata.is_some(),"has_esim":facts.has_esim});
        if connected {
            status["reader_name"] = json!(selection.reader);
        }
        if selection.generation != 0 {
            status["reader_generation"] = json!(selection.generation);
        }
        Self {
            status,
            usim_info: facts.metadata,
            selection,
            esim_manager_available: facts.has_esim,
            notification_aids: facts.notification_aids,
            esim_profile_name: facts.profile_name,
            pcsc_session: session,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Service(Arc<Inner>);
struct Inner {
    data_root: PathBuf,
    gate: Arc<Gate>,
    owner: Owner,
    producer: String,
    ble_socket: Option<PathBuf>,
    app_token: Option<String>,
    bridge: Mutex<Option<crate::reader_business_runtime::Bridge>>,
    ordinary_lifecycle: Mutex<Weak<crate::native_vowifi_session::NativeLifecycle>>,
    multistandby_service: Mutex<Weak<crate::multistandby_native_service::Service>>,
    life: Mutex<Life>,
    serial: tokio::sync::Mutex<()>,
    changed: tokio::sync::Notify,
    presence: tokio::sync::Mutex<presence::Cache>,
}
#[derive(Default)]
struct Life {
    stopping: bool,
    jobs: usize,
    fresh_jobs: usize,
    cleanup_jobs: usize,
    receipts: VecDeque<Receipt>,
    retirement_error: Option<String>,
}
#[derive(Clone)]
struct Receipt {
    id: String,
    payload: Value,
    result: Option<Result<Value, Failure>>,
}
struct Job(Service, bool);
impl Drop for Job {
    fn drop(&mut self) {
        let mut life = self.0.0.life.lock().unwrap_or_else(|e| e.into_inner());
        life.jobs = life.jobs.saturating_sub(1);
        if self.1 {
            life.cleanup_jobs = life.cleanup_jobs.saturating_sub(1);
        } else {
            life.fresh_jobs = life.fresh_jobs.saturating_sub(1);
        }
        self.0.0.changed.notify_waiters();
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Input {
    action: String,
    #[serde(default)]
    reader: String,
    #[serde(default)]
    expected_generation: Option<u64>,
    #[serde(default)]
    number: String,
    #[serde(default)]
    transaction: Option<CardTransaction>,
    #[serde(default)]
    pcsc: Option<Binding>,
    #[serde(default)]
    target_iccid: String,
    #[serde(default)]
    aid: String,
    #[serde(default)]
    operation_id: String,
    #[serde(default)]
    reset: Option<bool>,
}
struct Observation {
    facts: NativeReaderFacts,
    metadata_error: Option<String>,
    warnings: Vec<String>,
    codes: Vec<&'static str>,
}
struct AutomaticSelection {
    proposal: proletarius_api_contract::ReaderAutoConnect,
    inventory: Arc<Mutex<proletarius_reader::ReaderDiscovery>>,
    instance: String,
}

impl Service {
    pub(crate) fn new(
        data_root: PathBuf,
        gate: Arc<Gate>,
        owner: Owner,
        ble_socket: Option<PathBuf>,
        app_token: Option<String>,
        producer: String,
    ) -> Result<Self, Failure> {
        gate.ensure_native_reader_producer(&producer)
            .map_err(Failure::gate)?;
        gate.initialize_native_lifecycle(&producer)
            .map_err(Failure::gate)?;
        Ok(Self(Arc::new(Inner {
            data_root,
            gate,
            owner,
            producer,
            ble_socket,
            app_token,
            bridge: Mutex::new(None),
            ordinary_lifecycle: Mutex::new(Weak::new()),
            multistandby_service: Mutex::new(Weak::new()),
            life: Mutex::new(Life::default()),
            serial: tokio::sync::Mutex::new(()),
            changed: tokio::sync::Notify::new(),
            presence: tokio::sync::Mutex::new(presence::Cache::default()),
        })))
    }
    pub(crate) fn set_business_bridge(&self, bridge: crate::reader_business_runtime::Bridge) {
        *self.0.bridge.lock().unwrap_or_else(|e| e.into_inner()) = Some(bridge);
    }
    pub(crate) fn set_multistandby_service(
        &self,
        service: Weak<crate::multistandby_native_service::Service>,
    ) {
        *self
            .0
            .multistandby_service
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = service;
    }
    pub(crate) fn set_ordinary_lifecycle(
        &self,
        lifecycle: Weak<crate::native_vowifi_session::NativeLifecycle>,
    ) {
        *self
            .0
            .ordinary_lifecycle
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = lifecycle;
    }
    fn ordinary_lifecycle(
        &self,
    ) -> Result<Arc<crate::native_vowifi_session::NativeLifecycle>, Failure> {
        self.0
            .ordinary_lifecycle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
            .ok_or_else(|| Failure::new("reader_business_unavailable", "通信业务占用状态尚未确认"))
    }
    fn reject_maintenance(
        &self,
        reader: &str,
        generation: u64,
        message: &'static str,
    ) -> Result<Arc<crate::ordinary_vowifi_admission::Maintenance>, Failure> {
        self.ordinary_lifecycle()?
            .ordinary_admission()
            .begin_reader_maintenance(
                reader,
                generation,
                crate::ordinary_vowifi_admission::MaintenancePolicy::RejectAllOrdinary,
            )
            .map_err(|_| Failure::new("reader_operation_active", message))
    }
    // The consumer has awaited its actual physical job. Exact commit/abort
    // checks that transition's workers, not an observation timeout. If the
    // result is still unknown, retain the capability in the Gate; a later
    // unrelated snapshot must never be used as this operation's proof.
    fn finish_maintenance(&self, maintenance: Arc<crate::ordinary_vowifi_admission::Maintenance>) {
        if !self.snapshot().selection.pending {
            let _ = maintenance.release_confirmed();
        }
    }
    fn bridge(&self) -> Result<crate::reader_business_runtime::Bridge, Failure> {
        self.0
            .bridge
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| Failure::new("reader_business_unavailable", "通信业务占用状态尚未确认"))
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.snapshot_with_epoch().0
    }
    fn snapshot_with_epoch(&self) -> (Snapshot, u64) {
        let (selection, facts, session, epoch) = self.0.gate.native_reader_snapshot_with_epoch();
        (Snapshot::from_facts(selection, facts, session), epoch)
    }
    pub(crate) fn facts(&self) -> Snapshot {
        self.snapshot()
    }

    fn register_job(&self, cleanup_slot: bool, allow_stopping: bool) -> Result<Job, Failure> {
        let mut life = self.0.life.lock().unwrap_or_else(|e| e.into_inner());
        if life.stopping && !allow_stopping {
            return Err(Failure::new("backend_stopping", "服务正在退出"));
        }
        let count = if cleanup_slot {
            &mut life.cleanup_jobs
        } else {
            &mut life.fresh_jobs
        };
        let limit = if cleanup_slot { 8 } else { 16 };
        if *count >= limit {
            return Err(Failure::new(
                "reader_busy",
                "读卡器任务队列已满，请等待原操作完成",
            ));
        }
        *count += 1;
        life.jobs += 1;
        Ok(Job(self.clone(), cleanup_slot))
    }

    /// Register a retained owner before returning the future's waiter. Tokio
    /// task handles detach on drop; the inner task keeps admission and cleanup.
    async fn retained<T, F, Fut>(&self, existing: bool, work: F) -> Result<T, Failure>
    where
        T: Send + 'static,
        F: FnOnce(Service) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Failure>> + Send + 'static,
    {
        let job = self.register_job(existing, existing)?;
        let service = self.clone();
        tokio::spawn(async move {
            let _job = job;
            let _serial = service.0.serial.lock().await;
            work(service.clone()).await
        })
        .await
        .map_err(|_| Failure::new("reader_task_failed", "读卡器任务未确认，请保持原设备连接"))?
    }
    pub(crate) async fn connect(&self, reader: &str) -> Result<Snapshot, Failure> {
        let reader = reader.trim().to_owned();
        self.retained(false, move |service| async move {
            service.connect_inner(reader).await
        })
        .await
    }
    pub(crate) async fn connect_automatic(
        &self,
        proposal: proletarius_api_contract::ReaderAutoConnect,
        inventory: Arc<Mutex<proletarius_reader::ReaderDiscovery>>,
        instance: String,
    ) -> Result<Snapshot, Failure> {
        self.retained(false, move |service| async move {
            service
                .connect_inner_with_selection(
                    proposal.reader.clone(),
                    Some(AutomaticSelection {
                        proposal,
                        inventory,
                        instance,
                    }),
                )
                .await
        })
        .await
    }
    pub(crate) async fn disconnect(&self) -> Result<Snapshot, Failure> {
        self.retained(
            false,
            |service| async move { service.disconnect_inner().await },
        )
        .await
    }
    pub(crate) async fn refresh(&self) -> Result<Snapshot, Failure> {
        self.retained(false, |service| async move {
            service
                .observe_inner(None, None, false, "", "", None)
                .await
                .map(|(snapshot, _)| snapshot)
        })
        .await
    }
    pub(crate) async fn handle_service(&self, value: Value) -> Result<Value, Failure> {
        let input: Input = serde_json::from_value(value)
            .map_err(|_| Failure::new("invalid_request", "读卡器请求无效"))?;
        if !input.target_iccid.is_empty()
            && (!(10..=20).contains(&input.target_iccid.len())
                || !input.target_iccid.bytes().all(|c| c.is_ascii_digit()))
        {
            return Err(Failure::new("invalid_target_iccid", "切卡目标 ICCID 无效"));
        }
        if !input.aid.is_empty() {
            decode_aid(&input.aid)?;
        }
        match input.action.as_str() {
            "snapshot" => {
                let snapshot = self.snapshot();
                if snapshot.selection.pending {
                    return Err(Failure::gate("reader_lifecycle_pending"));
                }
                Ok(json!({"ok":true,"snapshot":snapshot}))
            }
            "phone_update" => {
                let generation = input
                    .expected_generation
                    .ok_or_else(|| Failure::gate("stale_reader_generation"))?;
                let applied =
                    self.0
                        .gate
                        .native_phone_update(&input.reader, generation, &input.number);
                Ok(json!({"ok":true,"applied":applied,"snapshot":self.snapshot()}))
            }
            "refresh" | "rebuild" => {
                if input.transaction.is_some() != input.pcsc.is_some()
                    && !input.reader.starts_with("BLE:")
                {
                    return Err(Failure::new("invalid_request", "读卡器操作能力不完整"));
                }
                if input.action == "rebuild"
                    && (input.operation_id.is_empty()
                        || input.operation_id.len() > 128
                        || input.operation_id.chars().any(char::is_control))
                {
                    return Err(Failure::new("invalid_request", "重建操作标识无效"));
                }
                let payload =
                    serde_json::to_value(&input).map_err(|_| Failure::gate("invalid_request"))?;
                self.retained(input.transaction.is_some(),move|service|async move {
                    let rebuild=input.action=="rebuild";
                    if rebuild {
                        let mut life=service.0.life.lock().unwrap_or_else(|e|e.into_inner());
                        if let Some(receipt)=life.receipts.iter().find(|r|r.id==input.operation_id) {
                            if receipt.payload!=payload {return Err(Failure::gate("reader_receipt_conflict"));}
                            return receipt.result.clone().unwrap_or_else(||Err(Failure::gate("reader_outcome_unknown")));
                        }
                        if life.receipts.len()>=64 {
                            // Old capabilities cannot execute after retirement.
                            // Retain every receipt still backed by a live owner,
                            // rather than evicting an unknown submitted result.
                            if let Some(index)=life.receipts.iter().position(|r|r.result.is_some() && serde_json::from_value::<Input>(r.payload.clone()).ok().and_then(|i|i.transaction).is_none_or(|tx|!service.0.gate.native_transaction_current(&tx))) {life.receipts.remove(index);}
                            else {return Err(Failure::new("reader_receipt_capacity","读卡器重建回执已满，请等待原操作退出"));}
                        }
                        life.receipts.push_back(Receipt{id:input.operation_id.clone(),payload,result:None});
                    }
                    let before=service.snapshot().selection;
                    let result=if (!input.reader.is_empty() && input.reader!=before.reader) || input.expected_generation.is_some_and(|g|g!=before.generation) { Err(Failure::gate("stale_reader_generation")) }
                        else {service.observe_inner(input.transaction,input.pcsc,rebuild,&input.target_iccid,&input.aid,input.reset).await.map(|(snapshot,finalized)|json!({"ok":true,"snapshot":snapshot,"finalized":finalized}))};
                    if rebuild { if let Some(receipt)=service.0.life.lock().unwrap_or_else(|e|e.into_inner()).receipts.iter_mut().find(|r|r.id==input.operation_id) {receipt.result=Some(result.clone());} }
                    result
                }).await
            }
            _ => Err(Failure::new("invalid_request", "读卡器操作无效")),
        }
    }

    // This proof is carried by the retained operation, not inferred from an
    // absent Go producer. All ordinary claims were excluded before dispatch;
    // the exact physical transition has now actually committed or retired.
    fn retire_selection_generation(
        &self,
        reader: &str,
        generation: u64,
        maintenance: &Arc<crate::ordinary_vowifi_admission::Maintenance>,
    ) -> Result<(), Failure> {
        if maintenance.policy()
            != crate::ordinary_vowifi_admission::MaintenancePolicy::RejectAllOrdinary
            || !maintenance.is_current()
            || !maintenance.ordinary_drained()
            || maintenance.reader() != reader
            || maintenance.reader_generation() != generation
            || self.snapshot().selection.pending
        {
            return Err(Failure::new(
                "reader_business_unconfirmed",
                "读卡器维护退出证据未确认",
            ));
        }
        Ok(())
    }
    async fn retire_generation(
        &self,
        reader: &str,
        generation: u64,
        producer: Option<String>,
    ) -> Result<(), Failure> {
        let multi = self
            .0
            .multistandby_service
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade();
        if let Some(multi) = multi {
            multi
                .retire_generation(reader, generation)
                .await
                .map_err(|error| Failure::new(error.code, error.message))?;
        }
        // None is supplied only by the exact physical transition's captured
        // admission facts, never by an unavailable or empty Go status snapshot.
        let Some(producer) = producer else {
            return Ok(());
        };
        let bridge = self.bridge()?;
        loop {
            if bridge.producer_retired(&producer) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    async fn close_confirmed(&self, session: LifecycleBinding) -> Result<(), Failure> {
        loop {
            let owner = self.0.owner.clone();
            let physical = session.clone();
            let result = tokio::task::spawn_blocking(move || owner.native_close(&physical))
                .await
                .unwrap_or_else(|_| {
                    Err(crate::reader_pcsc::Failure {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        code: "reader_task_failed",
                        message: "读卡器实际关闭未确认".into(),
                        submitted: true,
                        uncertain: true,
                    })
                });
            if result.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    async fn connect_inner(&self, reader: String) -> Result<Snapshot, Failure> {
        self.connect_inner_with_selection(reader, None).await
    }
    async fn connect_inner_with_selection(
        &self,
        reader: String,
        automatic: Option<AutomaticSelection>,
    ) -> Result<Snapshot, Failure> {
        if reader.is_empty() || reader.len() > 512 || reader.chars().any(char::is_control) {
            return Err(Failure::new("invalid_reader", "读卡器名称不能为空或无效"));
        }
        let before = self.snapshot();
        if before.selection.pending {
            return Err(Failure::gate("reader_lifecycle_pending"));
        }
        let scope = if before.selection.reader.is_empty() {
            reader.as_str()
        } else {
            before.selection.reader.as_str()
        };
        let maintenance = self.reject_maintenance(
            scope,
            before.selection.generation,
            "读卡器会话正在使用设备，请稍后重试",
        )?;
        let result = self
            .connect_with_maintenance(reader, &maintenance, automatic)
            .await;
        self.finish_maintenance(maintenance);
        result
    }
    async fn connect_with_maintenance(
        &self,
        reader: String,
        maintenance: &Arc<crate::ordinary_vowifi_admission::Maintenance>,
        automatic: Option<AutomaticSelection>,
    ) -> Result<Snapshot, Failure> {
        if reader.is_empty() || reader.len() > 512 || reader.chars().any(char::is_control) {
            return Err(Failure::new("invalid_reader", "读卡器名称不能为空或无效"));
        }
        let before = self.snapshot();
        if before.selection.pending {
            return Err(Failure::gate("reader_lifecycle_pending"));
        }
        let prepare = || {
            self.0.gate.prepare_native_transition(
                &self.0.producer,
                &before.selection,
                Kind::Connect,
                &reader,
                NativeAuthority::Idle,
            )
        };
        let transition = if let Some(automatic) = automatic {
            // The inventory cannot be replaced between claim validation and
            // gate.prepare's exact-generation compare-and-transition. No lock
            // survives physical I/O or an await.
            let inventory = automatic
                .inventory
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if crate::reader_inventory::propose(
                &inventory.snapshot(),
                &before.selection,
                &automatic.instance,
            )
            .as_ref()
                != Some(&automatic.proposal)
            {
                return Err(Failure::gate("reader_auto_connect_stale"));
            }
            prepare()
        } else {
            prepare()
        }
        .map_err(Failure::gate)?;
        if before.selection.reader == reader {
            self.0
                .gate
                .abort_native_transition(&transition)
                .map_err(Failure::gate)?;
            return Ok(before);
        }
        let worker = match self.0.gate.track_native_lifecycle_worker(&transition) {
            Ok(worker) => worker,
            Err(code) => {
                self.0
                    .gate
                    .abort_native_transition(&transition)
                    .map_err(Failure::gate)?;
                return Err(Failure::gate(code));
            }
        };
        let service = self.clone();
        let previous = before.pcsc_session.clone();
        let created = Arc::new(Mutex::new(None));
        let worker_created = created.clone();
        let job = tokio::task::spawn_blocking(move || {
            let _worker = worker;
            if let Some(previous) = previous {
                service
                    .0
                    .owner
                    .native_close(&previous)
                    .map_err(Failure::physical)?;
            }
            if reader.starts_with("BLE:") {
                let observation = service.ble_observation(&reader, false, "", "", None, true)?;
                Ok((None, observation))
            } else {
                let session = service
                    .0
                    .owner
                    .native_open(&reader)
                    .map_err(Failure::physical)?;
                *worker_created.lock().unwrap_or_else(|e| e.into_inner()) = Some(session.clone());
                // Opening a valid session may precede SIM readiness. Metadata
                // errors are nonfatal only while establishing that session.
                let observation = match service
                    .0
                    .owner
                    .native_work_on_session(&session, |transport| {
                        read_observation(transport, true, false, "", "")
                    }) {
                    Ok(result) => result?,
                    Err(error) if !error.uncertain && !error.submitted => Observation {
                        facts: NativeReaderFacts::default(),
                        metadata_error: Some(error.to_string()),
                        warnings: Vec::new(),
                        codes: Vec::new(),
                    },
                    Err(error) => return Err(Failure::physical(error)),
                };
                Ok((Some(session), observation))
            }
        })
        .await
        .unwrap_or_else(|_| Err(Failure::gate("reader_task_failed")));
        let (session, observation) = match job {
            Ok(value) => value,
            Err(error) => {
                // An old selection cannot survive its confirmed physical
                // close. Complete cleanup once, never replay the failed open.
                let created = created.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(created) = created {
                    self.close_confirmed(created).await?;
                }
                if let Some(previous) = before.pcsc_session.clone() {
                    self.close_confirmed(previous).await?;
                }
                self.0
                    .gate
                    .complete_native_failed_connect(&transition)
                    .map_err(Failure::gate)?;
                if !before.selection.reader.is_empty() {
                    self.retire_selection_generation(
                        &before.selection.reader,
                        before.selection.generation,
                        maintenance,
                    )?;
                }
                return Err(error);
            }
        };
        if let Err(error) = self.commit(&transition, session.as_ref(), observation.facts) {
            if let Some(session) = session {
                self.close_confirmed(session).await?;
            }
            self.0
                .gate
                .complete_native_failed_connect(&transition)
                .map_err(Failure::gate)?;
            if !before.selection.reader.is_empty() {
                self.retire_selection_generation(
                    &before.selection.reader,
                    before.selection.generation,
                    maintenance,
                )?;
            }
            return Err(error);
        }
        if !before.selection.reader.is_empty() {
            self.retire_selection_generation(
                &before.selection.reader,
                before.selection.generation,
                maintenance,
            )?;
        }
        Ok(self.snapshot())
    }
    async fn disconnect_inner(&self) -> Result<Snapshot, Failure> {
        let before = self.snapshot();
        if before.selection.pending {
            return Err(Failure::gate("reader_lifecycle_pending"));
        }
        if before.selection.reader.is_empty() {
            if self
                .ordinary_lifecycle()?
                .ordinary_admission()
                .has_ordinary_claim()
                || self.0.gate.reader_work_active("")
            {
                return Err(Failure::new(
                    "reader_operation_active",
                    "读卡器会话正在使用设备，请稍后重试",
                ));
            }
            return Ok(before);
        }
        let maintenance = self.reject_maintenance(
            &before.selection.reader,
            before.selection.generation,
            "读卡器会话正在使用设备，请稍后重试",
        )?;
        let result = self.disconnect_with_maintenance(&maintenance).await;
        self.finish_maintenance(maintenance);
        result
    }
    async fn disconnect_with_maintenance(
        &self,
        maintenance: &Arc<crate::ordinary_vowifi_admission::Maintenance>,
    ) -> Result<Snapshot, Failure> {
        let before = self.snapshot();
        if before.selection.pending {
            return Err(Failure::gate("reader_lifecycle_pending"));
        }
        if before.selection.reader.is_empty() {
            return Ok(before);
        }
        let transition = self
            .0
            .gate
            .prepare_native_transition(
                &self.0.producer,
                &before.selection,
                Kind::Disconnect,
                &before.selection.reader,
                NativeAuthority::Idle,
            )
            .map_err(Failure::gate)?;
        let worker = match self.0.gate.track_native_lifecycle_worker(&transition) {
            Ok(worker) => worker,
            Err(code) => {
                self.0
                    .gate
                    .abort_native_transition(&transition)
                    .map_err(Failure::gate)?;
                return Err(Failure::gate(code));
            }
        };
        let owner = self.0.owner.clone();
        let session = before.pcsc_session.clone();
        let cleanup = tokio::task::spawn_blocking(move || {
            let _worker = worker;
            if let Some(session) = session {
                owner.native_close(&session).map_err(Failure::physical)?;
            }
            Ok::<_, Failure>(())
        })
        .await
        .unwrap_or_else(|_| Err(Failure::gate("reader_task_failed")));
        if cleanup.is_err() {
            if let Some(session) = before.pcsc_session.clone() {
                self.close_confirmed(session).await?;
            } else {
                return cleanup.map(|_| self.snapshot());
            }
        }
        self.commit(&transition, None, NativeReaderFacts::default())?;
        self.retire_selection_generation(
            &before.selection.reader,
            before.selection.generation,
            maintenance,
        )?;
        Ok(self.snapshot())
    }
    fn commit(
        &self,
        transition: &crate::reader_reservation::NativeTransition,
        session: Option<&LifecycleBinding>,
        facts: NativeReaderFacts,
    ) -> Result<SelectionSnapshot, Failure> {
        if let Some(session) = session {
            self.0
                .owner
                .with_live_lifecycle_binding(&self.0.producer, &session.reader, |live| {
                    if live != session {
                        return Err("stale_physical_binding");
                    }
                    self.0
                        .gate
                        .commit_native_transition(transition, Some(live), facts)
                })
                .map_err(Failure::physical)?
                .map_err(Failure::gate)
        } else {
            self.0
                .gate
                .commit_native_transition(transition, None, facts)
                .map_err(Failure::gate)
        }
    }
    async fn observe_inner(
        &self,
        transaction: Option<CardTransaction>,
        pcsc: Option<Binding>,
        rebuild: bool,
        target: &str,
        aid: &str,
        reset: Option<bool>,
    ) -> Result<(Snapshot, Option<EsimSwitchFinalizeResponse>), Failure> {
        if transaction.is_some() {
            // The existing operation/native lease owns its retained maintenance
            // capability. A nested acquisition would deadlock its finalizer.
            return self
                .observe_with_maintenance(transaction, pcsc, rebuild, target, aid, reset, None)
                .await;
        }
        let before = self.snapshot();
        if before.selection.pending {
            return Err(Failure::gate("reader_lifecycle_pending"));
        }
        if before.selection.reader.is_empty() {
            return Err(Failure::new("reader_not_connected", "读卡器未连接"));
        }
        let maintenance = self.reject_maintenance(
            &before.selection.reader,
            before.selection.generation,
            "读卡器会话正在使用设备",
        )?;
        let result = self
            .observe_with_maintenance(
                transaction,
                pcsc,
                rebuild,
                target,
                aid,
                reset,
                Some(&maintenance),
            )
            .await;
        self.finish_maintenance(maintenance);
        result
    }
    async fn observe_with_maintenance(
        &self,
        transaction: Option<CardTransaction>,
        pcsc: Option<Binding>,
        rebuild: bool,
        target: &str,
        aid: &str,
        reset: Option<bool>,
        maintenance: Option<&Arc<crate::ordinary_vowifi_admission::Maintenance>>,
    ) -> Result<(Snapshot, Option<EsimSwitchFinalizeResponse>), Failure> {
        let before = self.snapshot();
        if before.selection.pending {
            return Err(Failure::gate("reader_lifecycle_pending"));
        }
        if before.selection.reader.is_empty() {
            return Err(Failure::new("reader_not_connected", "读卡器未连接"));
        }
        if transaction.is_none() && maintenance.is_none() {
            return Err(Failure::gate("reader_business_unconfirmed"));
        }
        let authority = transaction
            .clone()
            .map(NativeAuthority::Transaction)
            .unwrap_or(NativeAuthority::Idle);
        let transition = self
            .0
            .gate
            .prepare_native_transition(
                &self.0.producer,
                &before.selection,
                if rebuild {
                    Kind::Rebuild
                } else {
                    Kind::Identity
                },
                &before.selection.reader,
                authority,
            )
            .map_err(Failure::gate)?;
        // Register before physical dispatch, against the captured capability.
        let card_worker = if let Some(tx @ CardTransaction::Operation { .. }) = &transaction {
            match self.0.gate.track_card_io(
                tx,
                pcsc.as_ref()
                    .map(|b| b.producer_id.as_str())
                    .unwrap_or(&self.0.producer),
                &before.selection.reader,
            ) {
                Ok(worker) => Some(worker),
                Err(code) => {
                    self.0
                        .gate
                        .abort_native_transition(&transition)
                        .map_err(Failure::gate)?;
                    return Err(Failure::gate(code));
                }
            }
        } else {
            None
        };
        if let (Some(tx @ CardTransaction::Operation { .. }), Some(pcsc)) = (&transaction, &pcsc) {
            if let Err(code) = self.0.gate.register_card_pcsc(tx, pcsc) {
                drop(card_worker);
                self.0
                    .gate
                    .abort_native_transition(&transition)
                    .map_err(Failure::gate)?;
                return Err(Failure::gate(code));
            }
        }
        let worker = match self.0.gate.track_native_lifecycle_worker(&transition) {
            Ok(worker) => worker,
            Err(code) => {
                drop(card_worker);
                self.0
                    .gate
                    .abort_native_transition(&transition)
                    .map_err(Failure::gate)?;
                return Err(Failure::gate(code));
            }
        };
        let service = self.clone();
        let reader = before.selection.reader.clone();
        let session = before.pcsc_session.clone();
        let target = target.to_owned();
        let aid = aid.to_owned();
        let for_target = target.clone();
        let job = tokio::task::spawn_blocking(move || {
            let _card_worker = card_worker;
            let _worker = worker;
            if !target.is_empty() {
                std::thread::sleep(Duration::from_millis(500));
            }
            if reader.starts_with("BLE:") {
                return service.ble_observation(&reader, rebuild, &target, &aid, reset, false);
            }
            if let Some(pcsc) = pcsc {
                if rebuild {
                    service
                        .0
                        .owner
                        .native_reset(&pcsc, ResetAction::Reopen)
                        .map_err(Failure::physical)?;
                    if reset.unwrap_or(!target.is_empty()) && is_estk(&reader) {
                        service
                            .0
                            .owner
                            .native_reset(&pcsc, ResetAction::Reset)
                            .map_err(Failure::physical)?;
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
                service
                    .0
                    .owner
                    .with_transaction(Some(&pcsc), &reader, |transport| {
                        read_observation(transport, rebuild, rebuild, &target, &aid)
                    })
                    .map_err(Failure::physical)?
            } else {
                if rebuild {
                    return Err(Failure::new(
                        "reader_workflow_required",
                        "读卡器重建需要既有事务能力",
                    ));
                }
                let session =
                    session.ok_or_else(|| Failure::gate("physical_binding_unconfirmed"))?;
                service
                    .0
                    .owner
                    .native_work_on_session(&session, |transport| {
                        read_observation(transport, false, false, "", "")
                    })
                    .map_err(Failure::physical)?
            }
        })
        .await
        .unwrap_or_else(|_| Err(Failure::gate("reader_task_failed")));
        let observation = match job {
            Ok(observation) => observation,
            Err(error) => {
                // A metadata rejection leaves the physical session intact;
                // uncertain transport/reset/cleanup stays prepared, blocking
                // replay and new owners until external cleanup proof.
                if error.code == "reader_metadata_failed"
                    || error.code == "reader_workflow_required"
                {
                    self.0
                        .gate
                        .abort_native_transition(&transition)
                        .map_err(Failure::gate)?;
                } else {
                    self.0
                        .gate
                        .quarantine_native_transition(&transition)
                        .map_err(Failure::gate)?;
                    if transaction.is_none() {
                        if let Some(session) = before.pcsc_session.clone() {
                            self.close_confirmed(session).await?;
                        }
                        self.0
                            .gate
                            .complete_native_failed_connect(&transition)
                            .map_err(Failure::gate)?;
                        self.retire_selection_generation(
                            &before.selection.reader,
                            before.selection.generation,
                            maintenance
                                .ok_or_else(|| Failure::gate("reader_business_unconfirmed"))?,
                        )?;
                    }
                }
                return Err(error);
            }
        };
        let committed = match self.commit(
            &transition,
            before.pcsc_session.as_ref(),
            observation.facts.clone(),
        ) {
            Ok(committed) => committed,
            Err(error) => {
                if self
                    .0
                    .gate
                    .quarantine_native_transition(&transition)
                    .is_ok()
                    && transaction.is_none()
                {
                    if let Some(session) = before.pcsc_session.clone() {
                        self.close_confirmed(session).await?;
                    }
                    self.0
                        .gate
                        .complete_native_failed_connect(&transition)
                        .map_err(Failure::gate)?;
                    self.retire_selection_generation(
                        &before.selection.reader,
                        before.selection.generation,
                        maintenance.ok_or_else(|| Failure::gate("reader_business_unconfirmed"))?,
                    )?;
                }
                return Err(error);
            }
        };
        if let Some(CardTransaction::Native { binding, .. }) = &transaction {
            self.0
                .gate
                .advance_native_pin_generation(binding, committed.generation)
                .map_err(Failure::gate)?;
        }
        let snapshot = self.snapshot();
        let finalized = if for_target.is_empty() {
            None
        } else {
            Some(finalize_projection(
                &snapshot,
                &for_target,
                observation,
                &self.0.data_root,
            ))
        };
        Ok((snapshot, finalized))
    }
    fn ble_observation(
        &self,
        reader: &str,
        rebuild: bool,
        target: &str,
        aid: &str,
        reset: Option<bool>,
        connect: bool,
    ) -> Result<Observation, Failure> {
        let socket = self
            .0
            .ble_socket
            .as_deref()
            .ok_or_else(|| Failure::new("ble_bridge_unavailable", "蓝牙读卡器桥未配置"))?;
        let permit = crate::reader_ble_apdu::acquire_io_permit(socket)
            .map_err(|e| Failure::new("reader_busy", e))?;
        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect_reserved_with_gate(
            socket,
            self.0.app_token.as_deref(),
            reader,
            permit,
            self.0.gate.clone(),
        )
        .map_err(|e| Failure::new("ble_reader_unavailable", e))?;
        if rebuild && reset == Some(true) {
            transport
                .reset()
                .map_err(|e| Failure::new("reader_reset_unconfirmed", e))?;
            std::thread::sleep(Duration::from_millis(500));
        }
        let connection = transport.connection_binding();
        let mut observation =
            read_observation(&mut transport, connect || rebuild, rebuild, target, aid)?;
        observation.facts.ble_connection = connection;
        Ok(observation)
    }
    /// Poll the connection captured when this generation was physically read.
    /// This never turns UI idle, reader-name equality or a socket error into loss.
    pub(crate) async fn reconcile_ble_presence(&self) -> Result<bool, Failure> {
        self.retained(true, move |service| async move {
            let before = service.snapshot();
            if before.selection.pending || !before.selection.reader.starts_with("BLE:") {
                return Ok(false);
            }
            let Some(binding) = service
                .0
                .gate
                .ble_connection_binding(&before.selection.reader)
            else {
                return Ok(false);
            };
            let socket = service
                .0
                .ble_socket
                .clone()
                .ok_or_else(|| Failure::new("ble_bridge_unavailable", "蓝牙读卡器桥未配置"))?;
            let token = service
                .0
                .app_token
                .clone()
                .ok_or_else(|| Failure::new("ble_bridge_unavailable", "蓝牙读卡器桥未配置"))?;
            let captured = binding.clone();
            let proof = tokio::task::spawn_blocking(move || {
                crate::reader_ble_apdu::confirmed_close(&socket, &token, &captured)
            })
            .await
            .map_err(|_| Failure::gate("reader_task_failed"))?
            .map_err(|e| Failure::new("physical_loss_unconfirmed", e))?;
            let Some(proof) = proof else {
                return Ok(false);
            };
            let transition = service
                .0
                .gate
                .prepare_native_transition(
                    &service.0.producer,
                    &before.selection,
                    Kind::Loss,
                    &binding.reader,
                    NativeAuthority::BleLoss(binding.clone()),
                )
                .map_err(Failure::gate)?;
            let legacy = match transition.legacy_loss_producer() {
                Ok(legacy) => legacy,
                Err(code) => {
                    service
                        .0
                        .gate
                        .abort_native_transition(&transition)
                        .map_err(Failure::gate)?;
                    return Err(Failure::gate(code));
                }
            };
            if let Err(code) = service
                .0
                .gate
                .commit_confirmed_ble_presence_loss(&proof, &transition)
            {
                service
                    .0
                    .gate
                    .abort_native_transition(&transition)
                    .map_err(Failure::gate)?;
                return Err(Failure::gate(code));
            }
            service
                .retire_generation(&binding.reader, before.selection.generation, legacy)
                .await?;
            Ok(true)
        })
        .await
    }
    pub(crate) async fn confirmed_loss(
        &self,
        reader: &str,
        generation: u64,
        physical: Option<LifecycleBinding>,
    ) -> Result<bool, Failure> {
        let reader = reader.to_owned();
        self.retained(false, move |service| async move {
            let before = service.snapshot();
            if before.selection.pending
                || before.selection.reader != reader
                || before.selection.generation != generation
            {
                return Ok(false);
            }
            let authority = physical
                .clone()
                .map(NativeAuthority::Loss)
                .unwrap_or(NativeAuthority::Idle);
            let transition = service
                .0
                .gate
                .prepare_native_transition(
                    &service.0.producer,
                    &before.selection,
                    Kind::Loss,
                    &reader,
                    authority,
                )
                .map_err(Failure::gate)?;
            let legacy = match transition.legacy_loss_producer() {
                Ok(legacy) => legacy,
                Err(code) => {
                    service
                        .0
                        .gate
                        .abort_native_transition(&transition)
                        .map_err(Failure::gate)?;
                    return Err(Failure::gate(code));
                }
            };
            if physical.is_some() {
                if let Err(error) = service
                    .0
                    .gate
                    .commit_confirmed_native_presence_loss(&service.0.owner, &transition)
                {
                    service
                        .0
                        .gate
                        .abort_native_transition(&transition)
                        .map_err(Failure::gate)?;
                    return Err(Failure::gate(error));
                }
            } else {
                service.commit(&transition, None, NativeReaderFacts::default())?;
            }
            service
                .retire_generation(&reader, generation, legacy)
                .await?;
            Ok(true)
        })
        .await
    }
    pub(crate) fn begin_shutdown(&self) {
        self.0
            .life
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopping = true;
    }
    pub(crate) async fn finish_shutdown(&self) -> Result<(), String> {
        self.begin_shutdown();
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.0.life.lock().unwrap_or_else(|e| e.into_inner()).jobs == 0
                && !self.0.gate.native_pre_admission_active()
            {
                break;
            }
            tokio::select! {_=changed=>{},_=tokio::time::sleep(Duration::from_millis(100))=>{}}
        }
        if self.snapshot().selection.pending {
            return Err("读卡器物理生命周期结果尚未确认".into());
        }
        if let Some(error) = self
            .0
            .life
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retirement_error
            .clone()
        {
            return Err(error);
        }
        Ok(())
    }
}

impl NativeSessionSupplier for Service {
    fn capture_reader_maintenance(
        &self,
        reader: &str,
        generation: u64,
        policy: NativePinPolicy,
        ordinary: Option<&Arc<crate::ordinary_vowifi_admission::Claim>>,
    ) -> Result<
        Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
        proletarius_domain::BackendFailure,
    > {
        let lifecycle = self.ordinary_lifecycle().map_err(|e| {
            proletarius_domain::BackendFailure::ReaderAdmissionRejected { code: e.code }
        })?;
        if let Some(claim) = ordinary {
            if policy != NativePinPolicy::Identity {
                return Err(
                    proletarius_domain::BackendFailure::ReaderAdmissionRejected {
                        code: "invalid_ordinary_capability",
                    },
                );
            }
            lifecycle
                .ordinary_admission()
                .validate_reader_claim(claim, reader, generation)
                .map_err(
                    |e| proletarius_domain::BackendFailure::ReaderAdmissionRejected {
                        code: e.code,
                    },
                )?;
            return Ok(None);
        }
        let maintenance_policy = if policy == NativePinPolicy::Mutation {
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain
        } else if policy == NativePinPolicy::Identity {
            crate::ordinary_vowifi_admission::MaintenancePolicy::RejectAllOrdinary
        } else {
            crate::ordinary_vowifi_admission::MaintenancePolicy::Reject
        };
        lifecycle
            .ordinary_admission()
            .begin_reader_maintenance(reader, generation, maintenance_policy)
            .map(Some)
            .map_err(
                |e| proletarius_domain::BackendFailure::ReaderAdmissionRejected { code: e.code },
            )
    }
    fn drain_reader_maintenance(
        self: Arc<Self>,
        maintenance: Arc<crate::ordinary_vowifi_admission::Maintenance>,
    ) -> AdmissionFuture {
        Box::pin(async move {
            if !maintenance.is_current() {
                return Err("读卡器维护能力已失效".into());
            }
            let lifecycle = {
                self.0
                    .ordinary_lifecycle
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .upgrade()
            };
            if let Some(lifecycle) = lifecycle {
                if let Some(service) = lifecycle.ordinary_service() {
                    service
                        .stop_for_maintenance(
                            &maintenance,
                            tokio::time::Instant::now() + Duration::from_secs(180),
                        )
                        .await?;
                } else if maintenance.native_claim().is_some() {
                    return Err("原生 VoWiFi 退出尚未确认".into());
                }
                Ok(())
            } else {
                Err("通信业务占用状态尚未确认".into())
            }
        })
    }

    fn pre_admit(
        self: Arc<Self>,
        reader: String,
        policy: NativePinPolicy,
        ordinary: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
        maintenance: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
    ) -> PreAdmissionFuture {
        Box::pin(async move {
            // Pure validation after the retained native drain. No external stop
            // or card work is dispatched from pre-admission.
            let _job = self.register_job(true, false).map_err(|e| e.to_string())?;
            if self
                .0
                .life
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .stopping
            {
                return Err("服务正在退出".into());
            }
            let captured = Service::snapshot(&self).selection;
            if captured.pending || captured.reader != reader || captured.generation == 0 {
                return Err("读卡器代次已变化".into());
            }
            if let Some(claim) = ordinary {
                if policy != NativePinPolicy::Identity {
                    return Err("普通会话能力不允许维护操作".into());
                }
                // The captured shared claim excludes every ordinary Go/native
                // owner. Reader pre-admission separately excludes N-wait,
                // legacy business and submitted card workers. No Go cache or
                // producer is an authority for this native preparation.
                self.ordinary_lifecycle()
                    .map_err(|error| error.to_string())?
                    .ordinary_admission()
                    .validate_reader_claim(&claim, &reader, captured.generation)
                    .map_err(|error| error.to_string())?;
                if self.0.gate.selection_snapshot() != captured {
                    return Err("读卡器代次已变化".into());
                }
                return Ok(captured.generation);
            }
            match policy {
                // Capture holds an all-ordinary fence; the exact reader pre
                // separately excludes business/N-wait/actual physical workers.
                NativePinPolicy::Identity => {
                    let maintenance = maintenance
                        .as_ref()
                        .ok_or_else(|| "读卡器维护能力未确认".to_owned())?;
                    self.ordinary_lifecycle()
                        .map_err(|error| error.to_string())?
                        .ordinary_admission()
                        .validate_reader_maintenance(
                            maintenance,
                            &reader,
                            captured.generation,
                            crate::ordinary_vowifi_admission::MaintenancePolicy::RejectAllOrdinary,
                        )
                        .map_err(|error| error.to_string())?;
                }
                NativePinPolicy::Mutation => {
                    let maintenance = maintenance
                        .as_ref()
                        .ok_or_else(|| "读卡器维护能力未确认".to_owned())?;
                    self.ordinary_lifecycle()
                        .map_err(|error| error.to_string())?
                        .ordinary_admission()
                        .validate_reader_maintenance(
                            maintenance,
                            &reader,
                            captured.generation,
                            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain,
                        )
                        .map_err(|error| error.to_string())?;
                    if !maintenance.ordinary_drained() {
                        return Err("原生 VoWiFi 退出尚未确认".into());
                    }
                }
                NativePinPolicy::CardOnly => {}
            }
            if self
                .0
                .life
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .stopping
                || self.0.gate.selection_snapshot() != captured
            {
                return Err("读卡器代次已变化".into());
            }
            Ok(captured.generation)
        })
    }
    fn admit(self: Arc<Self>, binding: NativePinBinding) -> AdmissionFuture {
        Box::pin(async move {
            if self
                .0
                .life
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .stopping
            {
                return Err("服务正在退出".into());
            }
            let snapshot = Service::snapshot(&self);
            if snapshot.selection.pending
                || snapshot.selection.reader != binding.reader
                || snapshot.selection.generation != binding.reader_generation
            {
                return Err("读卡器代次已变化".into());
            }
            Ok(())
        })
    }
    fn snapshot(&self, binding: &NativePinBinding) -> Result<Value, String> {
        let snapshot = Service::snapshot(self);
        if snapshot.selection.pending
            || snapshot.selection.reader != binding.reader
            || snapshot.selection.generation != binding.reader_generation
        {
            return Err("读卡器代次已变化".into());
        }
        let metadata = match snapshot.usim_info {
            Some(metadata) => metadata,
            None if binding.policy != NativePinPolicy::Identity => {
                json!({"iccid":"","imsi":"","mcc":"","mnc":""})
            }
            None => return Err("读卡器无 SIM 卡信息".into()),
        };
        Ok(
            json!({"reader":snapshot.selection.reader,"reader_generation":snapshot.selection.generation,
            "esim_manager_available":snapshot.esim_manager_available,"notification_aids":snapshot.notification_aids,"session":snapshot.pcsc_session,
            "identity":{"iccid":metadata["iccid"],"imsi":metadata["imsi"],"mcc":metadata["mcc"],"mnc":metadata["mnc"],
                "gid1":text(&metadata,"gid1"),"gid2":text(&metadata,"gid2"),"spn":if text(&metadata,"operator_source")=="ef_spn" {text(&metadata,"operator_name")}else{""},
                "private_identity":text(&metadata,"private_identity"),"public_identity":text(&metadata,"public_identity"),"home_domain":text(&metadata,"home_domain"),"phone_number":text(&metadata,"phone_number"),"smsc":text(&metadata,"smsc"),"smsc_psi":text(&metadata,"smsc_psi"),"smsc_target":text(&metadata,"smsc_target")}}),
        )
    }
    fn finalize(
        self: Arc<Self>,
        binding: NativePinBinding,
        context: Option<NativeCardContext>,
        target: String,
        aid: String,
    ) -> SupplierFuture {
        Box::pin(async move {
            let transaction = CardTransaction::Native {
                binding,
                purpose: "finalize".into(),
                operation_sequence: Some(1),
            };
            // The coordinator already owns its supplier worker and FIFO. This
            // retained task borrows that same capability, never a second pin.
            let pcsc = context.as_ref().map(|context| context.binding().clone());
            self.retained(true, move |service| async move {
                service
                    .observe_inner(Some(transaction), pcsc, true, &target, &aid, None)
                    .await
                    .and_then(|(_, finalized)| {
                        finalized.ok_or_else(|| {
                            Failure::new("invalid_target_iccid", "切卡目标 ICCID 不能为空")
                        })
                    })
            })
            .await
            .map_err(|e| e.to_string())
        })
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}
fn is_estk(reader: &str) -> bool {
    reader.to_ascii_lowercase().contains("estkme")
}
pub(crate) fn metadata_value(
    metadata: proletarius_reader::ReaderSimMetadata,
) -> Result<Value, Failure> {
    if !(10..=20).contains(&metadata.iccid.len())
        || !metadata.iccid.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(Failure::new(
            "reader_metadata_failed",
            "读卡器返回的 ICCID 无效，拒绝提交身份快照",
        ));
    }
    let aid = crate::reader_esim_download::encode_hex_lower(&metadata.application_aid);
    let mut value =
        serde_json::to_value(metadata).map_err(|_| Failure::gate("reader_metadata_failed"))?;
    value.as_object_mut().unwrap().remove("spn");
    value.as_object_mut().unwrap().remove("isim_aid");
    value["application_aid"] = json!(aid);
    value["loaded_at"] = json!(
        time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| Failure::gate("clock_unavailable"))?
    );
    Ok(value)
}
fn read_observation<T: proletarius_sim::ApduTransport>(
    transport: &mut T,
    tolerate: bool,
    finalize: bool,
    target: &str,
    aid: &str,
) -> Result<Observation, Failure>
where
    T::Error: std::fmt::Display,
{
    let euicc = match crate::reader_esim_download::read_lifecycle_facts(transport) {
        Ok(facts) => facts,
        Err(_) if tolerate => crate::reader_esim_download::EuiccFacts::default(),
        Err(error) => return Err(Failure::new("reader_metadata_failed", error)),
    };
    let metadata = proletarius_reader::read_sim_metadata_auto_with_transport(transport)
        .map_err(|e| e.to_string())
        .and_then(|metadata| metadata_value(metadata).map_err(|e| e.to_string()));
    let (metadata, metadata_error) = match metadata {
        Ok(metadata) => (Some(metadata), None),
        Err(error) if tolerate => (None, Some(error)),
        Err(error) => return Err(Failure::new("reader_metadata_failed", error)),
    };
    let mut warnings = Vec::new();
    let mut codes = Vec::new();
    if finalize && !target.is_empty() {
        if !euicc.available {
            warnings.push(
                "切卡命令已受理，但运营商通知服务尚未恢复，请保持读卡器连接后刷新重试".into(),
            );
            codes.push("switch_notification_manager_unavailable");
        } else {
            let aids = if aid.trim().is_empty() {
                euicc.notification_aids.clone()
            } else {
                vec![decode_aid(aid)?]
            };
            if let Err(error) =
                crate::reader_esim_download::process_lifecycle_notifications(transport, &aids)
            {
                if error.contains("仍待发送") {
                    warnings.push("切卡命令已受理，但运营商切卡通知仍待发送；请保持网络和读卡器连接后刷新重试".into());
                    codes.push("switch_notification_pending");
                } else {
                    warnings.push(format!(
                        "切卡命令已受理，但向运营商确认切卡状态失败: {error}"
                    ));
                    codes.push("switch_notification_sync_failed");
                }
            }
        }
    }
    Ok(Observation {
        facts: NativeReaderFacts {
            metadata,
            has_esim: euicc.available,
            notification_aids: euicc
                .notification_aids
                .iter()
                .map(|aid| crate::reader_esim_download::encode_hex_lower(aid))
                .collect(),
            profile_name: euicc.profile_name,
            ble_connection: None,
        },
        metadata_error,
        warnings,
        codes,
    })
}
fn decode_aid(aid: &str) -> Result<Vec<u8>, Failure> {
    let aid = aid.trim();
    if !(10..=32).contains(&aid.len())
        || aid.len() % 2 != 0
        || !aid.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(Failure::new("invalid_aid", "eSIM 应用标识无效"));
    }
    (0..aid.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&aid[i..i + 2], 16).map_err(|_| Failure::gate("invalid_aid")))
        .collect()
}
fn finalize_projection(
    snapshot: &Snapshot,
    target: &str,
    mut observation: Observation,
    data_root: &std::path::Path,
) -> EsimSwitchFinalizeResponse {
    let confirmed = observation.metadata_error.is_none()
        && snapshot
            .usim_info
            .as_ref()
            .is_some_and(|metadata| text(metadata, "iccid") == target.trim());
    if !confirmed {
        let mut warning="Profile 切换已受理，但尚未确认读卡器呈现目标卡片身份；请刷新卡片状态，确认前不要重复切卡".to_owned();
        if !is_estk(&snapshot.selection.reader) {
            warning.push_str(
                "。若仍显示旧卡，请拔插读卡器后再次确认；应用没有对未经验证的读卡器强制复位",
            );
        }
        observation.warnings.push(warning);
        observation.codes.push("switch_reader_identity_unconfirmed");
    }
    let reader_info = crate::reader_info_projection::project_native(snapshot.clone(), data_root)
        .unwrap_or_else(|_| json!({"status":snapshot.status}));
    EsimSwitchFinalizeResponse {
        target_identity_confirmed: confirmed,
        reader_refresh_warning: observation.warnings.join("；"),
        reader_refresh_warning_code: observation.codes.join(","),
        reader_info,
    }
}

#[cfg(test)]
mod tests;
