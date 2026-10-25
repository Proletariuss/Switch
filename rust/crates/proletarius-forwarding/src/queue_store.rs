//! Exclusive-owner, encrypted queue persistence for the legacy SWF1 format.
//!
//! The sibling lock is checked by independent native child processes against
//! the historical flock protocol. N-wait and device-SMS use this Rust owner.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{
    DeliveryOutcome, DestinationBinding, EnqueueOptions, EnqueueSummary, ForwardingChannel,
    ForwardingMessage, JsonDocument, QueueMutationError, StateError, StateKey, StateLimits, open,
    queue::DestinationMutation, queue::DestinationMutationKind, seal,
};

const INITIAL_QUEUE: &[u8] = br#"{"seen":{},"pending":{}}"#;

/// Encrypt the exact initial queue document used by QueueStore. The GUI's
/// credential bootstrap may publish this while holding the historical writer
/// flock, before making a newly generated key active in Keychain.
pub fn seal_empty_queue(key: &StateKey) -> Result<Vec<u8>, StateError> {
    let limits = StateLimits::default();
    let document = JsonDocument::from_bytes(Zeroizing::new(INITIAL_QUEUE.to_vec()), limits)?;
    seal(key, &document, limits)
}
const PRIVATE_FILE_MODE: u32 = 0o600;
const STATE_LOCK_SUFFIX: &str = ".lock";
const BOOTSTRAP_LOCK_SUFFIX: &str = ".bootstrap.lock";
const KEY_ID_SUFFIX: &str = ".keyid";
const KEY_ID_MAGIC: &[u8; 8] = b"SWFKID1\0";
const KEY_ID_DOMAIN: &[u8] = b"Switch.SMSForwarding.QueueKey.v1\0";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateStoreError {
    InvalidPath,
    UnsafePermissions,
    Busy,
    Io,
    State(StateError),
    PersistenceUncertain,
    ReopenRequired,
}

impl std::fmt::Display for StateStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidPath => "forwarding state path is invalid",
            Self::UnsafePermissions => "forwarding state file permissions are too broad",
            Self::Busy => "forwarding state already has a writer",
            Self::Io => "forwarding state storage operation failed",
            Self::State(error) => return error.fmt(f),
            Self::PersistenceUncertain => {
                "forwarding queue replacement is visible but directory durability is uncertain"
            }
            Self::ReopenRequired => {
                "forwarding state store must be reopened after an uncertain write"
            }
        })
    }
}

impl std::error::Error for StateStoreError {}

impl From<StateError> for StateStoreError {
    fn from(value: StateError) -> Self {
        Self::State(value)
    }
}

/// One durable begin's live authority. It cannot be recreated from disk or
/// provider metadata, and retaining the Arc prevents instance identity reuse.
#[derive(Clone)]
pub(crate) struct AttemptToken {
    instance: Arc<AttemptInstance>,
    sequence: u64,
    task_id: String,
    channel: ForwardingChannel,
}

struct AttemptInstance;

impl PartialEq for AttemptToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.instance, &other.instance)
            && self.sequence == other.sequence
            && self.task_id == other.task_id
            && self.channel == other.channel
    }
}
impl Eq for AttemptToken {}
impl std::fmt::Debug for AttemptToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AttemptToken([REDACTED])")
    }
}

/// An exclusive-lock owner for one encrypted SWF1 queue file.
///
/// The lock lives beside the state file and is held for the object's lifetime.
/// Callers must route every Rust writer through the same canonical path. A
/// separate-process test peer verifies the historical shared-lock protocol.
pub struct QueueStore {
    path: PathBuf,
    _lock: Flock<File>,
    lock_path: PathBuf,
    key_id_path: PathBuf,
    key_id: [u8; 40],
    key_id_file: File,
    key: StateKey,
    limits: StateLimits,
    current: JsonDocument,
    instance: Arc<AttemptInstance>,
    attempt_sequence: u64,
    in_flight: HashMap<(String, ForwardingChannel), AttemptToken>,
    telegram_submitting_chunk: HashMap<String, usize>,
    poisoned: bool,
}

impl std::fmt::Debug for QueueStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("QueueStore([REDACTED])")
    }
}

impl QueueStore {
    /// Owners must call this before releasing their mutable queue guard after
    /// an unwind or otherwise unknown write result. This only strengthens the
    /// reopen fence; it never rewrites disk, clears Unknown or drops live
    /// attempt tokens. The owner must still observe actual cleanup.
    pub fn require_reopen(&mut self) {
        self.poisoned = true;
    }

    /// Acquires the queue's nonblocking exclusive writer lock and opens its
    /// existing authenticated state. A missing file creates only an in-memory
    /// empty starting point; it is not written until the first new SMS arrives.
    /// Existing corruption, unsupported formats, broad permissions, symlinks,
    /// and lock contention all fail closed.
    pub fn open(
        path: impl AsRef<Path>,
        key: StateKey,
        limits: StateLimits,
    ) -> Result<Self, StateStoreError> {
        let path = normalize_path(path.as_ref())?;
        let bootstrap_path = suffixed_path(&path, BOOTSTRAP_LOCK_SUFFIX)?;
        let bootstrap_file = open_lock_file(&bootstrap_path)?;
        let bootstrap = Flock::lock(bootstrap_file, FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| flock_error(error))?;
        verify_named_file(&bootstrap_path, &bootstrap)?;
        let lock_path = suffixed_path(&path, STATE_LOCK_SUFFIX)?;
        let lock_file = open_lock_file(&lock_path)?;
        let lock = Flock::lock(lock_file, FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| flock_error(error))?;
        verify_named_file(&lock_path, &lock)?;
        let current = load_document(&path, &key, limits)?;
        let key_id_path = suffixed_path(&path, KEY_ID_SUFFIX)?;
        let key_id = bind_key_identity(&key_id_path, &key)?;
        let key_id_file = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&key_id_path)
            .map_err(|_| StateStoreError::Io)?;
        verify_named_file(&key_id_path, &key_id_file)?;
        verify_key_identity(&key_id_path, &key_id)?;
        verify_named_file(&bootstrap_path, &bootstrap)?;
        verify_named_file(&lock_path, &lock)?;
        // The lock and witness names must survive a crash before admission.
        File::open(path.parent().ok_or(StateStoreError::InvalidPath)?)
            .and_then(|dir| dir.sync_all())
            .map_err(|_| StateStoreError::Io)?;
        drop(bootstrap);
        Ok(Self {
            path,
            _lock: lock,
            lock_path,
            key_id_path,
            key_id,
            key_id_file,
            key,
            limits,
            current,
            instance: Arc::new(AttemptInstance),
            attempt_sequence: 0,
            in_flight: HashMap::new(),
            telegram_submitting_chunk: HashMap::new(),
            poisoned: false,
        })
    }

    /// Reviews current state without returning message bodies or destination
    /// secrets. The held file lock prevents another cooperating writer from
    /// changing the file during this store's lifetime.
    pub fn inspect(&self) -> Result<crate::ForwardingQueueSummary, StateStoreError> {
        self.ensure_usable()?;
        let mut summary = self.current.inspect_go_queue()?;
        for task in &mut summary.tasks {
            task.telegram.in_flight = self
                .in_flight
                .contains_key(&(task.id.clone(), ForwardingChannel::Telegram));
            task.bark.in_flight = self
                .in_flight
                .contains_key(&(task.id.clone(), ForwardingChannel::Bark));
        }
        Ok(summary)
    }

    /// Status projection bound to the currently enabled receive identities.
    /// The identities themselves remain private and never enter the summary.
    pub fn inspect_for_bindings(
        &self,
        telegram_binding: Option<&DestinationBinding>,
        bark_binding: Option<&DestinationBinding>,
    ) -> Result<crate::ForwardingQueueSummary, StateStoreError> {
        self.ensure_usable()?;
        let mut summary = crate::schema::inspect_go_queue_with_bindings(
            self.current.as_bytes(),
            telegram_binding,
            bark_binding,
        )?;
        for task in &mut summary.tasks {
            task.telegram.in_flight = self
                .in_flight
                .contains_key(&(task.id.clone(), ForwardingChannel::Telegram));
            task.bark.in_flight = self
                .in_flight
                .contains_key(&(task.id.clone(), ForwardingChannel::Bark));
        }
        Ok(summary)
    }

    /// Returns due queue work in the Go forwarder's stable timestamp/task/channel
    /// order. Pass a binding only when that provider is enabled and has a ready
    /// sender. Legacy Telegram items without frozen chunks are returned with
    /// `needs_telegram_freeze()` set so the runtime must checkpoint the exact
    /// payload before network I/O.
    pub fn due_deliveries(
        &self,
        now: &str,
        telegram_binding: Option<&DestinationBinding>,
        bark_binding: Option<&DestinationBinding>,
    ) -> Result<Vec<crate::ForwardingDelivery>, StateStoreError> {
        self.ensure_usable()?;
        let mut candidates = crate::schema::due_deliveries(
            self.current.as_bytes(),
            now,
            telegram_binding,
            bark_binding,
        )?;
        candidates.retain(|candidate| {
            !self
                .in_flight
                .contains_key(&(candidate.task_id().to_owned(), candidate.channel()))
        });
        Ok(candidates)
    }

    /// Performs Go-compatible deduplication and persists new queue admission
    /// before reporting success. Provider work is never started here. If the
    /// atomic replacement has an uncertain directory-sync outcome, this store
    /// is poisoned and must be reopened before any later mutation.
    pub fn enqueue_and_persist(
        &mut self,
        messages: &[ForwardingMessage],
        received_at: &str,
        options: &EnqueueOptions,
    ) -> Result<EnqueueSummary, StateStoreError> {
        self.ensure_usable()?;
        let (next, summary) = self
            .current
            .enqueue(messages, received_at, options, self.limits)?;
        if summary.newly_seen == 0 {
            return Ok(summary);
        }
        let ciphertext = seal(&self.key, &next, self.limits)?;
        match atomic_replace(&self.path, &ciphertext) {
            Ok(()) => {
                self.current = next;
                Ok(summary)
            }
            Err(ReplaceError::BeforeCommit) => Err(StateStoreError::Io),
            Err(ReplaceError::AfterCommit) => {
                self.poisoned = true;
                Err(StateStoreError::PersistenceUncertain)
            }
        }
    }

    /// Binds one unbound legacy destination to the currently configured
    /// account. Existing identity, terminal state, or a disabled destination
    /// fails closed. The resulting SWF1 replacement is durable before success.
    pub fn bind_legacy_destination(
        &mut self,
        task_id: &str,
        channel: ForwardingChannel,
        enabled: bool,
        binding: &DestinationBinding,
        updated_at: &str,
    ) -> Result<(), QueueMutationError> {
        self.mutate_destination(task_id, channel, true, enabled, binding, updated_at)
    }

    /// Clears retry-blocking configuration/unknown-result markers only when
    /// the queue entry is still bound to the current enabled destination.
    /// Retry counters and frozen Telegram checkpoints are preserved.
    pub fn retry_destination(
        &mut self,
        task_id: &str,
        channel: ForwardingChannel,
        enabled: bool,
        binding: &DestinationBinding,
        updated_at: &str,
    ) -> Result<(), QueueMutationError> {
        self.mutate_destination(task_id, channel, false, enabled, binding, updated_at)
    }

    /// Cancels one explicitly selected destination. If this makes both
    /// destinations terminal, the task is atomically summarized into recent.
    pub fn cancel_destination(
        &mut self,
        task_id: &str,
        channel: ForwardingChannel,
        updated_at: &str,
    ) -> Result<(), QueueMutationError> {
        self.ensure_usable().map_err(|error| match error {
            StateStoreError::ReopenRequired => QueueMutationError::ReopenRequired,
            _ => QueueMutationError::StorageIo,
        })?;
        if self.in_flight.contains_key(&(task_id.to_owned(), channel)) {
            return Err(QueueMutationError::InFlight);
        }
        let next = crate::queue::cancel_destination(
            &self.current,
            task_id,
            channel,
            updated_at,
            self.limits,
        )?;
        self.commit_mutation(next)?;
        // Durable cancellation retires this destination's local submission
        // fence. Another channel's cancellation cannot grant Telegram rights.
        if channel == ForwardingChannel::Telegram {
            self.telegram_submitting_chunk.remove(task_id);
        }
        Ok(())
    }

    /// Durably marks a provider attempt as potentially submitted before the
    /// caller performs network I/O. A crash from this point requires explicit
    /// user resolution instead of silently replaying the notification.
    pub(crate) fn begin_destination_attempt(
        &mut self,
        task_id: &str,
        channel: ForwardingChannel,
        enabled: bool,
        binding: &DestinationBinding,
        attempted_at: &str,
    ) -> Result<AttemptToken, QueueMutationError> {
        self.ensure_usable().map_err(|error| match error {
            StateStoreError::ReopenRequired => QueueMutationError::ReopenRequired,
            _ => QueueMutationError::StorageIo,
        })?;
        let reservation = (task_id.to_owned(), channel);
        if self.in_flight.contains_key(&reservation) {
            return Err(QueueMutationError::InFlight);
        }
        let sequence = self
            .attempt_sequence
            .checked_add(1)
            .ok_or(QueueMutationError::AttemptSequenceExhausted)?;
        let next = crate::queue::begin_destination_attempt(
            &self.current,
            task_id,
            channel,
            enabled,
            binding,
            attempted_at,
            self.limits,
        )?;
        // Burn the checked sequence even if persistence fails; never recycle
        // an instance-local authority after an uncertain durable boundary.
        self.attempt_sequence = sequence;
        self.commit_mutation(next)?;
        let token = AttemptToken {
            instance: self.instance.clone(),
            sequence,
            task_id: task_id.to_owned(),
            channel,
        };
        self.in_flight.insert(reservation, token.clone());
        Ok(token)
    }

    /// Commits an explicit provider result. Unknown outcomes remain paused
    /// without a retry time; the reservation is released only after the result
    /// is durably stored.
    pub(crate) fn finish_destination_attempt(
        &mut self,
        task_id: &str,
        channel: ForwardingChannel,
        binding: &DestinationBinding,
        updated_at: &str,
        outcome: &DeliveryOutcome,
        token: &AttemptToken,
    ) -> Result<(), QueueMutationError> {
        self.ensure_usable().map_err(|error| match error {
            StateStoreError::ReopenRequired => QueueMutationError::ReopenRequired,
            _ => QueueMutationError::StorageIo,
        })?;
        let reservation = (task_id.to_owned(), channel);
        if !self.owns_attempt(task_id, channel, token) {
            return Err(QueueMutationError::InvalidDeliveryResult);
        }
        let next = crate::queue::finish_destination_attempt(
            &self.current,
            task_id,
            channel,
            binding,
            updated_at,
            outcome,
            self.limits,
        )?;
        self.commit_mutation(next)?;
        self.in_flight.remove(&reservation);
        if channel == ForwardingChannel::Telegram {
            self.telegram_submitting_chunk.remove(task_id);
        }
        Ok(())
    }

    /// Freezes Telegram's exact rendered payload before the first provider
    /// request. The caller supplies chunks rendered with the host-local time
    /// line; this method binds their digest to the queued SMS and durably
    /// stores the Go-compatible checkpoint. Existing checkpoints are never
    /// replaced with newly formatted content.
    pub fn freeze_telegram_progress(
        &mut self,
        task_id: &str,
        binding: &DestinationBinding,
        chunks: &[Zeroizing<String>],
    ) -> Result<(), QueueMutationError> {
        self.ensure_usable().map_err(|error| match error {
            StateStoreError::ReopenRequired => QueueMutationError::ReopenRequired,
            _ => QueueMutationError::StorageIo,
        })?;
        if self
            .in_flight
            .contains_key(&(task_id.to_owned(), ForwardingChannel::Telegram))
            || self.telegram_submitting_chunk.contains_key(task_id)
        {
            return Err(QueueMutationError::InFlight);
        }
        let next = crate::queue::freeze_telegram_progress(
            &self.current,
            task_id,
            binding,
            chunks,
            self.limits,
        )?;
        self.commit_mutation(next)
    }

    /// Commits a Telegram per-chunk submission boundary or API acknowledgement.
    /// Acknowledgement is accepted only for the chunk whose submission marker
    /// was durably written by this live store instance.
    pub(crate) fn checkpoint_telegram_chunk(
        &mut self,
        task_id: &str,
        enabled: bool,
        binding: &DestinationBinding,
        index: usize,
        submitting: bool,
        updated_at: &str,
        token: &AttemptToken,
    ) -> Result<(), QueueMutationError> {
        self.ensure_usable().map_err(|error| match error {
            StateStoreError::ReopenRequired => QueueMutationError::ReopenRequired,
            _ => QueueMutationError::StorageIo,
        })?;
        if !self.owns_attempt(task_id, ForwardingChannel::Telegram, token) {
            return Err(QueueMutationError::InvalidTelegramCheckpoint);
        }
        if submitting {
            if self.telegram_submitting_chunk.contains_key(task_id) {
                return Err(QueueMutationError::InvalidTelegramCheckpoint);
            }
        } else if index == 0 || self.telegram_submitting_chunk.get(task_id) != Some(&(index - 1)) {
            return Err(QueueMutationError::InvalidTelegramCheckpoint);
        }
        let next = crate::queue::checkpoint_telegram_chunk(
            &self.current,
            crate::queue::TelegramChunkMutation {
                task_id,
                enabled,
                binding,
                index,
                submitting,
                updated_at,
            },
            self.limits,
        )?;
        self.commit_mutation(next)?;
        if submitting {
            self.telegram_submitting_chunk
                .insert(task_id.to_owned(), index);
        } else {
            self.telegram_submitting_chunk.remove(task_id);
        }
        Ok(())
    }

    /// Retires only this live attempt's volatile owner. Even a poisoned store
    /// can release that right; durable Unknown and poison are never cleared.
    pub(crate) fn release_destination(
        &mut self,
        task_id: &str,
        channel: ForwardingChannel,
        token: &AttemptToken,
    ) -> bool {
        if !self.owns_attempt(task_id, channel, token) {
            return false;
        }
        self.in_flight.remove(&(task_id.to_owned(), channel));
        true
    }

    fn owns_attempt(
        &self,
        task_id: &str,
        channel: ForwardingChannel,
        token: &AttemptToken,
    ) -> bool {
        Arc::ptr_eq(&self.instance, &token.instance)
            && token.task_id == task_id
            && token.channel == channel
            && self.in_flight.get(&(task_id.to_owned(), channel)) == Some(token)
    }

    fn mutate_destination(
        &mut self,
        task_id: &str,
        channel: ForwardingChannel,
        bind_legacy: bool,
        enabled: bool,
        binding: &DestinationBinding,
        updated_at: &str,
    ) -> Result<(), QueueMutationError> {
        self.ensure_usable().map_err(|error| match error {
            StateStoreError::ReopenRequired => QueueMutationError::ReopenRequired,
            _ => QueueMutationError::StorageIo,
        })?;
        if self.in_flight.contains_key(&(task_id.to_owned(), channel)) {
            return Err(QueueMutationError::InFlight);
        }
        let next = crate::queue::mutate_destination(
            &self.current,
            DestinationMutation {
                task_id,
                channel,
                kind: if bind_legacy {
                    DestinationMutationKind::BindLegacy
                } else {
                    DestinationMutationKind::Retry
                },
                enabled,
                current_binding: binding,
                updated_at,
            },
            self.limits,
        )?;
        self.commit_mutation(next)?;
        // Retrying is explicit user authorization. Only its successful durable
        // commit retires a previous Telegram attempt's local chunk marker.
        if !bind_legacy && channel == ForwardingChannel::Telegram {
            self.telegram_submitting_chunk.remove(task_id);
        }
        Ok(())
    }

    fn commit_mutation(&mut self, next: JsonDocument) -> Result<(), QueueMutationError> {
        let ciphertext = seal(&self.key, &next, self.limits).map_err(QueueMutationError::State)?;
        match atomic_replace(&self.path, &ciphertext) {
            Ok(()) => {
                self.current = next;
                Ok(())
            }
            Err(ReplaceError::BeforeCommit) => Err(QueueMutationError::StorageIo),
            Err(ReplaceError::AfterCommit) => {
                self.poisoned = true;
                Err(QueueMutationError::PersistenceUncertain)
            }
        }
    }

    fn ensure_usable(&self) -> Result<(), StateStoreError> {
        #[cfg(feature = "queue-fault-tests")]
        {
            let held = {
                let mut locks = VERIFY_LOCKS.lock().unwrap();
                locks
                    .iter()
                    .position(|(target, _, _)| *target == self.path)
                    .map(|index| locks.remove(index))
            };
            if let Some((_, path, entered)) = held {
                let lock = File::open(path).map_err(|_| StateStoreError::Io)?;
                let _ = entered.send(());
                lock.lock().map_err(|_| StateStoreError::Io)?;
                drop(lock);
            }
        }
        if self.poisoned {
            Err(StateStoreError::ReopenRequired)
        } else {
            verify_named_file(&self.lock_path, &self._lock)?;
            verify_named_file(&self.key_id_path, &self.key_id_file)?;
            verify_key_identity(&self.key_id_path, &self.key_id)
        }
    }
}

fn flock_error(error: Errno) -> StateStoreError {
    if error == Errno::EWOULDBLOCK || error == Errno::EAGAIN {
        StateStoreError::Busy
    } else {
        StateStoreError::Io
    }
}

fn verify_named_file(path: &Path, file: &File) -> Result<(), StateStoreError> {
    let held = file.metadata().map_err(|_| StateStoreError::Io)?;
    let named = fs::symlink_metadata(path).map_err(|_| StateStoreError::Io)?;
    if !held.is_file()
        || held.uid() != nix::unistd::geteuid().as_raw()
        || held.mode() & 0o777 != PRIVATE_FILE_MODE
        || held.nlink() != 1
        || held.dev() != named.dev()
        || held.ino() != named.ino()
        || !named.is_file()
        || named.nlink() != 1
    {
        return Err(StateStoreError::UnsafePermissions);
    }
    Ok(())
}

fn expected_key_identity(key: &StateKey) -> [u8; 40] {
    let mut digest = Sha256::new();
    digest.update(KEY_ID_DOMAIN);
    digest.update(key.as_bytes());
    let mut id = [0u8; 40];
    id[..8].copy_from_slice(KEY_ID_MAGIC);
    id[8..].copy_from_slice(&digest.finalize());
    id
}

fn verify_key_identity(path: &Path, expected: &[u8; 40]) -> Result<(), StateStoreError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| StateStoreError::Io)?;
    verify_named_file(path, &file)?;
    let mut actual = Vec::new();
    Read::by_ref(&mut file)
        .take(41)
        .read_to_end(&mut actual)
        .map_err(|_| StateStoreError::Io)?;
    verify_named_file(path, &file)?;
    if actual.as_slice() != expected {
        return Err(StateStoreError::State(StateError::AuthenticationFailed));
    }
    Ok(())
}

fn bind_key_identity(path: &Path, key: &StateKey) -> Result<[u8; 40], StateStoreError> {
    let expected = expected_key_identity(key);
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    match options.open(path) {
        Ok(mut file) => {
            verify_named_file(path, &file)?;
            file.write_all(&expected)
                .and_then(|_| file.sync_all())
                .map_err(|_| StateStoreError::Io)?;
            File::open(path.parent().ok_or(StateStoreError::InvalidPath)?)
                .and_then(|dir| dir.sync_all())
                .map_err(|_| StateStoreError::Io)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_key_identity(path, &expected)?;
        }
        Err(_) => return Err(StateStoreError::Io),
    }
    Ok(expected)
}

fn normalize_path(path: &Path) -> Result<PathBuf, StateStoreError> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(StateStoreError::InvalidPath);
    }
    let parent = path.parent().ok_or(StateStoreError::InvalidPath)?;
    let parent = fs::canonicalize(parent).map_err(|_| StateStoreError::Io)?;
    let metadata = fs::metadata(&parent).map_err(|_| StateStoreError::Io)?;
    if !metadata.is_dir() {
        return Err(StateStoreError::InvalidPath);
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(StateStoreError::UnsafePermissions);
    }
    Ok(parent.join(path.file_name().ok_or(StateStoreError::InvalidPath)?))
}

fn suffixed_path(path: &Path, suffix: &str) -> Result<PathBuf, StateStoreError> {
    let mut name = path
        .file_name()
        .ok_or(StateStoreError::InvalidPath)?
        .to_os_string();
    name.push(suffix);
    Ok(path.with_file_name(name))
}

fn check_private_file(file: &File) -> Result<(), StateStoreError> {
    let metadata = file.metadata().map_err(|_| StateStoreError::Io)?;
    if !metadata.is_file() {
        return Err(StateStoreError::InvalidPath);
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(StateStoreError::UnsafePermissions);
    }
    Ok(())
}

fn open_lock_file(path: &Path) -> Result<File, StateStoreError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(PRIVATE_FILE_MODE)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    let file = options.open(path).map_err(|_| StateStoreError::Io)?;
    check_private_file(&file)?;
    Ok(file)
}

fn load_document(
    path: &Path,
    key: &StateKey,
    limits: StateLimits,
) -> Result<JsonDocument, StateStoreError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return JsonDocument::from_bytes(Zeroizing::new(INITIAL_QUEUE.to_vec()), limits)
                .map_err(Into::into);
        }
        Err(_) => return Err(StateStoreError::Io),
    };
    check_private_file(&file)?;
    let max_wire = limits
        .max_plaintext_bytes()
        .checked_add(crate::SWF1_OVERHEAD)
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or(StateStoreError::State(StateError::SizeLimit))?;
    let mut wire = Vec::new();
    wire.try_reserve_exact(max_wire.min(1024 * 1024))
        .map_err(|_| StateStoreError::Io)?;
    file.take(max_wire as u64)
        .read_to_end(&mut wire)
        .map_err(|_| StateStoreError::Io)?;
    let document = open(key, &wire, limits)?;
    document.inspect_go_queue()?;
    Ok(document)
}

#[derive(Clone, Copy)]
enum ReplaceError {
    BeforeCommit,
    AfterCommit,
}

#[cfg(test)]
pub(crate) fn panic_next_replace_after_rename_for_test() {
    PANIC_AFTER_QUEUE_RENAME.with(|failure| failure.set(true));
}

#[cfg(any(test, feature = "queue-fault-tests"))]
thread_local! {
    static FAIL_AFTER_QUEUE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "queue-fault-tests"))]
pub(crate) fn fail_next_replace_after_rename_for_test() {
    FAIL_AFTER_QUEUE_RENAME.with(|failure| failure.set(true));
}

#[cfg(any(test, feature = "queue-fault-tests"))]
pub(crate) fn fail_next_replace_before_rename_for_test() {
    FAIL_BEFORE_QUEUE_RENAME.with(|failure| failure.set(true));
}

#[cfg(any(test, feature = "queue-fault-tests"))]
thread_local! {
    static FAIL_BEFORE_QUEUE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PANIC_AFTER_QUEUE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(feature = "queue-fault-tests")]
type ReplaceLock = (PathBuf, PathBuf, std::sync::mpsc::Sender<()>);
#[cfg(feature = "queue-fault-tests")]
static REPLACE_LOCKS: std::sync::Mutex<Vec<ReplaceLock>> = std::sync::Mutex::new(Vec::new());
#[cfg(feature = "queue-fault-tests")]
static VERIFY_LOCKS: std::sync::Mutex<Vec<ReplaceLock>> = std::sync::Mutex::new(Vec::new());
#[cfg(feature = "queue-fault-tests")]
pub(crate) fn hold_next_verification(
    path: PathBuf,
    lock_path: PathBuf,
    entered: std::sync::mpsc::Sender<()>,
) {
    VERIFY_LOCKS
        .lock()
        .unwrap()
        .push((path, lock_path, entered));
}
#[cfg(feature = "queue-fault-tests")]
static FAIL_AFTER_QUEUE_RENAME_PATHS: std::sync::Mutex<Vec<PathBuf>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(feature = "queue-fault-tests")]
pub(crate) fn fail_next_replace_after_rename_for_path(path: PathBuf) {
    FAIL_AFTER_QUEUE_RENAME_PATHS.lock().unwrap().push(path);
}

#[cfg(feature = "queue-fault-tests")]
pub(crate) fn hold_next_replace(
    path: PathBuf,
    lock_path: PathBuf,
    entered: std::sync::mpsc::Sender<()>,
) {
    REPLACE_LOCKS
        .lock()
        .unwrap()
        .push((path, lock_path, entered));
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), ReplaceError> {
    // Path-scoped offline test injection. Production queue flock is nonblocking;
    // this independent real file lock models a slow synchronous disk syscall.
    #[cfg(feature = "queue-fault-tests")]
    {
        let held = {
            let mut locks = REPLACE_LOCKS.lock().unwrap();
            locks
                .iter()
                .position(|(target, _, _)| target == path)
                .map(|i| locks.remove(i))
        };
        if let Some((_, lock_path, entered)) = held {
            let lock = File::open(lock_path).map_err(|_| ReplaceError::BeforeCommit)?;
            let _ = entered.send(());
            lock.lock().map_err(|_| ReplaceError::BeforeCommit)?;
            // Release immediately after the independent std thread unlocks.
            drop(lock);
        }
    }

    let parent = path.parent().ok_or(ReplaceError::BeforeCommit)?;
    let name = path.file_name().ok_or(ReplaceError::BeforeCommit)?;
    let mut temp_path = PathBuf::new();
    let mut temp_file = None;
    for _ in 0..16 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name: OsString = name.to_os_string();
        temp_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
        temp_path = parent.join(temp_name);
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
        match options.open(&temp_path) {
            Ok(file) => {
                temp_file = Some(file);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(ReplaceError::BeforeCommit),
        }
    }
    let Some(mut temp_file) = temp_file else {
        return Err(ReplaceError::BeforeCommit);
    };
    if temp_file.write_all(bytes).is_err() || temp_file.sync_all().is_err() {
        drop(temp_file);
        let _ = fs::remove_file(&temp_path);
        return Err(ReplaceError::BeforeCommit);
    }
    if temp_file
        .set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .is_err()
    {
        drop(temp_file);
        let _ = fs::remove_file(&temp_path);
        return Err(ReplaceError::BeforeCommit);
    }
    if temp_file.sync_all().is_err() {
        drop(temp_file);
        let _ = fs::remove_file(&temp_path);
        return Err(ReplaceError::BeforeCommit);
    }
    drop(temp_file);
    #[cfg(any(test, feature = "queue-fault-tests"))]
    if FAIL_BEFORE_QUEUE_RENAME.with(|failure| failure.replace(false)) {
        let _ = fs::remove_file(&temp_path);
        return Err(ReplaceError::BeforeCommit);
    }
    if fs::rename(&temp_path, path).is_err() {
        let _ = fs::remove_file(&temp_path);
        return Err(ReplaceError::BeforeCommit);
    }
    #[cfg(feature = "queue-fault-tests")]
    {
        let mut failures = FAIL_AFTER_QUEUE_RENAME_PATHS.lock().unwrap();
        if let Some(index) = failures.iter().position(|candidate| candidate == path) {
            failures.remove(index);
            return Err(ReplaceError::AfterCommit);
        }
    }
    #[cfg(test)]
    if PANIC_AFTER_QUEUE_RENAME.with(|failure| failure.replace(false)) {
        panic!("synthetic unwind after actual queue rename");
    }
    #[cfg(any(test, feature = "queue-fault-tests"))]
    if FAIL_AFTER_QUEUE_RENAME.with(|failure| failure.replace(false)) {
        return Err(ReplaceError::AfterCommit);
    }
    let directory = match File::open(parent) {
        Ok(directory) => directory,
        Err(_) => return Err(ReplaceError::AfterCommit),
    };
    if directory.sync_all().is_err() {
        return Err(ReplaceError::AfterCommit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DestinationBinding;
    use sha2::{Digest, Sha256};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEST_ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let switch_root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .expect("Switch root from Cargo manifest");
            let parent = switch_root.join("build/tmp/forwarding-store-tests");
            fs::create_dir_all(&parent).unwrap();
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            for _ in 0..64 {
                let sequence = TEST_ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let root = parent.join(format!("{}-{nonce}-{sequence}", std::process::id()));
                match fs::create_dir(&root) {
                    Ok(()) => return Self(root),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create isolated test root: {error}"),
                }
            }
            panic!("could not create unique isolated test root")
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn key() -> StateKey {
        StateKey::from_slice(&(0..32).map(|byte| byte as u8).collect::<Vec<_>>()).unwrap()
    }

    fn options() -> EnqueueOptions {
        let state_key = key();
        let telegram =
            DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let bark = DestinationBinding::from_go_identity(
            &state_key,
            "bark\0https://api.day.app\0synthetic-device",
        );
        EnqueueOptions::new(true, &telegram, true, &bark)
    }

    fn message(id: &str) -> ForwardingMessage {
        ForwardingMessage::new(
            id,
            "incoming",
            "65075",
            "synthetic queue persistence test",
            "2026-01-02T03:04:05Z",
            "synthetic reader",
        )
        .unwrap()
    }

    fn limits() -> StateLimits {
        StateLimits::default()
    }

    fn checkpoint_seed(binding: &DestinationBinding) -> JsonDocument {
        let message = r#"{"id":"checkpoint-fixture","direction":"incoming","sender":"65075","content":"first second","timestamp":"2026-01-02T03:04:05Z","device":"fixture reader"}"#;
        let mut message_hash = Sha256::new();
        message_hash.update(message.as_bytes());
        let message_digest = format!("{:x}", message_hash.finalize());
        let chunks = ["first", "second"];
        let chunks_json = serde_json::to_string(&chunks).unwrap();
        let mut chunks_hash = Sha256::new();
        chunks_hash.update(chunks_json.as_bytes());
        let content_digest = format!("{:x}", chunks_hash.finalize());
        let task_id = checkpoint_task_id();
        let raw = format!(
            r#"{{"seen":{{}},"pending":{{"{task_id}":{{"message":{message},"updated_at":"2026-01-02T03:04:05Z","telegram_done":false,"bark_done":true,"telegram_try":0,"telegram_at":"0001-01-01T00:00:00Z","telegram_meta":{{"target_identity":"{}","last_attempt_at":"0001-01-01T00:00:00Z"}},"bark_meta":{{"not_requested":true}},"telegram_progress":{{"format_version":1,"message_digest":"{message_digest}","content_digest":"{content_digest}","chunks":{chunks_json},"confirmed":0}}}}}}}}"#,
            binding.to_hex()
        );
        JsonDocument::from_bytes(Zeroizing::new(raw.into_bytes()), limits()).unwrap()
    }

    fn checkpoint_task_id() -> String {
        format!("{:x}", Sha256::digest(b"checkpoint-fixture"))
    }

    fn golden_wire() -> Vec<u8> {
        include_str!("../tests/fixtures/go-state.swf1.hex")
            .trim()
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    fn seed_golden(path: &Path) {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(PRIVATE_FILE_MODE);
        options
            .open(path)
            .unwrap()
            .write_all(&golden_wire())
            .unwrap();
    }

    fn seed_checkpoint_store(path: &Path, binding: &DestinationBinding) -> QueueStore {
        let mut file = OpenOptions::new();
        file.write(true).create_new(true).mode(PRIVATE_FILE_MODE);
        file.open(path)
            .unwrap()
            .write_all(&seal(&key(), &checkpoint_seed(binding), limits()).unwrap())
            .unwrap();
        QueueStore::open(path, key(), limits()).unwrap()
    }

    // Exercise the real store entry points and compare both volatile authority
    // and encrypted persistence; a stale callback must have no effect on either.
    fn assert_rejected_attempt(
        store: &mut QueueStore,
        task_id: &str,
        binding: &DestinationBinding,
        token: &AttemptToken,
        ack: usize,
    ) {
        let disk = fs::read(&store.path).unwrap();
        let current = store.current.as_bytes().to_vec();
        let owners = store.in_flight.clone();
        let submitting = store.telegram_submitting_chunk.clone();
        assert!(!store.release_destination(task_id, ForwardingChannel::Telegram, token));
        assert_eq!(
            store.checkpoint_telegram_chunk(
                task_id,
                true,
                binding,
                ack,
                false,
                "2026-01-02T04:05:20Z",
                token,
            ),
            Err(QueueMutationError::InvalidTelegramCheckpoint)
        );
        assert_eq!(
            store.finish_destination_attempt(
                task_id,
                ForwardingChannel::Telegram,
                binding,
                "2026-01-02T04:05:20Z",
                &DeliveryOutcome::Delivered,
                token,
            ),
            Err(QueueMutationError::InvalidDeliveryResult)
        );
        assert_eq!(fs::read(&store.path).unwrap(), disk);
        assert_eq!(store.current.as_bytes(), current);
        assert_eq!(store.in_flight, owners);
        assert_eq!(store.telegram_submitting_chunk, submitting);
    }

    #[test]
    fn atomic_admission_reopens_and_duplicate_does_not_rewrite() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        {
            let mut store = QueueStore::open(&path, key(), limits()).unwrap();
            assert!(!path.exists(), "opening an absent queue must not write it");
            assert!(
                path.with_file_name("queue.swf1.lock").exists(),
                "Rust must claim its sibling lock before the first queue write"
            );
            let summary = store
                .enqueue_and_persist(
                    &[message("persistent-admission-001")],
                    "2026-01-02T03:04:06Z",
                    &options(),
                )
                .unwrap();
            assert_eq!(summary.queued, 1);
            assert_eq!(summary.newly_seen, 1);
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(store.inspect().unwrap().tasks.len(), 1);
        }

        let original = fs::read(&path).unwrap();
        {
            let mut store = QueueStore::open(&path, key(), limits()).unwrap();
            assert_eq!(store.inspect().unwrap().tasks.len(), 1);
            let summary = store
                .enqueue_and_persist(
                    &[message("persistent-admission-001")],
                    "2026-01-02T03:04:07Z",
                    &options(),
                )
                .unwrap();
            assert_eq!(summary.newly_seen, 0);
        }
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn uncertain_commit_requires_reopen_and_retry_deduplicates_real_swf1_record() {
        let root = TestRoot::new();
        let path = root.0.join("uncertain-queue.swf1");
        let id = "nwait:profile:message-after-rename";
        let mut store = QueueStore::open(&path, key(), limits()).unwrap();
        FAIL_AFTER_QUEUE_RENAME.with(|failure| failure.set(true));
        assert_eq!(
            store
                .enqueue_and_persist(&[message(id)], "2026-01-02T03:04:06Z", &options(),)
                .unwrap_err(),
            StateStoreError::PersistenceUncertain
        );
        assert_eq!(
            store
                .enqueue_and_persist(&[message(id)], "2026-01-02T03:04:07Z", &options(),)
                .unwrap_err(),
            StateStoreError::ReopenRequired
        );

        let committed = fs::read(&path).unwrap();
        drop(store);
        let mut reopened = QueueStore::open(&path, key(), limits()).unwrap();
        let summary = reopened.inspect().unwrap();
        assert_eq!(summary.tasks.len(), 1);
        assert_eq!(
            summary.tasks[0].id,
            format!("{:x}", Sha256::digest(id.as_bytes()))
        );
        let retry = reopened
            .enqueue_and_persist(&[message(id)], "2026-01-02T03:04:08Z", &options())
            .unwrap();
        assert_eq!(retry.newly_seen, 0);
        assert_eq!(reopened.inspect().unwrap().tasks.len(), 1);
        assert_eq!(fs::read(&path).unwrap(), committed);
    }

    #[test]
    fn due_delivery_snapshot_respects_current_bindings_and_attempt_reservations() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let state_key = key();
        let telegram =
            DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let bark = DestinationBinding::from_go_identity(
            &state_key,
            "bark\0https://api.day.app\0synthetic-device",
        );
        let mut store = QueueStore::open(&path, state_key, limits()).unwrap();
        store
            .enqueue_and_persist(
                &[message("due-snapshot-test")],
                "2026-01-02T03:04:06Z",
                &options(),
            )
            .unwrap();
        let task_id = store.inspect().unwrap().tasks[0].id.clone();

        let ready = store
            .due_deliveries("2026-01-02T03:04:07Z", Some(&telegram), Some(&bark))
            .unwrap();
        assert_eq!(ready.len(), 2);
        assert_eq!(ready[0].task_id(), task_id);
        assert_eq!(ready[0].channel(), ForwardingChannel::Telegram);
        assert!(ready[0].needs_telegram_freeze());
        assert_eq!(ready[1].channel(), ForwardingChannel::Bark);

        let _token = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T03:04:08Z",
            )
            .unwrap();
        let after_reservation = store
            .due_deliveries("2026-01-02T03:04:09Z", Some(&telegram), Some(&bark))
            .unwrap();
        assert_eq!(after_reservation.len(), 1);
        assert_eq!(after_reservation[0].channel(), ForwardingChannel::Bark);

        let changed = DestinationBinding::telegram(&key(), "changed-token", "synthetic-chat");
        let wrong_account = store
            .due_deliveries("2026-01-02T03:04:09Z", Some(&changed), None)
            .unwrap();
        assert!(wrong_account.is_empty());
    }

    #[test]
    fn telegram_payload_freeze_is_durable_and_never_reformatted() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let state_key = key();
        let telegram =
            DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let bark = DestinationBinding::from_go_identity(
            &state_key,
            "bark\0https://api.day.app\0synthetic-device",
        );
        let options = EnqueueOptions::new(true, &telegram, false, &bark);
        let mut store = QueueStore::open(&path, state_key, limits()).unwrap();
        store
            .enqueue_and_persist(
                &[ForwardingMessage::new(
                    "freeze-fixture",
                    "incoming",
                    "65075",
                    "first second",
                    "2026-01-02T03:04:05Z",
                    "fixture reader",
                )
                .unwrap()],
                "2026-01-02T03:04:06Z",
                &options,
            )
            .unwrap();
        let task_id = store.inspect().unwrap().tasks[0].id.clone();
        let due = store
            .due_deliveries("2026-01-02T03:04:07Z", Some(&telegram), None)
            .unwrap();
        assert_eq!(due.len(), 1);
        assert!(due[0].needs_telegram_freeze());

        let rendered = crate::telegram_chunks(&crate::ForwardedSms {
            sender: due[0].message().sender(),
            device: due[0].message().device(),
            timestamp_local: Some("2026-01-02 03:04:05"),
            content: due[0].message().content(),
        });
        store
            .freeze_telegram_progress(&task_id, &telegram, &rendered)
            .unwrap();

        let checkpoint = store.inspect().unwrap().tasks[0]
            .telegram
            .telegram_checkpoint
            .clone()
            .unwrap();
        assert!(checkpoint.chunk_shape_valid);
        assert!(checkpoint.digests_valid);
        assert_eq!(checkpoint.confirmed_parts, 0);
        let frozen = store
            .due_deliveries("2026-01-02T03:04:08Z", Some(&telegram), None)
            .unwrap();
        assert!(!frozen[0].needs_telegram_freeze());
        assert_eq!(frozen[0].confirmed_chunks(), 0);
        assert_eq!(
            frozen[0].telegram_chunks().unwrap()[0].as_str(),
            "发件人：65075\n线路：fixture reader\n时间：2026-01-02 03:04:05\n\nfirst second"
        );
        assert_eq!(
            store.freeze_telegram_progress(&task_id, &telegram, &rendered),
            Err(QueueMutationError::InvalidTelegramCheckpoint)
        );
    }

    #[test]
    fn lock_excludes_a_second_writer_and_releases_on_drop() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let first = QueueStore::open(&path, key(), limits()).unwrap();
        assert_eq!(
            QueueStore::open(&path, key(), limits()).unwrap_err(),
            StateStoreError::Busy
        );
        drop(first);
        assert!(QueueStore::open(&path, key(), limits()).is_ok());
    }

    #[test]
    fn wrong_key_corruption_and_broad_permissions_fail_closed() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let mut store = QueueStore::open(&path, key(), limits()).unwrap();
        store
            .enqueue_and_persist(
                &[message("persistent-admission-002")],
                "2026-01-02T03:04:06Z",
                &options(),
            )
            .unwrap();
        drop(store);

        assert!(matches!(
            QueueStore::open(&path, StateKey::from_slice(&[7; 32]).unwrap(), limits()),
            Err(StateStoreError::State(StateError::AuthenticationFailed))
        ));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            QueueStore::open(&path, key(), limits()).unwrap_err(),
            StateStoreError::UnsafePermissions
        );
    }

    #[test]
    fn empty_first_open_binds_key_before_admission_and_survives_restart() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let first = QueueStore::open(&path, key(), limits()).unwrap();
        assert!(!path.exists());
        assert!(root.0.join("queue.swf1.lock").exists());
        assert!(root.0.join("queue.swf1.keyid").exists());
        drop(first);
        assert!(matches!(
            QueueStore::open(&path, StateKey::from_slice(&[7; 32]).unwrap(), limits()),
            Err(StateStoreError::State(StateError::AuthenticationFailed))
        ));
        assert!(QueueStore::open(&path, key(), limits()).is_ok());
    }

    #[test]
    fn precommitted_key_identity_rejects_stale_configure_before_admission() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let witness = root.0.join("queue.swf1.keyid");
        bind_key_identity(&witness, &StateKey::from_slice(&[7; 32]).unwrap()).unwrap();
        assert!(matches!(
            QueueStore::open(&path, key(), limits()),
            Err(StateStoreError::State(StateError::AuthenticationFailed))
        ));
        assert!(!path.exists());
        assert!(QueueStore::open(&path, StateKey::from_slice(&[7; 32]).unwrap(), limits()).is_ok());
    }

    #[test]
    fn bootstrap_lock_and_witness_reject_symlinks_and_hardlinks() {
        for suffix in [BOOTSTRAP_LOCK_SUFFIX, KEY_ID_SUFFIX] {
            for is_symlink in [true, false] {
                let root = TestRoot::new();
                let path = root.0.join("queue.swf1");
                let target = root.0.join("outside");
                fs::write(&target, b"unrelated synthetic bytes").unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
                let entry = suffixed_path(&path, suffix).unwrap();
                if is_symlink {
                    std::os::unix::fs::symlink(&target, &entry).unwrap();
                } else {
                    fs::hard_link(&target, &entry).unwrap();
                }
                assert!(QueueStore::open(&path, key(), limits()).is_err());
                assert_eq!(fs::read(&target).unwrap(), b"unrelated synthetic bytes");
                assert!(!path.exists());
            }
        }
    }

    #[test]
    fn replacing_named_writer_or_witness_after_open_revokes_read_and_admission() {
        for suffix in [STATE_LOCK_SUFFIX, KEY_ID_SUFFIX] {
            let root = TestRoot::new();
            let path = root.0.join("queue.swf1");
            let mut store = QueueStore::open(&path, key(), limits()).unwrap();
            let named = suffixed_path(&path, suffix).unwrap();
            let bytes = fs::read(&named).unwrap();
            let old = root.0.join("original-entry");
            fs::rename(&named, &old).unwrap();
            let mut replacement = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&named)
                .unwrap();
            replacement.write_all(&bytes).unwrap();
            replacement.sync_all().unwrap();
            File::open(&root.0).unwrap().sync_all().unwrap();
            assert_eq!(
                store.inspect().unwrap_err(),
                StateStoreError::UnsafePermissions
            );
            assert_eq!(
                store
                    .due_deliveries("2026-01-02T03:04:06Z", None, None)
                    .unwrap_err(),
                StateStoreError::UnsafePermissions
            );
            assert_eq!(
                store
                    .enqueue_and_persist(
                        &[message("must-not-admit-after-replacement")],
                        "2026-01-02T03:04:06Z",
                        &options()
                    )
                    .unwrap_err(),
                StateStoreError::UnsafePermissions
            );
            assert!(
                !path.exists(),
                "replaced authority must not create ciphertext"
            );
        }
    }

    #[test]
    fn final_component_symlink_is_not_followed() {
        let root = TestRoot::new();
        let target = root.0.join("outside-target");
        fs::write(&target, b"unrelated synthetic bytes").unwrap();
        let path = root.0.join("queue.swf1");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(QueueStore::open(&path, key(), limits()).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"unrelated synthetic bytes");
    }

    #[test]
    fn shared_writable_parent_is_rejected_before_creating_a_lock() {
        let root = TestRoot::new();
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o777)).unwrap();
        let path = root.0.join("queue.swf1");
        assert_eq!(
            QueueStore::open(&path, key(), limits()).unwrap_err(),
            StateStoreError::UnsafePermissions
        );
        assert!(!root.0.join("queue.swf1.lock").exists());
    }

    #[test]
    fn legacy_bind_and_same_destination_retry_persist_without_changing_other_work() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        seed_golden(&path);
        let state_key = key();
        let telegram = DestinationBinding::telegram(&state_key, "fixture-token", "fixture-chat");
        let mut store = QueueStore::open(&path, state_key, limits()).unwrap();

        store
            .bind_legacy_destination(
                "fixture-legacy",
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            )
            .unwrap();
        let token = store
            .begin_destination_attempt(
                "fixture-legacy",
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            )
            .unwrap();
        assert_eq!(
            store.begin_destination_attempt(
                "fixture-legacy",
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            ),
            Err(QueueMutationError::InFlight)
        );
        assert_eq!(
            store.retry_destination(
                "fixture-legacy",
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:07Z",
            ),
            Err(QueueMutationError::InFlight)
        );
        assert!(!store.release_destination("fixture-legacy", ForwardingChannel::Bark, &token));
        assert!(store.release_destination("fixture-legacy", ForwardingChannel::Telegram, &token));
        store
            .retry_destination(
                "fixture-legacy",
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:07Z",
            )
            .unwrap();

        let summary = store.inspect().unwrap();
        let legacy = summary
            .tasks
            .iter()
            .find(|task| task.id == "fixture-legacy")
            .unwrap();
        assert!(legacy.telegram.destination_bound);
        assert!(!legacy.telegram.completed);
        assert!(legacy.bark.completed);
        assert_eq!(summary.recent_diagnostics, 1);

        let changed_binding = DestinationBinding::telegram(&key(), "changed-token", "fixture-chat");
        assert_eq!(
            store.retry_destination(
                "fixture-legacy",
                ForwardingChannel::Telegram,
                true,
                &changed_binding,
                "2026-01-02T04:05:08Z",
            ),
            Err(QueueMutationError::DestinationNotCurrent)
        );
        assert_eq!(
            store.bind_legacy_destination(
                "fixture-legacy",
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:08Z",
            ),
            Err(QueueMutationError::LegacyDestinationConfirmationNotApplicable)
        );
        assert_eq!(
            store.bind_legacy_destination(
                "fixture-legacy",
                ForwardingChannel::Telegram,
                false,
                &telegram,
                "2026-01-02T04:05:08Z",
            ),
            Err(QueueMutationError::LegacyDestinationConfirmationNotApplicable)
        );
        assert_eq!(
            store.retry_destination(
                "does-not-exist",
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:08Z",
            ),
            Err(QueueMutationError::TaskNotFound)
        );
    }

    #[test]
    fn provider_attempt_unknown_is_durable_and_success_clears_only_its_channel() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let state_key = key();
        let telegram =
            DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let task_id;
        {
            let mut store = QueueStore::open(&path, state_key, limits()).unwrap();
            store
                .enqueue_and_persist(
                    &[message("provider-attempt-transaction")],
                    "2026-01-02T04:05:06Z",
                    &options(),
                )
                .unwrap();
            task_id = store.inspect().unwrap().tasks[0].id.clone();
            let _token = store
                .begin_destination_attempt(
                    &task_id,
                    ForwardingChannel::Telegram,
                    true,
                    &telegram,
                    "2026-01-02T04:05:07Z",
                )
                .unwrap();
            let pending = store.inspect().unwrap();
            assert_eq!(pending.tasks[0].telegram.attempts, 1);
            assert!(pending.tasks[0].telegram.result_unknown);
            assert_eq!(pending.tasks[0].bark.attempts, 0);
            assert!(!pending.tasks[0].bark.completed);
            assert_eq!(
                store.begin_destination_attempt(
                    &task_id,
                    ForwardingChannel::Telegram,
                    true,
                    &telegram,
                    "2026-01-02T04:05:08Z",
                ),
                Err(QueueMutationError::InFlight)
            );
        }

        {
            let mut store = QueueStore::open(&path, key(), limits()).unwrap();
            let recovered = store.inspect().unwrap();
            assert!(recovered.tasks[0].telegram.result_unknown);
            assert_eq!(recovered.tasks[0].telegram.attempts, 1);
            assert_eq!(
                store.begin_destination_attempt(
                    &task_id,
                    ForwardingChannel::Telegram,
                    true,
                    &telegram,
                    "2026-01-02T04:05:09Z",
                ),
                Err(QueueMutationError::DestinationPaused)
            );
            store
                .retry_destination(
                    &task_id,
                    ForwardingChannel::Telegram,
                    true,
                    &telegram,
                    "2026-01-02T04:05:10Z",
                )
                .unwrap();
            let token = store
                .begin_destination_attempt(
                    &task_id,
                    ForwardingChannel::Telegram,
                    true,
                    &telegram,
                    "2026-01-02T04:05:11Z",
                )
                .unwrap();
            store
                .finish_destination_attempt(
                    &task_id,
                    ForwardingChannel::Telegram,
                    &telegram,
                    "2026-01-02T04:05:12Z",
                    &DeliveryOutcome::Delivered,
                    &token,
                )
                .unwrap();
            let completed = store.inspect().unwrap();
            assert!(completed.tasks[0].telegram.completed);
            assert!(!completed.tasks[0].telegram.result_unknown);
            assert_eq!(completed.tasks[0].telegram.attempts, 2);
            assert!(!completed.tasks[0].bark.completed);
            assert_eq!(completed.tasks[0].bark.attempts, 0);
        }
    }

    #[test]
    fn provider_failure_requires_safe_code_and_unknown_results_have_no_retry_time() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let state_key = key();
        let bark = DestinationBinding::from_go_identity(
            &state_key,
            "bark\0https://api.day.app\0synthetic-device",
        );
        let mut store = QueueStore::open(&path, state_key, limits()).unwrap();
        store
            .enqueue_and_persist(
                &[message("provider-failure-transaction")],
                "2026-01-02T04:05:06Z",
                &options(),
            )
            .unwrap();
        let task_id = store.inspect().unwrap().tasks[0].id.clone();
        let token = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Bark,
                true,
                &bark,
                "2026-01-02T04:05:07Z",
            )
            .unwrap();
        let unsafe_result = DeliveryOutcome::Failed {
            error_code: "sensitive-response-body".to_owned(),
            permanent: false,
            result_unknown: true,
            retry_at: None,
        };
        assert_eq!(
            store.finish_destination_attempt(
                &task_id,
                ForwardingChannel::Bark,
                &bark,
                "2026-01-02T04:05:08Z",
                &unsafe_result,
                &token,
            ),
            Err(QueueMutationError::InvalidDeliveryResult)
        );
        let unknown = DeliveryOutcome::Failed {
            error_code: "bark_response_invalid".to_owned(),
            permanent: false,
            result_unknown: true,
            retry_at: None,
        };
        store
            .finish_destination_attempt(
                &task_id,
                ForwardingChannel::Bark,
                &bark,
                "2026-01-02T04:05:08Z",
                &unknown,
                &token,
            )
            .unwrap();
        let summary = store.inspect().unwrap();
        assert!(summary.tasks[0].bark.result_unknown);
        assert_eq!(summary.tasks[0].bark.attempts, 1);
        assert_eq!(summary.tasks[0].telegram.attempts, 0);
    }

    #[test]
    fn terminal_completion_and_cancel_archive_once_without_sms_bodies() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let state_key = key();
        let telegram =
            DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let mut store = QueueStore::open(&path, state_key, limits()).unwrap();
        store
            .enqueue_and_persist(
                &[ForwardingMessage::new(
                    "terminal-archive-fixture",
                    "incoming",
                    "65075",
                    "synthetic private SMS body",
                    "2026-01-02T03:04:05Z",
                    "synthetic reader",
                )
                .unwrap()],
                "2026-01-02T04:05:06Z",
                &options(),
            )
            .unwrap();
        let task_id = store.inspect().unwrap().tasks[0].id.clone();
        let token = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:07Z",
            )
            .unwrap();
        store
            .finish_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                &telegram,
                "2026-01-02T04:05:08Z",
                &DeliveryOutcome::Delivered,
                &token,
            )
            .unwrap();
        assert_eq!(store.inspect().unwrap().tasks.len(), 1);
        store
            .cancel_destination(&task_id, ForwardingChannel::Bark, "2026-01-02T04:05:09Z")
            .unwrap();

        let summary = store.inspect().unwrap();
        assert!(summary.tasks.is_empty());
        assert_eq!(summary.recent_diagnostics, 1);
        let state: serde_json::Value = serde_json::from_slice(store.current.as_bytes()).unwrap();
        let recent = state["recent"][0].as_object().unwrap();
        assert_eq!(recent["id"], task_id);
        assert_eq!(recent["telegram"]["state"], "delivered");
        assert_eq!(recent["telegram"]["attempts"], 1);
        assert_eq!(recent["bark"]["state"], "cancelled");
        assert_eq!(recent["bark"]["attempts"], 0);
        assert_eq!(recent["updated_at"], "2026-01-02T04:05:09Z");
        assert!(!state.to_string().contains("synthetic private SMS body"));
    }

    #[test]
    fn terminal_recent_history_keeps_only_the_latest_100_diagnostics() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let state_key = key();
        let telegram =
            DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let bark = DestinationBinding::from_go_identity(
            &state_key,
            "bark\0https://api.day.app\0synthetic-device",
        );
        let options = EnqueueOptions::new(true, &telegram, false, &bark);
        let mut store = QueueStore::open(&path, state_key, limits()).unwrap();
        let mut task_ids = Vec::new();

        for index in 0..101 {
            store
                .enqueue_and_persist(
                    &[ForwardingMessage::new(
                        format!("bounded-terminal-{index}"),
                        "incoming",
                        "65075",
                        "synthetic private SMS body",
                        "2026-01-02T03:04:05Z",
                        "synthetic reader",
                    )
                    .unwrap()],
                    "2026-01-02T04:05:06Z",
                    &options,
                )
                .unwrap();
            let task_id = store
                .inspect()
                .unwrap()
                .tasks
                .into_iter()
                .find(|task| !task_ids.contains(&task.id))
                .expect("newly enqueued task")
                .id;
            task_ids.push(task_id.clone());
            let token = store
                .begin_destination_attempt(
                    &task_id,
                    ForwardingChannel::Telegram,
                    true,
                    &telegram,
                    "2026-01-02T04:05:07Z",
                )
                .unwrap();
            store
                .finish_destination_attempt(
                    &task_id,
                    ForwardingChannel::Telegram,
                    &telegram,
                    "2026-01-02T04:05:08Z",
                    &DeliveryOutcome::Delivered,
                    &token,
                )
                .unwrap();
        }

        let summary = store.inspect().unwrap();
        assert_eq!(
            summary.recent_diagnostics,
            100,
            "pending={}",
            summary.tasks.len()
        );
        let state: serde_json::Value = serde_json::from_slice(store.current.as_bytes()).unwrap();
        let recent = state["recent"].as_array().unwrap();
        assert_eq!(recent.len(), 100);
        assert_eq!(recent.first().unwrap()["id"], task_ids[1]);
        assert_eq!(recent.last().unwrap()["id"], task_ids[100]);
        assert!(state["pending"].as_object().unwrap().is_empty());
        assert!(!state.to_string().contains("synthetic private SMS body"));
    }

    #[test]
    fn telegram_chunk_checkpoints_are_ordered_durable_and_resume_after_confirmation() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let state_key = key();
        let telegram =
            DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let task_id = checkpoint_task_id();
        let seeded = checkpoint_seed(&telegram);
        let mut seed = OpenOptions::new();
        seed.write(true).create_new(true).mode(PRIVATE_FILE_MODE);
        seed.open(&path)
            .unwrap()
            .write_all(&seal(&state_key, &seeded, limits()).unwrap())
            .unwrap();

        {
            let mut store = QueueStore::open(&path, state_key, limits()).unwrap();
            let token = store
                .begin_destination_attempt(
                    &task_id,
                    ForwardingChannel::Telegram,
                    true,
                    &telegram,
                    "2026-01-02T04:05:06Z",
                )
                .unwrap();
            assert_eq!(
                store.checkpoint_telegram_chunk(
                    &task_id,
                    false,
                    &telegram,
                    0,
                    true,
                    "2026-01-02T04:05:06Z",
                    &token,
                ),
                Err(QueueMutationError::DestinationNotCurrent)
            );
            assert_eq!(
                store.checkpoint_telegram_chunk(
                    &task_id,
                    true,
                    &telegram,
                    1,
                    false,
                    "2026-01-02T04:05:07Z",
                    &token,
                ),
                Err(QueueMutationError::InvalidTelegramCheckpoint)
            );
            store
                .checkpoint_telegram_chunk(
                    &task_id,
                    true,
                    &telegram,
                    0,
                    true,
                    "2026-01-02T04:05:08Z",
                    &token,
                )
                .unwrap();
            assert_eq!(
                store.checkpoint_telegram_chunk(
                    &task_id,
                    true,
                    &telegram,
                    0,
                    true,
                    "2026-01-02T04:05:09Z",
                    &token,
                ),
                Err(QueueMutationError::InvalidTelegramCheckpoint)
            );
            store
                .checkpoint_telegram_chunk(
                    &task_id,
                    true,
                    &telegram,
                    1,
                    false,
                    "2026-01-02T04:05:10Z",
                    &token,
                )
                .unwrap();
            let summary = store.inspect().unwrap();
            let progress = summary.tasks[0]
                .telegram
                .telegram_checkpoint
                .as_ref()
                .unwrap();
            assert_eq!(progress.confirmed_parts, 1);
            assert!(!summary.tasks[0].telegram.result_unknown);
        }

        // Reopening simulates a process restart after a durable chunk ACK.
        // Explicit retry resumes at part 2 and cannot acknowledge part 1 again.
        let mut store = QueueStore::open(&path, key(), limits()).unwrap();
        store
            .retry_destination(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:11Z",
            )
            .unwrap();
        let token = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:12Z",
            )
            .unwrap();
        assert_eq!(
            store.checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                0,
                true,
                "2026-01-02T04:05:13Z",
                &token,
            ),
            Err(QueueMutationError::InvalidTelegramCheckpoint)
        );
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                1,
                true,
                "2026-01-02T04:05:14Z",
                &token,
            )
            .unwrap();
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                2,
                false,
                "2026-01-02T04:05:15Z",
                &token,
            )
            .unwrap();
        store
            .finish_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                &telegram,
                "2026-01-02T04:05:16Z",
                &DeliveryOutcome::Delivered,
                &token,
            )
            .unwrap();
        let summary = store.inspect().unwrap();
        assert!(summary.tasks.is_empty());
        assert_eq!(summary.recent_diagnostics, 1);
        let state: serde_json::Value = serde_json::from_slice(store.current.as_bytes()).unwrap();
        assert_eq!(state["recent"][0]["telegram"]["confirmed_parts"], 2);
        assert_eq!(state["recent"][0]["telegram"]["total_parts"], 2);
    }

    #[test]
    fn explicit_retry_retires_only_durable_success_and_stale_callbacks_cannot_touch_new_attempt() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let telegram = DestinationBinding::telegram(&key(), "synthetic-token", "synthetic-chat");
        let task_id = checkpoint_task_id();
        let mut store = seed_checkpoint_store(&path, &telegram);
        let old = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            )
            .unwrap();
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                0,
                true,
                "2026-01-02T04:05:07Z",
                &old,
            )
            .unwrap();
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                1,
                false,
                "2026-01-02T04:05:08Z",
                &old,
            )
            .unwrap();
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                1,
                true,
                "2026-01-02T04:05:09Z",
                &old,
            )
            .unwrap();
        let durable_progress = store.inspect().unwrap().tasks[0]
            .telegram
            .telegram_checkpoint
            .clone();
        let frozen = serde_json::from_slice::<serde_json::Value>(store.current.as_bytes()).unwrap()
            ["pending"][&task_id]["telegram_progress"]
            .clone();
        let before_in_flight_retry = fs::read(&path).unwrap();
        assert_eq!(
            store.retry_destination(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:10Z",
            ),
            Err(QueueMutationError::InFlight)
        );
        assert_eq!(fs::read(&path).unwrap(), before_in_flight_retry);
        assert!(store.release_destination(&task_id, ForwardingChannel::Telegram, &old));
        assert_eq!(
            store.begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:10Z",
            ),
            Err(QueueMutationError::DestinationPaused)
        );

        let before = fs::read(&path).unwrap();
        let current = store.current.as_bytes().to_vec();
        FAIL_BEFORE_QUEUE_RENAME.with(|failure| failure.set(true));
        assert_eq!(
            store.retry_destination(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:11Z",
            ),
            Err(QueueMutationError::StorageIo)
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(store.current.as_bytes(), current);
        assert_eq!(store.telegram_submitting_chunk.get(&task_id), Some(&1));
        assert!(store.inspect().unwrap().tasks[0].telegram.result_unknown);
        assert!(store.in_flight.is_empty());

        store
            .retry_destination(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:12Z",
            )
            .unwrap();
        assert!(!store.telegram_submitting_chunk.contains_key(&task_id));
        let summary = store.inspect().unwrap();
        assert_eq!(summary.tasks[0].telegram.attempts, 1);
        assert!(!summary.tasks[0].telegram.result_unknown);
        let progress = summary.tasks[0]
            .telegram
            .telegram_checkpoint
            .as_ref()
            .unwrap();
        assert_eq!(progress.confirmed_parts, 1);
        assert_eq!(progress.total_parts, durable_progress.unwrap().total_parts);
        let after = serde_json::from_slice::<serde_json::Value>(store.current.as_bytes()).unwrap();
        assert_eq!(after["pending"][&task_id]["telegram_progress"], frozen);
        let token = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:13Z",
            )
            .unwrap();
        assert_ne!(token, old);
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                1,
                true,
                "2026-01-02T04:05:14Z",
                &token,
            )
            .unwrap();
        assert_rejected_attempt(&mut store, &task_id, &telegram, &old, 2);
        assert_rejected_attempt(&mut store, "wrong-task", &telegram, &token, 2);
        assert!(!store.release_destination(&task_id, ForwardingChannel::Bark, &token));
        let disk = fs::read(&path).unwrap();
        assert_eq!(
            store.finish_destination_attempt(
                &task_id,
                ForwardingChannel::Bark,
                &telegram,
                "2026-01-02T04:05:15Z",
                &DeliveryOutcome::Delivered,
                &token,
            ),
            Err(QueueMutationError::InvalidDeliveryResult)
        );
        assert_eq!(fs::read(&path).unwrap(), disk);
        // A cloned capability represents the same owner and is valid.
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                2,
                false,
                "2026-01-02T04:05:16Z",
                &token.clone(),
            )
            .unwrap();
        store
            .finish_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                &telegram,
                "2026-01-02T04:05:17Z",
                &DeliveryOutcome::Delivered,
                &token,
            )
            .unwrap();
        assert!(!store.release_destination(&task_id, ForwardingChannel::Telegram, &old));
        assert!(!store.release_destination(&task_id, ForwardingChannel::Telegram, &token));
        let state: serde_json::Value = serde_json::from_slice(store.current.as_bytes()).unwrap();
        assert_eq!(state["recent"][0]["telegram"]["attempts"], 2);
        assert_eq!(state["recent"][0]["telegram"]["confirmed_parts"], 2);
    }

    #[test]
    fn attempt_tokens_reject_foreign_and_reopened_stores_even_with_same_sequence() {
        let root = TestRoot::new();
        let first_path = root.0.join("first.swf1");
        let other_path = root.0.join("other.swf1");
        let telegram = DestinationBinding::telegram(&key(), "synthetic-token", "synthetic-chat");
        let task_id = checkpoint_task_id();
        let mut first = seed_checkpoint_store(&first_path, &telegram);
        let mut other = seed_checkpoint_store(&other_path, &telegram);
        let old = first
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            )
            .unwrap();
        let foreign = other
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            )
            .unwrap();
        assert_eq!(old.sequence, foreign.sequence);
        assert_ne!(old, foreign);
        assert_rejected_attempt(&mut first, &task_id, &telegram, &foreign, 1);
        assert_rejected_attempt(&mut other, &task_id, &telegram, &old, 1);
        assert!(first.release_destination(&task_id, ForwardingChannel::Telegram, &old));
        assert!(first.inspect().unwrap().tasks[0].telegram.result_unknown);
        drop(first);
        let mut reopened = QueueStore::open(&first_path, key(), limits()).unwrap();
        assert_eq!(
            reopened.begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:07Z",
            ),
            Err(QueueMutationError::DestinationPaused)
        );
        reopened
            .retry_destination(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:08Z",
            )
            .unwrap();
        let fresh = reopened
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:09Z",
            )
            .unwrap();
        assert_eq!(fresh.sequence, old.sequence);
        assert!(!Arc::ptr_eq(&fresh.instance, &old.instance));
        assert_rejected_attempt(&mut reopened, &task_id, &telegram, &old, 1);
        reopened
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                0,
                true,
                "2026-01-02T04:05:10Z",
                &fresh,
            )
            .unwrap();
        assert!(reopened.release_destination(&task_id, ForwardingChannel::Telegram, &fresh));
        assert!(other.release_destination(&task_id, ForwardingChannel::Telegram, &foreign));
    }

    #[test]
    fn checked_attempt_sequence_exhaustion_never_wraps_or_changes_persistence() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let telegram = DestinationBinding::telegram(&key(), "synthetic-token", "synthetic-chat");
        let task_id = checkpoint_task_id();
        let mut store = seed_checkpoint_store(&path, &telegram);
        store.attempt_sequence = u64::MAX - 1;
        let last = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            )
            .unwrap();
        assert_eq!(last.sequence, u64::MAX);
        assert!(store.release_destination(&task_id, ForwardingChannel::Telegram, &last));
        store
            .retry_destination(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:07Z",
            )
            .unwrap();
        let disk = fs::read(&path).unwrap();
        let current = store.current.as_bytes().to_vec();
        assert_eq!(
            store.begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:08Z",
            ),
            Err(QueueMutationError::AttemptSequenceExhausted)
        );
        assert_eq!(store.attempt_sequence, u64::MAX);
        assert!(store.in_flight.is_empty());
        assert!(store.telegram_submitting_chunk.is_empty());
        assert_eq!(store.current.as_bytes(), current);
        assert_eq!(fs::read(&path).unwrap(), disk);
        assert_eq!(store.inspect().unwrap().tasks[0].telegram.attempts, 1);
    }

    #[test]
    fn failed_durable_begin_never_issues_owner_and_never_reuses_sequence() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let telegram = DestinationBinding::telegram(&key(), "synthetic-token", "synthetic-chat");
        let task_id = checkpoint_task_id();
        let mut store = seed_checkpoint_store(&path, &telegram);
        let disk = fs::read(&path).unwrap();
        FAIL_BEFORE_QUEUE_RENAME.with(|failure| failure.set(true));
        assert_eq!(
            store.begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            ),
            Err(QueueMutationError::StorageIo)
        );
        assert_eq!(store.attempt_sequence, 1);
        assert_eq!(fs::read(&path).unwrap(), disk);
        assert!(store.in_flight.is_empty());
        assert_eq!(store.inspect().unwrap().tasks[0].telegram.attempts, 0);
        let token = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:07Z",
            )
            .unwrap();
        assert_eq!(token.sequence, 2);
        assert!(store.release_destination(&task_id, ForwardingChannel::Telegram, &token));
        store
            .retry_destination(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:08Z",
            )
            .unwrap();
        FAIL_AFTER_QUEUE_RENAME.with(|failure| failure.set(true));
        assert_eq!(
            store.begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:09Z",
            ),
            Err(QueueMutationError::PersistenceUncertain)
        );
        assert_eq!(store.attempt_sequence, 3);
        assert!(store.in_flight.is_empty());
        assert!(store.telegram_submitting_chunk.is_empty());
        assert_eq!(
            store.inspect().unwrap_err(),
            StateStoreError::ReopenRequired
        );
        assert_eq!(
            store.begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:10Z",
            ),
            Err(QueueMutationError::ReopenRequired)
        );
        drop(store);
        let reopened = QueueStore::open(&path, key(), limits()).unwrap();
        let summary = reopened.inspect().unwrap();
        assert_eq!(summary.tasks[0].telegram.attempts, 2);
        assert!(summary.tasks[0].telegram.result_unknown);
        assert!(!summary.tasks[0].telegram.in_flight);
    }

    #[test]
    fn postrename_unwind_owner_explicitly_requires_reopen_preserving_disk_and_token_release() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let telegram = DestinationBinding::telegram(&key(), "synthetic-token", "synthetic-chat");
        let task_id = checkpoint_task_id();
        let mut store = seed_checkpoint_store(&path, &telegram);
        let token = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            )
            .unwrap();
        let current_before = store.current.as_bytes().to_vec();
        let disk_before = fs::read(&path).unwrap();
        PANIC_AFTER_QUEUE_RENAME.with(|failure| failure.set(true));
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                store
                    .enqueue_and_persist(
                        &[message("postrename-new-task")],
                        "2026-01-02T04:05:07Z",
                        &options(),
                    )
                    .unwrap();
            }))
            .is_err()
        );
        // Actual rename happened, but this is NOT automatic QueueStore unwind
        // recovery: the caller explicitly performs the owner fence below.
        assert_eq!(store.current.as_bytes(), current_before);
        let disk_after = fs::read(&path).unwrap();
        assert_ne!(disk_after, disk_before);
        store.require_reopen();
        store.require_reopen();
        assert_eq!(store.inspect(), Err(StateStoreError::ReopenRequired));
        assert_eq!(
            store.enqueue_and_persist(
                &[message("must-not-overwrite")],
                "2026-01-02T04:05:08Z",
                &options()
            ),
            Err(StateStoreError::ReopenRequired)
        );
        assert!(store.release_destination(&task_id, ForwardingChannel::Telegram, &token));
        assert!(!store.release_destination(&task_id, ForwardingChannel::Telegram, &token));
        assert!(
            store.current.inspect_go_queue().unwrap().tasks[0]
                .telegram
                .result_unknown
        );
        assert_eq!(store.current.as_bytes(), current_before);
        assert_eq!(fs::read(&path).unwrap(), disk_after);
        drop(store);
        let reopened = QueueStore::open(&path, key(), limits()).unwrap();
        let summary = reopened.inspect().unwrap();
        assert_eq!(summary.tasks.len(), 2);
        assert!(
            summary
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .unwrap()
                .telegram
                .result_unknown
        );
        let wire = open(&key(), &disk_after, limits()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(wire.as_bytes()).unwrap();
        assert!(
            json["pending"]
                .as_object()
                .unwrap()
                .values()
                .any(|task| task["message"]["id"] == "postrename-new-task")
        );
        assert!(
            !json["pending"]
                .as_object()
                .unwrap()
                .values()
                .any(|task| task["message"]["id"] == "must-not-overwrite")
        );
    }

    #[test]
    fn uncertain_retry_keeps_local_fence_until_reopen_without_replaying_authorization() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let telegram = DestinationBinding::telegram(&key(), "synthetic-token", "synthetic-chat");
        let task_id = checkpoint_task_id();
        let mut store = seed_checkpoint_store(&path, &telegram);
        let old = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            )
            .unwrap();
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                0,
                true,
                "2026-01-02T04:05:07Z",
                &old,
            )
            .unwrap();
        assert!(store.release_destination(&task_id, ForwardingChannel::Telegram, &old));
        let previous = store.current.as_bytes().to_vec();
        FAIL_AFTER_QUEUE_RENAME.with(|failure| failure.set(true));
        assert_eq!(
            store.retry_destination(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:08Z",
            ),
            Err(QueueMutationError::PersistenceUncertain)
        );
        assert_eq!(store.current.as_bytes(), previous);
        assert_eq!(store.telegram_submitting_chunk.get(&task_id), Some(&0));
        assert!(store.in_flight.is_empty());
        assert_eq!(
            store.inspect().unwrap_err(),
            StateStoreError::ReopenRequired
        );
        assert_eq!(
            store.retry_destination(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:09Z",
            ),
            Err(QueueMutationError::ReopenRequired)
        );
        assert_eq!(store.telegram_submitting_chunk.get(&task_id), Some(&0));
        drop(store);
        let mut reopened = QueueStore::open(&path, key(), limits()).unwrap();
        // The explicit authorization may already be in SWF1. Reopen observes it;
        // do not repeat the user action or pretend it was never committed.
        assert!(!reopened.inspect().unwrap().tasks[0].telegram.result_unknown);
        assert!(reopened.telegram_submitting_chunk.is_empty());
        let token = reopened
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:10Z",
            )
            .unwrap();
        assert_rejected_attempt(&mut reopened, &task_id, &telegram, &old, 1);
        reopened
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                0,
                true,
                "2026-01-02T04:05:11Z",
                &token,
            )
            .unwrap();
        // Even after an uncertain checkpoint, release can retire exactly its owner
        // but cannot unpoison the store or clear the local submission fence.
        FAIL_AFTER_QUEUE_RENAME.with(|failure| failure.set(true));
        assert_eq!(
            reopened.checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                1,
                false,
                "2026-01-02T04:05:12Z",
                &token,
            ),
            Err(QueueMutationError::PersistenceUncertain)
        );
        assert!(reopened.release_destination(&task_id, ForwardingChannel::Telegram, &token));
        assert!(!reopened.release_destination(&task_id, ForwardingChannel::Telegram, &token));
        assert_eq!(reopened.telegram_submitting_chunk.get(&task_id), Some(&0));
        assert_eq!(
            reopened.inspect().unwrap_err(),
            StateStoreError::ReopenRequired
        );
    }

    #[test]
    fn durable_telegram_cancel_retires_only_its_marker_and_preserves_terminal_diagnostics() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let telegram = DestinationBinding::telegram(&key(), "synthetic-token", "synthetic-chat");
        let mut store = QueueStore::open(&path, key(), limits()).unwrap();
        store
            .enqueue_and_persist(
                &[message("cancel-marker")],
                "2026-01-02T04:05:06Z",
                &options(),
            )
            .unwrap();
        let task_id = store.inspect().unwrap().tasks[0].id.clone();
        let chunks = [
            Zeroizing::new("first".to_owned()),
            Zeroizing::new("second".to_owned()),
        ];
        store
            .freeze_telegram_progress(&task_id, &telegram, &chunks)
            .unwrap();
        let token = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:07Z",
            )
            .unwrap();
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                0,
                true,
                "2026-01-02T04:05:08Z",
                &token,
            )
            .unwrap();
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                1,
                false,
                "2026-01-02T04:05:08Z",
                &token,
            )
            .unwrap();
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                1,
                true,
                "2026-01-02T04:05:08Z",
                &token,
            )
            .unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(
            store.cancel_destination(
                &task_id,
                ForwardingChannel::Telegram,
                "2026-01-02T04:05:09Z",
            ),
            Err(QueueMutationError::InFlight)
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(store.telegram_submitting_chunk.get(&task_id), Some(&1));
        assert!(store.release_destination(&task_id, ForwardingChannel::Telegram, &token));
        store
            .cancel_destination(&task_id, ForwardingChannel::Bark, "2026-01-02T04:05:10Z")
            .unwrap();
        assert_eq!(store.telegram_submitting_chunk.get(&task_id), Some(&1));
        let summary = store.inspect().unwrap();
        assert!(summary.tasks[0].telegram.result_unknown);
        assert_eq!(summary.tasks[0].telegram.attempts, 1);
        let progress = summary.tasks[0]
            .telegram
            .telegram_checkpoint
            .as_ref()
            .unwrap();
        assert_eq!(progress.confirmed_parts, 1);
        assert_eq!(progress.total_parts, 2);
        store
            .cancel_destination(
                &task_id,
                ForwardingChannel::Telegram,
                "2026-01-02T04:05:11Z",
            )
            .unwrap();
        assert!(!store.telegram_submitting_chunk.contains_key(&task_id));
        assert!(store.in_flight.is_empty());
        let state: serde_json::Value = serde_json::from_slice(store.current.as_bytes()).unwrap();
        assert!(state["pending"].as_object().unwrap().is_empty());
        assert_eq!(state["recent"][0]["telegram"]["state"], "cancelled");
        assert_eq!(state["recent"][0]["bark"]["state"], "cancelled");
        assert_eq!(state["recent"][0]["telegram"]["attempts"], 1);
        assert_eq!(state["recent"][0]["telegram"]["confirmed_parts"], 1);
        assert_eq!(state["recent"][0]["telegram"]["total_parts"], 2);
        assert!(!store.release_destination(&task_id, ForwardingChannel::Telegram, &token));
        drop(store);
        let reopened = QueueStore::open(&path, key(), limits()).unwrap();
        assert!(reopened.inspect().unwrap().tasks.is_empty());
        assert_eq!(reopened.inspect().unwrap().recent_diagnostics, 1);
    }

    #[test]
    fn failed_or_uncertain_telegram_cancel_preserves_local_submission_fence() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let telegram = DestinationBinding::telegram(&key(), "synthetic-token", "synthetic-chat");
        let task_id = checkpoint_task_id();
        let mut store = seed_checkpoint_store(&path, &telegram);
        let token = store
            .begin_destination_attempt(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &telegram,
                "2026-01-02T04:05:06Z",
            )
            .unwrap();
        store
            .checkpoint_telegram_chunk(
                &task_id,
                true,
                &telegram,
                0,
                true,
                "2026-01-02T04:05:07Z",
                &token,
            )
            .unwrap();
        assert!(store.release_destination(&task_id, ForwardingChannel::Telegram, &token));
        let disk = fs::read(&path).unwrap();
        let current = store.current.as_bytes().to_vec();
        FAIL_BEFORE_QUEUE_RENAME.with(|failure| failure.set(true));
        assert_eq!(
            store.cancel_destination(
                &task_id,
                ForwardingChannel::Telegram,
                "2026-01-02T04:05:08Z",
            ),
            Err(QueueMutationError::StorageIo)
        );
        assert_eq!(fs::read(&path).unwrap(), disk);
        assert_eq!(store.current.as_bytes(), current);
        assert_eq!(store.telegram_submitting_chunk.get(&task_id), Some(&0));
        FAIL_AFTER_QUEUE_RENAME.with(|failure| failure.set(true));
        assert_eq!(
            store.cancel_destination(
                &task_id,
                ForwardingChannel::Telegram,
                "2026-01-02T04:05:09Z",
            ),
            Err(QueueMutationError::PersistenceUncertain)
        );
        assert_eq!(store.current.as_bytes(), current);
        assert_eq!(store.telegram_submitting_chunk.get(&task_id), Some(&0));
        assert_eq!(
            store.inspect().unwrap_err(),
            StateStoreError::ReopenRequired
        );
        assert_eq!(
            store.cancel_destination(
                &task_id,
                ForwardingChannel::Telegram,
                "2026-01-02T04:05:10Z",
            ),
            Err(QueueMutationError::ReopenRequired)
        );
        assert_eq!(store.telegram_submitting_chunk.get(&task_id), Some(&0));
        drop(store);
        let reopened = QueueStore::open(&path, key(), limits()).unwrap();
        assert!(reopened.inspect().unwrap().tasks.is_empty());
        assert!(reopened.telegram_submitting_chunk.is_empty());
    }
}

#[cfg(any(test, feature = "producer-admission-candidate"))]
#[path = "queue_store/producer_admission.rs"]
mod producer_admission;
