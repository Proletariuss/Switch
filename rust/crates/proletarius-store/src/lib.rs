#![forbid(unsafe_code)]

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

#[cfg(any(test, feature = "device-sms-generation-seal"))]
pub mod device_sms_seal;
pub mod raw;

pub const STORE_CONTRACT: &str = "atomic-json-archive-v1";
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_MAX_ITEMS: usize = 1_000;
const PRESENCE_CONTRACT: &[u8] = b"switch-archive-presence-v1\n";

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct ArchiveStore {
    path: PathBuf,
    max_bytes: u64,
    max_items: usize,
    nonempty_ledger: bool,
    durable_presence: bool,
    observed_present: Arc<AtomicBool>,
    #[cfg(test)]
    after_lock_open: Option<PathBuf>,
    #[cfg(any(test, feature = "raw-fault-tests"))]
    fail_commit_stage: Arc<AtomicU64>,
    #[cfg(test)]
    fail_next_directory_sync: Arc<AtomicBool>,
}

impl ArchiveStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_bytes: DEFAULT_MAX_BYTES,
            max_items: DEFAULT_MAX_ITEMS,
            nonempty_ledger: false,
            durable_presence: false,
            observed_present: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            after_lock_open: None,
            #[cfg(any(test, feature = "raw-fault-tests"))]
            fail_commit_stage: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            fail_next_directory_sync: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_limits(
        path: impl Into<PathBuf>,
        max_bytes: u64,
        max_items: usize,
    ) -> Result<Self, StoreError> {
        if max_bytes == 0 || max_items == 0 {
            return Err(StoreError::InvalidLimit);
        }
        Ok(Self {
            path: path.into(),
            max_bytes,
            max_items,
            nonempty_ledger: false,
            durable_presence: false,
            observed_present: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            after_lock_open: None,
            #[cfg(any(test, feature = "raw-fault-tests"))]
            fail_commit_stage: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            fail_next_directory_sync: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Opt in for durable identity ledgers: only a never-created path is empty.
    /// Present empty/null/[] archives, later disappearance and item overflow
    /// fail closed under the archive lock; no ledger entries are truncated.
    pub fn require_nonempty_ledger(mut self) -> Result<Self, StoreError> {
        self.nonempty_ledger = true;
        self.durable_presence = true;
        self.observed_present.store(
            self.path.try_exists().map_err(StoreError::Io)?,
            Ordering::Release,
        );
        Ok(self)
    }

    /// Persist the fact that an archive has existed.  A marker plus the
    /// archive lock's independent anchor prevent a later Store instance from
    /// treating a deleted ledger (or a deleted marker) as a brand-new store.
    /// The first successful operation against a legacy archive enrolls it;
    /// an absent archive remains a valid never-created store until its first
    /// successful write.
    pub fn track_durable_presence(mut self) -> Self {
        self.durable_presence = true;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Vec<Value>, StoreError> {
        self.with_exclusive_lock(|store, _, _| store.load_unlocked())
    }

    /// Pin one immutable archive generation without decoding under its lock.
    /// Writers must keep using atomic replacement (as all store writers do).
    /// The returned descriptor then survives subsequent rename/delete safely;
    /// callers must stream with the same size and format limits as their store.
    pub fn snapshot_file(&self) -> Result<Option<File>, StoreError> {
        self.with_exclusive_lock(|store, _, _| {
            reject_non_regular(&store.path)?;
            let file = match File::open(&store.path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(StoreError::Io(error)),
            };
            let metadata = file.metadata().map_err(StoreError::Io)?;
            if !metadata.is_file() {
                return Err(StoreError::NotRegularFile);
            }
            if metadata.len() > store.max_bytes {
                return Err(StoreError::TooLarge {
                    actual: metadata.len(),
                    limit: store.max_bytes,
                });
            }
            Ok(Some(file))
        })
    }

    pub fn replace(&self, mut items: Vec<Value>) -> Result<(), StoreError> {
        self.with_exclusive_lock(move |store, lock, enrolled| {
            if store.nonempty_ledger {
                // Never overwrite a damaged or disappeared ledger from a
                // caller's cached projection.
                store.load_unlocked()?;
            }
            store.bound_items(&mut items)?;
            store.ensure_presence_before_persist(lock, enrolled)?;
            store.persist_unlocked(&items)
        })
    }

    /// Delete the backing archive after a caller has durably transferred its
    /// contents elsewhere.  This deliberately uses the same per-file lock
    /// and symlink checks as `load`/`replace`: recovery journals must never
    /// turn a stale or swapped path into an arbitrary file deletion.
    pub fn remove(&self) -> Result<(), StoreError> {
        self.with_exclusive_lock(|store, _, _| {
            reject_non_regular(&store.path)?;
            match fs::remove_file(&store.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(StoreError::Io(error)),
            }
        })
    }

    /// Removes a present archive only when its validated contents are empty.
    /// The read/check/delete sequence stays under the archive's cross-process
    /// writer lock so a concurrent writer's non-empty state cannot be erased.
    pub fn remove_if_empty(&self) -> Result<bool, StoreError> {
        self.with_exclusive_lock(|store, _, _| {
            let items = store.load_unlocked()?;
            if !items.is_empty() || !store.path.exists() {
                return Ok(false);
            }
            reject_non_regular(&store.path)?;
            match fs::remove_file(&store.path) {
                Ok(()) => {
                    File::open(store.parent()?)
                        .and_then(|directory| directory.sync_all())
                        .map_err(StoreError::Io)?;
                    Ok(true)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(StoreError::Io(error)),
            }
        })
    }

    /// Reconfirm directory-entry durability after an earlier atomic rename
    /// returned an uncertain result. This is intentionally separate from a
    /// no-op mutation so idempotent retry paths can fsync the directory
    /// without needlessly replacing the archive file again.
    pub fn confirm_directory_sync(&self) -> Result<(), StoreError> {
        self.with_exclusive_lock(|store, _, _| {
            File::open(store.parent()?)
                .and_then(|directory| directory.sync_all())
                .map_err(StoreError::Io)
        })
    }

    pub fn mutate<T>(
        &self,
        mutate: impl FnOnce(&mut Vec<Value>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.mutate_if_changed(move |items| mutate(items).map(|result| (result, true)))
    }

    /// Atomically mutate this archive while allowing idempotent operations to
    /// avoid rewriting unchanged bytes. The closure's changed flag is applied
    /// under the same file lock as the read and optional replacement.
    pub fn mutate_if_changed<T>(
        &self,
        mutate: impl FnOnce(&mut Vec<Value>) -> Result<(T, bool), StoreError>,
    ) -> Result<T, StoreError> {
        self.with_exclusive_lock(move |store, lock, enrolled| {
            let mut items = store.load_unlocked()?;
            let (result, changed) = mutate(&mut items)?;
            if changed {
                store.bound_items(&mut items)?;
                store.ensure_presence_before_persist(lock, enrolled)?;
                store.persist_unlocked(&items)?;
            }
            Ok(result)
        })
    }

    fn with_exclusive_lock<T>(
        &self,
        operation: impl FnOnce(&Self, &mut File, &mut bool) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let (mut lock, mut enrolled) = self.acquire_lock()?;
        // Enroll a legacy archive before any business closure observes it.
        // A never-created archive remains untouched by reads. Writers create
        // the witness immediately before their first possible archive commit.
        if self.durable_presence && !enrolled && self.path.try_exists().map_err(StoreError::Io)? {
            self.ensure_presence_unlocked(&mut lock, false)?;
            enrolled = true;
        }
        let result = operation(self, &mut lock, &mut enrolled)?;
        if self.durable_presence && self.path.try_exists().map_err(StoreError::Io)? {
            self.ensure_presence_unlocked(&mut lock, enrolled)?;
        }
        Ok(result)
    }

    fn acquire_lock(&self) -> Result<(File, bool), StoreError> {
        let parent = self.parent()?;
        fs::create_dir_all(parent).map_err(StoreError::Io)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(StoreError::Io)?;
        let lock_path = self.lock_path()?;
        reject_non_regular(&lock_path)?;
        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .map_err(StoreError::Io)?;
        #[cfg(test)]
        if let Some(path) = &self.after_lock_open {
            fs::write(path, b"opened").map_err(StoreError::Io)?;
        }
        lock.lock().map_err(StoreError::Io)?;
        self.validate_named_lock(&lock)?;
        let enrolled = if self.durable_presence {
            self.validate_presence_unlocked(&mut lock)?
        } else {
            false
        };
        Ok((lock, enrolled))
    }

    fn validate_named_lock(&self, lock: &File) -> Result<(), StoreError> {
        let path = self.lock_path()?;
        reject_non_regular(&path)?;
        let held = lock.metadata().map_err(StoreError::Io)?;
        let named = fs::symlink_metadata(path).map_err(StoreError::Io)?;
        if !held.is_file() || held.dev() != named.dev() || held.ino() != named.ino() {
            return Err(StoreError::Configuration("archive_lock_replaced".into()));
        }
        Ok(())
    }

    fn ensure_presence_before_persist(
        &self,
        lock: &mut File,
        enrolled: &mut bool,
    ) -> Result<(), StoreError> {
        if self.durable_presence && !*enrolled {
            self.ensure_presence_unlocked(lock, false)?;
            *enrolled = true;
        }
        Ok(())
    }

    fn load_unlocked(&self) -> Result<Vec<Value>, StoreError> {
        let Some(bytes) = self.read_bytes_unlocked()? else {
            if self.nonempty_ledger && self.observed_present.load(Ordering::Acquire) {
                return Err(StoreError::Configuration(
                    "invalid_durable_ledger_store".into(),
                ));
            }
            return Ok(Vec::new());
        };
        if bytes.is_empty() {
            return if self.nonempty_ledger {
                Err(StoreError::Configuration(
                    "invalid_durable_ledger_store".into(),
                ))
            } else {
                Ok(Vec::new())
            };
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(StoreError::Json)?;
        // The legacy Go writer serializes a cleared nil slice as JSON null.
        if value.is_null() && !self.nonempty_ledger {
            return Ok(Vec::new());
        }
        let Value::Array(items) = value else {
            return Err(StoreError::RootIsNotArray);
        };
        if self.nonempty_ledger && items.is_empty() {
            return Err(StoreError::Configuration(
                "invalid_durable_ledger_store".into(),
            ));
        }
        if items.len() > self.max_items {
            return Err(StoreError::TooManyItems {
                actual: items.len(),
                limit: self.max_items,
            });
        }
        if items.iter().any(|item| !item.is_object()) {
            return Err(StoreError::ItemIsNotObject);
        }
        Ok(items)
    }

    fn read_bytes_unlocked(&self) -> Result<Option<Vec<u8>>, StoreError> {
        reject_non_regular(&self.path)?;
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(StoreError::Io(error)),
        };
        let held = file.metadata().map_err(StoreError::Io)?;
        reject_non_regular(&self.path)?;
        let named = fs::symlink_metadata(&self.path).map_err(StoreError::Io)?;
        if !held.is_file() || held.dev() != named.dev() || held.ino() != named.ino() {
            return Err(StoreError::Configuration("archive_current_replaced".into()));
        }
        self.observed_present.store(true, Ordering::Release);
        let length = file.metadata().map_err(StoreError::Io)?.len();
        if length > self.max_bytes {
            return Err(StoreError::TooLarge {
                actual: length,
                limit: self.max_bytes,
            });
        }
        let capacity = usize::try_from(length).map_err(|_| StoreError::TooLarge {
            actual: length,
            limit: self.max_bytes,
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        (&file)
            .take(self.max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(StoreError::Io)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.max_bytes {
            return Err(StoreError::TooLarge {
                actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                limit: self.max_bytes,
            });
        }
        reject_non_regular(&self.path)?;
        let named = fs::symlink_metadata(&self.path).map_err(StoreError::Io)?;
        let after = file.metadata().map_err(StoreError::Io)?;
        if held.dev() != named.dev()
            || held.ino() != named.ino()
            || held.len() != after.len()
            || held.mtime() != after.mtime()
            || held.mtime_nsec() != after.mtime_nsec()
            || held.ctime() != after.ctime()
            || held.ctime_nsec() != after.ctime_nsec()
        {
            return Err(StoreError::Configuration("archive_current_replaced".into()));
        }
        Ok(Some(bytes))
    }

    fn persist_unlocked(&self, items: &[Value]) -> Result<(), StoreError> {
        if self.nonempty_ledger && items.is_empty() {
            return Err(StoreError::Configuration(
                "invalid_durable_ledger_store".into(),
            ));
        }
        if items.iter().any(|item| !item.is_object()) {
            return Err(StoreError::ItemIsNotObject);
        }
        let data = serde_json::to_vec(items).map_err(StoreError::Json)?;
        let length = u64::try_from(data.len()).unwrap_or(u64::MAX);
        if length > self.max_bytes {
            return Err(StoreError::TooLarge {
                actual: length,
                limit: self.max_bytes,
            });
        }
        self.persist_bytes_unlocked(&data)
            .map_err(|error| error.source)
    }

    // The single byte persistence protocol used by legacy and strict APIs.
    fn persist_bytes_unlocked(&self, data: &[u8]) -> Result<(), raw::CommitError> {
        use raw::{CommitError, CommitStage};
        let mut stage = CommitStage::BeforeRename;
        let mut temp_created = false;
        let mut temp_path = None;
        let result = (|| -> Result<(), StoreError> {
            reject_non_regular(&self.path)?;
            let temp = self.temp_path()?;
            temp_path = Some(temp.clone());
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)
                .map_err(StoreError::Io)?;
            temp_created = true;
            self.commit_fault(1)?;
            file.write_all(data).map_err(StoreError::Io)?;
            self.commit_fault(2)?;
            file.sync_all().map_err(StoreError::Io)?;
            self.commit_fault(3)?;
            fs::rename(&temp, &self.path).map_err(StoreError::Io)?;
            stage = CommitStage::AfterRename;
            self.observed_present.store(true, Ordering::Release);
            self.commit_fault(4)?;
            #[cfg(test)]
            if self.fail_next_directory_sync.swap(false, Ordering::SeqCst) {
                return Err(StoreError::Io(std::io::Error::other(
                    "injected directory sync failure after archive rename",
                )));
            }
            File::open(self.parent()?)
                .and_then(|directory| directory.sync_all())
                .map_err(StoreError::Io)?;
            self.commit_fault(5)?;
            Ok(())
        })();
        if result.is_err() && temp_created {
            if let Some(temp) = temp_path {
                let _ = fs::remove_file(temp);
            }
        }
        result.map_err(|source| CommitError { stage, source })
    }

    /// Explicit isolated fault builds only; no environment or production switch.
    #[cfg(feature = "raw-fault-tests")]
    pub fn fail_commit_at_for_test(&self, point: u64) {
        self.fail_commit_stage.store(point, Ordering::SeqCst);
    }

    fn commit_fault(&self, point: u64) -> Result<(), StoreError> {
        #[cfg(any(test, feature = "raw-fault-tests"))]
        if self
            .fail_commit_stage
            .compare_exchange(point, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(StoreError::Io(std::io::Error::other(format!(
                "injected commit stage {point}"
            ))));
        }
        let _ = point;
        Ok(())
    }

    fn bound_items(&self, items: &mut Vec<Value>) -> Result<(), StoreError> {
        if self.nonempty_ledger && items.len() > self.max_items {
            return Err(StoreError::TooManyItems {
                actual: items.len(),
                limit: self.max_items,
            });
        }
        items.truncate(self.max_items);
        Ok(())
    }

    fn parent(&self) -> Result<&Path, StoreError> {
        self.path.parent().ok_or(StoreError::MissingParent)
    }

    fn lock_path(&self) -> Result<PathBuf, StoreError> {
        let name = self
            .path
            .file_name()
            .ok_or(StoreError::MissingFileName)?
            .to_string_lossy();
        Ok(self.path.with_file_name(format!("{name}.lock")))
    }

    fn temp_path(&self) -> Result<PathBuf, StoreError> {
        let name = self
            .path
            .file_name()
            .ok_or(StoreError::MissingFileName)?
            .to_string_lossy();
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .path
            .with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), sequence)))
    }

    fn presence_path(&self) -> Result<PathBuf, StoreError> {
        let name = self
            .path
            .file_name()
            .ok_or(StoreError::MissingFileName)?
            .to_string_lossy();
        Ok(self.path.with_file_name(format!(".{name}.presence-v1")))
    }

    fn validate_presence_unlocked(&self, lock: &mut File) -> Result<bool, StoreError> {
        let lock_anchored = read_presence_contract(lock)?;
        let marker_path = self.presence_path()?;
        reject_non_regular(&marker_path)?;
        let marker_present = match File::open(&marker_path) {
            Ok(mut marker) => {
                if !read_presence_contract(&mut marker)? {
                    return Err(invalid_durable_presence());
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(StoreError::Io(error)),
        };
        if lock_anchored && !marker_present {
            return Err(invalid_durable_presence());
        }
        if marker_present || lock_anchored {
            reject_non_regular(&self.path)?;
            if !self.path.try_exists().map_err(StoreError::Io)? {
                return Err(invalid_durable_presence());
            }
            self.observed_present.store(true, Ordering::Release);
            return Ok(true);
        }
        Ok(false)
    }

    fn ensure_presence_unlocked(
        &self,
        lock: &mut File,
        already_enrolled: bool,
    ) -> Result<(), StoreError> {
        let marker_path = self.presence_path()?;
        reject_non_regular(&marker_path)?;
        if !already_enrolled || !marker_path.try_exists().map_err(StoreError::Io)? {
            let mut marker = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&marker_path)
                .map_err(StoreError::Io)?;
            marker
                .write_all(PRESENCE_CONTRACT)
                .and_then(|_| marker.sync_all())
                .map_err(StoreError::Io)?;
            File::open(self.parent()?)
                .and_then(|directory| directory.sync_all())
                .map_err(StoreError::Io)?;
        }
        lock.set_len(0).map_err(StoreError::Io)?;
        self.commit_fault(6)?;
        lock.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
        lock.write_all(PRESENCE_CONTRACT).map_err(StoreError::Io)?;
        lock.sync_all().map_err(StoreError::Io)?;
        self.observed_present.store(true, Ordering::Release);
        Ok(())
    }
}

fn invalid_durable_presence() -> StoreError {
    StoreError::Configuration("invalid_durable_presence_store".into())
}

fn read_presence_contract(file: &mut File) -> Result<bool, StoreError> {
    file.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
    let mut bytes = Vec::with_capacity(PRESENCE_CONTRACT.len() + 1);
    file.take((PRESENCE_CONTRACT.len() + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(StoreError::Io)?;
    if bytes.is_empty() {
        return Ok(false);
    }
    if bytes == PRESENCE_CONTRACT {
        return Ok(true);
    }
    Err(invalid_durable_presence())
}

fn reject_non_regular(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StoreError::SymlinkRejected),
        Ok(metadata) if !metadata.file_type().is_file() => Err(StoreError::NotRegularFile),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::Io(error)),
    }
}

#[derive(Debug)]
pub enum StoreError {
    InvalidLimit,
    Configuration(String),
    MissingParent,
    MissingFileName,
    SymlinkRejected,
    NotRegularFile,
    RootIsNotArray,
    ItemIsNotObject,
    TooLarge { actual: u64, limit: u64 },
    TooManyItems { actual: usize, limit: usize },
    Json(serde_json::Error),
    Io(std::io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit => formatter.write_str("archive limits must be non-zero"),
            Self::Configuration(message) => formatter.write_str(message),
            Self::MissingParent => formatter.write_str("archive path has no parent"),
            Self::MissingFileName => formatter.write_str("archive path has no file name"),
            Self::SymlinkRejected => {
                formatter.write_str("archive path must not be a symbolic link")
            }
            Self::NotRegularFile => formatter.write_str("archive path is not a regular file"),
            Self::RootIsNotArray => formatter.write_str("archive root must be a JSON array"),
            Self::ItemIsNotObject => formatter.write_str("archive items must be JSON objects"),
            Self::TooLarge { actual, limit } => {
                write!(formatter, "archive is {actual} bytes, limit is {limit}")
            }
            Self::TooManyItems { actual, limit } => {
                write!(formatter, "archive has {actual} items, limit is {limit}")
            }
            Self::Json(error) => write!(formatter, "archive JSON: {error}"),
            Self::Io(error) => write!(formatter, "archive I/O: {error}"),
        }
    }
}

impl std::error::Error for StoreError {}

#[cfg(test)]
mod tests {
    use super::{ArchiveStore, PRESENCE_CONTRACT, StoreError, TEMP_SEQUENCE};
    use serde_json::{Value, json};
    use std::fs;
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub(crate) struct TestDirectory(pub(crate) PathBuf);

    impl TestDirectory {
        pub(crate) fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "proletarius-store-test-{}-{nonce}",
                std::process::id(),
                nonce = nonce + u128::from(TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed))
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub(crate) fn archive(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn nonempty_ledger_initial_creation_corruption_and_capacity_fail_closed() {
        let directory = TestDirectory::new();
        let path = directory.archive("ledger.json");
        let store = ArchiveStore::with_limits(&path, 1024, 2)
            .unwrap()
            .require_nonempty_ledger()
            .unwrap();
        assert!(store.load().unwrap().is_empty());
        assert!(!path.exists());
        store
            .mutate(|items| {
                items.push(json!({"id": 1}));
                Ok(())
            })
            .unwrap();
        let committed = fs::read(&path).unwrap();
        assert!(
            store
                .replace(vec![json!({"id": 1}), json!({"id": 2}), json!({"id": 3})])
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), committed);
        for damage in [b"".as_slice(), b"null", b"[]", b"{broken"] {
            fs::write(&path, damage).unwrap();
            let reopened = ArchiveStore::new(&path).require_nonempty_ledger().unwrap();
            assert!(reopened.load().is_err());
            assert!(
                reopened
                    .mutate::<()>(|_| panic!("corrupt ledger admitted mutation"))
                    .is_err()
            );
            assert!(reopened.replace(vec![json!({"id": 2})]).is_err());
            assert_eq!(fs::read(&path).unwrap(), damage);
        }
        fs::remove_file(&path).unwrap();
        assert!(store.load().is_err());
    }

    #[test]
    fn durable_presence_survives_reopen_and_rejects_archive_or_marker_loss() {
        for damage in ["archive_missing", "marker_missing", "marker_corrupt"] {
            let directory = TestDirectory::new();
            let path = directory.archive("ledger.json");
            let store = ArchiveStore::new(&path).require_nonempty_ledger().unwrap();
            assert!(store.load().unwrap().is_empty());
            assert!(!path.exists(), "a read must not create a blank ledger");
            store.replace(vec![json!({"id":"durable"})]).unwrap();
            let marker = store.presence_path().unwrap();
            let lock = store.lock_path().unwrap();
            assert_eq!(fs::read(&marker).unwrap(), PRESENCE_CONTRACT);
            assert_eq!(fs::read(&lock).unwrap(), PRESENCE_CONTRACT);
            drop(store);

            let reopened = ArchiveStore::new(&path).require_nonempty_ledger().unwrap();
            assert_eq!(reopened.load().unwrap(), vec![json!({"id":"durable"})]);
            match damage {
                "archive_missing" => fs::remove_file(&path).unwrap(),
                "marker_missing" => fs::remove_file(&marker).unwrap(),
                "marker_corrupt" => fs::write(&marker, b"corrupt-marker").unwrap(),
                _ => unreachable!(),
            }
            let after_restart = ArchiveStore::new(&path).require_nonempty_ledger().unwrap();
            assert!(
                matches!(after_restart.load(), Err(StoreError::Configuration(code)) if code == "invalid_durable_presence_store"),
                "{damage} was treated as a new ledger"
            );
            assert!(
                after_restart
                    .mutate::<()>(|_| panic!("damaged presence admitted mutation"))
                    .is_err()
            );
        }
    }

    #[test]
    fn durable_presence_witness_precedes_first_archive_commit() {
        let directory = TestDirectory::new();
        let path = directory.archive("ledger.json");
        let store = ArchiveStore::new(&path).require_nonempty_ledger().unwrap();
        store.fail_next_directory_sync.store(true, Ordering::SeqCst);
        let result = store.replace(vec![json!({"id":"committed-before-error"})]);
        assert!(result.is_err());
        assert!(path.exists(), "the injected failure occurs after rename");
        assert_eq!(
            fs::read(store.presence_path().unwrap()).unwrap(),
            PRESENCE_CONTRACT,
            "the durable witness must exist before archive rename"
        );
        assert_eq!(
            fs::read(store.lock_path().unwrap()).unwrap(),
            PRESENCE_CONTRACT,
            "the independent anchor must exist before archive rename"
        );

        fs::remove_file(&path).unwrap();
        let reopened = ArchiveStore::new(&path).require_nonempty_ledger().unwrap();
        assert!(matches!(
            reopened.load(),
            Err(StoreError::Configuration(code)) if code == "invalid_durable_presence_store"
        ));
    }

    #[test]
    fn durable_presence_fails_closed_when_first_archive_write_fails() {
        let directory = TestDirectory::new();
        let path = directory.archive("ledger.json");
        let store = ArchiveStore::with_limits(&path, 2, 4)
            .unwrap()
            .require_nonempty_ledger()
            .unwrap();
        assert!(store.replace(vec![json!({"id":"too-large"})]).is_err());
        assert!(!path.exists());
        assert_eq!(
            fs::read(store.presence_path().unwrap()).unwrap(),
            PRESENCE_CONTRACT
        );
        assert_eq!(
            fs::read(store.lock_path().unwrap()).unwrap(),
            PRESENCE_CONTRACT
        );

        let reopened = ArchiveStore::with_limits(&path, 2, 4)
            .unwrap()
            .require_nonempty_ledger()
            .unwrap();
        assert!(matches!(
            reopened.load(),
            Err(StoreError::Configuration(code)) if code == "invalid_durable_presence_store"
        ));
    }

    #[test]
    fn durable_presence_enrolls_legacy_archive_before_returning_data() {
        let directory = TestDirectory::new();
        let path = directory.archive("ledger.json");
        fs::write(&path, br#"[{"id":"legacy"}]"#).unwrap();
        let store = ArchiveStore::new(&path).require_nonempty_ledger().unwrap();
        assert_eq!(store.load().unwrap(), vec![json!({"id":"legacy"})]);
        assert_eq!(
            fs::read(store.presence_path().unwrap()).unwrap(),
            PRESENCE_CONTRACT
        );
        assert_eq!(
            fs::read(store.lock_path().unwrap()).unwrap(),
            PRESENCE_CONTRACT
        );
    }

    #[test]
    fn durable_presence_marker_io_failure_is_fail_closed() {
        let directory = TestDirectory::new();
        let path = directory.archive("ledger.json");
        let store = ArchiveStore::new(&path).require_nonempty_ledger().unwrap();
        store.replace(vec![json!({"id":"durable"})]).unwrap();
        let marker = store.presence_path().unwrap();
        let original_mode = fs::metadata(&marker).unwrap().permissions();
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o000)).unwrap();
        let reopened = ArchiveStore::new(&path).require_nonempty_ledger().unwrap();
        let result = reopened.load();
        fs::set_permissions(&marker, original_mode).unwrap();
        assert!(matches!(result, Err(StoreError::Io(_))));
    }

    #[test]
    fn nonempty_ledger_rechecks_presence_after_waiting_for_the_archive_lock() {
        let directory = TestDirectory::new();
        let path = directory.archive("ledger.json");
        let store = ArchiveStore::new(&path).require_nonempty_ledger().unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.archive("ledger.json.lock"))
            .unwrap();
        lock.lock().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            result_tx
                .send(store.mutate::<()>(|_| panic!("present empty ledger passed locked check")))
                .unwrap();
        });
        started_rx.recv().unwrap();
        assert!(
            result_rx
                .recv_timeout(std::time::Duration::from_millis(75))
                .is_err()
        );
        fs::write(&path, b"[]").unwrap();
        lock.unlock().unwrap();
        assert!(
            result_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        worker.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"[]");
    }

    #[test]
    fn reads_legacy_cleared_archive() {
        let directory = TestDirectory::new();
        let path = directory.archive("sms-archive.json");
        fs::write(&path, b"null\n").unwrap();
        let store = ArchiveStore::new(path);
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn remove_if_empty_is_locked_validated_and_preserves_nonempty_or_corrupt_state() {
        let directory = TestDirectory::new();
        let path = directory.archive("device-sms-forwarding-admission.json");
        let store = ArchiveStore::new(&path);
        assert!(!store.remove_if_empty().unwrap());

        fs::write(&path, b"[]").unwrap();
        assert!(store.remove_if_empty().unwrap());
        assert!(!path.exists());

        store.replace(vec![json!({"was_enabled": true})]).unwrap();
        assert!(!store.remove_if_empty().unwrap());
        assert_eq!(store.load().unwrap(), vec![json!({"was_enabled": true})]);

        fs::write(&path, b"{").unwrap();
        assert!(store.remove_if_empty().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{");
    }

    #[test]
    fn reads_and_rewrites_go_sms_and_call_shapes_without_field_loss() {
        let directory = TestDirectory::new();
        for (name, fixture) in [
            (
                "sms-archive.json",
                json!([{
                    "id": "reader|fixture",
                    "sender": "888",
                    "content": "BAL",
                    "timestamp": "2026-09-01T12:03:52.123456789Z",
                    "archived": true,
                    "direction": "out",
                    "device": "reader",
                    "future_field": {"preserved": true}
                }]),
            ),
            (
                "call-log.json",
                json!([{
                    "id": "call-fixture",
                    "device": "reader",
                    "direction": "out",
                    "number": "888",
                    "answered": true,
                    "started_at": "2026-09-01T12:06:46Z",
                    "ended_at": "2026-09-01T12:07:04Z",
                    "duration": 18,
                    "future_field": [1, 2, 3]
                }]),
            ),
        ] {
            let path = directory.archive(name);
            fs::write(&path, serde_json::to_vec(&fixture).unwrap()).unwrap();
            let store = ArchiveStore::new(&path);
            let items = store.load().unwrap();
            store.replace(items).unwrap();
            let rewritten: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            assert_eq!(rewritten, fixture);
        }
    }

    #[test]
    fn concurrent_mutations_are_serialized_without_lost_updates() {
        let directory = TestDirectory::new();
        let store = Arc::new(ArchiveStore::new(directory.archive("sms-archive.json")));
        let mut workers = Vec::new();
        for index in 0..8 {
            let store = Arc::clone(&store);
            workers.push(thread::spawn(move || {
                store
                    .mutate(|items| {
                        items.push(json!({"id": index}));
                        Ok(())
                    })
                    .unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let mut ids = store
            .load()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_u64().unwrap())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn archive_writer_waits_for_a_competing_lock_handle() {
        let directory = TestDirectory::new();
        let store = ArchiveStore::new(directory.archive("sms-archive.json"));
        let lock_path = directory.archive("sms-archive.json.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)
            .expect("open competing writer lock");
        lock.lock().expect("hold competing writer lock");

        let (started_tx, started_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            result_tx.send(store.load()).unwrap();
        });
        started_rx.recv().expect("worker started");
        assert!(
            result_rx
                .recv_timeout(std::time::Duration::from_millis(75))
                .is_err(),
            "archive read bypassed the exclusive writer lock"
        );
        lock.unlock().expect("release competing writer lock");
        result_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("archive read resumes after lock release")
            .expect("archive read succeeds");
        worker.join().unwrap();
    }

    #[test]
    fn malformed_oversized_and_non_object_archives_are_rejected_intact() {
        let directory = TestDirectory::new();
        let path = directory.archive("sms-archive.json");
        let store = ArchiveStore::with_limits(&path, 32, 2).unwrap();
        fs::write(&path, b"{broken").unwrap();
        assert!(matches!(store.load(), Err(StoreError::Json(_))));
        assert_eq!(fs::read(&path).unwrap(), b"{broken");

        fs::write(&path, br#"[1]"#).unwrap();
        assert!(matches!(store.load(), Err(StoreError::ItemIsNotObject)));

        fs::write(&path, vec![b'x'; 33]).unwrap();
        assert!(matches!(
            store.load(),
            Err(StoreError::TooLarge {
                actual: 33,
                limit: 32
            })
        ));
    }

    #[test]
    fn uncertain_directory_sync_after_rename_recovers_from_the_visible_contents() {
        let directory = TestDirectory::new();
        let path = directory.archive("sms-archive.json");
        let store = ArchiveStore::new(&path);
        let committed = json!({"id":"message-1","content":"durable fixture"});
        store.fail_next_directory_sync.store(true, Ordering::SeqCst);

        let error = store
            .replace(vec![committed.clone()])
            .expect_err("injected after-rename sync result is uncertain");
        assert!(
            error
                .to_string()
                .contains("injected directory sync failure")
        );
        assert_eq!(
            store.load().expect("visible renamed contents"),
            vec![committed.clone()]
        );

        // A caller reopening after the lost/unknown result can inspect the
        // same committed data, then explicitly reconfirm the directory entry
        // before it acknowledges its source.
        drop(store);
        let reopened = ArchiveStore::new(path);
        assert_eq!(
            reopened.load().expect("recover after restart"),
            vec![committed]
        );
        reopened
            .confirm_directory_sync()
            .expect("retry directory durability confirmation");
    }

    #[test]
    fn newest_items_are_bounded_before_atomic_replace() {
        let directory = TestDirectory::new();
        let path = directory.archive("call-log.json");
        let store = ArchiveStore::with_limits(&path, 1024, 2).unwrap();
        store
            .replace(vec![json!({"id": 3}), json!({"id": 2}), json!({"id": 1})])
            .unwrap();
        assert_eq!(
            store.load().unwrap(),
            vec![json!({"id": 3}), json!({"id": 2})]
        );
    }
}
