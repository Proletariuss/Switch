//! Rust-owned device-inbox forwarding producer.
//!
//! Rust reads the same archive + volatile inbox projection as GET /api/sms.
//! Swift-compatible notification identity, no-backfill admission, deduplication,
//! and durable provider queueing remain Rust-owned.

use std::collections::BTreeSet;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use tokio::time::{MissedTickBehavior, interval};

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAX_INBOX_MESSAGES: usize = 100_000;
const MAX_FORWARDING_BATCH: usize = 200;
const MAX_FORWARDING_BATCH_FIELD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct InboxSnapshot {
    current_notification_ids: Vec<String>,
    messages: Vec<crate::sms_forwarding::DeviceSnapshotMessage>,
}

#[derive(Clone, Debug, Deserialize)]
struct UpstreamSMS {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    sender: String,
    #[serde(default)]
    content: String,
    timestamp: String,
    #[serde(default)]
    direction: String,
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    subscription_id: Option<String>,
}

#[derive(Clone)]
struct Stores {
    inbox: crate::message_archive::InboxState,
    handoffs: crate::forwarding_handoff_store::Store,
    admission: crate::device_forwarding_store::Store,
    forwarding: crate::sms_forwarding::Service,
}

trait Runtime: Sync {
    async fn current_handoff(
        &self,
    ) -> Result<Option<proletarius_api_contract::SmsForwardingHandoffV1>, &'static str>;
    async fn current_progress(
        &self,
    ) -> Result<Option<crate::device_forwarding_store::Progress>, &'static str>;
    async fn forwarding_enabled(&self) -> Result<bool, &'static str>;
    async fn inbox(&self) -> Result<InboxSnapshot, &'static str>;
    async fn submit(
        &self,
        current_notification_ids: Vec<String>,
        messages: Vec<crate::sms_forwarding::DeviceSnapshotMessage>,
        baseline_only: bool,
        include_current_snapshot: bool,
    ) -> Result<String, &'static str>;
}

impl Runtime for Stores {
    async fn current_handoff(
        &self,
    ) -> Result<Option<proletarius_api_contract::SmsForwardingHandoffV1>, &'static str> {
        let handoffs = self.handoffs.clone();
        crate::archive_io::run(move || handoffs.current())
            .await
            .map_err(|_| "device_sms_handoff_unavailable")
    }

    async fn current_progress(
        &self,
    ) -> Result<Option<crate::device_forwarding_store::Progress>, &'static str> {
        let admission = self.admission.clone();
        crate::archive_io::run(move || admission.current())
            .await
            .map_err(|_| "device_sms_admission_unavailable")?
            .map_or(Ok(None), |progress| Ok(Some(progress)))
    }

    async fn forwarding_enabled(&self) -> Result<bool, &'static str> {
        self.forwarding.admission_enabled().await
    }

    async fn inbox(&self) -> Result<InboxSnapshot, &'static str> {
        let items = crate::message_archive::forwarding_owned_list(&self.inbox).await?;
        let messages: Vec<UpstreamSMS> = serde_json::from_value(serde_json::Value::Array(items))
            .map_err(|_| "device_sms_source_invalid")?;
        build_snapshot(messages)
    }

    async fn submit(
        &self,
        current_notification_ids: Vec<String>,
        messages: Vec<crate::sms_forwarding::DeviceSnapshotMessage>,
        baseline_only: bool,
        include_current_snapshot: bool,
    ) -> Result<String, &'static str> {
        self.forwarding
            .submit_device_snapshot_from_rust(
                current_notification_ids,
                messages,
                baseline_only,
                include_current_snapshot,
            )
            .await
    }
}

pub(crate) async fn run(
    inbox: crate::message_archive::InboxState,
    handoffs: crate::forwarding_handoff_store::Store,
    admission: crate::device_forwarding_store::Store,
    forwarding: crate::sms_forwarding::Service,
) {
    let runtime = Stores {
        inbox,
        handoffs,
        admission,
        forwarding,
    };
    let mut ticker = interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_current_snapshot: Option<Vec<String>> = None;
    loop {
        ticker.tick().await;
        if let Err(code) = poll_once(&runtime, &mut last_current_snapshot).await {
            last_current_snapshot = None;
            // Error codes are static and never include SMS content or sender.
            crate::diagnostics::write_line(format_args!(
                "switchd-rs device SMS forwarding: {code}"
            ));
        }
    }
}

async fn poll_once(
    runtime: &impl Runtime,
    last_current_snapshot: &mut Option<Vec<String>>,
) -> Result<(), &'static str> {
    if !runtime.forwarding_enabled().await? {
        *last_current_snapshot = None;
        return Ok(());
    }
    let Some(handoff) = runtime.current_handoff().await? else {
        // Swift still owns the one-time migration snapshot. Do not start a
        // second producer until the handoff is durable.
        *last_current_snapshot = None;
        return Ok(());
    };

    let progress = runtime.current_progress().await?;
    if let Some(progress) = progress.as_ref()
        && progress.handoff_id.as_deref() != Some(handoff.handoff_id.as_str())
    {
        return Err("device_sms_handoff_conflict");
    }
    let baseline_only = progress
        .as_ref()
        .is_none_or(|progress| progress.was_enabled != Some(true) || progress.skip_next_snapshot);
    let snapshot = runtime.inbox().await?;

    if baseline_only {
        let current = snapshot.current_notification_ids.clone();
        runtime
            .submit(snapshot.current_notification_ids, Vec::new(), true, true)
            .await?;
        *last_current_snapshot = Some(current);
        return Ok(());
    }

    let current_changed =
        last_current_snapshot.as_ref() != Some(&snapshot.current_notification_ids);
    if current_changed {
        let disposition = runtime
            .submit(
                snapshot.current_notification_ids.clone(),
                Vec::new(),
                false,
                true,
            )
            .await?;
        *last_current_snapshot = Some(snapshot.current_notification_ids.clone());
        if disposition != "accepted" {
            // A disable/re-enable transition or configuration race requires a
            // fresh baseline before any pending message may be admitted.
            return Ok(());
        }
    }

    let accepted: BTreeSet<_> = progress
        .as_ref()
        .map(|progress| progress.accepted_notification_ids.iter().cloned().collect())
        .unwrap_or_default();
    let pending: Vec<_> = snapshot
        .messages
        .into_iter()
        .filter(|message| !accepted.contains(&message.notification_id))
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    for batch in message_batches(&pending) {
        let disposition = runtime.submit(Vec::new(), batch, false, false).await?;
        if disposition != "accepted" {
            *last_current_snapshot = None;
            return Ok(());
        }
    }
    *last_current_snapshot = Some(snapshot.current_notification_ids);
    Ok(())
}

fn message_batches(
    messages: &[crate::sms_forwarding::DeviceSnapshotMessage],
) -> Vec<Vec<crate::sms_forwarding::DeviceSnapshotMessage>> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut estimated_bytes = 0_usize;
    for (index, message) in messages.iter().enumerate() {
        let item_bytes = message.notification_id.len()
            + message.id.len()
            + message.direction.len()
            + message.sender.len()
            + message.content.len()
            + message.timestamp.len()
            + message.device.len()
            + 64;
        if index > start
            && (index - start >= MAX_FORWARDING_BATCH
                || estimated_bytes.saturating_add(item_bytes) > MAX_FORWARDING_BATCH_FIELD_BYTES)
        {
            result.push(messages[start..index].to_vec());
            start = index;
            estimated_bytes = 0;
        }
        estimated_bytes = estimated_bytes.saturating_add(item_bytes);
    }
    if start < messages.len() {
        result.push(messages[start..].to_vec());
    }
    result
}

fn build_snapshot(items: Vec<UpstreamSMS>) -> Result<InboxSnapshot, &'static str> {
    if items.len() > MAX_INBOX_MESSAGES {
        return Err("device_sms_source_limit_exceeded");
    }
    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    for (source_order, item) in items.into_iter().enumerate() {
        if item.direction == "out" {
            continue;
        }
        let (instant, swift_timestamp) = parse_swift_date(&item.timestamp)?;
        let device = item.device.as_deref().unwrap_or("legacy");
        let mut identity = format!(
            "{}|{}|{}|{}",
            device, swift_timestamp, item.sender, item.content
        );
        if let Some(subscription) = item
            .subscription_id
            .as_deref()
            .and_then(normalized_subscription)
        {
            identity = format!("subscription|{subscription}|{identity}");
        }
        let notification_id = sha256_hex(identity.as_bytes());
        if !seen.insert(notification_id.clone()) {
            continue;
        }
        let message_id = item
            .id
            .as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(&notification_id);
        let id = format!("device:{message_id}");
        if id.len() > 4096
            || item.sender.len() > 1024
            || item.content.len() > 65_536
            || device.len() > 256
        {
            return Err("device_sms_message_limit_exceeded");
        }
        records.push((
            instant,
            source_order,
            notification_id.clone(),
            crate::sms_forwarding::DeviceSnapshotMessage {
                notification_id,
                id,
                direction: "in".to_owned(),
                sender: item.sender,
                content: item.content,
                timestamp: item.timestamp,
                device: if device == "legacy" {
                    "设备".to_owned()
                } else {
                    device.to_owned()
                },
            },
        ));
    }
    // Swift submits messages oldest-first while preserving API order for ties.
    records.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    let messages: Vec<_> = records
        .into_iter()
        .map(|(_, _, _, message)| message)
        .collect();
    let current_notification_ids = seen.into_iter().collect();
    Ok(InboxSnapshot {
        current_notification_ids,
        messages,
    })
}

fn parse_swift_date(value: &str) -> Result<(OffsetDateTime, String), &'static str> {
    let parsed =
        OffsetDateTime::parse(value, &Rfc3339).map_err(|_| "device_sms_timestamp_invalid")?;
    // APIClient uses ISO8601DateFormatter with fractional seconds. Its supported
    // precision is milliseconds, including the explicit >3-digit fallback.
    let millis = parsed.nanosecond() / 1_000_000;
    let value = (parsed.unix_timestamp() as f64 + f64::from(millis) / 1000.0).to_string();
    let swift_timestamp = if value.contains('.') {
        value
    } else {
        format!("{value}.0")
    };
    Ok((parsed.to_offset(UtcOffset::UTC), swift_timestamp))
}

fn normalized_subscription(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    (bytes.len() == 69
        && bytes.starts_with(b"sub1_")
        && bytes[5..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)))
    .then_some(value)
}

fn sha256_hex(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn sms(id: &str, timestamp: &str, direction: &str) -> UpstreamSMS {
        UpstreamSMS {
            id: Some(id.to_owned()),
            sender: "65075".to_owned(),
            content: "PAC".to_owned(),
            timestamp: timestamp.to_owned(),
            direction: direction.to_owned(),
            device: Some("dji".to_owned()),
            subscription_id: None,
        }
    }

    #[test]
    fn notification_ids_match_swift_date_and_string_interpolation_vectors() {
        let vectors = [
            (
                "2026-10-04T12:34:56Z",
                "4a8c10b44ae80304162e88cb64ff42b12c2778a51a8c39f60498de2648456644",
            ),
            (
                "2026-10-04T12:34:56.1Z",
                "9ab3b4ed1bd438a30916c3154834ce605636a69e0d32bcb11b84cacf32c571f4",
            ),
            (
                "2026-10-04T12:34:56.125Z",
                "0142075ebf7df48d1610d8b60fbb68f8c10d00a17754bbf6514ffc9de7f2a3f7",
            ),
            (
                "2026-10-04T12:34:56.123456789Z",
                "86582843cac33f05460bcada687b0298ecd5786f3af86509b29bb04cf9a92c72",
            ),
            (
                "2026-10-04T14:34:56.125+02:00",
                "0142075ebf7df48d1610d8b60fbb68f8c10d00a17754bbf6514ffc9de7f2a3f7",
            ),
        ];
        for (timestamp, expected) in vectors {
            let (instant, seconds) = parse_swift_date(timestamp).unwrap();
            assert_eq!(
                sha256_hex(format!("dji|{seconds}|65075|PAC").as_bytes()),
                expected,
                "timestamp {timestamp}"
            );
            assert_eq!(instant.unix_timestamp(), 1_791_117_296);
        }
    }

    #[test]
    fn device_source_excludes_outgoing_preserves_backend_ids_and_sorts_oldest_first() {
        let snapshot = build_snapshot(vec![
            sms("newer-id", "2026-10-04T12:34:57Z", "in"),
            sms("outgoing-id", "2026-10-04T12:34:58Z", "out"),
            sms("older-id", "2026-10-04T12:34:56Z", ""),
        ])
        .unwrap();
        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].id, "device:older-id");
        assert_eq!(snapshot.messages[0].direction, "in");
        assert_eq!(snapshot.messages[1].id, "device:newer-id");
        assert_eq!(snapshot.current_notification_ids.len(), 2);
    }

    #[test]
    fn duplicate_swift_notification_identity_is_admitted_once() {
        let mut first = sms("backend-id-1", "2026-10-04T12:34:56.125Z", "in");
        let second = sms("backend-id-2", "2026-10-04T12:34:56.125Z", "in");
        first.device = Some("dji".to_owned());
        let snapshot = build_snapshot(vec![first, second]).unwrap();
        assert_eq!(snapshot.messages.len(), 1);
        assert_eq!(snapshot.messages[0].id, "device:backend-id-1");
        assert_eq!(snapshot.current_notification_ids.len(), 1);
    }

    #[test]
    fn subscription_scoping_matches_swift_and_invalid_subscription_is_ignored() {
        let base = "dji|1791117296.125|65075|PAC";
        let subscription = format!("sub1_{}", "a".repeat(64));
        let expected = sha256_hex(format!("subscription|{subscription}|{base}").as_bytes());
        let (instant, swift_timestamp) = parse_swift_date("2026-10-04T12:34:56.125Z").unwrap();
        let item = UpstreamSMS {
            subscription_id: Some(subscription),
            ..sms("id", "2026-10-04T12:34:56.125Z", "in")
        };
        let snapshot = build_snapshot(vec![item]).unwrap();
        assert_eq!(snapshot.current_notification_ids, vec![expected]);
        assert_eq!(instant.unix_timestamp(), 1_791_117_296);
        assert_eq!(swift_timestamp, "1791117296.125");

        let item = UpstreamSMS {
            subscription_id: Some(format!("sub1_{}", "A".repeat(64))),
            ..sms("id", "2026-10-04T12:34:56.125Z", "in")
        };
        let snapshot = build_snapshot(vec![item]).unwrap();
        assert_eq!(
            snapshot.current_notification_ids,
            vec![sha256_hex(base.as_bytes())]
        );
    }

    #[test]
    fn invalid_timestamps_and_oversized_fields_fail_closed() {
        assert_eq!(
            build_snapshot(vec![sms("id", "not-a-date", "in")]).unwrap_err(),
            "device_sms_timestamp_invalid"
        );
        let mut item = sms("id", "2026-10-04T12:34:56Z", "in");
        item.content = "x".repeat(65_537);
        assert_eq!(
            build_snapshot(vec![item]).unwrap_err(),
            "device_sms_message_limit_exceeded"
        );
        assert_eq!(
            build_snapshot(vec![
                sms("id", "2026-10-04T12:34:56Z", "in");
                MAX_INBOX_MESSAGES + 1
            ])
            .unwrap_err(),
            "device_sms_source_limit_exceeded"
        );
    }

    #[derive(Clone)]
    struct FakeRuntime(Arc<Mutex<FakeState>>);

    struct FakeState {
        handoff: Option<proletarius_api_contract::SmsForwardingHandoffV1>,
        progress: Option<crate::device_forwarding_store::Progress>,
        enabled: bool,
        admission_error: Option<&'static str>,
        inbox: InboxSnapshot,
        reads: usize,
        submissions: Vec<Submission>,
        next_disposition: Option<String>,
    }

    #[derive(Clone)]
    struct Submission {
        current_ids: Vec<String>,
        messages: Vec<crate::sms_forwarding::DeviceSnapshotMessage>,
        baseline_only: bool,
        include_snapshot: bool,
    }

    impl FakeRuntime {
        fn new(
            enabled: bool,
            progress: Option<crate::device_forwarding_store::Progress>,
            inbox: InboxSnapshot,
        ) -> Self {
            let handoff = serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "handoff_id": "123e4567-e89b-12d3-a456-426614174000",
                "device_sms": {"accepted_notification_ids": [], "was_enabled": true},
                "nwait_sms": {"cursor": "0", "seeded": true, "was_enabled": true}
            }))
            .unwrap();
            Self(Arc::new(Mutex::new(FakeState {
                handoff: Some(handoff),
                progress,
                enabled,
                admission_error: None,
                inbox,
                reads: 0,
                submissions: Vec::new(),
                next_disposition: None,
            })))
        }

        fn progress(
            enabled: bool,
            skip: bool,
            ids: Vec<String>,
        ) -> crate::device_forwarding_store::Progress {
            serde_json::from_value(serde_json::json!({
                "schema_version":1,
                "handoff_id":"123e4567-e89b-12d3-a456-426614174000",
                "accepted_notification_ids":ids,
                "was_enabled":enabled,
                "skip_next_snapshot":skip
            }))
            .unwrap()
        }

        fn submissions(&self) -> Vec<Submission> {
            self.0.lock().unwrap().submissions.clone()
        }
    }

    impl Runtime for FakeRuntime {
        async fn current_handoff(
            &self,
        ) -> Result<Option<proletarius_api_contract::SmsForwardingHandoffV1>, &'static str>
        {
            Ok(self.0.lock().unwrap().handoff.clone())
        }

        async fn current_progress(
            &self,
        ) -> Result<Option<crate::device_forwarding_store::Progress>, &'static str> {
            Ok(self.0.lock().unwrap().progress.clone())
        }

        async fn forwarding_enabled(&self) -> Result<bool, &'static str> {
            let state = self.0.lock().unwrap();
            state.admission_error.map_or(Ok(state.enabled), Err)
        }

        async fn inbox(&self) -> Result<InboxSnapshot, &'static str> {
            let mut state = self.0.lock().unwrap();
            state.reads += 1;
            Ok(state.inbox.clone())
        }

        async fn submit(
            &self,
            current_notification_ids: Vec<String>,
            messages: Vec<crate::sms_forwarding::DeviceSnapshotMessage>,
            baseline_only: bool,
            include_current_snapshot: bool,
        ) -> Result<String, &'static str> {
            let mut state = self.0.lock().unwrap();
            if baseline_only {
                state.progress = Some(Self::progress(
                    true,
                    false,
                    current_notification_ids.clone(),
                ));
            } else if let Some(progress) = state.progress.as_ref() {
                let mut ids = progress.accepted_notification_ids.clone();
                ids.extend(
                    messages
                        .iter()
                        .map(|message| message.notification_id.clone()),
                );
                ids.sort();
                ids.dedup();
                state.progress = Some(Self::progress(true, false, ids));
            }
            state.submissions.push(Submission {
                current_ids: current_notification_ids,
                messages,
                baseline_only,
                include_snapshot: include_current_snapshot,
            });
            if let Some(disposition) = state.next_disposition.take() {
                return Ok(disposition);
            }
            Ok(if baseline_only {
                "baseline"
            } else {
                "accepted"
            }
            .to_owned())
        }
    }

    fn synthetic_snapshot(count: usize) -> InboxSnapshot {
        let messages: Vec<_> = (0..count)
            .map(|index| crate::sms_forwarding::DeviceSnapshotMessage {
                notification_id: format!("{index:064x}"),
                id: format!("device:{index}"),
                direction: "in".to_owned(),
                sender: format!("sender-{index}"),
                content: "synthetic".to_owned(),
                timestamp: "2026-10-04T12:34:56Z".to_owned(),
                device: "reader".to_owned(),
            })
            .collect();
        InboxSnapshot {
            current_notification_ids: messages
                .iter()
                .map(|message| message.notification_id.clone())
                .collect(),
            messages,
        }
    }

    #[tokio::test]
    async fn uncertain_configuration_does_not_read_or_baseline_or_submit_device_sms() {
        for code in [
            "sms_forwarding_configuration_unconfirmed",
            "sms_forwarding_configuration_pending",
            "sms_forwarding_stopping",
            "sms_forwarding_runner_panicked",
            "device_forwarding_admission_unavailable",
        ] {
            let snapshot = synthetic_snapshot(3);
            let progress = FakeRuntime::progress(true, false, Vec::new());
            let original = progress.clone();
            let runtime = FakeRuntime::new(true, Some(progress), snapshot.clone());
            runtime.0.lock().unwrap().admission_error = Some(code);
            let mut last = Some(snapshot.current_notification_ids.clone());
            assert_eq!(poll_once(&runtime, &mut last).await, Err(code));
            assert_eq!(last, Some(snapshot.current_notification_ids));
            {
                let state = runtime.0.lock().unwrap();
                assert_eq!(state.progress, Some(original));
                assert_eq!(state.reads, 0);
                assert!(state.submissions.is_empty());
            }
            runtime.0.lock().unwrap().admission_error = None;
            poll_once(&runtime, &mut last).await.unwrap();
            let submissions = runtime.submissions();
            assert_eq!(submissions.iter().flat_map(|s| &s.messages).count(), 3);
            assert!(!submissions.iter().any(|s| s.baseline_only));
        }
    }

    #[tokio::test]
    async fn initial_or_reenabled_poll_baselines_inbox_before_any_enqueue() {
        let runtime = FakeRuntime::new(true, None, synthetic_snapshot(3));
        let mut cursor = None;
        poll_once(&runtime, &mut cursor).await.unwrap();
        let submissions = runtime.submissions();
        assert_eq!(submissions.len(), 1);
        assert!(submissions[0].baseline_only);
        assert!(submissions[0].include_snapshot);
        assert_eq!(submissions[0].messages.len(), 0);
        assert_eq!(submissions[0].current_ids.len(), 3);

        let runtime = FakeRuntime::new(
            true,
            Some(FakeRuntime::progress(false, true, Vec::new())),
            synthetic_snapshot(2),
        );
        let mut cursor = None;
        poll_once(&runtime, &mut cursor).await.unwrap();
        assert!(runtime.submissions()[0].baseline_only);
    }

    #[tokio::test]
    async fn disabled_poll_does_not_read_or_submit_device_messages() {
        let runtime = FakeRuntime::new(false, None, synthetic_snapshot(1));
        let mut cursor = None;
        poll_once(&runtime, &mut cursor).await.unwrap();
        assert_eq!(runtime.0.lock().unwrap().reads, 0);
        assert!(runtime.submissions().is_empty());
    }

    #[tokio::test]
    async fn already_accepted_ids_are_filtered_and_new_messages_are_batched() {
        let snapshot = synthetic_snapshot(205);
        let accepted_id = snapshot.messages[0].notification_id.clone();
        let runtime = FakeRuntime::new(
            true,
            Some(FakeRuntime::progress(true, false, vec![accepted_id])),
            snapshot,
        );
        let mut cursor = None;
        poll_once(&runtime, &mut cursor).await.unwrap();
        let submissions = runtime.submissions();
        assert_eq!(submissions.len(), 3);
        assert!(submissions[0].messages.is_empty());
        assert!(submissions[0].include_snapshot);
        assert_eq!(submissions[0].current_ids.len(), 205);
        assert_eq!(submissions[1].messages.len(), MAX_FORWARDING_BATCH);
        assert!(!submissions[1].include_snapshot);
        assert!(submissions[1].current_ids.is_empty());
        assert_eq!(submissions[2].messages.len(), 4);
        assert!(!submissions[2].include_snapshot);
    }

    #[tokio::test]
    async fn unchanged_accepted_snapshot_does_not_rewrite_the_admission_ledger() {
        let runtime = FakeRuntime::new(
            true,
            Some(FakeRuntime::progress(true, false, Vec::new())),
            synthetic_snapshot(2),
        );
        let mut cursor = None;
        poll_once(&runtime, &mut cursor).await.unwrap();
        assert_eq!(runtime.submissions().len(), 2);
        poll_once(&runtime, &mut cursor).await.unwrap();
        assert_eq!(runtime.submissions().len(), 2);
        assert_eq!(runtime.0.lock().unwrap().reads, 2);
    }

    #[tokio::test]
    async fn native_archive_source_runs_without_go_child_snapshot() {
        let runtime = FakeRuntime::new(true, None, synthetic_snapshot(1));
        let mut cursor = None;
        assert_eq!(poll_once(&runtime, &mut cursor).await, Ok(()));
        let submissions = runtime.submissions();
        assert_eq!(submissions.len(), 1);
        assert!(submissions[0].baseline_only);
        assert_eq!(submissions[0].current_ids.len(), 1);
    }

    #[tokio::test]
    async fn unknown_snapshot_disposition_does_not_admit_messages() {
        let runtime = FakeRuntime::new(
            true,
            Some(FakeRuntime::progress(true, false, Vec::new())),
            synthetic_snapshot(1),
        );
        runtime.0.lock().unwrap().next_disposition = Some("unknown".into());
        let mut cursor = None;
        poll_once(&runtime, &mut cursor).await.unwrap();
        let submissions = runtime.submissions();
        assert_eq!(submissions.len(), 1);
        assert!(submissions[0].messages.is_empty());
    }

    #[test]
    fn request_body_budget_splits_large_messages_and_keeps_every_record() {
        let mut snapshot = synthetic_snapshot(25);
        for message in &mut snapshot.messages {
            message.content = "x".repeat(100_000);
        }
        let batches = message_batches(&snapshot.messages);
        assert!(batches.len() > 2);
        assert!(
            batches
                .iter()
                .all(|batch| batch.len() <= MAX_FORWARDING_BATCH)
        );
        assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), 25);
        assert!(batches.iter().all(|batch| {
            batch
                .iter()
                .map(|message| {
                    message.notification_id.len()
                        + message.id.len()
                        + message.direction.len()
                        + message.sender.len()
                        + message.content.len()
                        + message.timestamp.len()
                        + message.device.len()
                        + 64
                })
                .sum::<usize>()
                <= MAX_FORWARDING_BATCH_FIELD_BYTES
        }));
    }
}
