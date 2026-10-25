//! Short, atomic proofs for the reader selection actor. These helpers never
//! perform card I/O. The closure may only inspect/mutate the selection Gate;
//! callers must acquire Owner before Gate, never call back into Owner, and
//! never await or run a physical operation inside the closure.

use super::{Failure, Owner, valid_reader, valid_token};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LifecycleBinding {
    pub(crate) producer: String,
    pub(crate) reader: String,
    pub(crate) session_id: String,
    pub(crate) session_sequence: u64,
}

impl Owner {
    /// Use the current Rust-owned session identity rather than trusting a
    /// session identity supplied by the business process. Keep both metadata
    /// locks until the Gate has consumed the proof, excluding replacement,
    /// disappearance and producer retirement during selection publication.
    pub(crate) fn with_live_lifecycle_binding<R>(
        &self,
        producer: &str,
        reader: &str,
        consume: impl FnOnce(&LifecycleBinding) -> R,
    ) -> Result<R, Failure> {
        if !valid_token(producer) || !valid_reader(reader) {
            return Err(Failure::invalid());
        }
        let owner = self.inner.state.lock().unwrap();
        if owner.producer != producer
            || !owner.active
            || owner.stopping
            || owner.pending_presence_handoffs.contains_key(reader)
        {
            return Err(Failure::stale());
        }
        let worker = owner.readers.get(reader).ok_or_else(Failure::stale)?;
        let state = worker.state.lock().unwrap();
        if state.producer != producer
            || state.session.is_empty()
            || state.session_seq == 0
            || state.stopping
            || state.closing
            || state.close_error.is_some()
        {
            return Err(Failure::stale());
        }
        // PC/SC handles are opened lazily. A valid logical session need not
        // yet have port_open=true; initialization failures remain nonfatal.
        let binding = LifecycleBinding {
            producer: producer.to_owned(),
            reader: reader.to_owned(),
            session_id: state.session.clone(),
            session_sequence: state.session_seq,
        };
        Ok(consume(&binding))
    }

    /// A close tombstone is only an anti-replay watermark, never proof that
    /// physical close succeeded. Only the exact pending handoff, published
    /// after finish_session_close succeeds, authorizes physical-loss cleanup.
    /// Holding Owner across the Gate closure excludes ACK and reopen races.
    pub(crate) fn with_confirmed_presence_loss<R>(
        &self,
        expected: &LifecycleBinding,
        consume: impl FnOnce() -> R,
    ) -> Result<R, Failure> {
        if !valid_token(&expected.producer)
            || !valid_reader(&expected.reader)
            || !valid_token(&expected.session_id)
            || expected.session_sequence == 0
        {
            return Err(Failure::invalid());
        }
        let owner = self.inner.state.lock().unwrap();
        if owner.producer != expected.producer {
            return Err(Failure::stale());
        }
        let pending = owner
            .pending_presence_handoffs
            .get(&expected.reader)
            .ok_or_else(Failure::stale)?;
        if !pending.close_confirmed
            || pending.session.producer != expected.producer
            || pending.session.reader != expected.reader
            || pending.session.id != expected.session_id
            || pending.session.sequence != expected.session_sequence
        {
            return Err(Failure::stale());
        }
        Ok(consume())
    }
}
