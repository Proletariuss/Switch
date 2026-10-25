//! Compile-only device-SMS dual witness fence. No issuer, migration executor,
//! active-file writer, claim or dispatch capability exists in this module.
//!
//! This kernel requires an existing active ledger, existing original named lock
//! and existing presence marker. It never creates an authority from absence.
//! Marker replacement becomes durable BEFORE the held lock inode is rewritten.
//! The current v1 decoder rejects the new magic in either original witness.
//! Mixed/partial/missing witnesses are fenced, never automatically repaired.
//! Only inspection can attest a coherent target; staging a target never writes
//! active bytes and does not mean the target has been committed or issued.
use crate::{ArchiveStore, PRESENCE_CONTRACT, StoreError, reject_non_regular};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

const ACTIVE: &str = "device-send-ledger.json";
const MARKER_TEMP: &str = ".device-send-ledger.json.generation-seal.tmp";
const MAGIC: &str = "switch-device-sms-generation-seal-v2";
const MAX_WITNESS: u64 = 512;
const MAX_ACTIVE: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identity {
    authority: String,
    generation: String,
}
impl Identity {
    /// Authority already checked by the expected dual-witness identity.
    pub fn authority_id(&self) -> &str {
        &self.authority
    }
    pub fn parse(authority: &str, generation: &str) -> Result<Self, GuardError> {
        if !hex(authority, 32) || !hex(generation, 32) {
            return Err(GuardError::Identity);
        }
        Ok(Self {
            authority: authority.into(),
            generation: generation.into(),
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuardError {
    Identity,
    Witness,
    Active,
    Changed,
    Conflict,
    Rollback,
    Exhausted,
}
#[derive(Debug)]
pub enum Error<S> {
    Storage(StoreError),
    Schema(S),
    Guard(GuardError),
    Injected(Point),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Point {
    MarkerCreated,
    MarkerPartial,
    MarkerWritten,
    MarkerSynced,
    MarkerRenamed,
    MarkerDirectorySynced,
    AnchorTruncated,
    AnchorPartial,
    AnchorWritten,
    AnchorSynced,
}
impl Point {
    pub const ALL: [Self; 10] = [
        Self::MarkerCreated,
        Self::MarkerPartial,
        Self::MarkerWritten,
        Self::MarkerSynced,
        Self::MarkerRenamed,
        Self::MarkerDirectorySynced,
        Self::AnchorTruncated,
        Self::AnchorPartial,
        Self::AnchorWritten,
        Self::AnchorSynced,
    ];
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    LegacyFenced,
    V2Target,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Seal {
    identity: Identity,
    legacy_digest: String,
    active_digest: String,
    revision: u64,
    floor: u64,
    kind: Kind,
}
/// Diagnostic receipt only. Not proof of issuance, claim or active commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct View(Seal);
impl View {
    pub fn revision(&self) -> u64 {
        self.0.revision
    }
    pub fn issued_floor(&self) -> u64 {
        self.0.floor
    }
    pub fn target_sha256(&self) -> &str {
        &self.0.active_digest
    }
}
/// Diagnostic observation of fixed sibling provenance. No writer capability.
#[derive(Clone, Debug)]
pub struct FamilyView {
    seal: View,
    stamps: Vec<Option<Stamp>>,
    lock_identity: (u64, u64),
}
impl FamilyView {
    pub fn seal(&self) -> &View {
        &self.seal
    }
    /// Already observed family members must remain the same immutable files.
    /// Previously missing stages may appear, but no observed file may vanish,
    /// change metadata, or be replaced even by identical bytes.
    pub fn preserves_observed_files(&self, prior: &Self) -> bool {
        self.lock_identity == prior.lock_identity
            && self.stamps.len() == prior.stamps.len()
            && self
                .stamps
                .iter()
                .zip(&prior.stamps)
                .all(|(now, before)| before.is_none() || now == before)
    }
}
fn hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
impl Seal {
    fn encode(&self) -> Vec<u8> {
        format!(
            "{MAGIC}\ndevice_sms\n{}\n{}\n1\n{}\n{}\n{:016x}\n{:016x}\n{}\n",
            self.identity.authority,
            self.identity.generation,
            self.legacy_digest,
            self.active_digest,
            self.revision,
            self.floor,
            match self.kind {
                Kind::LegacyFenced => "legacy_fenced",
                Kind::V2Target => "v2_target",
            }
        )
        .into_bytes()
    }
    fn decode(bytes: &[u8]) -> Result<Self, GuardError> {
        let text = std::str::from_utf8(bytes).map_err(|_| GuardError::Witness)?;
        let p: Vec<_> = text.split('\n').collect();
        if p.len() != 11
            || p[0] != MAGIC
            || p[1] != "device_sms"
            || p[4] != "1"
            || p[10] != ""
            || !hex(p[5], 64)
            || !hex(p[6], 64)
            || !hex(p[7], 16)
            || !hex(p[8], 16)
        {
            return Err(GuardError::Witness);
        }
        let seal = Self {
            identity: Identity::parse(p[2], p[3])?,
            legacy_digest: p[5].into(),
            active_digest: p[6].into(),
            revision: u64::from_str_radix(p[7], 16).map_err(|_| GuardError::Witness)?,
            floor: u64::from_str_radix(p[8], 16).map_err(|_| GuardError::Witness)?,
            kind: match p[9] {
                "legacy_fenced" => Kind::LegacyFenced,
                "v2_target" => Kind::V2Target,
                _ => return Err(GuardError::Witness),
            },
        };
        if (seal.kind == Kind::LegacyFenced
            && (seal.revision != 0 || seal.floor != 0 || seal.active_digest != seal.legacy_digest))
            || (seal.kind == Kind::V2Target && seal.revision == 0)
            || seal.encode() != bytes
        {
            return Err(GuardError::Witness);
        }
        Ok(seal)
    }
}
#[derive(Clone)]
pub struct Kernel {
    archive: ArchiveStore,
    #[cfg(any(test, feature = "raw-fault-tests"))]
    hook: Option<std::sync::Arc<dyn Fn(Point) -> bool + Send + Sync>>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Stamp(u64, u64, u64, i64, i64, i64, i64, u32);
fn stamp(path: &Path) -> Result<Stamp, StoreError> {
    reject_non_regular(path)?;
    let m = fs::symlink_metadata(path).map_err(StoreError::Io)?;
    Ok(Stamp(
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
        m.mode(),
    ))
}
fn optional_stamp(path: &Path) -> Result<Option<Stamp>, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => stamp(path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StoreError::Io(error)),
    }
}
fn read_witness(path: &Path) -> Result<Vec<u8>, StoreError> {
    reject_non_regular(path)?;
    let before = stamp(path)?;
    let file = File::open(path).map_err(StoreError::Io)?;
    let held = file.metadata().map_err(StoreError::Io)?;
    let named = fs::symlink_metadata(path).map_err(StoreError::Io)?;
    if held.dev() != named.dev() || held.ino() != named.ino() || held.len() > MAX_WITNESS {
        return Err(StoreError::Configuration("invalid_device_sms_seal".into()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_WITNESS + 1)
        .read_to_end(&mut bytes)
        .map_err(StoreError::Io)?;
    if bytes.len() as u64 > MAX_WITNESS || stamp(path)? != before {
        return Err(StoreError::Configuration("device_sms_seal_changed".into()));
    }
    Ok(bytes)
}
fn read_anchor(lock: &mut File) -> Result<Vec<u8>, StoreError> {
    if lock.metadata().map_err(StoreError::Io)?.len() > MAX_WITNESS {
        return Err(StoreError::Configuration(
            "invalid_device_sms_anchor".into(),
        ));
    }
    lock.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
    let mut bytes = Vec::new();
    lock.take(MAX_WITNESS + 1)
        .read_to_end(&mut bytes)
        .map_err(StoreError::Io)?;
    if bytes.len() as u64 > MAX_WITNESS {
        return Err(StoreError::Configuration(
            "invalid_device_sms_anchor".into(),
        ));
    }
    Ok(bytes)
}
struct Current {
    bytes: Vec<u8>,
    marker: Vec<u8>,
    anchor: Vec<u8>,
    active_stamp: Stamp,
    marker_stamp: Stamp,
}
impl Kernel {
    pub fn existing(root: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            archive: ArchiveStore::with_limits(root.join(ACTIVE), MAX_ACTIVE, 1)?,
            #[cfg(any(test, feature = "raw-fault-tests"))]
            hook: None,
        })
    }
    /// Explicit fault builds only; hook is absent without test/raw-fault-tests.
    #[cfg(any(test, feature = "raw-fault-tests"))]
    pub fn with_hook_for_test(
        mut self,
        hook: std::sync::Arc<dyn Fn(Point) -> bool + Send + Sync>,
    ) -> Self {
        self.hook = Some(hook);
        self
    }
    fn point<S>(&self, p: Point) -> Result<(), Error<S>> {
        #[cfg(any(test, feature = "raw-fault-tests"))]
        if self.hook.as_ref().is_some_and(|hook| hook(p)) {
            return Err(Error::Injected(p));
        }
        let _ = p;
        Ok(())
    }
    fn lock<S>(&self) -> Result<File, Error<S>> {
        let path = self.archive.lock_path().map_err(Error::Storage)?;
        reject_non_regular(&path).map_err(Error::Storage)?;
        // No create, no fallback to a fresh inode when an original anchor is missing.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| Error::Storage(StoreError::Io(e)))?;
        lock.lock().map_err(|e| Error::Storage(StoreError::Io(e)))?;
        self.archive
            .validate_named_lock(&lock)
            .map_err(Error::Storage)?;
        Ok(lock)
    }
    fn current<S>(&self, lock: &mut File) -> Result<Current, Error<S>> {
        let bytes = self
            .archive
            .read_bytes_unlocked()
            .map_err(Error::Storage)?
            .ok_or(Error::Guard(GuardError::Active))?;
        self.archive
            .validate_raw_bytes(&bytes)
            .map_err(Error::Storage)?;
        let marker_path = self.archive.presence_path().map_err(Error::Storage)?;
        let marker = read_witness(&marker_path).map_err(Error::Storage)?;
        let anchor = read_anchor(lock).map_err(Error::Storage)?;
        Ok(Current {
            bytes,
            marker,
            anchor,
            active_stamp: stamp(&self.archive.path).map_err(Error::Storage)?,
            marker_stamp: stamp(&marker_path).map_err(Error::Storage)?,
        })
    }
    fn recheck<S>(&self, lock: &mut File, current: &Current) -> Result<(), Error<S>> {
        self.archive
            .validate_named_lock(lock)
            .map_err(Error::Storage)?;
        if stamp(&self.archive.path).map_err(Error::Storage)? != current.active_stamp
            || self
                .archive
                .read_bytes_unlocked()
                .map_err(Error::Storage)?
                .as_deref()
                != Some(&current.bytes)
            || stamp(&self.archive.presence_path().map_err(Error::Storage)?)
                .map_err(Error::Storage)?
                != current.marker_stamp
            || read_witness(&self.archive.presence_path().map_err(Error::Storage)?)
                .map_err(Error::Storage)?
                != current.marker
            || read_anchor(lock).map_err(Error::Storage)? != current.anchor
        {
            return Err(Error::Guard(GuardError::Changed));
        }
        Ok(())
    }
    /// Installs a fence on an existing validated v1 ledger. `validate` must be
    /// a synchronous pure FULL device-SMS schema decoder; it runs under the
    /// original named lock before metadata writes. No old claim is revoked.
    pub fn install<S>(
        &self,
        identity: &Identity,
        validate: impl Fn(&[u8]) -> Result<(), S>,
    ) -> Result<View, Error<S>> {
        let mut lock = self.lock()?;
        let current = self.current(&mut lock)?;
        if current.marker != PRESENCE_CONTRACT || current.anchor != PRESENCE_CONTRACT {
            let seal = coherent(&current, identity).map_err(Error::Guard)?;
            if seal.kind != Kind::LegacyFenced {
                return Err(Error::Guard(GuardError::Conflict));
            }
            validate(&current.bytes).map_err(Error::Schema)?;
            self.recheck(&mut lock, &current)?;
            // A previous complete anchor write may have lost its fsync/ack.
            // Reconfirm durability without rewriting or repairing witness bytes.
            let marker = self.archive.presence_path().map_err(Error::Storage)?;
            File::open(marker)
                .and_then(|file| file.sync_all())
                .map_err(|error| Error::Storage(StoreError::Io(error)))?;
            lock.sync_all()
                .map_err(|error| Error::Storage(StoreError::Io(error)))?;
            File::open(self.archive.parent().map_err(Error::Storage)?)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| Error::Storage(StoreError::Io(error)))?;
            self.recheck(&mut lock, &current)?;
            return Ok(View(seal));
        }
        legacy(&current.bytes).map_err(Error::Guard)?;
        validate(&current.bytes).map_err(Error::Schema)?;
        self.recheck(&mut lock, &current)?;
        let hash = digest(&current.bytes);
        let seal = Seal {
            identity: identity.clone(),
            legacy_digest: hash.clone(),
            active_digest: hash,
            revision: 0,
            floor: 0,
            kind: Kind::LegacyFenced,
        };
        self.persist(&mut lock, &current, &seal)?;
        Ok(View(seal))
    }
    /// Validates existing dual witnesses AND the bound active bytes. Returns
    /// diagnostic identity/floor only; it neither issues IDs nor authorizes IO.
    /// Unknown, mixed, interrupted and rollback states are never auto-repaired.
    pub fn inspect<S>(
        &self,
        identity: &Identity,
        validate: impl Fn(&[u8]) -> Result<(), S>,
    ) -> Result<View, Error<S>> {
        let mut lock = self.lock()?;
        let current = self.current(&mut lock)?;
        let seal = coherent(&current, identity).map_err(Error::Guard)?;
        validate(&current.bytes).map_err(Error::Schema)?;
        self.recheck(&mut lock, &current)?;
        Ok(View(seal))
    }
    /// Inspect the fixed device-SMS migration family under the ORIGINAL held
    /// ledger lock and coherent generation witnesses. No presence enrollment,
    /// repair, sibling lock, write, issuance or transferable lock token.
    /// Present siblings are bounded strict raw JSON; exact bytes and inode stamps
    /// are checked again after the pure synchronous schema callback.
    pub fn inspect_family<T, S>(
        &self,
        identity: &Identity,
        inspect: impl FnOnce(&[u8], &[Option<Vec<u8>>]) -> Result<T, S>,
    ) -> Result<(FamilyView, T), Error<S>> {
        let mut lock = self.lock()?;
        let current = self.current(&mut lock)?;
        let seal = coherent(&current, identity).map_err(Error::Guard)?;
        let parent = self.archive.parent().map_err(Error::Storage)?;
        let mut stores = Vec::new();
        let mut stamps = Vec::new();
        let mut family = Vec::new();
        for name in [
            "device-send-ledger.legacy-v1.json",
            "device-send-ledger.v2-prepared.json",
            "device-send-ledger.v2-migration.json",
        ] {
            let store = ArchiveStore::with_limits(parent.join(name), MAX_ACTIVE, 1)
                .map_err(Error::Storage)?;
            let before = optional_stamp(&store.path).map_err(Error::Storage)?;
            let bytes = store.read_bytes_unlocked().map_err(Error::Storage)?;
            if let Some(ref bytes) = bytes {
                store.validate_raw_bytes(bytes).map_err(Error::Storage)?;
            }
            if optional_stamp(&store.path).map_err(Error::Storage)? != before {
                return Err(Error::Guard(GuardError::Changed));
            }
            stores.push(store);
            stamps.push(before);
            family.push(bytes);
        }
        let result = inspect(&current.bytes, &family).map_err(Error::Schema)?;
        self.recheck(&mut lock, &current)?;
        for ((store, before), bytes) in stores.iter().zip(&stamps).zip(&family) {
            if optional_stamp(&store.path).map_err(Error::Storage)? != *before
                || store.read_bytes_unlocked().map_err(Error::Storage)? != *bytes
            {
                return Err(Error::Guard(GuardError::Changed));
            }
        }
        self.recheck(&mut lock, &current)?;
        Ok((
            FamilyView {
                seal: View(seal),
                stamps,
                lock_identity: {
                    let metadata = lock
                        .metadata()
                        .map_err(|e| Error::Storage(StoreError::Io(e)))?;
                    (metadata.dev(), metadata.ino())
                },
            },
            result,
        ))
    }
    /// Advances an independent write-ahead floor/target seal. The proposed v2
    /// bytes are validated but NEVER written to active by this kernel. The
    /// returned view is NOT an active commit, issuance or dispatch receipt.
    /// Until some future separately reviewed executor commits those exact bytes,
    /// inspect rejects the older active file. No recovery executor is supplied.
    pub fn stage_v2_target<S>(
        &self,
        expected: &View,
        target: &[u8],
        validate: impl Fn(&[u8]) -> Result<(), S>,
    ) -> Result<View, Error<S>> {
        let mut lock = self.lock()?;
        let current = self.current(&mut lock)?;
        let seal = coherent(&current, &expected.0.identity).map_err(Error::Guard)?;
        if seal != expected.0 {
            return Err(Error::Guard(GuardError::Conflict));
        }
        validate(&current.bytes).map_err(Error::Schema)?;
        self.archive
            .validate_raw_bytes(target)
            .map_err(Error::Storage)?;
        let floor = v2_metadata(target, &seal).map_err(Error::Guard)?;
        if floor < seal.floor {
            return Err(Error::Guard(GuardError::Rollback));
        }
        validate(target).map_err(Error::Schema)?;
        self.recheck(&mut lock, &current)?;
        let next = Seal {
            identity: seal.identity,
            legacy_digest: seal.legacy_digest,
            active_digest: digest(target),
            revision: seal
                .revision
                .checked_add(1)
                .ok_or(Error::Guard(GuardError::Exhausted))?,
            floor,
            kind: Kind::V2Target,
        };
        self.persist(&mut lock, &current, &next)?;
        Ok(View(next))
    }
    fn persist<S>(&self, lock: &mut File, current: &Current, next: &Seal) -> Result<(), Error<S>> {
        let marker = self.archive.presence_path().map_err(Error::Storage)?;
        let parent = self.archive.parent().map_err(Error::Storage)?;
        let temp = parent.join(MARKER_TEMP);
        let bytes = next.encode();
        let half = bytes.len() / 2;
        let io = |e| Error::Storage(StoreError::Io(e));
        self.recheck(lock, current)?;
        // Fixed bounded staging name; an orphan is a refusal, never overwritten.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(io)?;
        self.point(Point::MarkerCreated)?;
        file.write_all(&bytes[..half]).map_err(io)?;
        self.point(Point::MarkerPartial)?;
        file.write_all(&bytes[half..]).map_err(io)?;
        self.point(Point::MarkerWritten)?;
        file.sync_all().map_err(io)?;
        let temporary_stamp = stamp(&temp).map_err(Error::Storage)?;
        self.point(Point::MarkerSynced)?;
        self.recheck(lock, current)?;
        if stamp(&temp).map_err(Error::Storage)? != temporary_stamp {
            return Err(Error::Guard(GuardError::Changed));
        }
        fs::rename(&temp, &marker).map_err(io)?;
        let committed_marker_stamp = stamp(&marker).map_err(Error::Storage)?;
        self.point(Point::MarkerRenamed)?;
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(io)?;
        self.point(Point::MarkerDirectorySynced)?;
        self.archive
            .validate_named_lock(lock)
            .map_err(Error::Storage)?;
        if self
            .archive
            .read_bytes_unlocked()
            .map_err(Error::Storage)?
            .as_deref()
            != Some(&current.bytes)
            || stamp(&self.archive.path).map_err(Error::Storage)? != current.active_stamp
            || stamp(&marker).map_err(Error::Storage)? != committed_marker_stamp
            || read_witness(&marker).map_err(Error::Storage)? != bytes
        {
            return Err(Error::Guard(GuardError::Changed));
        }
        // Keep the ORIGINAL held lock inode; queued old openers observe this seal.
        lock.set_len(0).map_err(io)?;
        self.point(Point::AnchorTruncated)?;
        lock.seek(SeekFrom::Start(0)).map_err(io)?;
        lock.write_all(&bytes[..half]).map_err(io)?;
        self.point(Point::AnchorPartial)?;
        lock.write_all(&bytes[half..]).map_err(io)?;
        self.point(Point::AnchorWritten)?;
        lock.sync_all().map_err(io)?;
        self.point(Point::AnchorSynced)?;
        self.archive
            .validate_named_lock(lock)
            .map_err(Error::Storage)?;
        if read_anchor(lock).map_err(Error::Storage)? != bytes
            || stamp(&marker).map_err(Error::Storage)? != committed_marker_stamp
            || read_witness(&marker).map_err(Error::Storage)? != bytes
            || self
                .archive
                .read_bytes_unlocked()
                .map_err(Error::Storage)?
                .as_deref()
                != Some(&current.bytes)
            || stamp(&self.archive.path).map_err(Error::Storage)? != current.active_stamp
        {
            return Err(Error::Guard(GuardError::Changed));
        }
        Ok(())
    }
}
fn legacy(bytes: &[u8]) -> Result<(), GuardError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| GuardError::Active)?;
    if value.as_array().is_none_or(|a| a.len() != 1)
        || value[0]["schema_version"] != 1
        || !value[0]["records"].is_object()
    {
        return Err(GuardError::Active);
    }
    Ok(())
}
fn v2_metadata(bytes: &[u8], seal: &Seal) -> Result<u64, GuardError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| GuardError::Active)?;
    let row = &value[0];
    let floor = row["issued_high_water"]
        .as_str()
        .ok_or(GuardError::Active)?;
    if value.as_array().is_none_or(|a| a.len() != 1)
        || row["schema_version"] != 2
        || row["family"] != "device_sms"
        || row["authority_id"] != seal.identity.authority
        || row["epoch"] != "1"
        || row["legacy"]["path"] != "device-send-ledger.legacy-v1.json"
        || row["legacy"]["sha256"] != seal.legacy_digest
        || floor.is_empty()
        || (floor.len() > 1 && floor.starts_with('0'))
        || !floor.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(GuardError::Identity);
    }
    floor.parse().map_err(|_| GuardError::Exhausted)
}
fn coherent(current: &Current, identity: &Identity) -> Result<Seal, GuardError> {
    if current.marker != current.anchor {
        return Err(GuardError::Witness);
    }
    let seal = Seal::decode(&current.marker)?;
    if &seal.identity != identity {
        return Err(GuardError::Identity);
    }
    if digest(&current.bytes) != seal.active_digest {
        return Err(GuardError::Active);
    }
    match seal.kind {
        Kind::LegacyFenced => legacy(&current.bytes)?,
        Kind::V2Target => {
            if v2_metadata(&current.bytes, &seal)? != seal.floor {
                return Err(GuardError::Rollback);
            }
        }
    }
    Ok(seal)
}

#[cfg(test)]
mod tests;
