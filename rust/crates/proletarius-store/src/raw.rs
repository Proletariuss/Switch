//! Opt-in original-byte transactions. This is a storage foundation, not an
//! authority initializer, claim grant, migration proof, or health latch.
use crate::{ArchiveStore, StoreError};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::os::unix::fs::MetadataExt;
use std::{collections::HashSet, fmt};

/// A result can be returned without replacement, preserving exact current bytes.
pub enum Mutation<T> {
    Unchanged(T),
    Replace { bytes: Vec<u8>, result: T },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitStage {
    /// Replacement has not happened; existing archive bytes remain unchanged.
    BeforeRename,
    /// Rename succeeded; durable commit was not acknowledged. Never grant on it.
    AfterRename,
}

#[derive(Debug)]
pub struct CommitError {
    pub stage: CommitStage,
    pub source: StoreError,
}
impl fmt::Display for CommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "archive commit {:?}: {}", self.stage, self.source)
    }
}
impl std::error::Error for CommitError {}

/// Current corruption/IO, schema errors, proposed invalidity, business refusal,
/// and uncertain commit stay separate. Callers must fence uncertain commits;
/// matching visible bytes do not prove a previous successful acknowledgement.
#[derive(Debug)]
pub enum TransactionError<Schema, Rejection> {
    Current(StoreError),
    CurrentSchema(Schema),
    Proposal(StoreError),
    ProposalSchema(Schema),
    Rejected(Rejection),
    Commit(CommitError),
}
impl<S: fmt::Display, R: fmt::Display> fmt::Display for TransactionError<S, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Current(e) => write!(f, "current archive: {e}"),
            Self::CurrentSchema(e) => write!(f, "current schema: {e}"),
            Self::Proposal(e) => write!(f, "proposed archive: {e}"),
            Self::ProposalSchema(e) => write!(f, "proposed schema: {e}"),
            Self::Rejected(e) => write!(f, "proposal refused: {e}"),
            Self::Commit(e) => e.fmt(f),
        }
    }
}
impl<S: fmt::Debug + fmt::Display, R: fmt::Debug + fmt::Display> std::error::Error
    for TransactionError<S, R>
{
}

/// Errors from original-lock inspection of a bounded sibling file family.
#[derive(Debug)]
pub enum FamilyInspectionError<Schema> {
    Storage(StoreError),
    Schema(Schema),
}

impl ArchiveStore {
    /// Inspect original bytes of this existing archive and at most three named
    /// sibling archives under this archive's existing writer lock. Present
    /// files must be strict nonempty JSON arrays of objects; duplicate keys at
    /// every depth are rejected before the pure schema callback runs. Missing
    /// siblings are explicit None, while a missing active archive fails closed.
    ///
    /// This is inspection only: no archive data is written, no migration or
    /// dispatch is authorized, and results are not a transferable lock token.
    /// The callback must be synchronous and pure, like transaction_raw. Future
    /// family writers MUST use this same original lock across the entire
    /// protocol. Individual sibling locks do not protect this family snapshot.
    /// Presence witnesses may be enrolled only after successful validation.
    /// Cooperative-lock and namespace limitations of transaction_raw apply.
    pub fn inspect_raw_family<T, S>(
        &self,
        siblings: &[&str],
        inspect: impl FnOnce(&[u8], &[Option<Vec<u8>>]) -> Result<T, S>,
    ) -> Result<T, FamilyInspectionError<S>> {
        use FamilyInspectionError::{Schema, Storage};
        use std::path::{Component, Path};
        if siblings.len() > 3 {
            return Err(Storage(StoreError::InvalidLimit));
        }
        let mut names = HashSet::new();
        let parent = self.parent().map_err(Storage)?;
        let mut stores = Vec::new();
        for name in siblings {
            let path = Path::new(name);
            let mut parts = path.components();
            if !matches!(parts.next(), Some(Component::Normal(_)))
                || parts.next().is_some()
                || !names.insert(*name)
                || path.file_name() == self.path.file_name()
                || parent.join(name) == self.lock_path().map_err(Storage)?
                || parent.join(name) == self.presence_path().map_err(Storage)?
            {
                return Err(Storage(StoreError::Configuration(
                    "invalid_archive_family_name".into(),
                )));
            }
            stores.push(
                ArchiveStore::with_limits(parent.join(name), self.max_bytes, self.max_items)
                    .map_err(Storage)?,
            );
        }
        let mut strict = self.clone();
        strict.durable_presence = true;
        let (mut lock, enrolled) = strict.acquire_lock().map_err(Storage)?;
        let current = strict
            .read_bytes_unlocked()
            .map_err(Storage)?
            .ok_or_else(|| Storage(invalid_nonempty()))?;
        strict.validate_raw_bytes(&current).map_err(Storage)?;
        let mut family = Vec::new();
        for store in &stores {
            let bytes = store.read_bytes_unlocked().map_err(Storage)?;
            if let Some(ref bytes) = bytes {
                store.validate_raw_bytes(bytes).map_err(Storage)?;
            }
            family.push(bytes);
        }
        let stamps = std::iter::once(&strict)
            .chain(stores.iter())
            .map(family_stamp)
            .collect::<Result<Vec<_>, _>>()
            .map_err(Storage)?;
        let result = inspect(&current, &family).map_err(Schema)?;
        strict.validate_named_lock(&lock).map_err(Storage)?;
        // Do not return facts from a callback that replaced an entry or from an
        // uncooperative writer changing a sibling during inspection.
        if strict.read_bytes_unlocked().map_err(Storage)?.as_deref() != Some(&current) {
            return Err(Storage(StoreError::Configuration(
                "archive_family_changed".into(),
            )));
        }
        for (store, before) in stores.iter().zip(&family) {
            if store.read_bytes_unlocked().map_err(Storage)? != *before {
                return Err(Storage(StoreError::Configuration(
                    "archive_family_changed".into(),
                )));
            }
        }
        for (store, before) in std::iter::once(&strict).chain(stores.iter()).zip(stamps) {
            if family_stamp(store).map_err(Storage)? != before {
                return Err(Storage(StoreError::Configuration(
                    "archive_family_replaced".into(),
                )));
            }
        }
        let rechecked = strict
            .validate_presence_unlocked(&mut lock)
            .map_err(Storage)?;
        if rechecked != enrolled {
            return Err(Storage(StoreError::Configuration(
                "archive_presence_changed".into(),
            )));
        }
        strict
            .ensure_presence_unlocked(&mut lock, rechecked)
            .map_err(Storage)?;
        strict.validate_named_lock(&lock).map_err(Storage)?;
        Ok(result)
    }
}

// A same-byte rename still changes family provenance and is rejected. The
// named lock alone is insufficient when a callback violates its purity contract.
#[derive(Eq, PartialEq)]
struct FamilyStamp(u64, u64, u64, i64, i64, i64, i64, u32);
fn family_stamp(store: &ArchiveStore) -> Result<Option<FamilyStamp>, StoreError> {
    let metadata = match std::fs::symlink_metadata(&store.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StoreError::Io(error)),
    };
    if !metadata.is_file() {
        return Err(StoreError::NotRegularFile);
    }
    Ok(Some(FamilyStamp(
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
        metadata.mode(),
    )))
}

impl ArchiveStore {
    /// Read bounded original bytes under the original named lock, strictly
    /// validate current and proposed nonempty arrays of objects, then commit.
    /// Duplicate object keys (including escaped-equivalent keys) at every depth
    /// are rejected before either callback observes current bytes.
    ///
    /// `validate` is the caller's strict schema decoder, invoked on both current
    /// and proposed bytes. `plan` runs once only after valid current bytes. Both
    /// callbacks MUST be synchronous, pure decode/plan/encode: no IO, dispatch,
    /// other physical owner acquisition, or recursive archive entry. They cannot
    /// await. This API does not enforce caller purity or supply a daemon latch.
    /// An existing nonempty authority is required; fixture creation is separate.
    /// A valid existing file without presence witnesses is enrolled under this
    /// lock (marker + lock anchor). Even `Unchanged` may write these metadata,
    /// while preserving archive bytes and inode. It never creates archive data.
    /// A valid marker plus an empty interrupted lock anchor is recoverable: the
    /// anchor is rewritten and synced before any successful result is returned.
    /// This relies on cooperative writers using this same named lock; pathname
    /// checks do not prove safety against arbitrary hostile external namespace
    /// replacement between the final check and rename, or root provenance.
    pub fn transaction_raw<T, S, R>(
        &self,
        validate: impl Fn(&[u8]) -> Result<(), S>,
        plan: impl FnOnce(&[u8]) -> Result<Mutation<T>, R>,
    ) -> Result<T, TransactionError<S, R>> {
        use TransactionError::*;
        let mut strict = self.clone();
        strict.durable_presence = true;
        let (mut lock, enrolled) = strict.acquire_lock().map_err(Current)?;
        let current = strict
            .read_bytes_unlocked()
            .map_err(Current)?
            .ok_or_else(|| Current(invalid_nonempty()))?;
        let identity =
            std::fs::symlink_metadata(&strict.path).map_err(|e| Current(StoreError::Io(e)))?;
        strict.validate_raw_bytes(&current).map_err(Current)?;
        validate(&current).map_err(CurrentSchema)?;
        let mutation = plan(&current).map_err(Rejected)?;
        if let Mutation::Replace { ref bytes, .. } = mutation {
            strict.validate_raw_bytes(bytes).map_err(Proposal)?;
            validate(bytes).map_err(ProposalSchema)?;
        }
        // Revalidate after callbacks and after any wait, before touching durable
        // state. Cooperative writers cannot replace the file while we hold lock.
        strict.validate_named_lock(&lock).map_err(Current)?;
        let named =
            std::fs::symlink_metadata(&strict.path).map_err(|e| Current(StoreError::Io(e)))?;
        if identity.dev() != named.dev()
            || identity.ino() != named.ino()
            || identity.len() != named.len()
            || identity.mtime() != named.mtime()
            || identity.mtime_nsec() != named.mtime_nsec()
            || identity.ctime() != named.ctime()
            || identity.ctime_nsec() != named.ctime_nsec()
        {
            return Err(Current(StoreError::Configuration(
                "archive_current_replaced".into(),
            )));
        }
        if strict.read_bytes_unlocked().map_err(Current)?.as_deref() != Some(current.as_slice()) {
            return Err(Current(StoreError::Configuration(
                "archive_current_changed".into(),
            )));
        }
        let rechecked_enrollment = strict
            .validate_presence_unlocked(&mut lock)
            .map_err(Current)?;
        if rechecked_enrollment != enrolled {
            return Err(Current(StoreError::Configuration(
                "archive_presence_changed".into(),
            )));
        }
        match mutation {
            Mutation::Unchanged(result) => {
                strict
                    .ensure_presence_unlocked(&mut lock, rechecked_enrollment)
                    .map_err(Current)?;
                strict.validate_named_lock(&lock).map_err(Current)?;
                Ok(result)
            }
            Mutation::Replace { bytes, result } => {
                strict
                    .ensure_presence_unlocked(&mut lock, rechecked_enrollment)
                    .map_err(|source| {
                        Commit(CommitError {
                            stage: CommitStage::BeforeRename,
                            source,
                        })
                    })?;
                strict.validate_named_lock(&lock).map_err(Current)?;
                strict.persist_bytes_unlocked(&bytes).map_err(Commit)?;
                Ok(result)
            }
        }
    }

    pub(crate) fn validate_raw_bytes(&self, bytes: &[u8]) -> Result<(), StoreError> {
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if length > self.max_bytes {
            return Err(StoreError::TooLarge {
                actual: length,
                limit: self.max_bytes,
            });
        }
        if bytes.is_empty() {
            return Err(invalid_nonempty());
        }
        // This streaming structural visitor never constructs serde_json::Value
        // and therefore never discards duplicate evidence before validation.
        let mut decoder = serde_json::Deserializer::from_slice(bytes);
        let shape = Shape::deserialize(&mut decoder).map_err(StoreError::Json)?;
        decoder.end().map_err(StoreError::Json)?;
        let Some((count, objects)) = shape.array else {
            return Err(StoreError::RootIsNotArray);
        };
        if count == 0 {
            return Err(invalid_nonempty());
        }
        if count > self.max_items {
            return Err(StoreError::TooManyItems {
                actual: count,
                limit: self.max_items,
            });
        }
        if !objects {
            return Err(StoreError::ItemIsNotObject);
        }
        Ok(())
    }
}

fn invalid_nonempty() -> StoreError {
    StoreError::Configuration("invalid_durable_ledger_store".into())
}

struct Shape {
    object: bool,
    array: Option<(usize, bool)>,
}
impl Shape {
    fn scalar() -> Self {
        Self {
            object: false,
            array: None,
        }
    }
}
impl<'de> Deserialize<'de> for Shape {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        decoder.deserialize_any(ShapeVisitor)
    }
}
struct ShapeVisitor;
impl<'de> Visitor<'de> for ShapeVisitor {
    type Value = Shape;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("strict JSON")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Shape, A::Error> {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            map.next_value::<Shape>()?;
        }
        Ok(Shape {
            object: true,
            array: None,
        })
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shape, A::Error> {
        let mut count = 0usize;
        let mut objects = true;
        while let Some(value) = seq.next_element::<Shape>()? {
            count = count
                .checked_add(1)
                .ok_or_else(|| de::Error::custom("array count overflow"))?;
            objects &= value.object;
        }
        Ok(Shape {
            object: false,
            array: Some((count, objects)),
        })
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Shape, E> {
        Ok(Shape::scalar())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Shape, E> {
        Ok(Shape::scalar())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Shape, E> {
        Ok(Shape::scalar())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Shape, E> {
        Ok(Shape::scalar())
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Shape, E> {
        Ok(Shape::scalar())
    }
    fn visit_unit<E: de::Error>(self) -> Result<Shape, E> {
        Ok(Shape::scalar())
    }
}

#[cfg(test)]
mod tests;
