use super::*;
use crate::device_send_store::Store as V1Store;
use crate::sms_store_health::GuardedArchive;
use proletarius_store::{ArchiveStore, StoreError};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    },
};

const AUTHORITY: &str = "0123456789abcdef0123456789abcdef";
const NONCE: &str = "abcdef0123456789abcdef0123456789";
static ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/tmp/device-send-authority-codec");
        fs::create_dir_all(&parent).unwrap();
        let root = parent.join(format!(
            "{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn legacy_record(id: &str, state: &str, archived: bool) -> Value {
    json!({"client_request_id":id,"phone":"+12025550100","message":"old body",
        "device":"reader","subscription_id":"sub-A","binding":"original-route",
        "state":state,"attempt":if state=="prepared" {""} else {"original-attempt"},
        "segments":if state=="accepted" {1} else {0},"archive_confirmed":archived,
        "created_at_ms":1,"updated_at_ms":2})
}
fn legacy_bytes(records: &[(&str, &str, bool)]) -> Vec<u8> {
    let records: serde_json::Map<_, _> = records
        .iter()
        .map(|(id, state, archived)| (id.to_string(), legacy_record(id, state, *archived)))
        .collect();
    serde_json::to_vec(&json!([{"schema_version":1,"records":records}])).unwrap()
}
fn legacy_index(bytes: &[u8]) -> CompleteLegacyIndex {
    CompleteLegacyIndex::from_snapshot(bytes, &digest(bytes).0).unwrap()
}
fn empty_with_legacy(bytes: &[u8]) -> AuthoritySnapshot {
    decode_snapshot_bytes(
        &serde_json::to_vec(&json!([{
            "schema_version":2,"family":"device_sms","authority_id":AUTHORITY,"epoch":"1",
            "issued_high_water":"0","legacy_new_admission":false,
            "legacy":{"path":"device-send-ledger.legacy-v1.json","sha256":digest(bytes).0},
            "live_slots":[],"terminal_cache":[],"legacy_overlay":[]
        }]))
        .unwrap(),
    )
    .unwrap()
}
fn empty() -> (AuthoritySnapshot, CompleteLegacyIndex) {
    let bytes = legacy_bytes(&[]);
    (empty_with_legacy(&bytes), legacy_index(&bytes))
}
fn request_value(sequence: u64, nonce: &str, device: &str, message: &str) -> Value {
    let id = format!("m2:ds:{AUTHORITY}:0000000000000001:{sequence:016x}");
    json!({"identity_version":2,"family":"device_sms","authority_id":AUTHORITY,
        "epoch":"1","sequence":sequence.to_string(),"client_request_id":id,
        "intent_nonce":nonce,"purpose":"device_sms_send","device":device,
        "payload":{"phone":"+12025550100","message":message,"expected_subscription_id":"sub-A"}})
}
fn request(sequence: u64, nonce: &str, device: &str, message: &str) -> ValidatedReserve {
    decode_reserve_bytes(
        &serde_json::to_vec(&request_value(sequence, nonce, device, message)).unwrap(),
    )
    .unwrap()
}
fn reserved(count: u64, alternate: bool) -> (AuthoritySnapshot, CompleteLegacyIndex) {
    let (mut state, legacy) = empty();
    for sequence in 1..=count {
        let device = if alternate && sequence % 2 == 0 {
            "dji"
        } else {
            "reader"
        };
        let next =
            plan_reserve(&state, &request(sequence, NONCE, device, "body"), &legacy).unwrap();
        state = next.next;
    }
    (state, legacy)
}
fn cache(slot: &ReservedSlot) -> TerminalRecord {
    TerminalRecord {
        sequence: slot.sequence,
        nonce: slot.nonce.clone(),
        purpose: slot.purpose,
        device: slot.device,
        payload_fingerprint: slot.payload_fingerprint.clone(),
        payload: slot.payload.clone(),
        outcome: TerminalOutcome::NotDispatched,
        execution: None,
        created_at_ms: slot.created_at_ms,
    }
}
fn snapshot_value(state: &AuthoritySnapshot) -> Value {
    serde_json::from_slice(&encode_checked(state).unwrap()).unwrap()
}
fn reject_snapshot(value: Value) {
    assert!(decode_snapshot_bytes(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn canonical_identity_is_explicit_fixed_family_epoch_and_checked_u64() {
    let wire = format!("m2:ds:{AUTHORITY}:0000000000000001:ffffffffffffffff");
    assert_eq!(wire.len(), 72);
    assert_eq!(DeviceSmsIdentity::parse_wire(&wire).unwrap().wire(), wire);
    assert_eq!(
        DeviceSmsIdentity::parse_wire(&":".repeat(128 * 1024)).err(),
        Some(CodecError::InvalidIdentity)
    );
    for invalid in [
        wire.to_uppercase(),
        wire.replace("m2:ds:", "m2:reader:"),
        wire.replace(":0000000000000001:", ":0000000000000002:"),
        wire.replace("ffffffffffffffff", "0000000000000000"),
        wire.replace("ffffffffffffffff", "10000000000000000"),
        format!(" {wire}"),
        format!("{wire}:extra"),
    ] {
        assert_eq!(
            DeviceSmsIdentity::parse_wire(&invalid).err(),
            Some(CodecError::InvalidIdentity)
        );
    }
    let (mut state, _) = empty();
    assert_eq!(next_identity(&state).unwrap().sequence.0, 1);
    state.issued_high_water = Decimal(u64::MAX);
    let before = encode_checked(&state).unwrap();
    assert_eq!(
        next_identity(&state).err(),
        Some(CodecError::SequenceExhausted)
    );
    assert_eq!(encode_checked(&state).unwrap(), before);
}

#[test]
fn strict_reserve_rejects_duplicates_escaped_duplicates_unknown_null_and_numeric_aliases() {
    let valid =
        serde_json::to_string(&request_value(1, NONCE, "reader", " keep whitespace \n")).unwrap();
    let decoded = decode_reserve_bytes(valid.as_bytes()).unwrap();
    assert_eq!(decoded.payload.message, " keep whitespace \n");
    for malformed in [
        valid.replacen("{", "{\"identity_version\":2,", 1),
        valid.replacen("{", "{\"identity_versi\\u006fn\":2,", 1),
        valid.replacen("\"payload\":{", "\"payload\":{\"message\":\"earlier\",", 1),
        valid.replacen(
            "\"payload\":{",
            "\"payload\":{\"mess\\u0061ge\":\"earlier\",",
            1,
        ),
        format!("{valid} null"),
        "null".into(),
        "[]".into(),
    ] {
        assert!(
            decode_reserve_bytes(malformed.as_bytes()).is_err(),
            "accepted duplicate/invalid raw input"
        );
    }
    let cases = [
        ("identity_version", json!(3)),
        ("sequence", json!(1)),
        ("sequence", json!(1.0)),
        ("sequence", json!("01")),
        ("sequence", json!("+1")),
        ("sequence", json!(" 1")),
        ("sequence", json!("18446744073709551616")),
        ("sequence", json!("0")),
        ("epoch", json!("2")),
        ("epoch", Value::Null),
        ("intent_nonce", json!(NONCE.to_uppercase())),
        ("family", json!("reader")),
        ("device", json!("Reader")),
        ("purpose", json!("claim")),
        ("client_request_id", json!("arbitrary-v1-id")),
        ("can_submit", json!(true)),
    ];
    for (field, replacement) in cases {
        let mut value = request_value(1, NONCE, "reader", "body");
        value[field] = replacement;
        assert!(decode_reserve_bytes(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    for field in ["phone", "message", "expected_subscription_id", "Phone"] {
        let mut value = request_value(1, NONCE, "reader", "body");
        value["payload"][field] = Value::Null;
        assert!(decode_reserve_bytes(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut value = request_value(1, NONCE, "reader", "body");
    value["payload"]["phone"] = json!(" +12025550100 ");
    assert!(decode_reserve_bytes(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn strict_snapshot_rejects_raw_duplicates_and_future_phases_without_empty_fallback() {
    let (state, _) = reserved(1, false);
    let bytes = encode_checked(&state).unwrap();
    let raw = String::from_utf8(bytes.clone()).unwrap();
    for invalid in [
        raw.replacen("{", "{\"schema_version\":2,", 1),
        raw.replacen("\"payload\":{", "\"payload\":{\"message\":\"first\",", 1),
        raw.replacen(
            "\"payload\":{",
            "\"payload\":{\"mess\\u0061ge\":\"first\",",
            1,
        ),
        "[]".into(),
        "null".into(),
        "{}".into(),
        format!("{raw} []"),
    ] {
        assert!(decode_snapshot_bytes(invalid.as_bytes()).is_err());
    }
    for (field, replacement) in [
        ("schema_version", json!(1)),
        ("family", json!("reader")),
        ("epoch", json!("2")),
        ("issued_high_water", json!(1)),
        ("issued_high_water", json!("01")),
        ("legacy_new_admission", json!(true)),
        ("extra", json!(false)),
    ] {
        let mut value = snapshot_value(&state);
        value[0][field] = replacement;
        reject_snapshot(value);
    }
    for (field, replacement) in [
        ("phase", json!("unknown")),
        ("phase", json!("accepted")),
        ("phase", json!("claimed")),
        ("phase", json!("submitting")),
        ("binding", json!("new-physical-route")),
        ("attempt", json!("claim-token")),
        ("archive_confirmed", json!(true)),
        ("dispatch_count", json!(1)),
        ("sequence", json!("0")),
        ("sequence", json!("2")),
        ("sequence", json!(1)),
        ("payload_fingerprint", json!("0".repeat(64))),
        ("can_submit", json!(true)),
    ] {
        let mut value = snapshot_value(&state);
        value[0]["live_slots"][0][field] = replacement;
        reject_snapshot(value);
    }
    for field in [
        "sequence",
        "nonce",
        "purpose",
        "device",
        "payload_fingerprint",
        "phase",
        "payload",
        "binding",
        "attempt",
        "dispatch_count",
        "archive_confirmed",
    ] {
        let mut value = snapshot_value(&state);
        value[0]["live_slots"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        reject_snapshot(value);
    }
    let mut value = snapshot_value(&state);
    let duplicate = value[0]["live_slots"][0].clone();
    value[0]["live_slots"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    reject_snapshot(value);
    let mut value = snapshot_value(&state);
    value[0]["legacy"]["path"] = json!("../live-ledger.json");
    reject_snapshot(value);
    assert_eq!(
        encode_checked(&decode_snapshot_bytes(&bytes).unwrap()).unwrap(),
        bytes
    );
}

#[test]
fn raw_json_tiny_values_have_a_bounded_parse_budget_before_typed_decode() {
    let oversized = format!("[{}]", "0,".repeat(MAX_JSON_VALUES) + "0");
    assert!(oversized.len() < RESERVE_BYTES);
    assert_eq!(
        strict_value(oversized.as_bytes(), RESERVE_BYTES).err(),
        Some(CodecError::InvalidJson)
    );
    let nested = format!("{}0{}", "[".repeat(200), "]".repeat(200));
    assert!(strict_value(nested.as_bytes(), RESERVE_BYTES).is_err());
}

#[test]
fn pure_reserve_is_same_snapshot_idempotent_and_never_a_dispatch_grant() {
    let (state, legacy) = empty();
    let before = encode_checked(&state).unwrap();
    let req = request(1, NONCE, "reader", " body \n");
    let plan = plan_reserve(&state, &req, &legacy).unwrap();
    assert_eq!(plan.disposition(), ReserveDisposition::Created);
    assert_eq!(state.issued_high_water.0, 0);
    assert_eq!(encode_checked(&state).unwrap(), before);
    let bytes = plan.encoded().unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let slot = &value[0]["live_slots"][0];
    assert_eq!(slot["phase"], "reserved_unbound");
    assert_eq!(slot["dispatch_count"], 0);
    assert!(slot["binding"].is_null() && slot["attempt"].is_null());
    assert_eq!(slot["archive_confirmed"], false);
    assert!(slot.get("can_submit").is_none());
    let current = decode_snapshot_bytes(&bytes).unwrap();
    let replay = plan_reserve(&current, &req, &legacy).unwrap();
    assert_eq!(replay.disposition(), ReserveDisposition::Existing);
    assert_eq!(replay.encoded().unwrap(), bytes);
}

#[test]
fn conflict_future_foreign_and_retired_reserve_never_advance_high_water() {
    let (state, legacy) = reserved(1, false);
    let before = encode_checked(&state).unwrap();
    for req in [
        request(1, "11111111111111111111111111111111", "reader", "body"),
        request(1, NONCE, "reader", "different"),
        request(1, NONCE, "dji", "body"),
    ] {
        assert_eq!(
            plan_reserve(&state, &req, &legacy).err(),
            Some(CodecError::Conflict)
        );
    }
    assert_eq!(
        plan_reserve(&state, &request(3, NONCE, "reader", "body"), &legacy).err(),
        Some(CodecError::NotIssued)
    );
    let mut foreign = request_value(2, NONCE, "reader", "body");
    foreign["authority_id"] = json!("1".repeat(32));
    foreign["client_request_id"] = json!(format!(
        "m2:ds:{}:0000000000000001:0000000000000002",
        "1".repeat(32)
    ));
    let foreign = decode_reserve_bytes(&serde_json::to_vec(&foreign).unwrap()).unwrap();
    assert_eq!(
        plan_reserve(&state, &foreign, &legacy).err(),
        Some(CodecError::AuthorityMismatch)
    );
    assert_eq!(encode_checked(&state).unwrap(), before);
    let mut retired = state.clone();
    retired.live_slots.clear();
    assert_eq!(
        plan_reserve(&retired, &request(1, NONCE, "reader", "tampered"), &legacy).err(),
        Some(CodecError::Retired)
    );
    assert_eq!(retired.issued_high_water.0, 1);
    retired.issued_high_water = Decimal(u64::MAX);
    assert_eq!(
        plan_reserve(
            &retired,
            &request(u64::MAX, NONCE, "reader", "body"),
            &legacy
        )
        .err(),
        Some(CodecError::Retired)
    );
}

#[test]
fn complete_legacy_index_requires_whole_validated_raw_snapshot_and_exact_digest() {
    let oversized = vec![b' '; SNAPSHOT_BYTES + 1];
    for wrong_digest in ["not-a-digest".to_owned(), "0".repeat(64)] {
        assert_eq!(
            CompleteLegacyIndex::from_snapshot(&oversized, &wrong_digest).err(),
            Some(CodecError::Capacity)
        );
    }
    drop(oversized);
    let bytes = legacy_bytes(&[
        ("prepared", "prepared", false),
        ("submitting", "submitting", false),
        ("unknown", "unknown", false),
        ("accepted-pending-archive", "accepted", false),
        ("accepted", "accepted", true),
    ]);
    let index = legacy_index(&bytes);
    assert_eq!(index.fences.len(), 5);
    assert_eq!(
        CompleteLegacyIndex::from_snapshot(&bytes, &"0".repeat(64)).err(),
        Some(CodecError::LegacyUnavailable)
    );
    for invalid in [
        b"[]".to_vec(),
        b"null".to_vec(),
        b"{\"intents\":[],\"next_cursor\":null}".to_vec(),
        String::from_utf8(bytes.clone())
            .unwrap()
            .replacen("{", "{\"schema_version\":1,", 1)
            .into_bytes(),
    ] {
        assert!(CompleteLegacyIndex::from_snapshot(&invalid, &digest(&invalid).0).is_err());
    }
    let mut invalid: Value = serde_json::from_slice(&bytes).unwrap();
    invalid[0]["records"]["unknown"]["state"] = json!("prepared");
    let invalid = serde_json::to_vec(&invalid).unwrap();
    assert!(CompleteLegacyIndex::from_snapshot(&invalid, &digest(&invalid).0).is_err());
    let mut state = empty_with_legacy(&bytes);
    state.legacy_overlay = index.fences.values().cloned().collect();
    assert!(plan_reserve(&state, &request(1, NONCE, "reader", "body"), &index).is_ok());
    state.legacy_overlay[0].original_attempt = "different-attempt".into();
    assert!(plan_reserve(&state, &request(1, NONCE, "reader", "body"), &index).is_err());
    let (other, other_index) = empty();
    assert_eq!(
        plan_reserve(&other, &request(1, NONCE, "reader", "body"), &index).err(),
        Some(CodecError::LegacyUnavailable)
    );
    assert_eq!(
        plan_reserve(&state, &request(1, NONCE, "reader", "body"), &other_index).err(),
        Some(CodecError::LegacyUnavailable)
    );
}

#[test]
fn full_v1_record_limit_builds_complete_index_and_over_limit_is_never_a_partial_proof() {
    let ids = (0..crate::device_send_store::MAX_RECORDS)
        .map(|n| format!("old-{n}"))
        .collect::<Vec<_>>();
    let mut records = ids
        .iter()
        .map(|id| (id.as_str(), "unknown", false))
        .collect::<Vec<_>>();
    let bytes = legacy_bytes(&records);
    let index = legacy_index(&bytes);
    assert_eq!(index.fences.len(), crate::device_send_store::MAX_RECORDS);
    assert!(index.fences.contains_key("old-0"));
    assert!(index.fences.contains_key(&format!(
        "old-{}",
        crate::device_send_store::MAX_RECORDS - 1
    )));
    records.push(("one-too-many", "unknown", false));
    let bytes = legacy_bytes(&records);
    assert_eq!(
        CompleteLegacyIndex::from_snapshot(&bytes, &digest(&bytes).0).err(),
        Some(CodecError::LegacyUnavailable)
    );
}

#[test]
fn legacy_wire_collision_in_any_v1_phase_blocks_reserve_without_burning_sequence() {
    let id = request(1, NONCE, "reader", "body").identity.wire();
    for (phase, archived) in [
        ("prepared", false),
        ("submitting", false),
        ("unknown", false),
        ("accepted", false),
        ("accepted", true),
    ] {
        let bytes = legacy_bytes(&[(&id, phase, archived)]);
        let state = empty_with_legacy(&bytes);
        let index = legacy_index(&bytes);
        let before = encode_checked(&state).unwrap();
        assert_eq!(
            plan_reserve(&state, &request(1, NONCE, "reader", "body"), &index).err(),
            Some(CodecError::LegacyCollision)
        );
        assert_eq!(state.issued_high_water.0, 0);
        assert_eq!(encode_checked(&state).unwrap(), before);
    }
}

#[test]
fn production_live_total_and_each_lane_are_fixed_and_full_rejection_does_not_retire() {
    let (state, legacy) = reserved(32, true);
    assert_eq!(state.live_slots.len(), 32);
    let before = encode_checked(&state).unwrap();
    assert_eq!(
        plan_reserve(&state, &request(33, NONCE, "reader", "body"), &legacy).err(),
        Some(CodecError::Capacity)
    );
    assert_eq!(encode_checked(&state).unwrap(), before);
    let (state, legacy) = reserved(16, false);
    assert_eq!(
        plan_reserve(&state, &request(17, NONCE, "reader", "body"), &legacy).err(),
        Some(CodecError::Capacity)
    );
    let plan = plan_reserve(&state, &request(17, NONCE, "dji", "body"), &legacy).unwrap();
    assert_eq!(plan.next.issued_high_water.0, 17);
}

#[test]
fn real_encoded_slot_cache_overlay_and_whole_snapshot_budgets_enforce_exact_boundaries() {
    let (mut state, _) = reserved(1, false);
    let slot = encoded_len(&state.live_slots[0]).unwrap() + FINISH_HEADROOM_BYTES;
    let exact = Quotas {
        slot_bytes: slot,
        ..Quotas::PRODUCTION
    };
    assert!(encode_with_quotas(&state, exact).is_ok());
    assert_eq!(
        encode_with_quotas(
            &state,
            Quotas {
                slot_bytes: slot - 1,
                ..exact
            }
        )
        .err(),
        Some(CodecError::Capacity)
    );
    let whole = encode_checked(&state).unwrap().len() + FINISH_HEADROOM_BYTES;
    let exact = Quotas {
        total_bytes: whole,
        ..Quotas::PRODUCTION
    };
    assert!(encode_with_quotas(&state, exact).is_ok());
    assert_eq!(
        encode_with_quotas(
            &state,
            Quotas {
                total_bytes: whole - 1,
                ..exact
            }
        )
        .err(),
        Some(CodecError::Capacity)
    );
    let slot = state.live_slots.remove(0);
    state.terminal_cache.push(cache(&slot));
    let cache_bytes = encoded_len(&state.terminal_cache).unwrap();
    let exact = Quotas {
        cache_bytes,
        ..Quotas::PRODUCTION
    };
    assert!(encode_with_quotas(&state, exact).is_ok());
    assert_eq!(
        encode_with_quotas(
            &state,
            Quotas {
                cache_bytes: cache_bytes - 1,
                ..exact
            }
        )
        .err(),
        Some(CodecError::Capacity)
    );
    let legacy = legacy_bytes(&[("old", "unknown", false)]);
    let index = legacy_index(&legacy);
    state.legacy = empty_with_legacy(&legacy).legacy;
    state.legacy_overlay = index.fences.values().cloned().collect();
    let item_bytes = encoded_len(&state.legacy_overlay[0]).unwrap();
    let exact = Quotas {
        overlay_bytes: item_bytes,
        ..Quotas::PRODUCTION
    };
    assert!(encode_with_quotas(&state, exact).is_ok());
    assert_eq!(
        encode_with_quotas(
            &state,
            Quotas {
                overlay_bytes: item_bytes - 1,
                ..exact
            }
        )
        .err(),
        Some(CodecError::Capacity)
    );
    assert_eq!(
        encode_with_quotas(
            &state,
            Quotas {
                live: MAX_LIVE + 1,
                ..Quotas::PRODUCTION
            }
        )
        .err(),
        Some(CodecError::Capacity)
    );
    assert_eq!(
        encode_with_quotas(
            &state,
            Quotas {
                total_bytes: SNAPSHOT_BYTES + 1,
                ..Quotas::PRODUCTION
            }
        )
        .err(),
        Some(CodecError::Capacity)
    );
}

#[test]
fn production_cache_count_byte_cap_overlay_count_and_payload_encoding_are_not_utf8_estimates() {
    let (mut state, legacy) = empty();
    let mut req = request_value(1, NONCE, "reader", &"\u{1}".repeat(64 * 1024));
    req["payload"]["phone"] = json!(format!("+{}", "\u{1}".repeat(127)));
    req["payload"]["expected_subscription_id"] = json!("s".repeat(128));
    let req = decode_reserve_bytes(&serde_json::to_vec(&req).unwrap()).unwrap();
    let plan = plan_reserve(&state, &req, &legacy).unwrap();
    let len = encoded_len(&plan.next.live_slots[0]).unwrap();
    assert!(len > 6 * 64 * 1024 && len + FINISH_HEADROOM_BYTES <= SLOT_BYTES);
    let mut cached = cache(&plan.next.live_slots[0]);
    for sequence in 1..=10 {
        cached.sequence = Decimal(sequence);
        state.terminal_cache.push(cached.clone());
    }
    state.issued_high_water = Decimal(10);
    assert!(encode_checked(&state).is_ok());
    cached.sequence = Decimal(11);
    state.terminal_cache.push(cached.clone());
    state.issued_high_water = Decimal(11);
    assert_eq!(encode_checked(&state).err(), Some(CodecError::Capacity));
    let (small, _) = reserved(1, false);
    let mut cached = cache(&small.live_slots[0]);
    state.terminal_cache.clear();
    for sequence in 1..=32 {
        cached.sequence = Decimal(sequence);
        state.terminal_cache.push(cached.clone());
    }
    state.issued_high_water = Decimal(32);
    assert!(encode_checked(&state).is_ok());
    cached.sequence = Decimal(33);
    state.terminal_cache.push(cached);
    state.issued_high_water = Decimal(33);
    assert_eq!(encode_checked(&state).err(), Some(CodecError::Capacity));
    state.terminal_cache.clear();
    let mut index = legacy_index(&legacy_bytes(&[("old", "unknown", false)]));
    let fence = index.fences.remove("old").unwrap();
    for n in 0..MAX_OVERLAY {
        let mut f = fence.clone();
        f.client_request_id = format!("old-{n}");
        state.legacy_overlay.push(f);
    }
    assert!(encode_checked(&state).is_ok());
    let mut f = fence;
    f.client_request_id = "extra".into();
    state.legacy_overlay.push(f);
    assert_eq!(encode_checked(&state).err(), Some(CodecError::Capacity));
    assert_eq!(
        decode_snapshot_bytes(&vec![b' '; SNAPSHOT_BYTES + 1]).err(),
        Some(CodecError::Capacity)
    );
    assert_eq!(
        decode_reserve_bytes(&vec![b' '; RESERVE_BYTES + 1]).err(),
        Some(CodecError::Capacity)
    );
}

#[test]
fn cached_not_dispatched_is_only_precise_terminal_replay_and_cannot_become_reserved_again() {
    let (mut state, legacy) = reserved(1, false);
    let slot = state.live_slots.remove(0);
    state.terminal_cache.push(cache(&slot));
    let before = encode_checked(&state).unwrap();
    let plan = plan_reserve(&state, &request(1, NONCE, "reader", "body"), &legacy).unwrap();
    assert_eq!(
        plan.disposition(),
        ReserveDisposition::TerminalNotDispatched
    );
    assert_eq!(plan.encoded().unwrap(), before);
    assert!(plan.next.live_slots.is_empty());
    let mut invalid = snapshot_value(&state);
    invalid[0]["terminal_cache"][0]["outcome"] = json!("accepted");
    reject_snapshot(invalid);
}

#[test]
fn unclaimed_retirement_is_exact_idempotent_and_never_issues_a_new_identity() {
    let (state, legacy) = reserved(1, false);
    let before = encode_checked(&state).unwrap();
    let req = request(1, NONCE, "reader", "body");
    let plan = plan_retire_unclaimed(&state, &req, &legacy).unwrap();
    assert_eq!(plan.disposition(), RetirementDisposition::Retired);
    assert_eq!(encode_checked(&state).unwrap(), before);
    assert_eq!(plan.next.issued_high_water.0, 1);
    assert!(plan.next.live_slots.is_empty());
    assert_eq!(plan.next.terminal_cache.len(), 1);
    let terminal = decode_snapshot_bytes(&plan.encoded().unwrap()).unwrap();
    assert_eq!(
        plan_retire_unclaimed(&terminal, &req, &legacy)
            .unwrap()
            .disposition(),
        RetirementDisposition::AlreadyTerminal
    );
    assert_eq!(
        plan_retire_unclaimed(&terminal, &req, &legacy)
            .unwrap()
            .encoded()
            .unwrap(),
        plan.encoded().unwrap()
    );
    assert_eq!(
        plan_retire_unclaimed(&state, &request(2, NONCE, "reader", "body"), &legacy).err(),
        Some(CodecError::NotIssued)
    );
    for changed in [
        request(1, "11111111111111111111111111111111", "reader", "body"),
        request(1, NONCE, "dji", "body"),
        request(1, NONCE, "reader", "other body"),
    ] {
        assert_eq!(
            plan_retire_unclaimed(&state, &changed, &legacy).err(),
            Some(CodecError::Conflict)
        );
    }
}

#[test]
fn bounded_not_dispatched_cache_evicts_oldest_without_reopening_claim_or_reserve() {
    let (mut state, legacy) = empty();
    for sequence in 1..=128 {
        let req = request(sequence, NONCE, "reader", "body");
        state = plan_reserve(&state, &req, &legacy).unwrap().next;
        state = plan_retire_unclaimed(&state, &req, &legacy).unwrap().next;
        assert!(state.live_slots.is_empty());
        assert!(state.terminal_cache.len() <= MAX_CACHE);
        assert_eq!(state.issued_high_water.0, sequence);
        state = decode_snapshot_bytes(&encode_checked(&state).unwrap()).unwrap();
    }
    assert_eq!(state.terminal_cache.len(), MAX_CACHE);
    assert_eq!(state.terminal_cache.first().unwrap().sequence.0, 97);
    for sequence in [1, 32, 96] {
        let req = request(sequence, NONCE, "reader", "body");
        assert_eq!(
            plan_reserve(&state, &req, &legacy).err(),
            Some(CodecError::Retired)
        );
        assert_eq!(
            plan_retire_unclaimed(&state, &req, &legacy).err(),
            Some(CodecError::Retired)
        );
    }
    assert_eq!(
        plan_reserve(&state, &request(128, NONCE, "reader", "body"), &legacy)
            .unwrap()
            .disposition(),
        ReserveDisposition::TerminalNotDispatched
    );
    assert_eq!(
        plan_reserve(&state, &request(129, NONCE, "reader", "body"), &legacy)
            .unwrap()
            .disposition(),
        ReserveDisposition::Created
    );
}

#[test]
fn small_cache_quota_evicts_only_terminal_records_and_keeps_live_slots() {
    let (mut state, legacy) = empty();
    let quotas = Quotas {
        cache: 1,
        ..Quotas::PRODUCTION
    };
    for sequence in 1..=3 {
        let req = request(sequence, NONCE, "reader", "body");
        state = plan_with_quotas(&state, &req, &legacy, quotas)
            .unwrap()
            .next;
        state = retire_with_quotas(&state, &req, &legacy, quotas)
            .unwrap()
            .next;
    }
    state = plan_with_quotas(
        &state,
        &request(4, NONCE, "dji", "unresolved reservation"),
        &legacy,
        quotas,
    )
    .unwrap()
    .next;
    let req = request(5, NONCE, "reader", "body");
    state = plan_with_quotas(&state, &req, &legacy, quotas)
        .unwrap()
        .next;
    state = retire_with_quotas(&state, &req, &legacy, quotas)
        .unwrap()
        .next;
    assert_eq!(state.issued_high_water.0, 5);
    assert_eq!(state.live_slots.len(), 1);
    assert_eq!(state.live_slots[0].sequence.0, 4);
    assert_eq!(state.terminal_cache.len(), 1);
    assert_eq!(state.terminal_cache[0].sequence.0, 5);
    assert_eq!(
        plan_with_quotas(
            &state,
            &request(1, NONCE, "reader", "body"),
            &legacy,
            quotas
        )
        .err(),
        Some(CodecError::Retired)
    );
}

#[test]
fn retirement_refuses_insufficient_cache_bytes_without_mutating_full_live_state() {
    let (state, legacy) = reserved(32, true);
    let before = encode_checked(&state).unwrap();
    let req = request(1, NONCE, "reader", "body");
    for quotas in [
        Quotas {
            cache: 0,
            ..Quotas::PRODUCTION
        },
        Quotas {
            cache_bytes: 1,
            ..Quotas::PRODUCTION
        },
        Quotas {
            total_bytes: before.len(),
            ..Quotas::PRODUCTION
        },
    ] {
        assert_eq!(
            retire_with_quotas(&state, &req, &legacy, quotas).err(),
            Some(CodecError::Capacity)
        );
        assert_eq!(encode_checked(&state).unwrap(), before);
        assert_eq!(state.live_slots.len(), MAX_LIVE);
        assert_eq!(state.issued_high_water.0, 32);
    }
}

// Canonical-only adapter: ArchiveStore already parsed Value and cannot expose
// raw duplicate keys. This fixture proves transaction/quota/locking behavior,
// not that a future real v2 file adapter is fully wired or raw-strict.
#[derive(Clone)]
struct FixtureAuthorityStore {
    archive: GuardedArchive,
}
impl FixtureAuthorityStore {
    fn create(path: &Path, state: &AuthoritySnapshot, file_budget: usize) -> Self {
        let archive = ArchiveStore::with_limits(path, file_budget as u64, 1)
            .unwrap()
            .require_nonempty_ledger()
            .unwrap();
        let items = serde_json::from_slice(&encode_checked(state).unwrap()).unwrap();
        archive.replace(items).unwrap();
        Self {
            archive: archive.into(),
        }
    }
    fn open(path: &Path) -> Self {
        let archive = ArchiveStore::with_limits(path, SNAPSHOT_BYTES as u64, 1)
            .unwrap()
            .require_nonempty_ledger()
            .unwrap();
        Self {
            archive: archive.into(),
        }
    }
    fn reserve(
        &self,
        request: &ValidatedReserve,
        legacy: &CompleteLegacyIndex,
    ) -> std::result::Result<ReserveDisposition, StoreError> {
        self.archive.mutate_if_changed(|items| {
            let state =
                decode_snapshot_bytes(&serde_json::to_vec(&*items).map_err(StoreError::Json)?)
                    .map_err(codec_store_error)?;
            let plan = plan_reserve(&state, request, legacy).map_err(codec_store_error)?;
            let disposition = plan.disposition();
            if disposition == ReserveDisposition::Created {
                *items = serde_json::from_slice(&plan.encoded().map_err(codec_store_error)?)
                    .map_err(StoreError::Json)?;
            }
            Ok((disposition, disposition == ReserveDisposition::Created))
        })
    }
    fn retire(
        &self,
        request: &ValidatedReserve,
        legacy: &CompleteLegacyIndex,
    ) -> std::result::Result<RetirementDisposition, StoreError> {
        self.archive.mutate_if_changed(|items| {
            let state =
                decode_snapshot_bytes(&serde_json::to_vec(&*items).map_err(StoreError::Json)?)
                    .map_err(codec_store_error)?;
            let plan = plan_retire_unclaimed(&state, request, legacy).map_err(codec_store_error)?;
            let disposition = plan.disposition();
            if disposition == RetirementDisposition::Retired {
                *items = serde_json::from_slice(&plan.encoded().map_err(codec_store_error)?)
                    .map_err(StoreError::Json)?;
            }
            Ok((disposition, disposition == RetirementDisposition::Retired))
        })
    }
}
fn codec_store_error(error: CodecError) -> StoreError {
    StoreError::Configuration(format!("authority_codec_{error:?}"))
}

#[test]
fn real_guarded_archive_commit_reopen_and_idempotent_retry_keep_same_snapshot() {
    let root = Root::new();
    let path = root.0.join("codec-fixture.json");
    let (state, legacy) = empty();
    let store = FixtureAuthorityStore::create(&path, &state, SNAPSHOT_BYTES);
    assert_eq!(
        store
            .reserve(&request(1, NONCE, "reader", "body"), &legacy)
            .unwrap(),
        ReserveDisposition::Created
    );
    let bytes = fs::read(&path).unwrap();
    let reopened = FixtureAuthorityStore::open(&path);
    assert_eq!(
        reopened
            .reserve(&request(1, NONCE, "reader", "body"), &legacy)
            .unwrap(),
        ReserveDisposition::Existing
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(
        reopened
            .reserve(
                &request(1, "1".repeat(32).as_str(), "reader", "body"),
                &legacy
            )
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        decode_snapshot_bytes(&bytes).unwrap().issued_high_water.0,
        1
    );
}

#[test]
fn real_archive_retirement_reopen_keeps_high_water_and_refuses_old_sequence() {
    let root = Root::new();
    let path = root.0.join("codec-fixture.json");
    let (state, legacy) = empty();
    let store = FixtureAuthorityStore::create(&path, &state, SNAPSHOT_BYTES);
    for sequence in 1..=40 {
        let req = request(sequence, NONCE, "reader", "body");
        assert_eq!(
            store.reserve(&req, &legacy).unwrap(),
            ReserveDisposition::Created
        );
        assert_eq!(
            store.retire(&req, &legacy).unwrap(),
            RetirementDisposition::Retired
        );
    }
    let before = fs::read(&path).unwrap();
    let reopened = FixtureAuthorityStore::open(&path);
    assert_eq!(
        reopened
            .retire(&request(40, NONCE, "reader", "body"), &legacy)
            .unwrap(),
        RetirementDisposition::AlreadyTerminal
    );
    assert!(
        reopened
            .reserve(&request(1, NONCE, "reader", "body"), &legacy)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let persisted = decode_snapshot_bytes(&before).unwrap();
    assert_eq!(persisted.issued_high_water.0, 40);
    assert_eq!(persisted.terminal_cache.len(), MAX_CACHE);
    assert!(persisted.live_slots.is_empty());
}

#[test]
fn real_store_full_or_incomplete_legacy_refusal_keeps_bytes_watermark_and_slots() {
    let root = Root::new();
    let path = root.0.join("codec-fixture.json");
    let (state, legacy) = reserved(32, true);
    let store = FixtureAuthorityStore::create(&path, &state, SNAPSHOT_BYTES);
    let before = fs::read(&path).unwrap();
    assert!(
        store
            .reserve(&request(33, NONCE, "reader", "body"), &legacy)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let wrong = legacy_index(&legacy_bytes(&[("different", "unknown", false)]));
    assert!(
        store
            .reserve(&request(1, NONCE, "reader", "body"), &wrong)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        !store.archive.status().restart_required,
        "known admission refusal is not a storage failure"
    );
    let reopened = decode_snapshot_bytes(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(reopened.issued_high_water.0, 32);
    assert_eq!(reopened.live_slots.len(), 32);
}

#[test]
fn real_file_lock_serializes_same_candidate_competition_without_hidden_renumbering() {
    let root = Root::new();
    let path = root.0.join("codec-fixture.json");
    let (state, _) = empty();
    let first = FixtureAuthorityStore::create(&path, &state, SNAPSHOT_BYTES);
    let second = FixtureAuthorityStore::open(&path);
    let barrier = Arc::new(Barrier::new(3));
    let threads = [first, second]
        .into_iter()
        .enumerate()
        .map(|(n, store)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let (_, legacy) = empty();
                let nonce = format!("{n:032x}");
                barrier.wait();
                store.reserve(&request(1, &nonce, "reader", "body"), &legacy)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Ok(ReserveDisposition::Created)))
            .count(),
        1
    );
    assert_eq!(results.iter().filter(|r| r.is_err()).count(), 1);
    let state = decode_snapshot_bytes(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(state.issued_high_water.0, 1);
    assert_eq!(state.live_slots.len(), 1);
}

#[test]
fn real_persist_limit_failure_returns_no_reservation_and_does_not_advance_disk_watermark() {
    let root = Root::new();
    let path = root.0.join("codec-fixture.json");
    let (state, legacy) = empty();
    let encoded = encode_checked(&state).unwrap();
    let store = FixtureAuthorityStore::create(&path, &state, encoded.len());
    // ArchiveStore serializes Value in map order; compare the actual persisted
    // generation rather than assuming the struct encoder uses that key order.
    let bytes = fs::read(&path).unwrap();
    assert!(
        store
            .reserve(&request(1, NONCE, "reader", "body"), &legacy)
            .is_err()
    );
    assert!(store.archive.status().restart_required);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let reopened = FixtureAuthorityStore::open(&path);
    assert_eq!(
        decode_snapshot_bytes(&fs::read(&path).unwrap())
            .unwrap()
            .issued_high_water
            .0,
        0
    );
    assert_eq!(
        reopened
            .reserve(&request(1, NONCE, "reader", "body"), &legacy)
            .unwrap(),
        ReserveDisposition::Created
    );
}

#[test]
fn current_v1_recover_and_all_apply_decoders_refuse_v2_without_rewriting_or_claiming() {
    let (state, _) = reserved(1, false);
    let value = snapshot_value(&state);
    for action in [
        "recover",
        "recover_go_owned",
        "prepare",
        "claim",
        "finish",
        "get",
        "list",
    ] {
        let root = Root::new();
        let path = root.0.join("device-send-ledger.json");
        let archive = ArchiveStore::with_limits(&path, SNAPSHOT_BYTES as u64, 1).unwrap();
        archive.replace(value.as_array().unwrap().clone()).unwrap();
        let before = fs::read(&path).unwrap();
        let store = V1Store::open(&root.0).unwrap(); // Opening itself is NOT decode.
        assert_eq!(store.status().state, "ready");
        let body = json!({"client_request_id":"old","phone":"+12025550100","message":"body", "device":"reader",
            "binding":"route-A","attempt":"attempt-A","state":"accepted","segments":1,"archive_confirmed":true});
        let rejected = match action {
            "recover" => store.recover().map(|_| Value::Null),
            "recover_go_owned" => store.recover_go_owned().map(|_| Value::Null),
            other => store.apply(other, body),
        };
        assert!(rejected.is_err(), "v1 action {action} accepted v2");
        assert!(store.status().restart_required);
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn unchanged_v1_path_keeps_prefix_shaped_legacy_id_a_v1_string_until_explicit_migration() {
    let root = Root::new();
    let store = V1Store::open(&root.0).unwrap();
    let id = request(1, NONCE, "reader", "body").identity.wire();
    let body = json!({"client_request_id":id,"phone":"+12025550100","message":"body", "device":"reader","binding":"route-A"});
    assert_eq!(store.apply("prepare", body).unwrap()["can_submit"], false);
    let claim = json!({"client_request_id":id,"binding":"route-A","attempt":"attempt-A"});
    assert_eq!(
        store.apply("claim", claim.clone()).unwrap()["can_submit"],
        true
    );
    assert_eq!(store.apply("claim", claim).unwrap()["can_submit"], false);
    let bytes = fs::read(root.0.join("device-send-ledger.json")).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value[0]["schema_version"], 1);
    let legacy = legacy_index(&bytes);
    let state = empty_with_legacy(&bytes);
    assert_eq!(
        plan_reserve(&state, &request(1, NONCE, "reader", "body"), &legacy).err(),
        Some(CodecError::LegacyCollision)
    );
}
