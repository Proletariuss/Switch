//! Compile-only, read-only sealed family adapter. Normal daemon routes do not
//! construct this owner. It cannot install seals, migrate, issue IDs or dispatch.
//! The real daemon lifetime lease enrolls only ONE adapter owner, whose clones
//! share serialized sticky health. No adapter drop/reopen clears that lease.
//! Root ownership excludes participating daemons; it does NOT revoke an old v1
//! grant already returned, prove physical drain, or adopt a surviving worker.
use super::*;
use crate::daemon_lifetime::RootProof;
use proletarius_store::device_sms_seal::{
    Error as SealError, FamilyView, GuardError, Identity, Kernel, View,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Debug)]
pub(super) enum Error {
    Owner(crate::daemon_lifetime::Error),
    Seal(SealError<CodecError>),
    Unavailable,
}
#[derive(Default)]
struct State {
    failed: bool,
    last: Option<FamilyView>,
}
struct Inner {
    root: PathBuf,
    proof: RootProof,
    identity: Identity,
    kernel: Kernel,
    state: Mutex<State>,
}
#[derive(Clone)]
pub(super) struct Adapter(Arc<Inner>);

/// Bounded historical obligations, never a completed-drain or dispatch permit.
/// Even zero unresolved records cannot certify an uncooperative process stopped.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct LegacyPins {
    pub(super) prepared: usize,
    pub(super) submitting: usize,
    pub(super) unknown: usize,
    pub(super) accepted_unarchived: usize,
    pub(super) accepted_archived: usize,
}
pub(super) struct Diagnosis {
    pub(super) seal: View,
    pub(super) family_step: family::RecoveryStep,
    pub(super) legacy_pins: LegacyPins,
}
impl Adapter {
    /// Compile-only enrollment on a caller-retained REAL lifetime lease. All
    /// failures consume enrollment. Caller must retain the lease process-wide;
    /// a later lease is another lifetime, never continuation of this adapter.
    pub(super) fn open(
        root: &Path,
        proof: RootProof,
        identity: Identity,
    ) -> std::result::Result<Self, Error> {
        proof.claim_device_sms_adapter(root).map_err(Error::Owner)?;
        let kernel = Kernel::existing(root).map_err(|e| Error::Seal(SealError::Storage(e)))?;
        let owner = Self(Arc::new(Inner {
            root: root.to_path_buf(),
            proof,
            identity,
            kernel,
            state: Mutex::new(State::default()),
        }));
        owner.inspect()?;
        Ok(owner)
    }
    pub(super) fn restart_required(&self) -> bool {
        self.0.state.lock().map_or(true, |mut state| {
            if self.0.proof.with_root(&self.0.root, |_| ()).is_err() {
                state.failed = true;
            }
            state.failed
        })
    }
    /// Serialize admission and error publication across clones. No disk/schema,
    /// provenance or ownership failure may be cleared by successful later bytes.
    pub(super) fn inspect(&self) -> std::result::Result<Diagnosis, Error> {
        let mut state = self.0.state.lock().map_err(|_| Error::Unavailable)?;
        if state.failed {
            return Err(Error::Unavailable);
        }
        let result = self
            .0
            .proof
            .with_root(&self.0.root, |_| {
                self.0
                    .kernel
                    .inspect_family(&self.0.identity, |active, siblings| {
                        diagnose(active, siblings, self.0.identity.authority_id())
                    })
            })
            .map_err(Error::Owner)
            .and_then(|v| v.map_err(Error::Seal));
        let result = result.and_then(|(observation, (family_step, legacy_pins))| {
            let seal = observation.seal();
            if state.last.as_ref().is_some_and(|last| {
                !observation.preserves_observed_files(last)
                    || seal.revision() < last.seal().revision()
                    || seal.issued_floor() < last.seal().issued_floor()
                    || (seal.revision() == last.seal().revision() && seal != last.seal())
            }) {
                return Err(Error::Seal(SealError::Guard(GuardError::Rollback)));
            }
            let seal = seal.clone();
            state.last = Some(observation);
            Ok(Diagnosis {
                seal,
                family_step,
                legacy_pins,
            })
        });
        if result.is_err() {
            state.failed = true;
        }
        result
    }
}
fn diagnose(
    active: &[u8],
    siblings: &[Option<Vec<u8>>],
    expected_authority: &str,
) -> Result<(family::RecoveryStep, LegacyPins)> {
    let inspected = family::inspect_bytes_for_authority(active, siblings, expected_authority)?;
    let legacy = if decode_snapshot_bytes(active).is_ok() {
        let bytes = siblings[0]
            .as_deref()
            .ok_or(CodecError::LegacyUnavailable)?;
        let snapshot = decode_snapshot_bytes(active)?;
        CompleteLegacyIndex::from_snapshot(bytes, &snapshot.legacy.sha256.0)?
    } else {
        CompleteLegacyIndex::from_snapshot(active, &digest(active).0)?
    };
    let mut pins = LegacyPins {
        prepared: 0,
        submitting: 0,
        unknown: 0,
        accepted_unarchived: 0,
        accepted_archived: 0,
    };
    for fence in legacy.fences.values() {
        match fence.original_state {
            LegacyPhase::Prepared => pins.prepared += 1,
            LegacyPhase::Submitting => pins.submitting += 1,
            LegacyPhase::Unknown => pins.unknown += 1,
            LegacyPhase::Accepted if fence.archive_confirmed => pins.accepted_archived += 1,
            LegacyPhase::Accepted => pins.accepted_unarchived += 1,
        }
    }
    Ok((inspected.next_step(), pins))
}

#[cfg(test)]
mod tests;
