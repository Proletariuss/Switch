//! Startup reconciliation owns a live device, not a replay of historical
//! writes. Every new write receives a separate durable ledger permit.
use super::*;
use crate::call_mode_adb::startup_recovery::{self as voice_cleanup, VoiceSettledProof};
use crate::call_mode_restore_journal::MutationKind;
use crate::startup_recovery::reconcile::{
    PreparedReconciliationStep, ReconciliationMutation, ReconciliationReadback,
    ReconciliationScope, ReconciliationSeal, SubmissionOutcome,
};

impl Authority for ReconciliationScope<'_> {
    fn check(&self) -> Result<(), TransportError> {
        ReconciliationScope::check(self)
    }
    fn deadline(&self) -> Instant {
        ReconciliationScope::deadline(self)
    }
    fn admission(&self) -> crate::module_radio::CallAdmission {
        ReconciliationScope::admission(self)
    }
    fn expected_imei(&self) -> &str {
        ReconciliationScope::expected_imei(self)
    }
}

/// Only constructed after actual successor verification and all checked closes.
/// It grants no transport, physical target, ordinary commit proof, or receipt.
pub(crate) struct NativeCompletion {
    identity: CallModeModuleIdentity,
    usb: UsbComposition,
    voice: VoiceConfiguration,
    close: CloseDisposition,
    seal: ReconciliationSeal,
    voice_settled: VoiceSettledProof,
}
impl fmt::Debug for NativeCompletion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeCompletion")
            .field("close", &self.close)
            .finish_non_exhaustive()
    }
}
impl NativeCompletion {
    pub(crate) fn identity(&self) -> &CallModeModuleIdentity {
        &self.identity
    }
    pub(crate) fn usb(&self) -> UsbComposition {
        self.usb
    }
    pub(crate) fn voice(&self) -> VoiceConfiguration {
        self.voice
    }
    pub(crate) fn close_disposition(&self) -> CloseDisposition {
        self.close
    }
    pub(crate) fn seal(&self) -> &ReconciliationSeal {
        &self.seal
    }
    pub(crate) fn voice_settled(&self) -> &VoiceSettledProof {
        &self.voice_settled
    }
}

fn pending() -> TransportError {
    failed(
        "startup_reconciliation_unconfirmed",
        "Startup reconciliation remains pending",
    )
}
fn same_voice(actual: VoiceConfiguration, target: VoiceConfiguration) -> bool {
    actual.ims == target.ims && actual.volte_disabled == target.volte_disabled
}
fn terminal_outcome(reply: &Reply) -> SubmissionOutcome {
    if reply.text.lines().map(str::trim).any(|line| {
        let line = line.to_ascii_uppercase();
        line == "ERROR" || line.starts_with("+CME ERROR:") || line.starts_with("+CMS ERROR:")
    }) && !reply.uncertain
    {
        SubmissionOutcome::KnownRejected
    } else if reply.ok && !reply.uncertain && !reply.fatal {
        SubmissionOutcome::Acknowledged
    } else {
        SubmissionOutcome::SubmittedUnknown
    }
}
fn checked_retire(
    port: &mut Option<Box<dyn Transport>>,
) -> Result<Option<CloseDisposition>, TransportError> {
    let Some(port) = port.take() else {
        return Ok(None);
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| port.checked_close()))
        .unwrap_or_else(|_| Err(close_unconfirmed()))
        .map(Some)
}

fn mutation_for(plan: &Plan) -> Result<ReconciliationMutation, TransportError> {
    match plan.restore_kind().ok_or_else(pending)? {
        MutationKind::UsbComposition { expected, target } => {
            Ok(ReconciliationMutation::Usb { expected, target })
        }
        MutationKind::Volte { disabled } => Ok(ReconciliationMutation::Volte {
            expected: plan
                .current_voice_observation()
                .ok_or_else(pending)?
                .volte_disabled,
            target: disabled,
        }),
        MutationKind::Ims { target } => Ok(ReconciliationMutation::Ims {
            expected: plan.current_voice_observation().ok_or_else(pending)?.ims,
            target,
        }),
        MutationKind::Reboot => Ok(ReconciliationMutation::Reboot),
        MutationKind::AdbAuthorize => Err(pending()),
    }
}

/// Drive the existing normal protocol Plan, but never mint or consume its
/// ordinary CallModeCommitProof/PreparedWrite. The concrete startup ledger
/// permit alone is checked at the actual native bulk boundary.
fn compensate(
    port: &mut dyn Transport,
    scope: &ReconciliationScope<'_>,
    action: Action,
) -> Result<Outcome, TransportError> {
    if !matches!(
        action,
        Action::CallModeUsbCompositionWrite { .. } | Action::CallModeVoice { .. }
    ) {
        return Err(pending());
    }
    action.validate().map_err(|_| pending())?;
    let mut plan = Plan::new_with_cancelled(
        action.clone(),
        remaining(scope)?,
        Arc::new(AtomicBool::new(false)),
    );
    let mut usb_step: Option<PreparedReconciliationStep> = None;
    while !plan.done() {
        remaining(scope)?;
        let Some(step) = plan.next() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        if !step.write {
            let response = port.command_with_guard(
                &step.command,
                None,
                step.timeout.min(remaining(scope)?),
                crate::module_radio_transport::RESPONSE_LIMIT,
                scope.admission(),
            );
            scope.check()?;
            plan.accept(Reply::response(response.map_err(|_| pending())?));
            continue;
        }
        if step.sensitive {
            return Err(pending());
        }
        let mutation = mutation_for(&plan)?;
        let prepared = scope.prepare(mutation.clone())?;
        prepared.check()?;
        let guard = prepared.admission();
        let timeout = step.timeout.min(remaining(scope)?);
        // An unwind after durable Prepare cannot remove that Prepare. Its
        // absent Outcome remains debt in the independent recovery ledger.
        let response = port.command_with_guard(
            &step.command,
            None,
            timeout,
            crate::module_radio_transport::RESPONSE_LIMIT,
            guard,
        );
        let reply = match response {
            Ok(text) => Reply::response(text),
            Err(error) => {
                scope.record_outcome(
                    &prepared,
                    if error.uncertain {
                        SubmissionOutcome::SubmittedUnknown
                    } else {
                        SubmissionOutcome::NotSubmitted
                    },
                )?;
                return Err(pending());
            }
        };
        let outcome = terminal_outcome(&reply);
        scope.record_outcome(&prepared, outcome)?;
        if outcome != SubmissionOutcome::Acknowledged {
            return Err(pending());
        }
        scope.check()?;
        match mutation {
            ReconciliationMutation::Usb { .. } => usb_step = Some(prepared),
            ReconciliationMutation::Volte { .. } | ReconciliationMutation::Ims { .. } => {
                // Normal Plan without its own journal skips the intermediate
                // VoLTE check. Perform a real read Plan and new durable receipt
                // before it can issue the next IMS write.
                let current = plan.current_voice_observation().ok_or_else(pending)?;
                let Action::CallModeVoice { target } = action else {
                    return Err(pending());
                };
                let actual = run_plan(port, scope, Action::CallModeVoiceRead)?
                    .voice
                    .ok_or_else(pending)?;
                if crate::module_radio_transport::voice_readback_error(current, target, actual)
                    .is_some()
                {
                    return Err(pending());
                }
                scope.record_readback(&prepared, ReconciliationReadback::Voice { actual })?;
            }
            _ => return Err(pending()),
        }
        // Feed only the actual physical ACK; the extra read never invents one.
        plan.accept(reply);
    }
    if plan.retire_transport() {
        return Err(pending());
    }
    let outcome = plan.finish();
    if !outcome.complete || !outcome.verified || outcome.uncertain {
        return Err(pending());
    }
    if let Some(prepared) = usb_step {
        scope.record_readback(
            &prepared,
            ReconciliationReadback::Usb {
                actual: outcome.usb_composition.ok_or_else(pending)?,
            },
        )?;
    }
    scope.check()?;
    Ok(outcome)
}

/// Exactly one new reboot submission. Unknown configuration writes never enter
/// this function; only this reboot's own uncertain ACK can be settled by the
/// subsequent observed retirement and strict owned successor identity.
fn submit_reboot(
    port: &mut dyn Transport,
    scope: &ReconciliationScope<'_>,
) -> Result<PreparedReconciliationStep, TransportError> {
    let mut plan = Plan::new_with_cancelled(
        Action::Reboot,
        remaining(scope)?,
        Arc::new(AtomicBool::new(false)),
    );
    while !plan.done() {
        let Some(step) = plan.next() else {
            remaining(scope)?;
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        if !step.write {
            let response = port.command_with_guard(
                &step.command,
                None,
                step.timeout.min(remaining(scope)?),
                crate::module_radio_transport::RESPONSE_LIMIT,
                scope.admission(),
            );
            scope.check()?;
            plan.accept(Reply::response(response.map_err(|_| pending())?));
            continue;
        }
        if mutation_for(&plan)? != ReconciliationMutation::Reboot || step.command != "AT+CFUN=1,1" {
            return Err(pending());
        }
        let prepared = scope.prepare(ReconciliationMutation::Reboot)?;
        prepared.check()?;
        let response = port.command_with_guard(
            &step.command,
            None,
            step.timeout.min(remaining(scope)?),
            crate::module_radio_transport::RESPONSE_LIMIT,
            prepared.admission(),
        );
        let outcome = match response {
            Ok(text) => terminal_outcome(&Reply::response(text)),
            Err(error) if !error.uncertain => SubmissionOutcome::NotSubmitted,
            Err(_) => SubmissionOutcome::SubmittedUnknown,
        };
        scope.record_outcome(&prepared, outcome)?;
        if matches!(
            outcome,
            SubmissionOutcome::Acknowledged | SubmissionOutcome::SubmittedUnknown
        ) {
            scope.check()?;
            return Ok(prepared);
        }
        return Err(pending());
    }
    Err(pending())
}

type InterfaceObservation = (u8, u8, u8, u8, u8, Vec<(u8, bool)>);
fn candidate_set(
    model: (u16, u16),
    expected: Candidate,
    descriptors: Vec<InterfaceObservation>,
) -> Result<Vec<Candidate>, TransportError> {
    let mut tuples = std::collections::BTreeSet::new();
    let mut candidates = Vec::new();
    for (interface, alternate, class, subclass, protocol, endpoints) in descriptors {
        if !super::super::supports_usb_at_interface(model, interface) {
            continue;
        }
        if !tuples.insert((interface, alternate)) {
            return Err(unconfirmed());
        }
        if (class, subclass, protocol) != (0xff, 0, 0) {
            continue;
        }
        let bulk: Vec<_> = endpoints
            .into_iter()
            .filter(|(_, bulk)| *bulk)
            .map(|(address, _)| address)
            .collect();
        if bulk.len() != 2
            || bulk
                .iter()
                .any(|endpoint| endpoint & 0x0f == 0 || endpoint & 0x70 != 0)
        {
            continue;
        }
        let incoming: Vec<_> = bulk
            .iter()
            .copied()
            .filter(|endpoint| endpoint & 0x80 != 0)
            .collect();
        let outgoing: Vec<_> = bulk
            .iter()
            .copied()
            .filter(|endpoint| endpoint & 0x80 == 0)
            .collect();
        if incoming.len() != 1 || outgoing.len() != 1 {
            continue;
        }
        if !super::super::usb_at_endpoint_pair_matches(model, interface, incoming[0], outgoing[0]) {
            continue;
        }
        candidates.push(Candidate {
            interface,
            alternate,
            endpoint_in: incoming[0],
            endpoint_out: outgoing[0],
        });
    }
    let has_rndis_at_pair = model == DEVICE_IDS[0]
        && candidates
            .iter()
            .any(|candidate| matches!(candidate.interface, 4 | 5));
    candidates.sort_by_key(|candidate| {
        let preference = if has_rndis_at_pair {
            if matches!(candidate.interface, 4 | 5) {
                0
            } else {
                1
            }
        } else if *candidate == expected {
            0
        } else {
            1
        };
        (preference, candidate.interface, candidate.alternate)
    });
    if candidates.is_empty() || candidates.len() > 256 {
        return Err(unconfirmed());
    }
    Ok(candidates)
}
fn retired_snapshot<D>(
    snapshot: Vec<Observation<D>>,
    binding: &BindingView<'_>,
    retired: &mut bool,
    is_original: impl Fn(&D) -> bool,
) -> Result<Option<D>, TransportError> {
    if snapshot.len() > MAX_SCAN_DEVICES {
        return Err(unconfirmed());
    }
    if snapshot.iter().any(|item| is_original(&item.device)) {
        if *retired {
            return Err(unconfirmed());
        }
        return Ok(None);
    }
    *retired = true;
    match select_exact(snapshot, binding) {
        Ok(selected) => Ok(Some(selected)),
        Err(error) if error.code == "startup_native_target_absent" => Ok(None),
        Err(error) => Err(error),
    }
}

// This private seam carries inert enumeration tokens in software fixtures; no
// caller-defined authority or fabricated PhysicalTarget crosses the API.
trait SessionBackend {
    type Device;
    fn initial(&mut self, binding: &BindingView<'_>) -> Result<Self::Device, TransportError>;
    fn open(
        &mut self,
        device: &Self::Device,
        binding: &BindingView<'_>,
    ) -> Result<Box<dyn Transport>, TransportError>;
    fn settle_voice(
        &mut self,
        scope: &ReconciliationScope<'_>,
        port: &mut dyn Transport,
    ) -> Result<VoiceSettledProof, TransportError>;
    fn verify_successor_voice(
        &mut self,
        scope: &ReconciliationScope<'_>,
        port: &mut dyn Transport,
        prior: VoiceSettledProof,
    ) -> Result<VoiceSettledProof, TransportError>;
    fn successor(
        &mut self,
        original: &Self::Device,
        binding: &BindingView<'_>,
        scope: &ReconciliationScope<'_>,
    ) -> Result<Self::Device, TransportError>;
    fn candidates(
        &mut self,
        device: &Self::Device,
        binding: &BindingView<'_>,
    ) -> Result<Vec<Candidate>, TransportError>;
}
fn with_candidate<'a>(binding: &BindingView<'a>, candidate: Candidate) -> BindingView<'a> {
    BindingView {
        model: binding.model,
        bus: binding.bus,
        ports: binding.ports,
        language: binding.language,
        serial: binding.serial,
        candidate,
    }
}
fn execute_session(
    scope: &ReconciliationScope<'_>,
    backend: &mut impl SessionBackend,
) -> Result<NativeCompletion, TransportError> {
    #[cfg(test)]
    RECONCILIATION_NATIVE_ENTRY_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    scope.check()?;
    let binding = BindingView::new(scope.binding());
    let original = backend.initial(&binding)?;
    let mut opened = Some(backend.open(&original, &binding)?);
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let port = opened.as_mut().ok_or_else(pending)?;
        let (_, first_usb, first_voice) = read_opened(port.as_mut(), scope)?;
        if first_usb.vendor_id != scope.target_usb().vendor_id
            || first_usb.product_id != scope.target_usb().product_id
        {
            return Err(pending());
        }
        scope.observe_current(first_usb, first_voice)?;
        let voice_settled = backend.settle_voice(scope, port.as_mut())?;
        voice_settled.validate_for(scope)?;
        scope.record_voice_settled(&voice_settled)?;
        let (_, usb, voice) = read_opened(port.as_mut(), scope)?;
        if usb != first_usb || voice != first_voice {
            return Err(pending());
        }
        // No observed-state reset after a cleanup action can change the
        // frozen new plan or excuse a precondition mismatch.
        if usb != scope.target_usb() {
            compensate(
                port.as_mut(),
                scope,
                Action::CallModeUsbCompositionWrite {
                    expected: usb,
                    target: scope.target_usb(),
                },
            )?;
        }
        if let Some(target) = scope.target_voice() {
            if !same_voice(voice, target) {
                compensate(port.as_mut(), scope, Action::CallModeVoice { target })?;
            }
        }
        // Even already-target QCFG cannot settle historical unknown USB or
        // Reboot. Always establish fresh activation with one owned reboot.
        let reboot = submit_reboot(port.as_mut(), scope)?;
        checked_retire(&mut opened)?;
        let successor = backend.successor(&original, &binding, scope)?;
        let candidates = backend.candidates(&successor, &binding)?;
        let mut verified = None;
        for candidate in candidates {
            match backend.open(&successor, &with_candidate(&binding, candidate)) {
                Ok(port) => opened = Some(port),
                Err(error)
                    if error.code == "usb_at_close_unconfirmed"
                        || error.code == "startup_native_callback_panicked" =>
                {
                    return Err(error);
                }
                Err(_) => {
                    scope.check()?;
                    continue;
                }
            }
            let read = read_opened(opened.as_mut().ok_or_else(pending)?.as_mut(), scope);
            match read {
                Ok(observation) => {
                    verified = Some(observation);
                    break;
                }
                Err(_) => {
                    if checked_retire(&mut opened)?
                        != Some(CloseDisposition::ReleasedAndHandleDropped)
                    {
                        return Err(pending());
                    }
                    scope.check()?;
                }
            }
        }
        let (_, usb, voice) = verified.ok_or_else(pending)?;
        if usb != scope.target_usb()
            || scope
                .target_voice()
                .is_some_and(|target| !same_voice(voice, target))
        {
            return Err(pending());
        }
        let voice_settled = backend.verify_successor_voice(
            scope,
            opened.as_mut().ok_or_else(pending)?.as_mut(),
            voice_settled,
        )?;
        scope.record_successor_voice(&voice_settled)?;
        // The ADB voice observation can take time. Recheck the actual retained
        // AT handle after that observer has checked-close, immediately before
        // committing the typed successor readback and final AT retirement.
        let (identity, usb, voice) =
            read_opened(opened.as_mut().ok_or_else(pending)?.as_mut(), scope)?;
        if usb != scope.target_usb()
            || scope
                .target_voice()
                .is_some_and(|target| !same_voice(voice, target))
        {
            return Err(pending());
        }
        scope.record_readback(
            &reboot,
            ReconciliationReadback::RebootSuccessor { usb, voice },
        )?;
        voice_settled.validate_for(scope)?;
        scope.check()?;
        Ok((identity, usb, voice, voice_settled))
    }))
    .unwrap_or_else(|_| Err(callback_panicked()));
    // No callback error or unwind can drop a claimed handle merely through
    // a destructor. A checked-close failure outranks the attempted result.
    let close = checked_retire(&mut opened)?;
    let (identity, usb, voice, voice_settled) = attempt?;
    if close != Some(CloseDisposition::ReleasedAndHandleDropped) {
        return Err(pending());
    }
    scope.check()?;
    voice_settled.validate_for(scope)?;
    Ok(NativeCompletion {
        identity,
        usb,
        voice,
        close: CloseDisposition::ReleasedAndHandleDropped,
        seal: scope.seal(),
        voice_settled,
    })
}

#[cfg(target_os = "macos")]
mod live {
    use super::*;
    use rusb::{Context, Device, TransferType};

    fn successor_candidates(
        backend: &native::NativeBackend,
        device: &Device<Context>,
        binding: &BindingView<'_>,
    ) -> Result<Vec<Candidate>, TransportError> {
        backend.model_topology(device, binding)?;
        let config = device
            .active_config_descriptor()
            .map_err(|_| unconfirmed())?;
        let mut descriptors = Vec::new();
        for interface in config.interfaces() {
            for alternate in interface.descriptors() {
                descriptors.push((
                    alternate.interface_number(),
                    alternate.setting_number(),
                    alternate.class_code(),
                    alternate.sub_class_code(),
                    alternate.protocol_code(),
                    alternate
                        .endpoint_descriptors()
                        .map(|endpoint| {
                            (
                                endpoint.address(),
                                endpoint.transfer_type() == TransferType::Bulk,
                            )
                        })
                        .collect(),
                ));
            }
        }
        backend.fence()?;
        candidate_set(binding.model, binding.candidate, descriptors)
    }

    fn await_successor(
        backend: &mut native::NativeBackend,
        original: &Device<Context>,
        binding: &BindingView<'_>,
        scope: &ReconciliationScope<'_>,
    ) -> Result<Device<Context>, TransportError> {
        let mut retired = false;
        loop {
            remaining(scope)?;
            let snapshot = backend.snapshot()?;
            // A complete snapshot alone establishes the original retirement.
            // Retain exactly one independently verified successor Device.
            if let Some(selected) = retired_snapshot(snapshot, binding, &mut retired, |device| {
                device.as_raw() == original.as_raw()
            })? {
                return Ok(selected);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    struct Session {
        backend: native::NativeBackend,
        owner: Option<OwnerLease>,
    }
    impl SessionBackend for Session {
        type Device = Device<Context>;
        fn initial(&mut self, binding: &BindingView<'_>) -> Result<Self::Device, TransportError> {
            let selected = select_exact(self.backend.snapshot()?, binding)?;
            // One strong lease spans both handle lifetimes and their gap.
            self.owner = Some(OwnerLease::acquire()?);
            Ok(selected)
        }
        fn open(
            &mut self,
            device: &Self::Device,
            binding: &BindingView<'_>,
        ) -> Result<Box<dyn Transport>, TransportError> {
            self.backend.open_retained(
                device,
                binding,
                self.owner.as_ref().ok_or_else(pending)?.clone(),
            )
        }
        fn settle_voice(
            &mut self,
            scope: &ReconciliationScope<'_>,
            port: &mut dyn Transport,
        ) -> Result<VoiceSettledProof, TransportError> {
            voice_cleanup::settle_original_voice(scope, &port.physical_target()?)
        }
        fn verify_successor_voice(
            &mut self,
            scope: &ReconciliationScope<'_>,
            port: &mut dyn Transport,
            prior: VoiceSettledProof,
        ) -> Result<VoiceSettledProof, TransportError> {
            voice_cleanup::verify_successor_voice(scope, &port.physical_target()?, prior)
        }
        fn successor(
            &mut self,
            original: &Self::Device,
            binding: &BindingView<'_>,
            scope: &ReconciliationScope<'_>,
        ) -> Result<Self::Device, TransportError> {
            await_successor(&mut self.backend, original, binding, scope)
        }
        fn candidates(
            &mut self,
            device: &Self::Device,
            binding: &BindingView<'_>,
        ) -> Result<Vec<Candidate>, TransportError> {
            successor_candidates(&self.backend, device, binding)
        }
    }
    pub(super) fn execute(
        scope: &ReconciliationScope<'_>,
    ) -> Result<NativeCompletion, TransportError> {
        let mut session = Session {
            backend: native::NativeBackend::new_authority(scope)?,
            owner: None,
        };
        let result = execute_session(scope, &mut session);
        // Every real handle was explicitly retired inside execute_session.
        // Now the last host lease retires before any completion can escape.
        drop(session);
        result
    }
}

// Counts entry boundaries only, never factories, hardware access or success.
// Production entry plus shared executor intentionally count twice when both run.
#[cfg(test)]
static RECONCILIATION_NATIVE_ENTRY_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(crate) fn reconciliation_native_entry_count() -> usize {
    RECONCILIATION_NATIVE_ENTRY_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn reconcile(
    scope: &ReconciliationScope<'_>,
) -> Result<NativeCompletion, TransportError> {
    #[cfg(test)]
    RECONCILIATION_NATIVE_ENTRY_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    scope.check()?;
    #[cfg(target_os = "macos")]
    {
        live::execute(scope)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(unconfirmed())
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{
    ReconciliationArrivalProbe, ReconciliationFixtureScenario, scripted_reconcile_observed,
    scripted_reconcile_owned_observed,
};
