//! Current-card notification projection and explicit synchronization helpers.
//!
//! Listing performs card reads only. Operator delivery and card confirmation
//! are admitted exclusively by the explicit POST mutation path.

use super::RetryNotificationFailure;
use proletarius_euicc::{NotificationInfo, PendingNotification};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct NotificationItem {
    pub(super) sequence_number: i64,
    pub(super) event: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) iccid: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) address: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) aid_hex: String,
    pub(super) can_retry: bool,
}

pub(super) trait NotificationCard {
    fn list(&mut self) -> Result<Vec<NotificationInfo>, String>;
    /// Every matching payload must be delivered before the sequence is removed.
    fn retrieve(&mut self, sequence: u64) -> Result<Vec<PendingNotification>, String>;
    /// Already-removed entries are successful confirmations.
    fn remove(&mut self, sequence: u64) -> Result<(), String>;
}

pub(super) fn list_read_only<C: NotificationCard>(
    card: &mut C,
    aid_hex: &str,
    cancelled: &AtomicBool,
) -> Result<Vec<NotificationItem>, RetryNotificationFailure> {
    ensure_active(cancelled, false)?;
    let notifications = card.list().map_err(RetryNotificationFailure::Failed)?;
    ensure_active(cancelled, false)?;
    let mut items: Vec<_> = notifications
        .into_iter()
        .map(|item| notification_item(item, aid_hex))
        .collect::<Result<_, _>>()?;
    items.sort_by_key(|item| std::cmp::Reverse(item.sequence_number));
    Ok(items)
}

/// Deliver every payload for one sequence before confirming that sequence on
/// the card. A later payload failure must not replay earlier successful ones.
pub(super) fn deliver_pending(
    cancelled: &AtomicBool,
    pending: &[PendingNotification],
    mut deliver: impl FnMut(&PendingNotification) -> Result<(), String>,
    mut remove: impl FnMut() -> Result<(), String>,
    mut wait: impl FnMut(Duration),
) -> Result<(), RetryNotificationFailure> {
    if pending.is_empty() {
        return Err(RetryNotificationFailure::NotFound);
    }
    ensure_active(cancelled, false)?;
    for notification in pending {
        retry_action(cancelled, true, &mut wait, || deliver(notification))?;
    }
    match retry_action(cancelled, true, &mut wait, &mut remove) {
        Err(RetryNotificationFailure::Failed(error)) => {
            Err(RetryNotificationFailure::Uncertain(error))
        }
        result => result,
    }
}

/// Preserve list availability when automatic delivery fails. A lost lease is
/// different: stop immediately and let the caller report the ownership loss.
pub(super) fn list_with_cleanup<C: NotificationCard>(
    card: &mut C,
    aid_hex: &str,
    cancelled: &AtomicBool,
    mut deliver: impl FnMut(&PendingNotification) -> Result<(), String>,
    mut wait: impl FnMut(Duration),
) -> Result<Vec<NotificationItem>, RetryNotificationFailure> {
    ensure_active(cancelled, false)?;
    let initial = card.list();
    ensure_active(cancelled, false)?;
    let mut notifications = initial.map_err(RetryNotificationFailure::Failed)?;
    let mut cleaned = HashSet::new();
    let mut delivery_started = false;
    for metadata in &notifications {
        if metadata.sequence_number == 0 || !matches!(metadata.event, 1 | 2) {
            continue;
        }
        let pending = retry_action(cancelled, delivery_started, &mut wait, || {
            card.retrieve(metadata.sequence_number)
        });
        let pending = match pending {
            Ok(pending) => pending,
            Err(RetryNotificationFailure::Failed(_)) => continue,
            Err(error) => return Err(error),
        };
        if pending.is_empty() {
            continue;
        }
        let mut delivered_all = true;
        for notification in &pending {
            // Lease loss while a network request is in flight is uncertain.
            delivery_started = true;
            match retry_action(cancelled, true, &mut wait, || deliver(notification)) {
                Ok(()) => {}
                Err(RetryNotificationFailure::Failed(_)) => {
                    delivered_all = false;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        if !delivered_all {
            continue;
        }
        match retry_action(cancelled, true, &mut wait, || {
            card.remove(metadata.sequence_number)
        }) {
            Ok(()) => {
                cleaned.insert(metadata.sequence_number);
            }
            Err(RetryNotificationFailure::Failed(_)) => {}
            Err(error) => return Err(error),
        }
    }
    if !cleaned.is_empty() {
        ensure_active(cancelled, delivery_started)?;
        // The Go contract uses a fresh card list when possible, otherwise it
        // hides only entries whose delivery AND card confirmation succeeded.
        match card.list() {
            Ok(refreshed) => notifications = refreshed,
            Err(_) => notifications.retain(|item| !cleaned.contains(&item.sequence_number)),
        }
    }
    ensure_active(cancelled, delivery_started)?;
    let mut items: Vec<_> = notifications
        .into_iter()
        .map(|item| notification_item(item, aid_hex))
        .collect::<Result<_, _>>()?;
    items.sort_by_key(|item| std::cmp::Reverse(item.sequence_number));
    Ok(items)
}

/// Candidate AIDs are supplied in priority order. Keep the first working AID
/// for identical notifications, then use stable descending sequence order.
pub(super) fn merge_items(items: Vec<NotificationItem>) -> Vec<NotificationItem> {
    let mut seen = HashSet::new();
    let mut unique = Vec::with_capacity(items.len());
    for item in items {
        let key = (
            item.sequence_number,
            item.event,
            item.iccid.clone(),
            item.address.clone(),
        );
        if seen.insert(key) {
            unique.push(item);
        }
    }
    unique.sort_by_key(|item| std::cmp::Reverse(item.sequence_number));
    unique
}

pub(super) fn ensure_active(
    cancelled: &AtomicBool,
    delivery_started: bool,
) -> Result<(), RetryNotificationFailure> {
    if !cancelled.load(Ordering::Acquire) {
        return Ok(());
    }
    Err(if delivery_started {
        RetryNotificationFailure::Uncertain(
            "reader lease was lost during notification synchronization".into(),
        )
    } else {
        RetryNotificationFailure::Cancelled
    })
}

fn retry_action<T>(
    cancelled: &AtomicBool,
    delivery_started: bool,
    wait: &mut impl FnMut(Duration),
    mut action: impl FnMut() -> Result<T, String>,
) -> Result<T, RetryNotificationFailure> {
    for attempt in 0..=3 {
        ensure_active(cancelled, delivery_started)?;
        let result = action();
        ensure_active(cancelled, delivery_started)?;
        match result {
            Ok(value) => return Ok(value),
            Err(error) if attempt == 3 => {
                return Err(RetryNotificationFailure::Failed(error));
            }
            Err(_) => wait(Duration::from_millis(300_u64 << attempt)),
        }
    }
    unreachable!("four bounded attempts always return a result")
}

fn notification_item(
    item: NotificationInfo,
    aid_hex: &str,
) -> Result<NotificationItem, RetryNotificationFailure> {
    let sequence_number = i64::try_from(item.sequence_number).map_err(|_| {
        RetryNotificationFailure::Failed("notification sequence is outside the API range".into())
    })?;
    let address = String::from_utf8_lossy(&item.address).into_owned();
    // Preserve euicc-go's representation exactly, including its handling of
    // non-decimal nibbles; display conversion must not hide a pending entry.
    let mut iccid = String::with_capacity(item.iccid.len() * 2);
    const HEX: &[u8] = b"0123456789abcdef";
    for byte in item.iccid {
        iccid.push(HEX[(byte & 0x0f) as usize] as char);
        iccid.push(HEX[(byte >> 4) as usize] as char);
    }
    if let Some(last_padding) = iccid.rfind('f') {
        iccid.truncate(last_padding);
    }
    Ok(NotificationItem {
        sequence_number,
        event: match item.event {
            0 => "install",
            1 => "enable",
            2 => "disable",
            3 => "delete",
            _ => "unknown",
        },
        iccid,
        can_retry: !address.is_empty(),
        address,
        aid_hex: aid_hex.to_ascii_uppercase(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};

    #[derive(Default)]
    struct Card {
        lists: VecDeque<Result<Vec<NotificationInfo>, String>>,
        pending: HashMap<u64, Vec<PendingNotification>>,
        retrieve_errors: HashSet<u64>,
        remove_errors: HashSet<u64>,
        retrieved: Vec<u64>,
        removed: Vec<u64>,
        list_calls: usize,
    }

    impl NotificationCard for Card {
        fn list(&mut self) -> Result<Vec<NotificationInfo>, String> {
            self.list_calls += 1;
            self.lists.pop_front().expect("unexpected list call")
        }

        fn retrieve(&mut self, sequence: u64) -> Result<Vec<PendingNotification>, String> {
            self.retrieved.push(sequence);
            if self.retrieve_errors.contains(&sequence) {
                Err("retrieve failed".into())
            } else {
                Ok(self.pending.get(&sequence).cloned().unwrap_or_default())
            }
        }

        fn remove(&mut self, sequence: u64) -> Result<(), String> {
            self.removed.push(sequence);
            if self.remove_errors.contains(&sequence) {
                Err("remove failed".into())
            } else {
                Ok(())
            }
        }
    }

    fn metadata(sequence: u64, event: u8) -> NotificationInfo {
        NotificationInfo {
            sequence_number: sequence,
            event,
            address: b"notify.example.com".to_vec(),
            iccid: proletarius_euicc::encode_iccid("8986001234567890123").unwrap(),
        }
    }

    fn pending(sequence: u64, payload: u8) -> PendingNotification {
        PendingNotification {
            sequence_number: sequence,
            address: "notify.example.com".into(),
            encoded: vec![payload],
        }
    }

    fn card(items: Vec<NotificationInfo>) -> Card {
        Card {
            lists: VecDeque::from([Ok(items)]),
            ..Card::default()
        }
    }

    #[test]
    fn mapping_preserves_swift_contract_and_stable_sequence_order() {
        let mut card = card(vec![metadata(9, 3), metadata(11, 0)]);
        let items = list_with_cleanup(
            &mut card,
            "a00001",
            &AtomicBool::new(false),
            |_| panic!("manual notification must not be sent"),
            |_| {},
        )
        .unwrap();
        assert_eq!(items[0].sequence_number, 11);
        assert_eq!(items[0].event, "install");
        assert_eq!(items[0].iccid, "8986001234567890123");
        assert_eq!(items[0].aid_hex, "A00001");
        assert!(items[0].can_retry);
        assert_eq!(items[1].event, "delete");
        assert!(card.retrieved.is_empty());
        let json = serde_json::to_value(&items[0]).unwrap();
        assert_eq!(json["sequence_number"], 11);
        assert_eq!(json["can_retry"], true);
        assert_eq!(json.as_object().unwrap().len(), 6);
    }

    #[test]
    fn read_only_list_never_retrieves_delivers_or_confirms_enable_disable() {
        let mut card = card(vec![metadata(12, 1), metadata(13, 2)]);
        card.pending.insert(12, vec![pending(12, 1)]);
        card.pending.insert(13, vec![pending(13, 2)]);
        let items = list_read_only(&mut card, "A00001", &AtomicBool::new(false)).unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item.sequence_number)
                .collect::<Vec<_>>(),
            [13, 12]
        );
        assert_eq!(card.list_calls, 1);
        assert!(
            card.retrieved.is_empty(),
            "GET projection retrieved a payload"
        );
        assert!(
            card.removed.is_empty(),
            "GET projection confirmed a card entry"
        );
    }

    #[test]
    fn empty_optional_fields_are_omitted_and_do_not_enable_retry() {
        let item = notification_item(NotificationInfo::default(), "").unwrap();
        let json = serde_json::to_value(item).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 3);
        assert_eq!(json["can_retry"], false);
    }

    #[test]
    fn cleanup_delivers_enable_disable_in_card_order_and_reloads() {
        let mut card = card(vec![
            metadata(9, 3),
            metadata(12, 1),
            metadata(11, 0),
            metadata(13, 2),
        ]);
        card.lists
            .push_back(Ok(vec![metadata(9, 3), metadata(11, 0)]));
        card.pending.insert(12, vec![pending(12, 12)]);
        card.pending.insert(13, vec![pending(13, 13)]);
        let mut sent = Vec::new();
        let items = list_with_cleanup(
            &mut card,
            "A00001",
            &AtomicBool::new(false),
            |item| {
                sent.push(item.sequence_number);
                Ok(())
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(sent, [12, 13]);
        assert_eq!(card.retrieved, [12, 13]);
        assert_eq!(card.removed, [12, 13]);
        assert_eq!(card.list_calls, 2);
        assert_eq!(
            items
                .iter()
                .map(|item| item.sequence_number)
                .collect::<Vec<_>>(),
            [11, 9]
        );
    }

    #[test]
    fn failed_retrieval_stays_visible_after_four_bounded_attempts() {
        let mut card = card(vec![metadata(12, 1), metadata(11, 0)]);
        card.retrieve_errors.insert(12);
        let mut waits = Vec::new();
        let items = list_with_cleanup(
            &mut card,
            "A00001",
            &AtomicBool::new(false),
            |_| panic!("no retrieved notification"),
            |wait| waits.push(wait.as_millis()),
        )
        .unwrap();
        assert_eq!(card.retrieved, [12; 4]);
        assert_eq!(waits, [300, 600, 1200]);
        assert!(card.removed.is_empty());
        assert_eq!(items.len(), 2);
        assert_eq!(card.list_calls, 1);
    }

    #[test]
    fn every_payload_must_be_delivered_and_successes_are_not_replayed() {
        let mut card = card(vec![metadata(12, 1)]);
        card.pending
            .insert(12, vec![pending(12, 1), pending(12, 2)]);
        let mut sent = Vec::new();
        let items = list_with_cleanup(
            &mut card,
            "A00001",
            &AtomicBool::new(false),
            |item| {
                sent.push(item.encoded[0]);
                if item.encoded == [2] {
                    Err("network failed".into())
                } else {
                    Ok(())
                }
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(sent, [1, 2, 2, 2, 2]);
        assert!(card.removed.is_empty());
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn manual_retry_later_payload_failure_never_confirms_or_replays_prior_success() {
        let pending = [pending(12, 1), pending(12, 2)];
        let mut delivered = Vec::new();
        let mut confirmations = 0;
        let result = deliver_pending(
            &AtomicBool::new(false),
            &pending,
            |item| {
                delivered.push(item.encoded[0]);
                if item.encoded == [2] {
                    Err("second payload failed".into())
                } else {
                    Ok(())
                }
            },
            || {
                confirmations += 1;
                Ok(())
            },
            |_| {},
        );
        assert!(matches!(result, Err(RetryNotificationFailure::Failed(_))));
        assert_eq!(delivered, [1, 2, 2, 2, 2]);
        assert_eq!(confirmations, 0);
    }

    #[test]
    fn manual_retry_confirms_once_only_after_all_payloads_succeed() {
        use std::cell::RefCell;
        let events = RefCell::new(Vec::new());
        deliver_pending(
            &AtomicBool::new(false),
            &[pending(12, 1), pending(12, 2)],
            |item| {
                events.borrow_mut().push(item.encoded[0]);
                Ok(())
            },
            || {
                events.borrow_mut().push(0);
                Ok(())
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(*events.borrow(), [1, 2, 0]);
        let result = deliver_pending(
            &AtomicBool::new(false),
            &[],
            |_| panic!("empty response must not send"),
            || panic!("empty response must not confirm"),
            |_| {},
        );
        assert!(matches!(result, Err(RetryNotificationFailure::NotFound)));
    }

    #[test]
    fn card_confirmation_failure_preserves_entry_after_successful_delivery() {
        let mut card = card(vec![metadata(12, 1)]);
        card.pending.insert(12, vec![pending(12, 1)]);
        card.remove_errors.insert(12);
        let mut sends = 0;
        let items = list_with_cleanup(
            &mut card,
            "A00001",
            &AtomicBool::new(false),
            |_| {
                sends += 1;
                Ok(())
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(sends, 1);
        assert_eq!(card.removed, [12; 4]);
        assert_eq!(items.len(), 1);
        assert_eq!(card.list_calls, 1);
    }

    #[test]
    fn reload_failure_filters_only_confirmed_sequences() {
        let mut card = card(vec![metadata(12, 1), metadata(13, 2), metadata(11, 0)]);
        card.pending.insert(12, vec![pending(12, 1)]);
        card.lists
            .push_back(Err("card disconnected before refresh".into()));
        let items = list_with_cleanup(
            &mut card,
            "A00001",
            &AtomicBool::new(false),
            |_| Ok(()),
            |_| {},
        )
        .unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item.sequence_number)
                .collect::<Vec<_>>(),
            [13, 11]
        );
        assert_eq!(card.removed, [12]);
    }

    #[test]
    fn successful_reload_is_authoritative_and_can_include_new_entries() {
        let mut card = card(vec![metadata(12, 1)]);
        card.pending.insert(12, vec![pending(12, 1)]);
        card.lists.push_back(Ok(vec![metadata(20, 0)]));
        let items = list_with_cleanup(
            &mut card,
            "A00001",
            &AtomicBool::new(false),
            |_| Ok(()),
            |_| {},
        )
        .unwrap();
        assert_eq!(items[0].sequence_number, 20);
    }

    #[test]
    fn zero_sequence_and_unknown_events_are_never_auto_cleaned() {
        let mut card = card(vec![metadata(0, 1), metadata(12, 4)]);
        let items = list_with_cleanup(
            &mut card,
            "A00001",
            &AtomicBool::new(false),
            |_| panic!("must not send"),
            |_| {},
        )
        .unwrap();
        assert_eq!(items[0].event, "unknown");
        assert!(card.retrieved.is_empty());
    }

    #[test]
    fn duplicate_aids_retain_first_working_aid_and_distinct_payload_identity() {
        let first = notification_item(metadata(12, 0), "A00001").unwrap();
        let duplicate = notification_item(metadata(12, 0), "A00002").unwrap();
        let different_event = notification_item(metadata(12, 3), "A00002").unwrap();
        let older = notification_item(metadata(11, 0), "A00003").unwrap();
        let items = merge_items(vec![
            older.clone(),
            first.clone(),
            duplicate,
            different_event.clone(),
        ]);
        assert_eq!(items, [first, different_event, older]);
    }

    #[test]
    fn initial_read_failure_is_an_error_not_an_empty_success() {
        let mut card = Card {
            lists: VecDeque::from([Err("read failed".into())]),
            ..Card::default()
        };
        assert!(matches!(
            list_with_cleanup(
                &mut card,
                "A00001",
                &AtomicBool::new(false),
                |_| Ok(()),
                |_| {}
            ),
            Err(RetryNotificationFailure::Failed(_))
        ));
    }

    #[test]
    fn cancelled_before_listing_never_accesses_card() {
        let mut card = Card::default();
        assert!(matches!(
            list_with_cleanup(
                &mut card,
                "A00001",
                &AtomicBool::new(true),
                |_| Ok(()),
                |_| {}
            ),
            Err(RetryNotificationFailure::Cancelled)
        ));
        assert_eq!(card.list_calls, 0);
    }

    #[test]
    fn cancellation_during_network_delivery_never_confirms_or_retries() {
        let mut card = card(vec![metadata(12, 1)]);
        card.pending.insert(12, vec![pending(12, 1)]);
        let cancelled = AtomicBool::new(false);
        let mut sends = 0;
        let result = list_with_cleanup(
            &mut card,
            "A00001",
            &cancelled,
            |_| {
                sends += 1;
                cancelled.store(true, Ordering::Release);
                Ok(())
            },
            |_| panic!("must not retry after lease loss"),
        );
        assert!(matches!(
            result,
            Err(RetryNotificationFailure::Uncertain(_))
        ));
        assert_eq!(sends, 1);
        assert!(card.removed.is_empty());
    }

    #[test]
    fn cancellation_during_backoff_prevents_next_card_attempt() {
        let mut card = card(vec![metadata(12, 1)]);
        card.retrieve_errors.insert(12);
        let cancelled = AtomicBool::new(false);
        let result = list_with_cleanup(
            &mut card,
            "A00001",
            &cancelled,
            |_| Ok(()),
            |_| cancelled.store(true, Ordering::Release),
        );
        assert!(matches!(result, Err(RetryNotificationFailure::Cancelled)));
        assert_eq!(card.retrieved, [12]);
    }

    #[test]
    fn oversized_sequence_is_rejected_before_serializing_invalid_swift_int64() {
        assert!(matches!(
            notification_item(metadata(u64::MAX, 0), "A00001"),
            Err(RetryNotificationFailure::Failed(_))
        ));
    }
}
