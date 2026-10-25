//! Fail-closed mutations of the legacy Go forwarding queue.
//!
//! Queue insertion creates delivery records only; it never submits provider I/O.
//! The root and existing map entries are spliced as raw JSON so unknown Go
//! fields, numeric spellings and frozen provider evidence survive byte-for-byte.

use std::collections::{BTreeMap, HashSet};

use serde::de::{Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{DestinationBinding, JsonDocument, StateError, StateLimits};

const MAX_SEEN_IDS: usize = 10_000;
const ZERO_TIME: &str = "0001-01-01T00:00:00Z";

/// SMS input held in zeroizing strings while admission constructs its encrypted
/// queue record. Debug intentionally omits every message field.
#[derive(Clone)]
pub struct ForwardingMessage {
    id: Zeroizing<String>,
    direction: Zeroizing<String>,
    sender: Zeroizing<String>,
    content: Zeroizing<String>,
    timestamp: Zeroizing<String>,
    device: Zeroizing<String>,
}

impl ForwardingMessage {
    pub fn new(
        id: impl Into<String>,
        direction: impl Into<String>,
        sender: impl Into<String>,
        content: impl Into<String>,
        timestamp: impl Into<String>,
        device: impl Into<String>,
    ) -> Result<Self, StateError> {
        let timestamp = timestamp.into();
        if crate::schema::canonical_go_timestamp(&timestamp).is_none() {
            return Err(StateError::InvalidMessage);
        }
        Ok(Self {
            id: Zeroizing::new(id.into()),
            direction: Zeroizing::new(direction.into()),
            sender: Zeroizing::new(sender.into()),
            content: Zeroizing::new(content.into()),
            timestamp: Zeroizing::new(timestamp),
            device: Zeroizing::new(device.into()),
        })
    }

    pub(crate) fn from_queue_fields(
        id: Zeroizing<String>,
        direction: Zeroizing<String>,
        sender: Zeroizing<String>,
        content: Zeroizing<String>,
        timestamp: Zeroizing<String>,
        device: Zeroizing<String>,
    ) -> Self {
        Self {
            id,
            direction,
            sender,
            content,
            timestamp,
            device,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn direction(&self) -> &str {
        &self.direction
    }

    pub fn sender(&self) -> &str {
        &self.sender
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }

    pub fn device(&self) -> &str {
        &self.device
    }
}

impl std::fmt::Debug for ForwardingMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ForwardingMessage([REDACTED])")
    }
}

/// Provider configuration projection. Identities are the existing Go HMAC
/// values; tokens, chat IDs and Bark keys never enter the persisted queue code.
pub struct EnqueueOptions {
    telegram_enabled: bool,
    telegram_identity: String,
    bark_enabled: bool,
    bark_identity: String,
}

impl EnqueueOptions {
    pub fn new(
        telegram_enabled: bool,
        telegram_identity: &DestinationBinding,
        bark_enabled: bool,
        bark_identity: &DestinationBinding,
    ) -> Self {
        Self {
            telegram_enabled,
            telegram_identity: telegram_identity.to_hex(),
            bark_enabled,
            bark_identity: bark_identity.to_hex(),
        }
    }
}

impl std::fmt::Debug for EnqueueOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EnqueueOptions([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EnqueueSummary {
    pub queued: usize,
    pub pending: usize,
    pub newly_seen: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ForwardingChannel {
    Telegram,
    Bark,
}

/// A due, destination-bound delivery copied from the encrypted queue. SMS
/// fields and frozen Telegram chunks stay in zeroizing buffers and are omitted
/// from Debug output. A missing Telegram checkpoint means a legacy task must
/// freeze its rendering before it can be submitted.
pub struct ForwardingDelivery {
    task_id: Zeroizing<String>,
    channel: ForwardingChannel,
    message: ForwardingMessage,
    attempts: i64,
    telegram_chunks: Option<Vec<Zeroizing<String>>>,
    confirmed_chunks: usize,
    timestamp_unix_millis: i64,
}

impl ForwardingDelivery {
    pub(crate) fn new(
        task_id: String,
        channel: ForwardingChannel,
        message: ForwardingMessage,
        attempts: i64,
        telegram_chunks: Option<Vec<Zeroizing<String>>>,
        confirmed_chunks: usize,
        timestamp_unix_millis: i64,
    ) -> Self {
        Self {
            task_id: Zeroizing::new(task_id),
            channel,
            message,
            attempts,
            telegram_chunks,
            confirmed_chunks,
            timestamp_unix_millis,
        }
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn channel(&self) -> ForwardingChannel {
        self.channel
    }

    pub fn message(&self) -> &ForwardingMessage {
        &self.message
    }

    pub fn attempts(&self) -> i64 {
        self.attempts
    }

    pub fn telegram_chunks(&self) -> Option<&[Zeroizing<String>]> {
        self.telegram_chunks.as_deref()
    }

    pub fn confirmed_chunks(&self) -> usize {
        self.confirmed_chunks
    }

    pub fn timestamp_unix_millis(&self) -> i64 {
        self.timestamp_unix_millis
    }

    pub fn needs_telegram_freeze(&self) -> bool {
        self.channel == ForwardingChannel::Telegram && self.telegram_chunks.is_none()
    }
}

impl std::fmt::Debug for ForwardingDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ForwardingDelivery([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueMutationError {
    TaskNotFound,
    DestinationAlreadyTerminal,
    LegacyDestinationConfirmationNotApplicable,
    DestinationNotCurrent,
    DestinationPaused,
    InvalidDeliveryResult,
    InvalidTelegramCheckpoint,
    InFlight,
    AttemptSequenceExhausted,
    State(StateError),
    StorageIo,
    PersistenceUncertain,
    ReopenRequired,
}

#[derive(Clone, Copy)]
pub(crate) enum DestinationMutationKind {
    BindLegacy,
    Retry,
}

pub(crate) struct DestinationMutation<'a> {
    pub task_id: &'a str,
    pub channel: ForwardingChannel,
    pub kind: DestinationMutationKind,
    pub enabled: bool,
    pub current_binding: &'a DestinationBinding,
    pub updated_at: &'a str,
}

pub(crate) struct TelegramChunkMutation<'a> {
    pub task_id: &'a str,
    pub enabled: bool,
    pub binding: &'a DestinationBinding,
    pub index: usize,
    pub submitting: bool,
    pub updated_at: &'a str,
}

impl std::fmt::Display for QueueMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TaskNotFound => "forwarding_task_not_found",
            Self::DestinationAlreadyTerminal => "forwarding_destination_already_terminal",
            Self::LegacyDestinationConfirmationNotApplicable => {
                "legacy_destination_confirmation_not_applicable"
            }
            Self::DestinationNotCurrent => "forwarding_destination_not_current",
            Self::DestinationPaused => "forwarding_destination_paused",
            Self::InvalidDeliveryResult => "forwarding_delivery_result_invalid",
            Self::InvalidTelegramCheckpoint => "forwarding_telegram_checkpoint_invalid",
            Self::InFlight => "forwarding_task_in_flight",
            Self::AttemptSequenceExhausted => "forwarding_attempt_sequence_exhausted",
            Self::State(error) => return error.fmt(f),
            Self::StorageIo => "forwarding state storage operation failed",
            Self::PersistenceUncertain => {
                "forwarding queue replacement is visible but directory durability is uncertain"
            }
            Self::ReopenRequired => {
                "forwarding state store must be reopened after an uncertain write"
            }
        })
    }
}

/// Provider result to durably commit after a request attempt. Error codes are
/// restricted to safe fixed identifiers; response bodies are never persisted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    Delivered,
    Failed {
        error_code: String,
        permanent: bool,
        result_unknown: bool,
        retry_at: Option<String>,
    },
}

impl std::error::Error for QueueMutationError {}

impl From<StateError> for QueueMutationError {
    fn from(value: StateError) -> Self {
        Self::State(value)
    }
}

#[derive(Clone)]
struct RawField<'a> {
    name: String,
    key_json: &'a str,
    value_json: &'a str,
}

struct RawObject<'a>(Vec<RawField<'a>>);

impl<'de> Deserialize<'de> for RawObject<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = RawObject<'de>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object with unique keys")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut fields = Vec::new();
                let mut names = HashSet::new();
                while let Some(key) = map.next_key::<&'de RawValue>()? {
                    let name: String = serde_json::from_str(key.get()).map_err(A::Error::custom)?;
                    if !names.insert(name.clone()) {
                        return Err(A::Error::custom("duplicate JSON key"));
                    }
                    let value = map.next_value::<&'de RawValue>()?;
                    fields.push(RawField {
                        name,
                        key_json: key.get(),
                        value_json: value.get(),
                    });
                }
                Ok(RawObject(fields))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

fn parse_object(bytes: &str) -> Result<RawObject<'_>, StateError> {
    let mut deserializer = serde_json::Deserializer::from_str(bytes);
    let object = RawObject::deserialize(&mut deserializer).map_err(map_parse_error)?;
    deserializer
        .end()
        .map_err(|_| StateError::InvalidStateSchema)?;
    Ok(object)
}

fn map_parse_error(error: serde_json::Error) -> StateError {
    if error.to_string().contains("duplicate JSON key") {
        StateError::DuplicateJsonKey
    } else {
        StateError::InvalidStateSchema
    }
}

fn parse_map(raw: Option<&str>) -> Result<Vec<RawField<'_>>, StateError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.trim() == "null" {
        return Ok(Vec::new());
    }
    parse_object(raw).map(|object| object.0)
}

fn parse_array(raw: Option<&str>) -> Result<Vec<&RawValue>, StateError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.trim() == "null" {
        return Ok(Vec::new());
    }
    let values =
        serde_json::from_str::<Vec<&RawValue>>(raw).map_err(|_| StateError::InvalidStateSchema)?;
    for value in &values {
        parse_object(value.get())?;
    }
    Ok(values)
}

fn find_field<'a>(
    fields: &'a [RawField<'_>],
    name: &str,
) -> Result<Option<&'a RawField<'a>>, StateError> {
    let mut found = None;
    for field in fields {
        if field.name.eq_ignore_ascii_case(name) {
            if found.is_some() || field.name != name {
                return Err(StateError::DuplicateJsonKey);
            }
            found = Some(field);
        }
    }
    Ok(found)
}

pub(crate) fn quote_go(value: &str) -> Result<Zeroizing<String>, StateError> {
    let mut encoded =
        Zeroizing::new(serde_json::to_string(value).map_err(|_| StateError::InvalidMessage)?);
    for (character, replacement) in [
        ('&', "\\u0026"),
        ('<', "\\u003c"),
        ('>', "\\u003e"),
        ('\u{2028}', "\\u2028"),
        ('\u{2029}', "\\u2029"),
    ] {
        let escaped = encoded.replace(character, replacement);
        *encoded = escaped;
    }
    Ok(encoded)
}

fn validate_identity(identity: &str) -> Result<(), StateError> {
    if identity.len() != 64
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StateError::InvalidTargetIdentity);
    }
    Ok(())
}

fn message_key(message: &ForwardingMessage) -> Result<String, StateError> {
    let explicit = message.id.trim();
    let mut digest = Sha256::new();
    if explicit.is_empty() {
        let utc_timestamp = crate::schema::go_timestamp_utc(&message.timestamp)
            .ok_or(StateError::InvalidMessage)?;
        for (index, component) in [
            message.device.as_bytes(),
            utc_timestamp.as_bytes(),
            message.sender.as_bytes(),
            message.content.as_bytes(),
        ]
        .into_iter()
        .enumerate()
        {
            if index > 0 {
                digest.update([0]);
            }
            digest.update(component);
        }
    } else {
        digest.update(explicit.as_bytes());
    }
    let digest = digest.finalize();
    let mut key = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut key, "{byte:02x}").expect("writing into String cannot fail");
    }
    Ok(key)
}

fn estimate_enqueue_growth(
    message: &ForwardingMessage,
    options: &EnqueueOptions,
) -> Result<usize, StateError> {
    // Any input byte can expand to a six-byte JSON escape. The fixed allowance
    // covers Go's delivery/meta field names, IDs, timestamps and delimiters.
    let raw_string_bytes = [
        message.id.len(),
        message.direction.len(),
        message.sender.len(),
        message.content.len(),
        message.timestamp.len(),
        message.device.len(),
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)
    .ok_or(StateError::SizeLimit)?;
    let mut growth = if options.telegram_enabled || options.bark_enabled {
        raw_string_bytes
            .checked_mul(6)
            .and_then(|size| size.checked_add(1_024))
            .ok_or(StateError::SizeLimit)?
    } else {
        128
    };
    // The seen entry is added even when providers are disabled.
    growth = growth.checked_add(128).ok_or(StateError::SizeLimit)?;
    Ok(growth)
}

fn timestamp_unix_millis(value: &str) -> Result<i64, StateError> {
    crate::schema::go_timestamp_unix_millis(value).ok_or(StateError::InvalidMessage)
}

fn delivery_json(
    message: &ForwardingMessage,
    received_at: &str,
    options: &EnqueueOptions,
) -> Result<Zeroizing<String>, StateError> {
    let message_id = quote_go(&message.id)?;
    let direction = quote_go(&message.direction)?;
    let sender = quote_go(&message.sender)?;
    let content = quote_go(&message.content)?;
    let timestamp = quote_go(
        crate::schema::canonical_go_timestamp(&message.timestamp)
            .ok_or(StateError::InvalidMessage)?
            .as_str(),
    )?;
    let device = quote_go(&message.device)?;
    let message_json = Zeroizing::new(format!(
        "{{\"id\":{},\"direction\":{},\"sender\":{},\"content\":{},\"timestamp\":{},\"device\":{}}}",
        message_id.as_str(),
        direction.as_str(),
        sender.as_str(),
        content.as_str(),
        timestamp.as_str(),
        device.as_str(),
    ));
    let received_at = quote_go(received_at)?;
    let zero_time = quote_go(ZERO_TIME)?;
    let telegram_identity = quote_go(&options.telegram_identity)?;
    let bark_identity = quote_go(&options.bark_identity)?;
    let telegram_skip = if options.telegram_enabled {
        ""
    } else {
        ",\"not_requested\":true"
    };
    let bark_skip = if options.bark_enabled {
        ""
    } else {
        ",\"not_requested\":true"
    };
    let telegram_meta = Zeroizing::new(format!(
        "{{\"target_identity\":{}{telegram_skip},\"last_attempt_at\":{}}}",
        telegram_identity.as_str(),
        zero_time.as_str()
    ));
    let bark_meta = Zeroizing::new(format!(
        "{{\"target_identity\":{}{bark_skip},\"last_attempt_at\":{}}}",
        bark_identity.as_str(),
        zero_time.as_str()
    ));
    Ok(Zeroizing::new(format!(
        "{{\"message\":{},\"updated_at\":{},\"telegram_done\":{},\"bark_done\":{},\"telegram_at\":{},\"bark_at\":{},\"telegram_meta\":{},\"bark_meta\":{}}}",
        message_json.as_str(),
        received_at.as_str(),
        !options.telegram_enabled,
        !options.bark_enabled,
        zero_time.as_str(),
        zero_time.as_str(),
        telegram_meta.as_str(),
        bark_meta.as_str(),
    )))
}

fn encode_map<T: AsRef<str>>(
    fields: &[RawField<'_>],
    additions: &[(String, T)],
) -> Zeroizing<String> {
    let mut output = Zeroizing::new(String::from("{"));
    let mut first = true;
    for field in fields {
        if !first {
            output.push(',');
        }
        first = false;
        output.push_str(field.key_json);
        output.push(':');
        output.push_str(field.value_json);
    }
    for (key, value) in additions {
        if !first {
            output.push(',');
        }
        first = false;
        output.push_str(quote_go(key).expect("queue keys are valid UTF-8").as_str());
        output.push(':');
        output.push_str(value.as_ref());
    }
    output.push('}');
    output
}

fn put_root_field(output: &mut Zeroizing<String>, key: &str, value: &str, first: &mut bool) {
    if !*first {
        output.push(',');
    }
    *first = false;
    output.push_str(
        quote_go(key)
            .expect("static root key is valid UTF-8")
            .as_str(),
    );
    output.push(':');
    output.push_str(value);
}

fn render_root(
    fields: &[RawField<'_>],
    seen_map: &str,
    pending_map: &str,
    recent_map: Option<&str>,
) -> Result<Zeroizing<String>, StateError> {
    let mut output = Zeroizing::new(String::from("{"));
    let mut first = true;
    let mut wrote_seen = false;
    let mut wrote_pending = false;
    let mut wrote_recent = false;
    for field in fields {
        if field.name == "seen" {
            put_root_field(&mut output, "seen", seen_map, &mut first);
            wrote_seen = true;
        } else if field.name == "pending" {
            put_root_field(&mut output, "pending", pending_map, &mut first);
            wrote_pending = true;
        } else if field.name == "recent" {
            if let Some(recent_map) = recent_map {
                put_root_field(&mut output, "recent", recent_map, &mut first);
            } else {
                if !first {
                    output.push(',');
                }
                first = false;
                output.push_str(field.key_json);
                output.push(':');
                output.push_str(field.value_json);
            }
            wrote_recent = true;
        } else {
            if !first {
                output.push(',');
            }
            first = false;
            output.push_str(field.key_json);
            output.push(':');
            output.push_str(field.value_json);
        }
    }
    if !wrote_seen {
        put_root_field(&mut output, "seen", seen_map, &mut first);
    }
    if !wrote_pending {
        put_root_field(&mut output, "pending", pending_map, &mut first);
    }
    if let Some(recent_map) = recent_map
        && !wrote_recent
    {
        put_root_field(&mut output, "recent", recent_map, &mut first);
    }
    output.push('}');
    Ok(output)
}

fn rewrite_object(
    fields: &[RawField<'_>],
    replacements: &[(&str, &str)],
    removals: &[&str],
) -> Result<Zeroizing<String>, StateError> {
    for (name, _) in replacements {
        find_field(fields, name)?;
    }
    for name in removals {
        find_field(fields, name)?;
    }
    let mut output = Zeroizing::new(String::from("{"));
    let mut first = true;
    let mut wrote = HashSet::new();
    for field in fields {
        if removals.contains(&field.name.as_str()) {
            continue;
        }
        if !first {
            output.push(',');
        }
        first = false;
        output.push_str(field.key_json);
        output.push(':');
        if let Some((_, value)) = replacements.iter().find(|(name, _)| *name == field.name) {
            output.push_str(value);
            wrote.insert(field.name.as_str());
        } else {
            output.push_str(field.value_json);
        }
    }
    for (name, value) in replacements {
        if !wrote.contains(name) {
            if !first {
                output.push(',');
            }
            first = false;
            output.push_str(quote_go(name)?.as_str());
            output.push(':');
            output.push_str(value);
        }
    }
    output.push('}');
    Ok(output)
}

fn field_bool(fields: &[RawField<'_>], name: &str) -> Result<bool, StateError> {
    let Some(field) = find_field(fields, name)? else {
        return Ok(false);
    };
    serde_json::from_str::<Option<bool>>(field.value_json)
        .map(|value| value.unwrap_or(false))
        .map_err(|_| StateError::InvalidStateSchema)
}

fn field_string(fields: &[RawField<'_>], name: &str) -> Result<String, StateError> {
    let Some(field) = find_field(fields, name)? else {
        return Ok(String::new());
    };
    serde_json::from_str::<Option<String>>(field.value_json)
        .map(|value| value.unwrap_or_default())
        .map_err(|_| StateError::InvalidStateSchema)
}

fn channel_fields(channel: ForwardingChannel) -> (&'static str, &'static str) {
    match channel {
        ForwardingChannel::Telegram => ("telegram_done", "telegram_meta"),
        ForwardingChannel::Bark => ("bark_done", "bark_meta"),
    }
}

fn destination_meta_fields(
    document: &JsonDocument,
    task_id: &str,
    channel: ForwardingChannel,
) -> Result<(bool, Zeroizing<String>), QueueMutationError> {
    document
        .inspect_go_queue()
        .map_err(QueueMutationError::State)?;
    let root_text =
        std::str::from_utf8(document.as_bytes()).map_err(|_| StateError::InvalidJsonObject)?;
    let root = parse_object(root_text)?;
    let pending_field = find_field(&root.0, "pending")?.ok_or(StateError::InvalidStateSchema)?;
    let pending_fields = parse_map(Some(pending_field.value_json))?;
    let Some(pending) = pending_fields.iter().find(|field| field.name == task_id) else {
        return Err(QueueMutationError::TaskNotFound);
    };
    let delivery = parse_object(pending.value_json)?;
    let (done_name, meta_name) = channel_fields(channel);
    let done = field_bool(&delivery.0, done_name)?;
    let meta_field = find_field(&delivery.0, meta_name)?;
    let meta_json = match meta_field.map(|field| field.value_json.trim()) {
        None | Some("null") => "{}",
        Some(raw) => raw,
    };
    if meta_json != "{}" {
        parse_object(meta_json)?;
    }
    Ok((done, Zeroizing::new(meta_json.to_owned())))
}

pub(crate) fn validate_destination_for_attempt(
    document: &JsonDocument,
    task_id: &str,
    channel: ForwardingChannel,
    enabled: bool,
    current_binding: &DestinationBinding,
) -> Result<(), QueueMutationError> {
    if !enabled {
        return Err(QueueMutationError::DestinationNotCurrent);
    }
    let identity = current_binding.to_hex();
    validate_identity(&identity)?;
    let (done, meta_json) = destination_meta_fields(document, task_id, channel)?;
    let meta_fields = parse_object(meta_json.as_str())?.0;
    if done || field_bool(&meta_fields, "cancelled")? {
        return Err(QueueMutationError::DestinationAlreadyTerminal);
    }
    let existing_identity = field_string(&meta_fields, "target_identity")?;
    if existing_identity.is_empty() || existing_identity != identity {
        return Err(QueueMutationError::DestinationNotCurrent);
    }
    Ok(())
}

pub(crate) fn mutate_destination(
    document: &JsonDocument,
    mutation: DestinationMutation<'_>,
    limits: StateLimits,
) -> Result<JsonDocument, QueueMutationError> {
    let DestinationMutation {
        task_id,
        channel,
        kind,
        enabled,
        current_binding,
        updated_at,
    } = mutation;
    let bind_legacy = matches!(kind, DestinationMutationKind::BindLegacy);
    let updated_at =
        crate::schema::canonical_go_timestamp(updated_at).ok_or(StateError::InvalidMessage)?;
    let identity = current_binding.to_hex();
    validate_identity(&identity)?;
    let (done, meta_json) = destination_meta_fields(document, task_id, channel)?;
    let meta_fields = parse_object(meta_json.as_str())?.0;
    let root_text =
        std::str::from_utf8(document.as_bytes()).map_err(|_| StateError::InvalidJsonObject)?;
    let root = parse_object(root_text)?;
    let pending_field = find_field(&root.0, "pending")?.ok_or(StateError::InvalidStateSchema)?;
    let pending_fields = parse_map(Some(pending_field.value_json))?;
    let Some(pending) = pending_fields.iter().find(|field| field.name == task_id) else {
        return Err(QueueMutationError::TaskNotFound);
    };
    let delivery = parse_object(pending.value_json)?;
    let (_, meta_name) = channel_fields(channel);
    if done || field_bool(&meta_fields, "cancelled")? {
        return Err(QueueMutationError::DestinationAlreadyTerminal);
    }
    let existing_identity = field_string(&meta_fields, "target_identity")?;
    if bind_legacy {
        if !enabled || !existing_identity.is_empty() {
            return Err(QueueMutationError::LegacyDestinationConfirmationNotApplicable);
        }
    } else if !enabled || existing_identity.is_empty() || existing_identity != identity {
        return Err(QueueMutationError::DestinationNotCurrent);
    }

    let identity_json = quote_go(&identity)?;
    let meta_replacements: Vec<(&str, &str)> = if bind_legacy {
        vec![("target_identity", identity_json.as_str())]
    } else {
        Vec::new()
    };
    let meta_removals: &[&str] = if bind_legacy {
        &[]
    } else {
        &["permanent", "result_unknown", "error_code"]
    };
    let updated_meta = rewrite_object(&meta_fields, &meta_replacements, meta_removals)?;
    let meta_fields = vec![(meta_name, updated_meta.as_str())];
    let updated_at_json = quote_go(updated_at.as_str())?;
    let mut delivery_replacements = vec![("updated_at", updated_at_json.as_str())];
    let reset_time_json;
    if !bind_legacy {
        reset_time_json = quote_go(ZERO_TIME)?;
        let next_time_name = match channel {
            ForwardingChannel::Telegram => "telegram_at",
            ForwardingChannel::Bark => "bark_at",
        };
        delivery_replacements.push((next_time_name, reset_time_json.as_str()));
    }
    let updated_delivery = rewrite_object(&delivery.0, &delivery_replacements, &[])?;
    let updated_delivery = rewrite_object(
        &parse_object(updated_delivery.as_str())?.0,
        &meta_fields,
        &[],
    )?;
    let updated_pending = pending_fields
        .iter()
        .map(|field| {
            if field.name == task_id {
                Ok((
                    field.name.clone(),
                    Zeroizing::new(updated_delivery.to_string()),
                ))
            } else {
                Ok((
                    field.name.clone(),
                    Zeroizing::new(field.value_json.to_owned()),
                ))
            }
        })
        .collect::<Result<Vec<_>, StateError>>()?;
    let pending_map = encode_owned_map(&pending_fields, &updated_pending)?;
    let seen_field = find_field(&root.0, "seen")?.ok_or(StateError::InvalidStateSchema)?;
    let seen_map = seen_field.value_json;
    let serialized = render_root(&root.0, seen_map, &pending_map, None)?;
    let mut plaintext = Zeroizing::new(Vec::new());
    plaintext
        .try_reserve_exact(serialized.len())
        .map_err(|_| StateError::AllocationFailed)?;
    plaintext.extend_from_slice(serialized.as_bytes());
    Ok(JsonDocument::from_bytes(plaintext, limits)?)
}

pub(crate) fn begin_destination_attempt(
    document: &JsonDocument,
    task_id: &str,
    channel: ForwardingChannel,
    enabled: bool,
    binding: &DestinationBinding,
    attempted_at: &str,
    limits: StateLimits,
) -> Result<JsonDocument, QueueMutationError> {
    let attempted_at = crate::schema::canonical_go_timestamp(attempted_at)
        .ok_or(QueueMutationError::InvalidDeliveryResult)?;
    validate_destination_for_attempt(document, task_id, channel, enabled, binding)?;

    let (done_name, meta_name) = channel_fields(channel);
    let tries_name = attempt_fields(channel).0;
    let root_text =
        std::str::from_utf8(document.as_bytes()).map_err(|_| StateError::InvalidJsonObject)?;
    let root = parse_object(root_text)?;
    let pending_field = find_field(&root.0, "pending")?.ok_or(StateError::InvalidStateSchema)?;
    let pending_fields = parse_map(Some(pending_field.value_json))?;
    let pending = pending_fields
        .iter()
        .find(|field| field.name == task_id)
        .ok_or(QueueMutationError::TaskNotFound)?;
    let delivery = parse_object(pending.value_json)?;
    if field_bool(&delivery.0, done_name)? {
        return Err(QueueMutationError::DestinationAlreadyTerminal);
    }
    let meta_json = find_field(&delivery.0, meta_name)?
        .map(|field| field.value_json.trim())
        .filter(|value| *value != "null")
        .unwrap_or("{}");
    let meta = parse_object(meta_json)?;
    if field_bool(&meta.0, "cancelled")?
        || field_bool(&meta.0, "permanent")?
        || field_bool(&meta.0, "result_unknown")?
    {
        return Err(QueueMutationError::DestinationPaused);
    }
    let attempts = match find_field(&delivery.0, tries_name)? {
        Some(field) if field.value_json.trim() == "null" => 0,
        Some(field) => serde_json::from_str::<i64>(field.value_json)
            .map_err(|_| StateError::InvalidStateSchema)?,
        None => 0,
    };
    if attempts < 0 {
        return Err(QueueMutationError::State(StateError::InvalidStateSchema));
    }
    let attempts = attempts
        .checked_add(1)
        .ok_or(QueueMutationError::InvalidDeliveryResult)?;
    let attempts_json = attempts.to_string();
    let updated_at_json = quote_go(&attempted_at)?;
    let unconfirmed_code = match channel {
        ForwardingChannel::Telegram => "telegram_submission_unconfirmed",
        ForwardingChannel::Bark => "bark_submission_unconfirmed",
    };
    let error_json = quote_go(unconfirmed_code)?;
    let unknown_json = "true";
    let meta_updated = rewrite_object(
        &meta.0,
        &[
            ("last_attempt_at", updated_at_json.as_str()),
            ("result_unknown", unknown_json),
            ("error_code", error_json.as_str()),
        ],
        &["permanent"],
    )?;
    let mut delivery_updated = rewrite_object(
        &delivery.0,
        &[
            ("updated_at", updated_at_json.as_str()),
            (tries_name, &attempts_json),
        ],
        &[],
    )?;
    delivery_updated = rewrite_object(
        &parse_object(delivery_updated.as_str())?.0,
        &[(meta_name, meta_updated.as_str())],
        &[],
    )?;
    replace_or_archive_terminal_entry(document, task_id, &delivery_updated, limits)
}

pub(crate) fn cancel_destination(
    document: &JsonDocument,
    task_id: &str,
    channel: ForwardingChannel,
    updated_at: &str,
    limits: StateLimits,
) -> Result<JsonDocument, QueueMutationError> {
    let updated_at = crate::schema::canonical_go_timestamp(updated_at)
        .ok_or(QueueMutationError::InvalidDeliveryResult)?;
    let (_, meta_name) = channel_fields(channel);
    let (done, meta_json) = destination_meta_fields(document, task_id, channel)?;
    let meta = parse_object(meta_json.as_str())?;
    if done || field_bool(&meta.0, "cancelled")? {
        return Err(QueueMutationError::DestinationAlreadyTerminal);
    }
    let meta_updated = rewrite_object(&meta.0, &[("cancelled", "true")], &[])?;
    let delivery = parse_delivery(document, task_id)?;
    let updated_at_json = quote_go(&updated_at)?;
    let delivery_updated = rewrite_object(
        &parse_object(delivery.as_str())?.0,
        &[("updated_at", updated_at_json.as_str())],
        &[],
    )?;
    let delivery_updated = rewrite_object(
        &parse_object(delivery_updated.as_str())?.0,
        &[(meta_name, meta_updated.as_str())],
        &[],
    )?;
    replace_or_archive_terminal_entry(document, task_id, &delivery_updated, limits)
}

/// Persists a Telegram chunk submission boundary or explicit API acknowledgement.
/// Digest and chunk validation use the Go-compatible queue projection; the raw
/// progress object is then rewritten without dropping future/unknown fields.
pub(crate) fn freeze_telegram_progress(
    document: &JsonDocument,
    task_id: &str,
    binding: &DestinationBinding,
    chunks: &[Zeroizing<String>],
    limits: StateLimits,
) -> Result<JsonDocument, QueueMutationError> {
    validate_destination_for_attempt(
        document,
        task_id,
        ForwardingChannel::Telegram,
        true,
        binding,
    )?;
    let summary = document.inspect_go_queue()?;
    let task = summary
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or(QueueMutationError::TaskNotFound)?;
    if task.telegram.telegram_checkpoint.is_some() {
        return Err(QueueMutationError::InvalidTelegramCheckpoint);
    }

    let delivery_raw = parse_delivery(document, task_id)?;
    let delivery_fields = parse_object(delivery_raw.as_str())?;
    let meta_json = find_field(&delivery_fields.0, "telegram_meta")?
        .map(|field| field.value_json.trim())
        .filter(|value| *value != "null")
        .unwrap_or("{}");
    let meta = parse_object(meta_json)?;
    if field_bool(&meta.0, "not_requested")?
        || field_bool(&meta.0, "permanent")?
        || field_bool(&meta.0, "result_unknown")?
    {
        return Err(QueueMutationError::DestinationPaused);
    }
    let progress_json = crate::schema::telegram_progress_json(delivery_raw.as_str(), chunks)?;
    let delivery_updated = rewrite_object(
        &delivery_fields.0,
        &[("telegram_progress", progress_json.as_str())],
        &[],
    )?;
    replace_pending_entry(document, task_id, &delivery_updated, limits)
}

pub(crate) fn checkpoint_telegram_chunk(
    document: &JsonDocument,
    mutation: TelegramChunkMutation<'_>,
    limits: StateLimits,
) -> Result<JsonDocument, QueueMutationError> {
    let TelegramChunkMutation {
        task_id,
        enabled,
        binding,
        index,
        submitting,
        updated_at,
    } = mutation;
    validate_destination_for_attempt(
        document,
        task_id,
        ForwardingChannel::Telegram,
        enabled,
        binding,
    )?;
    let summary = document.inspect_go_queue()?;
    let task = summary
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or(QueueMutationError::TaskNotFound)?;
    let progress = task
        .telegram
        .telegram_checkpoint
        .as_ref()
        .ok_or(QueueMutationError::InvalidTelegramCheckpoint)?;
    if !progress.chunk_shape_valid || !progress.digests_valid {
        return Err(QueueMutationError::InvalidTelegramCheckpoint);
    }
    let confirmed = usize::try_from(progress.confirmed_parts)
        .map_err(|_| QueueMutationError::InvalidTelegramCheckpoint)?;
    if if submitting {
        index != confirmed || index >= progress.total_parts
    } else {
        index != confirmed.saturating_add(1) || index > progress.total_parts
    } {
        return Err(QueueMutationError::InvalidTelegramCheckpoint);
    }
    let updated_at = crate::schema::canonical_go_timestamp(updated_at)
        .ok_or(QueueMutationError::InvalidDeliveryResult)?;
    let delivery_raw = parse_delivery(document, task_id)?;
    let delivery = parse_object(delivery_raw.as_str())?;
    let progress_field = find_field(&delivery.0, "telegram_progress")?
        .ok_or(QueueMutationError::InvalidTelegramCheckpoint)?;
    let progress_fields = parse_object(progress_field.value_json)?;
    let meta_field = find_field(&delivery.0, "telegram_meta")?;
    let meta_json = match meta_field.map(|field| field.value_json.trim()) {
        None | Some("null") => "{}",
        Some(raw) => raw,
    };
    let meta = parse_object(meta_json)?;
    let updated_at_json = quote_go(&updated_at)?;
    let (updated_progress, updated_meta) = if submitting {
        let unconfirmed = quote_go("telegram_submission_unconfirmed")?;
        (
            Zeroizing::new(progress_field.value_json.to_owned()),
            rewrite_object(
                &meta.0,
                &[
                    ("result_unknown", "true"),
                    ("error_code", unconfirmed.as_str()),
                ],
                &["permanent"],
            )?,
        )
    } else {
        let confirmed_json = index.to_string();
        (
            rewrite_object(&progress_fields.0, &[("confirmed", &confirmed_json)], &[])?,
            rewrite_object(&meta.0, &[], &["result_unknown", "error_code"])?,
        )
    };
    let delivery_updated = rewrite_object(
        &delivery.0,
        &[
            ("updated_at", updated_at_json.as_str()),
            ("telegram_progress", updated_progress.as_str()),
            ("telegram_meta", updated_meta.as_str()),
        ],
        &[],
    )?;
    replace_pending_entry(document, task_id, &delivery_updated, limits)
}

pub(crate) fn finish_destination_attempt(
    document: &JsonDocument,
    task_id: &str,
    channel: ForwardingChannel,
    binding: &DestinationBinding,
    updated_at: &str,
    outcome: &DeliveryOutcome,
    limits: StateLimits,
) -> Result<JsonDocument, QueueMutationError> {
    let updated_at = crate::schema::canonical_go_timestamp(updated_at)
        .ok_or(QueueMutationError::InvalidDeliveryResult)?;
    let (done_name, meta_name) = channel_fields(channel);
    let next_name = attempt_fields(channel).1;
    let (done_value, next_at, error_code, permanent, result_unknown) = match outcome {
        DeliveryOutcome::Delivered => (true, ZERO_TIME.to_owned(), None, false, false),
        DeliveryOutcome::Failed {
            error_code,
            permanent,
            result_unknown,
            retry_at,
        } => {
            if !safe_error_code(error_code) {
                return Err(QueueMutationError::InvalidDeliveryResult);
            }
            let next_at = if *result_unknown || *permanent {
                if retry_at.is_some() {
                    return Err(QueueMutationError::InvalidDeliveryResult);
                }
                ZERO_TIME.to_owned()
            } else {
                crate::schema::canonical_go_timestamp(
                    retry_at
                        .as_deref()
                        .ok_or(QueueMutationError::InvalidDeliveryResult)?,
                )
                .ok_or(QueueMutationError::InvalidDeliveryResult)?
            };
            (
                false,
                next_at,
                Some(error_code.as_str()),
                *permanent,
                *result_unknown,
            )
        }
    };

    let (_, meta_json) = destination_meta_fields(document, task_id, channel)?;
    let meta = parse_object(meta_json.as_str())?.0;
    if field_bool(&meta, "cancelled")?
        || field_string(&meta, "target_identity")? != binding.to_hex()
    {
        return Err(QueueMutationError::DestinationNotCurrent);
    }
    let result_was_unknown = field_bool(&meta, "result_unknown")?;
    if !result_was_unknown {
        let checkpoint = if channel == ForwardingChannel::Telegram {
            document
                .inspect_go_queue()?
                .tasks
                .into_iter()
                .find(|task| task.id == task_id)
                .and_then(|task| task.telegram.telegram_checkpoint)
        } else {
            None
        };
        let can_finish_after_explicit_chunk_boundary = match outcome {
            DeliveryOutcome::Delivered => checkpoint.as_ref().is_some_and(|progress| {
                progress.chunk_shape_valid
                    && progress.digests_valid
                    && progress.confirmed_parts >= 0
                    && progress.confirmed_parts as usize == progress.total_parts
            }),
            DeliveryOutcome::Failed {
                result_unknown: false,
                ..
            } => checkpoint.as_ref().is_some_and(|progress| {
                progress.chunk_shape_valid
                    && progress.digests_valid
                    && progress.confirmed_parts >= 0
                    && (progress.confirmed_parts as usize) < progress.total_parts
            }),
            _ => false,
        };
        if !can_finish_after_explicit_chunk_boundary {
            return Err(QueueMutationError::InvalidDeliveryResult);
        }
    }
    if matches!(outcome, DeliveryOutcome::Delivered)
        && channel == ForwardingChannel::Telegram
        && let Some(progress) = document
            .inspect_go_queue()?
            .tasks
            .into_iter()
            .find(|task| task.id == task_id)
            .and_then(|task| task.telegram.telegram_checkpoint)
        && (!progress.chunk_shape_valid
            || !progress.digests_valid
            || progress.confirmed_parts < 0
            || progress.confirmed_parts as usize != progress.total_parts)
    {
        return Err(QueueMutationError::InvalidDeliveryResult);
    }
    let updated_at_json = quote_go(&updated_at)?;
    let last_attempt = field_string(&meta, "last_attempt_at")?;
    let last_attempt_json = quote_go(&last_attempt)?;
    let error_json = error_code.map(quote_go).transpose()?;
    let mut meta_replacements: Vec<(&str, &str)> =
        vec![("last_attempt_at", last_attempt_json.as_str())];
    let mut meta_removals: Vec<&str> = Vec::new();
    if error_code.is_some() {
        let code_json = error_json.as_ref().expect("error code was encoded");
        if result_unknown {
            meta_replacements.push(("result_unknown", "true"));
        } else {
            meta_removals.push("result_unknown");
        }
        if permanent {
            meta_replacements.push(("permanent", "true"));
        } else {
            meta_removals.push("permanent");
        }
        meta_replacements.push(("error_code", code_json.as_str()));
    } else {
        meta_removals.extend(["result_unknown", "permanent", "error_code"]);
    }
    let meta_updated = rewrite_object(&meta, &meta_replacements, &meta_removals)?;
    let done_json = if done_value { "true" } else { "false" };
    let next_json = quote_go(&next_at)?;
    let delivery_raw = parse_delivery(document, task_id)?;
    let mut delivery_updated = rewrite_object(
        &parse_object(delivery_raw.as_str())?.0,
        &[
            ("updated_at", updated_at_json.as_str()),
            (done_name, done_json),
            (next_name, next_json.as_str()),
        ],
        &[],
    )?;
    delivery_updated = rewrite_object(
        &parse_object(delivery_updated.as_str())?.0,
        &[(meta_name, meta_updated.as_str())],
        &[],
    )?;
    replace_or_archive_terminal_entry(document, task_id, &delivery_updated, limits)
}

fn parse_delivery(
    document: &JsonDocument,
    task_id: &str,
) -> Result<Zeroizing<String>, QueueMutationError> {
    let root_text =
        std::str::from_utf8(document.as_bytes()).map_err(|_| StateError::InvalidJsonObject)?;
    let root = parse_object(root_text)?;
    let pending_field = find_field(&root.0, "pending")?.ok_or(StateError::InvalidStateSchema)?;
    let pending = parse_map(Some(pending_field.value_json))?;
    let field = pending
        .iter()
        .find(|field| field.name == task_id)
        .ok_or(QueueMutationError::TaskNotFound)?;
    Ok(Zeroizing::new(field.value_json.to_owned()))
}

fn replace_pending_entry(
    document: &JsonDocument,
    task_id: &str,
    updated_delivery: &str,
    limits: StateLimits,
) -> Result<JsonDocument, QueueMutationError> {
    let root_text =
        std::str::from_utf8(document.as_bytes()).map_err(|_| StateError::InvalidJsonObject)?;
    let root = parse_object(root_text)?;
    let pending_field = find_field(&root.0, "pending")?.ok_or(StateError::InvalidStateSchema)?;
    let pending_fields = parse_map(Some(pending_field.value_json))?;
    if !pending_fields.iter().any(|field| field.name == task_id) {
        return Err(QueueMutationError::TaskNotFound);
    }
    let updated_pending = pending_fields
        .iter()
        .map(|field| {
            let value = if field.name == task_id {
                updated_delivery.to_owned()
            } else {
                field.value_json.to_owned()
            };
            (field.name.clone(), Zeroizing::new(value))
        })
        .collect::<Vec<_>>();
    let pending_map = encode_owned_map(&pending_fields, &updated_pending)?;
    let seen = find_field(&root.0, "seen")?
        .ok_or(StateError::InvalidStateSchema)?
        .value_json;
    let serialized = render_root(&root.0, seen, pending_map.as_str(), None)?;
    let mut plaintext = Zeroizing::new(Vec::new());
    plaintext
        .try_reserve_exact(serialized.len())
        .map_err(|_| StateError::AllocationFailed)?;
    plaintext.extend_from_slice(serialized.as_bytes());
    Ok(JsonDocument::from_bytes(plaintext, limits)?)
}

fn replace_or_archive_terminal_entry(
    document: &JsonDocument,
    task_id: &str,
    updated_delivery: &str,
    limits: StateLimits,
) -> Result<JsonDocument, QueueMutationError> {
    let delivery = parse_object(updated_delivery)?;
    let telegram_meta = nested_meta(&delivery.0, "telegram_meta")?;
    let bark_meta = nested_meta(&delivery.0, "bark_meta")?;
    let telegram_terminal =
        field_bool(&delivery.0, "telegram_done")? || field_bool(&telegram_meta, "cancelled")?;
    let bark_terminal =
        field_bool(&delivery.0, "bark_done")? || field_bool(&bark_meta, "cancelled")?;
    if !telegram_terminal || !bark_terminal {
        return replace_pending_entry(document, task_id, updated_delivery, limits);
    }

    let root_text =
        std::str::from_utf8(document.as_bytes()).map_err(|_| StateError::InvalidJsonObject)?;
    let root = parse_object(root_text)?;
    let pending_root = find_field(&root.0, "pending")?.ok_or(StateError::InvalidStateSchema)?;
    let pending_fields = parse_map(Some(pending_root.value_json))?;
    if !pending_fields.iter().any(|field| field.name == task_id) {
        return Err(QueueMutationError::TaskNotFound);
    }
    let retained_pending = pending_fields
        .iter()
        .filter(|field| field.name != task_id)
        .cloned()
        .collect::<Vec<_>>();
    let pending_map = encode_map::<&str>(&retained_pending, &[]);
    let recent_root = find_field(&root.0, "recent")?;
    let mut recent = parse_array(recent_root.map(|field| field.value_json))?
        .into_iter()
        .map(|value| value.get().to_owned())
        .collect::<Vec<_>>();
    let diagnostic = terminal_diagnostic_json(document, task_id, &delivery.0)?;
    if recent.len() >= 100 {
        let remove = recent.len() - 99;
        recent.drain(..remove);
    }
    recent.push(diagnostic.to_string());
    let recent_map = Zeroizing::new(format!("[{}]", recent.join(",")));
    let seen = find_field(&root.0, "seen")?.ok_or(StateError::InvalidStateSchema)?;
    let serialized = render_root(
        &root.0,
        seen.value_json,
        pending_map.as_str(),
        Some(recent_map.as_str()),
    )?;
    let mut plaintext = Zeroizing::new(Vec::new());
    plaintext
        .try_reserve_exact(serialized.len())
        .map_err(|_| StateError::AllocationFailed)?;
    plaintext.extend_from_slice(serialized.as_bytes());
    Ok(JsonDocument::from_bytes(plaintext, limits)?)
}

fn nested_meta<'a>(
    delivery: &'a [RawField<'_>],
    name: &str,
) -> Result<Vec<RawField<'a>>, StateError> {
    let Some(field) = find_field(delivery, name)? else {
        return Ok(Vec::new());
    };
    if field.value_json.trim() == "null" {
        return Ok(Vec::new());
    }
    Ok(parse_object(field.value_json)?.0)
}

fn diagnostic_time(fields: &[RawField<'_>], name: &str) -> Result<Option<String>, StateError> {
    let value = field_string(fields, name)?;
    if value.is_empty() || value == ZERO_TIME {
        return Ok(None);
    }
    let utc = crate::schema::go_timestamp_utc(&value).ok_or(StateError::InvalidStateSchema)?;
    Ok(Some(utc))
}

fn terminal_diagnostic_json(
    document: &JsonDocument,
    task_id: &str,
    delivery: &[RawField<'_>],
) -> Result<Zeroizing<String>, QueueMutationError> {
    let summary = document.inspect_go_queue()?;
    let task = summary
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or(QueueMutationError::TaskNotFound)?;
    let telegram = terminal_channel_json(
        delivery,
        ForwardingChannel::Telegram,
        task.telegram.telegram_checkpoint.as_ref(),
    )?;
    let bark = terminal_channel_json(delivery, ForwardingChannel::Bark, None)?;
    let message_field = find_field(delivery, "message")?.ok_or(StateError::InvalidStateSchema)?;
    let message = parse_object(message_field.value_json)?;
    let mut updated_at = diagnostic_time(delivery, "updated_at")?;
    if updated_at.is_none() {
        updated_at = diagnostic_time(&message.0, "timestamp")?;
    }
    for name in ["telegram_meta", "bark_meta"] {
        let meta = nested_meta(delivery, name)?;
        if let Some(candidate) = diagnostic_time(&meta, "last_attempt_at")?
            && updated_at
                .as_ref()
                .is_none_or(|current| candidate > *current)
        {
            updated_at = Some(candidate);
        }
    }
    let updated_at = updated_at.unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
    let mut task = serde_json::Map::new();
    task.insert(
        "id".to_owned(),
        serde_json::Value::String(task_id.to_owned()),
    );
    task.insert("telegram".to_owned(), telegram);
    task.insert("bark".to_owned(), bark);
    task.insert(
        "updated_at".to_owned(),
        serde_json::Value::String(updated_at),
    );
    Ok(Zeroizing::new(
        serde_json::to_string(&serde_json::Value::Object(task))
            .map_err(|_| StateError::InvalidStateSchema)?,
    ))
}

fn terminal_channel_json(
    delivery: &[RawField<'_>],
    channel: ForwardingChannel,
    checkpoint: Option<&crate::TelegramCheckpointSummary>,
) -> Result<serde_json::Value, StateError> {
    let (done_name, meta_name) = channel_fields(channel);
    let (tries_name, next_name) = attempt_fields(channel);
    let meta = nested_meta(delivery, meta_name)?;
    let state = if field_bool(&meta, "not_requested")? {
        "skipped"
    } else if field_bool(&meta, "cancelled")? {
        "cancelled"
    } else if field_bool(delivery, done_name)? {
        "delivered"
    } else {
        return Err(StateError::InvalidStateSchema);
    };
    let mut diagnostic = serde_json::Map::new();
    diagnostic.insert(
        "state".to_owned(),
        serde_json::Value::String(state.to_owned()),
    );
    diagnostic.insert(
        "attempts".to_owned(),
        serde_json::Value::from(field_i64(delivery, tries_name)?),
    );
    if let Some(error) = find_field(&meta, "error_code")? {
        let code = serde_json::from_str::<Option<String>>(error.value_json)
            .map_err(|_| StateError::InvalidStateSchema)?
            .unwrap_or_default();
        if safe_error_code(&code) {
            diagnostic.insert("error_code".to_owned(), serde_json::Value::String(code));
        }
    }
    if let Some(value) = diagnostic_time(delivery, next_name)? {
        diagnostic.insert(
            "next_attempt_at".to_owned(),
            serde_json::Value::String(value),
        );
    }
    if let Some(value) = diagnostic_time(&meta, "last_attempt_at")? {
        diagnostic.insert(
            "last_attempt_at".to_owned(),
            serde_json::Value::String(value),
        );
    }
    if let Some(checkpoint) = checkpoint {
        if checkpoint.confirmed_parts > 0 {
            diagnostic.insert(
                "confirmed_parts".to_owned(),
                serde_json::Value::from(checkpoint.confirmed_parts),
            );
        }
        if checkpoint.total_parts > 0 {
            diagnostic.insert(
                "total_parts".to_owned(),
                serde_json::Value::from(checkpoint.total_parts),
            );
        }
    }
    Ok(serde_json::Value::Object(diagnostic))
}

fn field_i64(fields: &[RawField<'_>], name: &str) -> Result<i64, StateError> {
    let Some(field) = find_field(fields, name)? else {
        return Ok(0);
    };
    serde_json::from_str::<Option<i64>>(field.value_json)
        .map(|value| value.unwrap_or_default())
        .map_err(|_| StateError::InvalidStateSchema)
}

fn attempt_fields(channel: ForwardingChannel) -> (&'static str, &'static str) {
    match channel {
        ForwardingChannel::Telegram => ("telegram_try", "telegram_at"),
        ForwardingChannel::Bark => ("bark_try", "bark_at"),
    }
}

fn safe_error_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn encode_owned_map(
    fields: &[RawField<'_>],
    values: &[(String, Zeroizing<String>)],
) -> Result<Zeroizing<String>, StateError> {
    let mut output = Zeroizing::new(String::from("{"));
    let mut first = true;
    for field in fields {
        if !first {
            output.push(',');
        }
        first = false;
        output.push_str(field.key_json);
        output.push(':');
        let value = values
            .iter()
            .find(|(key, _)| key == &field.name)
            .ok_or(StateError::InvalidStateSchema)?;
        output.push_str(value.1.as_str());
    }
    output.push('}');
    Ok(output)
}

pub(crate) fn enqueue(
    document: &JsonDocument,
    messages: &[ForwardingMessage],
    received_at: &str,
    options: &EnqueueOptions,
    limits: StateLimits,
) -> Result<(JsonDocument, EnqueueSummary), StateError> {
    let received_at =
        crate::schema::canonical_go_timestamp(received_at).ok_or(StateError::InvalidMessage)?;
    validate_identity(&options.telegram_identity)?;
    validate_identity(&options.bark_identity)?;

    // Validate every pre-existing delivery before mutation. We never rewrite
    // an entry, but invalid typed state must not be normalized accidentally.
    document.inspect_go_queue()?;
    let root = parse_object(
        std::str::from_utf8(document.as_bytes()).map_err(|_| StateError::InvalidJsonObject)?,
    )?;
    let seen_root = find_field(&root.0, "seen")?;
    let pending_root = find_field(&root.0, "pending")?;
    let seen_fields = parse_map(seen_root.map(|field| field.value_json))?;
    let pending_fields = parse_map(pending_root.map(|field| field.value_json))?;
    let mut seen_values = BTreeMap::new();
    for field in &seen_fields {
        let timestamp = serde_json::from_str::<Option<i64>>(field.value_json)
            .map_err(|_| StateError::InvalidStateSchema)?
            .unwrap_or_default();
        seen_values.insert(field.name.clone(), timestamp);
    }
    let pending_keys: HashSet<String> = pending_fields
        .iter()
        .map(|field| field.name.clone())
        .collect();
    let mut new_seen = Vec::new();
    let mut new_pending: Vec<(String, Zeroizing<String>)> = Vec::new();
    let mut summary = EnqueueSummary::default();
    let mut estimated_size = document.as_bytes().len();

    for message in messages {
        let direction = message.direction.trim();
        if direction.eq_ignore_ascii_case("out") || direction.eq_ignore_ascii_case("outbound") {
            continue;
        }
        let key = message_key(message)?;
        if seen_values.contains_key(&key) || pending_keys.contains(&key) {
            continue;
        }
        estimated_size = estimated_size
            .checked_add(estimate_enqueue_growth(message, options)?)
            .ok_or(StateError::SizeLimit)?;
        if estimated_size > limits.max_plaintext_bytes() {
            return Err(StateError::SizeLimit);
        }
        let timestamp = timestamp_unix_millis(&message.timestamp)?;
        seen_values.insert(key.clone(), timestamp);
        new_seen.push((key.clone(), timestamp.to_string()));
        summary.newly_seen += 1;
        if options.telegram_enabled || options.bark_enabled {
            new_pending.push((key, delivery_json(message, &received_at, options)?));
            summary.queued += 1;
        }
    }

    if seen_values.len() > MAX_SEEN_IDS {
        let remove_count = seen_values.len() - MAX_SEEN_IDS;
        let mut ordered = seen_values
            .iter()
            .map(|(key, timestamp)| (*timestamp, key.clone()))
            .collect::<Vec<_>>();
        ordered.sort();
        let evicted = ordered
            .into_iter()
            .take(remove_count)
            .collect::<HashSet<_>>();
        seen_values.retain(|key, timestamp| !evicted.contains(&(*timestamp, key.clone())));
        new_seen.retain(|(key, _)| seen_values.contains_key(key));
    }

    // Preserve exact raw entries. Pruning only removes old `seen` IDs, matching
    // Go's bounded dedup ledger; it never removes pending delivery evidence.
    let evicted_keys: HashSet<_> = seen_fields
        .iter()
        .filter(|field| !seen_values.contains_key(&field.name))
        .map(|field| field.name.clone())
        .collect();
    let retained_seen = seen_fields
        .iter()
        .filter(|field| !evicted_keys.contains(&field.name))
        .cloned()
        .collect::<Vec<_>>();
    let seen_map = encode_map(&retained_seen, &new_seen);
    let pending_map = encode_map(&pending_fields, &new_pending);
    let serialized = render_root(&root.0, &seen_map, &pending_map, None)?;
    let mut plaintext = Zeroizing::new(Vec::new());
    plaintext
        .try_reserve_exact(serialized.len())
        .map_err(|_| StateError::AllocationFailed)?;
    plaintext.extend_from_slice(serialized.as_bytes());
    let next = JsonDocument::from_bytes(plaintext, limits)?;
    summary.pending = next.inspect_go_queue()?.tasks.len();
    Ok((next, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> crate::StateKey {
        crate::StateKey::from_slice(&(0..32).map(|value| value as u8).collect::<Vec<_>>()).unwrap()
    }

    fn options() -> EnqueueOptions {
        let key = key();
        let telegram = DestinationBinding::telegram(&key, "fixture-token", "fixture-chat");
        let bark =
            DestinationBinding::from_go_identity(&key, "bark\0https://api.day.app\0fixture-device");
        EnqueueOptions::new(true, &telegram, true, &bark)
    }

    fn sms(id: &str, direction: &str) -> ForwardingMessage {
        ForwardingMessage::new(
            id,
            direction,
            "65075",
            "PAC <>&\u{2028}😀",
            "2026-01-02T03:04:05.123400+08:00",
            "fixture reader",
        )
        .unwrap()
    }

    fn doc(text: &str) -> JsonDocument {
        JsonDocument::from_bytes(
            Zeroizing::new(text.as_bytes().to_vec()),
            StateLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn enqueue_is_idempotent_preserves_opaque_json_and_never_sends() {
        let original = concat!(
            "{ \"future\" : {\"number\":18446744073709551615999,\"text\":\"<>&\\u2028\"},",
            "\"seen\":null,\"pending\":null}"
        );
        let document = doc(original);
        let input = sms("queue-write-001", "incoming");
        assert_eq!(format!("{input:?}"), "ForwardingMessage([REDACTED])");
        let (queued, summary) = document
            .enqueue(
                &[input],
                "2026-01-02T04:04:06.000000000+08:00",
                &options(),
                StateLimits::default(),
            )
            .unwrap();
        assert_eq!(
            summary,
            EnqueueSummary {
                queued: 1,
                pending: 1,
                newly_seen: 1
            }
        );
        let json = std::str::from_utf8(queued.as_bytes()).unwrap();
        assert!(json.contains("18446744073709551615999"));
        assert!(json.contains("\\u2028"));
        assert!(json.contains("\\u003c\\u003e\\u0026\\u2028"));

        let value: serde_json::Value = serde_json::from_slice(queued.as_bytes()).unwrap();
        let key = message_key(&sms("queue-write-001", "incoming")).unwrap();
        assert_eq!(value["seen"][key.as_str()], 1_767_294_245_123_i64);
        assert_eq!(
            value["pending"][key.as_str()]["message"]["timestamp"],
            "2026-01-02T03:04:05.1234+08:00"
        );
        assert_eq!(
            value["pending"][key.as_str()]["updated_at"],
            "2026-01-02T04:04:06+08:00"
        );
        assert_eq!(value["pending"][key.as_str()]["telegram_done"], false);
        assert_eq!(value["pending"][key.as_str()]["bark_done"], false);

        let (same, duplicate) = queued
            .enqueue(
                &[sms("queue-write-001", "incoming")],
                "2026-01-02T04:04:07Z",
                &options(),
                StateLimits::default(),
            )
            .unwrap();
        assert_eq!(
            duplicate,
            EnqueueSummary {
                queued: 0,
                pending: 1,
                newly_seen: 0
            }
        );
        assert_eq!(same.as_bytes(), queued.as_bytes());
        assert!(format!("{:?}", options()).contains("REDACTED"));
    }

    #[test]
    fn enqueue_skips_outbound_and_rejects_duplicate_json_keys() {
        let (document, summary) = doc("{\"seen\":{},\"pending\":{}}")
            .enqueue(
                &[sms("outbound-001", " Out ")],
                "2026-01-02T04:04:06Z",
                &options(),
                StateLimits::default(),
            )
            .unwrap();
        assert_eq!(summary, EnqueueSummary::default());
        assert_eq!(document.as_bytes(), b"{\"seen\":{},\"pending\":{}}");

        let duplicate = doc("{\"seen\":{\"x\":1,\"x\":2},\"pending\":{}}")
            .enqueue(
                &[sms("queue-write-002", "incoming")],
                "2026-01-02T04:04:06Z",
                &options(),
                StateLimits::default(),
            )
            .unwrap_err();
        assert_eq!(duplicate, StateError::DuplicateJsonKey);
    }

    #[test]
    fn disabled_channels_only_advance_seen_and_mark_no_delivery() {
        let key = key();
        let telegram = DestinationBinding::telegram(&key, "", "");
        let bark = DestinationBinding::from_go_identity(&key, "bark\0https://api.day.app\0");
        let options = EnqueueOptions::new(false, &telegram, false, &bark);
        let (document, summary) = doc("{\"seen\":{},\"pending\":{}}")
            .enqueue(
                &[sms("disabled-001", "incoming")],
                "2026-01-02T04:04:06Z",
                &options,
                StateLimits::default(),
            )
            .unwrap();
        assert_eq!(
            summary,
            EnqueueSummary {
                queued: 0,
                pending: 0,
                newly_seen: 1
            }
        );
        let value: serde_json::Value = serde_json::from_slice(document.as_bytes()).unwrap();
        let key = message_key(&sms("disabled-001", "incoming")).unwrap();
        assert!(value["seen"].get(key.as_str()).is_some());
        assert!(value["pending"].as_object().unwrap().is_empty());
    }
}

#[cfg(any(test, feature = "producer-admission-candidate"))]
pub(crate) fn admission_extension(document: &JsonDocument) -> Result<Option<&str>, StateError> {
    let text =
        std::str::from_utf8(document.as_bytes()).map_err(|_| StateError::InvalidJsonObject)?;
    let root = parse_object(text)?;
    Ok(root
        .0
        .into_iter()
        .find(|f| f.name == crate::producer_admission::EXTENSION)
        .map(|f| f.value_json))
}
#[cfg(any(test, feature = "producer-admission-candidate"))]
pub(crate) fn replace_admission_extension(
    document: &JsonDocument,
    extension: &str,
    limits: StateLimits,
) -> Result<JsonDocument, StateError> {
    let text =
        std::str::from_utf8(document.as_bytes()).map_err(|_| StateError::InvalidJsonObject)?;
    let root = parse_object(text)?;
    let mut out = Zeroizing::new(String::from("{"));
    let mut first = true;
    for field in root.0 {
        if field.name == crate::producer_admission::EXTENSION {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(field.key_json);
        out.push(':');
        out.push_str(field.value_json);
    }
    put_root_field(
        &mut out,
        crate::producer_admission::EXTENSION,
        extension,
        &mut first,
    );
    out.push('}');
    JsonDocument::from_bytes(Zeroizing::new(out.as_bytes().to_vec()), limits)
}

#[cfg(any(test, feature = "producer-admission-candidate"))]
pub(crate) fn admission_page_digest(
    messages: &[ForwardingMessage],
    options: &EnqueueOptions,
    request: crate::producer_admission::PageRequest,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"switch-nwait-page-v1\0");
    for number in [
        request.expected_completed_sequence,
        request.from_cursor,
        request.to_cursor,
        messages.len() as u64,
    ] {
        hash.update(number.to_be_bytes());
    }
    hash.update([
        request.disposition as u8,
        options.telegram_enabled as u8,
        options.bark_enabled as u8,
    ]);
    for text in [&options.telegram_identity, &options.bark_identity] {
        hash.update((text.len() as u64).to_be_bytes());
        hash.update(text.as_bytes());
    }
    for message in messages {
        for text in [
            message.id(),
            message.direction(),
            message.sender(),
            message.content(),
            message.timestamp(),
            message.device(),
        ] {
            hash.update((text.len() as u64).to_be_bytes());
            hash.update(text.as_bytes());
        }
    }
    format!("{:x}", hash.finalize())
}
