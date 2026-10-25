//! Read-only whole-family migration prerequisite. No migration executor exists.
//!
//! The proposed ordered protocol is journal -> frozen raw v1 -> initial v2
//! prepared -> atomic replacement of the ORIGINAL active path. Each step needs
//! file + directory durability before the next, under the original writer lock.
//! Recovery derives progress from exact bytes, never from a claimed journal
//! phase. Frozen v1 and the journal remain required after replacement. This
//! module only inspects/proposes: executing a returned step after releasing the
//! lock is unsafe. A future reviewed executor must revalidate under that lock.
use super::*;
use proletarius_store::{ArchiveStore, raw::FamilyInspectionError};
use std::path::Path;

const ACTIVE: &str = "device-send-ledger.json";
const FROZEN: &str = "device-send-ledger.legacy-v1.json";
const PREPARED: &str = "device-send-ledger.v2-prepared.json";
const JOURNAL: &str = "device-send-ledger.v2-migration.json";
const SIBLINGS: [&str; 3] = [FROZEN, PREPARED, JOURNAL];

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MigrationJournal {
    schema_version: u8,
    family: Family,
    authority_id: AuthorityId,
    epoch: Decimal,
    legacy_sha256: SnapshotDigest,
    initial_v2_sha256: SnapshotDigest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecoveryStep {
    /// No migration journal exists. An authority candidate may be proposed.
    ProposeMigration,
    /// Journal exists; exact original v1 bytes must be frozen next.
    FreezeLegacy,
    /// Journal and exact frozen v1 exist; prepare the deterministic v2 next.
    PrepareV2,
    /// All preparation is exact. A future executor may replace active atomically.
    ReplaceActive,
    /// Active is v2 and the immutable legacy/journal witnesses validate.
    V2Present,
}

/// Inspected facts, NOT a lock token, migration commit or dispatch capability.
/// No constructor accepts caller-supplied snapshots, pages or legacy metadata.
pub(super) struct FamilyInspection {
    next_step: RecoveryStep,
    active_bytes: Vec<u8>,
}
impl FamilyInspection {
    pub(super) fn next_step(&self) -> RecoveryStep {
        self.next_step
    }

    /// Generate only an uncommitted deterministic proposal from a locked v1
    /// inspection. The caller must never persist this stale proposal without
    /// rereading the family under the same original transaction lock.
    pub(super) fn propose_migration(&self, authority_id: &str) -> Result<MigrationProposal> {
        if self.next_step != RecoveryStep::ProposeMigration || !canonical_hex(authority_id, 32) {
            return Err(CodecError::Conflict);
        }
        let legacy =
            CompleteLegacyIndex::from_snapshot(&self.active_bytes, &digest(&self.active_bytes).0)?;
        let authority_id = AuthorityId(authority_id.to_owned());
        let snapshot = initial_snapshot(&legacy, &authority_id)?;
        let prepared_bytes = encode_checked(&snapshot)?;
        let journal = MigrationJournal {
            schema_version: 1,
            family: Family::DeviceSms,
            authority_id,
            epoch: Decimal(1),
            legacy_sha256: legacy.digest,
            initial_v2_sha256: digest(&prepared_bytes),
        };
        let journal_bytes =
            serde_json::to_vec(&[journal]).map_err(|_| CodecError::InvalidSnapshot)?;
        Ok(MigrationProposal {
            frozen_bytes: self.active_bytes.clone(),
            prepared_bytes,
            journal_bytes,
        })
    }
}

/// Pure proposed bytes; no method writes files or yields an owner/claim handle.
pub(super) struct MigrationProposal {
    frozen_bytes: Vec<u8>,
    prepared_bytes: Vec<u8>,
    journal_bytes: Vec<u8>,
}

pub(super) fn inspect(
    root: &Path,
) -> std::result::Result<FamilyInspection, FamilyInspectionError<CodecError>> {
    ArchiveStore::with_limits(root.join(ACTIVE), SNAPSHOT_BYTES as u64, 1)
        .map_err(FamilyInspectionError::Storage)?
        .inspect_raw_family(&SIBLINGS, inspect_bytes)
}

/// Seal-aware inspection must bind pre-v2 journal/prepared authority too.
/// Plain family inspection has no expected seal and remains a pure planner.
/// A valid current-v1 body alone cannot prove that a staged authority belongs
/// to this already sealed generation. This method still writes nothing.
pub(super) fn inspect_bytes_for_authority(
    active: &[u8],
    siblings: &[Option<Vec<u8>>],
    expected_authority: &str,
) -> Result<FamilyInspection> {
    let inspected = inspect_bytes(active, siblings)?;
    if let Some(journal) = siblings[2].as_deref() {
        if read_journal(journal)?.authority_id.0 != expected_authority {
            return Err(CodecError::AuthorityMismatch);
        }
    }
    Ok(inspected)
}

fn initial_snapshot(
    legacy: &CompleteLegacyIndex,
    authority_id: &AuthorityId,
) -> Result<AuthoritySnapshot> {
    // A legacy v1 id may look like a v2 id. Never choose that authority and
    // accidentally strand a later candidate or reinterpret the old intent.
    if legacy.fences.keys().any(|id| {
        DeviceSmsIdentity::parse_wire(id).is_ok_and(|identity| &identity.authority == authority_id)
    }) {
        return Err(CodecError::LegacyCollision);
    }
    let snapshot = AuthoritySnapshot {
        schema_version: 2,
        family: Family::DeviceSms,
        authority_id: authority_id.clone(),
        epoch: Decimal(1),
        issued_high_water: Decimal(0),
        legacy_new_admission: false,
        legacy: LegacyReference {
            path: LegacyPath::FrozenV1,
            sha256: legacy.digest.clone(),
        },
        live_slots: vec![],
        terminal_cache: vec![],
        // Preserve ALL records, phases, original attempts and archive status.
        // No legacy phase is made claimable or retired by this proposal.
        legacy_overlay: legacy.fences.values().cloned().collect(),
    };
    encode_checked(&snapshot)?;
    Ok(snapshot)
}

fn read_journal(bytes: &[u8]) -> Result<MigrationJournal> {
    let value = strict_value(bytes, RESERVE_BYTES)?;
    let mut journals: Vec<MigrationJournal> =
        serde_json::from_value(value).map_err(|_| CodecError::InvalidSnapshot)?;
    if journals.len() != 1 {
        return Err(CodecError::InvalidSnapshot);
    }
    let journal = journals.remove(0);
    if journal.schema_version != 1 || journal.epoch.0 != 1 {
        return Err(CodecError::InvalidSnapshot);
    }
    Ok(journal)
}

pub(super) fn inspect_bytes(
    active: &[u8],
    siblings: &[Option<Vec<u8>>],
) -> Result<FamilyInspection> {
    let [frozen, prepared, journal] = siblings else {
        return Err(CodecError::InvalidSnapshot);
    };
    let Some(journal) = journal else {
        if frozen.is_some() || prepared.is_some() {
            return Err(CodecError::LegacyUnavailable);
        }
        // Existing v2 without migration witnesses must NOT be reconstructed as
        // a new empty authority. Present empty/null/[] and missing active also
        // fail at the raw storage boundary before this callback is entered.
        CompleteLegacyIndex::from_snapshot(active, &digest(active).0)?;
        return Ok(FamilyInspection {
            next_step: RecoveryStep::ProposeMigration,
            active_bytes: active.to_vec(),
        });
    };
    let journal = read_journal(journal)?;
    // Active is either exact original v1, or a valid committed v2. A v1 that
    // changed after staging cannot be silently overwritten with stale bytes.
    let active_is_original_v1 = digest(active) == journal.legacy_sha256;
    let legacy_bytes = if active_is_original_v1 {
        active
    } else {
        frozen.as_deref().ok_or(CodecError::LegacyUnavailable)?
    };
    let legacy = CompleteLegacyIndex::from_snapshot(legacy_bytes, &journal.legacy_sha256.0)?;
    let initial = initial_snapshot(&legacy, &journal.authority_id)?;
    let initial_bytes = encode_checked(&initial)?;
    if digest(&initial_bytes) != journal.initial_v2_sha256 {
        return Err(CodecError::InvalidSnapshot);
    }
    if let Some(bytes) = frozen {
        // Preserve exact raw v1 bytes, including unknown historical fields and
        // whitespace. Semantically equivalent rewritten JSON is not a witness.
        if bytes != legacy_bytes {
            return Err(CodecError::LegacyUnavailable);
        }
    }
    if let Some(bytes) = prepared {
        decode_snapshot_bytes(bytes)?;
        if bytes != &initial_bytes {
            return Err(CodecError::InvalidSnapshot);
        }
    }
    let next_step = if active_is_original_v1 {
        match (frozen.is_some(), prepared.is_some()) {
            (false, false) => RecoveryStep::FreezeLegacy,
            (true, false) => RecoveryStep::PrepareV2,
            (true, true) => RecoveryStep::ReplaceActive,
            // Ordered protocol never creates prepared before frozen.
            (false, true) => return Err(CodecError::LegacyUnavailable),
        }
    } else {
        let current = decode_snapshot_bytes(active)?;
        if current.authority_id != journal.authority_id
            || current.legacy.sha256 != legacy.digest
            || current.legacy_overlay != initial.legacy_overlay
        {
            return Err(CodecError::LegacyUnavailable);
        }
        // encode_checked already validates every sequence and slot fingerprint.
        // The chosen authority is disjoint from the full frozen v1 identity set.
        RecoveryStep::V2Present
    };
    Ok(FamilyInspection {
        next_step,
        active_bytes: active.to_vec(),
    })
}

#[cfg(test)]
mod tests;
