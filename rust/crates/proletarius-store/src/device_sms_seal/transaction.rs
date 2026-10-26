//! Original-lock migration and write-ahead sealed commits.
//! Only startup may recover a pending transaction. Live transactions never
//! clear an uncertain write; their daemon owner must latch unhealthy.
use super::*;
use serde::{Deserialize, Serialize};

const MEMBERS: [&str; 3] = [
    "device-send-ledger.legacy-v1.json",
    "device-send-ledger.v2-prepared.json",
    "device-send-ledger.v2-migration.json",
];
const WAL: &str = "device-send-ledger.v2-transaction.json";
const WAL_LIMIT: u64 = MAX_ACTIVE * 2 + 4096;

pub enum MigrationStep {
    Complete,
    WriteSibling { index: usize, bytes: Vec<u8> },
    ReplaceActive(Vec<u8>),
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema_version: u8,
    prior: String,
    next: String,
    target: String,
}

struct FamilyBytes {
    stores: Vec<ArchiveStore>,
    stamps: Vec<Option<Stamp>>,
    bytes: Vec<Option<Vec<u8>>>,
}
impl Kernel {
    fn family_bytes<S>(&self) -> Result<FamilyBytes, Error<S>> {
        let parent = self.archive.parent().map_err(Error::Storage)?;
        let mut family = FamilyBytes {
            stores: vec![],
            stamps: vec![],
            bytes: vec![],
        };
        for name in MEMBERS {
            let store = ArchiveStore::with_limits(parent.join(name), MAX_ACTIVE, 1)
                .map_err(Error::Storage)?;
            let before = optional_stamp(&store.path).map_err(Error::Storage)?;
            let bytes = store.read_bytes_unlocked().map_err(Error::Storage)?;
            if let Some(bytes) = &bytes {
                store.validate_raw_bytes(bytes).map_err(Error::Storage)?;
            }
            if optional_stamp(&store.path).map_err(Error::Storage)? != before {
                return Err(Error::Guard(GuardError::Changed));
            }
            family.stores.push(store);
            family.stamps.push(before);
            family.bytes.push(bytes);
        }
        Ok(family)
    }
    fn check_family<S>(&self, family: &FamilyBytes) -> Result<(), Error<S>> {
        for ((store, before), bytes) in family.stores.iter().zip(&family.stamps).zip(&family.bytes)
        {
            if optional_stamp(&store.path).map_err(Error::Storage)? != *before
                || store.read_bytes_unlocked().map_err(Error::Storage)? != *bytes
            {
                return Err(Error::Guard(GuardError::Changed));
            }
        }
        Ok(())
    }
    fn wal<S>(&self) -> Result<ArchiveStore, Error<S>> {
        ArchiveStore::with_limits(
            self.archive.parent().map_err(Error::Storage)?.join(WAL),
            WAL_LIMIT,
            1,
        )
        .map_err(Error::Storage)
    }
    fn sync_parent<S>(&self) -> Result<(), Error<S>> {
        File::open(self.archive.parent().map_err(Error::Storage)?)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| Error::Storage(StoreError::Io(e)))
    }
    fn no_pending<S>(&self) -> Result<(), Error<S>> {
        if optional_stamp(&self.wal()?.path)
            .map_err(Error::Storage)?
            .is_some()
            || optional_stamp(
                &self
                    .archive
                    .parent()
                    .map_err(Error::Storage)?
                    .join(MARKER_TEMP),
            )
            .map_err(Error::Storage)?
            .is_some()
        {
            return Err(Error::Guard(GuardError::Conflict));
        }
        Ok(())
    }
    fn next_seal<S>(&self, seal: &Seal, target: &[u8]) -> Result<Seal, Error<S>> {
        self.archive
            .validate_raw_bytes(target)
            .map_err(Error::Storage)?;
        let floor = v2_metadata(target, seal).map_err(Error::Guard)?;
        if floor < seal.floor {
            return Err(Error::Guard(GuardError::Rollback));
        }
        Ok(Seal {
            identity: seal.identity.clone(),
            legacy_digest: seal.legacy_digest.clone(),
            active_digest: digest(target),
            revision: seal
                .revision
                .checked_add(1)
                .ok_or(Error::Guard(GuardError::Exhausted))?,
            floor,
            kind: Kind::V2Target,
        })
    }
    fn commit_target<S>(
        &self,
        lock: &mut File,
        current: &Current,
        next: &Seal,
        target: &[u8],
    ) -> Result<(), Error<S>> {
        self.no_pending()?;
        self.recheck(lock, current)?;
        let pending = Pending {
            schema_version: 1,
            prior: String::from_utf8(current.marker.clone())
                .map_err(|_| Error::Guard(GuardError::Witness))?,
            next: String::from_utf8(next.encode())
                .map_err(|_| Error::Guard(GuardError::Witness))?,
            target: String::from_utf8(target.to_vec())
                .map_err(|_| Error::Guard(GuardError::Active))?,
        };
        let bytes =
            serde_json::to_vec(&[pending]).map_err(|e| Error::Storage(StoreError::Json(e)))?;
        let wal = self.wal()?;
        wal.replace_raw_locked(&bytes)
            .map_err(|e| Error::Storage(e.source))?;
        let wal_stamp = stamp(&wal.path).map_err(Error::Storage)?;
        self.point(Point::WalCommitted)?;
        self.persist(lock, current, next)?;
        if current.bytes != target {
            self.archive
                .replace_raw_locked(target)
                .map_err(|e| Error::Storage(e.source))?;
        } else {
            File::open(&self.archive.path)
                .and_then(|file| file.sync_all())
                .map_err(|e| Error::Storage(StoreError::Io(e)))?;
            self.sync_parent()?;
        }
        self.point(Point::ActiveCommitted)?;
        let committed = self.current(lock)?;
        if coherent(&committed, &next.identity).map_err(Error::Guard)? != *next {
            return Err(Error::Guard(GuardError::Changed));
        }
        self.finish_wal(lock, &wal, &bytes, &wal_stamp)
    }
    fn finish_wal<S>(
        &self,
        lock: &File,
        wal: &ArchiveStore,
        expected: &[u8],
        before: &Stamp,
    ) -> Result<(), Error<S>> {
        self.archive
            .validate_named_lock(lock)
            .map_err(Error::Storage)?;
        if stamp(&wal.path).map_err(Error::Storage)? != *before
            || wal
                .read_bytes_unlocked()
                .map_err(Error::Storage)?
                .as_deref()
                != Some(expected)
        {
            return Err(Error::Guard(GuardError::Changed));
        }
        fs::remove_file(&wal.path).map_err(|e| Error::Storage(StoreError::Io(e)))?;
        self.point(Point::WalRemoved)?;
        self.sync_parent()?;
        self.point(Point::WalDirectorySynced)
    }
    /// Resume only a self-consistent exact WAL. This is never called by live
    /// transactions. The new lifetime's root owner validates schema/provenance
    /// before any replay; unsupported, forged, missing or partial WALs refuse.
    fn recover_pending<S>(
        &self,
        lock: &mut File,
        validate: &impl Fn(&[u8], &[Option<Vec<u8>>], &Identity) -> Result<(), S>,
    ) -> Result<(), Error<S>> {
        let wal = self.wal()?;
        let Some(bytes) = wal.read_bytes_unlocked().map_err(Error::Storage)? else {
            return self.no_pending();
        };
        wal.validate_raw_bytes(&bytes).map_err(Error::Storage)?;
        let wal_stamp = stamp(&wal.path).map_err(Error::Storage)?;
        let mut values: Vec<Pending> =
            serde_json::from_slice(&bytes).map_err(|e| Error::Storage(StoreError::Json(e)))?;
        if values.len() != 1 {
            return Err(Error::Guard(GuardError::Witness));
        }
        let pending = values.remove(0);
        if pending.schema_version != 1 {
            return Err(Error::Guard(GuardError::Witness));
        }
        let next = Seal::decode(pending.next.as_bytes()).map_err(Error::Guard)?;
        let target = pending.target.as_bytes();
        self.archive
            .validate_raw_bytes(target)
            .map_err(Error::Storage)?;
        if digest(target) != next.active_digest {
            return Err(Error::Guard(GuardError::Active));
        }
        let prior_hash = if pending.prior.as_bytes() == PRESENCE_CONTRACT {
            if next.kind != Kind::LegacyFenced {
                return Err(Error::Guard(GuardError::Witness));
            }
            legacy(target).map_err(Error::Guard)?;
            next.legacy_digest.clone()
        } else {
            let prior = Seal::decode(pending.prior.as_bytes()).map_err(Error::Guard)?;
            if self.next_seal::<S>(&prior, target)? != next {
                return Err(Error::Guard(GuardError::Rollback));
            }
            prior.active_digest
        };
        let current = self.current(lock)?;
        let active_hash = digest(&current.bytes);
        if active_hash != prior_hash && active_hash != next.active_digest {
            return Err(Error::Guard(GuardError::Active));
        }
        // Marker replacement is atomic and precedes any anchor rewrite.
        let marker_old = current.marker == pending.prior.as_bytes();
        let marker_next = current.marker == pending.next.as_bytes();
        let anchor_old = current.anchor == pending.prior.as_bytes();
        let anchor_next_prefix = pending.next.as_bytes().starts_with(&current.anchor);
        if !(marker_old && anchor_old || marker_next && (anchor_old || anchor_next_prefix)) {
            return Err(Error::Guard(GuardError::Witness));
        }
        if active_hash == next.active_digest
            && active_hash != prior_hash
            && !(marker_next && current.anchor == pending.next.as_bytes())
        {
            return Err(Error::Guard(GuardError::Witness));
        }
        let family = self.family_bytes()?;
        validate(&current.bytes, &family.bytes, &next.identity).map_err(Error::Schema)?;
        validate(target, &family.bytes, &next.identity).map_err(Error::Schema)?;
        self.check_family(&family)?;
        self.recheck(lock, &current)?;
        if stamp(&wal.path).map_err(Error::Storage)? != wal_stamp {
            return Err(Error::Guard(GuardError::Changed));
        }
        let temp = self
            .archive
            .parent()
            .map_err(Error::Storage)?
            .join(MARKER_TEMP);
        if optional_stamp(&temp).map_err(Error::Storage)?.is_some() {
            let partial = read_witness(&temp).map_err(Error::Storage)?;
            if !pending.next.as_bytes().starts_with(&partial) {
                return Err(Error::Guard(GuardError::Witness));
            }
            // Only an exact interrupted prefix proven by the durable WAL may
            // be discarded. Unbound orphan staging names always fail closed.
            fs::remove_file(&temp).map_err(|e| Error::Storage(StoreError::Io(e)))?;
            self.sync_parent()?;
        }
        self.persist(lock, &current, &next)?;
        self.archive
            .replace_raw_locked(target)
            .map_err(|e| Error::Storage(e.source))?;
        self.point(Point::ActiveCommitted)?;
        let committed = self.current(lock)?;
        if coherent(&committed, &next.identity).map_err(Error::Guard)? != next {
            return Err(Error::Guard(GuardError::Changed));
        }
        self.check_family(&family)?;
        self.finish_wal(lock, &wal, &bytes, &wal_stamp)
    }
    /// Enroll a historical v1 archive only after its complete family schema
    /// succeeds under the original writer lock. Never recreate a missing lock
    /// when a presence witness exists, or repair a partially sealed generation.
    pub fn bootstrap_legacy<S>(
        &self,
        validate: impl FnOnce(&[u8], &[Option<Vec<u8>>]) -> Result<(), S>,
    ) -> Result<(), Error<S>> {
        use crate::raw::FamilyInspectionError;
        // A pending sealed transaction is exclusively the recovery executor's
        // responsibility, including its old-v1 prefix.
        if optional_stamp(&self.wal()?.path)
            .map_err(Error::Storage)?
            .is_some()
        {
            return Ok(());
        }
        let marker = self.archive.presence_path().map_err(Error::Storage)?;
        let marker_stamp = optional_stamp(&marker).map_err(Error::Storage)?;
        let anchor = self.archive.lock_path().map_err(Error::Storage)?;
        let anchor_stamp = optional_stamp(&anchor).map_err(Error::Storage)?;
        if marker_stamp.is_some() {
            if anchor_stamp.is_none() {
                return Err(Error::Guard(GuardError::Witness));
            }
            if read_witness(&marker).map_err(Error::Storage)? != PRESENCE_CONTRACT {
                return Ok(()); // sealed or corrupt: strict kernel recovery decides
            }
        } else if anchor_stamp.as_ref().is_some_and(|stamp| stamp.2 != 0) {
            return Err(Error::Guard(GuardError::Witness));
        }
        self.no_pending()?;
        self.archive
            .inspect_raw_family(&MEMBERS, validate)
            .map_err(|error| match error {
                FamilyInspectionError::Storage(error) => Error::Storage(error),
                FamilyInspectionError::Schema(error) => Error::Schema(error),
            })
    }

    /// Startup-only executor. The daemon must hold its real lifetime lease and
    /// an unpublished/drained send owner before entering. Every family step,
    /// including recovery, runs under the same original named ledger lock.
    pub fn open_or_migrate<S>(
        &self,
        candidate: impl FnOnce(&[u8]) -> Result<Identity, S>,
        validate: impl Fn(&[u8], &[Option<Vec<u8>>], &Identity) -> Result<(), S>,
        plan: impl Fn(&[u8], &[Option<Vec<u8>>], &Identity) -> Result<MigrationStep, S>,
    ) -> Result<Identity, Error<S>> {
        let mut lock = self.lock()?;
        self.recover_pending(&mut lock, &validate)?;
        let current = self.current(&mut lock)?;
        if current.marker == PRESENCE_CONTRACT && current.anchor == PRESENCE_CONTRACT {
            let family = self.family_bytes()?;
            // No sidecar may silently select or replace a virgin authority.
            if family.bytes.iter().any(Option::is_some) {
                return Err(Error::Guard(GuardError::Conflict));
            }
            legacy(&current.bytes).map_err(Error::Guard)?;
            let candidate = candidate(&current.bytes).map_err(Error::Schema)?;
            validate(&current.bytes, &family.bytes, &candidate).map_err(Error::Schema)?;
            // Reject a candidate authority colliding with any frozen legacy ID
            // before installing even the first v2 witness.
            let _ = plan(&current.bytes, &family.bytes, &candidate).map_err(Error::Schema)?;
            self.check_family(&family)?;
            let hash = digest(&current.bytes);
            let next = Seal {
                identity: candidate.clone(),
                legacy_digest: hash.clone(),
                active_digest: hash,
                revision: 0,
                floor: 0,
                kind: Kind::LegacyFenced,
            };
            self.commit_target(&mut lock, &current, &next, &current.bytes)?;
        }
        for _ in 0..5 {
            let current = self.current(&mut lock)?;
            let identity = Seal::decode(&current.marker)
                .map_err(Error::Guard)?
                .identity;
            coherent(&current, &identity).map_err(Error::Guard)?;
            let family = self.family_bytes()?;
            validate(&current.bytes, &family.bytes, &identity).map_err(Error::Schema)?;
            let step = plan(&current.bytes, &family.bytes, &identity).map_err(Error::Schema)?;
            self.check_family(&family)?;
            self.recheck(&mut lock, &current)?;
            match step {
                MigrationStep::Complete => {
                    if Seal::decode(&current.marker).map_err(Error::Guard)?.kind != Kind::V2Target {
                        return Err(Error::Guard(GuardError::Active));
                    }
                    return Ok(identity);
                }
                MigrationStep::WriteSibling { index, bytes } => {
                    let store = family
                        .stores
                        .get(index)
                        .ok_or(Error::Guard(GuardError::Conflict))?;
                    if family.bytes[index].is_some() {
                        return Err(Error::Guard(GuardError::Conflict));
                    }
                    let mut proposed = family.bytes.clone();
                    proposed[index] = Some(bytes.clone());
                    validate(&current.bytes, &proposed, &identity).map_err(Error::Schema)?;
                    store
                        .replace_raw_locked(&bytes)
                        .map_err(|e| Error::Storage(e.source))?;
                    self.point(match index {
                        0 => Point::FrozenCommitted,
                        1 => Point::PreparedCommitted,
                        _ => Point::JournalCommitted,
                    })?;
                }
                MigrationStep::ReplaceActive(bytes) => {
                    validate(&bytes, &family.bytes, &identity).map_err(Error::Schema)?;
                    let seal = coherent(&current, &identity).map_err(Error::Guard)?;
                    let next = self.next_seal(&seal, &bytes)?;
                    self.commit_target(&mut lock, &current, &next, &bytes)?;
                }
            }
        }
        Err(Error::Guard(GuardError::Conflict))
    }
    /// Live transactions refuse any pending recovery. Schema and proposals are
    /// validated under the original lock before WAL/dual-witness/active commit.
    pub fn transact_family<T, S>(
        &self,
        identity: &Identity,
        expected: &View,
        validate: impl Fn(&[u8], &[Option<Vec<u8>>], &Identity) -> Result<(), S>,
        plan: impl FnOnce(&[u8], &[Option<Vec<u8>>]) -> Result<(Option<Vec<u8>>, T), S>,
    ) -> Result<T, Error<S>> {
        let mut lock = self.lock()?;
        self.no_pending()?;
        let current = self.current(&mut lock)?;
        let seal = coherent(&current, identity).map_err(Error::Guard)?;
        if seal != expected.0 {
            return Err(Error::Guard(GuardError::Rollback));
        }
        if seal.kind != Kind::V2Target {
            return Err(Error::Guard(GuardError::Active));
        }
        let family = self.family_bytes()?;
        validate(&current.bytes, &family.bytes, identity).map_err(Error::Schema)?;
        let (next, result) = plan(&current.bytes, &family.bytes).map_err(Error::Rejected)?;
        if let Some(bytes) = next {
            validate(&bytes, &family.bytes, identity).map_err(Error::Schema)?;
            let next = self.next_seal(&seal, &bytes)?;
            self.check_family(&family)?;
            self.recheck(&mut lock, &current)?;
            self.commit_target(&mut lock, &current, &next, &bytes)?;
        } else {
            self.check_family(&family)?;
            self.recheck(&mut lock, &current)?;
        }
        Ok(result)
    }
}
