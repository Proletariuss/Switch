//! Pure transitions used exclusively inside the sealed adapter transaction.
use super::*;
use serde_json::json;

pub(super) fn valid_execution(execution: Option<&Execution>, terminal: bool) -> bool {
    let Some(e) = execution else {
        return !terminal;
    };
    if !super::super::valid_token(&e.binding)
        || (!e.subscription_id.is_empty() && !super::super::valid_token(&e.subscription_id))
    {
        return false;
    }
    match e.state.as_str() {
        "prepared" => !terminal && e.attempt.is_empty() && e.segments == 0 && !e.archive_confirmed,
        "submitting" => {
            !terminal
                && super::super::valid_token(&e.attempt)
                && e.segments == 0
                && !e.archive_confirmed
        }
        "unknown" => {
            !terminal
                && super::super::valid_token(&e.attempt)
                && e.segments <= 1024
                && !e.archive_confirmed
        }
        "accepted" => {
            super::super::valid_token(&e.attempt)
                && (1..=1024).contains(&e.segments)
                && (!terminal || e.archive_confirmed)
        }
        _ => false,
    }
}
pub(super) fn valid_terminal(record: &TerminalRecord) -> bool {
    match record.outcome {
        TerminalOutcome::NotDispatched => record.execution.is_none(),
        TerminalOutcome::AcceptedArchived => valid_execution(record.execution.as_ref(), true),
    }
}
pub(super) fn valid_overlay(fence: &LegacyFence, original: Option<&LegacyFence>) -> bool {
    let Some(original) = original else {
        return false;
    };
    fence.client_request_id == original.client_request_id
        && fence.original_attempt == original.original_attempt
        && fence.segments == original.segments
        && (fence.original_state == original.original_state
            || (original.original_state == LegacyPhase::Submitting
                && fence.original_state == LegacyPhase::Unknown))
        && (fence.archive_confirmed == original.archive_confirmed
            || (original.original_state == LegacyPhase::Accepted && fence.archive_confirmed))
}
fn identity(snapshot: &AuthoritySnapshot, sequence: Decimal) -> DeviceSmsIdentity {
    DeviceSmsIdentity {
        authority: snapshot.authority_id.clone(),
        epoch: snapshot.epoch,
        sequence,
    }
}
fn lane(device: DeviceLane) -> &'static str {
    match device {
        DeviceLane::Reader => "reader",
        DeviceLane::Dji => "dji",
    }
}
fn project(
    snapshot: &AuthoritySnapshot,
    sequence: Decimal,
    nonce: &IntentNonce,
    device: DeviceLane,
    payload: &Payload,
    execution: Option<&Execution>,
    created_at_ms: u64,
    cancelled: bool,
    can_submit: bool,
) -> Value {
    let mut value = json!({
        "identity_version":2,"client_request_id":identity(snapshot,sequence).wire(),
        "phone":payload.phone,"message":payload.message,"device":lane(device),
        "subscription_id":execution.map(|e|e.subscription_id.as_str()).unwrap_or(&payload.expected_subscription_id),
        "expected_subscription_id":payload.expected_subscription_id,
        "state":execution.map(|e|e.state.as_str()).unwrap_or(if cancelled {"cancelled"} else {"reserved"}),
        "binding":execution.map(|e|e.binding.as_str()).unwrap_or(""),
        "attempt":execution.map(|e|e.attempt.as_str()).unwrap_or(""),
        "segments":execution.map(|e|e.segments).unwrap_or(0),
        "archive_confirmed":execution.is_some_and(|e|e.archive_confirmed),
        "created_at_ms":created_at_ms,
        "updated_at_ms":execution.map(|e|e.updated_at_ms).unwrap_or(created_at_ms),
        "can_submit":can_submit
    });
    value["intent_nonce"] = json!(nonce.0);
    value
}
fn current_record(
    snapshot: &AuthoritySnapshot,
    sequence: Decimal,
    can_submit: bool,
) -> Result<Value> {
    if let Some(slot) = snapshot.live_slots.iter().find(|s| s.sequence == sequence) {
        return Ok(project(
            snapshot,
            sequence,
            &slot.nonce,
            slot.device,
            &slot.payload,
            slot.execution.as_ref(),
            slot.created_at_ms,
            false,
            can_submit,
        ));
    }
    if let Some(cached) = snapshot
        .terminal_cache
        .iter()
        .find(|s| s.sequence == sequence)
    {
        return Ok(project(
            snapshot,
            sequence,
            &cached.nonce,
            cached.device,
            &cached.payload,
            cached.execution.as_ref(),
            cached.created_at_ms,
            matches!(cached.outcome, TerminalOutcome::NotDispatched),
            false,
        ));
    }
    Err(if sequence <= snapshot.issued_high_water {
        CodecError::Retired
    } else {
        CodecError::NotIssued
    })
}
fn checked_sequence(snapshot: &AuthoritySnapshot, id: &str) -> Result<Decimal> {
    let identity = DeviceSmsIdentity::parse_wire(id)?;
    if identity.authority != snapshot.authority_id || identity.epoch != snapshot.epoch {
        return Err(CodecError::AuthorityMismatch);
    }
    if identity.sequence > snapshot.issued_high_water {
        return Err(CodecError::NotIssued);
    }
    Ok(identity.sequence)
}
fn legacy_records(
    snapshot: &AuthoritySnapshot,
    frozen: &[u8],
) -> Result<BTreeMap<String, super::super::Record>> {
    let index = CompleteLegacyIndex::from_snapshot(frozen, &snapshot.legacy.sha256.0)?;
    if snapshot.legacy_overlay.len() != index.fences.len()
        || snapshot
            .legacy_overlay
            .iter()
            .any(|f| !valid_overlay(f, index.fences.get(&f.client_request_id)))
    {
        return Err(CodecError::LegacyUnavailable);
    }
    let value = strict_value(frozen, SNAPSHOT_BYTES)?;
    let mut records =
        super::super::read(value.as_array().ok_or(CodecError::LegacyUnavailable)?, true)
            .map_err(|_| CodecError::LegacyUnavailable)?;
    for fence in &snapshot.legacy_overlay {
        let record = records
            .get_mut(&fence.client_request_id)
            .ok_or(CodecError::LegacyUnavailable)?;
        record.state = match fence.original_state {
            LegacyPhase::Prepared => "prepared",
            LegacyPhase::Submitting => "submitting",
            LegacyPhase::Unknown => "unknown",
            LegacyPhase::Accepted => "accepted",
        }
        .into();
        record.archive_confirmed = fence.archive_confirmed;
    }
    Ok(records)
}
fn lookup(
    snapshot: &AuthoritySnapshot,
    legacy: &BTreeMap<String, super::super::Record>,
    id: &str,
) -> Result<Value> {
    if let Some(record) = legacy.get(id) {
        return Ok(super::super::record_value(record, false));
    }
    let sequence = checked_sequence(snapshot, id).map_err(|e| {
        if e == CodecError::InvalidIdentity {
            CodecError::NotFound
        } else {
            e
        }
    })?;
    current_record(snapshot, sequence, false)
}
fn field<'a>(body: &'a Value, key: &str) -> Result<&'a str> {
    body[key].as_str().ok_or(CodecError::InvalidReserve)
}
fn token_field(body: &Value, key: &str) -> Result<String> {
    let value = field(body, key)?;
    if !super::super::valid_token(value) {
        return Err(CodecError::InvalidReserve);
    }
    Ok(value.into())
}
fn matches_payload(body: &Value, payload: &Payload, device: DeviceLane) -> bool {
    body["phone"] == payload.phone
        && body["message"] == payload.message
        && body["device"] == lane(device)
        && body["expected_subscription_id"].as_str().unwrap_or("")
            == payload.expected_subscription_id
}
fn safe_cache(snapshot: &mut AuthoritySnapshot, quotas: Quotas) -> Result<()> {
    let mut i = 0;
    while i < snapshot.live_slots.len() {
        let safe = snapshot.live_slots[i]
            .execution
            .as_ref()
            .is_some_and(|e| e.state == "accepted" && e.archive_confirmed);
        if !safe {
            i += 1;
            continue;
        }
        let slot = snapshot.live_slots.remove(i);
        snapshot.terminal_cache.push(TerminalRecord {
            sequence: slot.sequence,
            nonce: slot.nonce,
            purpose: slot.purpose,
            device: slot.device,
            payload_fingerprint: slot.payload_fingerprint,
            payload: slot.payload,
            outcome: TerminalOutcome::AcceptedArchived,
            execution: slot.execution,
            created_at_ms: slot.created_at_ms,
        });
    }
    snapshot
        .terminal_cache
        .sort_by_key(|record| record.sequence);
    loop {
        match encode_with_quotas(snapshot, quotas) {
            Ok(_) => return Ok(()),
            Err(CodecError::Capacity) if !snapshot.terminal_cache.is_empty() => {
                snapshot.terminal_cache.remove(0);
            }
            Err(e) => return Err(e),
        }
    }
}
fn check_archive_item(record: &Value, item: &Value, attempt: Option<&str>) -> Result<()> {
    let stamp = item["timestamp"]
        .as_str()
        .and_then(|v| {
            time::OffsetDateTime::parse(v, &time::format_description::well_known::Rfc3339).ok()
        })
        .map(|v| v.unix_timestamp_nanos());
    if record["state"] != "accepted"
        || attempt.is_some_and(|a| record["attempt"].as_str() != Some(a))
        || item["direction"] != "out"
        || item["sender"] != record["phone"]
        || item["content"] != record["message"]
        || item["device"] != record["device"]
        || item["subscription_id"].as_str().unwrap_or("")
            != record["subscription_id"].as_str().unwrap_or("")
        || stamp
            != record["created_at_ms"]
                .as_u64()
                .map(|n| i128::from(n) * 1_000_000)
    {
        return Err(CodecError::Conflict);
    }
    Ok(())
}
pub(in crate::device_send_store) fn apply(
    current: &[u8],
    frozen: &[u8],
    action: &str,
    body: &Value,
) -> Result<(Option<Vec<u8>>, Value)> {
    apply_with_quotas(current, frozen, action, body, Quotas::PRODUCTION)
}
fn apply_with_quotas(
    current: &[u8],
    frozen: &[u8],
    action: &str,
    body: &Value,
    quotas: Quotas,
) -> Result<(Option<Vec<u8>>, Value)> {
    let mut snapshot = decode_snapshot_bytes(current)?;
    let legacy = legacy_records(&snapshot, frozen)?;
    if action == "authority" {
        let candidate = next_identity(&snapshot).ok();
        return Ok((
            None,
            json!({"identity_version":2,"family":"device_sms",
            "authority_id":snapshot.authority_id.0,"epoch":snapshot.epoch.0.to_string(),
            "issued_high_water":snapshot.issued_high_water.0.to_string(),
            "next_sequence":candidate.as_ref().map(|id|id.sequence.0.to_string()),
            "next_client_request_id":candidate.map(|id|id.wire())}),
        ));
    }
    if action == "reserve" || action == "cancel" {
        let request = decode_reserve_bytes(
            &serde_json::to_vec(body).map_err(|_| CodecError::InvalidReserve)?,
        )?;
        let index = CompleteLegacyIndex::from_snapshot(frozen, &snapshot.legacy.sha256.0)?;
        if action == "reserve" {
            let planned = plan_with_quotas(&snapshot, &request, &index, quotas)?;
            let created = planned.disposition == ReserveDisposition::Created;
            snapshot = planned.next;
            if created {
                snapshot
                    .live_slots
                    .iter_mut()
                    .find(|s| s.sequence == request.identity.sequence)
                    .ok_or(CodecError::InvalidSnapshot)?
                    .created_at_ms = super::super::now();
            }
        } else {
            snapshot = retire_with_quotas(&snapshot, &request, &index, quotas)?.next;
        }
        let result = current_record(&snapshot, request.identity.sequence, false)?;
        return Ok((Some(encode_with_quotas(&snapshot, quotas)?), result));
    }
    if action == "list" {
        let cursor = body["cursor"].as_str().unwrap_or("");
        if cursor.len() > 128 {
            return Err(CodecError::InvalidReserve);
        }
        let mut records = BTreeMap::new();
        for (id, record) in &legacy {
            records.insert(id.clone(), super::super::record_value(record, false));
        }
        for sequence in snapshot
            .live_slots
            .iter()
            .map(|s| s.sequence)
            .chain(snapshot.terminal_cache.iter().map(|c| c.sequence))
        {
            let value = current_record(&snapshot, sequence, false)?;
            records.insert(identity(&snapshot, sequence).wire(), value);
        }
        let mut page: Vec<Value> = records
            .into_iter()
            .filter(|(id, _)| id.as_str() > cursor)
            .take(101)
            .map(|(_, v)| v)
            .collect();
        let more = page.len() > 100;
        page.truncate(100);
        let next = if more {
            page.last()
                .and_then(|v| v["client_request_id"].as_str())
                .map(str::to_owned)
        } else {
            None
        };
        return Ok((None, json!({"intents":page,"next_cursor":next})));
    }
    if action == "recover" || action == "recover_go" {
        for fence in &mut snapshot.legacy_overlay {
            if fence.original_state == LegacyPhase::Submitting
                && (action == "recover"
                    || legacy
                        .get(&fence.client_request_id)
                        .is_some_and(|r| crate::device_sms_send::go_recovery_binding(&r.binding)))
            {
                fence.original_state = LegacyPhase::Unknown;
            }
        }
        for slot in &mut snapshot.live_slots {
            if let Some(e) = &mut slot.execution {
                if e.state == "submitting"
                    && (action == "recover"
                        || crate::device_sms_send::go_recovery_binding(&e.binding))
                {
                    e.state = "unknown".into();
                    e.updated_at_ms = super::super::now();
                }
            }
        }
        return Ok((Some(encode_with_quotas(&snapshot, quotas)?), json!(true)));
    }
    let id = if action == "archive_receipt" {
        field(&body["item"], "send_request_id")?
    } else {
        field(body, "client_request_id")?
    };
    if !super::super::valid_token(id) {
        return Err(CodecError::InvalidReserve);
    }
    if action == "get" {
        return Ok((None, lookup(&snapshot, &legacy, id)?));
    }
    if action == "archive_receipt" {
        let record = lookup(&snapshot, &legacy, id)?;
        check_archive_item(&record, &body["item"], body["attempt"].as_str())?;
        let pending = record["archive_confirmed"] == false;
        if body["confirm"] != true || !pending {
            return Ok((None, json!(pending)));
        }
        if legacy.contains_key(id) {
            snapshot
                .legacy_overlay
                .iter_mut()
                .find(|f| f.client_request_id == id)
                .ok_or(CodecError::LegacyUnavailable)?
                .archive_confirmed = true;
        } else {
            let seq = checked_sequence(&snapshot, id)?;
            let execution = snapshot
                .live_slots
                .iter_mut()
                .find(|s| s.sequence == seq)
                .and_then(|s| s.execution.as_mut())
                .ok_or(CodecError::Conflict)?;
            execution.archive_confirmed = true;
            execution.updated_at_ms = super::super::now();
        }
        safe_cache(&mut snapshot, quotas)?;
        return Ok((Some(encode_with_quotas(&snapshot, quotas)?), json!(pending)));
    }
    if let Some(record) = legacy.get(id) {
        if action == "prepare"
            && body["phone"] == record.phone
            && body["message"] == record.message
            && body["device"] == record.device
        {
            return Ok((None, super::super::record_value(record, false)));
        }
        return Err(CodecError::Conflict);
    }
    if body["identity_version"] != 2 && matches!(action, "prepare" | "claim") {
        return Err(CodecError::UpgradeRequired);
    }
    let sequence = checked_sequence(&snapshot, id).map_err(|e| {
        if e == CodecError::InvalidIdentity {
            CodecError::UpgradeRequired
        } else {
            e
        }
    })?;
    // Even an extant terminal cache entry never re-enters a live state.
    let before = current_record(&snapshot, sequence, false)?;
    let Some(index) = snapshot
        .live_slots
        .iter()
        .position(|s| s.sequence == sequence)
    else {
        if action == "claim" && body["intent_nonce"] == before["intent_nonce"] {
            return Ok((None, before));
        }
        if action == "finish"
            && body["attempt"] == before["attempt"]
            && body["state"] == before["state"]
            && body["segments"] == before["segments"]
            && body["archive_confirmed"] == before["archive_confirmed"]
        {
            return Ok((None, before));
        }
        return Err(CodecError::Conflict);
    };
    let slot = &mut snapshot.live_slots[index];
    if matches!(action, "prepare" | "claim")
        && body["intent_nonce"].as_str() != Some(slot.nonce.0.as_str())
    {
        return Err(CodecError::Conflict);
    }
    let mut can_submit = false;
    match action {
        "prepare" => {
            if !matches_payload(body, &slot.payload, slot.device) {
                return Err(CodecError::Conflict);
            }
            let binding = token_field(body, "binding")?;
            let subscription = body["subscription_id"].as_str().unwrap_or("").to_owned();
            if !subscription.is_empty() && !super::super::valid_token(&subscription) {
                return Err(CodecError::InvalidReserve);
            }
            if !slot.payload.expected_subscription_id.is_empty()
                && slot.payload.expected_subscription_id != subscription
            {
                return Err(CodecError::Conflict);
            }
            if slot.execution.is_none() {
                slot.execution = Some(Execution {
                    binding,
                    subscription_id: subscription,
                    state: "prepared".into(),
                    attempt: String::new(),
                    segments: 0,
                    archive_confirmed: false,
                    updated_at_ms: super::super::now(),
                });
            }
        }
        "claim" => {
            let binding = token_field(body, "binding")?;
            let attempt = token_field(body, "attempt")?;
            let e = slot.execution.as_mut().ok_or(CodecError::Conflict)?;
            if e.binding != binding {
                return Err(CodecError::Conflict);
            }
            if e.state == "prepared" {
                e.state = "submitting".into();
                e.attempt = attempt;
                e.updated_at_ms = super::super::now();
                can_submit = true;
            }
        }
        "finish" => {
            let attempt = token_field(body, "attempt")?;
            let state = field(body, "state")?;
            let segments = body["segments"]
                .as_u64()
                .filter(|n| *n <= 1024)
                .ok_or(CodecError::InvalidReserve)? as usize;
            let archived = body["archive_confirmed"]
                .as_bool()
                .ok_or(CodecError::InvalidReserve)?;
            if !matches!(state, "accepted" | "unknown")
                || (state == "accepted" && segments == 0)
                || (archived && state != "accepted")
            {
                return Err(CodecError::InvalidReserve);
            }
            let e = slot.execution.as_mut().ok_or(CodecError::Conflict)?;
            if e.attempt != attempt || e.attempt.is_empty() {
                return Err(CodecError::Conflict);
            }
            if e.state == "submitting" {
                e.state = state.into();
                e.segments = segments;
                e.archive_confirmed = archived;
                e.updated_at_ms = super::super::now();
            } else if e.state != state || e.segments != segments || e.archive_confirmed != archived
            {
                return Err(CodecError::Conflict);
            }
        }
        _ => return Err(CodecError::InvalidReserve),
    }
    let result = current_record(&snapshot, sequence, can_submit)?;
    safe_cache(&mut snapshot, quotas)?;
    Ok((Some(encode_with_quotas(&snapshot, quotas)?), result))
}

#[cfg(test)]
mod tests {
    use super::*;
    const AUTHORITY: &str = "0123456789abcdef0123456789abcdef";
    fn frozen() -> Vec<u8> {
        serde_json::to_vec(&json!([{"schema_version":1,"records":{}}])).unwrap()
    }
    fn initial(legacy: &[u8]) -> Vec<u8> {
        let index = CompleteLegacyIndex::from_snapshot(legacy, &digest(legacy).0).unwrap();
        serde_json::to_vec(&json!([{
            "schema_version":2,"family":"device_sms","authority_id":AUTHORITY,"epoch":"1",
            "issued_high_water":"0","legacy_new_admission":false,
            "legacy":{"path":"device-send-ledger.legacy-v1.json","sha256":digest(legacy).0},
            "live_slots":[],"terminal_cache":[],"legacy_overlay":index.fences.values().collect::<Vec<_>>()
        }])).unwrap()
    }
    fn reserve(sequence: u64) -> Value {
        json!({"identity_version":2,"family":"device_sms","authority_id":AUTHORITY,
            "epoch":"1","sequence":sequence.to_string(),
            "client_request_id":format!("m2:ds:{AUTHORITY}:0000000000000001:{sequence:016x}"),
            "intent_nonce":format!("{sequence:032x}"),"purpose":"device_sms_send","device":"reader",
            "payload":{"phone":"+12025550100","message":"fixture","expected_subscription_id":""}})
    }
    fn transition(
        state: &mut Vec<u8>,
        legacy: &[u8],
        action: &str,
        body: Value,
        quota: Quotas,
    ) -> Result<Value> {
        let (next, value) = apply_with_quotas(state, legacy, action, &body, quota)?;
        if let Some(next) = next {
            *state = next;
        }
        Ok(value)
    }
    fn prepare_body(reserved: &Value) -> Value {
        json!({"identity_version":2,"client_request_id":reserved["client_request_id"],
            "intent_nonce":reserved["intent_nonce"],"phone":"+12025550100","message":"fixture",
            "device":"reader","expected_subscription_id":"","subscription_id":"sub-A","binding":"route-A"})
    }
    fn claim_body(reserved: &Value) -> Value {
        json!({"identity_version":2,"client_request_id":reserved["client_request_id"],
            "intent_nonce":reserved["intent_nonce"],"binding":"route-A","attempt":"only-attempt"})
    }
    fn finish_body(reserved: &Value, archived: bool) -> Value {
        json!({"client_request_id":reserved["client_request_id"],"attempt":"only-attempt",
            "state":"accepted","segments":1,"archive_confirmed":archived})
    }
    fn complete(state: &mut Vec<u8>, legacy: &[u8], seq: u64, archived: bool, q: Quotas) -> Value {
        let request = reserve(seq);
        let reserved = transition(state, legacy, "reserve", request, q).unwrap();
        transition(state, legacy, "prepare", prepare_body(&reserved), q).unwrap();
        assert_eq!(
            transition(state, legacy, "claim", claim_body(&reserved), q).unwrap()["can_submit"],
            true
        );
        assert_eq!(
            transition(state, legacy, "claim", claim_body(&reserved), q).unwrap()["can_submit"],
            false
        );
        transition(state, legacy, "finish", finish_body(&reserved, archived), q).unwrap()
    }
    #[test]
    fn tiny_quotas_keep_bounded_files_and_permanently_reject_evicted_old_ids() {
        let legacy = frozen();
        let mut state = initial(&legacy);
        let q = Quotas {
            live: 2,
            lane: 2,
            cache: 2,
            cache_bytes: 4096,
            ..Quotas::PRODUCTION
        };
        for seq in 1..=2000 {
            let record = complete(&mut state, &legacy, seq, true, q);
            assert_eq!(record["archive_confirmed"], true);
            let reopened = decode_snapshot_bytes(&state).unwrap();
            assert_eq!(reopened.issued_high_water.0, seq);
            assert!(reopened.live_slots.is_empty());
            assert!(reopened.terminal_cache.len() <= 2);
            assert!(
                state.len() < 6000,
                "active bytes grew with retired operations"
            );
        }
        for seq in 1..=1998 {
            let request = reserve(seq);
            let id = request["client_request_id"].clone();
            assert_eq!(
                transition(
                    &mut state,
                    &legacy,
                    "get",
                    json!({"client_request_id":id}),
                    q
                )
                .err(),
                Some(CodecError::Retired)
            );
            assert_eq!(
                transition(&mut state, &legacy, "reserve", request.clone(), q).err(),
                Some(CodecError::Retired)
            );
            assert_eq!(
                transition(&mut state, &legacy, "claim", claim_body(&request), q).err(),
                Some(CodecError::Retired)
            );
        }
    }
    #[test]
    fn unarchived_and_unknown_slots_never_retire_to_make_room() {
        let legacy = frozen();
        let mut state = initial(&legacy);
        let q = Quotas {
            live: 2,
            lane: 2,
            cache: 1,
            ..Quotas::PRODUCTION
        };
        complete(&mut state, &legacy, 1, false, q);
        let reserved = transition(&mut state, &legacy, "reserve", reserve(2), q).unwrap();
        transition(&mut state, &legacy, "prepare", prepare_body(&reserved), q).unwrap();
        transition(&mut state, &legacy, "claim", claim_body(&reserved), q).unwrap();
        transition(&mut state, &legacy, "recover", json!({}), q).unwrap();
        assert_eq!(
            transition(&mut state, &legacy, "reserve", reserve(3), q).err(),
            Some(CodecError::Capacity)
        );
        let unknown = transition(&mut state, &legacy, "claim", claim_body(&reserved), q).unwrap();
        assert_eq!(unknown["state"], "unknown");
        assert_eq!(unknown["can_submit"], false);
        assert_eq!(decode_snapshot_bytes(&state).unwrap().live_slots.len(), 2);
    }
    #[test]
    fn nonce_conflict_cannot_prepare_or_claim_and_cancel_burns_identity() {
        let legacy = frozen();
        let mut state = initial(&legacy);
        let q = Quotas::PRODUCTION;
        let request = reserve(1);
        let original = transition(&mut state, &legacy, "reserve", request.clone(), q).unwrap();
        let mut wrong = request.clone();
        wrong["intent_nonce"] = json!("ffffffffffffffffffffffffffffffff");
        assert_eq!(
            transition(&mut state, &legacy, "reserve", wrong.clone(), q).err(),
            Some(CodecError::Conflict)
        );
        let mut wrong_prepare = prepare_body(&original);
        wrong_prepare["intent_nonce"] = wrong["intent_nonce"].clone();
        assert_eq!(
            transition(&mut state, &legacy, "prepare", wrong_prepare, q).err(),
            Some(CodecError::Conflict)
        );
        assert_eq!(
            transition(&mut state, &legacy, "cancel", wrong, q).err(),
            Some(CodecError::Conflict)
        );
        let cancelled = transition(&mut state, &legacy, "cancel", request.clone(), q).unwrap();
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(cancelled["intent_nonce"], original["intent_nonce"]);
        assert_eq!(
            transition(&mut state, &legacy, "reserve", request, q).unwrap()["state"],
            "cancelled"
        );
        assert_eq!(
            transition(&mut state, &legacy, "claim", claim_body(&original), q).unwrap()["can_submit"],
            false
        );
    }
    #[test]
    fn full_legacy_history_stays_exact_and_archive_overlay_is_monotonic() {
        let mut records = serde_json::Map::new();
        for n in 0..10_000 {
            let id = format!("legacy-{n}");
            records.insert(
                id.clone(),
                json!({"client_request_id":id,"phone":"+12025550100","message":"legacy",
                "device":"reader","subscription_id":"","binding":"old-route","state":"accepted",
                "attempt":"old-attempt","segments":1,"archive_confirmed":false,
                "created_at_ms":1000,"updated_at_ms":1000}),
            );
        }
        let legacy = serde_json::to_vec(&json!([{"schema_version":1,"records":records}])).unwrap();
        let mut state = initial(&legacy);
        let original = legacy.clone();
        complete(&mut state, &legacy, 1, true, Quotas::PRODUCTION);
        let item = json!({"send_request_id":"legacy-9999","sender":"+12025550100","content":"legacy",
            "device":"reader","direction":"out","timestamp":"1970-01-01T00:00:01Z"});
        assert_eq!(
            transition(
                &mut state,
                &legacy,
                "archive_receipt",
                json!({"item":item,"attempt":"old-attempt","confirm":true}),
                Quotas::PRODUCTION
            )
            .unwrap(),
            true
        );
        let old = transition(
            &mut state,
            &legacy,
            "get",
            json!({"client_request_id":"legacy-9999"}),
            Quotas::PRODUCTION,
        )
        .unwrap();
        assert_eq!(old["identity_version"], 1);
        assert_eq!(old["archive_confirmed"], true);
        assert_eq!(old["attempt"], "old-attempt");
        assert_eq!(old["can_submit"], false);
        assert_eq!(legacy, original);
        assert_eq!(
            decode_snapshot_bytes(&state).unwrap().legacy_overlay.len(),
            10_000
        );
    }
}
