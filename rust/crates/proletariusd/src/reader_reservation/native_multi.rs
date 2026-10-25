//! Native N-wait parent and short card capabilities. No Go process, HTTP
//! principal, caller lifetime or timeout can manufacture physical completion.
use super::*;
use crate::reader_pcsc::{Binding, Failure, LifecycleBinding, Owner};
use operation::{Authority, Entry, OperationBinding, Phase};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tokio::sync::watch;

const TTL: Duration = Duration::from_secs(120);

/// Minted only by the native gate, never deserialized from a wire request.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeMultiBinding {
    producer: String,
    token: String,
    sequence: u64,
    reader: String,
    generation: u64,
    physical: Option<LifecycleBinding>,
    ble: Option<crate::reader_ble_apdu::ConnectionBinding>,
}
impl NativeMultiBinding {
    pub(crate) fn reader(&self) -> &str {
        &self.reader
    }
    pub(crate) fn reader_generation(&self) -> u64 {
        self.generation
    }
}
#[derive(Default)]
pub(super) struct NativeMultiState {
    owner: Option<NativeMultiBinding>,
    sequence: u64,
    confirmed_loss: Option<lifecycle::NativeReceipt>,
    last_loss_released: Option<NativeMultiBinding>,
}
impl NativeMultiState {
    pub(super) fn active(&self) -> bool {
        self.owner.is_some()
    }
    pub(super) fn matches(&self, b: &OperationBinding) -> bool {
        self.owner.as_ref().is_some_and(|p| {
            b.producer_id == p.producer
                && b.reader == p.reader
                && b.reader_generation == p.generation
                && matches!(&b.authority, Authority::Business {token,sequence,role}
                    if token == &p.token && *sequence == p.sequence && role == "multistandby")
        })
    }
    pub(super) fn retain_confirmed_loss(&mut self, receipt: lifecycle::NativeReceipt) {
        if self.owner.as_ref().is_some_and(|parent| {
            parent.physical.as_ref().is_some_and(|physical| {
                receipt.confirms_presence_loss(&parent.reader, parent.generation, physical)
            }) || parent.ble.as_ref().is_some_and(|ble| {
                receipt.confirms_ble_loss(&parent.producer, &parent.reader, parent.generation, ble)
            })
        }) {
            self.confirmed_loss = Some(receipt);
        }
    }
    fn next(&mut self) -> Result<u64, &'static str> {
        self.sequence = self.sequence.checked_add(1).ok_or("sequence_exhausted")?;
        Ok(self.sequence)
    }
}

struct Retirement {
    gate: Arc<Gate>,
    binding: OperationBinding,
}
impl Drop for Retirement {
    fn drop(&mut self) {
        let mut s = self.gate.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = s.operations.entries.get_mut(&self.binding.operation_token)
            && e.native
            && e.binding == self.binding
            && e.phase == Phase::Active
        {
            e.phase = Phase::Draining;
        }
    }
}
#[derive(Clone)]
pub(crate) struct NativeMultiOperation {
    pub(crate) transaction: CardTransaction,
    pub(crate) pcsc: Option<Binding>,
    completed: watch::Receiver<bool>,
    // Last handle drop only starts retained retirement. It never releases FIFO
    // or parent ownership; the worker and drive still have to actually finish.
    _retirement: Arc<Retirement>,
}
fn state_ble_binding(s: &State, reader: &str) -> Option<crate::reader_ble_apdu::ConnectionBinding> {
    s.selection.ble_connection().filter(|b| b.reader == reader)
}
fn current(s: &State, parent: &NativeMultiBinding) -> bool {
    s.native_multi.owner.as_ref() == Some(parent)
        && s.native_reader_producer.as_deref() == Some(parent.producer.as_str())
        && s.owner.as_deref() == Some(parent.token.as_str())
        && s.owner_reader.as_deref() == Some(parent.reader.as_str())
        && s.owner_generation == Some(parent.generation)
        && s.selection.native_owned()
        && !s.selection.pending()
        && s.selection.confirms(&parent.reader, parent.generation)
}
fn failure(code: &'static str) -> Failure {
    Failure {
        status: StatusCode::CONFLICT,
        code,
        message: "N待卡片事务未完成".into(),
        submitted: false,
        uncertain: false,
    }
}
impl Gate {
    pub(crate) fn acquire_native_multi(
        &self,
        reader: &str,
        generation: u64,
    ) -> Result<NativeMultiBinding, &'static str> {
        let token = crate::multistandby_store::random_token().map_err(|_| "entropy_unavailable")?;
        if !valid_operation_binding(&token, reader, generation) {
            return Err("invalid_reader_binding");
        }
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        let producer = s
            .native_reader_producer
            .clone()
            .ok_or("native_producer_unavailable")?;
        if !s.selection.native_owned()
            || s.selection.pending()
            || !s.selection.confirms(reader, generation)
        {
            return Err("stale_reader_generation");
        }
        if s.native_multi.active()
            || s.business.owner.is_some()
            || s.native.active()
            || s.legacy_business_guard.is_some()
            || s.ble_control_owner.is_some()
            || s.owner.is_some()
            || s.operations.active()
            || business::physical_workers(&s, None) != 0
        {
            return Err("card_busy");
        }
        let physical = if reader.starts_with("BLE:") {
            None
        } else {
            let physical = s
                .selection
                .physical
                .as_ref()
                .ok_or("stale_reader_session")?;
            if physical.reader != reader || physical.producer != producer {
                return Err("stale_reader_session");
            }
            Some(physical.clone())
        };
        let sequence = s.native_multi.next()?;
        business::install_multistandby_owner(&mut s, &token, reader, generation)?;
        let ble = if reader.starts_with("BLE:") {
            state_ble_binding(&s, reader)
        } else {
            None
        };
        let binding = NativeMultiBinding {
            producer,
            token,
            sequence,
            reader: reader.into(),
            generation,
            physical,
            ble,
        };
        s.native_multi.confirmed_loss = None;
        s.native_multi.owner = Some(binding.clone());
        Ok(binding)
    }
    pub(crate) fn native_multi_current(&self, parent: &NativeMultiBinding) -> bool {
        self.0.lock().is_ok_and(|s| current(&s, parent))
    }
    pub(crate) fn begin_native_multi_operation(
        self: &Arc<Self>,
        owner: Owner,
        parent: &NativeMultiBinding,
    ) -> Result<NativeMultiOperation, &'static str> {
        // No HTTP acquire accepts this issuer. The private native entry and
        // parent must both exist before any card adapter can authorize I/O.
        let token = crate::multistandby_store::random_token().map_err(|_| "entropy_unavailable")?;
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if !current(&s, parent) {
            return Err("owner_mismatch");
        }
        if s.native.active()
            || s.ble_control_owner.is_some()
            || business::physical_workers(&s, None) != 0
        {
            return Err("card_busy");
        }
        let sequence = s.native_multi.next()?;
        let binding = OperationBinding {
            producer_id: parent.producer.clone(),
            operation_token: token.clone(),
            operation_sequence: sequence,
            reader: parent.reader.clone(),
            reader_generation: parent.generation,
            purpose: "card".into(),
            target_reader: None,
            authority: Authority::Business {
                token: parent.token.clone(),
                sequence: parent.sequence,
                role: "multistandby".into(),
            },
        };
        let pcsc = if parent.reader.starts_with("BLE:") {
            None
        } else {
            let physical = s
                .selection
                .physical
                .as_ref()
                .ok_or("stale_reader_session")?;
            if physical.reader != parent.reader || physical.producer != parent.producer {
                return Err("stale_reader_session");
            }
            Some(Binding {
                producer_id: parent.producer.clone(),
                reader: parent.reader.clone(),
                session_id: physical.session_id.clone(),
                lease_id: token.clone(),
                lease_seq: sequence,
            })
        };
        let (completed, observed) = watch::channel(false);
        s.operations.entries.insert(
            token.clone(),
            Entry {
                binding: binding.clone(),
                reader: parent.reader.clone(),
                generation: parent.generation,
                phase: Phase::Active,
                deadline: Instant::now() + TTL,
                jobs: 0,
                pcsc: pcsc.iter().cloned().collect(),
                sessions: Vec::new(),
                native: true,
                native_completed: Some(completed),
            },
        );
        s.operation_workers
            .insert(token, (parent.reader.clone(), parent.generation));
        drop(s);
        tokio::spawn(operation::drive(self.clone(), owner, binding.clone()));
        Ok(NativeMultiOperation {
            transaction: CardTransaction::Operation {
                binding: binding.clone(),
            },
            pcsc,
            completed: observed,
            _retirement: Arc::new(Retirement {
                gate: self.clone(),
                binding,
            }),
        })
    }
    pub(crate) async fn acquire_native_multi_pcsc(
        self: &Arc<Self>,
        owner: Owner,
        operation: &NativeMultiOperation,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), Failure> {
        let Some(binding) = operation.pcsc.clone() else {
            return Ok(());
        };
        // Acquiring a PC/SC transaction may itself block. Keep the logical job
        // through the real worker reply even if its async caller disappears.
        let permit = self
            .track_card_io(
                &operation.transaction,
                &binding.producer_id,
                &binding.reader,
            )
            .map_err(failure)?;
        let gate = self.clone();
        let tx = operation.transaction.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let producer = binding.producer_id.clone();
            let reader = binding.reader.clone();
            let valid: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                !cancelled.load(Ordering::Acquire)
                    && gate
                        .validate_card_workflow(
                            &tx,
                            &reader,
                            match &tx {
                                CardTransaction::Operation { binding } => binding.reader_generation,
                                _ => 0,
                            },
                        )
                        .is_ok()
                    && gate
                        .0
                        .lock()
                        .is_ok_and(|s| operation::validate_io(&s, &tx, &producer, &reader).is_ok())
            });
            if !valid() {
                return Err(failure("owner_expired"));
            }
            owner
                .native_acquire_binding(&binding, TTL, valid)
                .map(|_| ())
        })
        .await
        .map_err(|_| failure("reader_worker_failed"))?
    }
    pub(crate) fn renew_native_multi_operation(
        &self,
        operation: &NativeMultiOperation,
    ) -> Result<(), &'static str> {
        let CardTransaction::Operation { binding } = &operation.transaction else {
            return Err("owner_mismatch");
        };
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        operation::validate_io(
            &s,
            &operation.transaction,
            &binding.producer_id,
            &binding.reader,
        )?;
        let entry = s
            .operations
            .entries
            .get_mut(&binding.operation_token)
            .filter(|e| e.native)
            .ok_or("owner_mismatch")?;
        entry.deadline = Instant::now() + TTL;
        Ok(())
    }
    pub(crate) async fn finish_native_multi_operation(
        &self,
        operation: &NativeMultiOperation,
    ) -> Result<(), &'static str> {
        let CardTransaction::Operation { binding } = &operation.transaction else {
            return Err("owner_mismatch");
        };
        let mut completed = operation.completed.clone();
        if *completed.borrow() {
            return Ok(());
        }
        {
            let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
            let e = s
                .operations
                .entries
                .get_mut(&binding.operation_token)
                .filter(|e| e.native && e.binding == *binding)
                .ok_or("operation_unknown")?;
            if e.phase == Phase::Active {
                e.phase = Phase::Draining;
            }
        }
        loop {
            if *completed.borrow() {
                return Ok(());
            }
            if completed.changed().await.is_err() {
                return Err("physical_cleanup_unconfirmed");
            }
        }
    }
    pub(crate) fn release_native_multi(
        &self,
        parent: &NativeMultiBinding,
    ) -> Result<(), &'static str> {
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if s.native_multi.owner.as_ref() != Some(parent) {
            return if s.last_released.as_deref() == Some(parent.token.as_str()) {
                Ok(())
            } else {
                Err("owner_mismatch")
            };
        }
        // Ordinary completion requires the original reader generation. Loss
        // retirement must use its separately retained exact physical receipt.
        if !current(&s, parent) {
            return Err("stale_reader_generation");
        }
        if s.operations
            .borrows_business(&parent.token, parent.sequence)
        {
            return Err("card_busy");
        }
        business::release_multistandby_owner(
            &mut s,
            &parent.token,
            &parent.reader,
            parent.generation,
        )?;
        s.native_multi.owner = None;
        s.native_multi.confirmed_loss = None;
        Ok(())
    }
    /// True means the exact old parent was retired after confirmed physical
    /// disappearance; it does not mean its original eSIM profile was restored.
    /// BLE requires its captured connection/close-operation receipt; legacy
    /// bindings without that identity remain fail-closed.
    pub(crate) fn release_native_multi_after_confirmed_loss(
        &self,
        parent: &NativeMultiBinding,
    ) -> Result<bool, &'static str> {
        let mut s = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if s.native_multi.last_loss_released.as_ref() == Some(parent) {
            return Ok(true);
        }
        if s.native_multi.owner.as_ref() != Some(parent) {
            return Ok(false);
        }
        if !s
            .native_multi
            .confirmed_loss
            .as_ref()
            .is_some_and(|receipt| {
                parent.physical.as_ref().is_some_and(|physical| {
                    receipt.confirms_presence_loss(&parent.reader, parent.generation, physical)
                }) || parent.ble.as_ref().is_some_and(|ble| {
                    receipt.confirms_ble_loss(
                        &parent.producer,
                        &parent.reader,
                        parent.generation,
                        ble,
                    )
                })
            })
            || s.selection.pending()
            || s.operations
                .borrows_business(&parent.token, parent.sequence)
            || business::physical_workers(&s, None) != 0
        {
            return Ok(false);
        }
        business::release_multistandby_owner(
            &mut s,
            &parent.token,
            &parent.reader,
            parent.generation,
        )?;
        s.native_multi.owner = None;
        s.native_multi.confirmed_loss = None;
        s.native_multi.last_loss_released = Some(parent.clone());
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
