//! Existing schema2 progress only. No legacy migration or empty initialization.
use super::{PageDisposition, PageReceipt, ProducerBinding, ProgressAck};
use proletarius_store::{
    ArchiveStore,
    raw::{CommitStage, Mutation, TransactionError},
};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressProjection {
    pub schema_version: u16,
    pub binding: ProducerBinding,
    pub cursor: u64,
    pub last_admission_sequence: u64,
    pub last_receipt_id: String,
    pub was_enabled: Option<bool>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressError {
    Unavailable,
    InvalidAuthority,
    Conflict,
    BeforeRename,
    AfterRename,
}
impl std::fmt::Display for ProgressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "progress {self:?}")
    }
}
impl std::error::Error for ProgressError {}
#[derive(Clone)]
pub struct ProgressStore {
    archive: ArchiveStore,
    binding: ProducerBinding,
    health: Arc<Mutex<bool>>,
}
impl ProgressStore {
    pub fn open_bound(
        path: impl AsRef<Path>,
        binding: &ProducerBinding,
    ) -> Result<Self, ProgressError> {
        let archive = ArchiveStore::with_limits(path.as_ref(), 16 * 1024, 1)
            .map_err(|_| ProgressError::InvalidAuthority)?;
        let store = Self {
            archive,
            binding: binding.clone(),
            health: Arc::new(Mutex::new(false)),
        };
        store.current()?;
        Ok(store)
    }
    fn decode(&self, bytes: &[u8]) -> Result<ProgressProjection, &'static str> {
        let mut records: Vec<ProgressProjection> =
            serde_json::from_slice(bytes).map_err(|_| "schema")?;
        if records.len() != 1 {
            return Err("schema");
        }
        let p = records.pop().ok_or("schema")?;
        if p.schema_version != 2
            || p.binding != self.binding
            || !p.binding.valid()
            || (p.last_admission_sequence == 0 && !p.last_receipt_id.is_empty())
            || (p.last_admission_sequence > 0 && p.was_enabled.is_none())
            || (p.last_admission_sequence > 0
                && !(p.last_receipt_id.len() == 64
                    && p.last_receipt_id.bytes().all(|b| b.is_ascii_hexdigit())))
        {
            return Err("schema");
        }
        Ok(p)
    }
    fn transact<T>(
        &self,
        plan: impl FnOnce(&[u8]) -> Result<Mutation<T>, &'static str>,
    ) -> Result<T, ProgressError> {
        let mut fault = self.health.lock().map_err(|_| ProgressError::Unavailable)?;
        if *fault {
            return Err(ProgressError::Unavailable);
        }
        let result = self
            .archive
            .transaction_raw(|bytes| self.decode(bytes).map(|_| ()), plan);
        match result {
            Ok(v) => Ok(v),
            Err(TransactionError::Rejected("conflict")) => Err(ProgressError::Conflict),
            Err(error) => {
                *fault = true;
                Err(match error {
                    TransactionError::Commit(e) if e.stage == CommitStage::AfterRename => {
                        ProgressError::AfterRename
                    }
                    TransactionError::Commit(_) => ProgressError::BeforeRename,
                    _ => ProgressError::InvalidAuthority,
                })
            }
        }
    }
    pub fn current(&self) -> Result<ProgressProjection, ProgressError> {
        self.transact(|bytes| Ok(Mutation::Unchanged(self.decode(bytes)?)))
    }
    /// The opaque ACK is constructed only after raw durability succeeds. A
    /// rename/acknowledgement failure latches this instance; reopening verifies
    /// exact visible sequence/receipt rather than reissuing queue admission.
    pub fn advance_from_receipt(
        &self,
        receipt: &PageReceipt,
    ) -> Result<ProgressAck, ProgressError> {
        if !receipt.valid_for(&self.binding) {
            return Err(ProgressError::Conflict);
        }
        self.transact(|bytes| {
            let mut p = self.decode(bytes)?;
            if p.last_admission_sequence == receipt.sequence
                && p.last_receipt_id == receipt.receipt_id
                && p.cursor == receipt.to_cursor
                && p.was_enabled == Some(receipt.disposition != PageDisposition::DisabledSkip)
            {
                // Matching visible bytes do not acknowledge a previous
                // AfterRename outcome. Recommit the exact original bytes and
                // wait for file + directory fsync before issuing any ACK.
                return Ok(Mutation::Replace {
                    bytes: bytes.to_vec(),
                    result: ProgressAck {
                        receipt: receipt.clone(),
                    },
                });
            }
            if p.last_admission_sequence.checked_add(1) != Some(receipt.sequence)
                || p.cursor != receipt.from_cursor
            {
                return Err("conflict");
            }
            p.cursor = receipt.to_cursor;
            p.last_admission_sequence = receipt.sequence;
            p.last_receipt_id = receipt.receipt_id.clone();
            p.was_enabled = Some(receipt.disposition != PageDisposition::DisabledSkip);
            let bytes = serde_json::to_vec(&[p]).map_err(|_| "encode")?;
            Ok(Mutation::Replace {
                bytes,
                result: ProgressAck {
                    receipt: receipt.clone(),
                },
            })
        })
    }
    #[cfg(feature = "queue-fault-tests")]
    pub(crate) fn fail_commit_at_for_test(&self, point: u64) {
        self.archive.fail_commit_at_for_test(point);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn binding() -> ProducerBinding {
        ProducerBinding {
            queue_id: "q".into(),
            source_id: "s".into(),
            handoff_id: "h".into(),
        }
    }
    fn fixture() -> (std::path::PathBuf, PageReceipt) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let root =
            std::path::PathBuf::from(std::env::var_os("SWITCH_BUILD_DIR").unwrap()).join(format!(
                "tmp/b2-progress-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
        fs::create_dir_all(&root).unwrap();
        let p = ProgressProjection {
            schema_version: 2,
            binding: binding(),
            cursor: 9,
            last_admission_sequence: 0,
            last_receipt_id: String::new(),
            was_enabled: Some(true),
        };
        fs::write(
            root.join("progress.json"),
            serde_json::to_vec(&[p]).unwrap(),
        )
        .unwrap();
        let digest = "a".repeat(64);
        let mut receipt = PageReceipt {
            sequence: 1,
            receipt_id: String::new(),
            from_cursor: 9,
            to_cursor: 10,
            disposition: PageDisposition::Deliver,
            page_digest: digest,
            newly_seen: 1,
            queued: 1,
        };
        receipt.receipt_id = receipt.expected_id(&binding()).unwrap();
        (root, receipt)
    }

    #[test]
    fn b2_progress_real_raw_before_after_rename_never_returns_ack_and_latches() {
        for point in [3, 4] {
            let (root, r) = fixture();
            let path = root.join("progress.json");
            let p = ProgressStore::open_bound(&path, &binding()).unwrap();
            p.archive.fail_commit_at_for_test(point);
            assert!(
                matches!(p.advance_from_receipt(&r),Err(e) if e==if point==3 {ProgressError::BeforeRename}else{ProgressError::AfterRename})
            );
            assert!(matches!(p.current(), Err(ProgressError::Unavailable)));
            drop(p);
            let reopened = ProgressStore::open_bound(&path, &binding()).unwrap();
            assert_eq!(
                reopened.current().unwrap().cursor,
                if point == 3 { 9 } else { 10 }
            );
            reopened.advance_from_receipt(&r).unwrap();
            assert_eq!(reopened.current().unwrap().cursor, 10);
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn b2_progress_exact_replay_is_unchanged_and_alien_receipt_has_no_ack() {
        let (root, mut r) = fixture();
        let path = root.join("progress.json");
        let p = ProgressStore::open_bound(&path, &binding()).unwrap();
        p.advance_from_receipt(&r).unwrap();
        let before = fs::read(&path).unwrap();
        p.advance_from_receipt(&r).unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
        r.receipt_id = "b".repeat(64);
        assert!(matches!(
            p.advance_from_receipt(&r),
            Err(ProgressError::Conflict)
        ));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn b2_progress_missing_and_v1_authorities_never_initialize() {
        let (root, _) = fixture();
        let path = root.join("progress.json");
        fs::remove_file(&path).unwrap();
        assert!(ProgressStore::open_bound(&path, &binding()).is_err());
        assert!(!path.exists());
        fs::write(&path, br#"[{"schema_version":1,"cursor":9}]"#).unwrap();
        assert!(ProgressStore::open_bound(&path, &binding()).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn b2_matching_projection_reconfirmation_before_after_rename_never_returns_ack() {
        for point in [3, 4] {
            let (root, receipt) = fixture();
            let path = root.join("progress.json");
            let first = ProgressStore::open_bound(&path, &binding()).unwrap();
            first.advance_from_receipt(&receipt).unwrap();
            drop(first);
            let before = fs::read(&path).unwrap();
            let reopened = ProgressStore::open_bound(&path, &binding()).unwrap();
            reopened.archive.fail_commit_at_for_test(point);
            assert!(
                matches!(reopened.advance_from_receipt(&receipt), Err(error) if error==if point==3 {ProgressError::BeforeRename}else{ProgressError::AfterRename})
            );
            assert!(matches!(
                reopened.current(),
                Err(ProgressError::Unavailable)
            ));
            assert_eq!(fs::read(&path).unwrap(), before);
            drop(reopened);
            let confirmed = ProgressStore::open_bound(&path, &binding()).unwrap();
            confirmed.advance_from_receipt(&receipt).unwrap();
            assert_eq!(fs::read(&path).unwrap(), before);
            fs::remove_dir_all(root).unwrap();
        }
    }
}
