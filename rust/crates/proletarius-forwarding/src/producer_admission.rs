//! Isolated B2 foundation. No production initializer or progress-ACK issuer.
//! Existing SWF1 files without authority metadata are refused, never migrated.
use crate::{JsonDocument, StateError, StateStoreError};
use serde::{Deserialize, Serialize};

pub mod progress;

pub(crate) const EXTENSION: &str = "rust_producer_admission_v1";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerBinding {
    pub queue_id: String,
    pub source_id: String,
    pub handoff_id: String,
}
impl ProducerBinding {
    pub(crate) fn valid(&self) -> bool {
        [&self.queue_id, &self.source_id, &self.handoff_id]
            .iter()
            .all(|v| {
                !v.is_empty()
                    && v.len() <= 128
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
            })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum PageDisposition {
    Deliver,
    DisabledSkip,
    ResumeBaseline,
}
#[derive(Clone, Copy, Debug)]
pub struct PageRequest {
    pub expected_completed_sequence: u64,
    pub from_cursor: u64,
    pub to_cursor: u64,
    pub disposition: PageDisposition,
}
/// Queue-issued receipt. External callers may inspect it but cannot deserialize
/// arbitrary JSON into authority to advance progress.
///
/// ```
/// use proletarius_forwarding::producer_admission::PageReceipt;
/// let _ = std::mem::size_of::<PageReceipt>();
/// ```
/// ```compile_fail
/// use proletarius_forwarding::producer_admission::PageReceipt;
/// let _: PageReceipt = serde_json::from_str("{}").unwrap();
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PageReceipt {
    pub(crate) sequence: u64,
    pub(crate) receipt_id: String,
    pub(crate) from_cursor: u64,
    pub(crate) to_cursor: u64,
    pub(crate) disposition: PageDisposition,
    pub(crate) page_digest: String,
    pub(crate) newly_seen: usize,
    pub(crate) queued: usize,
}
// Private on-disk DTO: Deserialize must never be implemented for the public
// opaque PageReceipt itself. Only a validated queue owner may issue it.
mod active_receipt_wire {
    use super::*;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StoredReceipt {
        sequence: u64,
        receipt_id: String,
        from_cursor: u64,
        to_cursor: u64,
        disposition: PageDisposition,
        page_digest: String,
        newly_seen: usize,
        queued: usize,
    }
    pub fn serialize<S: serde::Serializer>(
        value: &Option<PageReceipt>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<PageReceipt>, D::Error> {
        Ok(
            Option::<StoredReceipt>::deserialize(deserializer)?.map(|r| PageReceipt {
                sequence: r.sequence,
                receipt_id: r.receipt_id,
                from_cursor: r.from_cursor,
                to_cursor: r.to_cursor,
                disposition: r.disposition,
                page_digest: r.page_digest,
                newly_seen: r.newly_seen,
                queued: r.queued,
            }),
        )
    }
}
impl PageReceipt {
    pub(crate) fn expected_id(&self, binding: &ProducerBinding) -> Option<String> {
        use sha2::{Digest, Sha256};
        serde_json::to_vec(&(
            binding,
            self.sequence,
            self.from_cursor,
            self.to_cursor,
            self.disposition,
            &self.page_digest,
            self.newly_seen,
            self.queued,
        ))
        .ok()
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
    }
    pub(crate) fn valid_for(&self, binding: &ProducerBinding) -> bool {
        self.expected_id(binding).as_deref() == Some(self.receipt_id.as_str())
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn id(&self) -> &str {
        &self.receipt_id
    }
    pub fn from_cursor(&self) -> u64 {
        self.from_cursor
    }
    pub fn to_cursor(&self) -> u64 {
        self.to_cursor
    }
    pub fn newly_seen(&self) -> usize {
        self.newly_seen
    }
    pub fn queued(&self) -> usize {
        self.queued
    }
}
/// Opaque until a future schema2 progress store can issue a durable ACK.
/// There is deliberately no production constructor or serde decoder.
pub struct ProgressAck {
    pub(crate) receipt: PageReceipt,
}
impl ProgressAck {
    #[cfg(test)]
    pub(crate) fn for_fixture(receipt: &PageReceipt) -> Self {
        Self {
            receipt: receipt.clone(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    Store(StateStoreError),
    Uninitialized,
    Corrupt,
    BindingMismatch,
    Conflict,
    SequenceExhausted,
    PageTooLarge,
    CompatibilityUnproven,
}
impl From<StateStoreError> for AdmissionError {
    fn from(v: StateStoreError) -> Self {
        Self::Store(v)
    }
}
impl From<StateError> for AdmissionError {
    fn from(v: StateError) -> Self {
        Self::Store(StateStoreError::State(v))
    }
}
impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "producer admission {self:?}")
    }
}
impl std::error::Error for AdmissionError {}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Authority {
    pub schema: u16,
    pub lane: String,
    pub compatibility: String,
    pub binding: ProducerBinding,
    pub issued_sequence: u64,
    pub completed_sequence: u64,
    pub cursor: u64,
    pub last_receipt_id: String,
    #[serde(with = "active_receipt_wire")]
    pub active: Option<PageReceipt>,
}
fn digest(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Authority {
    pub(crate) fn decode(
        document: &JsonDocument,
        binding: &ProducerBinding,
    ) -> Result<Self, AdmissionError> {
        let raw =
            crate::queue::admission_extension(document)?.ok_or(AdmissionError::Uninitialized)?;
        // Feature-only candidate mode cannot authorize unproven old writers.
        // Isolated fixtures are accepted only in explicit fault/test builds.
        let authority: Self = serde_json::from_str(raw).map_err(|_| AdmissionError::Corrupt)?;
        if authority.compatibility != "isolated_fixture_only"
            || !cfg!(any(test, feature = "queue-fault-tests"))
        {
            return Err(AdmissionError::CompatibilityUnproven);
        }
        if authority.schema != 1 || authority.lane != "nwait_v1" || !authority.binding.valid() {
            return Err(AdmissionError::Corrupt);
        }
        if &authority.binding != binding {
            return Err(AdmissionError::BindingMismatch);
        }
        if authority.completed_sequence > authority.issued_sequence
            || (authority.completed_sequence == 0 && !authority.last_receipt_id.is_empty())
            || (authority.completed_sequence > 0 && !digest(&authority.last_receipt_id))
        {
            return Err(AdmissionError::Corrupt);
        }
        match &authority.active {
            None if authority.completed_sequence != authority.issued_sequence => {
                return Err(AdmissionError::Corrupt);
            }
            Some(r)
                if authority.completed_sequence.checked_add(1)
                    != Some(authority.issued_sequence)
                    || r.sequence != authority.issued_sequence
                    || r.from_cursor != authority.cursor
                    || r.to_cursor < r.from_cursor
                    || !r.valid_for(&authority.binding)
                    || !digest(&r.receipt_id)
                    || !digest(&r.page_digest)
                    || r.queued > r.newly_seen
                    || r.queued > 100
                    || r.newly_seen > 100 =>
            {
                return Err(AdmissionError::Corrupt);
            }
            _ => {}
        }
        Ok(authority)
    }
    pub(crate) fn encode(&self) -> Result<String, AdmissionError> {
        serde_json::to_string(self).map_err(|_| AdmissionError::Corrupt)
    }
}
