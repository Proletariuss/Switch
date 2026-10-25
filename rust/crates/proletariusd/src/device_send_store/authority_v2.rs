//! Compile-only device-SMS authority codec for reservation and safe retirement.
//!
//! No production Store, route, migration, claim, finalizer or GC calls this
//! module. Plans are pure proposed snapshots, never dispatch grants.
//! File adapters must validate raw JSON under their actual transaction lock;
//! the compile-only family inspector uses original bytes under that lock.
#![allow(dead_code)] // Intentionally unwired until the separately reviewed B batch.

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use sha2::{Digest, Sha256};

const SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
const RESERVE_BYTES: usize = 1024 * 1024;
// Bound tiny-value JSON allocation amplification before building the Value tree.
// Complete 10,000-record v1 fixtures remain below this conservative bound.
const MAX_JSON_VALUES: usize = 300_000;
const SLOT_BYTES: usize = 400 * 1024;
// Budget only. Future claim/finish schema and its worst-case bound are NOT
// implemented or proven by this reservation; no claim transition is exposed.
const FINISH_HEADROOM_BYTES: usize = 4 * 1024;
const CACHE_BYTES: usize = 4 * 1024 * 1024;
const OVERLAY_BYTES: usize = 1024;
const MAX_LIVE: usize = 32;
const MAX_LANE: usize = 16;
const MAX_CACHE: usize = 32;
const MAX_OVERLAY: usize = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CodecError {
    InvalidJson,
    InvalidIdentity,
    InvalidSnapshot,
    InvalidReserve,
    LegacyUnavailable,
    LegacyCollision,
    Conflict,
    Retired,
    NotIssued,
    AuthorityMismatch,
    SequenceExhausted,
    Capacity,
}

type Result<T> = std::result::Result<T, CodecError>;

fn canonical_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

macro_rules! hex_string {
    ($name:ident, $length:expr) => {
        #[derive(Clone, Eq, PartialEq, Serialize)]
        struct $name(String);
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                if !canonical_hex(&value, $length) {
                    return Err(de::Error::custom("noncanonical hex identity"));
                }
                Ok(Self(value))
            }
        }
    };
}
hex_string!(AuthorityId, 32);
hex_string!(IntentNonce, 32);
hex_string!(SnapshotDigest, 64);

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct Decimal(u64);
impl Serialize for Decimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(de::Error::custom("noncanonical decimal counter"));
        }
        value
            .parse::<u64>()
            .map(Self)
            .map_err(|_| de::Error::custom("counter exhausted"))
    }
}

/// Fields are private; a plain v1 client_request_id never selects this type.
#[derive(Clone, Eq, PartialEq)]
pub(super) struct DeviceSmsIdentity {
    authority: AuthorityId,
    epoch: Decimal,
    sequence: Decimal,
}
impl DeviceSmsIdentity {
    pub(super) fn parse_wire(value: &str) -> Result<Self> {
        if value.len() != 72 {
            return Err(CodecError::InvalidIdentity);
        }
        let parts: Vec<_> = value.split(':').collect();
        if parts.len() != 5
            || parts[0] != "m2"
            || parts[1] != "ds"
            || !canonical_hex(parts[2], 32)
            || !canonical_hex(parts[3], 16)
            || !canonical_hex(parts[4], 16)
        {
            return Err(CodecError::InvalidIdentity);
        }
        let epoch = u64::from_str_radix(parts[3], 16).map_err(|_| CodecError::InvalidIdentity)?;
        let sequence =
            u64::from_str_radix(parts[4], 16).map_err(|_| CodecError::InvalidIdentity)?;
        if epoch != 1 || sequence == 0 {
            return Err(CodecError::InvalidIdentity);
        }
        Ok(Self {
            authority: AuthorityId(parts[2].to_owned()),
            epoch: Decimal(epoch),
            sequence: Decimal(sequence),
        })
    }

    pub(super) fn wire(&self) -> String {
        format!(
            "m2:ds:{}:{:016x}:{:016x}",
            self.authority.0, self.epoch.0, self.sequence.0
        )
    }
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DeviceLane {
    Reader,
    Dji,
}
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
enum Family {
    #[serde(rename = "device_sms")]
    DeviceSms,
}
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
enum Purpose {
    #[serde(rename = "device_sms_send")]
    DeviceSmsSend,
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    phone: String,
    message: String,
    expected_subscription_id: String,
}
impl Payload {
    fn valid(&self) -> bool {
        !self.phone.is_empty()
            && self.phone == self.phone.trim()
            && self.phone.len() <= 128
            && !self.message.trim().is_empty()
            && self.message.len() <= 64 * 1024
            && (self.expected_subscription_id.is_empty()
                || super::valid_token(&self.expected_subscription_id))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReserveWire {
    identity_version: u8,
    family: Family,
    authority_id: AuthorityId,
    epoch: Decimal,
    sequence: Decimal,
    client_request_id: String,
    intent_nonce: IntentNonce,
    purpose: Purpose,
    device: DeviceLane,
    payload: Payload,
}

pub(super) struct ValidatedReserve {
    identity: DeviceSmsIdentity,
    nonce: IntentNonce,
    purpose: Purpose,
    device: DeviceLane,
    payload: Payload,
}

pub(super) fn decode_reserve_bytes(bytes: &[u8]) -> Result<ValidatedReserve> {
    let value = strict_value(bytes, RESERVE_BYTES)?;
    let wire: ReserveWire =
        serde_json::from_value(value).map_err(|_| CodecError::InvalidReserve)?;
    let identity = DeviceSmsIdentity::parse_wire(&wire.client_request_id)?;
    if wire.identity_version != 2
        || wire.epoch.0 != 1
        || wire.sequence.0 == 0
        || identity.authority != wire.authority_id
        || identity.epoch != wire.epoch
        || identity.sequence != wire.sequence
        || !wire.payload.valid()
    {
        return Err(CodecError::InvalidReserve);
    }
    Ok(ValidatedReserve {
        identity,
        nonce: wire.intent_nonce,
        purpose: wire.purpose,
        device: wire.device,
        payload: wire.payload,
    })
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyReference {
    path: LegacyPath,
    sha256: SnapshotDigest,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
enum LegacyPath {
    #[serde(rename = "device-send-ledger.legacy-v1.json")]
    FrozenV1,
}
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
enum ReservePhase {
    #[serde(rename = "reserved_unbound")]
    ReservedUnbound,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReservedSlot {
    sequence: Decimal,
    nonce: IntentNonce,
    purpose: Purpose,
    device: DeviceLane,
    payload_fingerprint: SnapshotDigest,
    phase: ReservePhase,
    payload: Payload,
    // Unit requires literal null, not a string/token/object. No dispatch right.
    binding: (),
    attempt: (),
    dispatch_count: u8,
    archive_confirmed: bool,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
enum TerminalOutcome {
    #[serde(rename = "not_dispatched")]
    NotDispatched,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TerminalRecord {
    sequence: Decimal,
    nonce: IntentNonce,
    purpose: Purpose,
    device: DeviceLane,
    payload_fingerprint: SnapshotDigest,
    payload: Payload,
    outcome: TerminalOutcome,
}

// A can read a frozen legacy fence but cannot mutate or claim one. Future
// mutable overlay transitions are deliberately not accepted by this codec.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LegacyPhase {
    Prepared,
    Submitting,
    Unknown,
    Accepted,
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyFence {
    client_request_id: String,
    original_state: LegacyPhase,
    original_attempt: String,
    segments: usize,
    archive_confirmed: bool,
}
impl LegacyFence {
    fn valid(&self) -> bool {
        super::valid_token(&self.client_request_id)
            && match self.original_state {
                LegacyPhase::Prepared => {
                    self.original_attempt.is_empty()
                        && self.segments == 0
                        && !self.archive_confirmed
                }
                LegacyPhase::Submitting => {
                    super::valid_token(&self.original_attempt)
                        && self.segments == 0
                        && !self.archive_confirmed
                }
                LegacyPhase::Unknown => {
                    super::valid_token(&self.original_attempt)
                        && self.segments <= 1024
                        && !self.archive_confirmed
                }
                LegacyPhase::Accepted => {
                    super::valid_token(&self.original_attempt)
                        && (1..=1024).contains(&self.segments)
                }
            }
    }
}

/// Opaque completeness proof over a whole validated v1 byte snapshot and its
/// expected digest. No constructor accepts a set, a page or caller metadata.
/// It does not prove these bytes came from the active path: the future locked
/// migration adapter must supply that provenance. A has no such adapter.
pub(super) struct CompleteLegacyIndex {
    digest: SnapshotDigest,
    fences: BTreeMap<String, LegacyFence>,
}
impl CompleteLegacyIndex {
    pub(super) fn from_snapshot(bytes: &[u8], expected_sha256: &str) -> Result<Self> {
        // Reject unbounded input before spending SHA-256 work on its contents,
        // regardless of whether the supplied expected digest is well formed.
        if bytes.len() > SNAPSHOT_BYTES {
            return Err(CodecError::Capacity);
        }
        if !canonical_hex(expected_sha256, 64) || digest(bytes).0 != expected_sha256 {
            return Err(CodecError::LegacyUnavailable);
        }
        let value = strict_value(bytes, SNAPSHOT_BYTES)?;
        let items = value.as_array().ok_or(CodecError::LegacyUnavailable)?;
        let records = super::read(items, true).map_err(|_| CodecError::LegacyUnavailable)?;
        let mut fences = BTreeMap::new();
        for (id, record) in records {
            let phase = match record.state.as_str() {
                "prepared" => LegacyPhase::Prepared,
                "submitting" => LegacyPhase::Submitting,
                "unknown" => LegacyPhase::Unknown,
                "accepted" => LegacyPhase::Accepted,
                _ => return Err(CodecError::LegacyUnavailable),
            };
            fences.insert(
                id.clone(),
                LegacyFence {
                    client_request_id: id,
                    original_state: phase,
                    original_attempt: record.attempt,
                    segments: record.segments,
                    archive_confirmed: record.archive_confirmed,
                },
            );
        }
        Ok(Self {
            digest: SnapshotDigest(expected_sha256.to_owned()),
            fences,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotWire {
    schema_version: u8,
    family: Family,
    authority_id: AuthorityId,
    epoch: Decimal,
    issued_high_water: Decimal,
    legacy_new_admission: bool,
    legacy: LegacyReference,
    live_slots: Vec<ReservedSlot>,
    terminal_cache: Vec<TerminalRecord>,
    legacy_overlay: Vec<LegacyFence>,
}

#[derive(Clone, Serialize)]
pub(super) struct AuthoritySnapshot {
    schema_version: u8,
    family: Family,
    authority_id: AuthorityId,
    epoch: Decimal,
    issued_high_water: Decimal,
    legacy_new_admission: bool,
    legacy: LegacyReference,
    live_slots: Vec<ReservedSlot>,
    terminal_cache: Vec<TerminalRecord>,
    legacy_overlay: Vec<LegacyFence>,
}

#[derive(Clone, Copy)]
struct Quotas {
    live: usize,
    lane: usize,
    slot_bytes: usize,
    cache: usize,
    cache_bytes: usize,
    overlay: usize,
    overlay_bytes: usize,
    total_bytes: usize,
}
impl Quotas {
    const PRODUCTION: Self = Self {
        live: MAX_LIVE,
        lane: MAX_LANE,
        slot_bytes: SLOT_BYTES,
        cache: MAX_CACHE,
        cache_bytes: CACHE_BYTES,
        overlay: MAX_OVERLAY,
        overlay_bytes: OVERLAY_BYTES,
        total_bytes: SNAPSHOT_BYTES,
    };

    fn bounded(self) -> bool {
        self.live <= MAX_LIVE
            && self.lane <= MAX_LANE
            && self.slot_bytes <= SLOT_BYTES
            && self.cache <= MAX_CACHE
            && self.cache_bytes <= CACHE_BYTES
            && self.overlay <= MAX_OVERLAY
            && self.overlay_bytes <= OVERLAY_BYTES
            && self.total_bytes <= SNAPSHOT_BYTES
    }
}

fn digest(bytes: &[u8]) -> SnapshotDigest {
    SnapshotDigest(format!("{:x}", Sha256::digest(bytes)))
}
fn payload_digest(
    purpose: Purpose,
    device: DeviceLane,
    payload: &Payload,
) -> Result<SnapshotDigest> {
    // Fixed ordered tuple defines the v2 digest without changing v1 raw input.
    let encoded =
        serde_json::to_vec(&(purpose, device, payload)).map_err(|_| CodecError::InvalidSnapshot)?;
    Ok(digest(&encoded))
}
fn encoded_len<T: Serialize>(value: &T) -> Result<usize> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| CodecError::InvalidSnapshot)
}

pub(super) fn decode_snapshot_bytes(bytes: &[u8]) -> Result<AuthoritySnapshot> {
    let value = strict_value(bytes, SNAPSHOT_BYTES)?;
    let items: Vec<SnapshotWire> =
        serde_json::from_value(value).map_err(|_| CodecError::InvalidSnapshot)?;
    if items.len() != 1 {
        return Err(CodecError::InvalidSnapshot);
    }
    let wire = items
        .into_iter()
        .next()
        .ok_or(CodecError::InvalidSnapshot)?;
    // The public-to-parent snapshot has no Deserialize implementation. Its
    // only external construction path is this raw-strict, checked decoder.
    let snapshot = AuthoritySnapshot {
        schema_version: wire.schema_version,
        family: wire.family,
        authority_id: wire.authority_id,
        epoch: wire.epoch,
        issued_high_water: wire.issued_high_water,
        legacy_new_admission: wire.legacy_new_admission,
        legacy: wire.legacy,
        live_slots: wire.live_slots,
        terminal_cache: wire.terminal_cache,
        legacy_overlay: wire.legacy_overlay,
    };
    encode_checked(&snapshot)?;
    Ok(snapshot)
}

pub(super) fn encode_checked(snapshot: &AuthoritySnapshot) -> Result<Vec<u8>> {
    encode_with_quotas(snapshot, Quotas::PRODUCTION)
}
fn encode_with_quotas(snapshot: &AuthoritySnapshot, quotas: Quotas) -> Result<Vec<u8>> {
    if !quotas.bounded() {
        return Err(CodecError::Capacity);
    }
    if snapshot.schema_version != 2 || snapshot.epoch.0 != 1 || snapshot.legacy_new_admission {
        return Err(CodecError::InvalidSnapshot);
    }
    if snapshot.live_slots.len() > quotas.live
        || snapshot.terminal_cache.len() > quotas.cache
        || snapshot.legacy_overlay.len() > quotas.overlay
    {
        return Err(CodecError::Capacity);
    }
    let mut sequences = BTreeSet::new();
    let mut lanes = [0_usize; 2];
    for slot in &snapshot.live_slots {
        if slot.sequence.0 == 0
            || slot.sequence > snapshot.issued_high_water
            || !sequences.insert(slot.sequence)
            || slot.dispatch_count != 0
            || slot.archive_confirmed
            || !slot.payload.valid()
            || slot.payload_fingerprint != payload_digest(slot.purpose, slot.device, &slot.payload)?
        {
            return Err(CodecError::InvalidSnapshot);
        }
        let lane = match slot.device {
            DeviceLane::Reader => 0,
            DeviceLane::Dji => 1,
        };
        lanes[lane] = lanes[lane].checked_add(1).ok_or(CodecError::Capacity)?;
        if lanes[lane] > quotas.lane
            || encoded_len(slot)?
                .checked_add(FINISH_HEADROOM_BYTES)
                .ok_or(CodecError::Capacity)?
                > quotas.slot_bytes
        {
            return Err(CodecError::Capacity);
        }
    }
    for cached in &snapshot.terminal_cache {
        if cached.sequence.0 == 0
            || cached.sequence > snapshot.issued_high_water
            || !sequences.insert(cached.sequence)
            || !cached.payload.valid()
            || cached.payload_fingerprint
                != payload_digest(cached.purpose, cached.device, &cached.payload)?
        {
            return Err(CodecError::InvalidSnapshot);
        }
        if encoded_len(cached)? > quotas.slot_bytes {
            return Err(CodecError::Capacity);
        }
    }
    if encoded_len(&snapshot.terminal_cache)? > quotas.cache_bytes {
        return Err(CodecError::Capacity);
    }
    let mut legacy_ids = BTreeSet::new();
    for fence in &snapshot.legacy_overlay {
        if !fence.valid() || !legacy_ids.insert(&fence.client_request_id) {
            return Err(CodecError::InvalidSnapshot);
        }
        if encoded_len(fence)? > quotas.overlay_bytes {
            return Err(CodecError::Capacity);
        }
    }
    let bytes = serde_json::to_vec(&[snapshot]).map_err(|_| CodecError::InvalidSnapshot)?;
    let headroom = snapshot
        .live_slots
        .len()
        .checked_mul(FINISH_HEADROOM_BYTES)
        .ok_or(CodecError::Capacity)?;
    if bytes
        .len()
        .checked_add(headroom)
        .ok_or(CodecError::Capacity)?
        > quotas.total_bytes
    {
        return Err(CodecError::Capacity);
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReserveDisposition {
    Created,
    Existing,
    TerminalNotDispatched,
}
/// An uncommitted proposal. No serializer, can_submit, claim or owner handle.
pub(super) struct ReservePlan {
    next: AuthoritySnapshot,
    disposition: ReserveDisposition,
}
impl ReservePlan {
    pub(super) fn encoded(&self) -> Result<Vec<u8>> {
        encode_checked(&self.next)
    }
    pub(super) fn disposition(&self) -> ReserveDisposition {
        self.disposition
    }
}

fn next_identity(current: &AuthoritySnapshot) -> Result<DeviceSmsIdentity> {
    let sequence = current
        .issued_high_water
        .0
        .checked_add(1)
        .ok_or(CodecError::SequenceExhausted)?;
    Ok(DeviceSmsIdentity {
        authority: current.authority_id.clone(),
        epoch: current.epoch,
        sequence: Decimal(sequence),
    })
}

pub(super) fn plan_reserve(
    current: &AuthoritySnapshot,
    request: &ValidatedReserve,
    legacy: &CompleteLegacyIndex,
) -> Result<ReservePlan> {
    plan_with_quotas(current, request, legacy, Quotas::PRODUCTION)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RetirementDisposition {
    Retired,
    AlreadyTerminal,
}

/// An uncommitted proposal. The only eligible phase has no binding, attempt,
/// claim, or physical dispatch. A future file adapter must commit this under
/// the same lock used by reserve and claim before returning a receipt.
pub(super) struct RetirementPlan {
    next: AuthoritySnapshot,
    disposition: RetirementDisposition,
}
impl RetirementPlan {
    pub(super) fn encoded(&self) -> Result<Vec<u8>> {
        encode_checked(&self.next)
    }
    pub(super) fn disposition(&self) -> RetirementDisposition {
        self.disposition
    }
}

/// Retire an exact, never-claimed reservation without recycling its identity.
/// Only the current codec's `reserved_unbound` phase and `not_dispatched`
/// terminal cache are accepted. Eviction removes the oldest terminal body;
/// the monotonically retained high-water mark still rejects that sequence.
pub(super) fn plan_retire_unclaimed(
    current: &AuthoritySnapshot,
    request: &ValidatedReserve,
    legacy: &CompleteLegacyIndex,
) -> Result<RetirementPlan> {
    retire_with_quotas(current, request, legacy, Quotas::PRODUCTION)
}

fn retire_with_quotas(
    current: &AuthoritySnapshot,
    request: &ValidatedReserve,
    legacy: &CompleteLegacyIndex,
    quotas: Quotas,
) -> Result<RetirementPlan> {
    // Reuse the complete legacy, authority, fingerprint, and identity checks.
    // A new candidate must never be signed merely because it was cancelled.
    let existing = plan_with_quotas(current, request, legacy, quotas)?;
    match existing.disposition {
        ReserveDisposition::TerminalNotDispatched => {
            return Ok(RetirementPlan {
                next: current.clone(),
                disposition: RetirementDisposition::AlreadyTerminal,
            });
        }
        ReserveDisposition::Created => return Err(CodecError::NotIssued),
        ReserveDisposition::Existing => {}
    }
    let mut next = current.clone();
    let index = next
        .live_slots
        .iter()
        .position(|slot| slot.sequence == request.identity.sequence)
        .ok_or(CodecError::InvalidSnapshot)?;
    let slot = next.live_slots.remove(index);
    next.terminal_cache.push(TerminalRecord {
        sequence: slot.sequence,
        nonce: slot.nonce,
        purpose: slot.purpose,
        device: slot.device,
        payload_fingerprint: slot.payload_fingerprint,
        payload: slot.payload,
        outcome: TerminalOutcome::NotDispatched,
    });
    next.terminal_cache.sort_by_key(|record| record.sequence);
    loop {
        match encode_with_quotas(&next, quotas) {
            Ok(_) => break,
            Err(CodecError::Capacity) if next.terminal_cache.len() > 1 => {
                next.terminal_cache.remove(0);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(RetirementPlan {
        next,
        disposition: RetirementDisposition::Retired,
    })
}
fn plan_with_quotas(
    current: &AuthoritySnapshot,
    request: &ValidatedReserve,
    legacy: &CompleteLegacyIndex,
    quotas: Quotas,
) -> Result<ReservePlan> {
    encode_with_quotas(current, quotas)?;
    if current.legacy.sha256 != legacy.digest
        || current
            .legacy_overlay
            .iter()
            .any(|fence| legacy.fences.get(&fence.client_request_id) != Some(fence))
    {
        return Err(CodecError::LegacyUnavailable);
    }
    if request.identity.authority != current.authority_id || request.identity.epoch != current.epoch
    {
        return Err(CodecError::AuthorityMismatch);
    }
    // Validate every extant v2 ID against the complete frozen legacy set, not
    // just this request. Never use a cache, page or only unresolved IDs here.
    for sequence in current
        .live_slots
        .iter()
        .map(|slot| slot.sequence)
        .chain(current.terminal_cache.iter().map(|cached| cached.sequence))
        .chain(std::iter::once(request.identity.sequence))
    {
        let id = DeviceSmsIdentity {
            authority: current.authority_id.clone(),
            epoch: current.epoch,
            sequence,
        }
        .wire();
        if legacy.fences.contains_key(&id) {
            return Err(CodecError::LegacyCollision);
        }
    }
    let fingerprint = payload_digest(request.purpose, request.device, &request.payload)?;
    if let Some(slot) = current
        .live_slots
        .iter()
        .find(|slot| slot.sequence == request.identity.sequence)
    {
        if slot.nonce != request.nonce
            || slot.device != request.device
            || slot.payload != request.payload
            || slot.purpose != request.purpose
            || slot.payload_fingerprint != fingerprint
        {
            return Err(CodecError::Conflict);
        }
        return Ok(ReservePlan {
            next: current.clone(),
            disposition: ReserveDisposition::Existing,
        });
    }
    if let Some(cached) = current
        .terminal_cache
        .iter()
        .find(|cached| cached.sequence == request.identity.sequence)
    {
        if cached.nonce != request.nonce
            || cached.device != request.device
            || cached.payload != request.payload
            || cached.purpose != request.purpose
            || cached.payload_fingerprint != fingerprint
        {
            return Err(CodecError::Conflict);
        }
        return Ok(ReservePlan {
            next: current.clone(),
            disposition: ReserveDisposition::TerminalNotDispatched,
        });
    }
    if request.identity.sequence <= current.issued_high_water {
        return Err(CodecError::Retired);
    }
    let candidate = next_identity(current)?;
    if request.identity != candidate {
        return Err(CodecError::NotIssued);
    }
    let mut next = current.clone();
    next.issued_high_water = request.identity.sequence;
    next.live_slots.push(ReservedSlot {
        sequence: request.identity.sequence,
        nonce: request.nonce.clone(),
        purpose: request.purpose,
        device: request.device,
        payload_fingerprint: fingerprint,
        phase: ReservePhase::ReservedUnbound,
        payload: request.payload.clone(),
        binding: (),
        attempt: (),
        dispatch_count: 0,
        archive_confirmed: false,
    });
    encode_with_quotas(&next, quotas)?;
    Ok(ReservePlan {
        next,
        disposition: ReserveDisposition::Created,
    })
}

// Preserve duplicate-key evidence at every object depth, including escaped
// equivalents. This raw parser must run before any generic Value conversion.
struct StrictSeed<'a>(&'a Cell<usize>);
impl<'de> de::DeserializeSeed<'de> for StrictSeed<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        let remaining = self
            .0
            .get()
            .checked_sub(1)
            .ok_or_else(|| <D::Error as de::Error>::custom("JSON value budget exceeded"))?;
        self.0.set(remaining);
        struct StrictVisitor<'a>(&'a Cell<usize>);
        impl<'de> de::Visitor<'de> for StrictVisitor<'_> {
            type Value = Value;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON with unique object keys")
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(Value::Null)
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Self::Value, E> {
                Ok(Value::Bool(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(Value::Number(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(Value::Number(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(Value::Number)
                    .ok_or_else(|| E::custom("invalid JSON number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(Value::String(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
                Ok(Value::String(v))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element_seed(StrictSeed(self.0))? {
                    values.push(value);
                }
                Ok(Value::Array(values))
            }
            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                    let value = map.next_value_seed(StrictSeed(self.0))?;
                    values.insert(key, value);
                }
                Ok(Value::Object(values))
            }
        }
        deserializer.deserialize_any(StrictVisitor(self.0))
    }
}
fn strict_value(bytes: &[u8], max: usize) -> Result<Value> {
    if bytes.len() > max {
        return Err(CodecError::Capacity);
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let budget = Cell::new(MAX_JSON_VALUES);
    let value = de::DeserializeSeed::deserialize(StrictSeed(&budget), &mut decoder)
        .map_err(|_| CodecError::InvalidJson)?;
    decoder.end().map_err(|_| CodecError::InvalidJson)?;
    Ok(value)
}

mod family;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod seal_tests;

mod seal_adapter;
