#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use pcsc::{Context, ReaderState, Scope, State};
use proletarius_sim::{
    AkaResult, ApduTransport, ISIM_AID_PREFIX, SimProtocolError, USIM_AID_PREFIX,
    authenticate_3g_apdu, decode_iccid, exchange_apdu, parse_authenticate_payload,
    read_binary_apdu, select_application_apdu, select_file_apdu,
};

pub mod euicc_lpa;
pub mod euicc_transport;
mod metadata;
pub mod session;

pub use metadata::{
    ReaderSimMetadata, read_sim_metadata_auto_with_transport,
    read_sim_metadata_module_with_transport, read_sim_metadata_with_transport,
};

#[derive(Debug)]
pub enum AuthenticationError {
    IdentityChanged,
    Session(session::SessionError),
    Exchange(String),
    Protocol(SimProtocolError),
    ApplicationUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AkaApplication {
    Usim,
    Isim,
}

impl AkaApplication {
    fn aid_prefix(self) -> &'static [u8] {
        match self {
            Self::Usim => USIM_AID_PREFIX,
            Self::Isim => ISIM_AID_PREFIX,
        }
    }
}

#[derive(Debug)]
pub enum IdentityError {
    Session(session::SessionError),
    Exchange(String),
    Protocol(SimProtocolError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReaderIdentity {
    pub iccid: String,
    pub imsi: String,
    pub mcc: String,
    pub mnc: String,
}

/// Carrier selection fields read with the stable identity in one PC/SC
/// transaction. Optional selector files must never discard ICCID/IMSI.
#[derive(Clone, Eq, PartialEq)]
pub struct ReaderCarrierIdentity {
    pub identity: ReaderIdentity,
    /// Complete USIM AID selected during this transaction.
    pub application_aid: Vec<u8>,
    pub gid1: String,
    pub gid2: String,
    pub spn: String,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(error) => write!(formatter, "open reader: {error}"),
            Self::Exchange(error) => write!(formatter, "exchange APDU: {error}"),
            Self::Protocol(error) => write!(formatter, "SIM protocol: {error}"),
        }
    }
}

impl std::error::Error for IdentityError {}

impl IdentityError {
    pub fn failure_code(&self) -> &'static str {
        match self {
            Self::Session(error) => session_failure_code(error).unwrap_or("reader_identity_failed"),
            _ => "reader_identity_failed",
        }
    }
}

fn session_failure_code(error: &session::SessionError) -> Option<&'static str> {
    match error {
        session::SessionError::Pcsc(pcsc::Error::NoSmartcard | pcsc::Error::RemovedCard) => {
            Some("reader_card_absent")
        }
        session::SessionError::Pcsc(pcsc::Error::NoReadersAvailable) => Some("reader_unavailable"),
        session::SessionError::Pcsc(pcsc::Error::SharingViolation | pcsc::Error::ResetCard) => {
            Some("reader_busy")
        }
        _ => None,
    }
}

impl fmt::Display for AuthenticationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdentityChanged => {
                formatter.write_str("SIM identity changed during authentication")
            }
            Self::Session(error) => write!(formatter, "open reader: {error}"),
            Self::Exchange(error) => write!(formatter, "exchange APDU: {error}"),
            Self::Protocol(error) => write!(formatter, "SIM protocol: {error}"),
            Self::ApplicationUnavailable => {
                formatter.write_str("requested SIM application unavailable")
            }
        }
    }
}

impl std::error::Error for AuthenticationError {}

impl AuthenticationError {
    /// Stable, non-sensitive failure classification for the local API.  The
    /// caller needs to distinguish a removed card from a transient ownership
    /// conflict; RAND/AUTN and PC/SC implementation details must never cross
    /// the process boundary.
    pub fn failure_code(&self) -> &'static str {
        match self {
            Self::IdentityChanged => "reader_identity_changed",
            Self::Session(error) => {
                session_failure_code(error).unwrap_or("reader_authentication_failed")
            }
            Self::ApplicationUnavailable => "reader_application_unavailable",
            _ => "reader_authentication_failed",
        }
    }
}

/// Executes one complete 3G AKA transaction while Rust exclusively owns the
/// physical card handle. The full EF-DIR AID is mandatory; a prefix is never
/// sent to the card and USIM/ISIM selection cannot silently cross over.
pub fn authenticate_aka(
    reader: &str,
    aid: &[u8],
    rand: &[u8],
    autn: &[u8],
) -> Result<AkaResult, AuthenticationError> {
    // PC/SC permits multiple shared handles, but AKA is a stateful APDU
    // sequence (SELECT + AUTHENTICATE). Serialize that sequence per reader so
    // concurrent IMS challenges cannot interleave application selection or
    // leave a card in the wrong USIM/ISIM context. Different readers retain
    // independent locks and may authenticate concurrently.
    let lock = reader_auth_lock(reader);
    let _guard = acquire_reader_operation(&lock).map_err(AuthenticationError::Session)?;
    let mut session =
        session::PcscSession::connect(reader).map_err(AuthenticationError::Session)?;
    let mut transaction = session
        .transaction()
        .map_err(AuthenticationError::Session)?;
    authenticate_with_transport(&mut transaction, aid, rand, autn)
}

/// Find the requested application's complete AID and run AKA while holding
/// one PC/SC transaction. No application-prefix fallback or Go-side card
/// handle is involved in this path.
pub fn authenticate_aka_auto(
    reader: &str,
    application: AkaApplication,
    rand: &[u8],
    autn: &[u8],
) -> Result<AkaResult, AuthenticationError> {
    let lock = reader_auth_lock(reader);
    let _guard = acquire_reader_operation(&lock).map_err(AuthenticationError::Session)?;
    let mut session =
        session::PcscSession::connect(reader).map_err(AuthenticationError::Session)?;
    let mut transaction = session
        .transaction()
        .map_err(AuthenticationError::Session)?;
    authenticate_auto_with_transport(&mut transaction, application, rand, autn)
}

/// Binds AKA to the selected SIM, not merely a reusable reader name. Identity
/// verification and AUTHENTICATE share one PC/SC transaction and operation lock.
pub fn authenticate_aka_auto_for_identity(
    reader: &str,
    application: AkaApplication,
    expected_iccid: &str,
    expected_imsi: &str,
    rand: &[u8],
    autn: &[u8],
) -> Result<AkaResult, AuthenticationError> {
    let lock = reader_auth_lock(reader);
    let _guard = acquire_reader_operation(&lock).map_err(AuthenticationError::Session)?;
    let mut session =
        session::PcscSession::connect(reader).map_err(AuthenticationError::Session)?;
    let mut transaction = session
        .transaction()
        .map_err(AuthenticationError::Session)?;
    authenticate_bound_with_transport(
        &mut transaction,
        application,
        expected_iccid,
        expected_imsi,
        rand,
        autn,
    )
}

/// Authenticate only after verifying the expected identity on the caller's
/// already-owned transaction. This function never acquires or opens a reader.
pub fn authenticate_bound_with_transport<T: ApduTransport>(
    transport: &mut T,
    application: AkaApplication,
    expected_iccid: &str,
    expected_imsi: &str,
    rand: &[u8],
    autn: &[u8],
) -> Result<AkaResult, AuthenticationError>
where
    T::Error: fmt::Display,
{
    if expected_iccid.is_empty() || expected_imsi.is_empty() {
        return Err(AuthenticationError::IdentityChanged);
    }
    let identity = read_identity_auto_with_transport(transport).map_err(|error| match error {
        IdentityError::Session(error) => AuthenticationError::Session(error),
        IdentityError::Exchange(error) => AuthenticationError::Exchange(error),
        IdentityError::Protocol(error) => AuthenticationError::Protocol(error),
    })?;
    if identity.iccid != expected_iccid || identity.imsi != expected_imsi {
        return Err(AuthenticationError::IdentityChanged);
    }
    authenticate_auto_with_transport(transport, application, rand, autn)
}

/// Authenticate a module-backed SIM on the caller's existing USB AT/APDU
/// transaction. The exact ICCID/IMSI check and requested application's full
/// EF-DIR AID discovery share the same transport transaction; module EF-DIR
/// records returning exactly 6986 are skipped, while all other failures stay
/// closed. No application fallback is performed.
pub fn authenticate_module_bound_with_transport<T: ApduTransport>(
    transport: &mut T,
    application: AkaApplication,
    expected_iccid: &str,
    expected_imsi: &str,
    rand: &[u8],
    autn: &[u8],
) -> Result<AkaResult, AuthenticationError>
where
    T::Error: fmt::Display,
{
    if expected_iccid.is_empty() || expected_imsi.is_empty() {
        return Err(AuthenticationError::IdentityChanged);
    }
    if rand.len() != 16 || autn.len() != 16 {
        return Err(AuthenticationError::Protocol(
            SimProtocolError::InvalidChallengeLength,
        ));
    }
    let usim_aid =
        discover_module_usim_aid_with_transport(transport).map_err(|error| match error {
            IdentityError::Session(error) => AuthenticationError::Session(error),
            IdentityError::Exchange(error) => AuthenticationError::Exchange(error),
            IdentityError::Protocol(SimProtocolError::MissingField(_)) => {
                AuthenticationError::ApplicationUnavailable
            }
            IdentityError::Protocol(error) => AuthenticationError::Protocol(error),
        })?;
    let identity =
        read_identity_with_transport(transport, &usim_aid).map_err(|error| match error {
            IdentityError::Session(error) => AuthenticationError::Session(error),
            IdentityError::Exchange(error) => AuthenticationError::Exchange(error),
            IdentityError::Protocol(error) => AuthenticationError::Protocol(error),
        })?;
    if identity.iccid != expected_iccid || identity.imsi != expected_imsi {
        return Err(AuthenticationError::IdentityChanged);
    }
    let aid = match application {
        AkaApplication::Usim => usim_aid,
        AkaApplication::Isim => discover_module_aid_with_transport(transport, ISIM_AID_PREFIX)
            .map_err(|error| match error {
                IdentityError::Session(error) => AuthenticationError::Session(error),
                IdentityError::Exchange(error) => AuthenticationError::Exchange(error),
                IdentityError::Protocol(SimProtocolError::MissingField(_)) => {
                    AuthenticationError::ApplicationUnavailable
                }
                IdentityError::Protocol(error) => AuthenticationError::Protocol(error),
            })?,
    };
    authenticate_with_transport(transport, &aid, rand, autn)
}

/// Discover the requested complete AID and authenticate on one caller-owned
/// APDU transport, without creating another physical session.
pub fn authenticate_auto_with_transport<T: ApduTransport>(
    transport: &mut T,
    application: AkaApplication,
    rand: &[u8],
    autn: &[u8],
) -> Result<AkaResult, AuthenticationError>
where
    T::Error: fmt::Display,
{
    if rand.len() != 16 || autn.len() != 16 {
        return Err(AuthenticationError::Protocol(
            SimProtocolError::InvalidChallengeLength,
        ));
    }
    let aid =
        discover_aid_with_transport(transport, application.aid_prefix()).map_err(|error| {
            match error {
                IdentityError::Session(error) => AuthenticationError::Session(error),
                IdentityError::Exchange(error) => AuthenticationError::Exchange(error),
                IdentityError::Protocol(SimProtocolError::MissingField(_)) => {
                    AuthenticationError::ApplicationUnavailable
                }
                IdentityError::Protocol(error) => AuthenticationError::Protocol(error),
            }
        })?;
    authenticate_with_transport(transport, &aid, rand, autn)
}

/// Read EF-ICCID without changing any card state beyond the requested USIM
/// application/file selection. The caller supplies the complete AID obtained
/// from EF-DIR; no AID prefix guessing is allowed.
pub fn read_iccid(reader: &str, aid: &[u8]) -> Result<String, IdentityError> {
    let lock = reader_auth_lock(reader);
    let _guard = acquire_reader_operation(&lock).map_err(IdentityError::Session)?;
    let mut session = session::PcscSession::connect(reader).map_err(IdentityError::Session)?;
    let mut transaction = session.transaction().map_err(IdentityError::Session)?;
    read_iccid_with_transport(&mut transaction, aid)
}

/// Discover a complete USIM AID from EF-DIR without relying on Go to list
/// applications. The operation is read-only and bound to one PC/SC transaction.
pub fn discover_usim_aid(reader: &str) -> Result<Vec<u8>, IdentityError> {
    let lock = reader_auth_lock(reader);
    let _guard = acquire_reader_operation(&lock).map_err(IdentityError::Session)?;
    let mut session = session::PcscSession::connect(reader).map_err(IdentityError::Session)?;
    let mut transaction = session.transaction().map_err(IdentityError::Session)?;
    discover_usim_aid_with_transport(&mut transaction)
}

/// Discover the USIM application and read identity in one transaction so a
/// concurrent profile switch cannot change the card between discovery and
/// identity APDUs.
pub fn read_identity_auto(reader: &str) -> Result<ReaderIdentity, IdentityError> {
    let lock = reader_auth_lock(reader);
    let _guard = acquire_reader_operation(&lock).map_err(IdentityError::Session)?;
    let mut session = session::PcscSession::connect(reader).map_err(IdentityError::Session)?;
    let mut transaction = session.transaction().map_err(IdentityError::Session)?;
    read_identity_auto_with_transport(&mut transaction)
}

/// Read identity plus EF-GID1 without allowing a profile switch between the
/// two reads. This is the native carrier-resolution entry used before a new
/// VoWiFi session is created.
pub fn read_carrier_identity_auto(reader: &str) -> Result<ReaderCarrierIdentity, IdentityError> {
    let lock = reader_auth_lock(reader);
    let _guard = acquire_reader_operation(&lock).map_err(IdentityError::Session)?;
    let mut session = session::PcscSession::connect(reader).map_err(IdentityError::Session)?;
    let mut transaction = session.transaction().map_err(IdentityError::Session)?;
    read_carrier_identity_auto_with_transport(&mut transaction)
}

/// Read identity from a complete USIM AID discovered on this same transaction.
pub fn read_identity_auto_with_transport<T: ApduTransport>(
    transport: &mut T,
) -> Result<ReaderIdentity, IdentityError>
where
    T::Error: fmt::Display,
{
    let aid = discover_usim_aid_with_transport(transport)?;
    read_identity_with_transport(transport, &aid)
}

/// Read stable identity and optional carrier selectors using the caller's
/// existing transaction. Discovery and all identity APDUs remain inseparable.
pub fn read_carrier_identity_auto_with_transport<T: ApduTransport>(
    transport: &mut T,
) -> Result<ReaderCarrierIdentity, IdentityError>
where
    T::Error: fmt::Display,
{
    let aid = discover_usim_aid_with_transport(transport)?;
    read_carrier_identity_with_transport(transport, &aid)
}

fn discover_usim_aid_with_transport<T: ApduTransport>(
    transport: &mut T,
) -> Result<Vec<u8>, IdentityError>
where
    T::Error: fmt::Display,
{
    discover_aid_with_transport(transport, USIM_AID_PREFIX)
}

pub(crate) fn discover_aid_with_transport<T: ApduTransport>(
    transport: &mut T,
    prefix: &[u8],
) -> Result<Vec<u8>, IdentityError>
where
    T::Error: fmt::Display,
{
    for file in [[0x3f, 0x00], [0x2f, 0x00]] {
        exchange_apdu(transport, &select_file_apdu(file))
            .map_err(|error| IdentityError::Exchange(error.to_string()))?
            .success_data()
            .map_err(IdentityError::Protocol)?;
    }
    for record in 1..=32_u8 {
        let response = exchange_apdu(transport, &[0, 0xb2, record, 4, 0])
            .map_err(|error| IdentityError::Exchange(error.to_string()))?;
        if response.sw1 == 0x6a && response.sw2 == 0x83 {
            break;
        }
        let data = response.success_data().map_err(IdentityError::Protocol)?;
        if let Some(aid) = application_aid_from_dir_record(data, prefix) {
            return Ok(aid);
        }
    }
    Err(IdentityError::Protocol(SimProtocolError::MissingField(
        "EF-DIR application AID",
    )))
}

/// Read module-backed SIM metadata using the module reader's EF-DIR behavior.
/// Some modem SIM interfaces return `6986` for an individual READ RECORD even
/// though later records remain readable. Skip only that exact status; all
/// other card errors remain fatal, and at least one full USIM AID is required.
pub fn discover_module_usim_aid_with_transport<T: ApduTransport>(
    transport: &mut T,
) -> Result<Vec<u8>, IdentityError>
where
    T::Error: fmt::Display,
{
    discover_module_aid_with_transport(transport, USIM_AID_PREFIX)
}

pub(crate) fn discover_module_aid_with_transport<T: ApduTransport>(
    transport: &mut T,
    prefix: &[u8],
) -> Result<Vec<u8>, IdentityError>
where
    T::Error: fmt::Display,
{
    for file in [[0x3f, 0x00], [0x2f, 0x00]] {
        exchange_apdu(transport, &select_file_apdu(file))
            .map_err(|error| IdentityError::Exchange(error.to_string()))?
            .success_data()
            .map_err(IdentityError::Protocol)?;
    }
    for record in 1..=32_u8 {
        let response = exchange_apdu(transport, &[0, 0xb2, record, 4, 0])
            .map_err(|error| IdentityError::Exchange(error.to_string()))?;
        if (response.sw1, response.sw2) == (0x6a, 0x83) {
            break;
        }
        if (response.sw1, response.sw2) == (0x69, 0x86) {
            continue;
        }
        let data = response.success_data().map_err(IdentityError::Protocol)?;
        if let Some(aid) = application_aid_from_dir_record(data, prefix) {
            return Ok(aid);
        }
    }
    Err(IdentityError::Protocol(SimProtocolError::MissingField(
        "EF-DIR application AID",
    )))
}

#[cfg(test)]
fn usim_aid_from_dir_record(record: &[u8]) -> Option<Vec<u8>> {
    application_aid_from_dir_record(record, USIM_AID_PREFIX)
}

fn application_aid_from_dir_record(record: &[u8], prefix: &[u8]) -> Option<Vec<u8>> {
    let mut remaining = record;
    while let Some((&tag, rest)) = remaining.split_first() {
        if tag == 0xff {
            break;
        }
        let (value, tail) = next_dir_tlv_value(rest)?;
        if tag == 0x61 {
            let mut application = value;
            while let Some((&child_tag, child_rest)) = application.split_first() {
                let (child_value, child_tail) = next_dir_tlv_value(child_rest)?;
                if child_tag == 0x4f
                    && (prefix.len() + 1..=16).contains(&child_value.len())
                    && child_value.starts_with(prefix)
                {
                    return Some(child_value.to_vec());
                }
                application = child_tail;
            }
        }
        remaining = tail;
    }
    None
}

fn next_dir_tlv_value(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let (&first, rest) = data.split_first()?;
    let (length, value) = if first < 0x80 {
        (usize::from(first), rest)
    } else if first == 0x81 {
        let (&length, value) = rest.split_first()?;
        (usize::from(length), value)
    } else {
        return None;
    };
    let value_end = value.get(..length)?;
    Some((value_end, &value[length..]))
}

/// Read stable IMS identity fields through one serialized PC/SC transaction.
/// This is read-only; eSIM/profile mutations remain outside this API.
pub fn read_identity(reader: &str, aid: &[u8]) -> Result<ReaderIdentity, IdentityError> {
    let lock = reader_auth_lock(reader);
    let _guard = acquire_reader_operation(&lock).map_err(IdentityError::Session)?;
    let mut session = session::PcscSession::connect(reader).map_err(IdentityError::Session)?;
    let mut transaction = session.transaction().map_err(IdentityError::Session)?;
    read_identity_with_transport(&mut transaction, aid)
}

/// Explicit-AID variant of [`read_carrier_identity_auto`].
pub fn read_carrier_identity(
    reader: &str,
    aid: &[u8],
) -> Result<ReaderCarrierIdentity, IdentityError> {
    let lock = reader_auth_lock(reader);
    let _guard = acquire_reader_operation(&lock).map_err(IdentityError::Session)?;
    let mut session = session::PcscSession::connect(reader).map_err(IdentityError::Session)?;
    let mut transaction = session.transaction().map_err(IdentityError::Session)?;
    read_carrier_identity_with_transport(&mut transaction, aid)
}

fn read_identity_with_transport<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
) -> Result<ReaderIdentity, IdentityError>
where
    T::Error: fmt::Display,
{
    Ok(read_identity_fields(transport, aid, false)?.identity)
}

/// Explicit-AID carrier identity read on an already-owned APDU transaction.
pub fn read_carrier_identity_with_transport<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
) -> Result<ReaderCarrierIdentity, IdentityError>
where
    T::Error: fmt::Display,
{
    read_identity_fields(transport, aid, true)
}

fn read_identity_fields<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    include_selector_fields: bool,
) -> Result<ReaderCarrierIdentity, IdentityError>
where
    T::Error: fmt::Display,
{
    let select = select_application_apdu(aid).map_err(IdentityError::Protocol)?;
    exchange_apdu(transport, &select)
        .map_err(|error| IdentityError::Exchange(error.to_string()))?
        .success_data()
        .map_err(IdentityError::Protocol)?;
    let imsi_raw = read_imsi_file(transport)?;
    let imsi = proletarius_sim::decode_imsi(&imsi_raw).map_err(IdentityError::Protocol)?;
    // EF-AD is optional on some profiles. Never guess a two- or three-digit
    // MNC from IMSI; return it empty so the caller may retain a trusted
    // carrier-derived value without discarding the ICCID/IMSI read.
    let mnc = read_transparent(transport, [0x6f, 0xad], 4)
        .ok()
        .and_then(|ad| proletarius_sim::decode_mnc_length(&ad).ok())
        .and_then(|length| imsi.get(3..3 + length).map(str::to_owned))
        .unwrap_or_default();
    // GID1/GID2 are optional and may have carrier-specific lengths. Bounded
    // short READ BINARY commands let 6Cxx correction select the actual length
    // without parsing card-specific FCP layouts. Preserve FF bytes because
    // carrier selectors intentionally include them in prefix values.
    let (gid1, gid2, spn) = if include_selector_fields {
        let gid1 = read_transparent(transport, [0x6f, 0x3e], 32)
            .map(|bytes| upper_hex(&bytes))
            .unwrap_or_default();
        let gid2 = read_transparent(transport, [0x6f, 0x3f], 32)
            .map(|bytes| upper_hex(&bytes))
            .unwrap_or_default();
        // EF-SPN starts with a display-condition octet. Only decode its
        // bounded GSM default-alphabet payload; a malformed or unsupported
        // value is treated as absent so it cannot select a wrong MVNO bundle.
        let spn = read_transparent(transport, [0x6f, 0x46], 17)
            .ok()
            .and_then(|bytes| decode_spn(&bytes))
            .unwrap_or_default();
        (gid1, gid2, spn)
    } else {
        (String::new(), String::new(), String::new())
    };
    let iccid = read_iccid_with_transport(transport, aid)?;
    Ok(ReaderCarrierIdentity {
        identity: ReaderIdentity {
            iccid,
            mcc: imsi[..3].to_owned(),
            mnc,
            imsi,
        },
        application_aid: aid.to_vec(),
        gid1,
        gid2,
        spn,
    })
}

fn upper_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(DIGITS[usize::from(byte >> 4)]));
        result.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    result
}

/// Decodes the common, single-octet subset of the GSM default alphabet used
/// by EF-SPN. Return no value rather than guessing when an extension or an
/// unsupported non-ASCII glyph is encountered: selector matching is allowed
/// to fall back only to an explicit generic profile.
fn decode_spn(bytes: &[u8]) -> Option<String> {
    let payload = trim_sim_padding(bytes.get(1..)?)?;
    let decoded = match payload.first().copied()? {
        0x80 => decode_ucs2(&payload[1..])?,
        0x81 => decode_compressed_ucs2(payload, 1)?,
        0x82 => decode_compressed_ucs2(payload, 2)?,
        _ if printable_ascii(payload) => String::from_utf8(payload.to_vec()).ok()?,
        _ => proletarius_sms::alphabet::decode(payload).ok()?,
    };
    let decoded = decoded.trim_matches('\0').trim().to_owned();
    (!decoded.is_empty()).then_some(decoded)
}

fn trim_sim_padding(bytes: &[u8]) -> Option<&[u8]> {
    let mut end = bytes.len();
    while end > 0 && matches!(bytes[end - 1], 0x00 | 0xff) {
        end -= 1;
    }
    (end > 0).then_some(&bytes[..end])
}

fn printable_ascii(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(|byte| (0x20..=0x7e).contains(byte))
}

pub(crate) fn decode_ucs2(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() % 2 != 0 {
        return None;
    }
    let words = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
    Some(
        char::decode_utf16(words)
            .map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect(),
    )
}

pub(crate) fn decode_compressed_ucs2(bytes: &[u8], base_width: usize) -> Option<String> {
    let header_len = 2usize.checked_add(base_width)?;
    if bytes.len() < header_len {
        return None;
    }
    let count = usize::from(*bytes.get(1)?);
    let payload = bytes
        .get(header_len..)?
        .get(..count.min(bytes.len() - header_len))?;
    let base = match base_width {
        1 => u32::from(*bytes.get(2)?) << 7,
        2 => u32::from(u16::from_be_bytes([*bytes.get(2)?, *bytes.get(3)?])),
        _ => return None,
    };
    let mut output = String::new();
    for value in payload {
        if *value < 0x80 {
            output.push_str(&proletarius_sms::alphabet::decode(&[*value]).ok()?);
        } else {
            output.push(char::from_u32(base + u32::from(value & 0x7f))?);
        }
    }
    Some(output)
}

fn read_imsi_file<T: ApduTransport>(transport: &mut T) -> Result<Vec<u8>, IdentityError>
where
    T::Error: fmt::Display,
{
    let select = select_file_apdu([0x6f, 0x07]);
    exchange_apdu(transport, &select)
        .map_err(|error| IdentityError::Exchange(error.to_string()))?
        .success_data()
        .map_err(IdentityError::Protocol)?;
    let first = exchange_apdu(transport, &read_binary_apdu(0, 1))
        .map_err(|error| IdentityError::Exchange(error.to_string()))?;
    let first = first.success_data().map_err(IdentityError::Protocol)?;
    if first.len() != 1 {
        return Err(IdentityError::Protocol(
            SimProtocolError::InvalidFieldLength {
                field: "IMSI length byte",
                expected: "exactly 1 byte",
                actual: first.len(),
            },
        ));
    }
    let Some(&length) = first.first() else {
        return Err(IdentityError::Protocol(SimProtocolError::MissingField(
            "IMSI length",
        )));
    };
    // EF-IMSI stores at most 15 digits plus one parity/type nibble: 8 BCD
    // bytes following the length byte. Reject corrupted lengths before a
    // second APDU is sent to the card.
    if !(4..=8).contains(&length) {
        return Err(IdentityError::Protocol(
            SimProtocolError::InvalidFieldLength {
                field: "IMSI",
                expected: "4..=8 BCD bytes",
                actual: usize::from(length),
            },
        ));
    }
    let response = exchange_apdu(transport, &read_binary_apdu(0, length + 1))
        .map_err(|error| IdentityError::Exchange(error.to_string()))?;
    Ok(response
        .success_data()
        .map_err(IdentityError::Protocol)?
        .to_vec())
}

fn read_transparent<T: ApduTransport>(
    transport: &mut T,
    file_id: [u8; 2],
    length: u8,
) -> Result<Vec<u8>, IdentityError>
where
    T::Error: fmt::Display,
{
    let select = select_file_apdu(file_id);
    exchange_apdu(transport, &select)
        .map_err(|error| IdentityError::Exchange(error.to_string()))?
        .success_data()
        .map_err(IdentityError::Protocol)?;
    let response = exchange_apdu(transport, &read_binary_apdu(0, length))
        .map_err(|error| IdentityError::Exchange(error.to_string()))?;
    Ok(response
        .success_data()
        .map_err(IdentityError::Protocol)?
        .to_vec())
}

fn read_iccid_with_transport<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
) -> Result<String, IdentityError>
where
    T::Error: fmt::Display,
{
    let select = select_application_apdu(aid).map_err(IdentityError::Protocol)?;
    exchange_apdu(transport, &select)
        .map_err(|error| IdentityError::Exchange(error.to_string()))?
        .success_data()
        .map_err(IdentityError::Protocol)?;
    // EF-ICCID lives below MF on both classic SIM and USIM cards. Explicitly
    // return to MF before selecting it; relying on the application's current
    // file context fails on cards that expose no 2FE2 under ADF-USIM.
    let select_mf = select_file_apdu([0x3f, 0x00]);
    exchange_apdu(transport, &select_mf)
        .map_err(|error| IdentityError::Exchange(error.to_string()))?
        .success_data()
        .map_err(IdentityError::Protocol)?;
    let select_iccid = select_file_apdu([0x2f, 0xe2]);
    exchange_apdu(transport, &select_iccid)
        .map_err(|error| IdentityError::Exchange(error.to_string()))?
        .success_data()
        .map_err(IdentityError::Protocol)?;
    let read = read_binary_apdu(0, 10);
    let response = exchange_apdu(transport, &read)
        .map_err(|error| IdentityError::Exchange(error.to_string()))?;
    let raw = response.success_data().map_err(IdentityError::Protocol)?;
    decode_iccid(raw).map_err(IdentityError::Protocol)
}

fn reader_auth_lock(reader: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Hot-plugged reader names can be ephemeral. Remove locks no longer held
    // so repeated device churn cannot grow this process-wide map indefinitely.
    locks.retain(|_, lock| lock.strong_count() != 0);
    let key = reader.trim().to_owned();
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

// Never queue physical operations behind an already running transaction: the
// HTTP caller may have timed out by the time that transaction releases its
// lock. Keep ownership with the active worker and reject new work as busy.
fn acquire_reader_operation(
    lock: &Mutex<()>,
) -> Result<std::sync::MutexGuard<'_, ()>, session::SessionError> {
    match lock.try_lock() {
        Ok(guard) => Ok(guard),
        Err(std::sync::TryLockError::Poisoned(error)) => Ok(error.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => {
            Err(session::SessionError::Pcsc(pcsc::Error::SharingViolation))
        }
    }
}

/// Run one physical-card operation under the process-wide per-reader gate.
///
/// The closure must own the complete PC/SC session/transaction and must not
/// hand APDU work to another task. A busy reader is rejected immediately;
/// queuing a card mutation behind a request whose caller may have timed out
/// could otherwise apply an unexpected delayed write. Cross-process ownership
/// is separate and must be acquired by the caller before entering this gate.
pub fn with_reader_operation<T>(
    reader: &str,
    operation: impl FnOnce() -> T,
) -> Result<T, session::SessionError> {
    let lock = reader_auth_lock(reader);
    let _guard = acquire_reader_operation(&lock)?;
    Ok(operation())
}

/// Explicit-AID authentication on a caller-owned complete APDU transaction.
pub fn authenticate_with_transport<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    rand: &[u8],
    autn: &[u8],
) -> Result<AkaResult, AuthenticationError>
where
    T::Error: fmt::Display,
{
    let select = select_application_apdu(aid).map_err(AuthenticationError::Protocol)?;
    exchange_apdu(transport, &select)
        .map_err(|error| AuthenticationError::Exchange(error.to_string()))?
        .success_data()
        .map_err(AuthenticationError::Protocol)?;
    let command = authenticate_3g_apdu(rand, autn).map_err(AuthenticationError::Protocol)?;
    let response = exchange_apdu(transport, &command)
        .map_err(|error| AuthenticationError::Exchange(error.to_string()))?;
    parse_authenticate_payload(
        response
            .success_data()
            .map_err(AuthenticationError::Protocol)?,
    )
    .map_err(AuthenticationError::Protocol)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardPresence {
    Unknown,
    Absent,
    Present,
    Unavailable,
}

impl CardPresence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Absent => "absent",
            Self::Present => "present",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReaderObservation {
    pub name: String,
    pub card: CardPresence,
    pub in_use: bool,
    pub muted: bool,
    pub atr: Vec<u8>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoverySnapshot {
    pub revision: u64,
    pub readers: Vec<ReaderObservation>,
    pub selected: Option<String>,
}

#[derive(Debug)]
pub struct DiscoveryError(String);

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DiscoveryError {}

pub trait ReaderScanner {
    fn scan(&self) -> Result<Vec<ReaderObservation>, DiscoveryError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PcscReaderScanner;

/// Additional OS power facts without changing the existing discovery DTO.
#[derive(Clone, Debug)]
pub struct ReaderCardStatus {
    pub reader: ReaderObservation,
    pub unpowered: bool,
    /// OS insertion/removal counter, not a card identifier. Some resource
    /// managers do not implement it and always report zero.
    pub event_count: u32,
}

impl ReaderScanner for PcscReaderScanner {
    fn scan(&self) -> Result<Vec<ReaderObservation>, DiscoveryError> {
        self.scan_card_status()
            .map(|readers| readers.into_iter().map(|status| status.reader).collect())
    }
}

impl PcscReaderScanner {
    /// Presence only: no card connection, reset or APDU is performed.
    pub fn scan_card_status(&self) -> Result<Vec<ReaderCardStatus>, DiscoveryError> {
        let context = Context::establish(Scope::System)
            .map_err(|error| DiscoveryError(format!("establish PC/SC context: {error}")))?;
        let names = match context.list_readers_owned() {
            Ok(names) => names,
            Err(pcsc::Error::NoReadersAvailable) => return Ok(Vec::new()),
            Err(error) => return Err(DiscoveryError(format!("list PC/SC readers: {error}"))),
        };
        if names.is_empty() {
            return Ok(Vec::new());
        }

        // Status polling does not connect to or reset a card. Rust may safely
        // observe presence while Go remains the sole APDU/hardware owner.
        let mut states = names
            .iter()
            .map(|name| ReaderState::new(name.as_c_str(), State::UNAWARE))
            .collect::<Vec<_>>();
        context
            .get_status_change(Duration::ZERO, &mut states)
            .map_err(|error| DiscoveryError(format!("read PC/SC status: {error}")))?;

        Ok(states
            .iter()
            .map(|state| {
                let flags = state.event_state();
                let card = if flags.intersects(State::UNKNOWN | State::UNAVAILABLE) {
                    CardPresence::Unavailable
                } else if flags.contains(State::PRESENT) {
                    CardPresence::Present
                } else if flags.contains(State::EMPTY) {
                    CardPresence::Absent
                } else {
                    CardPresence::Unknown
                };
                ReaderCardStatus {
                    reader: ReaderObservation {
                        name: state.name().to_string_lossy().into_owned(),
                        card,
                        in_use: flags.intersects(State::INUSE | State::EXCLUSIVE),
                        muted: flags.contains(State::MUTE),
                        atr: state.atr().to_vec(),
                    },
                    unpowered: flags.contains(State::UNPOWERED),
                    event_count: state.event_count(),
                }
            })
            .collect())
    }
}

#[derive(Debug, Default)]
pub struct ReaderDiscovery {
    snapshot: DiscoverySnapshot,
}

impl ReaderDiscovery {
    pub fn snapshot(&self) -> DiscoverySnapshot {
        self.snapshot.clone()
    }

    pub fn reconcile(
        &mut self,
        readers: Vec<ReaderObservation>,
        selected_hint: Option<&str>,
    ) -> DiscoverySnapshot {
        let selected = selected_hint
            .filter(|selected| readers.iter().any(|reader| reader.name == *selected))
            .map(str::to_owned);
        if self.snapshot.readers != readers || self.snapshot.selected != selected {
            self.snapshot.revision = self.snapshot.revision.saturating_add(1);
            self.snapshot.readers = readers;
            self.snapshot.selected = selected;
        }
        self.snapshot.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    #[test]
    fn discovery_revision_never_recycles_when_inventory_changes_at_exhaustion() {
        let mut inventory = super::ReaderDiscovery {
            snapshot: super::DiscoverySnapshot {
                revision: u64::MAX,
                ..Default::default()
            },
        };
        let result = inventory.reconcile(
            vec![super::ReaderObservation {
                name: "Reader".into(),
                card: super::CardPresence::Present,
                in_use: false,
                muted: false,
                atr: vec![],
            }],
            None,
        );
        assert_eq!(result.revision, u64::MAX);
        assert_eq!(result.readers[0].name, "Reader");
    }

    #[test]
    fn identity_and_authentication_share_device_error_classification() {
        for (error, expected) in [
            (pcsc::Error::SharingViolation, "reader_busy"),
            (pcsc::Error::RemovedCard, "reader_card_absent"),
            (pcsc::Error::NoReadersAvailable, "reader_unavailable"),
        ] {
            assert_eq!(
                super::IdentityError::Session(super::session::SessionError::Pcsc(error))
                    .failure_code(),
                expected
            );
            assert_eq!(
                super::AuthenticationError::Session(super::session::SessionError::Pcsc(error))
                    .failure_code(),
                expected
            );
        }
    }

    #[test]
    fn detached_worker_keeps_reader_busy_until_physical_operation_finishes() {
        let reader = "fixture-timeout-ownership";
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let lock = super::reader_auth_lock(reader);
            let _guard = super::acquire_reader_operation(&lock).unwrap();
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
        });
        started_rx.recv().unwrap();
        // Simulate the HTTP waiter disappearing while its blocking worker
        // remains active. Both authentication entry points reject before I/O.
        let automatic =
            super::authenticate_aka_auto(reader, super::AkaApplication::Usim, &[0; 16], &[0; 16]);
        let explicit = super::authenticate_aka(reader, &[0xa0; 7], &[0; 16], &[0; 16]);
        assert_eq!(automatic.unwrap_err().failure_code(), "reader_busy");
        assert_eq!(explicit.unwrap_err().failure_code(), "reader_busy");
        assert!(matches!(
            super::read_identity_auto(reader),
            Err(super::IdentityError::Session(
                super::session::SessionError::Pcsc(pcsc::Error::SharingViolation)
            ))
        ));
        let other = super::reader_auth_lock("fixture-independent-reader");
        assert!(super::acquire_reader_operation(&other).is_ok());
        finish_tx.send(()).unwrap();
        worker.join().unwrap();
        let lock = super::reader_auth_lock(reader);
        assert!(super::acquire_reader_operation(&lock).is_ok());
    }

    #[test]
    fn shared_reader_operation_gate_rejects_same_reader_without_blocking_other_readers() {
        let reader = "fixture-shared-operation-gate";
        let result = super::with_reader_operation(reader, || {
            let same_reader = super::with_reader_operation(reader, || ());
            assert!(matches!(
                same_reader,
                Err(super::session::SessionError::Pcsc(
                    pcsc::Error::SharingViolation
                ))
            ));

            let other_reader =
                super::with_reader_operation("fixture-independent-operation-gate", || 42).unwrap();
            assert_eq!(other_reader, 42);
            "operation completed"
        })
        .unwrap();

        assert_eq!(result, "operation completed");
        assert_eq!(
            super::with_reader_operation(reader, || 7).unwrap(),
            7,
            "the reader gate is released when the operation returns"
        );
    }

    use proletarius_sim::{AkaResult, ApduTransport};

    use super::{
        AkaApplication, AuthenticationError, CardPresence, ReaderDiscovery, ReaderObservation,
        authenticate_auto_with_transport, authenticate_module_bound_with_transport,
        authenticate_with_transport, discover_module_usim_aid_with_transport,
        discover_usim_aid_with_transport, read_carrier_identity_with_transport,
        read_iccid_with_transport, read_identity_auto_with_transport, read_identity_with_transport,
        read_imsi_file, usim_aid_from_dir_record,
    };

    struct ScriptedTransport {
        responses: VecDeque<Vec<u8>>,
        commands: Vec<Vec<u8>>,
    }

    impl ApduTransport for ScriptedTransport {
        type Error = String;

        fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
            self.commands.push(command.to_vec());
            self.responses
                .pop_front()
                .ok_or_else(|| "missing response".to_owned())
        }
    }

    fn reader(name: &str, card: CardPresence) -> ReaderObservation {
        ReaderObservation {
            name: name.to_owned(),
            card,
            in_use: false,
            muted: false,
            atr: Vec::new(),
        }
    }

    #[test]
    fn revision_changes_only_when_observed_state_changes() {
        let mut discovery = ReaderDiscovery::default();
        let first = discovery.reconcile(
            vec![reader("Reader A", CardPresence::Present)],
            Some("Reader A"),
        );
        assert_eq!(first.revision, 1);
        let unchanged = discovery.reconcile(first.readers.clone(), Some("Reader A"));
        assert_eq!(unchanged.revision, 1);

        let removed = discovery.reconcile(Vec::new(), Some("Reader A"));
        assert_eq!(removed.revision, 2);
        assert!(removed.selected.is_none());
    }

    #[test]
    fn selection_is_never_retained_after_reader_disappears() {
        let mut discovery = ReaderDiscovery::default();
        let snapshot = discovery.reconcile(
            vec![reader("Reader B", CardPresence::Absent)],
            Some("Reader A"),
        );
        assert_eq!(snapshot.selected, None);
        assert_eq!(snapshot.readers[0].card, CardPresence::Absent);
    }

    #[test]
    fn aka_selects_full_aid_before_authentication() {
        let mut success = vec![0xdb, 8];
        success.extend_from_slice(&[1; 8]);
        success.push(16);
        success.extend_from_slice(&[2; 16]);
        success.push(16);
        success.extend_from_slice(&[3; 16]);
        success.extend_from_slice(&[0x90, 0x00]);
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([vec![0x90, 0x00], success]),
            commands: Vec::new(),
        };
        let aid = [0xa0, 0, 0, 0x87, 0x10, 0x04, 0xff, 1];
        let result = authenticate_with_transport(&mut transport, &aid, &[0x11; 16], &[0x22; 16])
            .expect("AKA success");
        assert!(
            matches!(result, AkaResult::Success { res, ck, ik } if res == vec![1; 8] && ck == [2; 16] && ik == [3; 16])
        );
        assert_eq!(&transport.commands[0][5..], aid);
        assert_eq!(&transport.commands[1][..6], &[0, 0x88, 0, 0x81, 34, 16]);
    }

    #[test]
    fn auto_aka_discovers_isim_without_selecting_usim() {
        let usim = [0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff];
        let isim = [0xa0, 0, 0, 0, 0x87, 0x10, 4, 0xfe];
        let mut usim_record = vec![0x61, 10, 0x4f, 8];
        usim_record.extend_from_slice(&usim);
        usim_record.extend_from_slice(&[0x90, 0]);
        let mut isim_record = vec![0x61, 10, 0x4f, 8];
        isim_record.extend_from_slice(&isim);
        isim_record.extend_from_slice(&[0x90, 0]);
        let mut success = vec![0xdb, 8];
        success.extend_from_slice(&[1; 8]);
        success.push(16);
        success.extend_from_slice(&[2; 16]);
        success.push(16);
        success.extend_from_slice(&[3; 16]);
        success.extend_from_slice(&[0x90, 0]);
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([
                vec![0x90, 0],
                vec![0x90, 0],
                usim_record,
                isim_record,
                vec![0x90, 0],
                success,
            ]),
            commands: Vec::new(),
        };
        assert!(
            authenticate_auto_with_transport(
                &mut transport,
                AkaApplication::Isim,
                &[0x11; 16],
                &[0x22; 16]
            )
            .is_ok()
        );
        assert_eq!(&transport.commands[4][5..], isim);
        assert_eq!(transport.commands[5][1], 0x88);
    }

    #[test]
    fn auto_aka_missing_isim_does_not_send_authentication() {
        let usim = [0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff];
        let mut record = vec![0x61, 10, 0x4f, 8];
        record.extend_from_slice(&usim);
        record.extend_from_slice(&[0x90, 0]);
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([vec![0x90, 0], vec![0x90, 0], record, vec![0x6a, 0x83]]),
            commands: Vec::new(),
        };
        assert!(matches!(
            authenticate_auto_with_transport(
                &mut transport,
                AkaApplication::Isim,
                &[0x11; 16],
                &[0x22; 16]
            ),
            Err(AuthenticationError::ApplicationUnavailable)
        ));
        assert_eq!(transport.commands.len(), 4);
    }

    #[test]
    fn identity_reads_mf_iccid_in_a_bounded_sequence() {
        let aid = [0xa0, 0, 0, 0x87, 0x10, 0x02, 0xff, 1];
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([
                vec![0x90, 0],
                vec![0x90, 0],
                vec![0x90, 0],
                vec![
                    0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6, 0x90, 0,
                ],
            ]),
            commands: Vec::new(),
        };
        assert_eq!(
            read_iccid_with_transport(&mut transport, &aid).unwrap(),
            "8901234567890123456"
        );
        assert_eq!(transport.commands.len(), 4);
        assert_eq!(transport.commands[1], [0, 0xa4, 0, 4, 2, 0x3f, 0, 0]);
        assert_eq!(transport.commands[2], [0, 0xa4, 0, 4, 2, 0x2f, 0xe2, 0]);
        assert_eq!(transport.commands[3], [0, 0xb0, 0, 0, 10]);
    }

    #[test]
    fn identity_uses_imsi_declared_length_and_mnc_from_ef_ad() {
        let aid = [0xa0, 0, 0, 0x87, 0x10, 0x02, 0xff, 1];
        let imsi = [0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68];
        let iccid = [0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6];
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([
                vec![0x90, 0],    // USIM AID
                vec![0x90, 0],    // EF-IMSI
                vec![8, 0x90, 0], // length byte
                [imsi.as_slice(), &[0x90, 0]].concat(),
                vec![0x90, 0], // EF-AD
                vec![0, 0, 0, 2, 0x90, 0],
                vec![0x90, 0], // USIM AID for ICCID read
                vec![0x90, 0], // MF
                vec![0x90, 0], // EF-ICCID
                [iccid.as_slice(), &[0x90, 0]].concat(),
            ]),
            commands: Vec::new(),
        };
        let identity = read_identity_with_transport(&mut transport, &aid).unwrap();
        assert_eq!(identity.imsi, "234100896392986");
        assert_eq!(identity.mcc, "234");
        assert_eq!(identity.mnc, "10");
        assert_eq!(identity.iccid, "8901234567890123456");
        assert_eq!(transport.commands[2], [0, 0xb0, 0, 0, 1]);
        assert_eq!(transport.commands[3], [0, 0xb0, 0, 0, 9]);
        assert_eq!(transport.commands[5], [0, 0xb0, 0, 0, 4]);
    }

    #[test]
    fn carrier_identity_reads_optional_selector_fields_in_same_transaction() {
        let aid = [0xa0, 0, 0, 0x87, 0x10, 0x02, 0xff, 1];
        let imsi = [0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68];
        let iccid = [0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6];
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([
                vec![0x90, 0],
                vec![0x90, 0],
                vec![8, 0x90, 0],
                [imsi.as_slice(), &[0x90, 0]].concat(),
                vec![0x90, 0],
                vec![0, 0, 0, 2, 0x90, 0],
                vec![0x90, 0],
                vec![0x50, 0x8f, 0xff, 0xff, 0x90, 0],
                vec![0x90, 0],
                vec![0x42, 0xff, 0x90, 0],
                vec![0x90, 0],
                vec![
                    0x00, b'g', b'i', b'f', b'f', b'g', b'a', b'f', b'f', 0xff, 0x90, 0,
                ],
                vec![0x90, 0],
                vec![0x90, 0],
                vec![0x90, 0],
                [iccid.as_slice(), &[0x90, 0]].concat(),
            ]),
            commands: Vec::new(),
        };
        let result = read_carrier_identity_with_transport(&mut transport, &aid).unwrap();
        assert_eq!(result.identity.imsi, "234100896392986");
        assert_eq!(result.application_aid, aid);
        assert_eq!(result.identity.mnc, "10");
        assert_eq!(result.gid1, "508FFFFF");
        assert_eq!(result.gid2, "42FF");
        assert_eq!(result.spn, "giffgaff");
        assert_eq!(transport.commands[6], [0, 0xa4, 0, 4, 2, 0x6f, 0x3e, 0]);
        assert_eq!(transport.commands[7], [0, 0xb0, 0, 0, 32]);
        assert_eq!(transport.commands[8], [0, 0xa4, 0, 4, 2, 0x6f, 0x3f, 0]);
        assert_eq!(transport.commands[9], [0, 0xb0, 0, 0, 32]);
        assert_eq!(transport.commands[10], [0, 0xa4, 0, 4, 2, 0x6f, 0x46, 0]);
        assert_eq!(transport.commands[11], [0, 0xb0, 0, 0, 17]);
    }

    #[test]
    fn spn_decoder_rejects_unknown_bytes_without_selecting_a_name() {
        assert_eq!(super::decode_spn(&[0, b'O', b'2', 0xff]), Some("O2".into()));
        assert_eq!(super::decode_spn(&[0, 0x1b, b'A']), None);
        assert_eq!(super::decode_spn(&[0xff]), None);
    }

    #[test]
    fn absent_ef_ad_preserves_iccid_and_imsi_without_guessing_mnc() {
        let aid = [0xa0, 0, 0, 0x87, 0x10, 0x02, 0xff, 1];
        let imsi = [0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68];
        let iccid = [0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6];
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([
                vec![0x90, 0],
                vec![0x90, 0],
                vec![8, 0x90, 0],
                [imsi.as_slice(), &[0x90, 0]].concat(),
                vec![0x6a, 0x82], // EF-AD absent
                vec![0x90, 0],
                vec![0x90, 0],
                vec![0x90, 0],
                [iccid.as_slice(), &[0x90, 0]].concat(),
            ]),
            commands: Vec::new(),
        };
        let identity = read_identity_with_transport(&mut transport, &aid).unwrap();
        assert_eq!(identity.imsi, "234100896392986");
        assert_eq!(identity.mnc, "");
        assert_eq!(identity.iccid, "8901234567890123456");
    }

    #[test]
    fn invalid_imsi_length_is_rejected_before_second_read() {
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([vec![0x90, 0], vec![0xff, 0x90, 0]]),
            commands: Vec::new(),
        };
        assert!(read_imsi_file(&mut transport).is_err());
        assert_eq!(transport.commands.len(), 2);
    }

    #[test]
    fn ef_dir_discovery_uses_complete_usim_aid_not_an_isim_prefix() {
        let isim = [0xa0, 0, 0, 0, 0x87, 0x10, 0x04, 0xff];
        let usim = [0xa0, 0, 0, 0, 0x87, 0x10, 0x02, 0xff, 1];
        let mut first = vec![0x61, 10, 0x4f, 8];
        first.extend_from_slice(&isim);
        first.extend_from_slice(&[0xff, 0x90, 0]);
        let mut second = vec![0x61, 11, 0x4f, 9];
        second.extend_from_slice(&usim);
        second.extend_from_slice(&[0xff, 0x90, 0]);
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([vec![0x90, 0], vec![0x90, 0], first, second]),
            commands: Vec::new(),
        };
        assert_eq!(
            discover_usim_aid_with_transport(&mut transport).unwrap(),
            usim
        );
        assert_eq!(transport.commands[0], [0, 0xa4, 0, 4, 2, 0x3f, 0, 0]);
        assert_eq!(transport.commands[1], [0, 0xa4, 0, 4, 2, 0x2f, 0, 0]);
        assert_eq!(transport.commands[2], [0, 0xb2, 1, 4, 0]);
        assert_eq!(transport.commands[3], [0, 0xb2, 2, 4, 0]);
    }

    #[test]
    fn module_ef_dir_skips_only_exact_6986_and_reads_later_usim_record() {
        let usim = [0xa0, 0, 0, 0, 0x87, 0x10, 0x02, 0xff, 1];
        let mut record = vec![0x61, 11, 0x4f, 9];
        record.extend_from_slice(&usim);
        record.extend_from_slice(&[0xff, 0x90, 0]);
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([
                vec![0x90, 0],
                vec![0x90, 0],
                vec![0x69, 0x86], // module quirk: this record is inaccessible
                record,
            ]),
            commands: Vec::new(),
        };

        assert_eq!(
            discover_module_usim_aid_with_transport(&mut transport).unwrap(),
            usim
        );
        assert_eq!(transport.commands[2], [0, 0xb2, 1, 4, 0]);
        assert_eq!(transport.commands[3], [0, 0xb2, 2, 4, 0]);
    }

    #[test]
    fn module_ef_dir_does_not_skip_other_card_statuses() {
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([vec![0x90, 0], vec![0x90, 0], vec![0x69, 0x85]]),
            commands: Vec::new(),
        };

        assert!(discover_module_usim_aid_with_transport(&mut transport).is_err());
        assert_eq!(transport.commands.len(), 3);
    }

    #[test]
    fn module_bound_aka_skips_exact_6986_and_fails_before_auth_on_identity_change() {
        let aid = [0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff, 1];
        let mut dir = vec![0x61, 11, 0x4f, 9];
        dir.extend_from_slice(&aid);
        dir.extend_from_slice(&[0x90, 0]);
        let base = VecDeque::from([
            vec![0x90, 0],    // MF
            vec![0x90, 0],    // EF-DIR
            vec![0x69, 0x86], // exact module record quirk
            dir,
            vec![0x90, 0], // select USIM
            vec![0x90, 0], // select EF-IMSI
            vec![8, 0x90, 0],
            vec![
                0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68, 0x90, 0,
            ],
            vec![0x6a, 0x82], // optional EF-AD absent
            vec![0x90, 0],    // reselect USIM before ICCID
            vec![0x90, 0],    // select MF
            vec![0x90, 0],    // select EF-ICCID
            vec![
                0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6, 0x90, 0,
            ],
        ]);
        let mut transport = ScriptedTransport {
            responses: base,
            commands: Vec::new(),
        };
        let error = authenticate_module_bound_with_transport(
            &mut transport,
            AkaApplication::Usim,
            "8901234567890123456",
            "234100896392987",
            &[0x11; 16],
            &[0x22; 16],
        )
        .unwrap_err();
        assert!(
            matches!(error, AuthenticationError::IdentityChanged),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            transport.commands.len(),
            13,
            "identity mismatch must stop before AUTHENTICATE"
        );
    }

    #[test]
    fn malformed_ef_dir_record_never_yields_a_prefix_aid() {
        assert_eq!(usim_aid_from_dir_record(&[0x61, 3, 0x4f, 8, 0xa0]), None);
        assert_eq!(
            usim_aid_from_dir_record(&[0x61, 9, 0x4f, 7, 0xa0, 0, 0, 0, 0x87, 0x10, 2]),
            None
        );
    }

    #[test]
    fn auto_identity_reads_aid_and_identity_in_one_apdu_sequence() {
        let aid = [0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff, 1];
        let imsi = [0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68];
        let iccid = [0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6];
        let mut dir = vec![0x61, 11, 0x4f, 9];
        dir.extend_from_slice(&aid);
        dir.extend_from_slice(&[0x90, 0]);
        let mut transport = ScriptedTransport {
            responses: VecDeque::from([
                vec![0x90, 0], // MF
                vec![0x90, 0], // EF-DIR
                dir,
                vec![0x90, 0], // USIM AID
                vec![0x90, 0], // EF-IMSI
                vec![8, 0x90, 0],
                [imsi.as_slice(), &[0x90, 0]].concat(),
                vec![0x6a, 0x82], // no EF-AD
                vec![0x90, 0],    // USIM AID
                vec![0x90, 0],    // MF
                vec![0x90, 0],    // EF-ICCID
                [iccid.as_slice(), &[0x90, 0]].concat(),
            ]),
            commands: Vec::new(),
        };
        let identity_responses = transport.responses.clone();
        let identity = read_identity_auto_with_transport(&mut transport).unwrap();
        assert_eq!(identity.imsi, "234100896392986");
        assert_eq!(identity.iccid, "8901234567890123456");
        assert_eq!(identity.mnc, "");
        assert_eq!(transport.commands.len(), 12);
        for (iccid, imsi) in [
            ("8901234567890123457", identity.imsi.as_str()),
            (identity.iccid.as_str(), "234100896392987"),
        ] {
            let mut changed = ScriptedTransport {
                responses: identity_responses.clone(),
                commands: Vec::new(),
            };
            let error = super::authenticate_bound_with_transport(
                &mut changed,
                AkaApplication::Usim,
                iccid,
                imsi,
                &[0x11; 16],
                &[0x22; 16],
            )
            .unwrap_err();
            assert!(matches!(error, super::AuthenticationError::IdentityChanged));
            assert_eq!(error.failure_code(), "reader_identity_changed");
            assert!(!changed.commands.iter().any(|command| command[1] == 0x88));
        }
        let mut success = vec![0xdb, 8];
        success.extend([1; 8]);
        success.push(16);
        success.extend([2; 16]);
        success.push(16);
        success.extend([3; 16]);
        success.extend([0x90, 0]);
        let mut record = vec![0x61, 11, 0x4f, 9];
        record.extend(aid);
        record.extend([0x90, 0]);
        let mut responses = identity_responses;
        responses.extend([vec![0x90, 0], vec![0x90, 0], record, vec![0x90, 0], success]);
        let mut matching = ScriptedTransport {
            responses,
            commands: Vec::new(),
        };
        assert!(
            super::authenticate_bound_with_transport(
                &mut matching,
                AkaApplication::Usim,
                &identity.iccid,
                &identity.imsi,
                &[0x11; 16],
                &[0x22; 16]
            )
            .is_ok()
        );
        assert_eq!(
            matching
                .commands
                .iter()
                .filter(|command| command[1] == 0x88)
                .count(),
            1
        );
    }
}
