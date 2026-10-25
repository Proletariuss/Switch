//! An append-only compensation ledger. A published receipt covers only exact
//! retired inputs; it never rewrites an original outcome or grants native I/O.
#[cfg(test)]
use super::crash_tests::{PublicationPoint, observe_publication};
use super::{
    POLICY, ReconciliationMutation as Mutation, ReconciliationReadback as Readback,
    SUCCESSOR_POLICY, SubmissionOutcome as Outcome, evidence, storage,
};
use crate::daemon_lifetime::RootProof;
use crate::module_radio_transport::{UsbComposition, VoiceConfiguration};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub(super) const DOMAIN: &str = "startup-reconciliation-ledger-v1";
#[derive(Debug)]
pub(crate) struct Error;
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("startup reconciliation ledger is unavailable")
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(_: io::Error) -> Self {
        Self
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope {
    pub schema: u32,
    pub domain: String,
    pub attempt_id: String,
    pub sequence: u64,
    pub previous_sha256: Option<String>,
    pub event: Event,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Event {
    Requested {
        supersedes: Vec<Predecessor>,
        policy: String,
        successor_policy: String,
        original_sha256: String,
        journal_sha256: String,
        backup_id: String,
        backup_sha256: String,
        target_usb: UsbComposition,
        target_voice: Option<VoiceConfiguration>,
        original_voice: Option<crate::call_mode_adb::DurableVoiceBinding>,
    },
    Observed {
        usb: UsbComposition,
        voice: VoiceConfiguration,
    },
    Prepare {
        ordinal: u64,
        mutation: Mutation,
    },
    Outcome {
        ordinal: u64,
        outcome: Outcome,
    },
    Readback {
        ordinal: u64,
        readback: Readback,
    },
    VoiceSettled {
        evidence: Value,
    },
    SuccessorVoiceSettled {
        evidence: Value,
    },
    TargetVerified {
        usb: UsbComposition,
        voice: VoiceConfiguration,
    },
    NativeClosed,
    HostSettled,
    PhysicalSettled,
    RetirementPrepare {
        slot: String,
        archive: String,
    },
    PlacementCommitted {
        slot: String,
        archive: String,
    },
    PublicationCommitted {
        manifest_sha256: String,
    },
    CompletionReceipt {
        manifest_sha256: String,
        physical_settlement_sha256: String,
    },
}
#[derive(Clone)]
struct Step {
    ordinal: u64,
    mutation: Mutation,
    outcome: Option<Outcome>,
    readback: Option<Readback>,
}
pub(super) struct Writer {
    pub(super) store: storage::Store,
    target_usb: UsbComposition,
    target_voice: Option<VoiceConfiguration>,
    current: Option<(UsbComposition, VoiceConfiguration)>,
    step: Option<Step>,
    ordinal: u64,
    voice_settled: bool,
    successor_voice: bool,
    reboot_prepared: bool,
    reboot_verified: bool,
    finished: bool,
}
impl Writer {
    pub(super) fn create(
        root: &Path,
        proof: &RootProof,
        id: &str,
        manifest: &Value,
        journal: &crate::call_mode_restore_journal::RecoveryJournalCapture,
        backup: &crate::call_mode_backups::CapturedNativeBackup,
    ) -> Result<Self, Error> {
        let supersedes = proof
            .with_root(root, |held| -> Result<Vec<Predecessor>, Error> {
                let attempts = storage::attempts(held)?.unwrap_or_default();
                validate_combined_original_budget(held, &attempts)?;
                for old in &attempts {
                    verify_ancestor_archive(old)?;
                }
                attempts
                    .into_iter()
                    .map(|a| {
                        replay(&a)?;
                        Ok(Predecessor {
                            id: a.id.clone(),
                            head: a
                                .records
                                .last()
                                .map(|r| {
                                    storage::digest(
                                        &serde_json::to_vec(r).expect("encodable envelope"),
                                    )
                                })
                                .ok_or(Error)?,
                        })
                    })
                    .collect()
            })
            .map_err(|_| Error)??;
        let mut store = storage::Store::create(root, proof, id, manifest)?;
        let target_usb = backup.backup().usb;
        let target_voice = backup.backup().voice;
        store.append(Event::Requested {
            supersedes,
            policy: POLICY.into(),
            successor_policy: SUCCESSOR_POLICY.into(),
            original_sha256: storage::digest(&serde_json::to_vec(manifest).map_err(|_| Error)?),
            journal_sha256: journal.evidence_sha256().into(),
            backup_id: backup.id().into(),
            backup_sha256: backup.sha256(),
            target_usb,
            target_voice,
            original_voice: journal.intent().voice_recovery_binding.clone(),
        })?;
        Ok(Self {
            store,
            target_usb,
            target_voice,
            current: None,
            step: None,
            ordinal: 0,
            voice_settled: false,
            successor_voice: false,
            reboot_prepared: false,
            reboot_verified: false,
            finished: false,
        })
    }
    pub(super) fn validate(&self) -> Result<(), Error> {
        self.store.validate()?;
        self.store.with_root(|root| {
            let attempts = storage::attempts(root)?.ok_or(Error)?;
            validate_combined_original_budget(root, &attempts)?;
            let current = attempts
                .iter()
                .find(|a| a.id == self.store.id())
                .ok_or(Error)?;
            if !covers_predecessors(current, &attempts)? {
                return Err(Error);
            }
            for old in &attempts {
                replay(old)?;
                if old.id != self.store.id() {
                    verify_ancestor_archive(old)?;
                }
            }
            Ok(())
        })
    }
    pub(super) fn observe(
        &mut self,
        usb: UsbComposition,
        voice: VoiceConfiguration,
    ) -> Result<(), Error> {
        self.validate()?;
        if !usb.valid()
            || !voice.valid()
            || self.finished
            || self.current.is_some_and(|v| v != (usb, voice))
        {
            return Err(Error);
        }
        self.store.append(Event::Observed { usb, voice })?;
        self.current = Some((usb, voice));
        Ok(())
    }
    pub(super) fn prepare(&mut self, mutation: Mutation) -> Result<u64, Error> {
        self.validate()?;
        let (usb, voice) = self.current.ok_or(Error)?;
        if self.finished
            || self.reboot_verified
            || self.step.as_ref().is_some_and(|s| match s.outcome {
                None => true,
                Some(Outcome::Acknowledged) => {
                    s.readback.is_none() && !matches!(s.mutation, Mutation::ManagedVoiceCleanup)
                }
                Some(Outcome::SubmittedUnknown) => true,
                _ => false,
            })
        {
            return Err(Error);
        }
        let allowed = match &mutation {
            Mutation::ManagedVoiceCleanup => !self.voice_settled && self.ordinal == 0,
            Mutation::Usb { expected, target } => {
                self.voice_settled
                    && *expected == usb
                    && *target == self.target_usb
                    && target.valid()
            }
            Mutation::Volte { expected, target } => {
                self.voice_settled
                    && *expected == voice.volte_disabled
                    && self
                        .target_voice
                        .is_some_and(|v| v.volte_disabled == *target)
                    && *target <= 1
            }
            Mutation::Ims { expected, target } => {
                self.voice_settled
                    && *expected == voice.ims
                    && self.target_voice.is_some_and(|v| v.ims == *target)
                    && *target <= 2
            }
            Mutation::Reboot => self.voice_settled && !self.reboot_prepared,
        };
        if !allowed {
            return Err(Error);
        }
        if matches!(mutation, Mutation::Reboot) {
            self.reboot_prepared = true;
        }
        self.ordinal = self.ordinal.checked_add(1).ok_or(Error)?;
        self.store.append(Event::Prepare {
            ordinal: self.ordinal,
            mutation: mutation.clone(),
        })?;
        self.step = Some(Step {
            ordinal: self.ordinal,
            mutation,
            outcome: None,
            readback: None,
        });
        Ok(self.ordinal)
    }
    pub(super) fn validate_step(&self, ordinal: u64, mutation: &Mutation) -> Result<(), Error> {
        self.validate()?;
        match &self.step {
            Some(s) if s.ordinal == ordinal && &s.mutation == mutation && s.outcome.is_none() => {
                Ok(())
            }
            _ => Err(Error),
        }
    }
    pub(super) fn outcome(
        &mut self,
        ordinal: u64,
        mutation: &Mutation,
        outcome: Outcome,
    ) -> Result<(), Error> {
        self.validate_step(ordinal, mutation)?;
        self.store.append(Event::Outcome { ordinal, outcome })?;
        self.step.as_mut().ok_or(Error)?.outcome = Some(outcome);
        Ok(())
    }
    pub(super) fn readback(
        &mut self,
        ordinal: u64,
        mutation: &Mutation,
        readback: Readback,
    ) -> Result<(), Error> {
        self.validate()?;
        let s = self.step.as_ref().ok_or(Error)?;
        if s.ordinal != ordinal
            || &s.mutation != mutation
            || s.readback.is_some()
            || !(s.outcome == Some(Outcome::Acknowledged)
                || (matches!(mutation, Mutation::Reboot)
                    && s.outcome == Some(Outcome::SubmittedUnknown)))
        {
            return Err(Error);
        }
        let (usb, voice) = self.current.ok_or(Error)?;
        let next = match (&readback, mutation) {
            (Readback::Usb { actual }, Mutation::Usb { .. }) if actual.valid() => (*actual, voice),
            (Readback::Voice { actual }, Mutation::Ims { .. } | Mutation::Volte { .. })
                if actual.valid() =>
            {
                (usb, *actual)
            }
            (Readback::RebootSuccessor { usb, voice }, Mutation::Reboot)
                if self.successor_voice
                    && *usb == self.target_usb
                    && voice.valid()
                    && same_voice(self.target_voice, *voice) =>
            {
                self.reboot_verified = true;
                (*usb, *voice)
            }
            _ => return Err(Error),
        };
        self.store.append(Event::Readback { ordinal, readback })?;
        self.step.as_mut().ok_or(Error)?.readback = Some(readback);
        self.current = Some(next);
        Ok(())
    }
    pub(super) fn voice_settled(&mut self, evidence: Value) -> Result<(), Error> {
        self.validate()?;
        if self.voice_settled
            || self.ordinal > 1
            || self.step.as_ref().is_some_and(|s| {
                !matches!(s.mutation, Mutation::ManagedVoiceCleanup)
                    || s.outcome != Some(Outcome::Acknowledged)
            })
        {
            return Err(Error);
        }
        self.store.append(Event::VoiceSettled { evidence })?;
        self.voice_settled = true;
        Ok(())
    }
    pub(super) fn successor_voice(&mut self, evidence: Value) -> Result<(), Error> {
        self.validate()?;
        let s = self.step.as_ref().ok_or(Error)?;
        if !matches!(s.mutation, Mutation::Reboot)
            || !matches!(
                s.outcome,
                Some(Outcome::Acknowledged | Outcome::SubmittedUnknown)
            )
            || s.readback.is_some()
            || self.successor_voice
        {
            return Err(Error);
        }
        self.store
            .append(Event::SuccessorVoiceSettled { evidence })?;
        self.successor_voice = true;
        Ok(())
    }
    pub(super) fn native_closed(
        &mut self,
        usb: UsbComposition,
        voice: VoiceConfiguration,
    ) -> Result<(), Error> {
        self.validate()?;
        if !self.reboot_verified
            || !self.voice_settled
            || !self.successor_voice
            || usb != self.target_usb
            || !same_voice(self.target_voice, voice)
            || self.current != Some((usb, voice))
        {
            return Err(Error);
        }
        self.store.append(Event::TargetVerified { usb, voice })?;
        self.store.append(Event::NativeClosed)?;
        self.finished = true;
        Ok(())
    }
}
pub(super) fn same_voice(target: Option<VoiceConfiguration>, actual: VoiceConfiguration) -> bool {
    target.is_none_or(|t| t.ims == actual.ims && t.volte_disabled == actual.volte_disabled)
}

#[derive(PartialEq, Eq)]
pub(crate) enum RecoveryClassification {
    Absent,
    Pending,
    PublishedCoverage(PublishedCoverage),
}
#[derive(PartialEq, Eq)]
pub(crate) struct PublishedCoverage {
    attempt_id: String,
    receipt_sha256: String,
    slots: Vec<String>,
}
impl RecoveryClassification {
    pub(crate) fn covers_original_slot(&self, name: &str) -> bool {
        matches!(self,Self::PublishedCoverage(p)if p.slots.iter().any(|s|s==name))
    }
    pub(crate) fn has_unsupported_reason(&self) -> bool {
        matches!(self, Self::Pending)
    }
}
impl PublishedCoverage {
    pub(super) fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
    pub(super) fn receipt_sha256(&self) -> &str {
        &self.receipt_sha256
    }
}

// Compare all original bytes and inodes, allowing only the explicitly recorded
// directory-rename ctime transition on each retired top-level directory.
fn original_matches(original: &Value, current: &Value) -> bool {
    let mut expected = original.clone();
    for name in ["call-mode-restore.active", "voice-control"] {
        if !expected["slots"][name].is_null() {
            expected["slots"][name]["stamp"]["ctime"] =
                current["slots"][name]["stamp"]["ctime"].clone();
        }
    }
    expected == *current
}
#[derive(Clone, Copy)]
enum CoveragePolicy {
    Startup,
    ActiveWriter,
}

/// A positive startup result includes actual bounded metadata syncs. This is
/// not a pure parser and does not grant native I/O or root lifetime authority.
pub(crate) fn inspect_startup(root: &File) -> Result<RecoveryClassification, Error> {
    verify_and_resync_published_metadata(root, CoveragePolicy::Startup)
}

fn verify_and_resync_published_metadata(
    root: &File,
    policy: CoveragePolicy,
) -> Result<RecoveryClassification, Error> {
    let before = inspect_coverage(root, policy)?;
    if !matches!(before, RecoveryClassification::PublishedCoverage(_)) {
        return Ok(before);
    }
    // A complete readable receipt is only a candidate until its actual files
    // and parents have acknowledged sync. Do not create, repair or append any
    // record here; an earlier caller's error remains an error.
    let snapshot = storage::MetadataSnapshot::capture(root).map_err(|_| Error)?;
    // Bind the captured raw tree to a fully interpreted candidate before the
    // first recovery-file sync, not merely to an earlier path observation.
    if inspect_coverage(root, policy)? != before {
        return Err(Error);
    }
    snapshot.verify(root).map_err(|_| Error)?;
    snapshot.sync_and_verify(root).map_err(|_| Error)?;
    let after = inspect_coverage(root, policy)?;
    if after != before {
        return Err(Error);
    }
    // The complete typed revalidation above can traverse normal extensions.
    // Keep it inside the same exact raw/stamp/placement stability interval.
    snapshot.verify(root).map_err(|_| Error)?;
    Ok(after)
}

// Strict interpretation only: this function never syncs recovery records or
// mints a durability token. Its lifecycle positive reader has its own finite
// resync fence; the independent origin/namespace parsers remain read-only.
fn inspect_coverage(root: &File, policy: CoveragePolicy) -> Result<RecoveryClassification, Error> {
    let allow_active = matches!(policy, CoveragePolicy::ActiveWriter);
    validate_backup_lifecycle_anchors(root).map_err(|_| Error)?;
    if !super::backup_lifecycle::complete(root).map_err(|_| Error)? {
        return Ok(RecoveryClassification::Pending);
    }
    let Some(attempts) = storage::attempts(root)? else {
        return Ok(RecoveryClassification::Absent);
    };
    if attempts.is_empty() {
        return Ok(RecoveryClassification::Absent);
    }
    validate_combined_original_budget(root, &attempts)?;
    let Some(attempt) = select_published(&attempts)? else {
        return Ok(RecoveryClassification::Pending);
    };
    let replay = replay(attempt)?;
    for ancestor in &attempts {
        if ancestor.id != attempt.id {
            verify_ancestor_archive(ancestor)?;
        }
    }
    let Some(published) = &attempt.published else {
        return Ok(RecoveryClassification::Pending);
    };
    let Some(last) = attempt.records.last() else {
        return Ok(RecoveryClassification::Pending);
    };
    let Event::CompletionReceipt {
        manifest_sha256,
        physical_settlement_sha256,
    } = &last.event
    else {
        return Ok(RecoveryClassification::Pending);
    };
    if storage::digest(&serde_json::to_vec(published).map_err(|_| Error)?) != *manifest_sha256 {
        return Err(Error);
    }
    let mut physical = None;
    let mut host = false;
    let mut native = false;
    let mut target = false;
    let mut voice = None;
    let mut requested = None;
    let mut placements = Vec::new();
    let mut publication = false;
    for row in &attempt.records {
        match &row.event {
            Event::Requested {
                policy,
                successor_policy,
                original_sha256,
                original_voice,
                ..
            } => {
                if requested.is_some()
                    || policy != POLICY
                    || successor_policy != SUCCESSOR_POLICY
                    || *original_sha256
                        != storage::digest(
                            &serde_json::to_vec(&attempt.original).map_err(|_| Error)?,
                        )
                {
                    return Err(Error);
                }
                requested = Some(original_voice);
            }
            Event::VoiceSettled { evidence } => {
                if voice.replace(evidence).is_some() {
                    return Err(Error);
                }
            }
            Event::SuccessorVoiceSettled { evidence } => voice = Some(evidence),
            Event::TargetVerified { .. } => {
                if voice.is_none() {
                    return Err(Error);
                }
                target = true
            }
            Event::NativeClosed => {
                if !target {
                    return Err(Error);
                }
                native = true
            }
            Event::HostSettled => {
                if !native {
                    return Err(Error);
                }
                host = true
            }
            Event::PhysicalSettled => {
                if !host || physical.is_some() {
                    return Err(Error);
                }
                physical = Some(storage::digest(
                    &serde_json::to_vec(row).map_err(|_| Error)?,
                ));
            }
            Event::RetirementPrepare { slot, archive } => {
                if physical.is_none()
                    || !matches!(
                        (slot.as_str(), archive.as_str()),
                        ("call-mode-restore.active", "journal")
                            | ("voice-control", "voice-control")
                    )
                {
                    return Err(Error);
                }
            }
            Event::PlacementCommitted { slot, archive } => {
                if physical.is_none()
                    || !matches!(
                        (slot.as_str(), archive.as_str()),
                        ("call-mode-restore.active", "journal")
                            | ("voice-control", "voice-control")
                    )
                {
                    return Err(Error);
                }
                placements.push(slot.clone());
            }
            Event::PublicationCommitted {
                manifest_sha256: sha,
            } => {
                if physical.is_none() || sha != manifest_sha256 {
                    return Err(Error);
                }
                publication = true
            }
            Event::CompletionReceipt { .. } => {
                if !publication {
                    return Err(Error);
                }
            }
            _ => {}
        }
    }
    if physical.as_ref() != Some(physical_settlement_sha256) || requested.is_none() {
        return Err(Error);
    }
    for slot in ["call-mode-restore.active", "voice-control"] {
        if !attempt.original["slots"][slot].is_null() && !placements.iter().any(|n| n == slot) {
            return Err(Error);
        }
    }
    // Current active journal is never covered. A normal pending write always
    // wins over historical coverage and requires a fresh reconciliation.
    let active = nix::sys::stat::fstatat(
        root,
        "call-mode-restore.active",
        nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
    );
    if !allow_active && !matches!(active, Err(nix::errno::Errno::ENOENT)) {
        return Ok(RecoveryClassification::Pending);
    }
    let mut current = evidence::EvidenceTree::persisted_manifest(root, Some(&attempt.archive))
        .map_err(|_| Error)?;
    let receipt_sha256 = storage::digest(&serde_json::to_vec(last).map_err(|_| Error)?);
    let coverage = PublishedCoverage {
        attempt_id: attempt.id.clone(),
        receipt_sha256: receipt_sha256.clone(),
        slots: Vec::new(),
    };
    normalize_extensions(root, attempt, &mut current, &coverage, allow_active)?;
    if !original_matches(&attempt.original, &current) || *published != current {
        return Err(Error);
    }
    let voice = voice.ok_or(Error)?;
    if voice["original_manifest_sha256"]
        != storage::digest(&serde_json::to_vec(&attempt.original).map_err(|_| Error)?)
    {
        return Err(Error);
    }
    let archived_journal = storage::directory(&attempt.archive, "journal").map_err(|_| Error)?;
    let original_journal =
        crate::call_mode_restore_journal::inspect_readonly_directory(&archived_journal)
            .map_err(|_| Error)?;
    if original_journal.intent.voice_recovery_binding.as_ref() != requested.and_then(|v| v.as_ref())
        || original_journal.chain_sha256 != replay.journal_sha256
        || original_journal.backup_id != replay.backup_id
        || original_journal.backup_sha256 != replay.backup_sha256
        || original_journal.intent.target_usb != replay.target_usb
        || original_journal.intent.target_voice != replay.target_voice
    {
        return Err(Error);
    }
    let archived_voice = storage::optional_archive_voice(&attempt.archive)?;
    crate::call_mode_adb::startup_recovery::verify_settlement_evidence(
        voice,
        archived_voice.as_ref(),
        requested.and_then(|v| v.as_ref()),
    )
    .map_err(|_| Error)?;
    let receipt_sha256 = storage::digest(&serde_json::to_vec(last).map_err(|_| Error)?);
    Ok(RecoveryClassification::PublishedCoverage(
        PublishedCoverage {
            attempt_id: attempt.id.clone(),
            receipt_sha256,
            slots: [
                "call-mode-operation.lock",
                ".module-adb-interface.lock",
                "module-backups",
                "voice-runtime",
                "voice-control",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        },
    ))
}

struct Replay {
    journal_sha256: String,
    backup_id: String,
    backup_sha256: String,
    target_usb: UsbComposition,
    target_voice: Option<VoiceConfiguration>,
    physical_sha: Option<String>,
    completion: bool,
}
fn replay(attempt: &storage::Attempt) -> Result<Replay, Error> {
    let Some(first) = attempt.records.first() else {
        return Err(Error);
    };
    let Event::Requested {
        policy,
        successor_policy,
        original_sha256,
        journal_sha256,
        backup_id,
        backup_sha256,
        target_usb,
        target_voice,
        ..
    } = &first.event
    else {
        return Err(Error);
    };
    if policy != POLICY
        || successor_policy != SUCCESSOR_POLICY
        || *original_sha256
            != storage::digest(&serde_json::to_vec(&attempt.original).map_err(|_| Error)?)
        || !target_usb.valid()
        || target_voice.is_some_and(|v| !v.valid())
    {
        return Err(Error);
    }
    let mut current = None;
    let mut step: Option<Step> = None;
    let mut ordinal = 0;
    let mut voice = false;
    let mut successor_voice = false;
    let mut reboot = false;
    let mut reboot_prepared = false;
    let mut target = false;
    let mut closed = false;
    let mut host = false;
    let mut physical = None;
    let mut preparing: Option<(String, String)> = None;
    let mut placements = Vec::new();
    let mut published = None;
    let mut completion = false;
    for row in &attempt.records[1..] {
        if completion {
            return Err(Error);
        }
        match &row.event {
            Event::Requested { .. } => return Err(Error),
            Event::Observed { usb, voice: v } => {
                if target || !usb.valid() || !v.valid() || current.is_some_and(|c| c != (*usb, *v))
                {
                    return Err(Error);
                }
                current = Some((*usb, *v));
            }
            Event::Prepare {
                ordinal: n,
                mutation,
            } => {
                let (u, v) = current.ok_or(Error)?;
                if target
                    || reboot
                    || *n != ordinal + 1
                    || step.as_ref().is_some_and(|s| match s.outcome {
                        None | Some(Outcome::SubmittedUnknown) => true,
                        Some(Outcome::Acknowledged) => {
                            s.readback.is_none()
                                && !matches!(s.mutation, Mutation::ManagedVoiceCleanup)
                        }
                        _ => false,
                    })
                {
                    return Err(Error);
                }
                let valid = match mutation {
                    Mutation::ManagedVoiceCleanup => !voice && ordinal == 0,
                    Mutation::Usb { expected, target } => {
                        voice && *expected == u && target == target_usb
                    }
                    Mutation::Volte { expected, target } => {
                        voice
                            && *expected == v.volte_disabled
                            && target_voice.is_some_and(|v| v.volte_disabled == *target)
                    }
                    Mutation::Ims { expected, target } => {
                        voice
                            && *expected == v.ims
                            && target_voice.is_some_and(|v| v.ims == *target)
                    }
                    Mutation::Reboot => voice && !reboot_prepared,
                };
                if !valid {
                    return Err(Error);
                }
                if matches!(mutation, Mutation::Reboot) {
                    reboot_prepared = true;
                }
                ordinal = *n;
                step = Some(Step {
                    ordinal: *n,
                    mutation: mutation.clone(),
                    outcome: None,
                    readback: None,
                });
            }
            Event::Outcome {
                ordinal: n,
                outcome,
            } => {
                let s = step.as_mut().ok_or(Error)?;
                if s.ordinal != *n || s.outcome.is_some() || target {
                    return Err(Error);
                }
                s.outcome = Some(*outcome)
            }
            Event::VoiceSettled { evidence } => {
                if voice
                    || ordinal > 1
                    || step.as_ref().is_some_and(|s| {
                        !matches!(s.mutation, Mutation::ManagedVoiceCleanup)
                            || s.outcome != Some(Outcome::Acknowledged)
                    })
                    || evidence["original_manifest_sha256"] != *original_sha256
                {
                    return Err(Error);
                }
                voice = true
            }
            Event::SuccessorVoiceSettled { evidence } => {
                let s = step.as_ref().ok_or(Error)?;
                if successor_voice
                    || !matches!(s.mutation, Mutation::Reboot)
                    || !matches!(
                        s.outcome,
                        Some(Outcome::Acknowledged | Outcome::SubmittedUnknown)
                    )
                    || s.readback.is_some()
                    || evidence["original_manifest_sha256"] != *original_sha256
                    || evidence["successor"].is_null()
                {
                    return Err(Error);
                }
                successor_voice = true
            }
            Event::Readback {
                ordinal: n,
                readback,
            } => {
                let s = step.as_mut().ok_or(Error)?;
                if s.ordinal != *n
                    || s.readback.is_some()
                    || !(s.outcome == Some(Outcome::Acknowledged)
                        || (matches!(s.mutation, Mutation::Reboot)
                            && s.outcome == Some(Outcome::SubmittedUnknown)))
                {
                    return Err(Error);
                }
                let (u, v) = current.ok_or(Error)?;
                current = Some(match (&s.mutation, readback) {
                    (Mutation::Usb { .. }, Readback::Usb { actual }) if actual.valid() => {
                        (*actual, v)
                    }
                    (Mutation::Volte { .. } | Mutation::Ims { .. }, Readback::Voice { actual })
                        if actual.valid() =>
                    {
                        (u, *actual)
                    }
                    (Mutation::Reboot, Readback::RebootSuccessor { usb, voice })
                        if successor_voice
                            && *usb == *target_usb
                            && voice.valid()
                            && same_voice(*target_voice, *voice) =>
                    {
                        reboot = true;
                        (*usb, *voice)
                    }
                    _ => return Err(Error),
                });
                s.readback = Some(*readback)
            }
            Event::TargetVerified { usb, voice: v } => {
                if target
                    || !reboot
                    || !successor_voice
                    || *usb != *target_usb
                    || !same_voice(*target_voice, *v)
                    || current != Some((*usb, *v))
                {
                    return Err(Error);
                }
                target = true
            }
            Event::NativeClosed => {
                if !target || closed {
                    return Err(Error);
                }
                closed = true
            }
            Event::HostSettled => {
                if !closed || host {
                    return Err(Error);
                }
                host = true
            }
            Event::PhysicalSettled => {
                if !host || physical.is_some() {
                    return Err(Error);
                }
                physical = Some(storage::digest(
                    &serde_json::to_vec(row).map_err(|_| Error)?,
                ))
            }
            Event::RetirementPrepare { slot, archive } => {
                if physical.is_none()
                    || preparing.is_some()
                    || placements.contains(slot)
                    || !matches!(
                        (slot.as_str(), archive.as_str()),
                        ("call-mode-restore.active", "journal")
                            | ("voice-control", "voice-control")
                    )
                    || attempt.original["slots"][slot].is_null()
                {
                    return Err(Error);
                }
                preparing = Some((slot.clone(), archive.clone()))
            }
            Event::PlacementCommitted { slot, archive } => {
                if preparing.as_ref() != Some(&(slot.clone(), archive.clone())) {
                    return Err(Error);
                }
                preparing = None;
                placements.push(slot.clone())
            }
            Event::PublicationCommitted { manifest_sha256 } => {
                if physical.is_none()
                    || preparing.is_some()
                    || published.is_some()
                    || ["call-mode-restore.active", "voice-control"]
                        .iter()
                        .any(|s| {
                            !attempt.original["slots"][*s].is_null()
                                && !placements.iter().any(|p| p == s)
                        })
                {
                    return Err(Error);
                }
                published = Some(manifest_sha256.clone())
            }
            Event::CompletionReceipt {
                manifest_sha256,
                physical_settlement_sha256,
            } => {
                if published.as_ref() != Some(manifest_sha256)
                    || physical.as_ref() != Some(physical_settlement_sha256)
                {
                    return Err(Error);
                }
                completion = true
            }
        }
    }
    Ok(Replay {
        journal_sha256: journal_sha256.clone(),
        backup_id: backup_id.clone(),
        backup_sha256: backup_sha256.clone(),
        target_usb: *target_usb,
        target_voice: *target_voice,
        physical_sha: physical,
        completion,
    })
}

pub(super) struct MetadataResume {
    pub(super) attempt: storage::Attempt,
    pub(super) physical_sha: String,
}
pub(super) fn metadata_resume_candidate(root: &File) -> Result<Option<MetadataResume>, Error> {
    let Some(mut attempts) = storage::attempts(root)? else {
        return Ok(None);
    };
    validate_combined_original_budget(root, &attempts)?;
    let mut selected = None;
    for (index, attempt) in attempts.iter().enumerate() {
        replay(attempt)?;
        if covers_predecessors(attempt, &attempts)? {
            if selected.replace(index).is_some() {
                return Err(Error);
            }
        }
    }
    let Some(index) = selected else {
        return Ok(None);
    };
    let attempt = attempts.remove(index);
    let replay = replay(&attempt)?;
    let Some(physical_sha) = replay.physical_sha else {
        return Ok(None);
    };
    if replay.completion {
        return Ok(None);
    }
    let current =
        evidence::EvidenceTree::transition_manifest(root, &attempt.archive).map_err(|_| Error)?;
    if !original_matches(&attempt.original, &current) {
        return Err(Error);
    }
    let original_journal = if let Some(journal) =
        storage::optional_dir(&attempt.archive, "journal").map_err(|_| Error)?
    {
        journal
    } else {
        storage::directory(root, "call-mode-restore.active").map_err(|_| Error)?
    };
    let original = crate::call_mode_restore_journal::inspect_readonly_directory(&original_journal)
        .map_err(|_| Error)?;
    if original.chain_sha256 != replay.journal_sha256
        || original.backup_id != replay.backup_id
        || original.backup_sha256 != replay.backup_sha256
        || original.intent.target_usb != replay.target_usb
        || original.intent.target_voice != replay.target_voice
    {
        return Err(Error);
    }
    let voice = attempt
        .records
        .iter()
        .rev()
        .find_map(|r| match &r.event {
            Event::SuccessorVoiceSettled { evidence } => Some(evidence),
            _ => None,
        })
        .ok_or(Error)?;
    let control =
        match storage::optional_dir(&attempt.archive, "voice-control").map_err(|_| Error)? {
            Some(f) => Some(f),
            None => storage::optional_dir(root, "voice-control").map_err(|_| Error)?,
        };
    crate::call_mode_adb::startup_recovery::verify_settlement_evidence(
        voice,
        control.as_ref(),
        original.intent.voice_recovery_binding.as_ref(),
    )
    .map_err(|_| Error)?;
    Ok(Some(MetadataResume {
        attempt,
        physical_sha,
    }))
}
pub(super) fn finish_publication(
    root: &Path,
    proof: &RootProof,
    store: &mut storage::Store,
    original: &Value,
    physical_sha: &str,
) -> Result<String, Error> {
    proof
        .with_root(root, |held| -> Result<(), Error> {
            let ar = store.archive_parent()?;
            let current =
                evidence::EvidenceTree::transition_manifest(held, &ar).map_err(|_| Error)?;
            if !original_matches(original, &current) {
                return Err(Error);
            }
            Ok(())
        })
        .map_err(|_| Error)??;
    for (slot, archive) in [
        ("call-mode-restore.active", "journal"),
        ("voice-control", "voice-control"),
    ] {
        if original["slots"][slot].is_null() {
            continue;
        }
        let committed=store.records().iter().any(|r|matches!(&r.event,Event::PlacementCommitted{slot:s,archive:a}if s==slot&&a==archive));
        if committed {
            continue;
        }
        let prepared = store.records().iter().any(
            |r| matches!(&r.event,Event::RetirementPrepare{slot:s,archive:a}if s==slot&&a==archive),
        );
        if !prepared {
            #[cfg(test)]
            observe_publication(PublicationPoint::BeforePrepare, archive)?;
            store.append(Event::RetirementPrepare {
                slot: slot.into(),
                archive: archive.into(),
            })?;
            #[cfg(test)]
            observe_publication(PublicationPoint::AfterPrepare, archive)?;
        }
        let ar = store.archive_parent()?;
        let live = proof
            .with_root(root, |held| storage::optional_dir(held, slot))
            .map_err(|_| Error)?
            .map_err(|_| Error)?
            .is_some();
        let retired = storage::optional_dir(&ar, archive)
            .map_err(|_| Error)?
            .is_some();
        match (live, retired) {
            (true, false) => store.retire_slot(slot, archive)?,
            (false, true) => store.sync_publication_parents()?,
            _ => return Err(Error),
        }
        proof
            .with_root(root, |held| -> Result<(), Error> {
                let current =
                    evidence::EvidenceTree::transition_manifest(held, &ar).map_err(|_| Error)?;
                if !original_matches(original, &current) {
                    return Err(Error);
                }
                Ok(())
            })
            .map_err(|_| Error)??;
        #[cfg(test)]
        observe_publication(PublicationPoint::BeforePlacement, archive)?;
        store.append(Event::PlacementCommitted {
            slot: slot.into(),
            archive: archive.into(),
        })?;
        #[cfg(test)]
        observe_publication(PublicationPoint::AfterPlacement, archive)?;
    }
    let ar = store.archive_parent()?;
    let published = proof
        .with_root(root, |held| {
            evidence::EvidenceTree::persisted_manifest(held, Some(&ar))
        })
        .map_err(|_| Error)?
        .map_err(|_| Error)?;
    if !original_matches(original, &published) {
        return Err(Error);
    }
    #[cfg(test)]
    observe_publication(PublicationPoint::BeforeManifest, "publication")?;
    let sha = store.write_or_verify_value("published-manifest.json", &published)?;
    #[cfg(test)]
    observe_publication(PublicationPoint::AfterManifest, "publication")?;
    if !store
        .records()
        .iter()
        .any(|r| matches!(&r.event, Event::PublicationCommitted { .. }))
    {
        #[cfg(test)]
        observe_publication(PublicationPoint::BeforePublication, "publication")?;
        store.append(Event::PublicationCommitted {
            manifest_sha256: sha.clone(),
        })?;
        #[cfg(test)]
        observe_publication(PublicationPoint::AfterPublication, "publication")?;
    }
    if let Some(last) = store.records().last() {
        if matches!(last.event, Event::CompletionReceipt { .. }) {
            return Ok(storage::digest(
                &serde_json::to_vec(last).map_err(|_| Error)?,
            ));
        }
    }
    #[cfg(test)]
    observe_publication(PublicationPoint::BeforeReceipt, "publication")?;
    let receipt = store.append(Event::CompletionReceipt {
        manifest_sha256: sha,
        physical_settlement_sha256: physical_sha.into(),
    })?;
    #[cfg(test)]
    observe_publication(PublicationPoint::AfterReceipt, "publication")?;
    Ok(receipt)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct NormalJournalOrigin {
    publication: String,
    receipt_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Origin {
    schema: u32,
    publication: String,
    receipt_sha256: String,
    kind: String,
    id: String,
    sha256: String,
    entry: Option<Value>,
}
fn read_origins(root: &File, p: &PublishedCoverage) -> Result<Vec<Origin>, Error> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for value in storage::origins(root)? {
        let origin: Origin = serde_json::from_value(value).map_err(|_| Error)?;
        if origin.schema != 1
            || !matches!(
                origin.kind.as_str(),
                "backup" | "voice" | "backup_lifecycle"
            )
            || origin.id.is_empty()
            || origin.sha256.len() != 64
            || !seen.insert((origin.kind.clone(), origin.id.clone()))
        {
            return Err(Error);
        }
        if origin.kind == "backup_lifecycle" {
            check_backup_lifecycle_anchor(root, &origin).map_err(|_| Error)?;
        }
        if origin.publication == p.attempt_id && origin.receipt_sha256 == p.receipt_sha256 {
            out.push(origin)
        } else {
            let ancestors = storage::attempts(root)?.ok_or(Error)?;
            let at = ancestors
                .iter()
                .find(|a| a.id == origin.publication)
                .ok_or(Error)?;
            let last = at.records.last().ok_or(Error)?;
            if !matches!(last.event, Event::CompletionReceipt { .. })
                || storage::digest(&serde_json::to_vec(last).map_err(|_| Error)?)
                    != origin.receipt_sha256
            {
                return Err(Error);
            }
        }
    }
    Ok(out)
}
fn children(value: &Value) -> Result<&Vec<Value>, Error> {
    value["children"].as_array().ok_or(Error)
}
fn same_directory_identity(old: &Value, new: &Value) -> bool {
    ["dev", "ino", "mode", "uid", "gid"]
        .iter()
        .all(|key| old["stamp"][*key] == new["stamp"][*key])
        && old["name"] == new["name"]
        && old["sha256"] == new["sha256"]
}
// APFS counts directory entries in nlink; other supported filesystems may
// keep nlink unchanged for regular-file additions. The exact verified name
// set constrains this mutable directory observation; original child metadata
// remains fully immutable and is never normalized by this rule.
fn backup_directory_links_match(old: &Value, new: &Value) -> bool {
    let Some(old_links) = old["stamp"]["links"].as_u64() else {
        return false;
    };
    let Some(new_links) = new["stamp"]["links"].as_u64() else {
        return false;
    };
    let Some(old_names) = old["children"].as_array() else {
        return false;
    };
    let Some(new_names) = new["children"].as_array() else {
        return false;
    };
    let Some(added) = new_names.len().checked_sub(old_names.len()) else {
        return false;
    };
    new_links == old_links || old_links.checked_add(added as u64) == Some(new_links)
}
fn backup_extension(old: &Value, new: &Value, origins: &[Origin]) -> Result<(), Error> {
    if old == new {
        return Ok(());
    }
    if new.is_null() {
        return Err(Error);
    }
    let previous = if old.is_null() {
        Vec::new()
    } else {
        if !same_directory_identity(old, new) {
            return Err(Error);
        }
        children(old)?.clone()
    };
    let actual = children(new)?;
    for node in &previous {
        if !actual.iter().any(|v| v == node) {
            return Err(Error);
        }
    }
    for node in actual {
        if previous.iter().any(|v| v == node) {
            continue;
        }
        let id = node["name"].as_str().ok_or(Error)?;
        if !crate::call_mode_backups::valid_backup_id(id)
            || !node["children"].as_array().is_some_and(Vec::is_empty)
            || node["stamp"]["links"].as_u64() != Some(1)
            || node["stamp"]["mode"]
                .as_u64()
                .is_none_or(|mode| mode & nix::libc::S_IFMT as u64 != nix::libc::S_IFREG as u64)
            || !origins.iter().any(|o| {
                o.kind == "backup"
                    && o.id == id
                    && o.sha256 == node["sha256"]
                    && o.entry.as_ref() == Some(node)
            })
        {
            return Err(Error);
        }
    }
    if !old.is_null() && !backup_directory_links_match(old, new) {
        return Err(Error);
    }
    Ok(())
}
fn normalize_extensions(
    root: &File,
    attempt: &storage::Attempt,
    current: &mut Value,
    p: &PublishedCoverage,
    allow_active: bool,
) -> Result<(), Error> {
    let origins = read_origins(root, p)?;
    if let Some(checkpoints) =
        super::backup_lifecycle::verified_checkpoints(root).map_err(|_| Error)?
    {
        // Each checkpoint is backed by the complete typed generation chain,
        // exact live inventory and every retained original archive inode/raw.
        // Only a checkpoint independently accepted by the original receipt and
        // pre-existing normal origins can normalize the current public slot.
        let checkpoint = checkpoints.into_iter().find(|node| {
            backup_extension(&attempt.original["slots"]["module-backups"], node, &origins).is_ok()
        });
        if let Some(node) = checkpoint {
            current["slots"]["module-backups"] = node;
        } else if !super::backup_lifecycle::is_empty(root).map_err(|_| Error)? {
            return Err(Error);
        }
    }
    backup_extension(
        &attempt.original["slots"]["module-backups"],
        &current["slots"]["module-backups"],
        &origins,
    )?;
    current["slots"]["module-backups"] = attempt.original["slots"]["module-backups"].clone();
    let voices = origins
        .iter()
        .filter(|o| o.kind == "voice")
        .map(|o| (o.id.clone(), o.sha256.clone()))
        .collect::<Vec<_>>();
    if allow_active {
        crate::call_mode_adb::verify_normal_origin_writer_inventory(root, &voices)
            .map_err(|_| Error)?;
    } else {
        crate::call_mode_adb::verify_normal_origin_inventory(root, &voices).map_err(|_| Error)?;
    }
    if let Some(active) =
        storage::optional_dir(root, "call-mode-restore.active").map_err(|_| Error)?
    {
        if !allow_active {
            return Err(Error);
        }
        let journal = crate::call_mode_restore_journal::inspect_readonly_directory(&active)
            .map_err(|_| Error)?;
        if journal.recovery_origin.as_ref()
            != Some(&NormalJournalOrigin {
                publication: p.attempt_id.clone(),
                receipt_sha256: p.receipt_sha256.clone(),
            })
        {
            return Err(Error);
        }
    }
    Ok(())
}
/// Captured before a known private writer changes one namespace. It contains
/// actual pinned lineage and exact input inventory, never caller booleans.
pub(crate) struct NormalOriginPermit {
    root: std::path::PathBuf,
    proof: RootProof,
    publication: NormalJournalOrigin,
    before: Value,
}
pub(crate) fn capture_normal_origin(
    root: &Path,
    proof: &RootProof,
) -> io::Result<Option<NormalOriginPermit>> {
    proof
        .with_root(root, |held| -> io::Result<Option<NormalOriginPermit>> {
            match verify_and_resync_published_metadata(held, CoveragePolicy::ActiveWriter)
                .map_err(|_| io::Error::other("normal origin publication is unavailable"))?
            {
                RecoveryClassification::Absent => Ok(None),
                RecoveryClassification::Pending => {
                    Err(io::Error::other("normal origin publication is pending"))
                }
                RecoveryClassification::PublishedCoverage(p) => {
                    let before = evidence::EvidenceTree::persisted_manifest(held, None)?;
                    Ok(Some(NormalOriginPermit {
                        root: root.into(),
                        proof: proof.clone(),
                        publication: NormalJournalOrigin {
                            publication: p.attempt_id,
                            receipt_sha256: p.receipt_sha256,
                        },
                        before,
                    }))
                }
            }
        })
        .map_err(|_| io::Error::other("normal origin root changed"))?
}
pub(crate) fn capture_normal_journal_origin(
    root: &File,
) -> io::Result<Option<NormalJournalOrigin>> {
    match verify_and_resync_published_metadata(root, CoveragePolicy::ActiveWriter)
        .map_err(|_| io::Error::other("normal journal origin unavailable"))?
    {
        RecoveryClassification::Absent => Ok(None),
        RecoveryClassification::PublishedCoverage(p) => Ok(Some(NormalJournalOrigin {
            publication: p.attempt_id,
            receipt_sha256: p.receipt_sha256,
        })),
        RecoveryClassification::Pending => Err(io::Error::other("normal journal origin pending")),
    }
}
impl NormalOriginPermit {
    fn check_except(&self, held: &File, slot: &str) -> io::Result<Value> {
        let current = evidence::EvidenceTree::persisted_manifest(held, None)?;
        let mut comparison = current.clone();
        comparison["slots"][slot] = self.before["slots"][slot].clone();
        if comparison != self.before {
            return Err(io::Error::other("normal origin input changed"));
        }
        let attempts = storage::attempts(held)
            .map_err(|_| io::Error::other("normal origin ledger changed"))?
            .ok_or_else(|| io::Error::other("normal origin ledger disappeared"))?;
        let at = attempts
            .iter()
            .find(|a| a.id == self.publication.publication)
            .ok_or_else(|| io::Error::other("normal origin publication changed"))?;
        let last = at
            .records
            .last()
            .ok_or_else(|| io::Error::other("normal origin receipt missing"))?;
        if storage::digest(&serde_json::to_vec(last).map_err(io::Error::other)?)
            != self.publication.receipt_sha256
            || !matches!(last.event, Event::CompletionReceipt { .. })
        {
            return Err(io::Error::other("normal origin receipt changed"));
        }
        Ok(current)
    }
    pub(crate) fn register_voice(self, id: &str, sha: &str) -> io::Result<()> {
        self.proof
            .with_root(&self.root, |held| -> io::Result<()> {
                self.check_except(held, "voice-control")?;
                let p = PublishedCoverage {
                    attempt_id: self.publication.publication.clone(),
                    receipt_sha256: self.publication.receipt_sha256.clone(),
                    slots: Vec::new(),
                };
                let voices = read_origins(held, &p)
                    .map_err(|_| io::Error::other("normal voice origins unavailable"))?
                    .into_iter()
                    .filter(|o| o.kind == "voice")
                    .map(|o| (o.id, o.sha256))
                    .collect::<Vec<_>>();
                crate::call_mode_adb::verify_normal_origin_registration(held, &voices, id, sha)?;
                let origin = Origin {
                    schema: 1,
                    publication: p.attempt_id,
                    receipt_sha256: p.receipt_sha256,
                    kind: "voice".into(),
                    id: id.into(),
                    sha256: sha.into(),
                    entry: None,
                };
                storage::write_origin(
                    held,
                    &serde_json::to_value(origin).map_err(io::Error::other)?,
                )
            })
            .map_err(|_| io::Error::other("normal voice origin root changed"))?
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Predecessor {
    id: String,
    head: String,
}
fn select_published(attempts: &[storage::Attempt]) -> Result<Option<&storage::Attempt>, Error> {
    let mut candidates = Vec::new();
    for a in attempts {
        let r = replay(a)?;
        if !r.completion {
            continue;
        }
        let Event::Requested { supersedes, .. } = &a.records[0].event else {
            return Err(Error);
        };
        if supersedes.len() != attempts.len() - 1 {
            continue;
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut valid = true;
        for prev in supersedes {
            if !seen.insert(&prev.id) {
                return Err(Error);
            }
            let Some(other) = attempts.iter().find(|o| o.id == prev.id && o.id != a.id) else {
                valid = false;
                break;
            };
            let head = other
                .records
                .last()
                .map(|row| storage::digest(&serde_json::to_vec(row).expect("encodable envelope")))
                .ok_or(Error)?;
            if prev.head != head {
                valid = false;
                break;
            }
        }
        if valid {
            candidates.push(a)
        }
    }
    if candidates.len() > 1 {
        return Err(Error);
    }
    Ok(candidates.pop())
}

fn covers_predecessors(a: &storage::Attempt, attempts: &[storage::Attempt]) -> Result<bool, Error> {
    let Event::Requested { supersedes, .. } = &a.records.first().ok_or(Error)?.event else {
        return Err(Error);
    };
    if supersedes.len() != attempts.len() - 1 {
        return Ok(false);
    }
    let mut seen = std::collections::BTreeSet::new();
    for prev in supersedes {
        if !seen.insert(&prev.id) {
            return Err(Error);
        }
        let Some(other) = attempts.iter().find(|o| o.id == prev.id && o.id != a.id) else {
            return Ok(false);
        };
        let head = storage::digest(
            &serde_json::to_vec(other.records.last().ok_or(Error)?).map_err(|_| Error)?,
        );
        if prev.head != head {
            return Ok(false);
        }
    }
    Ok(true)
}

fn verify_ancestor_archive(attempt: &storage::Attempt) -> Result<(), Error> {
    let state = replay(attempt)?;
    let slots =
        evidence::EvidenceTree::retired_slots_manifest(&attempt.archive).map_err(|_| Error)?;
    for name in ["call-mode-restore.active", "voice-control"] {
        if slots[name].is_null() {
            continue;
        }
        let expected = if state.completion {
            &attempt.published.as_ref().ok_or(Error)?["slots"][name]
        } else {
            &attempt.original["slots"][name]
        };
        let mut actual = slots[name].clone();
        if !state.completion {
            actual["stamp"]["ctime"] = expected["stamp"]["ctime"].clone();
        }
        if actual != *expected {
            return Err(Error);
        }
    }
    Ok(())
}

fn validate_combined_original_budget(
    root: &File,
    attempts: &[storage::Attempt],
) -> Result<(), Error> {
    let (nodes, bytes) = combined_original_usage(root, attempts)?;
    if nodes > 768 || bytes > 8 * 1024 * 1024 {
        return Err(Error);
    }
    Ok(())
}
fn combined_original_usage(
    root: &File,
    attempts: &[storage::Attempt],
) -> Result<(usize, u64), Error> {
    fn visit(
        node: &Value,
        seen: &mut std::collections::BTreeSet<(u64, u64)>,
        bytes: &mut u64,
    ) -> Result<(), Error> {
        if node.is_null() {
            return Ok(());
        }
        let stamp = &node["stamp"];
        let identity = (
            stamp["dev"].as_u64().ok_or(Error)?,
            stamp["ino"].as_u64().ok_or(Error)?,
        );
        if !seen.insert(identity) {
            return Ok(());
        }
        if seen.len() > 512 {
            return Err(Error);
        }
        let mode = stamp["mode"].as_u64().ok_or(Error)?;
        if mode & nix::libc::S_IFMT as u64 == nix::libc::S_IFREG as u64 {
            *bytes = bytes
                .checked_add(stamp["size"].as_u64().ok_or(Error)?)
                .ok_or(Error)?;
            if *bytes > 8 * 1024 * 1024 {
                return Err(Error);
            }
        }
        for child in children(node)? {
            visit(child, seen, bytes)?;
        }
        Ok(())
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut bytes = 0;
    let live =
        if attempts.is_empty() && root.metadata().map_err(|_| Error)?.mode() & 0o7777 != 0o700 {
            evidence::backup_lifecycle_snapshot(root)
                .map_err(|_| Error)?
                .0
        } else {
            evidence::EvidenceTree::persisted_manifest(root, None).map_err(|_| Error)?
        };
    for node in live["slots"].as_object().ok_or(Error)?.values() {
        visit(node, &mut seen, &mut bytes)?;
    }
    for attempt in attempts {
        let slots =
            evidence::EvidenceTree::retired_slots_manifest(&attempt.archive).map_err(|_| Error)?;
        for node in slots.as_object().ok_or(Error)?.values() {
            visit(node, &mut seen, &mut bytes)?;
        }
    }
    let (lifecycle_nodes, lifecycle_bytes) =
        super::backup_lifecycle::budget_usage(root).map_err(|_| Error)?;
    Ok((
        seen.len().checked_add(lifecycle_nodes).ok_or(Error)?,
        bytes.checked_add(lifecycle_bytes as u64).ok_or(Error)?,
    ))
}
pub(super) fn reserve_backup_lifecycle_original_budget(
    root: &File,
    added_nodes: usize,
    added_bytes: usize,
) -> io::Result<()> {
    let attempts = storage::attempts(root)
        .map_err(|_| io::Error::other("backup lifecycle budget input changed"))?
        .unwrap_or_default();
    let (nodes, bytes) = combined_original_usage(root, &attempts)
        .map_err(|_| io::Error::other("backup lifecycle original budget unavailable"))?;
    if nodes.checked_add(added_nodes).is_none_or(|v| v > 768)
        || bytes
            .checked_add(added_bytes as u64)
            .is_none_or(|v| v > 8 * 1024 * 1024)
    {
        return Err(io::Error::other(
            "backup lifecycle combined durable capacity exhausted",
        ));
    }
    Ok(())
}

// Pure metadata receipt binding used by the independent lifecycle parser. It
// deliberately does not call inspect_coverage (which itself validates lifecycle).
pub(super) fn verify_backup_lifecycle_origin(
    root: &File,
    origin: Option<&NormalJournalOrigin>,
) -> io::Result<()> {
    if let Some(origin) = origin {
        verify_backup_lifecycle_origins(root, &[origin])
    } else {
        Ok(())
    }
}
pub(super) fn verify_backup_lifecycle_origins(
    root: &File,
    origins: &[&NormalJournalOrigin],
) -> io::Result<()> {
    if origins.is_empty() {
        return Ok(());
    }
    let attempts = storage::lifecycle_origin_attempts(root)
        .map_err(|_| io::Error::other("backup lifecycle publication missing"))?
        .ok_or_else(|| io::Error::other("backup lifecycle publication missing"))?;
    for origin in origins {
        let at = attempts
            .iter()
            .find(|a| a.id == origin.publication)
            .ok_or_else(|| io::Error::other("backup lifecycle original publication changed"))?;
        let last = at
            .records
            .last()
            .ok_or_else(|| io::Error::other("backup lifecycle receipt missing"))?;
        if !matches!(last.event, Event::CompletionReceipt { .. })
            || storage::digest(&serde_json::to_vec(last).map_err(io::Error::other)?)
                != origin.receipt_sha256
            || !replay(at)
                .map_err(|_| io::Error::other("backup lifecycle publication invalid"))?
                .completion
        {
            return Err(io::Error::other("backup lifecycle receipt changed"));
        }
    }
    Ok(())
}

fn check_backup_lifecycle_anchor(root: &File, origin: &Origin) -> io::Result<()> {
    let entry = origin
        .entry
        .as_ref()
        .ok_or_else(|| io::Error::other("backup lifecycle anchor missing"))?;
    if origin.schema != 1
        || origin.kind != "backup_lifecycle"
        || origin.id != super::backup_lifecycle::NAME
        || origin.sha256 != storage::digest(&serde_json::to_vec(entry).map_err(io::Error::other)?)
    {
        return Err(io::Error::other("backup lifecycle anchor changed"));
    }
    super::backup_lifecycle::validate_namespace_anchor(root, entry)
}
fn validate_backup_lifecycle_anchors(root: &File) -> io::Result<()> {
    let mut anchors = Vec::new();
    let mut bindings = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for value in
        storage::origins(root).map_err(|_| io::Error::other("backup lifecycle origins changed"))?
    {
        let origin: Origin = serde_json::from_value(value)
            .map_err(|_| io::Error::other("backup lifecycle origin invalid"))?;
        if origin.schema != 1
            || !matches!(
                origin.kind.as_str(),
                "voice" | "backup" | "backup_lifecycle"
            )
            || origin.id.is_empty()
            || origin.sha256.len() != 64
            || !seen.insert((origin.kind.clone(), origin.id.clone()))
        {
            return Err(io::Error::other(
                "backup lifecycle origin inventory invalid",
            ));
        }
        if origin.kind == "backup_lifecycle" {
            check_backup_lifecycle_anchor(root, &origin)?;
            anchors.push(());
        }
        let binding = NormalJournalOrigin {
            publication: origin.publication,
            receipt_sha256: origin.receipt_sha256,
        };
        if !bindings.contains(&binding) {
            bindings.push(binding);
        }
    }
    if anchors.len() > 1 {
        return Err(io::Error::other("backup lifecycle anchor duplicated"));
    }
    let refs = bindings.iter().collect::<Vec<_>>();
    verify_backup_lifecycle_origins(root, &refs)
}
pub(super) fn anchor_backup_lifecycle(
    root: &File,
    publication: Option<&NormalJournalOrigin>,
) -> io::Result<()> {
    let Some(publication) = publication else {
        return Ok(());
    };
    let values = storage::origins(root)
        .map_err(|_| io::Error::other("backup lifecycle origins unavailable"))?;
    if values.iter().any(|v| v["kind"] == "backup_lifecycle") {
        return validate_backup_lifecycle_anchors(root);
    }
    verify_backup_lifecycle_origin(root, Some(publication))?;
    let entry = super::backup_lifecycle::namespace_anchor(root)?;
    let origin = Origin {
        schema: 1,
        publication: publication.publication.clone(),
        receipt_sha256: publication.receipt_sha256.clone(),
        kind: "backup_lifecycle".into(),
        id: super::backup_lifecycle::NAME.into(),
        sha256: storage::digest(&serde_json::to_vec(&entry).map_err(io::Error::other)?),
        entry: Some(entry),
    };
    storage::write_origin(
        root,
        &serde_json::to_value(origin).map_err(io::Error::other)?,
    )?;
    validate_backup_lifecycle_anchors(root)
}
