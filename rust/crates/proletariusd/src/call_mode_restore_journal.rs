//! Durable evidence for one normal restore; never device or recovery authority.
//! All records are immutable, bounded, and anchored to the actual lifetime fd.
use crate::daemon_lifetime::RootProof;
use crate::module_radio_transport::{UsbComposition, VoiceConfiguration};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

mod storage;
#[cfg(test)]
mod tests;

pub(crate) const ACTIVE_NAME: &str = "call-mode-restore.active";
pub(crate) const ARCHIVE_NAME: &str = "call-mode-restore-archive";
const MAX_MUTATIONS: usize = 32;
const MAX_RECORDS: usize = 160;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const MAX_TOTAL_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Error(String);
impl Error {
    pub(crate) fn code(&self) -> &'static str {
        "call_mode_restore_journal_unavailable"
    }
    fn invalid(message: &str) -> Self {
        Self(message.into())
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self(format!("restore journal storage: {error}"))
    }
}
impl From<nix::errno::Errno> for Error {
    fn from(error: nix::errno::Errno) -> Self {
        std::io::Error::from(error).into()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    pub(crate) producer_id: String,
    pub(crate) reservation_token: String,
    pub(crate) reservation_sequence: u64,
    pub(crate) operation_id: String,
    pub(crate) module_imei: String,
    pub(crate) current_usb: UsbComposition,
    pub(crate) current_voice: VoiceConfiguration,
    pub(crate) target_usb: UsbComposition,
    pub(crate) target_voice: Option<VoiceConfiguration>,
    /// Observed data only. Older/unbound transports remain explicit None;
    /// deserializing this field never grants permission to open a device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recovery_binding: Option<crate::usb_at_transport::DurableUsbBinding>,
    /// Original-process observation captured while ADB admission is held.
    /// Missing in old records means unknown, never proof of historical cleanup.
    /// Like the USB observation, these bytes grant no recovery or STOP right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) voice_recovery_binding: Option<crate::call_mode_adb::DurableVoiceBinding>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Mutation {
    pub(crate) operation_id: String,
    pub(crate) kind: MutationKind,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MutationKind {
    UsbComposition {
        expected: UsbComposition,
        target: UsbComposition,
    },
    Ims {
        target: u8,
    },
    Volte {
        disabled: u8,
    },
    Reboot,
    AdbAuthorize,
}
/// Actual typed values observed after the write ACK. A module may expose old
/// or partially applied values until reboot; the final target remains strict.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Readback {
    UsbComposition { actual: UsbComposition },
    Ims { ims: u8, volte_capability: u8 },
    Volte { disabled: u8 },
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub(crate) restored_at: String,
    pub(crate) changed: bool,
    pub(crate) module_imei: String,
    pub(crate) usb: UsbComposition,
    pub(crate) voice: Option<VoiceConfiguration>,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Acknowledged,
    NotSubmitted,
    KnownRejected,
    SubmittedUnknown,
    VerifiedReadback,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Event {
    Intent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery_origin: Option<crate::startup_recovery::reconcile::ledger::NormalJournalOrigin>,
        intent: Intent,
        backup_id: String,
        backup_sha256: String,
    },
    SafetyCheckpoint {
        backup_id: String,
        backup_sha256: String,
    },
    Prepare {
        mutation: Mutation,
    },
    Outcome {
        prepare_sequence: u64,
        outcome: Outcome,
    },
    VerifiedReadback {
        prepare_sequence: u64,
        readback: Readback,
    },
    RebootSuccessorVerified {
        prepare_sequence: u64,
    },
    TargetVerified {
        receipt: Receipt,
    },
    Released,
    Receipt {
        receipt: Receipt,
    },
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u8,
    run_id: String,
    sequence: u64,
    previous_sha256: Option<String>,
    event: Event,
}
#[derive(Clone)]
pub(crate) struct Journal(Arc<Inner>);
struct Inner {
    root: PathBuf,
    proof: RootProof,
    state: Mutex<State>,
}
struct State {
    store: storage::Store,
    intent: Intent,
    mutations: Vec<Step>,
    safety: Option<(String, String)>,
    failed: Option<Error>,
    receipt: Option<Receipt>,
    target: Option<Receipt>,
    released: bool,
    archived: bool,
}
#[derive(Clone)]
struct Step {
    sequence: u64,
    mutation: Mutation,
    outcome: Option<Outcome>,
    readback: Option<Readback>,
}
/// An opaque identity for an already durable Prepare. It sends no command and
/// cannot be constructed from an operation id or a caller-supplied boolean.
#[derive(Clone)]
pub(crate) struct PreparedWrite {
    journal: Journal,
    sequence: u64,
    mutation: Mutation,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DiagnosticSummary {
    pub(crate) run_id: String,
    pub(crate) sequence: u64,
    pub(crate) prepared_steps: usize,
    pub(crate) unknown_steps: usize,
    pub(crate) receipt_present: bool,
    pub(crate) target_verified: bool,
    pub(crate) released: bool,
}

/// The validated history and its original descriptors, not a resumed normal
/// Journal. It owns no device, reservation or permission to modify old events.
pub(crate) struct RecoveryJournalCapture {
    root: PathBuf,
    proof: RootProof,
    input: storage::RecoveryInput,
}
impl fmt::Debug for RecoveryJournalCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryJournalCapture")
            .finish_non_exhaustive()
    }
}
impl RecoveryJournalCapture {
    pub(crate) fn verify_unchanged(&self, proof: &RootProof) -> Result<(), Error> {
        if !self.proof.same_authority(proof) {
            return Err(Error::invalid(
                "recovery journal belongs to another lifetime",
            ));
        }
        proof
            .with_root(&self.root, |held| self.input.verify(held))
            .map_err(|e| Error::invalid(&e.to_string()))??;
        Ok(())
    }
    pub(crate) fn summary(&self) -> DiagnosticSummary {
        self.input.summary()
    }
    pub(crate) fn intent(&self) -> &Intent {
        self.input.intent()
    }
    pub(crate) fn events(&self) -> impl Iterator<Item = &Event> {
        self.input.events()
    }
    pub(crate) fn backup_evidence(&self) -> (&str, &str) {
        self.input.backup_evidence()
    }
    pub(crate) fn evidence_sha256(&self) -> &str {
        self.input.evidence_sha256()
    }
}

pub(crate) fn capture_for_recovery(
    root: &Path,
    proof: &RootProof,
) -> Result<Option<RecoveryJournalCapture>, Error> {
    let input = proof
        .with_root(root, storage::RecoveryInput::capture)
        .map_err(|e| Error::invalid(&e.to_string()))??;
    Ok(input.map(|input| RecoveryJournalCapture {
        root: root.into(),
        proof: proof.clone(),
        input,
    }))
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn imei(value: &str) -> bool {
    value.len() == 15 && value.bytes().all(|b| b.is_ascii_digit())
}
fn same_target(left: &Receipt, right: &Receipt) -> bool {
    left.changed == right.changed
        && left.module_imei == right.module_imei
        && left.usb == right.usb
        && left.voice == right.voice
}
impl Intent {
    fn validate(&self) -> Result<(), Error> {
        if !token(&self.producer_id)
            || !token(&self.reservation_token)
            || !token(&self.operation_id)
            || self.reservation_sequence == 0
            || !imei(&self.module_imei)
            || !self.current_usb.valid()
            || !self.current_voice.valid()
            || !self.target_usb.valid()
            || self.target_voice.is_some_and(|v| !v.valid())
            || self
                .voice_recovery_binding
                .as_ref()
                .is_some_and(|binding| !binding.matches_producer(&self.producer_id))
        {
            return Err(Error::invalid("invalid bounded restore intent"));
        }
        Ok(())
    }
}
impl Mutation {
    fn validate(&self, intent: &Intent) -> Result<(), Error> {
        let allowed = match self.kind {
            MutationKind::UsbComposition { expected, target } => {
                expected == intent.current_usb
                    && target == intent.target_usb
                    && (expected.vendor_id, expected.product_id)
                        == (target.vendor_id, target.product_id)
            }
            MutationKind::Ims { target } => intent.target_voice.is_some_and(|v| v.ims == target),
            MutationKind::Volte { disabled } => intent
                .target_voice
                .is_some_and(|v| v.volte_disabled == disabled),
            MutationKind::Reboot => true,
            // Normal restore never needs a persistent ADB challenge response.
            MutationKind::AdbAuthorize => false,
        };
        if !token(&self.operation_id) || !allowed {
            return Err(Error::invalid("mutation is outside restore intent"));
        }
        Ok(())
    }
}
impl Readback {
    fn validate(self, mutation: &Mutation, intent: &Intent) -> bool {
        match (self, &mutation.kind) {
            (
                Self::UsbComposition { actual },
                MutationKind::UsbComposition { expected, target },
            ) => {
                actual.valid()
                    && (actual.vendor_id, actual.product_id)
                        == (expected.vendor_id, expected.product_id)
                    && (expected.vendor_id, expected.product_id)
                        == (target.vendor_id, target.product_id)
                    && actual
                        .flags
                        .iter()
                        .enumerate()
                        .all(|(i, value)| *value == expected.flags[i] || *value == target.flags[i])
            }
            (
                Self::Ims {
                    ims,
                    volte_capability,
                },
                MutationKind::Ims { target },
            ) => {
                (ims == intent.current_voice.ims || ims == *target)
                    && intent.target_voice.is_some_and(|v| {
                        volte_capability == intent.current_voice.volte_capability
                            || volte_capability == v.volte_capability
                    })
            }
            (Self::Volte { disabled }, MutationKind::Volte { disabled: target }) => {
                disabled == intent.current_voice.volte_disabled || disabled == *target
            }
            _ => false,
        }
    }
}
impl Journal {
    pub(crate) async fn begin(
        root: &Path,
        proof: &RootProof,
        captured: &crate::call_mode_backups::CapturedNativeBackup,
        intent: Intent,
    ) -> Result<Self, Error> {
        intent.validate()?;
        captured
            .verify_for_root(root, proof)
            .await
            .map_err(|_| Error::invalid("captured restore input authority changed"))?;
        if captured.backup().module.imei != intent.module_imei
            || captured.backup().usb != intent.target_usb
            || captured.backup().voice != intent.target_voice
        {
            return Err(Error::invalid("restore intent differs from captured input"));
        }
        let root = root.to_path_buf();
        let proof = proof.clone();
        let backup_id = captured.id().to_owned();
        let backup_sha256 = captured.sha256();
        let journal = tokio::task::spawn_blocking(move || {
            let store = proof
                .with_root(&root, |held| {
                    storage::Store::create(held, intent.clone(), backup_id, backup_sha256)
                })
                .map_err(|_| Error::invalid("restore journal root authority changed"))??;
            Ok::<_, Error>(Self(Arc::new(Inner {
                root,
                proof,
                state: Mutex::new(State {
                    store,
                    intent,
                    mutations: Vec::new(),
                    safety: None,
                    failed: None,
                    receipt: None,
                    target: None,
                    released: false,
                    archived: false,
                }),
            })))
        })
        .await
        .map_err(|_| Error::invalid("restore journal worker did not finish"))??;
        if captured
            .verify_for_root(&journal.0.root, &journal.0.proof)
            .await
            .is_err()
        {
            journal.poison(Error::invalid(
                "captured restore input changed during intent persistence",
            ));
            return Err(Error::invalid(
                "captured restore input changed during intent persistence",
            ));
        }
        Ok(journal)
    }
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn matches_reservation(&self, producer: &str, token: &str, sequence: u64) -> bool {
        let Ok(state) = self.0.state.lock() else {
            return false;
        };
        state.intent.producer_id == producer
            && state.intent.reservation_token == token
            && state.intent.reservation_sequence == sequence
    }
    pub(crate) fn validate_root(&self, proof: &RootProof) -> Result<(), Error> {
        if !self.0.proof.same_authority(proof) {
            let error = Error::invalid("restore journal belongs to another root authority");
            self.poison(error.clone());
            return Err(error);
        }
        self.with_state(|state, held| state.store.validate(held).map_err(Error::from))
    }
    /// Only the original successfully archived writer can satisfy the
    /// temporary new-admission fence. This never grants operation authority.
    pub(crate) fn completed_for_root(&self, proof: &RootProof) -> Result<(), Error> {
        if !self.0.proof.same_authority(proof) {
            return Err(Error::invalid("completed restore root authority differs"));
        }
        let state = self
            .0
            .state
            .lock()
            .map_err(|_| Error::invalid("completed restore owner panicked"))?;
        if let Some(error) = &state.failed {
            return Err(error.clone());
        }
        if !state.archived || state.receipt.is_none() || !state.released || state.target.is_none() {
            return Err(Error::invalid(
                "restore has not completed actual release and archival",
            ));
        }
        self.0
            .proof
            .with_root(&self.0.root, |held| state.store.validate_archived(held))
            .map_err(|_| Error::invalid("completed restore root authority changed"))??;
        Ok(())
    }
    fn poison(&self, error: Error) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .failed
            .get_or_insert(error);
    }
    fn with_state<T>(
        &self,
        work: impl FnOnce(&mut State, &std::fs::File) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut state = match self.0.state.lock() {
            Ok(state) => state,
            Err(error) => {
                let mut state = error.into_inner();
                let failure = Error::invalid("restore journal owner panicked during persistence");
                state.failed.get_or_insert_with(|| failure.clone());
                return Err(failure);
            }
        };
        if let Some(error) = &state.failed {
            return Err(error.clone());
        }
        if state.archived {
            return Err(Error::invalid("restore journal already archived"));
        }
        let result = self
            .0
            .proof
            .with_root(&self.0.root, |held| {
                state.store.validate(held)?;
                work(&mut state, held)
            })
            .map_err(|_| Error::invalid("restore journal root authority changed"))
            .and_then(|v| v);
        if let Err(error) = &result {
            state.failed.get_or_insert_with(|| error.clone());
        }
        result
    }
    pub(crate) fn safety_checkpoint(&self, backup_id: &str, sha256: &str) -> Result<(), Error> {
        self.with_state(|state, held| {
            if !crate::call_mode_backups::valid_backup_id(backup_id)
                || !digest(sha256)
                || !state.mutations.is_empty()
                || state.target.is_some()
                || state.released
            {
                return Err(Error::invalid("invalid or late restore safety checkpoint"));
            }
            let value = (backup_id.to_owned(), sha256.to_owned());
            if let Some(previous) = &state.safety {
                return if previous == &value {
                    Ok(())
                } else {
                    Err(Error::invalid("safety checkpoint changed"))
                };
            }
            state.store.append(
                held,
                Event::SafetyCheckpoint {
                    backup_id: value.0.clone(),
                    backup_sha256: value.1.clone(),
                },
            )?;
            state.safety = Some(value);
            Ok(())
        })
    }
    pub(crate) fn prepare_step(&self, mutation: Mutation) -> Result<PreparedWrite, Error> {
        self.with_state(|state, held| {
            mutation.validate(&state.intent)?;
            if state.receipt.is_some() || state.target.is_some() || state.released {
                return Err(Error::invalid("restore receipt already prepared"));
            }
            if let Some(step) = state
                .mutations
                .iter()
                .find(|step| step.mutation.operation_id == mutation.operation_id)
            {
                if step.mutation != mutation || step.outcome.is_some() {
                    return Err(Error::invalid("restore mutation cannot be replayed"));
                }
                return Ok(PreparedWrite {
                    journal: self.clone(),
                    sequence: step.sequence,
                    mutation,
                });
            }
            if state.safety.is_none()
                || state.mutations.len() >= MAX_MUTATIONS
                || state.mutations.iter().any(|s| {
                    !matches!(
                        s.outcome,
                        Some(
                            Outcome::VerifiedReadback
                                | Outcome::NotSubmitted
                                | Outcome::KnownRejected
                        )
                    )
                })
            {
                return Err(Error::invalid(
                    "prior restore mutation is unresolved or safety checkpoint is missing",
                ));
            }
            let sequence = state.store.append(
                held,
                Event::Prepare {
                    mutation: mutation.clone(),
                },
            )?;
            state.mutations.push(Step {
                sequence,
                mutation: mutation.clone(),
                outcome: None,
                readback: None,
            });
            Ok(PreparedWrite {
                journal: self.clone(),
                sequence,
                mutation,
            })
        })
    }
    /// Called only by the lifecycle after fresh target verification and actual
    /// original release. This persists evidence, never verifies physical I/O.
    pub(crate) fn finish_after_release(&self, receipt: Receipt) -> Result<(), Error> {
        self.with_state(|state, held| {
            if !imei(&receipt.module_imei)
                || receipt.module_imei != state.intent.module_imei
                || receipt.usb != state.intent.target_usb
                || receipt.voice != state.intent.target_voice
                || receipt.restored_at.is_empty()
                || receipt.restored_at.len() > 64
                || !state
                    .target
                    .as_ref()
                    .is_some_and(|target| same_target(target, &receipt))
                || !state.released
                || state
                    .mutations
                    .iter()
                    .any(|s| s.outcome != Some(Outcome::VerifiedReadback))
            {
                return Err(Error::invalid(
                    "restore receipt has unresolved mutations or differs from target",
                ));
            }
            if let Some(previous) = &state.receipt {
                if previous != &receipt {
                    return Err(Error::invalid("restore receipt changed"));
                }
            } else {
                state.store.append(
                    held,
                    Event::Receipt {
                        receipt: receipt.clone(),
                    },
                )?;
                state.receipt = Some(receipt);
            }
            state.store.archive(held)?;
            state.archived = true;
            Ok(())
        })
    }
    pub(crate) fn target_verified(&self, receipt: Receipt) -> Result<(), Error> {
        self.with_state(|state, held| {
            if receipt.module_imei != state.intent.module_imei
                || receipt.usb != state.intent.target_usb
                || receipt.voice != state.intent.target_voice
                || receipt.restored_at.len() > 64
                || state
                    .mutations
                    .iter()
                    .any(|s| s.outcome != Some(Outcome::VerifiedReadback))
                || state.released
            {
                return Err(Error::invalid(
                    "restore target checkpoint is unresolved or differs from intent",
                ));
            }
            if let Some(previous) = &state.target {
                return if previous == &receipt {
                    Ok(())
                } else {
                    Err(Error::invalid("verified restore target changed"))
                };
            }
            state.store.append(
                held,
                Event::TargetVerified {
                    receipt: receipt.clone(),
                },
            )?;
            state.target = Some(receipt);
            Ok(())
        })
    }
    pub(crate) fn released(&self) -> Result<(), Error> {
        self.with_state(|state, held| {
            if state.released {
                return Ok(());
            }
            state.store.append(held, Event::Released)?;
            state.released = true;
            Ok(())
        })
    }
}
impl PreparedWrite {
    pub(crate) fn validate_for_write(&self, mutation: &Mutation) -> Result<(), Error> {
        self.journal.with_state(|state, _| {
            if state.target.is_some() || state.released {
                return Err(Error::invalid(
                    "prepared mutation is after target/release checkpoint",
                ));
            }
            let step = state
                .mutations
                .iter()
                .find(|s| s.sequence == self.sequence)
                .ok_or_else(|| Error::invalid("unknown prepared mutation"))?;
            if mutation != &self.mutation || &step.mutation != mutation || step.outcome.is_some() {
                return Err(Error::invalid("prepared mutation no longer writable"));
            }
            Ok(())
        })
    }
    fn record(&self, outcome: Outcome) -> Result<(), Error> {
        self.journal.with_state(|state, held| {
            let index = state
                .mutations
                .iter()
                .position(|s| s.sequence == self.sequence && s.mutation == self.mutation)
                .ok_or_else(|| Error::invalid("unknown prepared mutation"))?;
            let previous = state.mutations[index].outcome;
            if previous == Some(outcome) {
                return Ok(());
            }
            if state.target.is_some() || state.released {
                return Err(Error::invalid(
                    "mutation result after target/release checkpoint",
                ));
            }
            if previous.is_some()
                && !(previous == Some(Outcome::Acknowledged)
                    && matches!(outcome, Outcome::SubmittedUnknown))
            {
                return Err(Error::invalid(
                    "restore mutation outcome cannot be overwritten",
                ));
            }
            #[cfg(test)]
            state.store.before_outcome()?;
            state.store.append(
                held,
                Event::Outcome {
                    prepare_sequence: self.sequence,
                    outcome,
                },
            )?;
            state.mutations[index].outcome = Some(outcome);
            Ok(())
        })
    }
    pub(crate) fn record_acknowledged(&self) -> Result<(), Error> {
        self.record(Outcome::Acknowledged)
    }
    pub(crate) fn record_not_submitted(&self) -> Result<(), Error> {
        self.record(Outcome::NotSubmitted)
    }
    pub(crate) fn record_known_rejected(&self) -> Result<(), Error> {
        self.record(Outcome::KnownRejected)
    }
    pub(crate) fn record_submitted_unknown(&self) -> Result<(), Error> {
        self.record(Outcome::SubmittedUnknown)
    }
    pub(crate) fn record_verified_readback(&self, readback: Readback) -> Result<(), Error> {
        self.journal.with_state(|state, held| {
            let index = state
                .mutations
                .iter()
                .position(|s| s.sequence == self.sequence && s.mutation == self.mutation)
                .ok_or_else(|| Error::invalid("unknown prepared typed readback"))?;
            let step = &state.mutations[index];
            if step.outcome == Some(Outcome::VerifiedReadback) && step.readback == Some(readback) {
                return Ok(());
            }
            if step.outcome != Some(Outcome::Acknowledged)
                || state.target.is_some()
                || state.released
                || !readback.validate(&self.mutation, &state.intent)
            {
                return Err(Error::invalid(
                    "typed readback lacks ACK or is outside audited original/target values",
                ));
            }
            #[cfg(test)]
            state.store.before_outcome()?;
            state.store.append(
                held,
                Event::VerifiedReadback {
                    prepare_sequence: self.sequence,
                    readback,
                },
            )?;
            state.mutations[index].outcome = Some(Outcome::VerifiedReadback);
            state.mutations[index].readback = Some(readback);
            Ok(())
        })
    }
    /// Only the original radio owner may call this after its sealed actual
    /// successor receipt. The original Unknown record remains in the chain.
    pub(crate) fn record_reboot_verified_after_successor(&self) -> Result<(), Error> {
        self.journal.with_state(|state, held| {
            if state.target.is_some() || state.released {
                return Err(Error::invalid(
                    "reboot refinement after target/release checkpoint",
                ));
            }
            let step = state
                .mutations
                .iter()
                .find(|s| s.sequence == self.sequence && s.mutation == self.mutation)
                .ok_or_else(|| Error::invalid("unknown prepared reboot"))?;
            if step.mutation.kind == MutationKind::Reboot
                && step.outcome == Some(Outcome::VerifiedReadback)
                && step.readback.is_none()
            {
                return Ok(());
            }
            if step.mutation.kind != MutationKind::Reboot
                || !matches!(
                    step.outcome,
                    Some(Outcome::Acknowledged | Outcome::SubmittedUnknown)
                )
            {
                return Err(Error::invalid(
                    "only the exact pending reboot may refine its successor result",
                ));
            }
            #[cfg(test)]
            state.store.before_outcome()?;
            state.store.append(
                held,
                Event::RebootSuccessorVerified {
                    prepare_sequence: self.sequence,
                },
            )?;
            state
                .mutations
                .iter_mut()
                .find(|s| s.sequence == self.sequence)
                .unwrap()
                .outcome = Some(Outcome::VerifiedReadback);
            Ok(())
        })
    }
}

/// Read-only startup diagnostics. Any active presence still requires isolation,
/// including a complete receipt not yet durably archived. No replay is exposed.
pub(crate) fn diagnostic_summary(
    root: &std::fs::File,
) -> std::io::Result<Option<DiagnosticSummary>> {
    storage::diagnostic_summary(root)
}
pub(crate) fn diagnostic_archived_summary(
    root: &std::fs::File,
    run_id: &str,
) -> std::io::Result<Option<DiagnosticSummary>> {
    storage::diagnostic_archived_summary(root, run_id)
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TestPhase {
    BeforeIntent,
    Intent,
    Prepared,
    BeforeOutcomePersist,
    TargetVerified,
    Released,
    Receipt,
    Archived,
}
#[cfg(test)]
pub(crate) use storage::set_test_hook;

/// A bounded typed view for independent recovery receipts and known normal
/// origins. This reader cannot reopen a writable Journal or mint a permit.
pub(crate) struct ReadonlyJournalEvidence {
    pub(crate) intent: Intent,
    pub(crate) backup_id: String,
    pub(crate) backup_sha256: String,
    pub(crate) chain_sha256: String,
    pub(crate) summary: DiagnosticSummary,
    pub(crate) recovery_origin:
        Option<crate::startup_recovery::reconcile::ledger::NormalJournalOrigin>,
}
pub(crate) fn inspect_readonly_directory(
    directory: &std::fs::File,
) -> std::io::Result<ReadonlyJournalEvidence> {
    storage::inspect_readonly_directory(directory)
}
