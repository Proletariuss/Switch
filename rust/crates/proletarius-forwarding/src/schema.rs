//! Read-only projection of the legacy Go diskState schema.
//!
//! The projection deliberately omits SMS bodies, provider credentials and
//! destination identities. It never serializes back to disk; `JsonDocument`
//! remains the byte-preserving source of truth until a complete typed writer
//! and historical-schema coverage are available.

use serde::Deserialize;
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use zeroize::Zeroizing;

use crate::{
    DestinationBinding, ForwardingChannel, ForwardingDelivery, ForwardingMessage, StateError,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardingQueueSummary {
    pub seeded: bool,
    pub seen_entries: usize,
    pub tasks: Vec<ForwardingTaskSummary>,
    /// Recent terminal diagnostics, newest first as in the Go status API.
    pub recent: Vec<ForwardingRecentTaskSummary>,
    pub recent_diagnostics: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardingTaskSummary {
    /// Opaque queue key used by existing local queue actions.
    pub id: String,
    pub updated_at: String,
    pub telegram: ForwardingDestinationSummary,
    pub bark: ForwardingDestinationSummary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardingDestinationSummary {
    pub completed: bool,
    pub attempts: i64,
    pub destination_bound: bool,
    pub destination_matches_current: bool,
    pub result_unknown: bool,
    pub permanent_failure: bool,
    pub cancelled: bool,
    pub not_requested: bool,
    pub error_code: Option<String>,
    pub next_attempt_at: Option<String>,
    pub last_attempt_at: Option<String>,
    /// True only for the current QueueStore's volatile provider reservation.
    pub in_flight: bool,
    pub telegram_checkpoint: Option<TelegramCheckpointSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardingRecentTaskSummary {
    pub id: String,
    pub updated_at: String,
    pub telegram: ForwardingRecentChannelSummary,
    pub bark: ForwardingRecentChannelSummary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardingRecentChannelSummary {
    pub state: String,
    pub attempts: i64,
    pub error_code: Option<String>,
    pub next_attempt_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub confirmed_parts: Option<i64>,
    pub total_parts: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramCheckpointSummary {
    pub format_version: i64,
    pub confirmed_parts: i64,
    pub total_parts: usize,
    /// Checks version, non-empty <=4,000-rune chunks and confirmed bounds.
    pub chunk_shape_valid: bool,
    pub digests_present: bool,
    /// SHA-256 checkpoints match Go's `encoding/json` serialization of the
    /// message and frozen chunk list. A false value is never recovery-eligible.
    pub digests_valid: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GoDiskState {
    #[serde(deserialize_with = "deserialize_null_default")]
    seeded: bool,
    seen: GoSeenSummary,
    pending: GoPendingSummary,
    recent: GoRecentSummary,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GoDelivery {
    #[serde(deserialize_with = "deserialize_null_default")]
    message: GoSmsMessage,
    #[serde(deserialize_with = "deserialize_null_default")]
    telegram_done: bool,
    #[serde(deserialize_with = "deserialize_null_default")]
    bark_done: bool,
    #[serde(deserialize_with = "deserialize_null_default")]
    telegram_try: i64,
    #[serde(deserialize_with = "deserialize_null_default")]
    bark_try: i64,
    #[serde(deserialize_with = "deserialize_null_default")]
    telegram_at: GoTimestamp,
    #[serde(deserialize_with = "deserialize_null_default")]
    bark_at: GoTimestamp,
    #[serde(deserialize_with = "deserialize_null_default")]
    updated_at: GoTimestamp,
    #[serde(deserialize_with = "deserialize_null_default")]
    telegram_meta: GoDestinationMeta,
    #[serde(deserialize_with = "deserialize_null_default")]
    bark_meta: GoDestinationMeta,
    telegram_progress: Option<GoTelegramProgress>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GoDestinationMeta {
    #[serde(rename = "target_identity")]
    #[serde(deserialize_with = "deserialize_null_default")]
    target_identity: Zeroizing<String>,
    #[serde(deserialize_with = "deserialize_null_default")]
    error_code: Zeroizing<String>,
    #[serde(deserialize_with = "deserialize_null_default")]
    last_attempt_at: GoTimestamp,
    #[serde(deserialize_with = "deserialize_null_default")]
    permanent: bool,
    #[serde(deserialize_with = "deserialize_null_default")]
    result_unknown: bool,
    #[serde(deserialize_with = "deserialize_null_default")]
    cancelled: bool,
    #[serde(deserialize_with = "deserialize_null_default")]
    not_requested: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GoTelegramProgress {
    #[serde(deserialize_with = "deserialize_null_default")]
    format_version: i64,
    message_digest: GoDigest,
    content_digest: GoDigest,
    chunks: GoChunkSummary,
    #[serde(deserialize_with = "deserialize_null_default")]
    confirmed: i64,
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Default)]
struct GoSmsMessage {
    id: Zeroizing<String>,
    direction: Zeroizing<String>,
    sender: Zeroizing<String>,
    content: Zeroizing<String>,
    timestamp: GoTimestamp,
    device: Zeroizing<String>,
}

impl<'de> Deserialize<'de> for GoSmsMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Fields {
            #[serde(deserialize_with = "deserialize_null_default")]
            id: Zeroizing<String>,
            #[serde(deserialize_with = "deserialize_null_default")]
            direction: Zeroizing<String>,
            #[serde(deserialize_with = "deserialize_null_default")]
            sender: Zeroizing<String>,
            #[serde(deserialize_with = "deserialize_null_default")]
            content: Zeroizing<String>,
            #[serde(deserialize_with = "deserialize_null_default")]
            timestamp: GoTimestamp,
            #[serde(deserialize_with = "deserialize_null_default")]
            device: Zeroizing<String>,
        }

        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            id: fields.id,
            direction: fields.direction,
            sender: fields.sender,
            content: fields.content,
            timestamp: fields.timestamp,
            device: fields.device,
        })
    }
}

struct GoTimestamp(Zeroizing<String>);

impl Default for GoTimestamp {
    fn default() -> Self {
        Self(Zeroizing::new("0001-01-01T00:00:00Z".to_owned()))
    }
}

impl GoTimestamp {
    fn order_key(&self) -> Option<(i64, u32)> {
        go_timestamp_order_key(&self.0)
    }

    fn nonzero_string(&self) -> Option<String> {
        (self.0.as_str() != "0001-01-01T00:00:00Z").then(|| self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for GoTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TimestampString;

        impl Visitor<'_> for TimestampString {
            type Value = GoTimestamp;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Go time.Time RFC3339 JSON string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let canonical = canonical_go_timestamp(value)
                    .ok_or_else(|| E::custom("invalid Go time.Time JSON string"))?;
                Ok(GoTimestamp(Zeroizing::new(canonical)))
            }

            fn visit_borrowed_str<E>(self, value: &'_ str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(value)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let value = Zeroizing::new(value);
                self.visit_str(&value)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(GoTimestamp::default())
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_unit()
            }
        }

        deserializer.deserialize_any(TimestampString)
    }
}

#[derive(Default)]
struct GoDigest(Zeroizing<String>);

impl<'de> Deserialize<'de> for GoDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DigestString;

        impl Visitor<'_> for DigestString {
            type Value = GoDigest;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Go SHA-256 digest string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(GoDigest(Zeroizing::new(value.to_owned())))
            }

            fn visit_borrowed_str<E>(self, value: &'_ str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(value)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(GoDigest(Zeroizing::new(value)))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(GoDigest::default())
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_unit()
            }
        }

        deserializer.deserialize_any(DigestString)
    }
}

struct GoChunkSummary {
    count: usize,
    all_valid: bool,
    digest: [u8; 32],
    values: Vec<Zeroizing<String>>,
}

impl Default for GoChunkSummary {
    fn default() -> Self {
        Self::nil_slice()
    }
}

impl GoChunkSummary {
    fn nil_slice() -> Self {
        Self {
            count: 0,
            all_valid: false,
            digest: Sha256::digest(b"null").into(),
            values: Vec::new(),
        }
    }
}

#[derive(Default)]
struct GoPendingSummary(BTreeMap<String, GoDelivery>);

impl<'de> Deserialize<'de> for GoPendingSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PendingMap;

        impl<'de> Visitor<'de> for PendingMap {
            type Value = GoPendingSummary;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("the Go pending-delivery map or null")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut entries = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, Option<GoDelivery>>()? {
                    entries.insert(key, value.unwrap_or_default());
                }
                Ok(GoPendingSummary(entries))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(GoPendingSummary::default())
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_unit()
            }
        }

        deserializer.deserialize_any(PendingMap)
    }
}

#[derive(Default)]
struct GoSeenSummary {
    count: usize,
}

impl<'de> Deserialize<'de> for GoSeenSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SeenMap;

        impl<'de> Visitor<'de> for SeenMap {
            type Value = GoSeenSummary;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("the Go seen-ID map")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut count = 0_usize;
                while map.next_entry::<IgnoredAny, Option<i64>>()?.is_some() {
                    count = count.saturating_add(1);
                }
                Ok(GoSeenSummary { count })
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(GoSeenSummary::default())
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_unit()
            }
        }

        deserializer.deserialize_any(SeenMap)
    }
}

#[derive(Default)]
struct GoRecentSummary {
    count: usize,
    newest: VecDeque<GoRecentTask>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GoRecentTask {
    id: String,
    telegram: GoRecentChannel,
    bark: GoRecentChannel,
    #[serde(deserialize_with = "deserialize_null_default")]
    updated_at: GoTimestamp,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GoRecentChannel {
    state: String,
    #[serde(deserialize_with = "deserialize_null_default")]
    attempts: i64,
    #[serde(deserialize_with = "deserialize_null_default")]
    error_code: Zeroizing<String>,
    #[serde(deserialize_with = "deserialize_null_default")]
    next_attempt_at: Option<GoTimestamp>,
    #[serde(deserialize_with = "deserialize_null_default")]
    last_attempt_at: Option<GoTimestamp>,
    #[serde(deserialize_with = "deserialize_null_default")]
    confirmed_parts: Option<i64>,
    #[serde(deserialize_with = "deserialize_null_default")]
    total_parts: Option<usize>,
}

impl<'de> Deserialize<'de> for GoRecentSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RecentSequence;

        impl<'de> Visitor<'de> for RecentSequence {
            type Value = GoRecentSummary;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("the Go recent diagnostic array")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut count = 0_usize;
                let mut newest = VecDeque::with_capacity(100);
                while let Some(item) = sequence.next_element::<Option<GoRecentTask>>()? {
                    count = count.saturating_add(1);
                    if newest.len() == 100 {
                        newest.pop_front();
                    }
                    newest.push_back(item.unwrap_or_default());
                }
                Ok(GoRecentSummary { count, newest })
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(GoRecentSummary::default())
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_unit()
            }
        }

        deserializer.deserialize_any(RecentSequence)
    }
}

impl<'de> Deserialize<'de> for GoChunkSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ChunkSequence;

        impl<'de> Visitor<'de> for ChunkSequence {
            type Value = GoChunkSummary;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array of Go Telegram chunk strings")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut summary = GoChunkSummary {
                    count: 0,
                    all_valid: true,
                    digest: [0; 32],
                    values: Vec::new(),
                };
                let mut hasher = Sha256::new();
                hasher.update(b"[");
                while let Some(chunk) = sequence.next_element::<GoChunkMetric>()? {
                    if summary.count > 0 {
                        hasher.update(b",");
                    }
                    update_go_json_string(&mut hasher, &chunk.value);
                    summary.count = summary.count.saturating_add(1);
                    summary.all_valid &= chunk.valid && !chunk.was_null;
                    summary.values.push(chunk.value);
                }
                hasher.update(b"]");
                summary.digest = hasher.finalize().into();
                Ok(summary)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(GoChunkSummary::nil_slice())
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_unit()
            }
        }

        deserializer.deserialize_any(ChunkSequence)
    }
}

struct GoChunkMetric {
    value: Zeroizing<String>,
    valid: bool,
    was_null: bool,
}

impl<'de> Deserialize<'de> for GoChunkMetric {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ChunkString;

        impl Visitor<'_> for ChunkString {
            type Value = GoChunkMetric;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string or null, as used by Go []string decoding")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(GoChunkMetric {
                    value: Zeroizing::new(value.to_owned()),
                    valid: !value.is_empty() && value.chars().count() <= 4_000,
                    was_null: false,
                })
            }

            fn visit_borrowed_str<E>(self, value: &'_ str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(value)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let value = Zeroizing::new(value);
                self.visit_str(&value)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(GoChunkMetric {
                    value: Zeroizing::default(),
                    valid: false,
                    was_null: true,
                })
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_unit()
            }
        }

        deserializer.deserialize_any(ChunkString)
    }
}

pub(crate) fn inspect_go_queue(bytes: &[u8]) -> Result<ForwardingQueueSummary, StateError> {
    inspect_go_queue_with_bindings(bytes, None, None)
}

pub(crate) fn inspect_go_queue_with_bindings(
    bytes: &[u8],
    telegram_binding: Option<&DestinationBinding>,
    bark_binding: Option<&DestinationBinding>,
) -> Result<ForwardingQueueSummary, StateError> {
    let state: GoDiskState =
        serde_json::from_slice(bytes).map_err(|_| StateError::InvalidStateSchema)?;
    let telegram_identity = telegram_binding.map(DestinationBinding::to_hex);
    let bark_identity = bark_binding.map(DestinationBinding::to_hex);
    let mut tasks: Vec<_> = state
        .pending
        .0
        .into_iter()
        .map(|(id, delivery)| {
            let message_digest = go_sms_message_digest(&delivery.message);
            let message_timestamp = delivery.message.timestamp.nonzero_string();
            let updated_at = delivery
                .updated_at
                .nonzero_string()
                .or_else(|| {
                    [
                        message_timestamp.clone(),
                        delivery.telegram_meta.last_attempt_at.nonzero_string(),
                        delivery.bark_meta.last_attempt_at.nonzero_string(),
                    ]
                    .into_iter()
                    .flatten()
                    .max_by_key(|value| go_timestamp_order_key(value))
                })
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
            let timestamp_order = delivery
                .message
                .timestamp
                .order_key()
                .unwrap_or((i64::MIN, 0));
            let telegram_checkpoint = delivery.telegram_progress.map(|progress| {
                let chunk_shape_valid = progress.format_version == 1
                    && progress.chunks.count > 0
                    && progress.chunks.all_valid
                    && progress.confirmed >= 0
                    && usize::try_from(progress.confirmed)
                        .is_ok_and(|confirmed| confirmed <= progress.chunks.count);
                let digests_valid = progress.message_digest.matches(&message_digest)
                    && progress.content_digest.matches(&progress.chunks.digest);
                TelegramCheckpointSummary {
                    format_version: progress.format_version,
                    confirmed_parts: progress.confirmed,
                    total_parts: progress.chunks.count,
                    chunk_shape_valid,
                    digests_present: !progress.message_digest.0.is_empty()
                        && !progress.content_digest.0.is_empty(),
                    digests_valid,
                }
            });
            (
                (timestamp_order, id.clone()),
                ForwardingTaskSummary {
                    id,
                    updated_at,
                    telegram: ForwardingDestinationSummary {
                        completed: delivery.telegram_done,
                        attempts: delivery.telegram_try,
                        destination_bound: !delivery.telegram_meta.target_identity.is_empty(),
                        destination_matches_current: telegram_identity.as_deref()
                            == Some(delivery.telegram_meta.target_identity.as_str()),
                        result_unknown: delivery.telegram_meta.result_unknown,
                        permanent_failure: delivery.telegram_meta.permanent,
                        cancelled: delivery.telegram_meta.cancelled,
                        not_requested: delivery.telegram_meta.not_requested,
                        error_code: nonempty(&delivery.telegram_meta.error_code),
                        next_attempt_at: delivery.telegram_at.nonzero_string(),
                        last_attempt_at: delivery.telegram_meta.last_attempt_at.nonzero_string(),
                        in_flight: false,
                        telegram_checkpoint,
                    },
                    bark: ForwardingDestinationSummary {
                        completed: delivery.bark_done,
                        attempts: delivery.bark_try,
                        destination_bound: !delivery.bark_meta.target_identity.is_empty(),
                        destination_matches_current: bark_identity.as_deref()
                            == Some(delivery.bark_meta.target_identity.as_str()),
                        result_unknown: delivery.bark_meta.result_unknown,
                        permanent_failure: delivery.bark_meta.permanent,
                        cancelled: delivery.bark_meta.cancelled,
                        not_requested: delivery.bark_meta.not_requested,
                        error_code: nonempty(&delivery.bark_meta.error_code),
                        next_attempt_at: delivery.bark_at.nonzero_string(),
                        last_attempt_at: delivery.bark_meta.last_attempt_at.nonzero_string(),
                        in_flight: false,
                        telegram_checkpoint: None,
                    },
                },
            )
        })
        .collect();

    tasks.sort_by(|left, right| left.0.cmp(&right.0));
    let tasks = tasks.into_iter().map(|(_, task)| task).collect();
    let recent = state
        .recent
        .newest
        .into_iter()
        .rev()
        .map(|task| ForwardingRecentTaskSummary {
            id: task.id,
            updated_at: task
                .updated_at
                .nonzero_string()
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
            telegram: recent_channel(task.telegram),
            bark: recent_channel(task.bark),
        })
        .collect::<Vec<_>>();
    let recent_diagnostics = state.recent.count;

    Ok(ForwardingQueueSummary {
        seeded: state.seeded,
        seen_entries: state.seen.count,
        tasks,
        recent,
        recent_diagnostics,
    })
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn recent_channel(channel: GoRecentChannel) -> ForwardingRecentChannelSummary {
    ForwardingRecentChannelSummary {
        state: channel.state,
        attempts: channel.attempts,
        error_code: nonempty(&channel.error_code),
        next_attempt_at: channel
            .next_attempt_at
            .and_then(|value| value.nonzero_string()),
        last_attempt_at: channel
            .last_attempt_at
            .and_then(|value| value.nonzero_string()),
        confirmed_parts: channel.confirmed_parts,
        total_parts: channel.total_parts,
    }
}

/// Selects only already-bound, enabled, due destinations. A `None` binding
/// means the corresponding provider is disabled or has no usable sender.
/// The full queue is validated before candidates are copied out, including
/// historical Telegram checkpoint digests.
pub(crate) fn due_deliveries(
    bytes: &[u8],
    now: &str,
    telegram_binding: Option<&DestinationBinding>,
    bark_binding: Option<&DestinationBinding>,
) -> Result<Vec<ForwardingDelivery>, StateError> {
    // Reuse the strict schema and digest checks used by the status view. This
    // makes malformed or tampered checkpoints in any task fail closed.
    let _ = inspect_go_queue(bytes)?;
    let now_key = go_timestamp_order_key(now).ok_or(StateError::InvalidMessage)?;
    let telegram_identity = telegram_binding.map(|binding| Zeroizing::new(binding.to_hex()));
    let bark_identity = bark_binding.map(|binding| Zeroizing::new(binding.to_hex()));
    let state: GoDiskState =
        serde_json::from_slice(bytes).map_err(|_| StateError::InvalidStateSchema)?;
    let mut candidates = Vec::new();

    for (task_id, delivery) in state.pending.0 {
        if delivery
            .message
            .direction
            .trim()
            .eq_ignore_ascii_case("out")
            || delivery
                .message
                .direction
                .trim()
                .eq_ignore_ascii_case("outbound")
        {
            continue;
        }
        let timestamp_key = delivery
            .message
            .timestamp
            .order_key()
            .ok_or(StateError::InvalidStateSchema)?;
        let timestamp_ms = timestamp_key
            .0
            .checked_mul(1_000)
            .and_then(|millis| millis.checked_add((timestamp_key.1 / 1_000_000) as i64))
            .ok_or(StateError::InvalidStateSchema)?;
        let message = delivery.message.as_forwarding_message();

        if let Some(expected) = telegram_identity.as_deref()
            && destination_is_due(
                &delivery.telegram_meta,
                delivery.telegram_done,
                &delivery.telegram_at,
                expected,
                now_key,
            )
        {
            let checkpoint = delivery.telegram_progress.as_ref().and_then(|progress| {
                checkpoint_is_valid(progress, &delivery.message)
                    .then(|| progress.chunks.values.clone())
            });
            if delivery.telegram_progress.is_none() || checkpoint.is_some() {
                let confirmed = delivery
                    .telegram_progress
                    .as_ref()
                    .map_or(0, |progress| progress.confirmed as usize);
                candidates.push((
                    timestamp_key,
                    task_id.clone(),
                    0_u8,
                    ForwardingDelivery::new(
                        task_id.clone(),
                        ForwardingChannel::Telegram,
                        message.clone(),
                        delivery.telegram_try,
                        checkpoint,
                        confirmed,
                        timestamp_ms,
                    ),
                ));
            }
        }

        if let Some(expected) = bark_identity.as_deref()
            && destination_is_due(
                &delivery.bark_meta,
                delivery.bark_done,
                &delivery.bark_at,
                expected,
                now_key,
            )
        {
            candidates.push((
                timestamp_key,
                task_id.clone(),
                1_u8,
                ForwardingDelivery::new(
                    task_id,
                    ForwardingChannel::Bark,
                    message,
                    delivery.bark_try,
                    None,
                    0,
                    timestamp_ms,
                ),
            ));
        }
    }

    candidates.sort_by(|left, right| (left.0, &left.1, left.2).cmp(&(right.0, &right.1, right.2)));
    Ok(candidates
        .into_iter()
        .map(|(_, _, _, candidate)| candidate)
        .collect())
}

fn destination_is_due(
    meta: &GoDestinationMeta,
    done: bool,
    next_attempt: &GoTimestamp,
    expected_identity: &str,
    now: (i64, u32),
) -> bool {
    if meta.not_requested
        || meta.cancelled
        || done
        || meta.permanent
        || meta.result_unknown
        || meta.target_identity.is_empty()
        || meta.target_identity.as_str() != expected_identity
    {
        return false;
    }
    next_attempt
        .order_key()
        .is_some_and(|next_attempt_key| next_attempt_key <= now)
}

fn checkpoint_is_valid(progress: &GoTelegramProgress, message: &GoSmsMessage) -> bool {
    let valid_shape = progress.format_version == 1
        && progress.chunks.count > 0
        && progress.chunks.all_valid
        && progress.confirmed >= 0
        && usize::try_from(progress.confirmed)
            .is_ok_and(|confirmed| confirmed <= progress.chunks.count);
    valid_shape
        && progress
            .message_digest
            .matches(&go_sms_message_digest(message))
        && progress.content_digest.matches(&progress.chunks.digest)
}

pub(crate) fn telegram_progress_json(
    delivery_json: &str,
    chunks: &[Zeroizing<String>],
) -> Result<Zeroizing<String>, StateError> {
    if chunks.is_empty()
        || chunks
            .iter()
            .any(|chunk| chunk.is_empty() || chunk.chars().count() > 4_000)
    {
        return Err(StateError::InvalidMessage);
    }
    let delivery: GoDelivery =
        serde_json::from_str(delivery_json).map_err(|_| StateError::InvalidStateSchema)?;
    if delivery.telegram_progress.is_some() {
        return Err(StateError::InvalidStateSchema);
    }

    let message_digest = digest_hex(&go_sms_message_digest(&delivery.message));
    let mut chunks_hasher = Sha256::new();
    chunks_hasher.update(b"[");
    let mut chunk_json = Zeroizing::new(String::from("["));
    for (index, chunk) in chunks.iter().enumerate() {
        if index > 0 {
            chunks_hasher.update(b",");
            chunk_json.push(',');
        }
        update_go_json_string(&mut chunks_hasher, chunk);
        let encoded = crate::queue::quote_go(chunk)?;
        chunk_json.push_str(&encoded);
    }
    chunks_hasher.update(b"]");
    chunk_json.push(']');
    let content_digest = digest_hex(&chunks_hasher.finalize().into());
    Ok(Zeroizing::new(format!(
        "{{\"format_version\":1,\"message_digest\":\"{message_digest}\",\"content_digest\":\"{content_digest}\",\"chunks\":{},\"confirmed\":0}}",
        chunk_json.as_str()
    )))
}

fn digest_hex(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

impl GoSmsMessage {
    fn as_forwarding_message(&self) -> ForwardingMessage {
        ForwardingMessage::from_queue_fields(
            self.id.clone(),
            self.direction.clone(),
            self.sender.clone(),
            self.content.clone(),
            self.timestamp.0.clone(),
            self.device.clone(),
        )
    }
}

impl GoDigest {
    fn matches(&self, digest: &[u8; 32]) -> bool {
        if self.0.len() != 64 {
            return false;
        }
        let mut expected = [0_u8; 64];
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (index, byte) in digest.iter().enumerate() {
            expected[index * 2] = HEX[(byte >> 4) as usize];
            expected[index * 2 + 1] = HEX[(byte & 0x0f) as usize];
        }
        self.0.as_bytes() == expected
    }
}

fn go_sms_message_digest(message: &GoSmsMessage) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"{\"id\":");
    update_go_json_string(&mut hasher, &message.id);
    hasher.update(b",\"direction\":");
    update_go_json_string(&mut hasher, &message.direction);
    hasher.update(b",\"sender\":");
    update_go_json_string(&mut hasher, &message.sender);
    hasher.update(b",\"content\":");
    update_go_json_string(&mut hasher, &message.content);
    hasher.update(b",\"timestamp\":");
    update_go_json_string(&mut hasher, &message.timestamp.0);
    hasher.update(b",\"device\":");
    update_go_json_string(&mut hasher, &message.device);
    hasher.update(b"}");
    hasher.finalize().into()
}

fn update_go_json_string(hasher: &mut Sha256, value: &str) {
    hasher.update(b"\"");
    for character in value.chars() {
        match character {
            '"' => hasher.update(b"\\\""),
            '\\' => hasher.update(b"\\\\"),
            '\u{0008}' => hasher.update(b"\\b"),
            '\u{000c}' => hasher.update(b"\\f"),
            '\n' => hasher.update(b"\\n"),
            '\r' => hasher.update(b"\\r"),
            '\t' => hasher.update(b"\\t"),
            '<' => hasher.update(b"\\u003c"),
            '>' => hasher.update(b"\\u003e"),
            '&' => hasher.update(b"\\u0026"),
            '\u{2028}' => hasher.update(b"\\u2028"),
            '\u{2029}' => hasher.update(b"\\u2029"),
            control if control <= '\u{001f}' => {
                let scalar = control as u32;
                let mut escaped = *b"\\u0000";
                const HEX: &[u8; 16] = b"0123456789abcdef";
                escaped[4] = HEX[((scalar >> 4) & 0x0f) as usize];
                escaped[5] = HEX[(scalar & 0x0f) as usize];
                hasher.update(escaped);
            }
            other => {
                let mut utf8 = [0_u8; 4];
                hasher.update(other.encode_utf8(&mut utf8).as_bytes());
            }
        }
    }
    hasher.update(b"\"");
}

pub(crate) fn canonical_go_timestamp(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    if !value.is_ascii() || bytes.len() < 20 {
        return None;
    }
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let number = |start: usize, length: usize| -> Option<u32> {
        let slice = bytes.get(start..start + length)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        slice.iter().try_fold(0_u32, |value, digit| {
            value.checked_mul(10)?.checked_add((digit - b'0') as u32)
        })
    };
    let year = number(0, 4)?;
    let month = number(5, 2)?;
    let day = number(8, 2)?;
    let hour = number(11, 2)?;
    let minute = number(14, 2)?;
    let second = number(17, 2)?;
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap_year => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let mut index = 19;
    let mut fraction = None;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        let digits = bytes.get(start..index)?;
        if digits.is_empty() || digits.len() > 9 {
            return None;
        }
        let end = digits
            .iter()
            .rposition(|digit| *digit != b'0')
            .map(|i| i + 1);
        fraction = end.map(|end| &digits[..end]);
    }

    let zone = bytes.get(index..)?;
    let canonical_zone = if zone == b"Z" {
        "Z".to_owned()
    } else {
        if zone.len() != 6 || zone[3] != b':' || !matches!(zone[0], b'+' | b'-') {
            return None;
        }
        if !zone[1..3]
            .iter()
            .chain(zone[4..6].iter())
            .all(u8::is_ascii_digit)
        {
            return None;
        }
        let offset_hour = number(index + 1, 2)?;
        let offset_minute = number(index + 4, 2)?;
        if offset_hour > 23 || offset_minute > 59 {
            return None;
        }
        if offset_hour == 0 && offset_minute == 0 {
            "Z".to_owned()
        } else {
            std::str::from_utf8(zone).ok()?.to_owned()
        }
    };

    let mut canonical = String::with_capacity(30);
    canonical.push_str(std::str::from_utf8(&bytes[..19]).ok()?);
    if let Some(fraction) = fraction {
        canonical.push('.');
        canonical.push_str(std::str::from_utf8(fraction).ok()?);
    }
    canonical.push_str(&canonical_zone);
    Some(canonical)
}

struct GoTimestampParts {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    nanos: u32,
    offset_seconds: i32,
}

fn timestamp_parts(value: &str) -> Option<GoTimestampParts> {
    let canonical = canonical_go_timestamp(value)?;
    let bytes = canonical.as_bytes();
    let number = |start: usize, count: usize| -> Option<u32> {
        bytes
            .get(start..start + count)?
            .iter()
            .try_fold(0_u32, |value, byte| {
                value.checked_mul(10)?.checked_add((byte - b'0') as u32)
            })
    };
    let year = number(0, 4)? as i64;
    let month = number(5, 2)?;
    let day = number(8, 2)?;
    let hour = number(11, 2)?;
    let minute = number(14, 2)?;
    let second = number(17, 2)?;
    let mut offset_index = 19;
    let mut nanos = 0_u32;
    if bytes.get(offset_index) == Some(&b'.') {
        offset_index += 1;
        let start = offset_index;
        while bytes.get(offset_index).is_some_and(u8::is_ascii_digit) {
            offset_index += 1;
        }
        let fraction = bytes.get(start..offset_index)?;
        let mut fraction_text = fraction.to_vec();
        while fraction_text.len() < 9 {
            fraction_text.push(b'0');
        }
        nanos = std::str::from_utf8(&fraction_text).ok()?.parse().ok()?;
    }
    let offset = match bytes.get(offset_index..) {
        Some(zone) if zone == b"Z" => 0,
        Some(zone) if zone.len() == 6 => {
            let hours = std::str::from_utf8(&zone[1..3]).ok()?.parse::<i32>().ok()?;
            let minutes = std::str::from_utf8(&zone[4..6]).ok()?.parse::<i32>().ok()?;
            let sign = if zone[0] == b'-' { -1 } else { 1 };
            sign * (hours * 3600 + minutes * 60)
        }
        _ => return None,
    };
    Some(GoTimestampParts {
        year,
        month,
        day,
        hour,
        minute,
        second,
        nanos,
        offset_seconds: offset,
    })
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let month_prime = month as i64 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

fn timestamp_unix_seconds(parts: &GoTimestampParts) -> Option<i64> {
    days_from_civil(parts.year, parts.month, parts.day)
        .checked_mul(86_400)?
        .checked_add((parts.hour * 3600 + parts.minute * 60 + parts.second) as i64)?
        .checked_sub(parts.offset_seconds as i64)
}

pub(crate) fn go_timestamp_unix_millis(value: &str) -> Option<i64> {
    let (seconds, nanos) = go_timestamp_order_key(value)?;
    seconds
        .checked_mul(1_000)?
        .checked_add((nanos / 1_000_000) as i64)
}

fn go_timestamp_order_key(value: &str) -> Option<(i64, u32)> {
    let parts = timestamp_parts(value)?;
    Some((timestamp_unix_seconds(&parts)?, parts.nanos))
}

pub(crate) fn go_timestamp_utc(value: &str) -> Option<String> {
    let parts = timestamp_parts(value)?;
    let seconds = timestamp_unix_seconds(&parts)?;
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    if !(0..=9999).contains(&year) {
        return None;
    }
    let mut output = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        second_of_day / 3600,
        (second_of_day % 3600) / 60,
        second_of_day % 60
    );
    let nanos = parts.nanos;
    if nanos > 0 {
        let mut fraction = format!("{nanos:09}");
        while fraction.ends_with('0') {
            fraction.pop();
        }
        output.push('.');
        output.push_str(&fraction);
    }
    output.push('Z');
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GO_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/go-state.json");

    #[test]
    fn go_fixture_projects_unknown_and_checkpoint_state_without_exposing_sms_body() {
        let summary = inspect_go_queue(GO_FIXTURE).expect("typed Go state");
        assert!(summary.seeded);
        assert_eq!(summary.seen_entries, 2);
        assert_eq!(summary.tasks.len(), 2);
        assert_eq!(summary.recent_diagnostics, 1);
        assert_eq!(summary.recent.len(), 1);
        assert_eq!(summary.recent[0].id, "fixture-terminal");
        assert_eq!(summary.recent[0].telegram.state, "delivered");
        assert_eq!(summary.recent[0].bark.state, "cancelled");
        assert_eq!(summary.recent[0].telegram.confirmed_parts, Some(2));
        assert_eq!(summary.tasks[0].id, "fixture-legacy");

        let checkpointed = summary
            .tasks
            .iter()
            .find(|task| task.telegram.telegram_checkpoint.is_some())
            .expect("frozen Telegram checkpoint");
        assert_eq!(checkpointed.id.len(), 64);
        assert!(checkpointed.telegram.result_unknown);
        assert_eq!(checkpointed.telegram.attempts, 2);
        let progress = checkpointed.telegram.telegram_checkpoint.as_ref().unwrap();
        assert_eq!(progress.confirmed_parts, 1);
        assert_eq!(progress.total_parts, 2);
        assert!(progress.chunk_shape_valid);
        assert!(progress.digests_present);
        assert!(progress.digests_valid);
        assert!(checkpointed.bark.permanent_failure);
        assert!(checkpointed.bark.destination_bound);
        assert_eq!(
            checkpointed.telegram.error_code.as_deref(),
            Some("telegram_submission_unconfirmed")
        );
        assert_eq!(
            checkpointed.telegram.next_attempt_at.as_deref(),
            Some("2026-01-02T03:05:05.123456789+08:00")
        );

        let legacy = summary
            .tasks
            .iter()
            .find(|task| task.telegram.telegram_checkpoint.is_none())
            .expect("legacy task without checkpoint");
        assert!(!legacy.telegram.destination_bound);
        assert!(!legacy.bark.destination_bound);
    }

    #[test]
    fn typed_inspection_rejects_schema_conflicts_but_keeps_the_original_document_usable() {
        let raw = br#"{ "seeded": true, "seen": {}, "pending": {}, "future": {"x":1} }"#;
        let document = crate::JsonDocument::from_bytes(
            zeroize::Zeroizing::new(raw.to_vec()),
            crate::StateLimits::default(),
        )
        .unwrap();
        assert!(document.inspect_go_queue().unwrap().seeded);
        assert_eq!(document.as_bytes(), raw);

        let malformed = crate::JsonDocument::from_bytes(
            zeroize::Zeroizing::new(br#"{"seeded":"true"}"#.to_vec()),
            crate::StateLimits::default(),
        )
        .unwrap();
        assert_eq!(
            malformed.inspect_go_queue().unwrap_err(),
            StateError::InvalidStateSchema
        );
    }

    #[test]
    fn checkpoint_shape_flags_empty_long_and_out_of_range_chunks() {
        let cases = [
            (r#"{"format_version":1,"chunks":[],"confirmed":0}"#, false),
            (r#"{"format_version":1,"chunks":[""],"confirmed":0}"#, false),
            (
                r#"{"format_version":1,"chunks":["ok"],"confirmed":2}"#,
                false,
            ),
            (
                r#"{"format_version":2,"chunks":["ok"],"confirmed":1}"#,
                false,
            ),
        ];
        for (raw_progress, expected) in cases {
            let raw = format!(r#"{{"pending":{{"task":{{"telegram_progress":{raw_progress}}}}}}}"#);
            let state = inspect_go_queue(raw.as_bytes()).unwrap();
            assert_eq!(
                state.tasks[0]
                    .telegram
                    .telegram_checkpoint
                    .as_ref()
                    .unwrap()
                    .chunk_shape_valid,
                expected
            );
        }
        let long_chunk = "x".repeat(4_001);
        let raw = serde_json::json!({
            "pending": {"task": {"telegram_progress": {
                "format_version": 1, "chunks": [long_chunk], "confirmed": 0
            }}}
        });
        let state = inspect_go_queue(raw.to_string().as_bytes()).unwrap();
        assert!(
            !state.tasks[0]
                .telegram
                .telegram_checkpoint
                .as_ref()
                .unwrap()
                .chunk_shape_valid
        );
    }

    #[test]
    fn go_message_and_frozen_chunk_digests_fail_closed_when_changed() {
        let original = std::str::from_utf8(GO_FIXTURE).unwrap();
        let changed_message = original.replace(
            "\"sender\":\"fixture-sender\"",
            "\"sender\":\"changed-sender\"",
        );
        let changed_message = inspect_go_queue(changed_message.as_bytes()).unwrap();
        let progress = changed_message
            .tasks
            .iter()
            .find_map(|task| task.telegram.telegram_checkpoint.as_ref())
            .unwrap();
        assert!(!progress.digests_valid);

        let changed_chunk =
            original.replacen("发件人：fixture-sender", "发件人：changed-sender", 1);
        let changed_chunk = inspect_go_queue(changed_chunk.as_bytes()).unwrap();
        let progress = changed_chunk
            .tasks
            .iter()
            .find_map(|task| task.telegram.telegram_checkpoint.as_ref())
            .unwrap();
        assert!(!progress.digests_valid);
    }

    #[test]
    fn go_timestamp_canonicalization_trims_fraction_and_normalizes_zero_offset() {
        assert_eq!(
            canonical_go_timestamp("2026-01-02T03:04:05.123456000+00:00").as_deref(),
            Some("2026-01-02T03:04:05.123456Z")
        );
        assert_eq!(
            canonical_go_timestamp("2026-01-02T03:04:05.000000000Z").as_deref(),
            Some("2026-01-02T03:04:05Z")
        );
        assert!(canonical_go_timestamp("2025-02-29T03:04:05Z").is_none());
        assert!(canonical_go_timestamp("2026-01-02T24:00:00Z").is_none());
        assert!(canonical_go_timestamp("2026-01-02T03:04:05.1234567890Z").is_none());
        assert_eq!(
            go_timestamp_utc("2026-01-02T03:04:05.123400+08:00").as_deref(),
            Some("2026-01-01T19:04:05.1234Z")
        );
        assert_eq!(
            go_timestamp_unix_millis("2026-01-02T03:04:05.123400+08:00"),
            Some(1_767_294_245_123)
        );
        assert!(
            go_timestamp_order_key("2026-10-04T12:00:00.000000100Z")
                < go_timestamp_order_key("2026-10-04T12:00:00.000000900Z")
        );
    }

    #[test]
    fn due_delivery_selection_matches_go_order_and_fail_closed_gates() {
        let key =
            crate::StateKey::from_slice(&(0..32).map(|value| value as u8).collect::<Vec<_>>())
                .unwrap();
        let telegram = DestinationBinding::telegram(&key, "bot-token", "chat-1");
        let bark = DestinationBinding::bark(&key, "https://bark.example", "device-key").unwrap();
        let other_telegram = DestinationBinding::telegram(&key, "another-token", "chat-1");
        let telegram_identity = telegram.to_hex();
        let bark_identity = bark.to_hex();
        let zero_time = "0001-01-01T00:00:00Z";
        let make_delivery = |timestamp: &str| {
            serde_json::json!({
                "message": {
                    "id": "sms-id",
                    "direction": "in",
                    "sender": "sender",
                    "content": "private body",
                    "timestamp": timestamp,
                    "device": "line"
                },
                "telegram_done": false,
                "bark_done": false,
                "telegram_try": 2,
                "bark_try": 1,
                "telegram_at": zero_time,
                "bark_at": zero_time,
                "telegram_meta": {
                    "target_identity": telegram_identity,
                    "last_attempt_at": zero_time
                },
                "bark_meta": {
                    "target_identity": bark_identity,
                    "last_attempt_at": zero_time
                }
            })
        };
        let mut pending = serde_json::Map::new();
        pending.insert("z-tie".to_owned(), make_delivery("2026-10-04T10:00:00Z"));
        pending.insert("a-tie".to_owned(), make_delivery("2026-10-04T10:00:00Z"));
        let mut future = make_delivery("2026-10-04T10:01:00Z");
        future["telegram_at"] = serde_json::json!("2026-10-04T12:00:00.000000999Z");
        pending.insert("future".to_owned(), future);
        let mut unknown = make_delivery("2026-10-04T09:00:00Z");
        unknown["telegram_meta"]["result_unknown"] = serde_json::json!(true);
        pending.insert("unknown".to_owned(), unknown);
        let mut changed = make_delivery("2026-10-04T08:00:00Z");
        changed["telegram_meta"]["target_identity"] = serde_json::json!(other_telegram.to_hex());
        pending.insert("changed".to_owned(), changed);

        let bytes = serde_json::to_vec(&serde_json::json!({
            "seeded": true,
            "seen": {},
            "pending": pending
        }))
        .unwrap();
        let candidates = due_deliveries(
            &bytes,
            "2026-10-04T12:00:00.000000500Z",
            Some(&telegram),
            Some(&bark),
        )
        .unwrap();
        let actual = candidates
            .iter()
            .map(|candidate| format!("{}:{:?}", candidate.task_id(), candidate.channel()))
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                "changed:Bark",
                "unknown:Bark",
                "a-tie:Telegram",
                "a-tie:Bark",
                "z-tie:Telegram",
                "z-tie:Bark",
                "future:Bark"
            ]
        );
        assert_eq!(candidates[2].attempts(), 2);
        assert!(candidates[2].needs_telegram_freeze());
        assert!(candidates[2].telegram_chunks().is_none());
        assert_eq!(candidates[2].message().content(), "private body");
        assert!(!format!("{:?}", candidates[2]).contains("private body"));

        let status = inspect_go_queue_with_bindings(&bytes, Some(&telegram), Some(&bark)).unwrap();
        let matched = status.tasks.iter().find(|task| task.id == "a-tie").unwrap();
        assert!(matched.telegram.destination_matches_current);
        assert!(matched.bark.destination_matches_current);

        assert!(
            due_deliveries(&bytes, "2026-10-04T12:00:00.000000500Z", None, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn go_null_state_fields_project_as_their_zero_values_without_mutating_source() {
        let raw = br#"{"seeded":null,"seen":null,"pending":{"legacy-null":null},"recent":null}"#;
        let document = crate::JsonDocument::from_bytes(
            Zeroizing::new(raw.to_vec()),
            crate::StateLimits::default(),
        )
        .unwrap();
        let summary = document.inspect_go_queue().unwrap();
        assert!(!summary.seeded);
        assert_eq!(summary.seen_entries, 0);
        assert_eq!(summary.recent_diagnostics, 0);
        assert_eq!(summary.tasks.len(), 1);
        assert_eq!(summary.tasks[0].id, "legacy-null");
        assert_eq!(summary.tasks[0].telegram.attempts, 0);
        assert!(summary.tasks[0].telegram.telegram_checkpoint.is_none());
        assert_eq!(document.as_bytes(), raw);
    }

    #[test]
    fn sms_body_and_target_hashes_are_not_present_in_summary_debug() {
        let summary = inspect_go_queue(GO_FIXTURE).unwrap();
        let debug = format!("{summary:?}");
        assert!(!debug.contains("fixture-sender"));
        assert!(!debug.contains("fixture-reader"));
        assert!(!debug.contains("<>&"));
        assert!(!debug.contains(&"a".repeat(64)));
        assert!(!debug.contains(&"b".repeat(64)));
    }
}
