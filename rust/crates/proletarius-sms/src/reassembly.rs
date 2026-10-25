//! Bounded reassembly of concatenated SMS text. Expiry is owned by the caller.
use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};

use crate::{deliver::Message, user_data::Content};

const MAX_PENDING_GROUPS: usize = 128;
const MAX_PARTS: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    sender: String,
    reference: u16,
    wide_reference: bool,
    total: u8,
    dcs: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    NotConcatenated,
    NotText,
    InvalidPart,
    ConflictingDuplicate,
    Capacity,
}

#[derive(Default)]
pub struct Reassembler {
    groups: HashMap<Key, Group>,
}

#[derive(Default)]
struct Group {
    parts: BTreeMap<u8, String>,
    updated_at: Option<Instant>,
}

impl Reassembler {
    pub fn push(&mut self, message: Message) -> Result<Option<String>, Error> {
        self.push_at(message, Instant::now())
    }

    pub fn push_at(&mut self, message: Message, now: Instant) -> Result<Option<String>, Error> {
        let concat = message.concatenation.ok_or(Error::NotConcatenated)?;
        if usize::from(concat.total) > MAX_PARTS
            || concat.sequence == 0
            || concat.sequence > concat.total
        {
            return Err(Error::InvalidPart);
        }
        let Content::Text(text) = message.content else {
            return Err(Error::NotText);
        };
        let key = Key {
            sender: message.sender,
            reference: concat.reference,
            wide_reference: concat.wide_reference,
            total: concat.total,
            dcs: message.dcs,
        };
        if !self.groups.contains_key(&key) && self.groups.len() == MAX_PENDING_GROUPS {
            return Err(Error::Capacity);
        }
        let group = self.groups.entry(key.clone()).or_default();
        if let Some(existing) = group.parts.get(&concat.sequence) {
            if existing == &text {
                return Ok(None);
            }
            return Err(Error::ConflictingDuplicate);
        }
        group.parts.insert(concat.sequence, text);
        group.updated_at = Some(now);
        if group.parts.len() != usize::from(concat.total) {
            return Ok(None);
        }
        let mut complete = String::new();
        for sequence in 1..=concat.total {
            complete.push_str(group.parts.get(&sequence).ok_or(Error::InvalidPart)?);
        }
        self.groups.remove(&key);
        Ok(Some(complete))
    }

    pub fn cleanup(&mut self, now: Instant, ttl: Duration) {
        self.groups.retain(|_, group| {
            group
                .updated_at
                .is_some_and(|updated_at| now.saturating_duration_since(updated_at) < ttl)
        });
    }

    pub fn pending_len(&self) -> usize {
        self.groups.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{deliver::Message, timestamp::Timestamp, udh::Concatenation};

    fn part(sender: &str, sequence: u8, text: &str) -> Message {
        Message {
            sender: sender.into(),
            timestamp: [0; 7],
            service_center_time: Some(Timestamp {
                year: 2026,
                month: 9,
                day: 6,
                hour: 0,
                minute: 0,
                second: 0,
                offset_minutes: 0,
            }),
            protocol_id: 0,
            dcs: 0,
            concatenation: Some(Concatenation {
                reference: 42,
                wide_reference: false,
                total: 2,
                sequence,
            }),
            udh_elements: Vec::new(),
            content: Content::Text(text.into()),
        }
    }

    #[test]
    fn completes_out_of_order_once_without_mixing_senders() {
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.push(part("one", 2, "world")).unwrap(), None);
        assert_eq!(reassembler.push(part("two", 1, "wrong")).unwrap(), None);
        assert_eq!(
            reassembler.push(part("one", 1, "hello ")).unwrap(),
            Some("hello world".into())
        );
        assert_eq!(reassembler.pending_len(), 1);
    }

    #[test]
    fn rejects_conflicts_and_binary_without_losing_pending_group() {
        let mut reassembler = Reassembler::default();
        reassembler.push(part("one", 1, "first")).unwrap();
        assert_eq!(
            reassembler.push(part("one", 1, "other")),
            Err(Error::ConflictingDuplicate)
        );
        assert_eq!(reassembler.push(part("one", 1, "first")).unwrap(), None);
        let mut binary = part("one", 2, "");
        binary.content = Content::Binary(vec![1]);
        assert_eq!(reassembler.push(binary), Err(Error::NotText));
        assert_eq!(
            reassembler.push(part("one", 2, "last")).unwrap(),
            Some("firstlast".into())
        );
    }

    #[test]
    fn capacity_is_bounded() {
        let mut reassembler = Reassembler::default();
        for index in 0..MAX_PENDING_GROUPS {
            let mut message = part("same", 1, "x");
            message.concatenation.as_mut().unwrap().reference = index as u16;
            reassembler.push(message).unwrap();
        }
        let mut overflow = part("same", 1, "x");
        overflow.concatenation.as_mut().unwrap().reference = 999;
        assert_eq!(reassembler.push(overflow), Err(Error::Capacity));
    }

    #[test]
    fn duplicate_does_not_extend_expiry_and_cleanup_is_deterministic() {
        let mut reassembler = Reassembler::default();
        let start = Instant::now();
        reassembler.push_at(part("one", 1, "first"), start).unwrap();
        reassembler
            .push_at(part("one", 1, "first"), start + Duration::from_secs(500))
            .unwrap();
        reassembler.cleanup(start + Duration::from_secs(601), Duration::from_secs(600));
        assert_eq!(reassembler.pending_len(), 0);
    }
}
