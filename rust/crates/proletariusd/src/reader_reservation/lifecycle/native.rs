//! Typed native reader transitions. Prepared work retains admission until its
//! actual card worker and cleanup have ended; a receipt never replays I/O.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeTransitionKind {
    Connect,
    Disconnect,
    Identity,
    Rebuild,
    Loss,
}
impl NativeTransitionKind {
    fn name(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Disconnect | Self::Loss => "disconnect",
            Self::Identity => "identity",
            Self::Rebuild => "rebuild",
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum NativeAuthority {
    Idle,
    Transaction(CardTransaction),
    Loss(LifecycleBinding),
    BleLoss(crate::reader_ble_apdu::ConnectionBinding),
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NativeReaderFacts {
    pub(crate) metadata: Option<Value>,
    pub(crate) has_esim: bool,
    pub(crate) notification_aids: Vec<String>,
    pub(crate) profile_name: String,
    #[serde(skip)]
    pub(crate) ble_connection: Option<crate::reader_ble_apdu::ConnectionBinding>,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct NativeIntent {
    pub(super) producer: String,
    pub(super) token: String,
    sequence: u64,
    kind: NativeTransitionKind,
    expected: SelectionSnapshot,
    reader: String,
    pub(super) authority: NativeAuthority,
    legacy_loss: Result<Option<String>, &'static str>,
}
#[derive(Clone)]
pub(in crate::reader_reservation) struct NativeReceipt {
    pub(super) intent: NativeIntent,
    pub(super) snapshot: SelectionSnapshot,
    pub(super) facts: NativeReaderFacts,
    pub(super) physical: Option<LifecycleBinding>,
    pub(super) aborted: bool,
}
impl NativeReceipt {
    pub(in crate::reader_reservation) fn confirms_ble_loss(
        &self,
        producer: &str,
        reader: &str,
        generation: u64,
        connection: &crate::reader_ble_apdu::ConnectionBinding,
    ) -> bool {
        !self.aborted
            && self.intent.kind == NativeTransitionKind::Loss
            && self.intent.producer == producer
            && self.intent.expected.reader == reader
            && self.intent.expected.generation == generation
            && self.intent.reader == connection.reader
            && matches!(&self.intent.authority,NativeAuthority::BleLoss(bound) if bound==connection)
            && self.physical.is_none()
            && self.snapshot.reader.is_empty()
            && generation.checked_add(1) == Some(self.snapshot.generation)
    }
    pub(in crate::reader_reservation) fn confirms_presence_loss(
        &self,
        reader: &str,
        generation: u64,
        physical: &LifecycleBinding,
    ) -> bool {
        !self.aborted
            && self.intent.kind == NativeTransitionKind::Loss
            && self.intent.expected.reader == reader
            && self.intent.expected.generation == generation
            && self.intent.producer == physical.producer
            && self.intent.reader == physical.reader
            && matches!(&self.intent.authority, NativeAuthority::Loss(bound) if bound == physical)
            && self.physical.is_none()
            && self.snapshot.reader.is_empty()
            && generation.checked_add(1) == Some(self.snapshot.generation)
    }
}
#[derive(Clone)]
pub(crate) struct NativeTransition(NativeIntent);
impl NativeTransition {
    pub(crate) fn legacy_loss_producer(&self) -> Result<Option<String>, &'static str> {
        self.0.legacy_loss.clone()
    }
}
pub(crate) struct NativeLifecycleWorker(Arc<Gate>, String);
impl Drop for NativeLifecycleWorker {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap_or_else(|error| error.into_inner());
        if state
            .selection
            .native_pending
            .as_ref()
            .is_some_and(|pending| pending.token == self.1)
        {
            state.selection.native_workers = state.selection.native_workers.saturating_sub(1);
        }
    }
}

impl Gate {
    pub(crate) fn ble_connection_binding(
        &self,
        reader: &str,
    ) -> Option<crate::reader_ble_apdu::ConnectionBinding> {
        let state = self.0.lock().ok()?;
        if state.selection.snapshot.reader != reader {
            return None;
        }
        state
            .selection
            .native_facts
            .as_ref()?
            .ble_connection
            .clone()
    }
    /// A short N-wait workflow borrows its existing business transaction; it
    /// cannot mint an ordinary VoWiFi lease or change the selected generation.
    pub(crate) fn validate_card_workflow(
        &self,
        transaction: &CardTransaction,
        reader: &str,
        generation: u64,
    ) -> Result<(), &'static str> {
        let state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        validate_multistandby_workflow(&state, transaction, reader, generation)
    }

    pub(crate) fn publish_multistandby_metadata(
        &self,
        transaction: &CardTransaction,
        reader: &str,
        generation: u64,
        mut facts: NativeReaderFacts,
    ) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        validate_multistandby_workflow(&state, transaction, reader, generation)?;
        let CardTransaction::Operation { binding } = transaction else {
            return Err("owner_mismatch");
        };
        if !state.operations.workflow_running(&binding.operation_token) {
            return Err("reader_physical_work_pending");
        }
        if facts.metadata.is_some() {
            state.selection.physical_metadata_epoch =
                state.selection.physical_metadata_epoch.saturating_add(1);
        }
        state.reader_metadata = facts
            .metadata
            .clone()
            .map(|metadata| (reader.to_owned(), generation, metadata));
        facts.ble_connection = state
            .selection
            .native_facts
            .as_ref()
            .and_then(|f| f.ble_connection.clone());
        state.selection.native_facts = Some(facts);
        Ok(())
    }

    pub(crate) fn native_transaction_current(&self, transaction: &CardTransaction) -> bool {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match transaction {
            CardTransaction::Operation { binding } => super::super::operation::validate_io(
                &state,
                transaction,
                &binding.producer_id,
                &binding.reader,
            )
            .is_ok(),
            CardTransaction::Native {
                binding,
                purpose,
                operation_sequence,
            } => {
                purpose == "finalize"
                    && *operation_sequence == Some(1)
                    && state.native.finalizing(binding)
            }
        }
    }

    pub(crate) fn quarantine_native_transition(
        &self,
        transition: &NativeTransition,
    ) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.selection.native_pending.as_ref() != Some(&transition.0)
            || state.selection.native_workers != 0
        {
            return Err("reader_physical_work_pending");
        }
        state.selection.native_unconfirmed = true;
        Ok(())
    }
    pub(crate) fn complete_native_failed_connect(
        &self,
        transition: &NativeTransition,
    ) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.selection.native_pending.as_ref() != Some(&transition.0)
            || state.selection.native_workers != 0
            || (!matches!(
                transition.0.kind,
                NativeTransitionKind::Connect | NativeTransitionKind::Identity
            ) || !matches!(transition.0.authority, NativeAuthority::Idle))
        {
            return Err("reader_physical_work_pending");
        }
        state.selection.snapshot = SelectionSnapshot {
            generation: transition
                .0
                .expected
                .generation
                .checked_add(1)
                .ok_or("generation_exhausted")?,
            ..Default::default()
        };
        state.selection.physical = None;
        state.selection.native_facts = None;
        state.reader_metadata = None;
        state.selection.native_pending = None;
        state.selection.native_unconfirmed = false;
        let snapshot = state.selection.snapshot.clone();
        state.selection.native_terminal.push_back(NativeReceipt {
            intent: transition.0.clone(),
            snapshot,
            facts: NativeReaderFacts::default(),
            physical: None,
            aborted: true,
        });
        while state.selection.native_terminal.len() > 64 {
            state.selection.native_terminal.pop_front();
        }
        super::super::business::finish_retired_business(&mut state);
        Ok(())
    }

    pub(crate) fn native_phone_update(&self, reader: &str, generation: u64, number: &str) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if number.trim().is_empty() || !state.selection.confirms(reader, generation) {
            return false;
        }
        let Some(metadata) = state
            .selection
            .native_facts
            .as_mut()
            .and_then(|f| f.metadata.as_mut())
            .and_then(Value::as_object_mut)
        else {
            return false;
        };
        metadata.insert(
            "phone_number".into(),
            Value::String(number.trim().to_owned()),
        );
        let metadata = Value::Object(metadata.clone());
        state.reader_metadata = Some((reader.into(), generation, metadata));
        true
    }

    pub(crate) fn native_registered_identity_current(
        &self,
        reader: &str,
        generation: u64,
        iccid: &str,
        imsi: &str,
    ) -> bool {
        self.0
            .lock()
            .is_ok_and(|state| registered_identity_current(&state, reader, generation, iccid, imsi))
    }
    pub(crate) fn native_registered_phone_update(
        &self,
        reader: &str,
        generation: u64,
        iccid: &str,
        imsi: &str,
        number: &str,
    ) -> bool {
        let Ok(mut state) = self.0.lock() else {
            return false;
        };
        if crate::profile_notes::is_missing_phone(number)
            || !registered_identity_current(&state, reader, generation, iccid, imsi)
        {
            return false;
        }
        let metadata = state
            .selection
            .native_facts
            .as_mut()
            .and_then(|facts| facts.metadata.as_mut())
            .and_then(Value::as_object_mut)
            .expect("identity check requires metadata object");
        metadata.insert("phone_number".into(), Value::String(number.trim().into()));
        let metadata = Value::Object(metadata.clone());
        state.reader_metadata = Some((reader.into(), generation, metadata));
        true
    }

    pub(crate) fn initialize_native_lifecycle(&self, producer: &str) -> Result<(), &'static str> {
        if producer.is_empty() || producer.len() > 128 || producer.chars().any(char::is_control) {
            return Err("invalid_native_producer");
        }
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        match &state.selection.native_producer {
            Some(current) if current != producer => return Err("native_reader_producer_conflict"),
            Some(_) => return Ok(()),
            None if state.selection.pending() || !state.selection.snapshot.reader.is_empty() => {
                return Err("reader_lifecycle_pending");
            }
            None => {}
        }
        state.selection.native_producer = Some(producer.into());
        Ok(())
    }
    pub(crate) fn native_lifecycle_owned(&self) -> bool {
        self.0
            .lock()
            .map(|state| state.selection.native_owned())
            .unwrap_or(true)
    }
    pub(crate) fn native_reader_snapshot(
        &self,
    ) -> (
        SelectionSnapshot,
        Option<NativeReaderFacts>,
        Option<LifecycleBinding>,
    ) {
        let (selection, facts, physical, _) = self.native_reader_snapshot_with_epoch();
        (selection, facts, physical)
    }
    /// A physical publication proof and its facts share one Gate lock. Never
    /// pair a separately loaded epoch with a newer/older metadata snapshot.
    pub(crate) fn native_reader_snapshot_with_epoch(
        &self,
    ) -> (
        SelectionSnapshot,
        Option<NativeReaderFacts>,
        Option<LifecycleBinding>,
        u64,
    ) {
        let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        (
            state.selection.observed(),
            state.selection.native_facts.clone(),
            state.selection.physical.clone(),
            state.selection.physical_metadata_epoch,
        )
    }
    pub(crate) fn prepare_native_transition(
        &self,
        producer: &str,
        expected: &SelectionSnapshot,
        kind: NativeTransitionKind,
        reader: &str,
        authority: NativeAuthority,
    ) -> Result<NativeTransition, &'static str> {
        let token = crate::multistandby_store::random_token().map_err(|_| "entropy_unavailable")?;
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.selection.native_producer.as_deref() != Some(producer) {
            return Err("stale_native_producer");
        }
        if state.selection.pending() || expected.pending {
            return Err("reader_lifecycle_pending");
        }
        if expected.generation == u64::MAX {
            return Err("generation_exhausted");
        }
        if state.selection.snapshot != *expected {
            return Err("stale_reader_generation");
        }
        if reader.trim().is_empty() || reader.len() > 512 || reader.chars().any(char::is_control) {
            return Err("invalid_reader_binding");
        }
        if kind != NativeTransitionKind::Connect && reader != expected.reader {
            return Err("stale_reader_generation");
        }
        if !state.ble_io_workers.is_empty() {
            return Err("card_busy");
        }
        match &authority {
            NativeAuthority::Idle
                if state.business.owner.is_none()
                    && !state.native.active()
                    && state.owner.is_none()
                    && !state.operations.active()
                    && state.workers.is_empty()
                    && state.operation_workers.is_empty()
                    && state.ble_control_owner.is_none() => {}
            NativeAuthority::Transaction(CardTransaction::Operation { binding })
                if state.operation_workers.get(&binding.operation_token)
                    == Some(&(expected.reader.clone(), expected.generation))
                    && state.operations.lifecycle_allowed(
                        &binding.operation_token,
                        kind.name(),
                        reader,
                    )
                    && state.ble_control_owner.is_none()
                    && !state.native.owner_active() => {}
            NativeAuthority::Transaction(CardTransaction::Native {
                binding,
                purpose,
                operation_sequence,
            }) if binding.reader == expected.reader
                && purpose == "finalize"
                && *operation_sequence == Some(1)
                && state.native.finalizing(binding)
                && state.native.lifecycle_authorized(binding) => {}
            NativeAuthority::Loss(binding)
                if state.selection.physical.as_ref() == Some(binding)
                    && !state.native.active()
                    && state.operation_workers.is_empty() => {}
            NativeAuthority::BleLoss(binding)
                if kind == NativeTransitionKind::Loss
                    && binding.reader == reader
                    && state
                        .selection
                        .native_facts
                        .as_ref()
                        .and_then(|f| f.ble_connection.as_ref())
                        == Some(binding)
                    && !state.native.active()
                    && state.operation_workers.is_empty() => {}
            _ => return Err("card_busy"),
        }
        if let NativeAuthority::Transaction(tx @ CardTransaction::Operation { binding }) =
            &authority
        {
            super::super::operation::validate_io(
                &state,
                tx,
                &binding.producer_id,
                &binding.reader,
            )?;
        }
        let sequence = state
            .selection
            .native_high_water
            .checked_add(1)
            .ok_or("generation_exhausted")?;
        // Capture ownership while the physical selection gate is held. A Go
        // status snapshot (or its absence) is never evidence of no legacy owner.
        // The pending transition prevents new physical work after this capture.
        let legacy_loss = if kind == NativeTransitionKind::Loss {
            (|| {
                let admission = state
                    .ordinary_admission
                    .as_ref()
                    .ok_or("reader_business_unconfirmed")?;
                let ordinary = admission
                    .current_go_claim()
                    .map_err(|_| "reader_business_unconfirmed")?
                    .and_then(|claim| claim.go_binding())
                    .filter(|binding| binding.source != "module");
                let business = if state.business.owner.is_some()
                    || state
                        .legacy_business_guard
                        .as_ref()
                        .is_some_and(|guard| !guard.retired)
                {
                    Some(
                        state
                            .business
                            .producer()
                            .ok_or("reader_business_unconfirmed")?
                            .to_owned(),
                    )
                } else {
                    None
                };
                match (business, ordinary) {
                    (Some(producer), Some(claim)) if producer != claim.producer => {
                        Err("reader_business_unconfirmed")
                    }
                    (Some(producer), _) => Ok(Some(producer)),
                    (None, Some(claim)) => Ok(Some(claim.producer)),
                    (None, None) => Ok(None),
                }
            })()
        } else {
            Err("reader_loss_proof_required")
        };
        let intent = NativeIntent {
            producer: producer.into(),
            token,
            sequence,
            kind,
            expected: expected.clone(),
            reader: reader.into(),
            authority,
            legacy_loss,
        };
        state.selection.native_high_water = sequence;
        state.selection.native_pending = Some(intent.clone());
        Ok(NativeTransition(intent))
    }
    pub(crate) fn track_native_lifecycle_worker(
        self: &Arc<Self>,
        transition: &NativeTransition,
    ) -> Result<NativeLifecycleWorker, &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state.selection.native_pending.as_ref() != Some(&transition.0) {
            return Err("stale_reader_transition");
        }
        state.selection.native_workers += 1;
        Ok(NativeLifecycleWorker(
            self.clone(),
            transition.0.token.clone(),
        ))
    }
    /// Consume Owner's exact successfully closed presence handoff while its
    /// lock excludes ACK/reopen, then pin the committed receipt to the parent.
    /// Generic logical commits alone cannot grant native loss retirement.
    pub(crate) fn commit_confirmed_native_presence_loss(
        &self,
        owner: &Owner,
        transition: &NativeTransition,
    ) -> Result<SelectionSnapshot, &'static str> {
        let NativeAuthority::Loss(binding) = &transition.0.authority else {
            return Err("physical_loss_unconfirmed");
        };
        if transition.0.kind != NativeTransitionKind::Loss {
            return Err("physical_loss_unconfirmed");
        }
        owner
            .with_confirmed_presence_loss(binding, || {
                let snapshot =
                    self.commit_native_transition(transition, None, NativeReaderFacts::default())?;
                let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
                let receipt = state
                    .selection
                    .native_terminal
                    .iter()
                    .find(|receipt| receipt.intent == transition.0 && !receipt.aborted)
                    .cloned()
                    .ok_or("physical_loss_receipt_missing")?;
                state.native_multi.retain_confirmed_loss(receipt);
                Ok(snapshot)
            })
            .map_err(|_| "physical_loss_unconfirmed")?
    }
    pub(crate) fn commit_confirmed_ble_presence_loss(
        &self,
        proof: &crate::reader_ble_apdu::CloseProof,
        transition: &NativeTransition,
    ) -> Result<SelectionSnapshot, &'static str> {
        let NativeAuthority::BleLoss(binding) = &transition.0.authority else {
            return Err("physical_loss_unconfirmed");
        };
        if transition.0.kind != NativeTransitionKind::Loss || binding != proof.binding() {
            return Err("physical_loss_unconfirmed");
        }
        let snapshot =
            self.commit_native_transition(transition, None, NativeReaderFacts::default())?;
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        let receipt = state
            .selection
            .native_terminal
            .iter()
            .find(|r| r.intent == transition.0 && !r.aborted)
            .cloned()
            .ok_or("physical_loss_receipt_missing")?;
        state.native_multi.retain_confirmed_loss(receipt);
        Ok(snapshot)
    }
    pub(crate) fn commit_native_transition(
        &self,
        transition: &NativeTransition,
        physical: Option<&LifecycleBinding>,
        facts: NativeReaderFacts,
    ) -> Result<SelectionSnapshot, &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if let Some(receipt) = state
            .selection
            .native_terminal
            .iter()
            .find(|receipt| receipt.intent == transition.0)
        {
            return if !receipt.aborted
                && receipt.facts == facts
                && receipt.physical.as_ref() == physical
            {
                Ok(receipt.snapshot.clone())
            } else {
                Err("native_reader_receipt_conflict")
            };
        }
        if state.selection.native_pending.as_ref() != Some(&transition.0) {
            return Err("stale_reader_transition");
        }
        if state.selection.native_workers != 0 {
            return Err("reader_physical_work_pending");
        }
        let intent = &transition.0;
        if state.selection.snapshot != intent.expected {
            return Err("stale_reader_generation");
        }
        if !matches!(
            intent.kind,
            NativeTransitionKind::Disconnect | NativeTransitionKind::Loss
        ) && !intent.reader.starts_with("BLE:")
        {
            let physical = physical.ok_or("physical_binding_unconfirmed")?;
            if physical.reader != intent.reader
                || physical.producer != intent.producer
                || (intent.kind != NativeTransitionKind::Connect
                    && state.selection.physical.as_ref() != Some(physical))
            {
                return Err("stale_physical_binding");
            }
        }
        match &intent.authority {
            NativeAuthority::Transaction(CardTransaction::Operation { binding })
                if !state.operations.lifecycle_allowed(
                    &binding.operation_token,
                    intent.kind.name(),
                    &intent.reader,
                ) =>
            {
                return Err("owner_expired");
            }
            NativeAuthority::Transaction(CardTransaction::Native { binding, .. })
                if !state.native.lifecycle_authorized(binding) =>
            {
                return Err("owner_expired");
            }
            _ => {}
        }
        let controlled = matches!(&intent.authority, NativeAuthority::Transaction(CardTransaction::Operation { binding }) if matches!(&binding.authority, super::super::operation::Authority::Business {token,sequence,role} if role == "multistandby" && state.business.matches_operation(token,*sequence,role,&binding.reader,binding.reader_generation)));
        let previous_iccid = state
            .selection
            .native_facts
            .as_ref()
            .and_then(|facts| facts.metadata.as_ref())
            .and_then(|metadata| metadata["iccid"].as_str())
            .unwrap_or_default();
        let current_iccid = facts
            .metadata
            .as_ref()
            .and_then(|metadata| metadata["iccid"].as_str())
            .unwrap_or_default();
        if facts.metadata.as_ref().is_some_and(|metadata| {
            !metadata.is_object()
                || current_iccid.len() < 10
                || current_iccid.len() > 20
                || !current_iccid.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return Err("invalid_reader_metadata");
        }
        let mut snapshot = intent.expected.clone();
        let changed_identity = !previous_iccid.is_empty()
            && !current_iccid.is_empty()
            && previous_iccid != current_iccid;
        if !controlled && (intent.kind != NativeTransitionKind::Identity || changed_identity) {
            snapshot.generation = snapshot
                .generation
                .checked_add(1)
                .ok_or("generation_exhausted")?;
        }
        if intent.kind == NativeTransitionKind::Connect {
            snapshot.reader = intent.reader.clone();
            snapshot.connected_at_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "clock_unavailable")?
                .as_millis()
                .try_into()
                .map_err(|_| "clock_unavailable")?;
        } else if matches!(
            intent.kind,
            NativeTransitionKind::Disconnect | NativeTransitionKind::Loss
        ) {
            snapshot.reader.clear();
            snapshot.connected_at_ms = 0;
        }
        let disconnected = snapshot.reader.is_empty();
        state.reader_metadata = if disconnected {
            None
        } else {
            facts.metadata.as_ref().map(|metadata| {
                (
                    snapshot.reader.clone(),
                    snapshot.generation,
                    metadata.clone(),
                )
            })
        };
        if !disconnected && facts.metadata.is_some() {
            // This point is reached only for a newly committed transition.
            // The exact NativeReceipt replay returns above without advancing.
            state.selection.physical_metadata_epoch =
                state.selection.physical_metadata_epoch.saturating_add(1);
        }
        state.selection.native_facts = (!disconnected).then_some(facts.clone());
        state.selection.physical = if disconnected {
            None
        } else {
            physical.cloned()
        };
        state.selection.snapshot = snapshot.clone();
        if let NativeAuthority::Transaction(CardTransaction::Operation { binding }) =
            &intent.authority
        {
            state
                .operations
                .advance(&binding.operation_token, &snapshot);
            if let Some(scope) = state.operation_workers.get_mut(&binding.operation_token) {
                *scope = (snapshot.reader.clone(), snapshot.generation);
            }
        }
        state.selection.native_pending = None;
        state.selection.native_unconfirmed = false;
        state.selection.native_terminal.push_back(NativeReceipt {
            intent: intent.clone(),
            snapshot: snapshot.clone(),
            facts,
            physical: physical.cloned(),
            aborted: false,
        });
        while state.selection.native_terminal.len() > 64 {
            state.selection.native_terminal.pop_front();
        }
        super::super::business::finish_retired_business(&mut state);
        Ok(snapshot)
    }
    pub(crate) fn abort_native_transition(
        &self,
        transition: &NativeTransition,
    ) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "reader_gate_failed")?;
        if state
            .selection
            .native_terminal
            .iter()
            .any(|receipt| receipt.intent == transition.0 && receipt.aborted)
        {
            return Ok(());
        }
        if state.selection.native_pending.as_ref() != Some(&transition.0) {
            return Err("stale_reader_transition");
        }
        if state.selection.native_workers != 0 {
            return Err("reader_physical_work_pending");
        }
        let snapshot = state.selection.snapshot.clone();
        state.selection.native_pending = None;
        state.selection.native_unconfirmed = false;
        state.selection.native_terminal.push_back(NativeReceipt {
            intent: transition.0.clone(),
            snapshot,
            facts: NativeReaderFacts::default(),
            physical: None,
            aborted: true,
        });
        while state.selection.native_terminal.len() > 64 {
            state.selection.native_terminal.pop_front();
        }
        super::super::business::finish_retired_business(&mut state);
        Ok(())
    }
}

fn registered_identity_current(
    state: &super::super::State,
    reader: &str,
    generation: u64,
    iccid: &str,
    imsi: &str,
) -> bool {
    let normalize = |v: &str| {
        v.trim()
            .trim_matches('"')
            .trim_end_matches(['F', 'f'])
            .to_owned()
    };
    let Some(metadata) = state
        .selection
        .native_facts
        .as_ref()
        .and_then(|f| f.metadata.as_ref())
        .and_then(Value::as_object)
    else {
        return false;
    };
    state.selection.confirms(reader, generation)
        && !iccid.trim().is_empty()
        && !imsi.trim().is_empty()
        && metadata
            .get("iccid")
            .and_then(Value::as_str)
            .is_some_and(|v| normalize(v) == normalize(iccid))
        && metadata
            .get("imsi")
            .and_then(Value::as_str)
            .is_some_and(|v| v.trim() == imsi.trim())
}

fn validate_multistandby_workflow(
    state: &State,
    transaction: &CardTransaction,
    reader: &str,
    generation: u64,
) -> Result<(), &'static str> {
    let CardTransaction::Operation { binding } = transaction else {
        return Err("owner_mismatch");
    };
    if !matches!(&binding.authority, super::super::operation::Authority::Business { role, .. } if role == "multistandby")
        || binding.purpose != "card"
        || binding.reader != reader
        || binding.reader_generation != generation
    {
        return Err("owner_mismatch");
    }
    if !state.selection.native_owned()
        || state.selection.pending()
        || !state.selection.confirms(reader, generation)
    {
        return Err("stale_reader_generation");
    }
    super::super::operation::validate_io(state, transaction, &binding.producer_id, reader)
}

#[cfg(test)]
mod tests;
