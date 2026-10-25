use super::*;
use crate::producer_admission::{
    AdmissionError, Authority, PageDisposition, PageReceipt, PageRequest, ProducerBinding,
    ProgressAck,
};

impl QueueStore {
    /// No bootstrap/migration: a missing queue or missing/unproven metadata is
    /// an error. Callers cannot use a new empty queue to forget an obligation.
    pub fn open_bound_page_authority(
        path: impl AsRef<Path>,
        key: StateKey,
        limits: StateLimits,
        binding: &ProducerBinding,
    ) -> Result<Self, AdmissionError> {
        let store = Self::open(path, key, limits)?;
        Authority::decode(&store.current, binding)?;
        Ok(store)
    }
    pub fn active_page_receipt(
        &self,
        binding: &ProducerBinding,
    ) -> Result<Option<PageReceipt>, AdmissionError> {
        self.ensure_usable()?;
        Ok(Authority::decode(&self.current, binding)?.active)
    }
    /// Pure mutation plus the existing real SWF1 atomic commit kernel. No
    /// provider IO. Receipt and new tasks always belong to the same ciphertext.
    pub fn admit_page_and_persist(
        &mut self,
        binding: &ProducerBinding,
        request: PageRequest,
        messages: &[ForwardingMessage],
        received_at: &str,
        options: &EnqueueOptions,
    ) -> Result<PageReceipt, AdmissionError> {
        self.ensure_usable()?;
        let mut authority = Authority::decode(&self.current, binding)?;
        if messages.len() > 100 {
            return Err(AdmissionError::PageTooLarge);
        }
        if request.to_cursor < request.from_cursor
            || request.from_cursor != authority.cursor
            || request.expected_completed_sequence != authority.completed_sequence
        {
            return Err(AdmissionError::Conflict);
        }
        if request.disposition != PageDisposition::Deliver && !messages.is_empty() {
            return Err(AdmissionError::Conflict);
        }
        let page_digest = crate::queue::admission_page_digest(messages, options, request);
        if let Some(receipt) = authority.active {
            if receipt.page_digest != page_digest {
                return Err(AdmissionError::Conflict);
            }
            // Receipt proves earlier admission even if every pending/seen key
            // has since been deleted. Never invoke enqueue again on replay.
            return Ok(receipt);
        }
        let sequence = authority
            .issued_sequence
            .checked_add(1)
            .ok_or(AdmissionError::SequenceExhausted)?;
        let (next, summary) = self
            .current
            .enqueue(messages, received_at, options, self.limits)?;
        let mut receipt = PageReceipt {
            sequence,
            receipt_id: String::new(),
            from_cursor: request.from_cursor,
            to_cursor: request.to_cursor,
            disposition: request.disposition,
            page_digest,
            newly_seen: summary.newly_seen,
            queued: summary.queued,
        };
        receipt.receipt_id = receipt
            .expected_id(binding)
            .ok_or(AdmissionError::Corrupt)?;
        authority.issued_sequence = sequence;
        authority.active = Some(receipt.clone());
        let next =
            crate::queue::replace_admission_extension(&next, &authority.encode()?, self.limits)?;
        // Even empty/all-dedup pages must persist the receipt.
        self.persist_admission_document(next)?;
        Ok(receipt)
    }
    pub fn complete_page_and_persist(
        &mut self,
        binding: &ProducerBinding,
        ack: ProgressAck,
    ) -> Result<(), AdmissionError> {
        self.ensure_usable()?;
        let mut authority = Authority::decode(&self.current, binding)?;
        match authority.active.as_ref() {
            Some(active) if active == &ack.receipt => {}
            None if authority.completed_sequence == ack.receipt.sequence
                && authority.last_receipt_id == ack.receipt.receipt_id =>
            {
                return Ok(());
            }
            _ => return Err(AdmissionError::Conflict),
        }
        authority.completed_sequence = ack.receipt.sequence;
        authority.cursor = ack.receipt.to_cursor;
        authority.last_receipt_id = ack.receipt.receipt_id;
        authority.active = None;
        let next = crate::queue::replace_admission_extension(
            &self.current,
            &authority.encode()?,
            self.limits,
        )?;
        self.persist_admission_document(next)
    }
    fn persist_admission_document(&mut self, next: JsonDocument) -> Result<(), AdmissionError> {
        let ciphertext = seal(&self.key, &next, self.limits)?;
        match atomic_replace(&self.path, &ciphertext) {
            Ok(()) => {
                self.current = next;
                Ok(())
            }
            Err(ReplaceError::BeforeCommit) => Err(AdmissionError::Store(StateStoreError::Io)),
            Err(ReplaceError::AfterCommit) => {
                self.poisoned = true;
                Err(AdmissionError::Store(StateStoreError::PersistenceUncertain))
            }
        }
    }
}

#[cfg(test)]
#[path = "producer_admission/tests.rs"]
mod tests;
