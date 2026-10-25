//! Read/prepare-only format candidate. No QueueStore owner accepts SWF2 yet.
//! Header identities/hashes describe inputs, not bootstrap or dispatch grants.
use crate::{JsonDocument, StateError, StateKey, StateLimits};
use aes_gcm::{AeadInOut, Aes256Gcm, KeyInit, aead::Nonce};
use serde::{
    Deserialize, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fmt};
use zeroize::Zeroizing;
const MAGIC: &[u8; 4] = b"SWF2";
const OVERHEAD: usize = 32;
const MAX_HEADER: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatError {
    State(StateError),
    InvalidHeader,
    UnsupportedPhase,
    DuplicateKey,
    DigestMismatch,
}
impl From<StateError> for FormatError {
    fn from(v: StateError) -> Self {
        Self::State(v)
    }
}
impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "queue format {self:?}")
    }
}
impl std::error::Error for FormatError {}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormatIdentity {
    pub root_id: String,
    pub source_id: String,
    pub queue_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedHeader {
    pub schema_version: u16,
    pub phase: String,
    pub identity: FormatIdentity,
    pub legacy_wire_sha256: String,
    pub legacy_plaintext_sha256: String,
    pub fresh_intent_witness_sha256: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodedFormat {
    Swf1,
    Swf2Prepared,
}
pub struct DecodedState {
    pub format: DecodedFormat,
    pub header: Option<PreparedHeader>,
    pub document: JsonDocument,
}
fn hex(value: &str, n: usize) -> bool {
    value.len() == n
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_header(h: &PreparedHeader) -> Result<(), FormatError> {
    if h.schema_version != 2 {
        return Err(FormatError::InvalidHeader);
    }
    if h.phase != "prepared" {
        return Err(FormatError::UnsupportedPhase);
    }
    if [
        &h.identity.root_id,
        &h.identity.source_id,
        &h.identity.queue_id,
    ]
    .iter()
    .any(|s| !hex(s, 32))
        || [
            &h.legacy_wire_sha256,
            &h.legacy_plaintext_sha256,
            &h.fresh_intent_witness_sha256,
        ]
        .iter()
        .any(|s| !hex(s, 64))
    {
        return Err(FormatError::InvalidHeader);
    }
    Ok(())
}
/// SWF1 is never rewritten by this read. SWF2 decrypts a bounded strict header
/// followed by the exact original legacy JSON bytes (no Value normalization).
pub fn read_compatible(
    key: &StateKey,
    wire: &[u8],
    limits: StateLimits,
) -> Result<DecodedState, FormatError> {
    if wire.starts_with(b"SWF1") {
        let document = crate::open(key, wire, limits)?;
        document.inspect_go_queue()?;
        return Ok(DecodedState {
            format: DecodedFormat::Swf1,
            header: None,
            document,
        });
    }
    if !wire.starts_with(MAGIC) {
        return Err(StateError::UnsupportedFormat.into());
    }
    let payload = decrypt(key, wire, limits)?;
    if payload.len() < 4 {
        return Err(FormatError::InvalidHeader);
    }
    let n = u32::from_be_bytes(payload[..4].try_into().unwrap()) as usize;
    if n == 0 || n > MAX_HEADER || n > payload.len() - 4 {
        return Err(FormatError::InvalidHeader);
    }
    strict_json(&payload[4..4 + n])?;
    let header: PreparedHeader =
        serde_json::from_slice(&payload[4..4 + n]).map_err(|_| FormatError::InvalidHeader)?;
    validate_header(&header)?;
    let body = &payload[4 + n..];
    if format!("{:x}", Sha256::digest(body)) != header.legacy_plaintext_sha256 {
        return Err(FormatError::DigestMismatch);
    }
    let document = JsonDocument::from_bytes(Zeroizing::new(body.to_vec()), limits)?;
    strict_json(document.as_bytes())?;
    document.inspect_go_queue()?;
    Ok(DecodedState {
        format: DecodedFormat::Swf2Prepared,
        header: Some(header),
        document,
    })
}
/// Produces prepared bytes only; performs no filesystem IO, migration or grant.
/// Missing owner/drain/provenance checks cannot be bypassed with this header.
pub fn prepare_from_swf1(
    key: &StateKey,
    legacy_wire: &[u8],
    identity: FormatIdentity,
    fresh_witness_sha256: String,
    limits: StateLimits,
) -> Result<Vec<u8>, FormatError> {
    prepare_with_entropy(
        key,
        legacy_wire,
        identity,
        fresh_witness_sha256,
        limits,
        getrandom::fill,
    )
}
fn prepare_with_entropy<E>(
    key: &StateKey,
    legacy_wire: &[u8],
    identity: FormatIdentity,
    fresh_witness_sha256: String,
    limits: StateLimits,
    entropy: impl FnOnce(&mut [u8]) -> Result<(), E>,
) -> Result<Vec<u8>, FormatError> {
    if !legacy_wire.starts_with(b"SWF1") {
        return Err(StateError::UnsupportedFormat.into());
    }
    let document = crate::open(key, legacy_wire, limits)?;
    document.inspect_go_queue()?;
    strict_json(document.as_bytes())?;
    let header = PreparedHeader {
        schema_version: 2,
        phase: "prepared".into(),
        identity,
        legacy_wire_sha256: format!("{:x}", Sha256::digest(legacy_wire)),
        legacy_plaintext_sha256: format!("{:x}", Sha256::digest(document.as_bytes())),
        fresh_intent_witness_sha256: fresh_witness_sha256,
    };
    validate_header(&header)?;
    let encoded = serde_json::to_vec(&header).map_err(|_| FormatError::InvalidHeader)?;
    if encoded.len() > MAX_HEADER {
        return Err(FormatError::InvalidHeader);
    }
    let mut payload = Zeroizing::new(Vec::new());
    payload
        .try_reserve_exact(4 + encoded.len() + document.as_bytes().len() + 16)
        .map_err(|_| StateError::AllocationFailed)?;
    payload.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
    payload.extend_from_slice(&encoded);
    payload.extend_from_slice(document.as_bytes());
    let mut nonce = [0u8; 12];
    entropy(&mut nonce).map_err(|_| StateError::EntropyUnavailable)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).expect("fixed key");
    let nonce_t: Nonce<Aes256Gcm> = nonce.into();
    cipher
        .encrypt_in_place(&nonce_t, MAGIC, &mut *payload)
        .map_err(|_| StateError::EncryptionFailed)?;
    let mut wire = Vec::new();
    wire.try_reserve_exact(16 + payload.len())
        .map_err(|_| StateError::AllocationFailed)?;
    wire.extend_from_slice(MAGIC);
    wire.extend_from_slice(&nonce);
    wire.extend_from_slice(&payload);
    Ok(wire)
}
fn decrypt(
    key: &StateKey,
    wire: &[u8],
    limits: StateLimits,
) -> Result<Zeroizing<Vec<u8>>, FormatError> {
    if wire.len() < OVERHEAD {
        return Err(StateError::Truncated.into());
    }
    let max = limits
        .max_plaintext_bytes()
        .checked_add(MAX_HEADER + 4 + OVERHEAD)
        .ok_or(StateError::SizeLimit)?;
    if wire.len() > max {
        return Err(StateError::SizeLimit.into());
    }
    let mut payload = Zeroizing::new(wire[16..].to_vec());
    let nonce: Nonce<Aes256Gcm> = wire[4..16].try_into().unwrap();
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).expect("fixed key");
    cipher
        .decrypt_in_place(&nonce, MAGIC, &mut *payload)
        .map_err(|_| StateError::AuthenticationFailed)?;
    Ok(payload)
}
// Duplicate evidence is checked recursively before either strict metadata or
// queue decoding sees original bytes; escaped-equivalent keys also collide.
struct Strict;
impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(StrictVisitor)
    }
}
struct StrictVisitor;
impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Strict;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("strict JSON")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
        let mut keys = HashSet::new();
        while let Some(k) = a.next_key::<String>()? {
            if !keys.insert(k) {
                return Err(de::Error::custom("duplicate JSON key"));
            }
            a.next_value::<Strict>()?;
        }
        Ok(Strict)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
        while a.next_element::<Strict>()?.is_some() {}
        Ok(Strict)
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Strict, E> {
        Ok(Strict)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Strict, E> {
        Ok(Strict)
    }
}
pub(crate) fn strict_json(bytes: &[u8]) -> Result<(), FormatError> {
    let mut d = serde_json::Deserializer::from_slice(bytes);
    Strict::deserialize(&mut d)
        .and_then(|_| d.end())
        .map_err(|e| {
            if e.to_string().contains("duplicate JSON key") {
                FormatError::DuplicateKey
            } else {
                FormatError::InvalidHeader
            }
        })
}
#[cfg(test)]
mod tests;
