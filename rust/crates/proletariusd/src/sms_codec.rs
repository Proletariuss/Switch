//! Native RP envelope encoding only; this endpoint never sends an SMS.
use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const STORED_SNAPSHOT_BODY_LIMIT: usize = 512 * 1024;
const STORED_SNAPSHOT_RECORD_LIMIT: usize = 4096;

static STORED_REASSEMBLER: OnceLock<Mutex<proletarius_sms::reassembly::Reassembler>> =
    OnceLock::new();

#[derive(Deserialize)]
struct EncodeRequest {
    reference: u8,
    smsc: String,
    tpdu: Vec<u8>,
}

pub(crate) async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let decoding = request.uri().path() == "/api/v2/sms/decode-rpdu";
    let preparing = request.uri().path() == "/api/v2/sms/prepare-tpdu";
    let decoding_incoming = request.uri().path() == "/api/v2/sms/decode-incoming-tpdu";
    let decoding_stored_snapshot = request.uri().path() == "/api/v2/sms/decode-stored-snapshot";
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        if preparing {
            262144
        } else if decoding_stored_snapshot {
            STORED_SNAPSHOT_BODY_LIMIT
        } else {
            4096
        },
        std::time::Duration::from_secs(5),
    )
    .await
    {
        Ok(body) => body,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(StatusCode::REQUEST_TIMEOUT);
        }
        Err(_) => return failure(StatusCode::BAD_REQUEST),
    };
    if decoding_incoming {
        decode_incoming(&body)
    } else if decoding_stored_snapshot {
        decode_stored_snapshot(&body)
    } else if preparing {
        prepare(&body)
    } else if decoding {
        decode(&body)
    } else {
        encode(&body)
    }
}

fn decode_incoming(body: &[u8]) -> Response<Full<Bytes>> {
    #[derive(Deserialize)]
    struct Input {
        tpdu: Vec<u8>,
        #[serde(default)]
        stored: bool,
        #[serde(default)]
        reassemble: bool,
    }
    let Ok(input) = serde_json::from_slice::<Input>(body) else {
        return failure(StatusCode::BAD_REQUEST);
    };
    match input.tpdu.first().map(|value| value & 3) {
        Some(0) => decode_deliver_tpdu(&input.tpdu, input.stored, input.reassemble),
        Some(2) => decode_status_report_tpdu(&input.tpdu),
        _ => failure(StatusCode::UNPROCESSABLE_ENTITY),
    }
}

fn decode_deliver_tpdu(tpdu: &[u8], stored: bool, reassemble: bool) -> Response<Full<Bytes>> {
    match decode_deliver_value(tpdu, stored, reassemble) {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(status) => failure(status),
    }
}

fn decode_deliver_value(
    tpdu: &[u8],
    stored: bool,
    reassemble: bool,
) -> Result<serde_json::Value, StatusCode> {
    let message = if stored {
        proletarius_sms::deliver::parse_stored(tpdu)
    } else {
        proletarius_sms::deliver::parse(tpdu)
    };
    let message = message.map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    let binary_summary = proletarius_sms::binary_sms::format(&message);
    let is_binary = binary_summary.is_some();
    let wants_reassembly = stored && reassemble && message.concatenation.is_some();
    let reassembled = if wants_reassembly {
        let mut reassembly_message = message.clone();
        if let Some(summary) = binary_summary.as_ref() {
            reassembly_message.content = proletarius_sms::user_data::Content::Text(summary.clone());
        }
        match reassemble_stored(reassembly_message) {
            Ok(value) => value,
            Err(()) => return Err(StatusCode::UNPROCESSABLE_ENTITY),
        }
    } else {
        None
    };
    let complete = !wants_reassembly || reassembled.is_some();
    let (text, binary, binary_summary) = if !complete {
        (None, None, None)
    } else if is_binary {
        let summary = if wants_reassembly {
            reassembled
        } else {
            binary_summary
        };
        let raw = match &message.content {
            proletarius_sms::user_data::Content::Binary(value) => Some(value.clone()),
            proletarius_sms::user_data::Content::Text(_) => None,
        };
        (None, raw, summary)
    } else if wants_reassembly {
        (reassembled, None, None)
    } else {
        match message.content {
            proletarius_sms::user_data::Content::Text(value) => (Some(value), None, None),
            proletarius_sms::user_data::Content::Binary(value) => (None, Some(value), None),
        }
    };
    let concat = message.concatenation.map(|value| {
        json!({
            "reference":value.reference,"wide_reference":value.wide_reference,
            "total":value.total,"sequence":value.sequence
        })
    });
    let service_center_time = message.service_center_time.map(|value| {
        json!({
            "year":value.year,"month":value.month,"day":value.day,
            "hour":value.hour,"minute":value.minute,"second":value.second,
            "offset_minutes":value.offset_minutes
        })
    });
    Ok(json!({
        "kind":"deliver",
        "sender":message.sender,"timestamp":message.timestamp,
        "service_center_time":service_center_time,
        "protocol_id":message.protocol_id,"dcs":message.dcs,
        "text":text,"binary":binary,"binary_summary":binary_summary,
        "concatenation":concat,"is_binary":is_binary,"complete":complete
    }))
}

fn decode_stored_snapshot(body: &[u8]) -> Response<Full<Bytes>> {
    #[derive(Deserialize)]
    struct Input {
        response: String,
        #[serde(default)]
        single_index: Option<u32>,
    }

    let Ok(input) = serde_json::from_slice::<Input>(body) else {
        return failure(StatusCode::BAD_REQUEST);
    };
    if input.single_index == Some(0) {
        return failure(StatusCode::BAD_REQUEST);
    }
    let response = match input.single_index {
        Some(index) => stored_read_snapshot(&input.response, index),
        None => input.response.clone(),
    };
    match decode_stored_snapshot_response(&response) {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(status) => failure(status),
    }
}

/// Decode a bounded modem CMGL response using the same record and multipart
/// rules exposed to the legacy Go compatibility consumer. The native SMS
/// receive coordinator uses this function so framing, PDU parsing and group
/// conflict handling stay in one Rust implementation.
pub(crate) fn decode_stored_snapshot_response(
    response: &str,
) -> Result<serde_json::Value, StatusCode> {
    let mut snapshot = decode_stored_snapshot_with_sources(response)?;
    if let Some(records) = snapshot["records"].as_array_mut() {
        for record in records {
            if let Some(object) = record.as_object_mut() {
                object.remove("source_indices");
                object.remove("canonical_pdu");
            }
        }
    }
    Ok(snapshot)
}

/// Internal USB proof metadata never crosses the legacy codec RPC. In
/// particular, a second full PDU must not inflate that bounded wire response.
pub(crate) fn decode_stored_snapshot_with_sources(
    response: &str,
) -> Result<serde_json::Value, StatusCode> {
    let mut records = Vec::new();
    let mut groups: HashMap<StoredConcatKey, StoredConcatGroup> = HashMap::new();
    let lines = response.replace('\r', "\n");
    let lines: Vec<&str> = lines
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut index = 0;
    while index + 1 < lines.len() {
        let header = lines[index];
        if !header.starts_with("+CMGL:") {
            index += 1;
            continue;
        }
        let pdu = lines[index + 1];
        if !is_hex_string(pdu) {
            index += 1;
            continue;
        }
        if records.len() >= STORED_SNAPSHOT_RECORD_LIMIT {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        let record_index = cmgl_index(header);
        let canonical_pdu = if parse_header_tpdu_length(header).is_some() {
            pdu.to_ascii_uppercase()
        } else {
            pdu.to_owned()
        };
        match extract_stored_tpdu(header, pdu) {
            Err(error) => records.push(json!({
                "index":record_index,"pdu":canonical_pdu,"error":error
            })),
            Ok(tpdu) => match decode_deliver_value(&tpdu, true, false) {
                Ok(mut decoded) => {
                    if let Some((key, sequence)) = stored_concat_key(&decoded) {
                        let content = decoded["binary_summary"]
                            .as_str()
                            .or_else(|| decoded["text"].as_str())
                            .unwrap_or_default()
                            .to_owned();
                        let timestamp = decoded["service_center_time"].clone();
                        decoded["complete"] = json!(false);
                        decoded["text"] = serde_json::Value::Null;
                        decoded["binary_summary"] = serde_json::Value::Null;
                        let position = records.len();
                        let group = groups.entry(key).or_default();
                        group.positions.push(position);
                        let part = StoredConcatPart {
                            pdu: canonical_pdu.clone(),
                            content,
                            position,
                            service_center_time: timestamp,
                        };
                        if let Some(previous) = group.parts.get(&sequence) {
                            if !previous.pdu.eq_ignore_ascii_case(&part.pdu) {
                                group.ambiguous = true;
                            }
                        } else {
                            group.parts.insert(sequence, part);
                        }
                    }
                    records.push(json!({
                        "index":record_index,"pdu":canonical_pdu,"decoded":decoded,
                        "source_indices":[record_index],
                        "canonical_pdu":canonical_stored_pdu(header, pdu)
                    }));
                }
                Err(_) => records.push(json!({
                    "index":record_index,"pdu":canonical_pdu,"error":"tpdu_decode_failed"
                })),
            },
        }
        index += 2;
    }
    for (key, group) in groups {
        if group.ambiguous || group.parts.len() != usize::from(key.total) {
            continue;
        }
        let mut content = String::new();
        let mut complete_position = 0;
        let mut first_timestamp = None;
        let mut complete = true;
        for sequence in 1..=key.total {
            let Some(part) = group.parts.get(&sequence) else {
                complete = false;
                break;
            };
            content.push_str(&part.content);
            complete_position = complete_position.max(part.position);
            if sequence == 1 {
                first_timestamp = Some(part.service_center_time.clone());
            }
        }
        if !complete {
            continue;
        }
        let source_indices: Vec<_> = group
            .positions
            .iter()
            .filter_map(|position| records.get(*position).map(|record| record["index"].clone()))
            .collect();
        let Some(decoded) = records
            .get_mut(complete_position)
            .and_then(|record| record.get_mut("decoded"))
        else {
            continue;
        };
        decoded["complete"] = json!(true);
        if decoded["is_binary"].as_bool().unwrap_or(false) {
            decoded["binary_summary"] = json!(content);
        } else {
            decoded["text"] = json!(content);
        }
        if let Some(timestamp) = first_timestamp.filter(|value| !value.is_null()) {
            decoded["service_center_time"] = timestamp;
        }
        records[complete_position]["source_indices"] = json!(source_indices);
    }
    Ok(json!({"records":records}))
}

// Convert one AT+CMGR response to the same bounded snapshot representation
// used by AT+CMGL. `single_index` is a Go-compatible transitional hook: the
// Rust service owns the response framing and PDU length normalization while
// the Go serial owner still performs the command and supplies its result.
fn stored_read_snapshot(response: &str, source_index: u32) -> String {
    let Some(pdu) = stored_read_pdu(response) else {
        return String::new();
    };
    format!("+CMGL: {source_index}\r\n{pdu}\r\n")
}

pub(crate) fn stored_read_pdu(response: &str) -> Option<String> {
    let normalized = response.replace('\r', "");
    let lines: Vec<&str> = normalized.split('\n').map(str::trim).collect();
    for (position, header) in lines.iter().enumerate() {
        if !header.starts_with("+CMGR:") {
            continue;
        }
        let Some(raw_pdu) = lines.get(position + 1).copied() else {
            return None;
        };
        if raw_pdu.is_empty() || raw_pdu == "OK" {
            return None;
        }
        let pdu = match parse_header_tpdu_length(header) {
            Some(tpdu_length) => trim_full_pdu_by_tpdu_length(raw_pdu, tpdu_length),
            None => raw_pdu.to_owned(),
        };
        if !is_hex_string(&pdu) {
            return None;
        }
        return Some(pdu);
    }
    None
}

/// A deletion fingerprint preserves the complete SMSC and TPDU. Only explicit
/// modem framing padding is removed, exactly as for CMGR readback. Malformed
/// or truncated framing must never grant authority to delete a storage slot.
pub(crate) fn canonical_stored_pdu(header: &str, raw_pdu: &str) -> Option<String> {
    let raw_pdu = raw_pdu.trim();
    if raw_pdu.len() > 8192 || !is_hex_string(raw_pdu) {
        return None;
    }
    let full = decode_hex(raw_pdu).ok()?;
    let offset = 1_usize.checked_add(usize::from(*full.first()?))?;
    if offset.checked_add(2)? > full.len() {
        return None;
    }
    match parse_header_tpdu_length(header) {
        Some(length) if length >= 2 && offset.checked_add(length)? <= full.len() => {
            Some(trim_full_pdu_by_tpdu_length(raw_pdu, length))
        }
        Some(_) => None,
        None => Some(raw_pdu.to_ascii_uppercase()),
    }
}

// Match the Go SMS storage framing helper used before the Rust codec owned
// CMGR responses. This only bounds known SMSC/TPDU framing and never invents
// bytes when a modem header is incomplete or malformed.
fn trim_full_pdu_by_tpdu_length(raw_pdu: &str, tpdu_length: usize) -> String {
    let raw_pdu = raw_pdu.trim();
    let Ok(mut full) = decode_hex(raw_pdu) else {
        return raw_pdu.to_owned();
    };
    let Some(smsc_length) = full.first().copied() else {
        return raw_pdu.to_owned();
    };
    let tpdu_offset = 1_usize + usize::from(smsc_length);
    let Some(expected_length) = tpdu_offset.checked_add(tpdu_length) else {
        return raw_pdu.to_ascii_uppercase();
    };
    if expected_length > full.len() {
        return raw_pdu.to_ascii_uppercase();
    }
    full.truncate(expected_length);
    if let Some(tpdu_end) = trim_deliver_tpdu_padding(&full[tpdu_offset..]) {
        full.truncate(tpdu_offset + tpdu_end);
    }
    encode_hex_upper(&full)
}

fn trim_deliver_tpdu_padding(tpdu: &[u8]) -> Option<usize> {
    if tpdu.first().is_none_or(|first| first & 0x03 != 0) {
        return None;
    }
    let mut offset = 1_usize;
    if offset.checked_add(2)? > tpdu.len() {
        return None;
    }
    let address_digits = usize::from(tpdu[offset]);
    offset = offset.checked_add(2 + address_digits.div_ceil(2))?;
    if offset.checked_add(10)? > tpdu.len() {
        return None;
    }
    let dcs = tpdu[offset + 1];
    offset = offset.checked_add(2 + 7)?;
    let user_data_length = usize::from(*tpdu.get(offset)?);
    let data_offset = offset.checked_add(1)?;
    let data_octets = match proletarius_sms::user_data::coding(dcs) {
        proletarius_sms::user_data::Coding::Gsm7 => user_data_length.checked_mul(7)?.div_ceil(8),
        proletarius_sms::user_data::Coding::Binary | proletarius_sms::user_data::Coding::Utf16 => {
            user_data_length
        }
        proletarius_sms::user_data::Coding::Unsupported => return None,
    };
    let declared_end = data_offset.checked_add(data_octets)?;
    (declared_end < tpdu.len()).then_some(declared_end)
}

fn encode_hex_upper(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StoredConcatKey {
    sender: String,
    reference: u16,
    wide_reference: bool,
    total: u8,
    dcs: u8,
}

#[derive(Default)]
struct StoredConcatGroup {
    ambiguous: bool,
    parts: HashMap<u8, StoredConcatPart>,
    positions: Vec<usize>,
}

struct StoredConcatPart {
    pdu: String,
    content: String,
    position: usize,
    service_center_time: serde_json::Value,
}

fn stored_concat_key(decoded: &serde_json::Value) -> Option<(StoredConcatKey, u8)> {
    let concatenation = decoded.get("concatenation")?.as_object()?;
    let sender = decoded.get("sender")?.as_str()?.to_owned();
    let reference = u16::try_from(concatenation.get("reference")?.as_u64()?).ok()?;
    let wide_reference = concatenation.get("wide_reference")?.as_bool()?;
    let total = u8::try_from(concatenation.get("total")?.as_u64()?).ok()?;
    let sequence = u8::try_from(concatenation.get("sequence")?.as_u64()?).ok()?;
    let dcs = u8::try_from(decoded.get("dcs")?.as_u64()?).ok()?;
    if total < 2 || sequence == 0 || sequence > total || (!wide_reference && reference > 255) {
        return None;
    }
    Some((
        StoredConcatKey {
            sender,
            reference,
            wide_reference,
            total,
            dcs,
        },
        sequence,
    ))
}

fn is_hex_string(value: &str) -> bool {
    value.len() >= 2
        && value.len().is_multiple_of(2)
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn cmgl_index(header: &str) -> usize {
    header
        .strip_prefix("+CMGL:")
        .map(str::trim_start)
        .map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().unwrap_or(0)
        })
        .unwrap_or(0)
}

fn parse_header_tpdu_length(header: &str) -> Option<usize> {
    let token = header.rsplit_once(',')?.1.trim();
    token.parse::<usize>().ok()
}

fn extract_stored_tpdu(header: &str, pdu_hex: &str) -> Result<Vec<u8>, &'static str> {
    let mut full = decode_hex(pdu_hex).map_err(|_| "invalid_pdu_hex")?;
    if let Some(tpdu_length) = parse_header_tpdu_length(header)
        && let Some(smsc_length) = full.first().copied()
    {
        let expected = 1_usize
            .checked_add(usize::from(smsc_length))
            .and_then(|offset| offset.checked_add(tpdu_length));
        if let Some(expected) = expected.filter(|expected| *expected <= full.len()) {
            full.truncate(expected);
        }
    }
    if full.len() < 2 {
        return Err("pdu_too_short");
    }
    let tpdu_offset = 1 + usize::from(full[0]);
    if tpdu_offset >= full.len() {
        return Err("invalid_smsc_length");
    }
    Ok(full[tpdu_offset..].to_vec())
}

fn decode_hex(value: &str) -> Result<Vec<u8>, ()> {
    let bytes = value.as_bytes();
    if !is_hex_string(value) {
        return Err(());
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        let high = hex_nibble(pair[0]).ok_or(())?;
        let low = hex_nibble(pair[1]).ok_or(())?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn reassemble_stored(message: proletarius_sms::deliver::Message) -> Result<Option<String>, ()> {
    let now = Instant::now();
    let mut reassembler = STORED_REASSEMBLER
        .get_or_init(|| Mutex::new(proletarius_sms::reassembly::Reassembler::default()))
        .lock()
        .map_err(|_| ())?;
    reassembler.cleanup(now, Duration::from_secs(10 * 60));
    match reassembler.push_at(message, now) {
        Ok(value) => Ok(value),
        Err(proletarius_sms::reassembly::Error::ConflictingDuplicate) => Ok(None),
        Err(_) => Err(()),
    }
}

fn decode_status_report_tpdu(tpdu: &[u8]) -> Response<Full<Bytes>> {
    let Ok(report) = proletarius_sms::status_report::parse(tpdu) else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY);
    };
    let service_center_time = report.service_center_time.map(|value| {
        json!({
            "year":value.year,"month":value.month,"day":value.day,
            "hour":value.hour,"minute":value.minute,"second":value.second,
            "offset_minutes":value.offset_minutes
        })
    });
    crate::json_response(
        StatusCode::OK,
        &json!({
            "kind":"status_report", "reference":report.reference,
            "recipient":report.recipient, "status_code":report.status_code,
            "delivery_status":report.status.as_str(),
            "service_center_time":service_center_time
        }),
    )
}

fn prepare(body: &[u8]) -> Response<Full<Bytes>> {
    #[derive(Deserialize)]
    struct Input {
        recipient: String,
        text: String,
        reference: u8,
        encoding: Option<String>,
        request_status_report: Option<bool>,
    }
    let Ok(input) = serde_json::from_slice::<Input>(body) else {
        return failure(StatusCode::BAD_REQUEST);
    };
    let encoding = match input.encoding.as_deref().unwrap_or("auto") {
        "auto" => proletarius_sms::submit::EncodingPolicy::Auto,
        "ucs2" => proletarius_sms::submit::EncodingPolicy::Ucs2,
        "modem" => proletarius_sms::submit::EncodingPolicy::Modem,
        _ => return failure(StatusCode::BAD_REQUEST),
    };
    match proletarius_sms::submit::prepare_with_policy(
        &input.recipient,
        &input.text,
        input.reference,
        encoding,
        input.request_status_report.unwrap_or(true),
    ) {
        Ok(parts) => crate::json_response(StatusCode::OK, &json!({"tpdus":parts})),
        Err(_) => failure(StatusCode::UNPROCESSABLE_ENTITY),
    }
}

fn decode(body: &[u8]) -> Response<Full<Bytes>> {
    #[derive(Deserialize)]
    struct DecodeRequest {
        rpdu: Vec<u8>,
    }
    let Ok(input) = serde_json::from_slice::<DecodeRequest>(body) else {
        return failure(StatusCode::BAD_REQUEST);
    };
    match proletarius_sms::MobileTerminatedData::parse(&input.rpdu) {
        Ok(message) => crate::json_response(
            StatusCode::OK,
            &json!({
                "reference": message.reference, "originator": message.originator,
                "tpdu": message.tpdu
            }),
        ),
        Err(_) => failure(StatusCode::UNPROCESSABLE_ENTITY),
    }
}

fn encode(body: &[u8]) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_slice::<EncodeRequest>(body) else {
        return failure(StatusCode::BAD_REQUEST);
    };
    match proletarius_sms::build_mobile_originated_data(input.reference, &input.smsc, &input.tpdu) {
        Ok(rpdu) => crate::json_response(StatusCode::OK, &json!({"rpdu":rpdu})),
        Err(_) => failure(StatusCode::UNPROCESSABLE_ENTITY),
    }
}

fn failure(status: StatusCode) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"短信协议参数无效或读取超时","code":"sms_encode_failed","retryable":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmgl_record(index: usize, pdu: &str) -> String {
        format!("+CMGL: {index},0,,{}\r\n{pdu}\r\n", pdu.len() / 2 - 1)
    }

    async fn stored_snapshot_value(snapshot: &str) -> serde_json::Value {
        let body = serde_json::to_vec(&json!({"response":snapshot})).unwrap();
        let response = decode_stored_snapshot(&body);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn stored_snapshot_parser_decodes_records_and_preserves_per_record_errors() {
        let snapshot = concat!(
            "AT+CMGL=4\r\n",
            "+CMGL: 7,0,,17\r\n",
            "0000038188f8000862906000000000020041\r\n",
            "+CMGL: 8,0,,1\r\n",
            "00\r\n",
            "+CMGL: 9,0,,1\r\n",
            "not-hex\r\n",
            "OK\r\n"
        );
        let body = serde_json::to_vec(&json!({"response":snapshot})).unwrap();
        let response = decode_stored_snapshot(&body);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let records = value["records"].as_array().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["index"], 7);
        assert_eq!(records[0]["pdu"], "0000038188F8000862906000000000020041");
        assert_eq!(records[0]["decoded"]["sender"], "888");
        assert_eq!(records[0]["decoded"]["text"], "A");
        assert_eq!(records[0]["decoded"]["complete"], true);
        assert_eq!(records[1]["index"], 8);
        assert_eq!(records[1]["pdu"], "00");
        assert_eq!(records[1]["error"], "pdu_too_short");
    }

    #[tokio::test]
    async fn cmgr_response_is_normalized_in_rust_before_stored_message_decode() {
        let pdu = "0000038188F8000862906000000000020041";
        let response = format!("+CMGR: 0,,18\r\n{pdu}00\r\nOK\r\n");
        let body = serde_json::to_vec(&json!({"response":response,"single_index":7})).unwrap();
        let result = decode_stored_snapshot(&body);
        assert_eq!(result.status(), StatusCode::OK);
        let bytes = result.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let records = value["records"].as_array().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["index"], 7);
        assert_eq!(records[0]["pdu"], pdu);
        assert_eq!(records[0]["decoded"]["sender"], "888");
        assert_eq!(records[0]["decoded"]["text"], "A");
    }

    #[tokio::test]
    async fn cmgr_response_without_a_pdu_produces_no_record_and_zero_index_is_rejected() {
        let empty = serde_json::to_vec(&json!({
            "response":"+CMGR: 0,,17\r\nOK\r\n",
            "single_index":7
        }))
        .unwrap();
        let response = decode_stored_snapshot(&empty);
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["records"].as_array().unwrap().len(), 0);

        let zero = serde_json::to_vec(&json!({"response":"","single_index":0})).unwrap();
        assert_eq!(
            decode_stored_snapshot(&zero).status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn stored_snapshot_reassembles_only_records_from_that_snapshot() {
        let part_one = "0040038188F800006290600000000008050003C9020182";
        let part_two = "0040038188F800006290700000000008050003C9020284";
        let snapshot = format!("{}{}", cmgl_record(8, part_two), cmgl_record(7, part_one));
        let value = stored_snapshot_value(&snapshot).await;
        let records = value["records"].as_array().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["decoded"]["complete"], false);
        assert_eq!(records[0]["decoded"]["text"], serde_json::Value::Null);
        assert_eq!(records[0]["decoded"]["service_center_time"]["day"], 7);
        assert_eq!(records[1]["index"], 7);
        assert_eq!(records[1]["decoded"]["complete"], true);
        assert_eq!(records[1]["decoded"]["text"], "AB");
        assert_eq!(records[1]["decoded"]["service_center_time"]["day"], 6);

        let later_snapshot = cmgl_record(9, part_two);
        let later = stored_snapshot_value(&later_snapshot).await;
        let later_record = &later["records"][0]["decoded"];
        assert_eq!(later_record["complete"], false);
        assert_eq!(later_record["text"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn stored_snapshot_conflicting_fragment_duplicate_blocks_the_group() {
        let first = "0040038188F800006290600000000008050003C9020182";
        let conflicting_first = "0040038188F800006290600000000008050003C9020184";
        let second = "0040038188F800006290600000000008050003C9020286";
        let snapshot = format!(
            "{}{}{}",
            cmgl_record(7, first),
            cmgl_record(8, conflicting_first),
            cmgl_record(9, second)
        );
        let value = stored_snapshot_value(&snapshot).await;
        let records = value["records"].as_array().unwrap();
        assert_eq!(records.len(), 3);
        assert!(
            records
                .iter()
                .all(|record| record["decoded"]["complete"] == false)
        );
    }

    #[test]
    fn stored_snapshot_parser_rejects_unbounded_record_counts() {
        let mut snapshot = String::new();
        for index in 0..=STORED_SNAPSHOT_RECORD_LIMIT {
            snapshot.push_str(&format!("+CMGL: {index},0,,1\n00\n"));
        }
        let body = serde_json::to_vec(&json!({"response":snapshot})).unwrap();
        assert_eq!(
            decode_stored_snapshot(&body).status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    async fn prepares_text_as_numeric_tpdu_arrays() {
        let response = prepare(br#"{"recipient":"888","text":"BAL","reference":42}"#);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value,
            json!({"tpdus":[[33,0,3,129,136,248,0,0,3,194,32,19]]})
        );
    }

    #[tokio::test]
    async fn prepares_legacy_modem_tpdu_without_status_report_and_forces_ucs2() {
        let response = prepare(
            br#"{"recipient":"888","text":"BAL","reference":42,"encoding":"modem","request_status_report":false}"#,
        );
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value,
            json!({"tpdus":[[1,1,3,129,136,248,0,0,3,194,32,19]]})
        );

        let response = prepare(
            r#"{"recipient":"10086","text":"€","reference":42,"encoding":"modem","request_status_report":false}"#.as_bytes(),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["tpdus"][0][0], 1);
        assert_eq!(value["tpdus"][0][8], 8);
    }

    #[tokio::test]
    async fn incoming_envelope_decodes_without_transmission() {
        let response = decode(br#"{"rpdu":[1,42,4,145,68,119,246,0,3,1,2,3]}"#);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value,
            json!({"reference":42,"originator":"+44776","tpdu":[1,2,3]})
        );
        assert_eq!(
            decode(br#"{"rpdu":[1,42,0,0,3,1]}"#).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn deliver_tpdu_decodes_to_explicit_text_or_binary() {
        let response =
            decode_incoming(br#"{"tpdu":[0,3,129,136,248,0,8,98,144,96,0,0,0,0,2,0,65]}"#);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["sender"], "888");
        assert_eq!(value["kind"], "deliver");
        assert_eq!(value["text"], "A");
        assert_eq!(value["binary"], serde_json::Value::Null);
        assert_eq!(
            decode_incoming(br#"{"tpdu":[1,2,3]}"#).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn stored_deliver_tpdu_accepts_only_declared_fixed_slot_padding() {
        let mut tpdu = vec![0, 3, 129, 136, 248, 0, 8, 98, 144, 96, 0, 0, 0, 0, 2, 0, 65];
        let mut strict = serde_json::json!({ "tpdu": tpdu });
        let strict_body = serde_json::to_vec(&strict).unwrap();
        assert_eq!(decode_incoming(&strict_body).status(), StatusCode::OK);

        tpdu = vec![0, 3, 129, 136, 248, 0, 8, 98, 144, 96, 0, 0, 0, 0, 2, 0, 65];
        tpdu.extend_from_slice(&[0; 16]);
        strict["tpdu"] = serde_json::json!(tpdu);
        let strict_body = serde_json::to_vec(&strict).unwrap();
        assert_eq!(
            decode_incoming(&strict_body).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        strict["stored"] = serde_json::json!(true);
        let stored_body = serde_json::to_vec(&strict).unwrap();
        let response = decode_incoming(&stored_body);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["sender"], "888");
        assert_eq!(value["text"], "A");
    }

    #[tokio::test]
    async fn stored_deliver_tpdu_preserves_concatenation_metadata() {
        let tpdu = [
            0x40, 3, 0x81, 0x88, 0xf8, 0, 0, 0x62, 0x90, 0x60, 0, 0, 0, 0, 8, 5, 0, 3, 37, 3, 2,
            0x82,
        ];
        let request = serde_json::json!({ "tpdu": tpdu, "stored": true });
        let body = serde_json::to_vec(&request).unwrap();
        let response = decode_incoming(&body);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["sender"], "888");
        assert_eq!(value["text"], "A");
        assert_eq!(
            value["concatenation"],
            json!({
                "reference": 37,
                "wide_reference": false,
                "total": 3,
                "sequence": 2
            })
        );
    }

    #[tokio::test]
    async fn stored_reassembly_hides_partial_text_and_publishes_once_complete() {
        let make_part = |sequence, packed_text| {
            serde_json::json!({
                "tpdu": [
                    0x40, 3, 0x81, 0x88, 0xf8, 0, 0, 0x62, 0x90, 0x60, 0, 0, 0, 0,
                    8, 5, 0, 3, 201, 2, sequence, packed_text
                ],
                "stored": true,
                "reassemble": true
            })
        };
        let first = serde_json::to_vec(&make_part(1, 0x82)).unwrap();
        let response = decode_incoming(&first);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["complete"], false);
        assert_eq!(value["text"], serde_json::Value::Null);

        let second = serde_json::to_vec(&make_part(2, 0x84)).unwrap();
        let response = decode_incoming(&second);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["complete"], true);
        assert_eq!(value["text"], "AB");
        assert_eq!(value["concatenation"]["reference"], 201);
    }

    #[tokio::test]
    async fn binary_deliver_returns_rust_classification_and_raw_bytes() {
        let request = serde_json::json!({
            "tpdu": [0,3,129,136,248,0,4,98,144,96,0,0,0,0,4,222,173,190,239],
            "stored": true,
            "reassemble": true
        });
        let response = decode_incoming(&serde_json::to_vec(&request).unwrap());
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["is_binary"], true);
        assert_eq!(value["text"], serde_json::Value::Null);
        assert_eq!(value["binary"], json!([222, 173, 190, 239]));
        assert_eq!(value["binary_summary"], "[二进制数据]\nraw=deadbeef");
    }

    #[tokio::test]
    async fn stored_binary_parts_are_hidden_until_rust_reassembly_finishes() {
        let make_part = |sequence: u8, payload: u8| {
            serde_json::json!({
                "tpdu": [
                    0x40,3,129,136,248,0,4,98,144,96,0,0,0,0,7,
                    5,0,3,211,2,sequence,payload
                ],
                "stored": true,
                "reassemble": true
            })
        };
        let first = decode_incoming(&serde_json::to_vec(&make_part(1, 0xaa)).unwrap());
        let body = first.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["complete"], false);
        assert_eq!(value["is_binary"], true);
        assert_eq!(value["binary"], serde_json::Value::Null);
        assert_eq!(value["binary_summary"], serde_json::Value::Null);

        let second = decode_incoming(&serde_json::to_vec(&make_part(2, 0xbb)).unwrap());
        let body = second.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["complete"], true);
        assert_eq!(value["is_binary"], true);
        assert!(
            value["binary_summary"]
                .as_str()
                .unwrap()
                .contains("raw=aa[二进制数据]")
        );
        assert!(
            value["binary_summary"]
                .as_str()
                .unwrap()
                .ends_with("raw=bb")
        );
    }

    #[tokio::test]
    async fn status_report_is_native_and_explicit() {
        let response = decode_incoming(
            br#"{"tpdu":[2,1,4,145,68,119,98,128,98,97,33,0,0,98,128,98,97,33,0,0,0]}"#,
        );
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["kind"], "status_report");
        assert_eq!(value["reference"], 1);
        assert_eq!(value["recipient"], "+4477");
        assert_eq!(value["delivery_status"], "delivered");
        assert_eq!(
            value["service_center_time"],
            json!({
                "year":2026,"month":8,"day":26,
                "hour":16,"minute":12,"second":0,"offset_minutes":0
            })
        );
    }

    #[tokio::test]
    async fn octet_array_preserves_wire_bytes() {
        let response = encode(br#"{"reference":42,"smsc":"888","tpdu":[1,42]}"#);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value, json!({"rpdu":[0,42,0,3,129,136,248,2,1,42]}));
    }

    #[test]
    fn invalid_wire_types_and_protocol_inputs_are_rejected() {
        for body in [
            r#"{"reference":256,"smsc":"888","tpdu":[1]}"#,
            r#"{"reference":1,"smsc":"888","tpdu":"AQ=="}"#,
            r#"{"reference":1,"smsc":"888","tpdu":[-1]}"#,
        ] {
            assert_eq!(encode(body.as_bytes()).status(), StatusCode::BAD_REQUEST);
        }
        for body in [
            r#"{"reference":1,"smsc":"invalid","tpdu":[1]}"#,
            r#"{"reference":1,"smsc":"888","tpdu":[]}"#,
        ] {
            assert_eq!(
                encode(body.as_bytes()).status(),
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
    }
}
