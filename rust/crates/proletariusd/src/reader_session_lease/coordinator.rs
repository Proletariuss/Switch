//! Rust-owned native lease lifetime and direct physical session supplier.
use super::*;
use crate::reader_reservation::{Gate, NativePinBinding, NativePinPolicy, NativeWorkerPermit};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::time::Instant;

use crate::ordinary_vowifi_admission::{Claim, Maintenance, MaintenancePolicy};

pub(super) struct AcquisitionReceipt {
    gate: Arc<Gate>,
    stage: Mutex<AcquisitionStage>,
    task_finished: AtomicBool,
}
enum AcquisitionStage {
    Unsubmitted,
    Preparing(crate::reader_reservation::NativePreAdmission),
    Maintenance(Arc<Maintenance>),
    Owner(Arc<Coordinator>),
}
impl AcquisitionReceipt {
    pub(super) fn new(gate: Arc<Gate>) -> Arc<Self> {
        Arc::new(Self {
            gate,
            stage: Mutex::new(AcquisitionStage::Unsubmitted),
            task_finished: AtomicBool::new(true),
        })
    }
    pub(super) fn confirmed(&self) -> bool {
        if !self.task_finished.load(Ordering::Acquire) {
            return false;
        }
        let Ok(stage) = self.stage.lock() else {
            return false;
        };
        match &*stage {
            AcquisitionStage::Unsubmitted => true,
            AcquisitionStage::Preparing(pre) => !self.gate.native_pre_admission_current(pre),
            AcquisitionStage::Owner(owner) => owner.released(),
            AcquisitionStage::Maintenance(maintenance) => !maintenance.is_current(),
        }
    }
}
struct PreparationFinished(Arc<AcquisitionReceipt>);
impl Drop for PreparationFinished {
    fn drop(&mut self) {
        self.0.task_finished.store(true, Ordering::Release);
    }
}
async fn finish_preparation(
    gate: &Gate,
    pre: Option<&crate::reader_reservation::NativePreAdmission>,
    maintenance: Option<&Arc<Maintenance>>,
) -> Result<(), BackendFailure> {
    if let Some(maintenance) = maintenance {
        // Stop was already submitted. Observe only the captured ordinary
        // owner; cancellation or an error may not replay stop or release it.
        while !maintenance.ordinary_drained() {
            sleep(POLL).await;
        }
    }
    if let Some(pre) = pre {
        gate.finish_native_pre_admission(pre).map_err(failure)?;
    }
    if let Some(maintenance) = maintenance {
        maintenance
            .release_confirmed()
            .map_err(|e| failure(e.to_string()))?;
    }
    Ok(())
}

const LIFETIME: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(250);
const PHYSICAL_TRANSACTION_CAPACITY: usize = 32;

#[derive(Clone)]
struct FinalizeRequest {
    binding: NativePinBinding,
    target_iccid: String,
    aid: String,
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Preparing,
    Active,
    Draining,
    Released,
}
struct Finalization {
    request: FinalizeRequest,
    submitted: bool,
    completed: Option<Result<EsimSwitchFinalizeResponse, String>>,
    terminal: bool,
    result: Option<Result<EsimSwitchFinalizeResponse, String>>,
}
struct State {
    phase: Phase,
    deadline: Instant,
    snapshot: Option<Arc<AcquireResponse>>,
    pcsc: Option<crate::reader_pcsc::Binding>,
    error: Option<String>,
    finalization: Option<Finalization>,
    release_timeout: Duration,
}
pub(super) struct Coordinator {
    binding: NativePinBinding,
    gate: Arc<Gate>,
    owner: crate::reader_pcsc::Owner,
    supplier: Arc<dyn NativeSessionSupplier>,
    maintenance: Option<Arc<Maintenance>>,
    cancelled: Arc<AtomicBool>,
    state: Mutex<State>,
    // All physical transactions behind this reader lease share one FIFO
    // admission queue. The mutex is owned by the actual blocking job after
    // submission, so cancelling its async waiter cannot release card access.
    transaction: Arc<tokio::sync::Mutex<()>>,
    transaction_slots: Arc<tokio::sync::Semaphore>,
    changed: watch::Sender<u64>,
}
pub(super) struct PhysicalTransactionPermit {
    _slot: tokio::sync::OwnedSemaphorePermit,
    _serial: tokio::sync::OwnedMutexGuard<()>,
}
struct CancelAcquire(Option<Arc<Coordinator>>);
impl Drop for CancelAcquire {
    fn drop(&mut self) {
        if let Some(owner) = self.0.take() {
            owner.drain();
        }
    }
}
struct CancelPreparation(Option<Arc<AtomicBool>>);
impl Drop for CancelPreparation {
    fn drop(&mut self) {
        if let Some(flag) = &self.0 {
            flag.store(true, Ordering::Release);
        }
    }
}
struct Acquired(Option<(Arc<Coordinator>, Arc<AcquireResponse>)>);
impl Drop for Acquired {
    fn drop(&mut self) {
        if let Some((owner, _)) = &self.0 {
            owner.drain();
        }
    }
}
fn admission_failure(code: &'static str) -> BackendFailure {
    if matches!(
        code,
        "card_busy" | "nwait_owns_reader" | "reader_maintenance_busy"
    ) {
        BackendFailure::ReaderAdmissionRejected { code }
    } else {
        failure(code)
    }
}
fn failure(message: impl Into<String>) -> BackendFailure {
    BackendFailure::GoBackendUnavailable(message.into())
}
impl Coordinator {
    pub(super) async fn acquire(
        _socket: &Path,
        _token: Option<&str>,
        reader: &str,
        gate: Arc<Gate>,
        owner: crate::reader_pcsc::Owner,
        policy: NativePinPolicy,
        ordinary: Option<Arc<Claim>>,
        receipt: Option<Arc<AcquisitionReceipt>>,
    ) -> Result<(Arc<Self>, Arc<AcquireResponse>), BackendFailure> {
        let selected = gate.selection_snapshot();
        if selected.pending {
            return Err(admission_failure("card_busy"));
        }
        if selected.reader != reader || selected.generation == 0 {
            return Err(admission_failure("stale_reader_generation"));
        }
        if let Some(claim) = &ordinary {
            if policy != NativePinPolicy::Identity {
                return Err(failure(
                    "ordinary reader capability requires identity policy",
                ));
            }
            claim
                .validate_reader_start(reader, selected.generation)
                .map_err(|e| BackendFailure::ReaderAdmissionRejected { code: e.code })?;
        }
        let receipt = receipt.unwrap_or_else(|| AcquisitionReceipt::new(gate.clone()));
        let supplier = gate.native_session_supplier().map_err(failure)?;
        // A maintenance slot must precede stopping Starting, whose own exact
        // reader pre can still be occupied. It also rejects existing N-wait,
        // LPA or legacy business before any stop is submitted.
        let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            supplier.capture_reader_maintenance(
                reader,
                selected.generation,
                policy,
                ordinary.as_ref(),
            )
        }));
        let maintenance = match captured {
            Ok(value) => value?,
            Err(_) => {
                return Err(failure(
                    "reader maintenance capture failed before physical submission",
                ));
            }
        };
        let pre = if let Some(maintenance) = &maintenance {
            *receipt.stage.lock().unwrap() = AcquisitionStage::Maintenance(maintenance.clone());
            None
        } else {
            let pre = gate
                .begin_native_pre_admission(reader, policy)
                .map_err(admission_failure)?;
            *receipt.stage.lock().unwrap() = AcquisitionStage::Preparing(pre.clone());
            Some(pre)
        };
        receipt.task_finished.store(false, Ordering::Release);
        let reader = reader.to_owned();
        let generation = selected.generation;
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut cancellation = CancelPreparation(Some(cancelled.clone()));
        // HTTP cancellation only sets a flag. This retained owner reconciles
        // the exact stop and physical work without releasing the shared slot.
        let task = tokio::spawn(async move {
            let _finished = PreparationFinished(receipt.clone());
            if let Some(maintenance) = &maintenance {
                if maintenance.policy() == MaintenancePolicy::StopAndDrain {
                    let source = supplier.clone();
                    let captured = maintenance.clone();
                    let drain =
                        tokio::spawn(
                            async move { source.drain_reader_maintenance(captured).await },
                        )
                        .await;
                    if !matches!(drain, Ok(Ok(()))) {
                        finish_preparation(&gate, pre.as_ref(), Some(maintenance)).await?;
                        return Err(failure(
                            "native ordinary stop outcome was reconciled without replay",
                        ));
                    }
                }
            }
            let source = supplier.clone();
            let captured_reader = reader.clone();
            let captured_ordinary = ordinary.clone();
            let captured_maintenance = maintenance.clone();
            let outcome = tokio::spawn(async move {
                source
                    .pre_admit(
                        captured_reader,
                        policy,
                        captured_ordinary,
                        captured_maintenance,
                    )
                    .await
            })
            .await;
            let expected = match outcome {
                Ok(result) => result,
                Err(_) => Err(
                    "native reader preparation worker failed; captured cleanup was reconciled"
                        .to_owned(),
                ),
            };
            let expected = match expected {
                Ok(value) => value,
                Err(error) => {
                    finish_preparation(&gate, pre.as_ref(), maintenance.as_ref()).await?;
                    return Err(failure(error));
                }
            };
            if let Some(maintenance) = &maintenance {
                while !maintenance.ordinary_drained() {
                    sleep(POLL).await;
                }
            }
            if cancelled.load(Ordering::Acquire) || expected != generation {
                finish_preparation(&gate, pre.as_ref(), maintenance.as_ref()).await?;
                return Err(failure(
                    "native reader preparation was cancelled or its generation changed",
                ));
            }
            let pre = match pre {
                Some(pre) => pre,
                None => match gate.begin_native_pre_admission(&reader, policy) {
                    Ok(pre) => {
                        *receipt.stage.lock().unwrap() = AcquisitionStage::Preparing(pre.clone());
                        pre
                    }
                    Err(code) => {
                        finish_preparation(&gate, None, maintenance.as_ref()).await?;
                        return Err(admission_failure(code));
                    }
                },
            };
            if pre.generation != generation {
                finish_preparation(&gate, Some(&pre), maintenance.as_ref()).await?;
                return Err(admission_failure("stale_reader_generation"));
            }
            // Maintenance published while this retained preparation ran must
            // cancel further startup. The already-captured pre is retired by
            // this owner, rather than passed to another ordinary generation.
            if let Some(claim) = &ordinary
                && let Err(error) = claim.validate_reader_start(&reader, generation)
            {
                finish_preparation(&gate, Some(&pre), maintenance.as_ref()).await?;
                return Err(BackendFailure::ReaderAdmissionRejected { code: error.code });
            }
            let binding = match gate.begin_native_pin_from_pre(&pre) {
                Ok(binding) => binding,
                Err(code) => {
                    finish_preparation(&gate, Some(&pre), maintenance.as_ref()).await?;
                    return Err(admission_failure(code));
                }
            };
            let acquired = Self::acquire_bound(
                binding,
                gate,
                owner,
                supplier,
                cancelled,
                maintenance,
                receipt,
            )
            .await?;
            Ok(Acquired(Some(acquired)))
        });
        let mut result = task
            .await
            .map_err(|_| failure("native reader preparation owner failed"))??;
        cancellation.0 = None;
        Ok(result
            .0
            .take()
            .expect("native acquisition returns one exact owner"))
    }
    async fn acquire_bound(
        binding: NativePinBinding,
        gate: Arc<Gate>,
        owner: crate::reader_pcsc::Owner,
        supplier: Arc<dyn NativeSessionSupplier>,
        cancelled: Arc<AtomicBool>,
        maintenance: Option<Arc<Maintenance>>,
        receipt: Arc<AcquisitionReceipt>,
    ) -> Result<(Arc<Self>, Arc<AcquireResponse>), BackendFailure> {
        // This owner exists before any physical request can be submitted. Response loss can
        // therefore never erase the identity needed for status/unpin cleanup.
        let (changed, _) = watch::channel(0);
        let this = Arc::new(Self {
            binding,
            gate,
            owner,
            supplier,
            maintenance,
            cancelled,
            transaction: Arc::new(tokio::sync::Mutex::new(())),
            transaction_slots: Arc::new(tokio::sync::Semaphore::new(PHYSICAL_TRANSACTION_CAPACITY)),
            changed,
            state: Mutex::new(State {
                phase: Phase::Preparing,
                deadline: Instant::now() + LIFETIME,
                snapshot: None,
                pcsc: None,
                error: None,
                finalization: None,
                release_timeout: RELEASE_TIMEOUT,
            }),
        });
        *receipt.stage.lock().unwrap() = AcquisitionStage::Owner(this.clone());
        let mut cancellation = CancelAcquire(Some(this.clone()));
        tokio::spawn(this.clone().drive());
        let mut changes = this.changed.subscribe();
        let result = tokio::time::timeout(LEASE_TIMEOUT, async {
            loop {
                {
                    let state = this.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.phase == Phase::Active {
                        return Ok(state
                            .snapshot
                            .as_ref()
                            .expect("active pin has a snapshot")
                            .clone());
                    }
                    if state.phase == Phase::Released {
                        return Err(failure(state.error.clone().unwrap_or_else(|| {
                            "reader native lease acquisition was retired".into()
                        })));
                    }
                    // A definite adapter rejection may still retain its authMu
                    // or physical transaction. Publish the original error only
                    // after cleanup, so compatibility fallback cannot overlap it.
                }
                if changes.changed().await.is_err() {
                    return Err(failure("reader native lease owner stopped"));
                }
            }
        })
        .await
        .map_err(|_| failure("reader native lease acquisition is unconfirmed"))?;
        let snapshot = result?;
        cancellation.0 = None;
        Ok((this, snapshot))
    }
    fn notify(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub(super) fn drain(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase != Phase::Released {
            state.phase = Phase::Draining;
        }
        let _ = self.gate.drain_native_pin(&self.binding);
        drop(state);
        self.notify();
    }
    pub(super) fn work_guard(
        &self,
    ) -> Result<NativeWorkerPermit, crate::reader_task::ReaderTaskError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase != Phase::Active
            || state.deadline <= Instant::now()
            || state.finalization.is_some()
        {
            return Err(crate::reader_task::ReaderTaskError::Busy);
        }
        self.gate
            .track_native_worker(&self.binding)
            .map_err(|_| crate::reader_task::ReaderTaskError::Busy)
    }
    pub(super) async fn transaction_guard(
        self: &Arc<Self>,
    ) -> Result<PhysicalTransactionPermit, crate::reader_task::ReaderTaskError> {
        // A try-acquire bounds the active plus waiting set. Tokio's mutex
        // provides FIFO order for the admitted requests; overflow retains the
        // existing fail-fast Busy error instead of growing an unbounded queue.
        let slot = self
            .transaction_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| crate::reader_task::ReaderTaskError::Busy)?;
        let serial = self.transaction.clone().lock_owned().await;
        Ok(PhysicalTransactionPermit {
            _slot: slot,
            _serial: serial,
        })
    }
    pub(super) fn renew(&self) -> Result<(), BackendFailure> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase != Phase::Active
            || state.deadline <= Instant::now()
            || !self.gate.native_pin_producer_current(&self.binding)
        {
            return Err(failure("reader native lease has expired or is draining"));
        }
        state.deadline = Instant::now() + LIFETIME;
        Ok(())
    }
    pub(super) fn released(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).phase == Phase::Released
    }
    #[cfg(test)]
    pub(super) fn set_release_timeout(&self, value: Duration) {
        self.state.lock().unwrap().release_timeout = value;
    }
    pub(super) async fn release(&self) -> Result<(), BackendFailure> {
        self.drain();
        let mut changes = self.changed.subscribe();
        let deadline = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .release_timeout;
        tokio::time::timeout(deadline, async {
            while !self.released() {
                changes
                    .changed()
                    .await
                    .map_err(|_| failure("reader native lease owner stopped"))?;
            }
            Ok(())
        })
        .await
        .map_err(|_| failure("reader native lease release remains unconfirmed"))?
    }
    pub(super) async fn finalize(
        &self,
        target: &str,
        aid: &str,
    ) -> Result<EsimSwitchFinalizeResponse, BackendFailure> {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = &state.finalization {
                if existing.request.target_iccid != target || existing.request.aid != aid {
                    return Err(failure(
                        "reader finalization payload conflicts with the original request",
                    ));
                }
            } else {
                if state.phase != Phase::Active || state.deadline <= Instant::now() {
                    return Err(failure("reader native lease is not active"));
                }
                self.gate
                    .begin_native_finalize(&self.binding)
                    .map_err(|code| failure(format!("reader finalize: {code}")))?;
                state.finalization = Some(Finalization {
                    request: FinalizeRequest {
                        binding: self.binding.clone(),
                        target_iccid: target.into(),
                        aid: aid.into(),
                    },
                    submitted: false,
                    completed: None,
                    terminal: false,
                    result: None,
                });
            }
        }
        self.notify();
        let mut changes = self.changed.subscribe();
        tokio::time::timeout(ESIM_SWITCH_FINALIZE_TIMEOUT, async {
            loop {
                if let Some(result) = self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .finalization
                    .as_ref()
                    .and_then(|v| v.result.clone())
                {
                    return result.map_err(failure);
                }
                changes
                    .changed()
                    .await
                    .map_err(|_| failure("reader finalization owner stopped"))?;
            }
        })
        .await
        .map_err(|_| {
            failure("reader finalization remains unconfirmed; do not repeat the card operation")
        })?
    }
    async fn prepare(&self) -> Result<Arc<AcquireResponse>, BackendFailure> {
        let supplier = self.supplier.clone();
        let pin = self.binding.clone();
        tokio::spawn(async move { supplier.admit(pin).await })
            .await
            .map_err(|_| failure("native reader admission worker failed"))?
            .map_err(failure)?;
        if self.cancelled.load(Ordering::Acquire) {
            return Err(failure("native reader acquisition was cancelled"));
        }
        let raw = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.supplier.snapshot(&self.binding)
        }))
        .map_err(|_| failure("native reader snapshot worker failed"))?
        .map_err(failure)?;
        let mut snapshot: AcquireResponse = serde_json::from_value(raw)
            .map_err(|_| failure("native reader snapshot is malformed"))?;
        snapshot.lease_id = self.binding.lease_id.clone();
        // Identity validation precedes physical acquisition, but the exact
        // cleanup binding is published before the blocking job can begin.
        validate_snapshot(&snapshot, &self.binding)?;
        if self.binding.reader.starts_with("BLE:") {
            if snapshot.session.is_some() || snapshot.pcsc.is_some() {
                return Err(failure("BLE snapshot returned PC/SC authority"));
            }
        } else {
            let session = snapshot
                .session
                .as_ref()
                .ok_or_else(|| failure("native reader lacks a physical session"))?;
            let binding = self
                .owner
                .native_pin_binding(session, &self.binding)
                .map_err(|e| failure(e.to_string()))?;
            self.state.lock().unwrap_or_else(|e| e.into_inner()).pcsc = Some(binding.clone());
            let permit = self
                .gate
                .track_native_supplier_worker(&self.binding, false)
                .map_err(failure)?;
            let owner = self.owner.clone();
            let captured = binding.clone();
            let gate = self.gate.clone();
            let pin = self.binding.clone();
            let cancelled = self.cancelled.clone();
            tokio::task::spawn_blocking(move || {
                let _actual_worker = permit;
                gate.validate_native_pin(&pin, "pin")
                    .map_err(|code| failure(format!("native reader acquisition: {code}")))?;
                let admission = Arc::new(move || {
                    !cancelled.load(Ordering::Acquire)
                        && gate.validate_native_pin(&pin, "pin").is_ok()
                });
                owner
                    .native_acquire_binding(&captured, LIFETIME, admission)
                    .map_err(|e| failure(e.to_string()))
            })
            .await
            .map_err(|_| failure("native reader acquisition worker failed"))??;
            snapshot.pcsc = Some(
                serde_json::to_value(binding)
                    .map_err(|_| failure("native binding serialization failed"))?,
            );
        }
        Ok(Arc::new(snapshot))
    }
    async fn finalize_step(&self) -> bool {
        let request = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(finalization) = state.finalization.as_mut().filter(|v| !v.terminal) else {
                return false;
            };
            if finalization.submitted {
                None
            } else {
                finalization.submitted = true;
                Some(finalization.request.clone())
            }
        };
        if let Some(request) = request {
            let result = async {
                let permit = self
                    .gate
                    .track_native_supplier_worker(&self.binding, true)
                    .map_err(str::to_owned)?;
                let serial = self.transaction.clone().lock_owned().await;
                let binding = self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pcsc
                    .clone();
                let context = binding.map(|binding| NativeCardContext {
                    owner: self.owner.clone(),
                    binding,
                });
                let supplier = self.supplier.clone();
                tokio::spawn(async move {
                    let _actual_worker = permit;
                    let _fifo = serial;
                    supplier
                        .finalize(request.binding, context, request.target_iccid, request.aid)
                        .await
                })
                .await
                .map_err(|_| "native reader finalization worker failed".to_owned())?
            }
            .await;
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .finalization
                .as_mut()
                .unwrap()
                .completed = Some(result);
        }
        // Even a supplier failure may leave a cancelled blocking/bridge job.
        // Never resubmit finalization; wait for its actual physical drain.
        if !self.gate.native_physical_workers_idle(&self.binding) {
            return true;
        }
        let physical = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pcsc
            .clone();
        if let Some(binding) = physical {
            if !self.physical(binding, true).await {
                return true;
            }
        }
        let invalidated = match self.gate.retire_native_finalizer_selection(&self.binding) {
            Ok(value) => value,
            Err(_) => return true,
        };
        if self.gate.finish_native_finalize(&self.binding).is_err() {
            return true;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let finalization = state.finalization.as_mut().unwrap();
        finalization.result = Some(if invalidated {
            Err("reader finalization could not confirm selection; reselect the reader before continuing".into())
        } else {
            finalization
                .completed
                .take()
                .unwrap_or_else(|| Err("native finalization receipt is unconfirmed".into()))
        });
        finalization.terminal = true;
        state.phase = Phase::Draining;
        let _ = self.gate.drain_native_pin(&self.binding);
        drop(state);
        self.notify();
        false
    }
    async fn physical(&self, binding: crate::reader_pcsc::Binding, release: bool) -> bool {
        let owner = self.owner.clone();
        tokio::task::spawn_blocking(move || {
            if release {
                let _ = owner.release(&binding);
                owner.status(&binding).is_ok_and(|value| {
                    value.get("terminal").and_then(serde_json::Value::as_bool) == Some(true)
                })
            } else {
                owner.renew(&binding, LIFETIME).is_ok()
            }
        })
        .await
        .unwrap_or(false)
    }
    async fn drive(self: Arc<Self>) {
        let mut prepared = false;
        let mut last_physical_renew = Instant::now();
        let mut changes = self.changed.subscribe();
        loop {
            let phase = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.deadline <= Instant::now()
                    || self.cancelled.load(Ordering::Acquire)
                    || !self.gate.native_pin_producer_current(&self.binding)
                {
                    if state.phase != Phase::Released {
                        state.phase = Phase::Draining;
                    }
                    let _ = self.gate.drain_native_pin(&self.binding);
                }
                state.phase
            };
            if phase == Phase::Released {
                return;
            }
            if self.finalize_step().await {
                tokio::select! { _ = sleep(POLL) => {}, _ = changes.changed() => {} }
                continue;
            }
            match phase {
                Phase::Preparing => {
                    if !prepared {
                        prepared = true;
                        let snapshot = self.prepare().await;
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        match snapshot {
                            Ok(snapshot)
                                if state.phase == Phase::Preparing
                                    && !self.cancelled.load(Ordering::Acquire) =>
                            {
                                match self.gate.activate_native_pin(&self.binding) {
                                    Ok(()) => {
                                        state.snapshot = Some(snapshot);
                                        state.phase = Phase::Active;
                                    }
                                    Err(code) => {
                                        state.error = Some(code.to_owned());
                                        state.phase = Phase::Draining;
                                    }
                                }
                            }
                            Err(error) => {
                                state.error = Some(error.to_string());
                                state.phase = Phase::Draining;
                            }
                            _ => {
                                state.phase = Phase::Draining;
                            }
                        }
                        let draining = state.phase == Phase::Draining;
                        drop(state);
                        if draining {
                            let _ = self.gate.drain_native_pin(&self.binding);
                        }
                        self.notify();
                    }
                }
                Phase::Active => {
                    let pcsc = self
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pcsc
                        .clone();
                    if let Some(pcsc) =
                        pcsc.filter(|_| last_physical_renew.elapsed() >= LEASE_RENEW_INTERVAL)
                    {
                        if !self.physical(pcsc, false).await {
                            self.drain();
                        }
                        last_physical_renew = Instant::now();
                    }
                }
                Phase::Draining => {
                    let _ = self.gate.drain_native_pin(&self.binding);
                    if self.gate.native_workers_idle(&self.binding) {
                        let pcsc = self
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .pcsc
                            .clone();
                        let physical_done = match pcsc {
                            Some(binding) => self.physical(binding, true).await,
                            None => true,
                        };
                        if physical_done && self.gate.finish_native_pin(&self.binding).is_ok() {
                            if let Some(maintenance) = &self.maintenance {
                                if maintenance.release_confirmed().is_err() {
                                    // Logical pin already retired; retain the shared barrier
                                    // rather than claiming unknown maintenance completion.
                                    self.state.lock().unwrap_or_else(|e| e.into_inner()).error =
                                        Some("reader maintenance release unconfirmed".into());
                                    self.notify();
                                    return;
                                }
                            }
                            self.state.lock().unwrap_or_else(|e| e.into_inner()).phase =
                                Phase::Released;
                            self.notify();
                            return;
                        }
                    }
                }
                Phase::Released => return,
            }
            tokio::select! { _ = sleep(POLL) => {}, _ = changes.changed() => {} }
        }
    }
}
fn validate_snapshot(
    snapshot: &AcquireResponse,
    binding: &NativePinBinding,
) -> Result<(), BackendFailure> {
    let identity = &snapshot.identity;
    if snapshot.lease_id != binding.lease_id
        || snapshot.reader != binding.reader
        || snapshot.reader_generation != binding.reader_generation
        || (binding.policy == NativePinPolicy::Identity
            && !(valid_digits(&identity.iccid, 18, 22)
                && valid_digits(&identity.imsi, 5, 15)
                && valid_digits(&identity.mcc, 3, 3)
                && valid_digits(&identity.mnc, 2, 3)))
        || !valid_notification_aids(&snapshot.notification_aids)
        || [&identity.private_identity, &identity.public_identity]
            .into_iter()
            .any(|v| v.len() > 512 || v.bytes().any(|b| b.is_ascii_control()))
    {
        return Err(failure("reader pin returned an invalid immutable snapshot"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
