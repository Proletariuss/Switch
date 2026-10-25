//! Compatibility with the Go SMS forwarder's SWF1 encrypted state envelope.
//!
//! This library implements the SWF1 envelope, Go-compatible admission,
//! explicitly authorized queue-state transactions, Telegram chunk checkpoints,
//! provider adapters, and the durable worker used by the production queue
//! owner. The caller obtains configuration and key material through the
//! approved local API and must
//! never interpret durable queue admission as permission to replay an
//! uncertain provider submission.

#![forbid(unsafe_code)]

mod bark_crypto;
#[cfg(any(test, feature = "queue-format-v2-candidate"))]
pub mod format_v2;
#[cfg(any(test, feature = "producer-admission-candidate"))]
pub mod producer_admission;
mod provider;
mod queue;
mod queue_store;
#[cfg(any(test, feature = "queue-format-v2-candidate"))]
pub mod root_preflight;
mod schema;
mod target_binding;
mod worker;

/// Explicitly enabled by the daemon's dev-dependency only. No environment or
/// HTTP switch can arm this hook in a production build.
#[cfg(feature = "queue-fault-tests")]
#[doc(hidden)]
pub mod test_support {
    pub fn hold_next_replace(
        path: std::path::PathBuf,
        lock_path: std::path::PathBuf,
        entered: std::sync::mpsc::Sender<()>,
    ) {
        crate::queue_store::hold_next_replace(path, lock_path, entered);
    }

    /// Inject a one-time post-rename durability fault for one queue path.
    /// This follows queue I/O when the daemon moves it to a blocking worker.
    pub fn fail_next_replace_after_rename_for_path(path: std::path::PathBuf) {
        crate::queue_store::fail_next_replace_after_rename_for_path(path);
    }

    /// Fail the next queue replacement on this thread after its real rename.
    /// The queue remains poisoned until its owner closes and reopens it.
    pub fn fail_next_replace_before_rename() {
        crate::queue_store::fail_next_replace_before_rename_for_test();
    }
    #[cfg(feature = "producer-admission-candidate")]
    pub fn fail_progress_before_rename(store: &crate::producer_admission::progress::ProgressStore) {
        store.fail_commit_at_for_test(3);
    }
    #[cfg(feature = "producer-admission-candidate")]
    pub fn fail_progress_after_rename(store: &crate::producer_admission::progress::ProgressStore) {
        store.fail_commit_at_for_test(4);
    }
    pub fn fail_next_replace_after_rename() {
        crate::queue_store::fail_next_replace_after_rename_for_test();
    }
}

pub use bark_crypto::{BarkEncryptionAlgorithm, BarkEncryptionError, encrypt_bark_payload};
pub use provider::{
    BarkSender, DeliveryDisposition, DeliveryFailure, ForwardedSms, ProxyMode,
    TELEGRAM_CHUNK_LIMIT, TelegramSender, telegram_chunks,
};
pub use queue::{
    DeliveryOutcome, EnqueueOptions, EnqueueSummary, ForwardingChannel, ForwardingDelivery,
    ForwardingMessage, QueueMutationError,
};
pub use queue_store::{QueueStore, StateStoreError, seal_empty_queue};
pub use schema::{
    ForwardingDestinationSummary, ForwardingQueueSummary, ForwardingRecentChannelSummary,
    ForwardingRecentTaskSummary, ForwardingTaskSummary, TelegramCheckpointSummary,
};
pub use target_binding::DestinationBinding;
pub use worker::{
    ForwardingClock, ForwardingWorker, SystemForwardingClock, WorkerError, WorkerSummary,
};

use aes_gcm::{AeadInOut, Aes256Gcm, KeyInit, aead::Nonce};
use serde::{Deserialize, de::IgnoredAny};
use zeroize::Zeroizing;

const MAGIC: &[u8; 4] = b"SWF1";
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const OVERHEAD: usize = MAGIC.len() + NONCE_BYTES + TAG_BYTES;

/// Defensive allocation ceiling, not a size field or limit in the Go format.
/// Larger legacy queues must be explicitly handled before any live takeover;
/// rejecting them must never create a new empty queue.
pub const MAX_PLAINTEXT_BYTES: usize = 64 * 1024 * 1024;
/// Resource policy for opaque JSON validation, including the root object.
pub const MAX_JSON_DEPTH: usize = 128;
const SWF1_OVERHEAD: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateError {
    InvalidKeyLength,
    InvalidLimit,
    SizeLimit,
    AllocationFailed,
    UnsupportedFormat,
    Truncated,
    AuthenticationFailed,
    InvalidJsonObject,
    NestingLimit,
    InvalidStateSchema,
    EntropyUnavailable,
    EncryptionFailed,
    InvalidTargetIdentity,
    DuplicateJsonKey,
    InvalidMessage,
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidKeyLength => "SWF1 key must contain exactly 32 bytes",
            Self::InvalidLimit => "invalid SWF1 plaintext size limit",
            Self::SizeLimit => "SWF1 state exceeds the configured size limit",
            Self::AllocationFailed => "cannot allocate SWF1 state buffer",
            Self::UnsupportedFormat => "unsupported encrypted forwarding state format",
            Self::Truncated => "encrypted forwarding state is truncated",
            Self::AuthenticationFailed => "forwarding state authentication failed",
            Self::InvalidJsonObject => "forwarding state is not a complete UTF-8 JSON object",
            Self::NestingLimit => "forwarding state JSON exceeds the nesting limit",
            Self::InvalidStateSchema => "forwarding state does not match the Go queue schema",
            Self::EntropyUnavailable => "system cryptographic entropy unavailable",
            Self::EncryptionFailed => "forwarding state encryption failed",
            Self::InvalidTargetIdentity => "forwarding destination identity is invalid",
            Self::DuplicateJsonKey => "forwarding state contains a duplicate JSON key",
            Self::InvalidMessage => "SMS message is invalid for queue admission",
        })
    }
}

impl std::error::Error for StateError {}

/// In-memory key; no serialization, cloning, logging or storage integration.
/// The owned copy is zeroized on drop. A caller remains responsible for any
/// source buffer passed to [`StateKey::from_slice`].
pub struct StateKey(Zeroizing<[u8; 32]>);

impl StateKey {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, StateError> {
        if bytes.len() != 32 {
            return Err(StateError::InvalidKeyLength);
        }
        let mut key = Zeroizing::new([0; 32]);
        key.copy_from_slice(bytes);
        Ok(Self(key))
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for StateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StateKey([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateLimits {
    max_plaintext_bytes: usize,
}

impl StateLimits {
    pub fn new(max_plaintext_bytes: usize) -> Result<Self, StateError> {
        if !(1..=MAX_PLAINTEXT_BYTES).contains(&max_plaintext_bytes) {
            return Err(StateError::InvalidLimit);
        }
        Ok(Self {
            max_plaintext_bytes,
        })
    }

    fn check(self, bytes: usize) -> Result<(), StateError> {
        if bytes > self.max_plaintext_bytes {
            Err(StateError::SizeLimit)
        } else {
            Ok(())
        }
    }

    pub(crate) fn max_plaintext_bytes(self) -> usize {
        self.max_plaintext_bytes
    }
}

impl Default for StateLimits {
    fn default() -> Self {
        Self {
            max_plaintext_bytes: MAX_PLAINTEXT_BYTES,
        }
    }
}

/// An opaque JSON object. Read-only projection preserves the complete original
/// bytes; queue admission splices map entries while retaining existing raw
/// fields. Syntactically valid but semantically invalid queue state can still
/// be held here until an operation explicitly validates it. The owned
/// plaintext buffer is zeroized on drop, including error paths.
pub struct JsonDocument(Zeroizing<Vec<u8>>);

impl JsonDocument {
    /// Consumes an already protected buffer, including when validation fails.
    pub fn from_bytes(bytes: Zeroizing<Vec<u8>>, limits: StateLimits) -> Result<Self, StateError> {
        limits.check(bytes.len())?;
        validate_json_object(&bytes)?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Reads the known Go queue fields into a redacted, non-mutable summary.
    /// Unknown fields and original JSON bytes remain in this document. Known
    /// Telegram digests are checked, but this does not authorize provider work.
    pub fn inspect_go_queue(&self) -> Result<ForwardingQueueSummary, StateError> {
        schema::inspect_go_queue(self.as_bytes())
    }

    /// Adds incoming messages to the legacy Go-compatible `seen` and `pending`
    /// maps without contacting a provider. Existing JSON values, including
    /// unknown fields and frozen delivery checkpoints, are retained verbatim.
    pub fn enqueue(
        &self,
        messages: &[ForwardingMessage],
        received_at: &str,
        options: &EnqueueOptions,
        limits: StateLimits,
    ) -> Result<(Self, EnqueueSummary), StateError> {
        queue::enqueue(self, messages, received_at, options, limits)
    }
}

impl std::fmt::Debug for JsonDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JsonDocument([REDACTED])")
    }
}

fn validate_json_object(bytes: &[u8]) -> Result<(), StateError> {
    let text = std::str::from_utf8(bytes).map_err(|_| StateError::InvalidJsonObject)?;
    if text
        .trim_start_matches([' ', '\t', '\r', '\n'])
        .as_bytes()
        .first()
        != Some(&b'{')
    {
        return Err(StateError::InvalidJsonObject);
    }
    // IgnoredAny's iterative parser does not impose the normal serde nesting
    // limit. Bound nesting before it grows the scratch stack; bracket characters
    // inside strings (including escaped quotes) do not change nesting depth.
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'[' | b'{' => {
                    depth += 1;
                    if depth > MAX_JSON_DEPTH {
                        return Err(StateError::NestingLimit);
                    }
                }
                b']' | b'}' => {
                    depth = depth.checked_sub(1).ok_or(StateError::InvalidJsonObject)?;
                }
                _ => {}
            }
        }
    }
    // Validate the complete syntax without copying SMS bodies into a Value
    // tree. No fields are interpreted or discarded from the original buffer.
    let mut decoder = serde_json::Deserializer::from_str(text);
    IgnoredAny::deserialize(&mut decoder).map_err(|_| StateError::InvalidJsonObject)?;
    decoder.end().map_err(|_| StateError::InvalidJsonObject)
}

/// Authenticates an entire SWF1 envelope before exposing any plaintext.
/// Missing/truncated/corrupt inputs fail closed; there is no empty-state fallback.
pub fn open(key: &StateKey, data: &[u8], limits: StateLimits) -> Result<JsonDocument, StateError> {
    if data.len() > limits.max_plaintext_bytes + OVERHEAD {
        return Err(StateError::SizeLimit);
    }
    if data.len() < MAGIC.len() {
        return Err(StateError::Truncated);
    }
    if &data[..MAGIC.len()] != MAGIC {
        return Err(StateError::UnsupportedFormat);
    }
    if data.len() < OVERHEAD {
        return Err(StateError::Truncated);
    }
    let mut buffer = protected_buffer(data.len() - MAGIC.len() - NONCE_BYTES)?;
    buffer.extend_from_slice(&data[MAGIC.len() + NONCE_BYTES..]);
    let nonce: Nonce<Aes256Gcm> = data[MAGIC.len()..MAGIC.len() + NONCE_BYTES]
        .try_into()
        .expect("length checked above");
    let cipher = Aes256Gcm::new_from_slice(key.0.as_ref()).expect("fixed 32-byte key");
    cipher
        .decrypt_in_place(&nonce, MAGIC, &mut *buffer)
        .map_err(|_| StateError::AuthenticationFailed)?;
    JsonDocument::from_bytes(buffer, limits)
}

/// Encrypts unchanged JSON using a fresh OS-generated 96-bit nonce. The public
/// API deliberately provides no caller-selected nonce. It has no file effects.
pub fn seal(
    key: &StateKey,
    document: &JsonDocument,
    limits: StateLimits,
) -> Result<Vec<u8>, StateError> {
    seal_with_entropy(key, document, limits, getrandom::fill)
}

fn seal_with_entropy<E>(
    key: &StateKey,
    document: &JsonDocument,
    limits: StateLimits,
    entropy: impl FnOnce(&mut [u8]) -> Result<(), E>,
) -> Result<Vec<u8>, StateError> {
    limits.check(document.0.len())?;
    let mut nonce = [0; NONCE_BYTES];
    entropy(&mut nonce).map_err(|_| StateError::EntropyUnavailable)?;
    let mut buffer = protected_buffer(document.0.len() + TAG_BYTES)?;
    buffer.extend_from_slice(&document.0);
    let cipher = Aes256Gcm::new_from_slice(key.0.as_ref()).expect("fixed 32-byte key");
    cipher
        .encrypt_in_place(&nonce.into(), MAGIC, &mut *buffer)
        .map_err(|_| StateError::EncryptionFailed)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(document.0.len() + OVERHEAD)
        .map_err(|_| StateError::AllocationFailed)?;
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&buffer);
    Ok(output)
}

fn protected_buffer(capacity: usize) -> Result<Zeroizing<Vec<u8>>, StateError> {
    let mut bytes = Zeroizing::new(Vec::new());
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| StateError::AllocationFailed)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "root-bootstrap-candidate"))]
pub mod root_bootstrap;
