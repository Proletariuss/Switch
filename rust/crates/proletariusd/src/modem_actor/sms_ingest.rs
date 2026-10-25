//! Stored module SMS receive transaction shared by the compatibility endpoint
//! and the native actor scheduler. Serial work stays on the existing actor;
//! only a confirmed durable archive permits generation-fenced source cleanup.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IngestStoredSmsRequest {
    #[serde(default)]
    pub(super) storage: Option<String>,
    #[serde(default)]
    pub(super) index: Option<u32>,
    pub(super) expected_sms_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct StoredSmsCandidate {
    pub(super) index: u32,
    pub(super) pdu: String,
    pub(super) sender: String,
    pub(super) content: String,
    pub(super) timestamp: String,
    pub(super) multipart: bool,
}

pub(super) async fn ingest_stored_sms(body: &[u8], actor: ModemActor) -> Response<Full<Bytes>> {
    let store = match crate::message_archive::archive() {
        Ok(store) => store,
        Err(error) => return crate::message_archive::store_error(error),
    };
    ingest_stored_sms_with_store(body, actor, store).await
}

pub(super) async fn ingest_stored_sms_with_store(
    body: &[u8],
    actor: ModemActor,
    store: proletarius_store::ArchiveStore,
) -> Response<Full<Bytes>> {
    let input = match serde_json::from_slice::<IngestStoredSmsRequest>(body) {
        Ok(input) => input,
        Err(_) => return invalid_json(),
    };
    match ingest(input, actor, store, None).await {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(response) => response,
    }
}

/// Receive one observed message group, or scan the selected storage when the
/// index is absent. `fence` belongs to the native scheduler's original session;
/// `None` preserves the compatibility endpoint's generation-only behavior.
pub(super) async fn ingest(
    input: IngestStoredSmsRequest,
    actor: ModemActor,
    store: proletarius_store::ArchiveStore,
    fence: Option<ReceiveFence>,
) -> Result<Value, Response<Full<Bytes>>> {
    if input.index == Some(0) {
        return Err(invalid_json());
    }
    let read_actor = actor.clone();
    let storage = input.storage.clone();
    let generation = input.expected_sms_generation;
    let snapshot = match tokio::task::spawn_blocking(move || {
        read_actor.read_stored_sms_fenced(storage, None, generation, fence)
    })
    .await
    {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(error)) => return Err(actor_error_response(error)),
        Err(_) => {
            return Err(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "serial_actor_stopped",
                "serial actor task failed",
            ));
        }
    };
    let decoded =
        match crate::sms_codec::decode_stored_snapshot_response(&snapshot.lines.join("\r\n")) {
            Ok(decoded) => decoded,
            Err(status) => {
                let code = if status == StatusCode::PAYLOAD_TOO_LARGE {
                    "sms_snapshot_record_limit"
                } else {
                    "sms_snapshot_decode_failed"
                };
                return Err(error_response(
                    status,
                    code,
                    "stored SMS snapshot could not be decoded",
                ));
            }
        };
    let Some(records) = decoded.get("records").and_then(Value::as_array) else {
        return Err(error_response(
            StatusCode::BAD_GATEWAY,
            "sms_snapshot_decode_failed",
            "Rust stored SMS decoder returned an invalid snapshot",
        ));
    };
    let fallback_timestamp = match time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
    {
        Ok(value) => value,
        Err(_) => {
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "clock_failed",
                "system clock is unavailable",
            ));
        }
    };
    let candidates = stored_sms_candidates(records, input.index, &fallback_timestamp);
    if candidates.is_empty() {
        return Ok(json!({
            "accepted":true,
            "storage":snapshot.storage,
            "storage_restored":snapshot.storage_restored,
            "archived":0,
            "deleted":0
        }));
    }
    let generation_actor = actor.clone();
    match tokio::task::spawn_blocking(move || {
        generation_actor.confirm_sms_generation_fenced(generation, fence)
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(actor_error_response(error)),
        Err(_) => {
            return Err(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "serial_actor_stopped",
                "serial actor task failed",
            ));
        }
    }
    let messages = candidates
        .iter()
        .map(|candidate| {
            json!({
                "sender":candidate.sender,
                "content":candidate.content,
                "timestamp":candidate.timestamp,
                "direction":"in",
                "device":"dji"
            })
        })
        .collect::<Vec<_>>();
    let archived = match crate::message_archive::ingest_messages_to(store, messages).await {
        Ok(archived) => archived,
        Err(error) => return Err(crate::message_archive::store_error(error)),
    };

    let mut deleted = 0;
    if snapshot.storage == "ME" {
        for candidate in candidates.iter().filter(|candidate| !candidate.multipart) {
            let delete_actor = actor.clone();
            let index = candidate.index;
            let pdu = candidate.pdu.clone();
            match tokio::task::spawn_blocking(move || {
                delete_actor.delete_archived_sms_fenced(index, pdu, generation, fence)
            })
            .await
            {
                Ok(Ok(result)) if result.deleted => deleted += 1,
                Ok(Ok(result)) => {
                    crate::diagnostics::write_line(format_args!(
                        "switchd-rs: SMS archived; source deletion not confirmed for index {} ({})",
                        candidate.index, result.reason
                    ));
                }
                Ok(Err(error)) => {
                    crate::diagnostics::write_line(format_args!(
                        "switchd-rs: SMS archived; source deletion not confirmed for index {} ({})",
                        candidate.index, error.code
                    ));
                }
                Err(_) => {
                    // The archive is already durable. Keep the source whenever
                    // verification or deletion cannot be confirmed.
                    crate::diagnostics::write_line(format_args!(
                        "switchd-rs: SMS archived; serial actor did not confirm source deletion for index {}",
                        candidate.index
                    ));
                }
            }
        }
    }
    Ok(json!({
        "accepted":true,
        "storage":snapshot.storage,
        "storage_restored":snapshot.storage_restored,
        "archived":archived,
        "deleted":deleted
    }))
}

pub(super) fn stored_sms_candidates(
    records: &[Value],
    requested_index: Option<u32>,
    fallback_timestamp: &str,
) -> Vec<StoredSmsCandidate> {
    let target = requested_index.and_then(|requested| {
        records.iter().find(|record| {
            record.get("index").and_then(Value::as_u64) == Some(u64::from(requested))
        })
    });
    let target_decoded = target.and_then(|record| record.get("decoded"));
    let target_has_concat = target_decoded
        .and_then(|decoded| decoded.get("concatenation"))
        .is_some_and(|value| value.is_object());
    let target_concat = target_decoded.and_then(stored_sms_concat_key);

    records
        .iter()
        .filter_map(|record| {
            let index = u32::try_from(record.get("index")?.as_u64()?).ok()?;
            if index == 0 {
                return None;
            }
            let pdu = record.get("pdu")?.as_str()?.to_owned();
            let decoded = record.get("decoded")?;
            if decoded.get("kind")?.as_str()? != "deliver"
                || decoded.get("complete")?.as_bool()? != true
            {
                return None;
            }
            let concat_key = stored_sms_concat_key(decoded);
            match requested_index {
                Some(_) if target_has_concat => {
                    if concat_key.is_none() || concat_key != target_concat {
                        return None;
                    }
                }
                Some(requested) if index != requested => return None,
                _ => {}
            }
            let sender = decoded.get("sender")?.as_str()?.trim();
            let content = decoded
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| decoded.get("binary_summary").and_then(Value::as_str))?
                .trim();
            if sender.is_empty() || content.is_empty() {
                return None;
            }
            let timestamp = stored_sms_timestamp(decoded, fallback_timestamp)?;
            let multipart = decoded
                .get("concatenation")
                .is_some_and(|value| value.is_object());
            Some(StoredSmsCandidate {
                index,
                pdu,
                sender: sender.to_owned(),
                content: content.to_owned(),
                timestamp,
                multipart,
            })
        })
        .collect()
}

pub(super) fn stored_sms_concat_key(decoded: &Value) -> Option<(String, u16, bool, u8, u8)> {
    let concatenation = decoded.get("concatenation")?.as_object()?;
    Some((
        decoded.get("sender")?.as_str()?.to_owned(),
        u16::try_from(concatenation.get("reference")?.as_u64()?).ok()?,
        concatenation.get("wide_reference")?.as_bool()?,
        u8::try_from(concatenation.get("total")?.as_u64()?).ok()?,
        u8::try_from(decoded.get("dcs")?.as_u64()?).ok()?,
    ))
}

pub(super) fn stored_sms_timestamp(decoded: &Value, fallback_timestamp: &str) -> Option<String> {
    let Some(value) = decoded
        .get("service_center_time")
        .filter(|value| !value.is_null())
    else {
        return Some(fallback_timestamp.to_owned());
    };
    let year = i32::try_from(value.get("year")?.as_i64()?).ok()?;
    let month = u8::try_from(value.get("month")?.as_u64()?).ok()?;
    let day = u8::try_from(value.get("day")?.as_u64()?).ok()?;
    let hour = u8::try_from(value.get("hour")?.as_u64()?).ok()?;
    let minute = u8::try_from(value.get("minute")?.as_u64()?).ok()?;
    let second = u8::try_from(value.get("second")?.as_u64()?).ok()?;
    let offset_minutes = i16::try_from(value.get("offset_minutes")?.as_i64()?).ok()?;
    if !(1900..=2099).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
        || !(-840..=840).contains(&offset_minutes)
    {
        return None;
    }
    let date =
        time::Date::from_calendar_date(year, time::Month::try_from(month).ok()?, day).ok()?;
    let time = time::Time::from_hms(hour, minute, second).ok()?;
    let offset = time::UtcOffset::from_whole_seconds(i32::from(offset_minutes) * 60).ok()?;
    time::PrimitiveDateTime::new(date, time)
        .assume_offset(offset)
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}
